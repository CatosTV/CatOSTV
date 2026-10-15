//! Running external system tools safely.
//!
//! Every external command CatosTV runs goes through [`Cmd`] and a [`Runner`]:
//!
//! * no shell is involved (arguments are passed as an argv list, so SSIDs,
//!   passwords and paths can never be interpreted as shell syntax);
//! * stdin is closed, so tools such as `bluetoothctl` can never sit waiting
//!   for interactive input;
//! * every command has a timeout, so a hung daemon can never freeze the UI.
//!
//! Tests and the installer dry-run mode substitute a fake [`Runner`].

use crate::error::{Error, Result};
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Default timeout for quick status queries (`nmcli radio wifi`, ...).
pub const QUICK: Duration = Duration::from_secs(4);
/// Timeout for slower interactive operations (Wi-Fi scan / connect).
pub const SLOW: Duration = Duration::from_secs(45);
/// Timeout for very long installer operations (`pacstrap`).
pub const VERY_LONG: Duration = Duration::from_secs(60 * 60 * 3);

/// A fully specified external command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub timeout: Duration,
}

impl Cmd {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            timeout: QUICK,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Human-readable rendering for logs and the dry-run plan. Arguments that
    /// contain spaces or quotes are single-quoted. This is for display only;
    /// it is never passed to a shell.
    pub fn display(&self) -> String {
        let mut out = quote_for_display(&self.program);
        for a in &self.args {
            out.push(' ');
            out.push_str(&quote_for_display(a));
        }
        out
    }

    /// Like [`Cmd::display`] but replaces the argument following `password`
    /// with `********`, for logs that might be exported.
    pub fn display_redacted(&self) -> String {
        let mut redacted = self.clone();
        let mut hide_next = false;
        for a in &mut redacted.args {
            if hide_next {
                *a = "********".into();
                hide_next = false;
            } else if a == "password" {
                hide_next = true;
            }
        }
        redacted.display()
    }
}

fn quote_for_display(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Result of running a [`Cmd`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    /// Exit code if the process exited normally.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        !self.timed_out && self.code == Some(0)
    }

    /// Convenience constructor used by fakes in tests.
    pub fn ok(stdout: impl Into<String>) -> Self {
        Self {
            code: Some(0),
            stdout: stdout.into(),
            ..Self::default()
        }
    }

    pub fn failed(code: i32, stderr: impl Into<String>) -> Self {
        Self {
            code: Some(code),
            stderr: stderr.into(),
            ..Self::default()
        }
    }

    /// Short description of why a command failed, for error messages.
    pub fn failure_reason(&self) -> String {
        if self.timed_out {
            return "timed out".into();
        }
        let detail = self.stderr.trim();
        let detail = if detail.is_empty() {
            self.stdout.trim()
        } else {
            detail
        };
        let last_line = detail.lines().last().unwrap_or("").trim();
        match (self.code, last_line.is_empty()) {
            (Some(c), true) => format!("exit code {c}"),
            (Some(c), false) => format!("exit code {c}: {last_line}"),
            (None, true) => "terminated by a signal".into(),
            (None, false) => format!("terminated by a signal: {last_line}"),
        }
    }
}

/// Something that can execute commands. The real implementation is
/// [`SystemRunner`]; tests use scripted fakes.
pub trait Runner {
    fn run(&mut self, cmd: &Cmd) -> Result<Output>;

    /// Run and require exit code 0.
    fn run_ok(&mut self, cmd: &Cmd) -> Result<Output> {
        let out = self.run(cmd)?;
        if out.success() {
            Ok(out)
        } else {
            Err(Error::msg(format!(
                "{} failed ({})",
                cmd.program,
                out.failure_reason()
            )))
        }
    }
}

/// Executes commands on the real system.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&mut self, cmd: &Cmd) -> Result<Output> {
        run_with_timeout(cmd)
    }
}

/// Spawn `cmd`, capture stdout/stderr, kill it if it exceeds its timeout.
pub fn run_with_timeout(cmd: &Cmd) -> Result<Output> {
    let mut child = Command::new(&cmd.program)
        .args(&cmd.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::msg(format!("{} is not installed", cmd.program))
            } else {
                Error::msg(format!("could not start {}: {e}", cmd.program))
            }
        })?;

    // Drain pipes on helper threads so a chatty process cannot block on a
    // full pipe while we wait for it.
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });
    let err_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });

    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None if start.elapsed() >= cmd.timeout => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    };

    let stdout = String::from_utf8_lossy(&out_thread.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_thread.join().unwrap_or_default()).into_owned();
    Ok(Output {
        code: if timed_out {
            None
        } else {
            status.and_then(|s| s.code())
        },
        stdout,
        stderr,
        timed_out,
    })
}

/// Like [`run_with_timeout`], but hands every stdout line (split on `\n`
/// and `\r`, so progress output from pacman/curl arrives as it happens) to
/// `on_line` while the command runs. The full output is still returned.
pub fn run_with_lines(cmd: &Cmd, on_line: &mut dyn FnMut(&str)) -> Result<Output> {
    use std::sync::mpsc;
    let mut child = Command::new(&cmd.program)
        .args(&cmd.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::msg(format!("{} is not installed", cmd.program))
            } else {
                Error::msg(format!("could not start {}: {e}", cmd.program))
            }
        })?;
    let (tx, rx) = mpsc::channel::<String>();
    let mut stdout = child.stdout.take();
    let out_thread = thread::spawn(move || {
        let mut all = Vec::new();
        let mut line = Vec::new();
        let mut buf = [0u8; 4096];
        if let Some(s) = stdout.as_mut() {
            while let Ok(n) = s.read(&mut buf) {
                if n == 0 {
                    break;
                }
                all.extend_from_slice(&buf[..n]);
                for &b in &buf[..n] {
                    if b == b'\n' || b == b'\r' {
                        if !line.is_empty() {
                            let _ = tx.send(String::from_utf8_lossy(&line).into_owned());
                            line.clear();
                        }
                    } else {
                        line.push(b);
                    }
                }
            }
        }
        if !line.is_empty() {
            let _ = tx.send(String::from_utf8_lossy(&line).into_owned());
        }
        all
    });
    let mut stderr = child.stderr.take();
    let err_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        while let Ok(l) = rx.try_recv() {
            on_line(&l);
        }
        match child.try_wait()? {
            Some(status) => break Some(status),
            None if start.elapsed() >= cmd.timeout => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            None => thread::sleep(Duration::from_millis(50)),
        }
    };
    let stdout = String::from_utf8_lossy(&out_thread.join().unwrap_or_default()).into_owned();
    while let Ok(l) = rx.try_recv() {
        on_line(&l);
    }
    let stderr = String::from_utf8_lossy(&err_thread.join().unwrap_or_default()).into_owned();
    Ok(Output {
        code: if timed_out {
            None
        } else {
            status.and_then(|s| s.code())
        },
        stdout,
        stderr,
        timed_out,
    })
}

/// Find an executable on `$PATH` (or check an absolute/relative path).
pub fn find_executable(name: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let is_exec = |p: &std::path::Path| {
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    };
    if name.is_empty() {
        return None;
    }
    if name.contains('/') {
        let p = std::path::PathBuf::from(name);
        return is_exec(&p).then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| is_exec(p))
}

#[cfg(test)]
pub mod fake {
    //! Scripted runner for unit tests.
    use super::*;
    use std::collections::VecDeque;

    /// Returns queued responses in order and records every command it saw.
    #[derive(Default)]
    pub struct FakeRunner {
        pub seen: Vec<Cmd>,
        pub responses: VecDeque<Output>,
        /// Response used when the queue is empty.
        pub default: Output,
    }

    impl FakeRunner {
        pub fn new() -> Self {
            Self {
                default: Output::ok(""),
                ..Self::default()
            }
        }
        pub fn push(&mut self, out: Output) -> &mut Self {
            self.responses.push_back(out);
            self
        }
    }

    impl Runner for FakeRunner {
        fn run(&mut self, cmd: &Cmd) -> Result<Output> {
            self.seen.push(cmd.clone());
            Ok(self
                .responses
                .pop_front()
                .unwrap_or_else(|| self.default.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout_and_exit_code() {
        let out = run_with_timeout(&Cmd::new("sh").args(["-c", "echo hi; exit 3"])).unwrap();
        assert_eq!(out.stdout.trim(), "hi");
        assert_eq!(out.code, Some(3));
        assert!(!out.success());
        assert_eq!(out.failure_reason(), "exit code 3: hi");
    }

    #[test]
    fn kills_hung_process_after_timeout() {
        let start = Instant::now();
        let out = run_with_timeout(
            &Cmd::new("sleep")
                .arg("5")
                .timeout(Duration::from_millis(200)),
        )
        .unwrap();
        assert!(out.timed_out);
        assert!(!out.success());
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn stdin_is_closed_so_interactive_tools_cannot_block() {
        // `cat` with no args reads stdin; with stdin=/dev/null it exits at once.
        let out = run_with_timeout(&Cmd::new("cat").timeout(Duration::from_secs(2))).unwrap();
        assert!(out.success());
    }

    #[test]
    fn missing_program_is_a_readable_error() {
        let err = run_with_timeout(&Cmd::new("definitely-not-a-real-tool-xyz")).unwrap_err();
        assert_eq!(
            err.message(),
            "definitely-not-a-real-tool-xyz is not installed"
        );
    }

    #[test]
    fn display_quotes_and_redacts() {
        let c =
            Cmd::new("nmcli").args(["device", "wifi", "connect", "My Net", "password", "s3cret"]);
        assert_eq!(
            c.display(),
            "nmcli device wifi connect 'My Net' password s3cret"
        );
        assert_eq!(
            c.display_redacted(),
            "nmcli device wifi connect 'My Net' password '********'"
        );
    }

    #[test]
    fn find_executable_finds_sh() {
        assert!(find_executable("sh").is_some());
        assert!(find_executable("no-such-binary-catostv").is_none());
        assert!(find_executable("").is_none());
    }
}
