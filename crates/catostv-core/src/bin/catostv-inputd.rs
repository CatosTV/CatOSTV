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
//! Mouse mode (`catostv_core::cursor`): while the shell says it is on for
//! the Linux app in front, this program holds the keyboards and remotes,
//! moves a virtual mouse with the arrows (OK clicks, held OK is the right
//! button, Back ends it) and passes every other key on through its own
//! keyboard. Controllers move the pointer with the left stick or D-pad.
//!
//! Privacy: this program can read every keystroke. It logs device names and
//! Back-class gestures only, never other key codes, and keeps no key
//! history beyond the gesture detector's state. In mouse mode the keys it
//! passes on are written to the virtual keyboard and forgotten (only which
//! keys are down right now is kept, to let go of them at the end).

use catostv_core::cursor::{self, Click, Clicker, Dir, Motion, Role};
use catostv_core::evdev::{self, DeviceMsg, ScanNotice, Watcher};
use catostv_core::gamepad::{self, PadMapper, PadOut};
use catostv_core::gesture::{self, Detector, Gesture};
use catostv_core::ipc::{self, ShellCommand};
use catostv_core::process::{Cmd, Runner, SystemRunner};
use catostv_core::uinput::{self, BTN_LEFT, BTN_RIGHT, Front, VirtualKeyboard, VirtualMouse};
use catostv_core::{compositor, prefs};
use std::collections::{HashMap, HashSet};
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

/// One of CatosTV's own remote apps (CatTube, the players) is in front: a
/// controller presses keys in it (`gamepad::REMOTE_APPS`).
fn remote_app_in_front() -> bool {
    uinput::front() == Front::App && gamepad::keys_on()
}

// ------------------------------------------------------------ mouse mode

/// How often the mouse mode file is looked at.
const MOUSE_CHECK: Duration = Duration::from_millis(150);
/// Pointer updates while it moves (about 120 a second).
const MOUSE_FRAME: f64 = 0.008;
/// Right stick all the way: wheel notches per second.
const SCROLL_RATE: f64 = 14.0;

/// A short note on screen (mako), replacing the previous one.
fn notify(title: &str, body: &str) {
    let id_path = catostv_core::env::runtime_dir().join("catostv-mouse-nid");
    let mut args = vec![
        "-a".to_string(),
        "CatosTV".to_string(),
        "-t".to_string(),
        "4000".to_string(),
        "-p".to_string(),
    ];
    if let Some(id) = std::fs::read_to_string(&id_path)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
    {
        args.push("-r".into());
        args.push(id);
    }
    args.push(title.into());
    args.push(body.into());
    if let Ok(out) = SystemRunner.run(&Cmd::new("notify-send").args(args)) {
        let id = out.stdout.trim();
        if !id.is_empty() {
            let _ = std::fs::write(&id_path, id);
        }
    }
}

/// Take the note down (CatosTV came to the front: it would sit over it).
fn dismiss_note() {
    let id_path = catostv_core::env::runtime_dir().join("catostv-mouse-nid");
    if let Some(id) = std::fs::read_to_string(&id_path)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
    {
        let _ = SystemRunner.run(&Cmd::new("makoctl").args(["dismiss", "-n", &id]));
    }
}

/// Mouse mode: the devices, what is held, and the pointer's state.
#[derive(Default)]
struct Mouse {
    on: bool,
    mouse: Option<VirtualMouse>,
    keys: Option<VirtualKeyboard>,
    /// Devices could not be made (no /dev/uinput): do not try again soon.
    failed_at: Option<Instant>,
    /// Keyboards and remotes held now.
    held: HashSet<String>,
    /// Keys passed on that are down now (let go of when mouse mode ends).
    down: HashSet<u16>,
    motion: Motion,
    clicker: Clicker,
    /// Controller sticks by device: left x, y, right x, y.
    sticks: HashMap<String, [f64; 4]>,
    rest: (f64, f64),
    scroll_rest: (f64, f64),
    last_frame: Option<f64>,
    next_check: Option<Instant>,
}

impl Mouse {
    /// Make the virtual devices (once).
    fn devices(&mut self) -> bool {
        if self.mouse.is_some() && self.keys.is_some() {
            return true;
        }
        if self
            .failed_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
        {
            return false;
        }
        let made = VirtualMouse::create(cursor::MOUSE_NAME).and_then(|m| {
            let k = VirtualKeyboard::create_full(cursor::KEYS_NAME)?;
            Ok((m, k))
        });
        match made {
            Ok((m, k)) => {
                self.mouse = Some(m);
                self.keys = Some(k);
                true
            }
            Err(e) => {
                log(&format!("mouse mode: cannot make the virtual mouse ({e})"));
                self.failed_at = Some(Instant::now());
                false
            }
        }
    }

    /// Follow the mouse mode file (and what is in front).
    fn check(&mut self, watcher: &mut Watcher, pads: &mut HashMap<String, PadMapper>) {
        let now = Instant::now();
        if self.next_check.is_some_and(|t| now < t) {
            return;
        }
        self.next_check = Some(now + MOUSE_CHECK);
        let want = cursor::is_on() && uinput::front() == Front::App;
        if want && !self.on {
            if !self.devices() {
                cursor::set_on(false);
                return;
            }
            self.on = true;
            watcher.set_raw(true);
            for m in pads.values_mut() {
                m.reset();
            }
            log("mouse mode on");
            notify(
                "Mouse pointer on",
                "Arrows move it, OK clicks, Back turns it off.",
            );
        } else if !want && self.on {
            self.leave(watcher, pads, false);
        }
        if self.on {
            self.hold_all(watcher);
        }
    }

    /// Hold every keyboard and remote not held yet. One with a key down
    /// waits (holding it now would leave that key stuck for everyone).
    fn hold_all(&mut self, watcher: &Watcher) {
        for id in watcher.keyboards() {
            if self.held.contains(&id) || watcher.keys_held(&id) {
                continue;
            }
            match watcher.grab(&id, true) {
                Ok(()) => {
                    self.held.insert(id);
                }
                Err(e) => log(&format!("mouse mode: cannot hold {id}: {e}")),
            }
        }
        self.held.retain(|id| watcher.keyboards().contains(id));
    }

    /// Mouse mode off: let go of everything. `by_user`: Back, a letter or
    /// a controller's B, so the shell is told and a note says how to get
    /// it back.
    fn leave(
        &mut self,
        watcher: &mut Watcher,
        pads: &mut HashMap<String, PadMapper>,
        by_user: bool,
    ) {
        if let Some(k) = self.keys.as_mut() {
            for code in self.down.drain() {
                let _ = k.key(code, false);
            }
        }
        for id in self.held.drain() {
            let _ = watcher.grab(&id, false);
        }
        watcher.set_raw(false);
        for m in pads.values_mut() {
            m.reset();
        }
        self.on = false;
        self.motion.reset();
        self.clicker.reset();
        self.sticks.clear();
        self.rest = (0.0, 0.0);
        self.scroll_rest = (0.0, 0.0);
        self.last_frame = None;
        log(if by_user {
            "mouse mode off (by the user)"
        } else {
            "mouse mode off"
        });
        if !by_user {
            dismiss_note();
        }
        if by_user {
            cursor::set_on(false);
            notify(
                "Mouse pointer off",
                "Turn it on again in the Control Center.",
            );
        }
    }

    /// Press a key for a controller in one of CatosTV's remote apps.
    fn tap(&mut self, code: u16) {
        if !self.devices() {
            return;
        }
        if let Some(k) = self.keys.as_mut() {
            let _ = k.tap(code);
        }
    }

    fn click(&mut self, c: Click) {
        if let Some(m) = self.mouse.as_mut() {
            let _ = m.click(match c {
                Click::Left => BTN_LEFT,
                Click::Right => BTN_RIGHT,
            });
        }
    }

    fn pointer(&mut self, dx: i32, dy: i32) {
        if let Some(m) = self.mouse.as_mut() {
            let _ = m.move_by(dx, dy);
        }
    }

    /// A key of a held keyboard or remote. Returns true when mouse mode
    /// should end (by the user).
    fn key(&mut self, device: &str, code: u16, value: i32, now: f64) -> bool {
        if !self.held.contains(device) {
            // Not held (a key was down when mouse mode began): the key
            // already went to the app.
            return false;
        }
        match cursor::role(code) {
            Role::Move(d) => match value {
                1 => {
                    let (dx, dy) = self.motion.press(d, now);
                    self.pointer(dx, dy);
                }
                0 => self.motion.release(d, now),
                _ => {}
            },
            Role::Click => match value {
                1 => self.clicker.press(now),
                0 => {
                    if let Some(c) = self.clicker.release(now) {
                        self.click(c);
                    }
                }
                _ => {}
            },
            Role::RightClick if value == 1 => self.click(Click::Right),
            Role::Scroll(n) if value >= 1 => {
                if let Some(m) = self.mouse.as_mut() {
                    let _ = m.wheel(n, 0);
                }
            }
            Role::Leave if value == 1 => return true,
            Role::Typing if value == 1 => {
                // Typing on a keyboard: it gets its keys back, this one
                // included.
                if let Some(k) = self.keys.as_mut() {
                    let _ = k.tap(code);
                }
                return true;
            }
            Role::Pass => {
                if let Some(k) = self.keys.as_mut() {
                    let _ = k.key_value(code, value);
                }
                match value {
                    1 => {
                        self.down.insert(code);
                    }
                    0 => {
                        self.down.remove(&code);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        false
    }

    /// A controller in mouse mode. Returns true when mouse mode should end.
    fn pad(
        &mut self,
        mapper: &mut PadMapper,
        event: gamepad::PadEvent,
        time: f64,
        dry_run: bool,
    ) -> bool {
        use catostv_core::gamepad::{
            ABS_HAT0X, BTN_DPAD_DOWN, BTN_DPAD_LEFT, BTN_DPAD_RIGHT, BTN_DPAD_UP, PadEvent,
        };
        let dpad = |code| match code {
            BTN_DPAD_UP => Some(Dir::Up),
            BTN_DPAD_DOWN => Some(Dir::Down),
            BTN_DPAD_LEFT => Some(Dir::Left),
            BTN_DPAD_RIGHT => Some(Dir::Right),
            _ => None,
        };
        match event {
            PadEvent::Button { code, pressed } if dpad(code).is_some() => {
                let d = dpad(code).unwrap_or(Dir::Up);
                if pressed {
                    let (dx, dy) = self.motion.press(d, time);
                    self.pointer(dx, dy);
                } else {
                    self.motion.release(d, time);
                }
            }
            PadEvent::Hat { code, value } => {
                let (neg, pos) = if code == ABS_HAT0X {
                    (Dir::Left, Dir::Right)
                } else {
                    (Dir::Up, Dir::Down)
                };
                self.motion.release(neg, time);
                self.motion.release(pos, time);
                if value != 0 {
                    let d = if value < 0 { neg } else { pos };
                    let (dx, dy) = self.motion.press(d, time);
                    self.pointer(dx, dy);
                }
            }
            // The stick comes as positions (DeviceMsg::Axis).
            PadEvent::Stick { .. } => {}
            PadEvent::Button { .. } => {
                for out in mapper.on_event(event, time) {
                    match out {
                        PadOut::Ok => self.click(Click::Left),
                        PadOut::Menu => self.click(Click::Right),
                        PadOut::Back => return true,
                        PadOut::Home | PadOut::Quick => pad(out, dry_run),
                        PadOut::Up | PadOut::Down | PadOut::Left | PadOut::Right => {}
                    }
                }
            }
        }
        false
    }

    fn axis(&mut self, device: String, code: u16, value: f64) {
        let i = match code {
            0x00 => 0,
            0x01 => 1,
            0x03 => 2,
            0x04 => 3,
            _ => return,
        };
        self.sticks.entry(device).or_insert([0.0; 4])[i] = value;
    }

    fn sticks_moving(&self) -> bool {
        self.sticks.values().any(|s| {
            cursor::stick_velocity(s[0], s[1]) != (0.0, 0.0) || s[3].abs() > cursor::STICK_DEAD
        })
    }

    /// Move and scroll for the time since the last frame.
    fn frame(&mut self, now: f64) {
        if let Some(c) = self.clicker.tick(now) {
            self.click(c);
        }
        let dt = self.last_frame.map_or(0.0, |l| (now - l).clamp(0.0, 0.05));
        self.last_frame = Some(now);
        let (mut dx, mut dy) = self.motion.step(now);
        let (mut vx, mut vy, mut wheel) = (0.0, 0.0, 0.0);
        for s in self.sticks.values() {
            let (x, y) = cursor::stick_velocity(s[0], s[1]);
            vx += x;
            vy += y;
            if s[3].abs() > cursor::STICK_DEAD {
                // Stick pushed up (negative) scrolls up.
                wheel -= s[3] * SCROLL_RATE;
            }
        }
        let fx = self.rest.0 + vx * dt;
        let fy = self.rest.1 + vy * dt;
        self.rest = (fx - fx.trunc(), fy - fy.trunc());
        dx += fx.trunc() as i32;
        dy += fy.trunc() as i32;
        self.pointer(dx, dy);
        let w = self.scroll_rest.0 + wheel * dt;
        self.scroll_rest.0 = w - w.trunc();
        if w.trunc() != 0.0
            && let Some(m) = self.mouse.as_mut()
        {
            let _ = m.wheel(w.trunc() as i32, 0);
        }
        if !self.motion.moving(now) && !self.sticks_moving() {
            self.last_frame = None;
        }
    }

    /// Seconds until mouse mode needs to run again.
    fn wait(&self, now: f64) -> Option<f64> {
        if !self.on {
            return None;
        }
        if self.motion.moving(now) || self.sticks_moving() {
            return Some(MOUSE_FRAME);
        }
        self.clicker.deadline().map(|d| (d - now).max(0.0))
    }
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
    let mut mouse = Mouse::default();
    loop {
        // Mouse mode follows the shell (never in a dry run).
        if !dry_run {
            mouse.check(watcher, &mut pads);
        }
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

        let mut timeout = next_scan
            .saturating_duration_since(Instant::now())
            .min(MOUSE_CHECK);
        if let Some(w) = mouse.wait(watcher.now()) {
            timeout = timeout.min(Duration::from_secs_f64(w));
        }
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
            Some(DeviceMsg::Raw {
                device,
                code,
                value,
            }) => {
                if mouse.on && mouse.key(&device, code, value, watcher.now()) {
                    mouse.leave(watcher, &mut pads, true);
                }
            }
            Some(DeviceMsg::Axis {
                device,
                code,
                value,
            }) => {
                if mouse.on {
                    mouse.axis(device, code, value);
                }
            }
            Some(DeviceMsg::Pad {
                device,
                event,
                time,
            }) if mouse.on => {
                let nintendo = ui.nintendo_for(names.get(&device).map_or("", String::as_str));
                let bindings = ui.pad_mapper_bindings();
                let m = pads.entry(device).or_insert_with(|| {
                    let mut m = PadMapper::new(nintendo);
                    m.bindings = bindings;
                    m
                });
                if mouse.pad(m, event, time, dry_run) {
                    mouse.leave(watcher, &mut pads, true);
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
                        let keys = remote_app_in_front();
                        for out in outs {
                            match gamepad::key_for(out).filter(|_| keys && !dry_run) {
                                Some(code) => mouse.tap(code),
                                None => pad(out, dry_run),
                            }
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
                mouse.held.remove(&device);
                mouse.sticks.remove(&device);
                // A key held on that device will never report its release.
                detector.reset();
                pads.remove(&device);
            }
            None => {}
        }
        if mouse.on {
            mouse.frame(watcher.now());
        }
        if let Some(g) = detector.tick(watcher.now()) {
            act(g, dry_run);
        }
        let now = watcher.now();
        let repeats: Vec<PadOut> = pads.values_mut().filter_map(|m| m.tick(now)).collect();
        // A held direction never acts over an Android app, nor in mouse
        // mode (it moves the pointer there).
        if !repeats.is_empty() && !over_android() && !mouse.on {
            let keys = remote_app_in_front();
            for out in repeats {
                match gamepad::key_for(out).filter(|_| keys && !dry_run) {
                    Some(code) => mouse.tap(code),
                    None => pad(out, dry_run),
                }
            }
        }
    }
}
