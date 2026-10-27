//! Switching labwc workspaces from the shell.
//!
//! labwc has no command IPC, but it does honour key bindings coming from
//! virtual keyboards (matched by keysym). CatosTV therefore reserves two key
//! chords that no remote produces and binds them in the installed
//! `rc.xml`:
//!
//! | Chord        | labwc action        | Used for                          |
//! |--------------|---------------------|-----------------------------------|
//! | `W-A-F11`    | GoToDesktop 1       | return to CatosTV after an app exits |
//! | `W-A-F12`    | GoToDesktop 2       | resume a running app              |
//! | `W-A-F9`     | shell window onto desktop 2, on top | Quick settings or the volume panel over an app |
//! | `W-A-F10`    | shell window back to desktop 1 | that closed again           |
//!
//! The shell "presses" them with `wtype`. A unit test checks that rc.xml
//! still contains the bindings.
//!
//! Status: designed against labwc's keyboard.c (virtual keyboards fall back
//! to keysym matching). Runtime-UNVERIFIED until tested on an appliance.

use crate::error::{Error, Result};
use crate::process::{Cmd, Runner};

pub const SHOW_SHELL_KEYBIND: &str = "W-A-F11";
pub const SHOW_APP_KEYBIND: &str = "W-A-F12";
pub const OVER_APP_KEYBIND: &str = "W-A-F9";
pub const LEAVE_APP_KEYBIND: &str = "W-A-F10";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AndroidRecovery {
    Shell,
    App,
}

/// Android may open a window on its own, even while the shell still draws.
pub fn android_recovery(
    android_activated: bool,
    shell_expected: bool,
    android_expected: bool,
) -> Option<AndroidRecovery> {
    if !android_activated {
        None
    } else if shell_expected {
        Some(AndroidRecovery::Shell)
    } else if !android_expected {
        Some(AndroidRecovery::App)
    } else {
        None
    }
}

fn chord(key: &str) -> Cmd {
    Cmd::new("wtype").args([
        "-M", "logo", "-M", "alt", "-k", key, "-m", "alt", "-m", "logo",
    ])
}

fn press(r: &mut dyn Runner, key: &str, appliance: bool) -> Result<()> {
    if !appliance {
        return Err(Error::msg(
            "Switching to the app needs the CatosTV appliance session",
        ));
    }
    // One chord at a time: two overlapping ones (an app's window going while
    // the next one opens) can let a key through without its modifiers.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    r.run_ok(&chord(key))?;
    Ok(())
}

/// Show the App workspace (resume the running app).
pub fn show_app_workspace(r: &mut dyn Runner, appliance: bool) -> Result<()> {
    press(r, "F12", appliance)
}

/// Show the CatosTV workspace (e.g. after the foreground app exited).
pub fn show_shell_workspace(r: &mut dyn Runner, appliance: bool) -> Result<()> {
    press(r, "F11", appliance)
}

/// Bring the CatosTV window onto the App desktop, on top of the app, with
/// the keys (Quick settings or the volume panel over an app).
pub fn shell_over_app(r: &mut dyn Runner, appliance: bool) -> Result<()> {
    press(r, "F9", appliance)
}

/// Send the CatosTV window back to its own desktop (the app stays in front).
pub fn shell_off_app(r: &mut dyn Runner, appliance: bool) -> Result<()> {
    press(r, "F10", appliance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::fake::FakeRunner;

    #[test]
    fn android_yields_to_the_requested_screen() {
        use AndroidRecovery::{App, Shell};
        assert_eq!(android_recovery(true, true, false), Some(Shell));
        // The last app may still be Android after returning to Home.
        assert_eq!(android_recovery(true, true, true), Some(Shell));
        assert_eq!(android_recovery(true, false, false), Some(App));
        assert_eq!(android_recovery(true, false, true), None);
        assert_eq!(android_recovery(false, true, false), None);
    }

    #[test]
    fn android_windows_never_switch_the_desktop() {
        // A window Android opens on its own (boot, crash, updater) must not
        // pull the TV away from CatosTV; only the shell switches desktops.
        let rc = std::fs::read_to_string(format!(
            "{}/../../target-root/home/catostv/.config/labwc/rc.xml",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let q = rc
            .find("<query identifier=\"waydroid*\" />")
            .expect("waydroid query");
        let then: String = rc[q..q + 700]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let t = then.find("<then>").expect("then");
        let e = then.find("</then>").expect("end of then");
        let branch = &then[t..e];
        assert!(
            branch.contains("<action name=\"SendToDesktop\" to=\"2\" follow=\"no\" />"),
            "{branch}"
        );
        // No Maximize for Android windows: a sized configure is a display
        // re-plug to Waydroid's HAL.
        assert!(!branch.contains("<action name=\"Maximize\""), "{branch}");
    }

    #[test]
    fn wtype_chord() {
        let mut r = FakeRunner::new();
        show_app_workspace(&mut r, true).unwrap();
        assert_eq!(
            r.seen[0].display(),
            "wtype -M logo -M alt -k F12 -m alt -m logo"
        );
        assert!(show_shell_workspace(&mut r, false).is_err());
        assert_eq!(r.seen.len(), 1);
    }

    #[test]
    fn installed_rc_xml_binds_the_reserved_chords() {
        let rc = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target-root/home/catostv/.config/labwc/rc.xml"
        ))
        .expect("rc.xml present in repo");
        let block = |chord: &str| {
            let start = rc
                .find(&format!("key=\"{chord}\""))
                .unwrap_or_else(|| panic!("rc.xml lacks keybind {chord}"));
            rc[start..start + rc[start..].find("</keybind>").unwrap()].to_string()
        };
        for (chord, desktop) in [(SHOW_SHELL_KEYBIND, "1"), (SHOW_APP_KEYBIND, "2")] {
            let b = block(chord);
            assert!(
                b.contains(&format!("GoToDesktop\" to=\"{desktop}\"")),
                "{chord} must go to desktop {desktop}: {b}"
            );
        }
        for (chord, desktop) in [(OVER_APP_KEYBIND, "2"), (LEAVE_APP_KEYBIND, "1")] {
            let b = block(chord);
            assert!(b.contains("<query identifier=\"catostv-shell\" />"), "{b}");
            assert!(
                b.contains(&format!("SendToDesktop\" to=\"{desktop}\" follow=\"no\"")),
                "{chord}: {b}"
            );
        }
        // Every way to the CatosTV desktop takes the window back there
        // first, in case it was over an app.
        for (at, _) in rc.match_indices("<action name=\"GoToDesktop\" to=\"1\" />") {
            let before = &rc[..at];
            let kb = before.rfind("<keybind").unwrap();
            assert!(
                before[kb..].contains("<query identifier=\"catostv-shell\" />"),
                "{}",
                &rc[kb..at]
            );
        }
    }
}
