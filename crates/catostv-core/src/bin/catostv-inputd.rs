//! `catostv-inputd`, passive escape-gesture watcher.
//!
//! Reads every keyboard/remote that has a Back-class key (Escape, Backspace,
//! KEY_BACK) without grabbing it, and sends `escape` to the shell when the
//! user presses Back three times quickly or holds it for a second
//! (`catostv_core::gesture`). The shell decides whether an app is in front.
//!
//! ```text
//! catostv-inputd             run (started from labwc autostart, supervised)
//! catostv-inputd --dry-run   run, but print gestures instead of sending IPC
//! catostv-inputd --list      print the input devices and whether they are watched
//! ```
//!
//! Game controllers (USB / Bluetooth) are read too and navigate CatosTV: D-pad,
//! A = OK, B = Back, Y/Start = Menu, X/Select = Control Center, Guide = Home
//! (`catostv_core::gamepad`). Settings → Remote & keys
//! can turn this off or swap A/B (`~/.config/catostv/ui.conf`, re-read every
//! few seconds).
//!
//! Privacy: this program can read every keystroke. It logs device names and
//! Back-class gestures only, never other key codes, and keeps no key
//! history beyond the gesture detector's state.

use catostv_core::evdev::{self, DeviceMsg, ScanNotice, Watcher};
use catostv_core::gamepad::{self, PadMapper, PadOut};
use catostv_core::gesture::{self, Detector, Gesture};
use catostv_core::ipc::{self, ShellCommand};
use catostv_core::process::SystemRunner;
use catostv_core::uinput::{self, Front};
use catostv_core::{compositor, prefs};
use std::collections::HashMap;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: catostv-inputd [--dry-run | --list] [--dev-dir DIR --sys-dir DIR]
  (no option)  watch keyboards/remotes and send `escape` to catostv-shell on
               3x Back within 0.6 s or Back held for 1 s
  --dry-run    same, but print gestures instead of sending them
  --list       print input devices and whether they are watched, then exit
  --dev-dir, --sys-dir   read devices from DIR instead of /dev/input and
               /sys/class/input (testing with fixture files / FIFOs)";

/// Linux `O_NONBLOCK` (same value on x86-64 and aarch64).
const O_NONBLOCK: i32 = 0o4000;

fn log(msg: &str) {
    eprintln!("catostv-inputd: {msg}");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut dry_run = false;
    let mut list = false;
    let mut dev_dir = "/dev/input".to_string();
    let mut sys_dir = "/sys/class/input".to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" => dry_run = true,
            "--list" => list = true,
            "--dev-dir" | "--sys-dir" => {
                let Some(v) = it.next() else {
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                };
                if a == "--dev-dir" {
                    dev_dir = v.clone();
                } else {
                    sys_dir = v.clone();
                }
            }
            "-h" | "--help" | "help" => {
                println!("{USAGE}");
                return;
            }
            _ => {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let mut watcher = Watcher::new(dev_dir, sys_dir);
    if list {
        print_devices(&watcher);
        return;
    }
    run(&mut watcher, dry_run);
}

fn print_devices(watcher: &Watcher) {
    let devices = watcher.devices();
    if devices.is_empty() {
        println!("no /dev/input/event* devices found");
        return;
    }
    for d in devices {
        let status = if !d.is_interesting() {
            "skip  ".to_string()
        } else {
            // Non-blocking so a FIFO test fixture cannot hang the listing.
            let open = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(O_NONBLOCK)
                .open(&d.path);
            match open {
                Ok(_) => "WATCH ".to_string(),
                Err(e) => format!("WATCH (cannot open: {e}) "),
            }
        };
        println!(
            "{status}{:<9} back keys: {:<26} \"{}\"",
            d.id,
            d.back_key_names(),
            d.name
        );
    }
}

fn act(g: Gesture, dry_run: bool) {
    let cmd = g.command();
    if dry_run {
        println!(
            "gesture: {} -> {} (dry run, not sent)",
            g.describe(),
            cmd.as_str()
        );
        return;
    }
    log(&format!("gesture: {} -> {}", g.describe(), cmd.as_str()));
    if let Err(e) = ipc::send(cmd) {
        // The shell may be restarting; the next gesture will try again.
        log(&format!("could not send {}: {e}", cmd.as_str()));
    }
}

/// Send what a controller asked for (over an Android app only Guide gets
/// here, see `gamepad::over_android`). Home and the Control Center first
/// bring CatosTV's desktop to the front (the shell's own UI loop is paused
/// while an app is in front, like the Home key's compositor binding).
fn pad(out: PadOut, dry_run: bool) {
    let cmd = match out {
        PadOut::Up => ShellCommand::NavUp,
        PadOut::Down => ShellCommand::NavDown,
        PadOut::Left => ShellCommand::NavLeft,
        PadOut::Right => ShellCommand::NavRight,
        PadOut::Ok => ShellCommand::NavOk,
        PadOut::Back => ShellCommand::NavBack,
        PadOut::Menu => ShellCommand::NavMenu,
        PadOut::Quick => ShellCommand::Quick,
        PadOut::Home => ShellCommand::Home,
    };
    if dry_run {
        println!(
            "controller: {out:?} -> {} (dry run, not sent)",
            cmd.as_str()
        );
        return;
    }
    if matches!(out, PadOut::Home | PadOut::Quick) {
        let _ = compositor::show_shell_workspace(&mut SystemRunner, true);
    }
    // The shell may be restarting; nothing to retry for a button press.
    let _ = ipc::send(cmd);
}

/// An Android app is in front (published by the shell).
fn over_android() -> bool {
    uinput::front() == Front::Android
}

fn run(watcher: &mut Watcher, dry_run: bool) -> ! {
    log(&format!(
        "started{}: escape = {}x Back within {} s, or Back held {} s",
        if dry_run { " (dry run)" } else { "" },
        gesture::TRIPLE_PRESS_COUNT,
        gesture::TRIPLE_PRESS_GAP_SECS,
        gesture::HOLD_SECS,
    ));
    let mut detector = Detector::new();
    let mut pads: HashMap<String, PadMapper> = HashMap::new();
    // Device id → name, for the per-controller layout (Settings).
    let mut names: HashMap<String, String> = HashMap::new();
    let mut ui = prefs::load();
    let mut next_scan = Instant::now();
    let mut said_none = false;
    loop {
        if Instant::now() >= next_scan {
            // Controller settings may have changed in Settings.
            ui = prefs::load();
            for (id, m) in pads.iter_mut() {
                m.nintendo = ui.nintendo_for(names.get(id).map_or("", String::as_str));
                m.bindings = ui.pad_mapper_bindings();
            }
            for n in watcher.rescan() {
                match n {
                    ScanNotice::Started(d) if d.gamepad => {
                        names.insert(d.id.clone(), d.name.clone());
                        log(&format!(
                            "watching {} \"{}\" (game controller)",
                            d.id, d.name
                        ))
                    }
                    ScanNotice::Started(d) => log(&format!(
                        "watching {} \"{}\" (back keys: {})",
                        d.id,
                        d.name,
                        d.back_key_names()
                    )),
                    ScanNotice::OpenFailed { device, error } => log(&format!(
                        "cannot open {} \"{}\": {error} (is the user in the 'input' group?); will retry",
                        device.id, device.name
                    )),
                }
            }
            if watcher.active_count() == 0 && !said_none {
                log(&format!(
                    "no keyboard or remote with a Back key yet; rescanning every {} s",
                    evdev::RESCAN_INTERVAL.as_secs()
                ));
            }
            said_none = watcher.active_count() == 0;
            next_scan = Instant::now() + evdev::RESCAN_INTERVAL;
        }

        let mut timeout = next_scan.saturating_duration_since(Instant::now());
        let deadlines = detector
            .next_deadline()
            .into_iter()
            .chain(pads.values().filter_map(PadMapper::next_deadline));
        for deadline in deadlines {
            let wait = (deadline - watcher.now()).max(0.0);
            timeout = timeout.min(Duration::from_secs_f64(wait));
        }

        match watcher.recv_timeout(timeout) {
            Some(DeviceMsg::Key { event, .. }) => {
                if let Some(g) = detector.on_key(event) {
                    act(g, dry_run);
                }
            }
            Some(DeviceMsg::Pad {
                device,
                event,
                time,
            }) => {
                // Settings → Controller navigation is waiting for a button:
                // hand the code over instead of acting on it.
                if let catostv_core::gamepad::PadEvent::Button {
                    code,
                    pressed: true,
                } = event
                    && gamepad::capture_flag().exists()
                {
                    let _ = std::fs::write(gamepad::captured_file(), format!("{code}\n"));
                    let _ = ipc::send(ShellCommand::PadCaptured);
                    continue;
                }
                if ui.pad_nav {
                    let nintendo = ui.nintendo_for(names.get(&device).map_or("", String::as_str));
                    let bindings = ui.pad_mapper_bindings();
                    let m = pads.entry(device).or_insert_with(|| {
                        let mut m = PadMapper::new(nintendo);
                        m.bindings = bindings;
                        m
                    });
                    // The mapper always sees the event (its held-direction
                    // state stays right); over an Android app its output is
                    // dropped and only Guide acts.
                    let outs = m.on_event(event, time);
                    if over_android() {
                        if let Some(out) = gamepad::over_android(event) {
                            pad(out, dry_run);
                        }
                    } else {
                        for out in outs {
                            pad(out, dry_run);
                        }
                    }
                }
            }
            Some(DeviceMsg::Dropped { device }) => {
                log(&format!(
                    "{device}: kernel dropped events; gesture state reset"
                ));
                detector.reset();
                if let Some(m) = pads.get_mut(&device) {
                    m.reset();
                }
            }
            Some(DeviceMsg::Gone { device, reason }) => {
                log(&format!("stopped watching {device}: {reason}"));
                // A key held on that device will never report its release.
                detector.reset();
                pads.remove(&device);
            }
            None => {}
        }
        if let Some(g) = detector.tick(watcher.now()) {
            act(g, dry_run);
        }
        let now = watcher.now();
        let repeats: Vec<PadOut> = pads.values_mut().filter_map(|m| m.tick(now)).collect();
        // A held direction never acts over an Android app.
        if !repeats.is_empty() && !over_android() {
            for out in repeats {
                pad(out, dry_run);
            }
        }
    }
}
