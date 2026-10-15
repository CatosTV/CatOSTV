//! Settings → Debug: SSH access for development, the login it
//! uses, and facts a developer asks for first (display relay, GPU, build).
//!
//! * **Debug SSH** is off by default and off again after every boot
//!   (`catostv-debug-boot.service`). Switching it on (root unit
//!   `catostv-debug-ssh-on.service`, `/usr/local/lib/catostv/debug-ssh on`)
//!   starts `sshd` until the next boot, gives the TV account `catostv` a
//!   password the first time (`catostv`) and lets it use `sudo` with that
//!   password while SSH is on. Off stops both.
//! * **Change password** stages the new password in a file only the TV
//!   account can read ([`PASSWORD_STAGE`]); the root unit
//!   `catostv-debug-password.service` sets it with `chpasswd` and deletes
//!   the file.
//! * The old development login `ok` stays until a login as `catostv` over
//!   SSH has been seen; then "Remove the old ok login" appears
//!   (`catostv-debug-remove-ok.service`, which checks again itself).
//!
//! Everything here only reads files the TV account can read; nothing needs
//! root.

use crate::error::{Error, Result};
use crate::process::{Cmd, Runner};
use std::path::Path;
use std::time::Duration;

pub const SSH_ON_UNIT: &str = "catostv-debug-ssh-on.service";
pub const SSH_OFF_UNIT: &str = "catostv-debug-ssh-off.service";
pub const PASSWORD_UNIT: &str = "catostv-debug-password.service";
pub const REMOVE_OK_UNIT: &str = "catostv-debug-remove-ok.service";

/// Written by `debug-ssh` ("on" / "off"); gone after a reboot.
pub const SSH_STATE: &str = "/run/catostv/debug-ssh";
/// The new password, staged by the shell for [`PASSWORD_UNIT`].
pub const PASSWORD_STAGE: &str = "/run/catostv/debug/password";
/// Exists once the password is no longer the default.
pub const PASSWORD_CHANGED: &str = "/var/lib/catostv/debug/password-changed";
/// The login (and default password) shown on the Debug page.
pub const LOGIN: &str = "catostv";
pub const DEFAULT_PASSWORD: &str = "catostv";
/// The old development login.
pub const OLD_LOGIN: &str = "ok";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DebugStatus {
    pub ssh_on: bool,
    pub password_changed: bool,
    /// Someone is (or was, this boot) logged in as `catostv` over SSH.
    pub ssh_login_seen: bool,
    pub old_login_present: bool,
    /// The Android display relay (`catostv-wlfilter`) is running.
    pub relay_running: bool,
    /// "Intel (i915)", one entry per GPU.
    pub gpus: Vec<String>,
}

/// Read the status. `relay_running` comes from the caller's process check.
pub fn status(relay_running: bool) -> DebugStatus {
    DebugStatus {
        ssh_on: std::fs::read_to_string(SSH_STATE).is_ok_and(|s| s.trim() == "on"),
        password_changed: Path::new(PASSWORD_CHANGED).exists(),
        ssh_login_seen: login_seen(),
        old_login_present: std::fs::read_to_string("/etc/passwd")
            .is_ok_and(|p| has_account(&p, OLD_LOGIN)),
        relay_running,
        gpus: gpus(Path::new("/sys/class/drm")),
    }
}

/// A `catostv` SSH login has worked (now, or once before: remembered in
/// the TV account's state folder, since sessions are short).
fn login_seen() -> bool {
    let flag = crate::env::state_dir().join("debug-login-seen");
    if flag.exists() {
        return true;
    }
    let now = ssh_session_of(Path::new("/run/systemd/sessions"), LOGIN);
    if now {
        let _ = std::fs::write(&flag, "");
    }
    now
}

pub fn has_account(passwd: &str, name: &str) -> bool {
    passwd.lines().any(|l| l.split(':').next() == Some(name))
}

/// An SSH session of `user` in systemd-logind's session files
/// (`/run/systemd/sessions/<id>`, readable by everyone).
pub fn ssh_session_of(dir: &Path, user: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.flatten()
        .any(|e| std::fs::read_to_string(e.path()).is_ok_and(|s| is_ssh_session(&s, user)))
}

pub fn is_ssh_session(session_file: &str, user: &str) -> bool {
    let get = |k: &str| {
        session_file
            .lines()
            .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
    };
    get("USER") == Some(user) && get("SERVICE") == Some("sshd")
}

/// The GPUs as "Vendor (driver)" from `/sys/class/drm/card*/device`.
pub fn gpus(drm: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(drm) else {
        return Vec::new();
    };
    let mut cards: Vec<_> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            n.starts_with("card") && n[4..].chars().all(|c| c.is_ascii_digit()) && n.len() > 4
        })
        .collect();
    cards.sort();
    cards
        .into_iter()
        .filter_map(|c| {
            let dev = drm.join(&c).join("device");
            let vendor = std::fs::read_to_string(dev.join("vendor")).ok()?;
            let driver = std::fs::read_link(dev.join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()));
            Some(gpu_label(vendor.trim(), driver.as_deref()))
        })
        .collect()
}

pub fn gpu_label(vendor_id: &str, driver: Option<&str>) -> String {
    let vendor = match vendor_id {
        "0x8086" => "Intel",
        "0x1002" => "AMD",
        "0x10de" => "NVIDIA",
        "0x1af4" => "Virtual",
        other => other,
    };
    match driver {
        Some(d) => format!("{vendor} ({d})"),
        None => format!("{vendor} (no driver)"),
    }
}

/// Run one of the Debug root units and wait for it (they are oneshots;
/// polkit lets the TV account start exactly these).
pub fn run_unit(r: &mut dyn Runner, unit: &str, what: &str) -> Result<()> {
    let out = r.run(
        &Cmd::new("systemctl")
            .args(["--no-ask-password", "start", unit])
            .timeout(Duration::from_secs(60)),
    )?;
    if out.success() {
        Ok(())
    } else {
        Err(Error::msg(format!(
            "{what} failed: {}",
            out.failure_reason()
        )))
    }
}

/// A password the root helper accepts: 4–128 characters, no line breaks
/// or colons (`chpasswd` reads `user:password` lines).
pub fn password_ok(pw: &str) -> std::result::Result<(), &'static str> {
    let n = pw.chars().count();
    if n < 4 {
        return Err("Use at least 4 characters");
    }
    if n > 128 {
        return Err("Use at most 128 characters");
    }
    if pw.chars().any(|c| c == ':' || c.is_control()) {
        return Err("Colons and control characters can't be used");
    }
    Ok(())
}

/// Write the new password for [`PASSWORD_UNIT`]: a fresh file, mode 0600,
/// never through a symlink.
pub fn stage_password(path: &Path, pw: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(pw.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_and_sessions() {
        let passwd = "root:x:0:0::/root:/bin/bash\nok:x:1001:1001::/home/ok:/usr/bin/bash\n";
        assert!(has_account(passwd, "ok"));
        assert!(!has_account(passwd, "o"));
        let ssh = "UID=1000\nUSER=catostv\nSERVICE=sshd\nREMOTE=1\n";
        assert!(is_ssh_session(ssh, "catostv"));
        assert!(!is_ssh_session(ssh, "ok"));
        assert!(!is_ssh_session("USER=catostv\nSERVICE=greetd\n", "catostv"));
    }

    #[test]
    fn gpu_labels() {
        assert_eq!(gpu_label("0x8086", Some("i915")), "Intel (i915)");
        assert_eq!(gpu_label("0x10de", Some("nvidia")), "NVIDIA (nvidia)");
        assert_eq!(gpu_label("0x1234", None), "0x1234 (no driver)");
    }

    #[test]
    fn gpus_from_a_fake_sysfs() {
        let dir = std::env::temp_dir().join(format!("catostv-drm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dev = dir.join("card1/device");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::create_dir_all(dir.join("card1-HDMI-A-1")).unwrap();
        std::fs::write(dev.join("vendor"), "0x1002\n").unwrap();
        std::os::unix::fs::symlink("../../bus/pci/drivers/amdgpu", dev.join("driver")).unwrap();
        assert_eq!(gpus(&dir), vec!["AMD (amdgpu)".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn passwords() {
        assert!(password_ok("catostv").is_ok());
        assert!(password_ok("abc").is_err());
        assert!(password_ok("a:bcd").is_err());
        assert!(password_ok("ab\ncd").is_err());
        assert!(password_ok(&"x".repeat(129)).is_err());
    }

    #[test]
    fn staged_password_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let p = std::env::temp_dir().join(format!("catostv-pw-{}", std::process::id()));
        stage_password(&p, "secret1").unwrap();
        stage_password(&p, "secret2").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "secret2");
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_file(&p);
    }
}
