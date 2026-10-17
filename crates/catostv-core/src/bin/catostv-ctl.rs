//! `catostv-ctl <command>`, send one IPC command to the running shell, or
//! change the volume.
//!
//! Used by labwc key bindings (Home, Control Center, power key, volume).
//! Exit codes: 0 sent, 1 shell not reachable / volume failed, 2 usage error.
//!
//! `volume-up`, `volume-down` and `mute` go to the shell, which shows its
//! volume panel (over apps too). Without a shell they are handled here:
//! `wpctl`, and the level as a notification drawn by mako.

use catostv_core::audio;
use catostv_core::ipc::{self, ShellCommand};
use catostv_core::process::{Cmd, Runner, SystemRunner};

/// The id mako gave the last volume notification is kept here, so the
/// next change replaces it instead of stacking a new one
/// ("two panels, 1 more, 2 more…"). A made-up id does not replace
/// anything: `-r` only works with an id the server handed out.
fn volume_id_path() -> std::path::PathBuf {
    catostv_core::env::runtime_dir().join("catostv-volume-nid")
}

fn volume(arg: &str) -> Option<i32> {
    let mut r = SystemRunner;
    let res = match arg {
        "volume-up" => audio::step(&mut r, 5),
        "volume-down" => audio::step(&mut r, -5),
        "mute" => audio::toggle_mute(&mut r),
        _ => return None,
    };
    match res {
        Ok(v) => {
            let (title, value) = if v.muted {
                ("Muted".to_string(), 0)
            } else {
                (format!("Volume {}%", v.percent), v.percent)
            };
            let id_path = volume_id_path();
            let mut args = vec![
                "-a".to_string(),
                "CatosTV".to_string(),
                "-t".to_string(),
                "1500".to_string(),
                "-p".to_string(),
                "-h".to_string(),
                format!("int:value:{value}"),
            ];
            if let Some(id) = std::fs::read_to_string(&id_path)
                .ok()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
            {
                args.push("-r".into());
                args.push(id);
            }
            args.push(title);
            if let Ok(out) = r.run(&Cmd::new("notify-send").args(args)) {
                let id = out.stdout.trim();
                if !id.is_empty() {
                    let _ = std::fs::write(&id_path, id);
                }
            }
            Some(0)
        }
        Err(e) => {
            eprintln!("catostv-ctl: {e}");
            Some(1)
        }
    }
}

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_else(|| "home".into());
    if matches!(arg.as_str(), "-h" | "--help" | "help") {
        println!("{}", ShellCommand::usage());
        println!("socket: {}", ipc::socket_path().display());
        return;
    }
    let Some(cmd) = ShellCommand::parse(&arg) else {
        eprintln!("{}", ShellCommand::usage());
        std::process::exit(2);
    };
    let is_volume = matches!(
        cmd,
        ShellCommand::VolumeUp | ShellCommand::VolumeDown | ShellCommand::Mute
    );
    if let Err(e) = ipc::send(cmd) {
        if is_volume && let Some(code) = volume(&arg) {
            std::process::exit(code);
        }
        eprintln!("catostv-ctl: {e}");
        std::process::exit(1);
    }
}
