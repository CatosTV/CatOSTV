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
//!
//! The shell "presses" them with `wtype`. A unit test checks that rc.xml
//! still contains both bindings.
//!
//! Status: designed against labwc's keyboard.c (virtual keyboards fall back
//! to keysym matching). Runtime-UNVERIFIED until tested on an appliance.

use crate::error::{Error, Result};
use crate::process::{Cmd, Runner};

pub const SHOW_SHELL_KEYBIND: &str = "W-A-F11";
pub const SHOW_APP_KEYBIND: &str = "W-A-F12";

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::fake::FakeRunner;

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
        for (chord, desktop) in [(SHOW_SHELL_KEYBIND, "1"), (SHOW_APP_KEYBIND, "2")] {
            let start = rc
                .find(&format!("key=\"{chord}\""))
                .unwrap_or_else(|| panic!("rc.xml lacks keybind {chord}"));
            let block = &rc[start..start + rc[start..].find("</keybind>").unwrap()];
            assert!(
                block.contains(&format!("GoToDesktop\" to=\"{desktop}\"")),
                "{chord} must go to desktop {desktop}: {block}"
            );
        }
    }
}
