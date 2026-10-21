//! # catostv-core
//!
//! Display-independent logic for CatosTV. **Standard library only**, this
//! crate must build and test with no network access and no GUI stack, so
//! the parts of CatosTV most likely to hurt someone (disk selection, install
//! plan, process control) are always verifiable.
//!
//! Module map:
//!
//! | Area            | Modules                                              |
//! |-----------------|------------------------------------------------------|
//! | Foundations     | `error`, `process` (timeouts, no shell), `env`       |
//! | UI support      | `prefs` (accent, first run), `fonts` (TTC faces)     |
//! | Remote input    | `input` (keys → actions, long press), `osk`          |
//! | Escape gesture  | `gesture` (repeat/hold Back), `evdev` (devices)      |
//! | Shell UX        | `shell` (navigation state machine), `settings`       |
//! | Installer UX    | `installer` (wizard state machine), `timezone`,      |
//! |                 | `keyboard` (XKB layouts)                             |
//! | Installer core  | `disk` (target safety), `install` (plan + executor)  |
//! | Apps            | `apps` (registry model), `lifecycle` (processes)     |
//! | App sources     | `desktop` (Flatpak/Waydroid discovery), `flatpak`,   |
//! |                 | `icon_theme` (Papirus icons)                         |
//! |                 | (Flathub store), `android` (Waydroid, USB APKs)      |
//! | System adapters | `net`, `bluetooth`, `audio`, `power`, `compositor`,  |
//! |                 | `sysinfo` (free storage)                             |
//! | Devices         | `display` (wlr-randr), `cec` (HDMI-CEC → keys)       |
//! | IPC             | `ipc` (compositor shortcuts → shell)                 |
//! | LAN daemons     | `http`, `ssdp`, `signal` (castd / remoted plumbing)  |
//! | Casting         | `dlna` (MediaRenderer), `mpv` (player), `dial` (YouTube) |
//! | Phone remote    | `remote` (web remote), `kdeconnect` (KDE Connect)    |

pub mod env;
pub mod error;
pub mod fonts;
pub mod process;

pub mod input;
pub mod osk;

pub mod evdev;
pub mod gamepad;
pub mod gesture;
pub mod gpu;

pub mod installer;
pub mod keyboard;
pub mod prefs;
pub mod settings;
pub mod shell;
pub mod timezone;

pub mod disk;
pub mod install;

pub mod apps;
pub mod lifecycle;

pub mod android;
pub mod cattube;
pub mod debug;
pub mod desktop;
pub mod files;
pub mod fin;
pub mod flatpak;
pub mod icon_theme;
pub mod jellyfin;
pub mod plex;
pub mod sysinfo;

pub mod audio;
pub mod bluetooth;
pub mod cec;
pub mod compositor;
pub mod display;
pub mod net;
pub mod power;
pub mod toplevel;
pub mod uinput;
pub mod updates;
pub mod upnext;
pub mod usage;
pub mod wlfilter;

pub mod ipc;

pub mod http;
pub mod signal;
pub mod ssdp;

pub mod dial;
pub mod dlna;
pub mod mpv;

pub mod kdeconnect;
pub mod remote;
pub mod screensaver;
pub mod volume_panel;

pub use error::{Error, Result};

#[cfg(test)]
mod testutil;

#[cfg(test)]
mod repo_consistency {
    //! Cross-file checks that keep config files and code in sync.

    fn repo_file(rel: &str) -> String {
        std::fs::read_to_string(format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    /// Minimal reader for the simple `[[app]]` / `key = "value"` subset used
    /// by apps.toml (the real parser lives in the GUI crate).
    fn read_apps_toml(text: &str) -> Vec<crate::apps::AppSpec> {
        let mut apps: Vec<crate::apps::AppSpec> = Vec::new();
        for line in text.lines().map(str::trim) {
            if line == "[[app]]" {
                apps.push(Default::default());
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            if line.starts_with('#') {
                continue;
            }
            let v = v.trim().trim_matches('"').to_string();
            let Some(a) = apps.last_mut() else { continue };
            match k.trim() {
                "name" => a.name = v,
                "subtitle" => a.subtitle = Some(v),
                "icon" => a.icon = Some(v),
                "accent" => a.accent = Some(v),
                "command" => a.command = v,
                _ => {}
            }
        }
        apps
    }

    #[test]
    fn shipped_apps_toml_matches_builtin_defaults() {
        let shipped = read_apps_toml(&repo_file(
            "target-root/home/catostv/.config/catostv/apps.toml",
        ));
        assert_eq!(shipped, crate::apps::default_specs());
    }

    #[test]
    fn firefox_profiles_referenced_by_apps_exist() {
        let mut argvs: Vec<Vec<String>> = crate::apps::defaults()
            .into_iter()
            .map(|a| a.argv().unwrap())
            .collect();
        argvs.push(crate::dial::youtube_command().unwrap());
        let mut checked = 0;
        for argv in argvs {
            if let Some(i) = argv.iter().position(|a| a == "--profile") {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/home/catostv".into());
                let rel = argv[i + 1].replacen(&home, "home/catostv", 1);
                repo_file(&format!("target-root/{rel}/user.js"));
                checked += 1;
            }
        }
        assert!(checked >= 1, "the YouTube cast profile is checked");
    }

    #[test]
    fn session_scripts_do_not_use_private_dbus() {
        for f in [
            "target-root/usr/local/bin/catostv-session",
            "iso-overlay/usr-local-bin-catostv-live-session",
        ] {
            let text = repo_file(f);
            assert!(
                !text
                    .lines()
                    .any(|l| !l.trim_start().starts_with('#') && l.contains("dbus-run-session")),
                "{f} must use the systemd user bus"
            );
        }
    }

    #[test]
    fn greetd_never_falls_back_to_a_login_prompt() {
        for f in [
            "target-root/etc/greetd/config.toml",
            "iso-overlay/etc/greetd/config.toml",
        ] {
            let text = repo_file(f);
            assert!(!text.contains("agreety --cmd"), "{f}");
        }
    }

    #[test]
    fn every_installed_binary_is_built_into_the_iso() {
        let script = repo_file("scripts/build-iso.sh");
        let line = script
            .lines()
            .find(|l| l.starts_with("BINARIES=("))
            .expect("BINARIES= line in build-iso.sh");
        for b in crate::install::INSTALLED_BINARIES {
            assert!(line.contains(b), "build-iso.sh BINARIES lacks {b}");
        }
        // Their modes come from the same list (mkarchiso drops modes).
        assert!(script.contains(r#"for b in "${BINARIES[@]}" catostv-live-session; do"#));
        assert!(script.contains(r#"[\"/usr/local/bin/$b\"]=\"0:0:755\""#));
    }

    #[test]
    fn deploy_script_pushes_every_installed_binary() {
        let script = repo_file("scripts/deploy.sh");
        let line = script
            .lines()
            .find(|l| l.starts_with("BINS=("))
            .expect("BINS= line in deploy.sh");
        let listed: Vec<&str> = line
            .trim_start_matches("BINS=(")
            .trim_end_matches(')')
            .split_whitespace()
            .collect();
        assert_eq!(listed, crate::install::INSTALLED_BINARIES);
    }

    #[test]
    fn session_daemons_are_supervised() {
        let autostart = repo_file("target-root/home/catostv/.config/labwc/autostart");
        for b in [
            "catostv-shell",
            "catostv-inputd",
            "catostv-cecd",
            "catostv-castd",
            "catostv-remoted",
            "catostv-wlfilter",
        ] {
            assert!(
                autostart
                    .lines()
                    .any(|l| l.trim_start().starts_with("supervise ")
                        && l.contains(&format!("/usr/local/bin/{b}"))),
                "autostart does not supervise {b}"
            );
        }
    }

    #[test]
    fn home_key_is_global_in_the_installed_session() {
        let rc = repo_file("target-root/home/catostv/.config/labwc/rc.xml");
        for key in ["XF86HomePage", "W-Home", "Home"] {
            let start = rc
                .find(&format!("key=\"{key}\""))
                .unwrap_or_else(|| panic!("rc.xml lacks {key}"));
            let block = &rc[start..start + rc[start..].find("</keybind>").unwrap()];
            assert!(block.contains("catostv-ctl home"), "{key}: {block}");
        }
    }

    #[test]
    fn window_rules_never_toggle_fullscreen() {
        for f in [
            "target-root/home/catostv/.config/labwc/rc.xml",
            "iso-overlay/root/.config/labwc/rc.xml",
        ] {
            // Window rules only (the green window button and F11
            // toggle full screen on purpose; a rule must never).
            let text = repo_file(f);
            let rules = text
                .split("<windowRules>")
                .nth(1)
                .and_then(|r| r.split("</windowRules>").next())
                .unwrap_or("");
            assert!(!rules.contains("name=\"ToggleFullscreen\""), "{f}");
        }
    }
}
