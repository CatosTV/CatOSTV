//! App lifecycle manager: launch, track, close and detect crashes.
//!
//! Every app is started directly (no shell) in its **own process group**.
//! The group id lets CatosTV:
//!
//! * know an app is still running even if its launcher process exits after
//!   starting the real program (common for `steam`, `spotify-launcher`);
//! * close the whole app (SIGTERM to the group, SIGKILL after a grace period);
//! * reap every child, so no zombie processes accumulate in the shell.
//!
//! Window focus is not handled here, see [`crate::compositor`] for how the
//! shell switches the compositor between the CatosTV and App workspaces.

use crate::apps::AppEntry;
use crate::error::{Error, Result};
use std::collections::HashMap;
use std::fs::File;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// How long a closing app gets after SIGTERM before SIGKILL.
pub const CLOSE_GRACE: Duration = Duration::from_secs(5);

const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
const EPERM: i32 = 1;

mod ffi {
    unsafe extern "C" {
        // POSIX kill(2). pid_t is i32 on Linux.
        pub fn kill(pid: i32, sig: i32) -> i32;
    }
}

/// Send `sig` to every process in group `pgid`.
///
/// Refuses `pgid <= 1`: `kill(0, …)` would signal CatosTV's own group and
/// `kill(-1, …)` every process the user owns.
fn signal_group(pgid: i32, sig: i32) -> std::io::Result<()> {
    if pgid <= 1 {
        return Err(std::io::Error::other(
            "refusing to signal an invalid process group",
        ));
    }
    // SAFETY: kill(2) has no memory-safety preconditions; pgid was checked.
    let r = unsafe { ffi::kill(-pgid, sig) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// True if any process in the group still exists.
fn group_alive(pgid: i32) -> bool {
    match signal_group(pgid, 0) {
        Ok(()) => true,
        Err(e) => e.raw_os_error() == Some(EPERM),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppState {
    NotRunning,
    Running,
    /// Close requested; waiting for the process group to exit.
    Closing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// The app ended on its own with success.
    Exited { id: String },
    /// The app ended on its own with a failure exit code or signal.
    Crashed { id: String, reason: String },
    /// The app ended after CatosTV asked it to close.
    Closed { id: String },
}

impl LifecycleEvent {
    pub fn id(&self) -> &str {
        match self {
            Self::Exited { id } | Self::Crashed { id, .. } | Self::Closed { id } => id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchResult {
    Started,
    /// Singleton app already running; the caller should resume it instead.
    AlreadyRunning,
}

struct Tracked {
    child: Option<Child>,
    pgid: i32,
    leader_status: Option<ExitStatus>,
    closing_since: Option<Instant>,
    killed: bool,
}

/// Tracks running apps. Call [`AppManager::poll`] regularly (the shell does
/// it a few times per second).
#[derive(Default)]
pub struct AppManager {
    tracked: HashMap<String, Tracked>,
    log_dir: Option<PathBuf>,
    /// Where the process group of every running app is written ("<pgid>
    /// <id>" per line), so the next shell can stop what this one leaves
    /// behind ([`stop_leftovers`]).
    record: Option<PathBuf>,
}

/// The record of running apps the shell keeps in the runtime folder.
pub fn record_path() -> PathBuf {
    crate::env::runtime_dir().join("catostv-apps")
}

/// Stop every app a previous shell left running (it was killed or
/// restarted without closing them: a deploy, a crash). Each recorded
/// process group gets SIGTERM, then SIGKILL after 2 s. The record lives in
/// the runtime folder, which a reboot empties, so it never names groups of
/// an earlier boot. Returns the ids of the apps that were still running.
pub fn stop_leftovers(record: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(record).unwrap_or_default();
    let _ = std::fs::remove_file(record);
    let groups: Vec<(i32, String)> = text
        .lines()
        .filter_map(|l| {
            let (pgid, id) = l.split_once(' ')?;
            let pgid: i32 = pgid.parse().ok()?;
            (pgid > 1).then(|| (pgid, id.to_string()))
        })
        .filter(|(pgid, _)| group_alive(*pgid))
        .collect();
    for (pgid, _) in &groups {
        let _ = signal_group(*pgid, SIGTERM);
    }
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until && groups.iter().any(|(p, _)| group_alive(*p)) {
        std::thread::sleep(Duration::from_millis(100));
    }
    for (pgid, _) in &groups {
        if group_alive(*pgid) {
            let _ = signal_group(*pgid, SIGKILL);
        }
    }
    groups.into_iter().map(|(_, id)| id).collect()
}

impl AppManager {
    /// `log_dir`: where each app's stdout/stderr is written as `<id>.log`
    /// (overwritten on each launch). `None` discards output.
    pub fn new(log_dir: Option<PathBuf>) -> Self {
        Self {
            tracked: HashMap::new(),
            log_dir,
            record: None,
        }
    }

    /// Keep the record of running apps at `path` (see [`stop_leftovers`]).
    pub fn with_record(mut self, path: PathBuf) -> Self {
        self.record = Some(path);
        self.write_record();
        self
    }

    fn write_record(&self) {
        let Some(path) = &self.record else { return };
        let text: String = self
            .tracked
            .iter()
            .map(|(id, t)| format!("{} {}\n", t.pgid, id.split('#').next().unwrap_or(id)))
            .collect();
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    pub fn state(&self, id: &str) -> AppState {
        match self.tracked.get(id) {
            None => AppState::NotRunning,
            Some(t) if t.closing_since.is_some() => AppState::Closing,
            Some(_) => AppState::Running,
        }
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.state(id) == AppState::Running
    }

    pub fn running_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .tracked
            .iter()
            .filter(|(_, t)| t.closing_since.is_none())
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    pub fn launch(&mut self, app: &AppEntry) -> Result<LaunchResult> {
        if app.singleton && self.tracked.contains_key(&app.id) {
            return Ok(LaunchResult::AlreadyRunning);
        }
        let argv = app.argv()?;
        if crate::process::find_executable(&argv[0]).is_none() {
            return Err(Error::msg(format!("{} is not installed", app.name)));
        }
        let (stdout, stderr) = self.log_files(&app.id);
        let child = Command::new(&argv[0])
            .args(&argv[1..])
            .env("CATOSTV_APP_ID", &app.id)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .process_group(0)
            .spawn()
            .map_err(|e| Error::msg(format!("{} could not start: {e}", app.name)))?;
        let pgid = child.id() as i32;
        // A non-singleton app launched twice keeps only the newest handle
        // under its id; the older instance is still reaped below because
        // its Tracked entry is moved to a unique key.
        if let Some(old) = self.tracked.remove(&app.id) {
            self.tracked.insert(format!("{}#{}", app.id, old.pgid), old);
        }
        self.tracked.insert(
            app.id.clone(),
            Tracked {
                child: Some(child),
                pgid,
                leader_status: None,
                closing_since: None,
                killed: false,
            },
        );
        self.write_record();
        Ok(LaunchResult::Started)
    }

    fn log_files(&self, id: &str) -> (Stdio, Stdio) {
        let Some(dir) = &self.log_dir else {
            return (Stdio::null(), Stdio::null());
        };
        let _ = std::fs::create_dir_all(dir);
        match File::create(dir.join(format!("{id}.log"))) {
            Ok(f) => match f.try_clone() {
                Ok(f2) => (Stdio::from(f), Stdio::from(f2)),
                Err(_) => (Stdio::from(f), Stdio::null()),
            },
            Err(_) => (Stdio::null(), Stdio::null()),
        }
    }

    /// Ask an app to close. Returns false if it was not running.
    pub fn close(&mut self, id: &str) -> bool {
        let Some(t) = self.tracked.get_mut(id) else {
            return false;
        };
        if t.closing_since.is_none() {
            t.closing_since = Some(Instant::now());
            let _ = signal_group(t.pgid, SIGTERM);
        }
        true
    }

    /// Reap children, detect exits, escalate slow closes. Returns what
    /// changed since the last call.
    pub fn poll(&mut self) -> Vec<LifecycleEvent> {
        let mut events = Vec::new();
        let mut finished = Vec::new();
        for (key, t) in self.tracked.iter_mut() {
            if let Some(child) = t.child.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                t.leader_status = Some(status);
                t.child = None;
            }
            let leader_gone = t.child.is_none();
            if leader_gone && !group_alive(t.pgid) {
                finished.push(key.clone());
                continue;
            }
            if let Some(since) = t.closing_since
                && !t.killed
                && since.elapsed() >= CLOSE_GRACE
            {
                let _ = signal_group(t.pgid, SIGKILL);
                t.killed = true;
            }
        }
        for key in finished {
            let t = self.tracked.remove(&key).expect("key collected above");
            let id = key.split('#').next().unwrap_or(&key).to_string();
            let event = if t.closing_since.is_some() {
                LifecycleEvent::Closed { id }
            } else {
                match t.leader_status {
                    Some(s) if s.success() => LifecycleEvent::Exited { id },
                    Some(s) => LifecycleEvent::Crashed {
                        id,
                        reason: describe_status(s),
                    },
                    None => LifecycleEvent::Exited { id },
                }
            };
            events.push(event);
        }
        if !events.is_empty() {
            self.write_record();
        }
        events
    }

    /// Close everything (used when the shell itself is shutting down).
    pub fn close_all(&mut self) {
        let ids: Vec<String> = self.tracked.keys().cloned().collect();
        for id in ids {
            self.close(&id);
        }
    }
}

fn describe_status(s: ExitStatus) -> String {
    match (s.code(), s.signal()) {
        (Some(c), _) => format!("exit code {c}"),
        (None, Some(sig)) => format!("signal {sig}"),
        (None, None) => "unknown status".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, command: &str) -> AppEntry {
        AppEntry {
            id: id.into(),
            name: id.into(),
            subtitle: String::new(),
            icon: "X".into(),
            accent: "violet".into(),
            command: command.into(),
            singleton: true,
            icon_path: None,
            source: Default::default(),
        }
    }

    fn poll_until(m: &mut AppManager, secs: u64) -> Vec<LifecycleEvent> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let ev = m.poll();
            if !ev.is_empty() || Instant::now() > deadline {
                return ev;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn refuses_dangerous_process_groups() {
        assert!(signal_group(0, 0).is_err());
        assert!(signal_group(1, 0).is_err());
        assert!(signal_group(-5, 0).is_err());
    }

    #[test]
    fn clean_exit_and_crash_are_distinguished() {
        let mut m = AppManager::new(None);
        m.launch(&app("ok", "true")).unwrap();
        assert_eq!(
            poll_until(&mut m, 5),
            vec![LifecycleEvent::Exited { id: "ok".into() }]
        );
        m.launch(&app("bad", "sh -c 'exit 3'")).unwrap();
        assert_eq!(
            poll_until(&mut m, 5),
            vec![LifecycleEvent::Crashed {
                id: "bad".into(),
                reason: "exit code 3".into()
            }]
        );
        assert_eq!(m.state("bad"), AppState::NotRunning);
    }

    #[test]
    fn singleton_and_close() {
        let mut m = AppManager::new(None);
        let a = app("sleeper", "sleep 30");
        assert_eq!(m.launch(&a).unwrap(), LaunchResult::Started);
        assert_eq!(m.launch(&a).unwrap(), LaunchResult::AlreadyRunning);
        assert!(m.poll().is_empty());
        assert!(m.is_running("sleeper"));
        assert_eq!(m.running_ids(), vec!["sleeper".to_string()]);
        assert!(m.close("sleeper"));
        assert_eq!(m.state("sleeper"), AppState::Closing);
        assert_eq!(
            poll_until(&mut m, 5),
            vec![LifecycleEvent::Closed {
                id: "sleeper".into()
            }]
        );
        assert!(!m.close("sleeper"));
    }

    #[test]
    fn group_outlives_launcher_process() {
        // Launcher exits immediately but leaves its real program running in
        // the same process group, like many app wrapper scripts do.
        let mut m = AppManager::new(None);
        m.launch(&app("wrapper", "sh -c 'sleep 30 & exit 0'"))
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(m.poll().is_empty());
        assert!(m.is_running("wrapper"));
        m.close("wrapper");
        assert_eq!(
            poll_until(&mut m, 5),
            vec![LifecycleEvent::Closed {
                id: "wrapper".into()
            }]
        );
    }

    #[test]
    fn stubborn_app_is_killed_after_grace_period() {
        let mut m = AppManager::new(None);
        m.launch(&app(
            "stubborn",
            "sh -c 'trap \"\" TERM; while :; do sleep 0.1; done'",
        ))
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        m.close("stubborn");
        let ev = poll_until(&mut m, CLOSE_GRACE.as_secs() + 5);
        assert_eq!(
            ev,
            vec![LifecycleEvent::Closed {
                id: "stubborn".into()
            }]
        );
    }

    #[test]
    fn missing_program_is_reported() {
        let mut m = AppManager::new(None);
        let err = m.launch(&app("nope", "no-such-tv-app-xyz")).unwrap_err();
        assert_eq!(err.message(), "nope is not installed");
    }

    #[test]
    fn output_goes_to_log_file() {
        let dir = std::env::temp_dir().join(format!("catostv-test-logs-{}", std::process::id()));
        let mut m = AppManager::new(Some(dir.clone()));
        m.launch(&app("echo", "sh -c 'echo hello-log'")).unwrap();
        poll_until(&mut m, 5);
        let log = std::fs::read_to_string(dir.join("echo.log")).unwrap();
        assert_eq!(log.trim(), "hello-log");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_shell_stops_what_the_last_one_left_running() {
        let record = std::env::temp_dir().join(format!("catostv-apps-{}", std::process::id()));
        let mut m = AppManager::new(None).with_record(record.clone());
        let app = app("sleeper", "sleep 30");
        m.launch(&app).unwrap();
        assert!(
            std::fs::read_to_string(&record)
                .unwrap()
                .ends_with(" sleeper\n")
        );
        // The shell goes away without closing it; the next one cleans up.
        std::mem::forget(m);
        assert_eq!(stop_leftovers(&record), vec!["sleeper".to_string()]);
        assert!(!record.exists());
        assert!(stop_leftovers(&record).is_empty());
    }
}
