//! Shell IPC: compositor shortcuts → running `catostv-shell`.
//!
//! Transport: one Unix **datagram** per command, plain ASCII, no reply.
//! Socket: `$XDG_RUNTIME_DIR/catostv-shell.sock`
//! (fallback `/tmp/catostv-shell-<uid>.sock` when `XDG_RUNTIME_DIR` is unset).
//!
//! The protocol is documented in docs/ARCHITECTURE.md ("IPC"). To add a
//! command: add a variant to [`ShellCommand`], extend `ALL`, `as_str` and
//! `parse`, handle it in `shell::Shell::handle_ipc`, and update the docs.

use crate::error::{Error, Result};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::thread;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellCommand {
    /// Go to the Home page and close overlays.
    Home,
    /// Open the Control Center over Home.
    Quick,
    Settings,
    Search,
    Apps,
    /// Open the power menu (bound to the hardware power key).
    Power,
    /// Re-read `apps.toml`.
    Reload,
    /// Sent by `catostv-inputd` when the user repeats or holds Back inside an
    /// app (the "escape gesture"). The shell returns to Home only if an app
    /// is in front; inside CatosTV's own screens it is ignored.
    Escape,
    /// Navigation from a game controller (`catostv-inputd`).
    NavUp,
    NavDown,
    NavLeft,
    NavRight,
    NavOk,
    NavBack,
    NavMenu,
    /// `catostv-inputd` captured a controller button for Settings →
    /// Controller navigation (the code is in `pad::CAPTURED_FILE`).
    PadCaptured,
    /// The volume keys (labwc runs `catostv-ctl volume-up` and so on).
    VolumeUp,
    VolumeDown,
    Mute,
}

impl ShellCommand {
    pub const ALL: [ShellCommand; 19] = [
        Self::Home,
        Self::Quick,
        Self::Settings,
        Self::Search,
        Self::Apps,
        Self::Power,
        Self::Reload,
        Self::Escape,
        Self::NavUp,
        Self::NavDown,
        Self::NavLeft,
        Self::NavRight,
        Self::NavOk,
        Self::NavBack,
        Self::NavMenu,
        Self::PadCaptured,
        Self::VolumeUp,
        Self::VolumeDown,
        Self::Mute,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Quick => "quick",
            Self::Settings => "settings",
            Self::Search => "search",
            Self::Apps => "apps",
            Self::Power => "power",
            Self::Reload => "reload",
            Self::Escape => "escape",
            Self::NavUp => "nav-up",
            Self::NavDown => "nav-down",
            Self::NavLeft => "nav-left",
            Self::NavRight => "nav-right",
            Self::NavOk => "nav-ok",
            Self::NavBack => "nav-back",
            Self::NavMenu => "nav-menu",
            Self::PadCaptured => "pad-captured",
            Self::VolumeUp => "volume-up",
            Self::VolumeDown => "volume-down",
            Self::Mute => "mute",
        }
    }

    /// The navigation action of a `nav-*` command.
    pub fn nav_action(self) -> Option<crate::input::Action> {
        use crate::input::Action;
        Some(match self {
            Self::NavUp => Action::Up,
            Self::NavDown => Action::Down,
            Self::NavLeft => Action::Left,
            Self::NavRight => Action::Right,
            Self::NavOk => Action::Ok,
            Self::NavBack => Action::Back,
            Self::NavMenu => Action::Options,
            _ => return None,
        })
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    pub fn usage() -> String {
        let names: Vec<&str> = Self::ALL.iter().map(|c| c.as_str()).collect();
        format!("usage: catostv-ctl <{}>", names.join("|"))
    }
}

fn current_uid() -> u32 {
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(0)
}

pub fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("catostv-shell.sock"),
        None => PathBuf::from(format!("/tmp/catostv-shell-{}.sock", current_uid())),
    }
}

pub fn send(command: ShellCommand) -> Result<()> {
    send_to(&socket_path(), command)
}

pub fn send_to(path: &Path, command: ShellCommand) -> Result<()> {
    let sock = UnixDatagram::unbound()?;
    sock.send_to(command.as_str().as_bytes(), path)
        .map_err(|e| {
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) {
                Error::msg(format!(
                    "CatosTV shell is not running (no socket at {})",
                    path.display()
                ))
            } else {
                Error::msg(format!("sending '{}' failed: {e}", command.as_str()))
            }
        })?;
    Ok(())
}

/// Bind the socket and deliver each valid command to `on_command` from a
/// background thread. Unknown datagrams are ignored. Returns an error if the
/// socket cannot be bound (the shell keeps working without IPC).
pub fn spawn_listener<F>(path: PathBuf, on_command: F) -> Result<thread::JoinHandle<()>>
where
    F: Fn(ShellCommand) + Send + 'static,
{
    // A stale socket from a previous (crashed) shell would make bind fail.
    let _ = std::fs::remove_file(&path);
    let sock = UnixDatagram::bind(&path)
        .map_err(|e| Error::msg(format!("IPC bind failed at {}: {e}", path.display())))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    let handle = thread::Builder::new()
        .name("catostv-ipc".into())
        .spawn(move || {
            let mut buf = [0u8; 128];
            loop {
                match sock.recv(&mut buf) {
                    Ok(n) => {
                        if let Some(cmd) = std::str::from_utf8(&buf[..n])
                            .ok()
                            .and_then(ShellCommand::parse)
                        {
                            on_command(cmd);
                        }
                    }
                    Err(e) => {
                        eprintln!("catostv-shell: IPC receive failed: {e}");
                        break;
                    }
                }
            }
        })?;
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn parse_round_trip() {
        for c in ShellCommand::ALL {
            assert_eq!(ShellCommand::parse(c.as_str()), Some(c));
        }
        assert_eq!(ShellCommand::parse(" home\n"), Some(ShellCommand::Home));
        assert_eq!(ShellCommand::parse("rm -rf"), None);
        assert!(ShellCommand::usage().contains("home|quick|settings"));
    }

    #[test]
    fn send_and_receive_over_socket() {
        let path =
            std::env::temp_dir().join(format!("catostv-ipc-test-{}.sock", std::process::id()));
        let (tx, rx) = mpsc::channel();
        spawn_listener(path.clone(), move |c| {
            let _ = tx.send(c);
        })
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        send_to(&path, ShellCommand::Quick).unwrap();
        send_to(&path, ShellCommand::Power).unwrap();
        // Garbage is ignored, not delivered.
        UnixDatagram::unbound()
            .unwrap()
            .send_to(b"garbage", &path)
            .unwrap();
        send_to(&path, ShellCommand::Home).unwrap();
        let got: Vec<_> = (0..3)
            .map(|_| rx.recv_timeout(Duration::from_secs(2)).unwrap())
            .collect();
        assert_eq!(
            got,
            vec![ShellCommand::Quick, ShellCommand::Power, ShellCommand::Home]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sending_without_shell_gives_clear_error() {
        let err = send_to(Path::new("/nonexistent/catostv.sock"), ShellCommand::Home).unwrap_err();
        assert!(err.message().contains("not running"), "{err}");
    }
}
