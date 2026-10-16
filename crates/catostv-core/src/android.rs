//! Android apps through Waydroid (LineageOS in an LXC container, microG
//! instead of Google services, Aurora Store as the app store).
//!
//! Split of responsibilities:
//!
//! * **Root, once:** `catostv-android-setup.service` (a oneshot running
//!   `/usr/local/lib/catostv/android-setup`) loads binder, runs `waydroid init`,
//!   enables `waydroid-container.service`, adds microG + ARM translation with
//!   casualsnek's `waydroid_script`, and downloads the Aurora Store APK. The
//!   TV account may start exactly this unit (polkit rule
//!   `50-catostv-android.rules`); progress is published in
//!   [`SETUP_STATE_PATH`] ([`parse_setup_state`]).
//! * **User session (the shell):** [`session_app_entry`] (`waydroid session
//!   start`, long-running), then [`install_aurora_if_missing`] once, then
//!   [`install_apk`] / [`list_apps`] / [`launch_cmd`]. Installed apps appear
//!   in the drawer via [`crate::desktop`] (Waydroid writes
//!   `~/.local/share/applications/waydroid.<package>.desktop`).
//!
//! Waydroid facts used here were read from the waydroid 1.6.3 source
//! (`tools/actions/status.py`, `app_manager.py`, `services/user_manager.py`):
//!
//! * `waydroid status` prints `Session:\tRUNNING|STOPPED`, then (running only)
//!   `Container:\t<lxc state>`, always `Vendor type:\t…`; before `waydroid
//!   init` it prints `Waydroid is not initialized, run "waydroid init"`.
//! * `waydroid app install|list` report errors **only as log lines on
//!   stderr** (`WayDroid session is stopped`, `Failed to access IPlatform
//!   service`) and still exit 0, see [`waydroid_error`].
//! * `waydroid app launch <pkg>` returns at once when a session runs; when
//!   none runs it starts one **in the foreground** and keeps running.

use crate::apps::{AppEntry, AppSource};
use crate::disk::{self, BlockDevice};
use crate::error::{Error, Result};
use crate::process::{Cmd, Output, QUICK, Runner, SLOW, VERY_LONG};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// The root oneshot unit that performs the privileged setup.
pub const SETUP_SERVICE: &str = "catostv-android-setup.service";
/// The script the unit runs.
pub const SETUP_SCRIPT: &str = "/usr/local/lib/catostv/android-setup";
/// Machine-readable progress, world-readable, rewritten atomically.
pub const SETUP_STATE_PATH: &str = "/run/catostv/android-setup.state";
/// Full log of the setup script (world-readable).
pub const SETUP_LOG_PATH: &str = "/var/log/catostv-android-setup.log";
/// Written by the script after a fully successful setup.
pub const SETUP_DONE_PATH: &str = "/var/lib/catostv/android/setup-done";
/// Aurora Store APK downloaded from F-Droid by the setup script.
pub const AURORA_APK_PATH: &str = "/var/lib/catostv/android/AuroraStore.apk";
pub const AURORA_PACKAGE: &str = "com.aurora.store";
/// Waydroid's container manager (enabled by the setup script).
pub const CONTAINER_SERVICE: &str = "waydroid-container.service";
/// Waydroid's per-app icons, relative to `$XDG_DATA_HOME` (`~/.local/share`).
pub const WAYDROID_ICONS_REL: &str = "waydroid/data/icons";

/// `waydroid` is a Python program; give it time to start and talk D-Bus.
pub const WAYDROID_TIMEOUT: Duration = Duration::from_secs(30);
/// Installing a large APK (copy + PackageManager).
pub const APK_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

/// Validate an Android package name before it is used as an argument:
/// at least two dot-separated segments of `[A-Za-z0-9_]`, each starting
/// with a letter, at most 255 characters.
pub fn is_valid_package(pkg: &str) -> bool {
    if pkg.is_empty() || pkg.len() > 255 {
        return false;
    }
    let parts: Vec<&str> = pkg.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|p| {
            p.starts_with(|c: char| c.is_ascii_alphabetic())
                && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

fn check_package(pkg: &str) -> Result<()> {
    if is_valid_package(pkg) {
        Ok(())
    } else {
        Err(Error::msg(format!(
            "{pkg:?} is not a valid Android package name"
        )))
    }
}

// ---------------------------------------------------------------- status

/// What CatosTV knows about Android on this machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AndroidStatus {
    /// The `waydroid` program could be started.
    pub installed: bool,
    /// `waydroid init` has completed (config + images present).
    pub initialized: bool,
    /// `waydroid-container.service` is active (ready to start a session).
    pub container: bool,
    /// A user session runs (Android is booted or booting).
    pub session: bool,
    /// LXC state while a session runs: `RUNNING`, `FROZEN`, …
    pub container_state: Option<String>,
    /// `MAINLINE`, `HALIUM_…`, informational.
    pub vendor_type: Option<String>,
    /// The Wayland socket the running session was started on
    /// (`wayland-android` when it goes through `catostv-wlfilter`).
    pub wayland_display: Option<String>,
    /// `android-keystore-check` found Android's keystore database damaged
    /// Android cannot finish booting until Repair starts it over.
    pub keystore_broken: bool,
}

/// Written by `android-keystore-check` when the keystore database fails
/// its integrity check (one line: SQLite's verdict).
pub const KEYSTORE_BROKEN_PATH: &str = "/run/catostv/android-keystore-broken";

pub fn keystore_broken() -> bool {
    std::fs::metadata(KEYSTORE_BROKEN_PATH).is_ok()
}

impl AndroidStatus {
    /// The session runs through the display filter.
    pub fn through_filter(&self) -> bool {
        self.wayland_display.as_deref() == Some(crate::wlfilter::ANDROID_DISPLAY)
    }
}

impl AndroidStatus {
    /// Android can be used right now.
    pub fn ready(&self) -> bool {
        self.installed && self.initialized && self.container && self.session
    }
}

/// Parse `waydroid status` output into an [`AndroidStatus`] (without
/// `installed`/`container`, which come from elsewhere).
pub fn parse_status(text: &str) -> AndroidStatus {
    let mut st = AndroidStatus::default();
    if text.contains("not initialized") {
        return st;
    }
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Session" => {
                st.initialized = true;
                st.session = value == "RUNNING";
            }
            "Container" => st.container_state = Some(value.to_string()),
            "Vendor type" => st.vendor_type = Some(value.to_string()),
            "Wayland display" => st.wayland_display = Some(value.to_string()),
            _ => {}
        }
    }
    st
}

pub fn status_cmd() -> Cmd {
    Cmd::new("waydroid").arg("status").timeout(WAYDROID_TIMEOUT)
}

pub fn container_active_cmd() -> Cmd {
    Cmd::new("systemctl")
        .args(["is-active", CONTAINER_SERVICE])
        .timeout(QUICK)
}

/// Query Waydroid. Never fails: an absent `waydroid` gives `installed: false`.
pub fn status(r: &mut dyn Runner) -> AndroidStatus {
    let out = match r.run(&status_cmd()) {
        Ok(o) if !o.timed_out => o,
        // Present but hung (e.g. D-Bus stuck): report what we know.
        Ok(_) => {
            return AndroidStatus {
                installed: true,
                ..AndroidStatus::default()
            };
        }
        Err(_) => return AndroidStatus::default(),
    };
    let mut st = parse_status(&format!("{}\n{}", out.stdout, out.stderr));
    st.installed = true;
    let service_active = r
        .run(&container_active_cmd())
        .map(|o| o.stdout.trim() == "active")
        .unwrap_or(false);
    st.container =
        service_active || matches!(st.container_state.as_deref(), Some("RUNNING" | "FROZEN"));
    st.keystore_broken = keystore_broken();
    st
}

// ---------------------------------------------------------------- setup

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupResult {
    Ok,
    Failed(String),
}

/// Progress of `catostv-android-setup.service`, as published in
/// [`SETUP_STATE_PATH`]:
///
/// ```text
/// step=3/7 Starting the Android container
/// result=failed:Downloading Android (about 800 MB)
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetupState {
    pub step: u32,
    pub total: u32,
    /// Human-readable text of the current (or failed) step.
    pub message: String,
    /// `None` while the setup runs.
    pub result: Option<SetupResult>,
}

impl SetupState {
    pub fn running(&self) -> bool {
        self.result.is_none()
    }

    /// 0.0–1.0 for a progress bar (a finished step counts as done).
    pub fn fraction(&self) -> f32 {
        match self.result {
            Some(SetupResult::Ok) => 1.0,
            _ if self.total == 0 => 0.0,
            _ => (self.step.saturating_sub(1) as f32 / self.total as f32).clamp(0.0, 1.0),
        }
    }
}

/// Parse the state file. Unknown lines are ignored; the last `step=` and
/// `result=` lines win.
pub fn parse_setup_state(text: &str) -> SetupState {
    let mut st = SetupState::default();
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("step=") {
            let (counter, msg) = rest.split_once(' ').unwrap_or((rest, ""));
            if let Some((n, m)) = counter.split_once('/')
                && let (Ok(n), Ok(m)) = (n.parse(), m.parse())
            {
                st.step = n;
                st.total = m;
                st.message = msg.trim().to_string();
            }
        } else if let Some(rest) = line.strip_prefix("result=") {
            st.result = Some(if rest == "ok" {
                SetupResult::Ok
            } else {
                let reason = rest.strip_prefix("failed:").unwrap_or(rest).trim();
                SetupResult::Failed(if reason.is_empty() {
                    "unknown error".into()
                } else {
                    reason.to_string()
                })
            });
        }
    }
    st
}

pub fn read_setup_state_from(path: &Path) -> Option<SetupState> {
    std::fs::read_to_string(path)
        .ok()
        .map(|t| parse_setup_state(&t))
}

/// The current setup state, `None` if the setup has not run since boot.
pub fn read_setup_state() -> Option<SetupState> {
    read_setup_state_from(Path::new(SETUP_STATE_PATH))
}

/// True after a fully successful setup (survives reboots).
pub fn is_setup_done() -> bool {
    Path::new(SETUP_DONE_PATH).is_file()
}

pub fn start_setup_cmd() -> Cmd {
    Cmd::new("systemctl")
        .args(["--no-ask-password", "start", "--no-block", SETUP_SERVICE])
        .timeout(SLOW)
}

/// Queue the privileged setup (returns immediately; poll
/// [`read_setup_state`]). Allowed for the active local `catostv` session by
/// `/etc/polkit-1/rules.d/50-catostv-android.rules`.
pub fn start_setup(r: &mut dyn Runner) -> Result<()> {
    let out = r.run(&start_setup_cmd())?;
    if out.success() {
        return Ok(());
    }
    if out.stderr.contains("authentication required") || out.stderr.contains("Access denied") {
        return Err(Error::msg(
            "CatosTV is not allowed to set up Android (polkit rule missing)",
        ));
    }
    Err(Error::msg(format!(
        "Could not start Android setup ({})",
        out.failure_reason()
    )))
}

// ---------------------------------------------------------------- session

/// `waydroid session start`, **long-running**: it stays in the foreground
/// for as long as Android runs. Do not run it through a [`Runner`] (which
/// waits for exit); start it with [`session_app_entry`] and the lifecycle
/// manager, or another detached spawn.
pub fn session_start_cmd() -> Cmd {
    Cmd::new("waydroid")
        .args(["session", "start"])
        .timeout(VERY_LONG)
}

pub fn session_stop_cmd() -> Cmd {
    Cmd::new("waydroid")
        .args(["session", "stop"])
        .timeout(WAYDROID_TIMEOUT)
}

/// Waydroid writes this when a session starts; it names the Wayland socket
/// the session is bound to (`… run/xdg/wayland-0` mount entry).
pub const SESSION_CONFIG: &str = "/var/lib/waydroid/lxc/waydroid/config_session";

/// The host Wayland socket a Waydroid session config binds.
pub fn session_socket(config: &str) -> Option<PathBuf> {
    config.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("lxc.mount.entry")?.trim_start();
        let mut f = rest.strip_prefix('=')?.split_whitespace();
        let host = f.next()?;
        (f.next()? == "run/xdg/wayland-0").then(|| PathBuf::from(host))
    })
}

/// Is the running Waydroid session bound to a Wayland socket that is gone
/// or was made after the session started? That is what a TV
/// session restart leaves: Android keeps running on the old, dead display,
/// `waydroid status` still says RUNNING, and nothing Android draws reaches
/// the screen. Only meaningful while a session runs.
pub fn session_is_stale(config_path: &Path) -> bool {
    let (Ok(cfg_meta), Ok(text)) = (
        std::fs::metadata(config_path),
        std::fs::read_to_string(config_path),
    ) else {
        return false;
    };
    let Some(socket) = session_socket(&text) else {
        return false;
    };
    match std::fs::metadata(&socket) {
        Err(_) => true,
        Ok(m) => match (m.modified(), cfg_meta.modified()) {
            // A second of slack: the session config is written right after
            // the socket on a normal start.
            (Ok(sock), Ok(cfg)) => sock > cfg + Duration::from_secs(1),
            _ => false,
        },
    }
}

/// Stop a stale session and wait (≤ 20 s) until Waydroid reports it gone,
/// so the next launch starts a fresh one on the current display.
pub fn stop_stale_session(r: &mut dyn Runner, sleep: &mut dyn FnMut(Duration)) -> Result<()> {
    r.run_ok(&session_stop_cmd())?;
    for _ in 0..40 {
        if !status(r).session {
            return Ok(());
        }
        sleep(Duration::from_millis(500));
    }
    Err(Error::msg("Android did not stop"))
}

/// Id of the hidden lifecycle entry that keeps the Waydroid session alive.
pub const SESSION_APP_ID: &str = "catostv-android-session";

/// Starts `waydroid session start` with `WAYLAND_DISPLAY` pointing at
/// `catostv-wlfilter`'s relay socket when it is up (the relay hides
/// the idle-inhibit protocol from Android's display HAL, which otherwise
/// deadlocks against Android's window manager), else at the
/// compositor with a log line.
pub const SESSION_SCRIPT: &str = "/usr/local/lib/catostv/android-session";

/// An [`AppEntry`] the shell can hand to `AppManager::launch` to run the
/// Waydroid session in its own process group (logs in
/// `~/.local/state/catostv/apps/catostv-android-session.log`, closable, crash
/// reported). Not meant to be shown in the drawer.
pub fn session_app_entry() -> AppEntry {
    AppEntry {
        id: SESSION_APP_ID.into(),
        name: "Android".into(),
        subtitle: "Waydroid session".into(),
        icon: "A".into(),
        accent: "green".into(),
        command: SESSION_SCRIPT.into(),
        singleton: true,
        icon_path: None,
        source: AppSource::Configured,
    }
}

// ---------------------------------------------------------------- apps

/// Known Waydroid error lines (logged to stderr with exit code 0).
pub fn waydroid_error(out: &Output) -> Option<String> {
    if out.timed_out {
        return Some("Android did not answer in time".into());
    }
    let text = format!("{}\n{}", out.stdout, out.stderr);
    for line in text.lines() {
        let l = line.trim();
        if l.contains("session is stopped") {
            return Some("Android is not running".into());
        }
        if l.contains("not initialized") {
            return Some("Android is not set up yet".into());
        }
        if l.contains("Failed to access IPlatform service") {
            return Some("Android is still starting; try again in a moment".into());
        }
        if l.contains("Failed to uninstall package") {
            return Some(l.to_string());
        }
        // tools/__init__.py: logging.info("ERROR: " + str(e))
        if let Some(pos) = l.find("ERROR: ") {
            return Some(l[pos + 7..].trim().to_string());
        }
    }
    if !out.success() {
        return Some(out.failure_reason());
    }
    None
}

fn waydroid_ok(r: &mut dyn Runner, cmd: &Cmd) -> Result<Output> {
    let out = r.run(cmd)?;
    match waydroid_error(&out) {
        Some(e) => Err(Error::msg(e)),
        None => Ok(out),
    }
}

/// One app reported by `waydroid app list`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AndroidApp {
    pub name: String,
    pub package: String,
    pub categories: Vec<String>,
}

impl AndroidApp {
    /// Has a launcher icon (Waydroid only exports these as desktop files).
    pub fn launchable(&self) -> bool {
        self.categories
            .iter()
            .any(|c| c == "android.intent.category.LAUNCHER")
    }
}

/// Parse `waydroid app list`:
///
/// ```text
/// Name: Aurora Store
/// packageName: com.aurora.store
/// categories:
///     android.intent.category.LAUNCHER   (TAB-indented)
/// ```
pub fn parse_app_list(text: &str) -> Vec<AndroidApp> {
    let mut apps: Vec<AndroidApp> = Vec::new();
    let mut in_categories = false;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("Name: ") {
            apps.push(AndroidApp {
                name: name.trim().to_string(),
                ..AndroidApp::default()
            });
            in_categories = false;
        } else if let Some(pkg) = line.strip_prefix("packageName: ") {
            if let Some(a) = apps.last_mut() {
                a.package = pkg.trim().to_string();
            }
            in_categories = false;
        } else if line.trim_end() == "categories:" {
            in_categories = true;
        } else if in_categories && line.starts_with(['\t', ' ']) && !line.trim().is_empty() {
            if let Some(a) = apps.last_mut() {
                a.categories.push(line.trim().to_string());
            }
        } else {
            in_categories = false;
        }
    }
    apps.retain(|a| is_valid_package(&a.package));
    apps
}

pub fn list_apps_cmd() -> Cmd {
    Cmd::new("waydroid")
        .args(["app", "list"])
        .timeout(WAYDROID_TIMEOUT)
}

/// Apps installed in Android (needs a running session).
pub fn list_apps(r: &mut dyn Runner) -> Result<Vec<AndroidApp>> {
    Ok(parse_app_list(&waydroid_ok(r, &list_apps_cmd())?.stdout))
}

/// How long to wait for Android to answer after its session starts. The
/// container reports `RUNNING` well before Android has booted; the first
/// boot after setup can take minutes on slow disks.
pub const READY_TIMEOUT: Duration = Duration::from_secs(240);
/// Pause between "are you up yet?" probes.
pub const READY_POLL: Duration = Duration::from_secs(3);

/// Wait until Android answers `waydroid app list`, i.e. it has really
/// booted, and return the installed apps. Tries until `budget` is used up
/// (`sleep` is injected so tests do not wait).
pub fn wait_until_ready(
    r: &mut dyn Runner,
    budget: Duration,
    sleep: &mut dyn FnMut(Duration),
) -> Result<Vec<AndroidApp>> {
    let mut waited = Duration::ZERO;
    loop {
        // An empty list is Android answering before its package service
        // is up (seen on the TV image: `waydroid app list` prints nothing
        // for a while after boot); there are always a few apps once it is
        // ready (the icon sync ran on that empty answer and Home
        // showed no Android apps).
        let last = match list_apps(r) {
            Ok(apps) if !apps.is_empty() => return Ok(apps),
            Ok(_) => Error::msg("Android has not listed its apps yet"),
            Err(e) => e,
        };
        if waited >= budget {
            return Err(Error::msg(format!("Android is still starting ({last})")));
        }
        sleep(READY_POLL);
        waited += READY_POLL;
    }
}

/// argv for launching `pkg`. The caller must have validated `pkg`
/// ([`is_valid_package`]); [`launch_cmd`] does.
pub fn launch_argv(pkg: &str) -> Vec<String> {
    ["waydroid", "app", "launch", pkg]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// `waydroid app launch <pkg>` (returns at once when a session runs).
pub fn launch_cmd(pkg: &str) -> Result<Cmd> {
    check_package(pkg)?;
    let argv = launch_argv(pkg);
    Ok(Cmd::new(&argv[0])
        .args(&argv[1..])
        .timeout(WAYDROID_TIMEOUT))
}

/// Category of an Android TV home-screen entry. Apps made only for TVs
/// (SmartTube…) have this instead of the phone LAUNCHER category, so
/// Waydroid makes no desktop file for them and `waydroid app launch` cannot
/// open them.
pub const LEANBACK: &str = "android.intent.category.LEANBACK_LAUNCHER";

/// CatosTV's own desktop files for TV-only apps:
/// `~/.local/share/applications/catostv-android.<package>.desktop`. A
/// different prefix from Waydroid's `waydroid.` because Waydroid deletes
/// and rewrites its own files when a session starts.
pub const TV_DESKTOP_PREFIX: &str = "catostv-android.";

impl AndroidApp {
    /// Made for TVs only: an Android TV launcher entry, no phone icon.
    pub fn tv_only(&self) -> bool {
        !self.launchable() && self.categories.iter().any(|c| c == LEANBACK)
    }
}

pub fn tv_desktop_path(home: &Path, pkg: &str) -> PathBuf {
    home.join(".local/share/applications")
        .join(format!("{TV_DESKTOP_PREFIX}{pkg}.desktop"))
}

/// The desktop file for a TV-only app. The Exec line is the same as
/// Waydroid's (that is what `desktop::scan_waydroid` accepts); CatosTV starts
/// such apps through [`tv_launch_cmd`] instead.
pub fn tv_desktop_entry(home: &Path, app: &AndroidApp) -> String {
    let name: String = app.name.chars().filter(|c| !c.is_control()).collect();
    format!(
        "[Desktop Entry]\nType=Application\nName={name}\nExec=waydroid app launch {pkg}\n\
         Icon={icon}\nCategories=X-WayDroid-App;\nX-CatosTV-TV-Only=true\nNoDisplay=false\n",
        pkg = app.package,
        icon = icon_path(home, &app.package).display(),
    )
}

/// Waydroid's icon cache relative to the home directory.
pub const WAYDROID_ICONS_REL_FROM_HOME: &str = ".local/share/waydroid/data/icons";

/// Icons CatosTV made from an app's APK (`android-icon`), relative to
/// the home directory: for apps installed after Android booted, which
/// Waydroid's platform service only gives an icon at the next boot.
pub const CATOSTV_ICONS_REL_FROM_HOME: &str = ".local/share/catostv/android-icons";

/// Makes a PNG icon from an installed app's APK; run as the TV account.
pub const ICON_SCRIPT: &str = "/usr/local/lib/catostv/android-icon";

fn waydroid_icon(home: &Path, pkg: &str) -> PathBuf {
    home.join(WAYDROID_ICONS_REL_FROM_HOME)
        .join(format!("{pkg}.png"))
}

fn catostv_icon(home: &Path, pkg: &str) -> PathBuf {
    home.join(CATOSTV_ICONS_REL_FROM_HOME)
        .join(format!("{pkg}.png"))
}

/// The icon for an app's desktop file: Waydroid's own when it has one,
/// else the one CatosTV made, else Waydroid's path (it appears there at the
/// next boot).
pub fn icon_path(home: &Path, pkg: &str) -> PathBuf {
    let waydroid = waydroid_icon(home, pkg);
    let own = catostv_icon(home, pkg);
    if !waydroid.is_file() && own.is_file() {
        own
    } else {
        waydroid
    }
}

/// Make icons for the apps that have none yet: Waydroid writes
/// them only while Android boots, so an app installed afterwards, by
/// CatosTV's APK install or the Play Store, showed a name without an icon.
/// `android-icon` reads the app's APK; apps with a vector-only icon keep
/// the name tile. Returns true when an icon was made.
pub fn ensure_icons(r: &mut dyn Runner, home: &Path, apps: &[AndroidApp]) -> bool {
    let mut made = false;
    for app in apps
        .iter()
        .filter(|a| !is_hidden_package(&a.package) && (a.tv_only() || a.launchable()))
    {
        if waydroid_icon(home, &app.package).is_file() || catostv_icon(home, &app.package).is_file()
        {
            continue;
        }
        let out = catostv_icon(home, &app.package);
        if let Some(dir) = out.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let cmd = Cmd::new(ICON_SCRIPT)
            .args([app.package.clone(), out.display().to_string()])
            .timeout(Duration::from_secs(20));
        if r.run_ok(&cmd).is_ok() && out.is_file() {
            made = true;
        }
    }
    made
}

/// Published by `android-watch` while Android reports a finished boot
/// (`sys.boot_completed`); gone while it starts, stops or is down.
pub const BOOTED_PATH: &str = "/run/catostv/android-booted";

pub fn is_booted() -> bool {
    std::fs::metadata(BOOTED_PATH).is_ok_and(|m| m.is_file())
}

/// Android packages that are not apps for the TV's Home: Google's and
/// Android's own services, launchers and setup helpers that still carry a
/// launcher entry on the Android TV image ("Android TV Remote
/// Service" showed up as an app, and opening it brought SmartTube up).
/// The Play Store and real apps stay.
pub fn is_hidden_package(pkg: &str) -> bool {
    let p = pkg.to_ascii_lowercase();
    if p == "com.android.vending" {
        return false;
    }
    const HIDDEN_PARTS: &[&str] = &[
        "remote.service",
        "katniss",
        "setupwraith",
        "launcherx",
        "tvlauncher",
        "leanbacklauncher",
        "tvrecommendations",
        "backdrop",
        "feedback",
        "inputmethod",
        "partnersetup",
        "onetimeinitializer",
        "configupdater",
        "overlay",
        "webview",
        "packageinstaller",
        "documentsui",
        "systemui",
        "frameworkpackagestubs",
        "captiveportal",
        "printspooler",
        "tungsten",
        "gms",
        "gsf",
        "tts",
        "settings",
        "catapult",
        "trebuchet",
    ];
    HIDDEN_PARTS.iter().any(|part| p.contains(part))
        || p.starts_with("org.lineageos.")
        || p.starts_with("com.android.")
        || p.starts_with("com.waydroid")
        || p.starts_with("id.waydro")
}

/// Make CatosTV's own desktop files match `apps` (the installed Android
/// apps): one for every app with a TV or phone launcher entry, hidden
/// packages aside. Waydroid publishes its own `waydroid.<pkg>.desktop`
/// files too, but deletes them whenever its session stops and writes them
/// back only once Android has booted again, so Home lost every Android
/// icon for a minute after each restart; CatosTV's files stay, and
/// the app scanner prefers them (they sort first). An empty `apps` means
/// "Android did not answer", and removes nothing; neither does an answer
/// from an Android that has not finished booting (`booted` false:
/// the Play Store icon went while Android was restarting). Returns true
/// when something changed (the app list needs a refresh).
pub fn sync_tv_desktops(home: &Path, apps: &[AndroidApp], booted: bool) -> bool {
    let dir = home.join(".local/share/applications");
    let mut changed = false;
    let wanted: Vec<&AndroidApp> = apps
        .iter()
        .filter(|a| !is_hidden_package(&a.package))
        .filter(|a| a.tv_only() || a.launchable())
        .collect();
    for app in &wanted {
        let path = tv_desktop_path(home, &app.package);
        let text = tv_desktop_entry(home, app);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            let _ = std::fs::create_dir_all(&dir);
            if std::fs::write(&path, text).is_ok() {
                changed = true;
            }
        }
    }
    if apps.is_empty() || !booted {
        return changed;
    }
    // An icon goes only when its app is missing from two answers in a
    // row, both from a booted Android: a half-booted Android answered with one app once and
    // the sync threw away the other seven. The previous answer is kept
    // next to the icons; without one, nothing is removed this time.
    let memo = dir.join(TV_DESKTOP_MEMO);
    let previous: Option<Vec<String>> = std::fs::read_to_string(&memo)
        .ok()
        .map(|t| t.lines().map(str::to_string).collect());
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(pkg) = name
                .strip_prefix(TV_DESKTOP_PREFIX)
                .and_then(|n| n.strip_suffix(".desktop"))
            else {
                continue;
            };
            let keep = wanted.iter().any(|a| a.package == pkg)
                || previous.as_ref().is_none_or(|p| p.iter().any(|x| x == pkg));
            if !keep && std::fs::remove_file(e.path()).is_ok() {
                changed = true;
            }
        }
    }
    let now: String = wanted.iter().map(|a| format!("{}\n", a.package)).collect();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&memo, now);
    changed
}

/// The previous answer's packages, kept with the icons (hidden file).
const TV_DESKTOP_MEMO: &str = ".catostv-android-apps";

/// Is `pkg` one of the TV-only apps CatosTV made a desktop file for?
pub fn is_tv_only(home: &Path, pkg: &str) -> bool {
    tv_desktop_path(home, pkg).is_file()
}

/// Template unit that opens a TV-only app as root (its Android TV entry
/// point is started with `am start -n`, which needs root).
pub const LAUNCH_UNIT_PREFIX: &str = "catostv-android-launch@";

/// `systemctl start catostv-android-launch@<pkg>.service`.
pub fn tv_launch_cmd(pkg: &str) -> Result<Cmd> {
    check_package(pkg)?;
    Ok(Cmd::new("systemctl")
        .args([
            "--no-ask-password".to_string(),
            "start".to_string(),
            format!("{LAUNCH_UNIT_PREFIX}{pkg}.service"),
        ])
        .timeout(WAYDROID_TIMEOUT))
}

/// Which Android app is in front, published by the root service
/// catostv-android-watch (`/run/catostv/android-top`, one package name).
pub const TOP_PATH: &str = "/run/catostv/android-top";

pub fn read_top() -> Option<String> {
    let s = std::fs::read_to_string(TOP_PATH).ok()?;
    let s = s.trim();
    is_valid_package(s).then(|| s.to_string())
}

/// Android's tasks, top to bottom, one package per line (published by
/// `android-watch`).
pub const TASKS_PATH: &str = "/run/catostv/android-tasks";

/// What CatosTV last opened in Android and the tasks Android had just before
/// (written by `android-launch`): package first, then one package
/// per task, top to bottom.
pub const LAUNCHED_PATH: &str = "/run/catostv/android-launched";

fn package_lines(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|l| is_valid_package(l))
        .map(str::to_string)
        .collect()
}

pub fn read_tasks() -> Vec<String> {
    std::fs::read_to_string(TASKS_PATH)
        .map(|s| package_lines(&s))
        .unwrap_or_default()
}

/// The app CatosTV opened and the packages that had tasks before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub package: String,
    pub before: Vec<String>,
}

pub fn parse_launched(s: &str) -> Option<Launched> {
    let mut lines = package_lines(s).into_iter();
    Some(Launched {
        package: lines.next()?,
        before: lines.collect(),
    })
}

pub fn read_launched() -> Option<Launched> {
    parse_launched(&std::fs::read_to_string(LAUNCHED_PATH).ok()?)
}

/// the user backed out of the app CatosTV opened, and Android
/// now shows an app that was already running before (full-screen mode
/// shows Android's previous task when Back passes an app's first screen).
/// `top` is Android's front app, `tasks` its tasks top to bottom.
///
/// True when `top` had a task before the launch and the launched app's
/// task is not directly beneath it (Back sends that task to the back).
/// False for the launched app itself, Android's system UI, an app that
/// had no task before (a sign-in browser, the package installer, Google
/// account screens), and an existing app the launched one brought forward
/// (its task directly beneath, e.g. Play Store's "Open"). Android's own
/// home screen is handled by [`is_android_home`].
pub fn backed_out(top: &str, launched: &Launched, tasks: &[String]) -> bool {
    if top == launched.package || top == "com.android.systemui" || top == "android" {
        return false;
    }
    if !launched.before.iter().any(|p| p == top) {
        return false;
    }
    match tasks.iter().position(|p| p == top) {
        Some(i) => tasks.get(i + 1) != Some(&launched.package),
        None => true,
    }
}

/// The setup's download progress from the tail of its log: `waydroid
/// init` prints `[Downloading] 184.81 MB/193.87 MB` lines.
/// `(done, total)` in whole megabytes, from the last such line.
pub fn read_download_progress() -> Option<(u32, u32)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(SETUP_LOG_PATH).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(8192);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = String::new();
    f.read_to_string(&mut tail).ok()?;
    parse_download_progress(&tail)
}

pub fn parse_download_progress(text: &str) -> Option<(u32, u32)> {
    let last = text.rsplit("[Downloading]").next()?;
    if last.len() == text.len() {
        return None; // no marker at all
    }
    let mut nums = last
        .split_whitespace()
        .take(4)
        .flat_map(|w| w.split('/'))
        .filter_map(|w| w.parse::<f64>().ok());
    let done = nums.next()?;
    let total = nums.next()?;
    (total > 0.0 && done >= 0.0).then(|| (done.round() as u32, total.round() as u32))
}

/// Published by `android-watch`: a fingerprint of Android's
/// installed third-party packages, changed whenever the Play Store (or
/// anything else inside Android) installs or removes an app, so the shell
/// re-reads the app list and makes Home icons for TV-only apps.
pub const APPS_PATH: &str = "/run/catostv/android-apps";

pub fn read_apps_stamp() -> Option<String> {
    let s = std::fs::read_to_string(APPS_PATH).ok()?;
    let s = s.trim();
    (!s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| s.to_string())
}

/// Waydroid's one full-screen window (`waydroid.active_apps=Waydroid`),
/// which shows every Android app. The Android TV 16 image's surfaceflinger
/// does not pass task names to the display HAL (no `TID:` layer names),
/// so per-app windows (`waydroid.<package>`, ATV13 and the stock image)
/// never open there; CatosTV always uses full-screen mode.
pub const WINDOW: &str = "Waydroid";

/// The compositor window of an Android app: the shared full-screen window
/// for every package.
pub fn window_of(_package: &str) -> String {
    WINDOW.to_string()
}

/// Published by `android-watch`: "<surfaceflinger pid> <restarts since
/// the watcher started>". The watcher re-applies "hardware overlays off"
/// to every new surfaceflinger; the shell watches `restarts` to
/// leave an app whose screen service just died.
pub const SF_PATH: &str = "/run/catostv/android-sf";

/// Published by `android-watch`: "<count> <package>", the last
/// Android app it closed because Android reported it hung. The shell
/// watches `count` to tell the user.
pub const HUNG_PATH: &str = "/run/catostv/android-hung";

/// `(count, package)` from [`HUNG_PATH`], if published.
pub fn read_hung() -> Option<(u32, String)> {
    parse_hung(&std::fs::read_to_string(HUNG_PATH).ok()?)
}

pub fn parse_hung(s: &str) -> Option<(u32, String)> {
    let mut it = s.split_whitespace();
    let count = it.next()?.parse().ok()?;
    let pkg = it.next()?;
    pkg.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
        .then(|| (count, pkg.to_string()))
}

/// Published by `android-watch`: "<count>", how often Android stopped
/// being able to start apps (new app processes never attach). The shell
/// watches the count and restarts Android.
pub const STUCK_PATH: &str = "/run/catostv/android-stuck";

/// The count from [`STUCK_PATH`], if published.
pub fn read_stuck() -> Option<u32> {
    std::fs::read_to_string(STUCK_PATH)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Root-only diagnostics script (`sudo catostv-diag android` runs it).
pub const DIAG_SCRIPT: &str = "/usr/local/lib/catostv/android-diag";

/// `(pid, restarts)` from [`SF_PATH`], if published.
pub fn read_sf() -> Option<(u32, u32)> {
    parse_sf(&std::fs::read_to_string(SF_PATH).ok()?)
}

pub fn parse_sf(s: &str) -> Option<(u32, u32)> {
    let mut it = s.split_whitespace();
    let pid = it.next()?.parse().ok()?;
    let restarts = it.next()?.parse().ok()?;
    Some((pid, restarts))
}

/// Android's own home screen: when it comes to the front, the user has
/// backed out of the app, so CatosTV takes over again (Android apps then
/// behave like CatosTV's own: Back from the first screen goes Home).
pub fn is_android_home(pkg: &str) -> bool {
    let p = pkg.to_ascii_lowercase();
    p.contains("launcher")
        || p.contains("trebuchet")
        || p.contains("catapult")
        || p.contains("leanback")
        || p == "com.android.settings.fallbackhome"
}

/// Google Play certification id, published by `android-gsf-id` (GApps
/// images).
pub const GSF_ID_PATH: &str = "/var/lib/catostv/android/gsf-id";

pub fn read_gsf_id() -> Option<String> {
    let s = std::fs::read_to_string(GSF_ID_PATH).ok()?;
    let s = s.trim();
    (s.len() >= 10 && s.chars().all(|c| c.is_ascii_digit())).then(|| s.to_string())
}

/// Which image the setup installed (`/var/lib/catostv/android/image`,
/// e.g. "atv16-gapps"); `None` before the Android TV setup ran.
pub const IMAGE_PATH: &str = "/var/lib/catostv/android/image";

pub fn installed_image() -> Option<String> {
    let s = std::fs::read_to_string(IMAGE_PATH).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

pub fn has_google_play() -> bool {
    installed_image().is_some_and(|i| i.ends_with("-gapps"))
}

/// What Settings shows for an installed image id: "atv16-gapps" →
/// "Android TV 16 · Google Play"; a CatosTV-built image
/// ("atv16-custom-<build>-gapps") names its build.
pub fn image_label(id: &str) -> String {
    let (base, store) = match id.rsplit_once('-') {
        Some((b, "gapps")) => (b, Some("Google Play")),
        Some((b, "vanilla")) => (b, Some("Aurora Store")),
        _ => (id, None),
    };
    let (tag, build) = match base.split_once("-custom-") {
        Some((t, b)) => (t, Some(b)),
        None => (base, None),
    };
    let mut parts = vec![match tag {
        "atv16" => "Android TV 16".to_string(),
        "atv13" => "Android TV 13".to_string(),
        other => other.to_string(),
    }];
    if let Some(b) = build {
        parts.push(format!("CatosTV build {b}"));
    }
    parts.extend(store.map(str::to_string));
    parts.join(" · ")
}

/// Template unit that force-stops one Android app as root
/// (`waydroid shell` needs root). Started per package by the shell; the
/// polkit rule allows only `start` of units with this prefix.
pub const STOP_UNIT_PREFIX: &str = "catostv-android-stop@";

/// `systemctl start catostv-android-stop@<pkg>.service`, closes an Android
/// app (Recents X, app options Close). The package is validated first and
/// validated again by the root script.
pub fn stop_cmd(pkg: &str) -> Result<Cmd> {
    check_package(pkg)?;
    Ok(Cmd::new("systemctl")
        .args([
            "--no-ask-password".to_string(),
            "start".to_string(),
            "--no-block".to_string(),
            format!("{STOP_UNIT_PREFIX}{pkg}.service"),
        ])
        .timeout(SLOW))
}

/// Root unit that sizes Android's layout for the TV (`wm density`, see
/// `/usr/local/lib/catostv/android-tv-density`).
pub const DENSITY_UNIT: &str = "catostv-android-density.service";

/// `systemctl start catostv-android-density.service` (polkit allows the TV
/// session to start exactly this unit).
pub fn density_cmd() -> Cmd {
    Cmd::new("systemctl")
        .args(["--no-ask-password", "start", "--no-block", DENSITY_UNIT])
        .timeout(SLOW)
}

/// Root unit behind Settings → Android → Reset Android.
pub const REPAIR_UNIT: &str = "catostv-android-repair.service";

pub fn repair_cmd() -> Cmd {
    Cmd::new("systemctl")
        .args(["--no-ask-password", "start", "--no-block", REPAIR_UNIT])
        .timeout(SLOW)
}

/// Reset Android is running (the oneshot unit is "activating").
pub fn repair_running(r: &mut dyn Runner) -> bool {
    r.run(&Cmd::new("systemctl").args(["is-active", REPAIR_UNIT]))
        .is_ok_and(|o| matches!(o.stdout.trim(), "activating" | "active" | "reloading"))
}

/// Root unit template that uninstalls one Android app (`pm uninstall`
/// as root: `waydroid app remove` through the user session failed for
/// apps Aurora installed).
pub const REMOVE_UNIT_PREFIX: &str = "catostv-android-remove@";

/// Uninstall an Android app (waits for the unit; its failure is the
/// error).
pub fn remove_app(r: &mut dyn Runner, pkg: &str) -> Result<()> {
    check_package(pkg)?;
    let out = r.run(
        &Cmd::new("systemctl")
            .args([
                "--no-ask-password".to_string(),
                "start".to_string(),
                format!("{REMOVE_UNIT_PREFIX}{pkg}.service"),
            ])
            .timeout(WAYDROID_TIMEOUT),
    )?;
    if out.success() {
        Ok(())
    } else {
        Err(Error::msg(format!("Android would not uninstall {pkg}")))
    }
}

/// An APK path is acceptable when it is absolute, valid UTF-8 without
/// control characters, has no `..` component and ends in `.apk`.
pub fn validate_apk_path(path: &Path) -> Result<()> {
    let s = path
        .to_str()
        .ok_or_else(|| Error::msg("APK path is not valid text"))?;
    if !path.is_absolute() {
        return Err(Error::msg("APK path must be absolute"));
    }
    if s.chars().any(char::is_control) {
        return Err(Error::msg("APK path contains control characters"));
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(Error::msg("APK path must not contain .."));
    }
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("apk"))
    {
        return Err(Error::msg("Only .apk files can be installed"));
    }
    Ok(())
}

pub fn install_apk_cmd(path: &Path) -> Result<Cmd> {
    validate_apk_path(path)?;
    Ok(Cmd::new("waydroid")
        .args(["app", "install"])
        .arg(path.to_string_lossy())
        .timeout(APK_INSTALL_TIMEOUT))
}

/// Install an APK into Android (needs a running session). Waydroid hands
/// the file to Android's package manager; the app appears in
/// [`list_apps`] (and the drawer) a few seconds later.
pub fn install_apk(r: &mut dyn Runner, path: &Path) -> Result<()> {
    let cmd = install_apk_cmd(path)?;
    if !path.is_file() {
        return Err(Error::msg(format!("{} was not found", path.display())));
    }
    waydroid_ok(r, &cmd).map(|_| ())
}

/// How long to watch for the app an APK install adds.
pub const INSTALL_WATCH: Duration = Duration::from_secs(60);
const INSTALL_POLL: Duration = Duration::from_secs(2);

// ---------------------------------------------------- root install
//
// `waydroid app install` never completes on the Android TV 16 image (its
// install service commits with an immutable PendingIntent, which Android
// 16 rejects). Installs go through Android's package manager as root
// instead: the shell stages a copy of the APK and a request id in the TV
// account's cache, starts `catostv-android-install.service` (polkit allows
// `start`), and polls the result file the root script writes.

/// Where the shell stages the APK (`install.apk`) and the request id
/// (`request`), under the TV account's home.
pub const INSTALL_STAGE_DIR: &str = ".cache/catostv/android-install";
/// The unit that runs `/usr/local/lib/catostv/android-install`.
pub const INSTALL_UNIT: &str = "catostv-android-install.service";
/// `id=<id>` then `result=ok` or `result=failed:<reason>`.
pub const INSTALL_RESULT_PATH: &str = "/run/catostv/android-install.result";

/// Copy `apk` into the stage folder with a fresh request id; returns the id
/// the result must carry. The path is validated like a direct install.
pub fn stage_apk(home: &Path, apk: &Path) -> Result<String> {
    validate_apk_path(apk)?;
    if !apk.is_file() {
        return Err(Error::msg(format!("{} was not found", apk.display())));
    }
    let dir = home.join(INSTALL_STAGE_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| Error::msg(format!("{}: {e}", dir.display())))?;
    let target = dir.join("install.apk");
    let tmp = dir.join("install.apk.part");
    std::fs::copy(apk, &tmp).map_err(|e| Error::msg(format!("could not copy the APK: {e}")))?;
    std::fs::rename(&tmp, &target)
        .map_err(|e| Error::msg(format!("could not stage the APK: {e}")))?;
    let id = format!(
        "{:x}{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        std::process::id()
    );
    std::fs::write(dir.join("request"), format!("{id}\n"))
        .map_err(|e| Error::msg(format!("could not write the install request: {e}")))?;
    Ok(id)
}

/// `systemctl --no-ask-password start --no-block catostv-android-install.service`.
pub fn install_unit_cmd() -> Cmd {
    Cmd::new("systemctl")
        .args(["--no-ask-password", "start", "--no-block", INSTALL_UNIT])
        .timeout(WAYDROID_TIMEOUT)
}

/// The outcome for request `id` from the result file: `None` while it is
/// not there yet (or belongs to another request).
pub fn parse_install_result(text: &str, id: &str) -> Option<std::result::Result<(), String>> {
    let mut found = false;
    let mut result = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("id=") {
            found = v.trim() == id;
        } else if let Some(v) = line.strip_prefix("result=") {
            result = Some(v.trim().to_string());
        }
    }
    if !found {
        return None;
    }
    match result.as_deref() {
        Some("ok") => Some(Ok(())),
        Some(r) => Some(Err(r.strip_prefix("failed:").unwrap_or(r).to_string())),
        None => None,
    }
}

pub fn read_install_result(id: &str) -> Option<std::result::Result<(), String>> {
    let text = std::fs::read_to_string(INSTALL_RESULT_PATH).ok()?;
    parse_install_result(&text, id)
}

/// Stage `apk`, start the root installer and wait for its verdict (up to
/// [`APK_INSTALL_TIMEOUT`]), then name the app Android added, if any
/// (`Ok(None)`: installed but nothing new listed, an update, or a package
/// without a launcher entry). `sleep` is injected for tests.
pub fn install_apk_as_root(
    r: &mut dyn Runner,
    home: &Path,
    apk: &Path,
    sleep: &mut dyn FnMut(Duration),
) -> Result<Option<AndroidApp>> {
    let before: Vec<String> = list_apps(r)
        .map(|a| a.into_iter().map(|a| a.package).collect())
        .unwrap_or_default();
    let id = stage_apk(home, apk)?;
    r.run_ok(&install_unit_cmd())?;
    let mut waited = Duration::ZERO;
    let verdict = loop {
        sleep(INSTALL_POLL);
        waited += INSTALL_POLL;
        if let Some(v) = read_install_result(&id) {
            break v;
        }
        if waited >= APK_INSTALL_TIMEOUT {
            return Err(Error::msg("Android did not finish the install in time"));
        }
    };
    verdict.map_err(|reason| Error::msg(format!("Android refused the APK: {reason}")))?;
    let now = list_apps(r).unwrap_or_default();
    Ok(now.into_iter().find(|a| !before.contains(&a.package)))
}

/// Install `path` and wait until Android lists a new app
/// (`waydroid app install` returns at once, so "installed" appeared
/// even when Android had not added anything). `Ok(Some(app))` = the new
/// app; `Ok(None)` = nothing new within [`INSTALL_WATCH`] (an update of an
/// app that was already there, or Android refused the APK).
pub fn install_apk_and_wait(
    r: &mut dyn Runner,
    path: &Path,
    sleep: &mut dyn FnMut(Duration),
) -> Result<Option<AndroidApp>> {
    let before: Vec<String> = list_apps(r)?.into_iter().map(|a| a.package).collect();
    install_apk(r, path)?;
    let mut waited = Duration::ZERO;
    while waited < INSTALL_WATCH {
        sleep(INSTALL_POLL);
        waited += INSTALL_POLL;
        if let Ok(now) = list_apps(r)
            && let Some(new) = now.into_iter().find(|a| !before.contains(&a.package))
        {
            return Ok(Some(new));
        }
    }
    Ok(None)
}

/// Install the Aurora Store downloaded by the setup script unless Android
/// already has it (microG's bundle may ship an old copy; Android then
/// treats ours as an update). Returns `true` if an install was started.
pub fn install_aurora_if_missing(r: &mut dyn Runner) -> Result<bool> {
    install_aurora_from(r, Path::new(AURORA_APK_PATH))
}

fn install_aurora_from(r: &mut dyn Runner, apk: &Path) -> Result<bool> {
    let apps = list_apps(r)?;
    if apps.iter().any(|a| a.package == AURORA_PACKAGE) {
        return Ok(false);
    }
    install_apk(r, apk)?;
    Ok(true)
}

/// ARM translation the setup script installs for a CPU (`/proc/cpuinfo`):
/// libhoudini on Intel, libndk otherwise (the Arch wiki's advice).
pub fn arm_translation_for(cpuinfo: &str) -> &'static str {
    if cpuinfo.contains("GenuineIntel") {
        "libhoudini"
    } else {
        "libndk"
    }
}

// ---------------------------------------------------------------- USB APKs

/// Filesystems searched for APKs on removable drives.
pub const APK_FILESYSTEMS: &[&str] = &["vfat", "exfat", "ntfs", "ntfs3", "ext4"];
/// How deep below a drive's root APKs are looked for.
pub const APK_SEARCH_DEPTH: usize = 3;
/// Stop after this many APKs (a drive full of APK backups stays usable).
pub const APK_SEARCH_LIMIT: usize = 500;

/// Mount points that mark a disk as the running system.
const SYSTEM_MOUNTS: &[&str] = &["/", "/boot", "/efi", "/boot/efi", "/home", "[SWAP]"];

/// Filesystems on removable/USB disks that may hold APKs. Pure: works on
/// [`disk::parse_lsblk_pairs`] output.
pub fn apk_source_devices(all: &[BlockDevice]) -> Vec<BlockDevice> {
    let disk_of = |d: &BlockDevice| -> Option<BlockDevice> {
        let mut cur = d.clone();
        // Walk up to the whole disk (partition → disk; a few levels at most).
        for _ in 0..4 {
            if cur.kind == "disk" {
                return Some(cur);
            }
            cur = all.iter().find(|p| p.name == cur.pkname)?.clone();
        }
        None
    };
    let family_mounts = |disk: &BlockDevice| -> Vec<String> {
        all.iter()
            .filter(|d| d.name == disk.name || d.pkname == disk.name)
            .flat_map(|d| d.mountpoints.iter().cloned())
            .collect()
    };
    all.iter()
        .filter(|d| {
            (d.kind == "part" || d.kind == "disk") && APK_FILESYSTEMS.contains(&d.fstype.as_str())
        })
        .filter(|d| {
            let Some(disk) = disk_of(d) else {
                return false;
            };
            let removable = disk.removable || disk.tran == "usb" || d.removable || d.tran == "usb";
            let system = family_mounts(&disk)
                .iter()
                .any(|m| SYSTEM_MOUNTS.contains(&m.as_str()));
            removable && !system
        })
        .cloned()
        .collect()
}

/// Mount point from `udisksctl mount` stdout: `Mounted /dev/sdb1 at
/// /run/media/catostv/USB` (udisks ≤ 2.8 added a final `.`; see
/// [`resolve_mount_point`]).
pub fn parse_udisks_mount(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("Mounted ")?;
        // Device paths contain no spaces, so the first " at " separates.
        let (_, mount) = rest.split_once(" at ")?;
        let mount = mount.trim();
        mount.starts_with('/').then(|| mount.to_string())
    })
}

/// Mount point from udisks' AlreadyMounted error:
/// ``… is already mounted at `/run/media/catostv/USB'.``
pub fn parse_already_mounted(text: &str) -> Option<String> {
    let start = text.find("already mounted at `")? + "already mounted at `".len();
    let end = text[start..].find('\'')? + start;
    let m = &text[start..end];
    m.starts_with('/').then(|| m.to_string())
}

/// Drop a trailing `.` printed by old udisksctl versions if the path with
/// it does not exist but the one without does.
pub fn resolve_mount_point(raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if !p.exists()
        && let Some(trimmed) = raw.strip_suffix('.')
        && Path::new(trimmed).exists()
    {
        return PathBuf::from(trimmed);
    }
    p
}

pub fn mount_readonly_cmd(device: &str) -> Result<Cmd> {
    if !device.starts_with("/dev/")
        || device.contains("..")
        || device.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(Error::msg(format!("invalid device path {device:?}")));
    }
    Ok(Cmd::new("udisksctl")
        .args([
            "mount",
            "--no-user-interaction",
            "-b",
            device,
            "--options",
            "ro",
        ])
        .timeout(SLOW))
}

/// Mount a removable filesystem read-only through udisks2 (allowed for the
/// active local session by udisks2's default polkit policy). Returns the
/// mount point (`/run/media/<user>/<label>`).
pub fn mount_readonly(r: &mut dyn Runner, device: &str) -> Result<PathBuf> {
    let out = r.run(&mount_readonly_cmd(device)?)?;
    if out.success()
        && let Some(m) = parse_udisks_mount(&out.stdout)
    {
        return Ok(resolve_mount_point(&m));
    }
    if let Some(m) = parse_already_mounted(&out.stderr) {
        return Ok(PathBuf::from(m));
    }
    Err(Error::msg(format!(
        "Could not open the USB drive {device} ({})",
        out.failure_reason()
    )))
}

/// `*.apk` files (any case) at most `depth` directory levels below `root`.
/// Hidden entries (`.name`) and Windows system folders (`$…`, `System
/// Volume Information`) are skipped; symlinks are never followed.
pub fn find_apks(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(root, depth, &mut found);
    found.sort();
    found.truncate(APK_SEARCH_LIMIT);
    found
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if found.len() >= APK_SEARCH_LIMIT {
            return;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name.starts_with('$') || name == "System Volume Information" {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else {
            continue;
        };
        if meta.is_file() {
            let p = e.path();
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("apk"))
                && validate_apk_path(&p).is_ok()
            {
                found.push(p);
            }
        } else if meta.is_dir() && depth > 0 {
            walk(&e.path(), depth - 1, found);
        }
    }
}

/// Find APKs on every plugged-in USB/removable drive, mounting unmounted
/// filesystems read-only first. Fails only if no drive could be read.
/// One row of the APK picker: a folder to open or an `.apk` to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub dir: bool,
}

/// Mount every removable drive that is not mounted yet (read-only, via
/// udisks), so the APK picker can show them. Errors are ignored: a drive
/// that cannot be mounted is simply not listed.
pub fn mount_removable(r: &mut dyn Runner) {
    let Ok(out) = r.run_ok(&Cmd::new("lsblk").args(["-P", "-b", "-o", disk::LSBLK_COLUMNS])) else {
        return;
    };
    for d in apk_source_devices(&disk::parse_lsblk_pairs(&out.stdout)) {
        if d.mountpoints.is_empty() {
            let _ = mount_readonly(r, &d.path);
        }
    }
}

/// Where the APK picker starts ("any storage"):
/// Downloads and Home of `home`, then every mounted drive under
/// `/run/media/<user>`, `/media` and `/mnt`.
pub fn apk_roots(home: &Path) -> Vec<FileEntry> {
    let mut v = Vec::new();
    let entry = |name: &str, p: &Path| FileEntry {
        name: name.to_string(),
        path: p.to_string_lossy().into_owned(),
        dir: true,
    };
    let downloads = home.join("Downloads");
    if downloads.is_dir() {
        v.push(entry("Downloads", &downloads));
    }
    v.push(entry("Home", home));
    let user = home
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for base in [
        PathBuf::from("/run/media").join(&user),
        PathBuf::from("/media"),
        PathBuf::from("/mnt"),
    ] {
        for d in list_apk_dir(&base).into_iter().filter(|e| e.dir) {
            v.push(d);
        }
    }
    v
}

/// Folders and `.apk` files in `dir`: folders first, then files, each
/// sorted case-insensitively. Hidden and Windows system entries are left
/// out; symlinks are not followed; at most [`APK_SEARCH_LIMIT`] rows.
pub fn list_apk_dir(dir: &Path) -> Vec<FileEntry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<FileEntry> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.starts_with('$') || name == "System Volume Information"
            {
                return None;
            }
            let meta = std::fs::symlink_metadata(e.path()).ok()?;
            let path = e.path();
            let dir = meta.is_dir();
            let apk = meta.is_file()
                && path
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("apk"))
                && validate_apk_path(&path).is_ok();
            (dir || apk).then(|| FileEntry {
                name,
                path: path.to_string_lossy().into_owned(),
                dir,
            })
        })
        .collect();
    v.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    v.truncate(APK_SEARCH_LIMIT);
    v
}

pub fn removable_apks(r: &mut dyn Runner) -> Result<Vec<PathBuf>> {
    let out = r.run_ok(&Cmd::new("lsblk").args(["-P", "-b", "-o", disk::LSBLK_COLUMNS]))?;
    let devices = apk_source_devices(&disk::parse_lsblk_pairs(&out.stdout));
    let mut apks = Vec::new();
    let mut errors = Vec::new();
    let mut readable = 0;
    for d in &devices {
        let mount = match d.mountpoints.first() {
            Some(m) => PathBuf::from(m),
            None => match mount_readonly(r, &d.path) {
                Ok(m) => m,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            },
        };
        readable += 1;
        apks.extend(find_apks(&mount, APK_SEARCH_DEPTH));
    }
    if readable == 0
        && let Some(e) = errors.into_iter().next()
    {
        return Err(e);
    }
    apks.sort();
    apks.dedup();
    Ok(apks)
}

#[cfg(test)]
mod tests {

    #[test]
    fn hung_notice_parses() {
        assert_eq!(
            parse_hung("3 org.smarttube.stable\n"),
            Some((3, "org.smarttube.stable".to_string()))
        );
        assert_eq!(parse_hung("x org.a"), None);
        assert_eq!(parse_hung("1"), None);
        assert_eq!(parse_hung("1 org.a;rm"), None);
    }
    use super::*;

    #[test]
    fn parses_surfaceflinger_state() {
        assert_eq!(parse_sf("1234 2\n"), Some((1234, 2)));
        assert_eq!(parse_sf(""), None);
        assert_eq!(parse_sf("x 1"), None);
    }
    use crate::process::fake::FakeRunner;
    use crate::testutil::TempDir;

    fn repo_file(rel: &str) -> String {
        std::fs::read_to_string(format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    #[test]
    fn overlay_files_match_the_code() {
        let script = repo_file("target-root/usr/local/lib/catostv/android-setup");
        assert!(script.starts_with("#!/usr/bin/env bash\n"));
        assert!(script.contains("\nset -euo pipefail\n"));
        for path in [SETUP_LOG_PATH, "/run/catostv", "/var/lib/catostv"] {
            assert!(script.contains(path), "{path}");
        }
        assert!(script.contains("STATE_FILE=$STATE_DIR/android-setup.state"));
        assert_eq!(SETUP_STATE_PATH, "/run/catostv/android-setup.state");
        assert!(script.contains("DONE_FILE=$ANDROID_DIR/setup-done"));
        assert!(script.contains("APK_FILE=$ANDROID_DIR/AuroraStore.apk"));
        assert!(SETUP_DONE_PATH.starts_with("/var/lib/catostv/android/"));
        assert!(AURORA_APK_PATH.starts_with("/var/lib/catostv/android/"));
        // Android TV image: the OTA channel and both variants.
        assert!(script.contains("waydroid-atv.github.io/ota/a16-tv/system"));
        assert!(script.contains("IMAGE_TAG=atv16"));
        assert!(script.contains("atv.setup.bt_remote_pairing false"));
        assert!(script.contains("-s \"$SYSTEM_TYPE\""));
        assert!(script.contains("android_variant"));
        assert!(script.contains("IMAGE_FILE=$ANDROID_DIR/image"));
        assert_eq!(IMAGE_PATH, "/var/lib/catostv/android/image");
        // A CatosTV-built image goes through Waydroid's pre-installed
        // images folder, never the OTA download, and is identified by its
        // build in the image marker (deploy.sh keeps "atv16-" installs).
        assert!(script.contains("CUSTOM_DIR=$ANDROID_DIR/custom"));
        assert!(script.contains("PREINSTALLED_LINK=/etc/waydroid-extra/images"));
        assert!(script.contains("ln -sfn \"$CUSTOM_DIR\" \"$PREINSTALLED_LINK\""));
        assert!(script.contains("WANT_IMAGE=\"$IMAGE_TAG-custom-$(custom_image_id)-$VARIANT\""));
        assert!(script.contains("waydroid init -f -r lineage -s \"$SYSTEM_TYPE\""));
        let deploy = include_str!("../../../scripts/deploy.sh");
        assert!(deploy.contains("--android-image"));
        assert!(deploy.contains("/var/lib/catostv/android/custom.new"));
        assert!(deploy.contains("grep -qs '^atv16-' /var/lib/catostv/android/image"));
        // Steps announced as N/5 must be 1..=5 in order.
        let steps: Vec<u32> = script
            .lines()
            .filter_map(|l| l.strip_prefix("step "))
            .filter_map(|l| l.split_whitespace().next()?.parse().ok())
            .collect();
        assert_eq!(steps, (1..=5).collect::<Vec<_>>());
        assert!(script.contains("readonly TOTAL=5"));
        // The keystore check must never delete a keystore on its own
        // (that is how the lock-settings key went missing); Repair
        // starts Android over instead, with one previous copy kept.
        let check =
            include_str!("../../../target-root/usr/local/lib/catostv/android-keystore-check");
        assert!(!check.contains("-delete"));
        assert!(!check.contains("mode=ro"));
        assert!(check.contains("data.previous-"));
        assert!(check.contains(KEYSTORE_BROKEN_PATH));
        assert!(check.contains("PRAGMA quick_check"));
        // The watcher publishes what the shell reads, and keeps asking
        // for the Play certification id until it exists.
        let watch = include_str!("../../../target-root/usr/local/lib/catostv/android-watch");
        for needle in [
            TOP_PATH,
            APPS_PATH,
            GSF_ID_PATH,
            "publish_gsf",
            "android-gsf-id",
        ] {
            assert!(watch.contains(needle), "android-watch lacks {needle}");
        }

        let unit = repo_file("target-root/etc/systemd/system/catostv-android-setup.service");
        assert!(unit.contains(&format!("ExecStart=/usr/bin/bash {SETUP_SCRIPT}\n")));
        assert!(unit.contains("Type=oneshot"));
        assert!(
            !unit.contains("[Install]"),
            "the setup unit must never be enabled"
        );

        let rule = repo_file("target-root/etc/polkit-1/rules.d/50-catostv-android.rules");
        assert!(rule.contains(&format!("action.lookup(\"unit\") == \"{SETUP_SERVICE}\"")));
        assert!(rule.contains("action.lookup(\"verb\") == \"start\""));
        assert!(rule.contains("subject.user == \"catostv\""));
        assert!(rule.contains("subject.local") && rule.contains("subject.active"));
        assert_eq!(rule.matches("polkit.Result.YES").count(), 1);
    }

    #[test]
    fn package_validation() {
        for ok in [
            "com.aurora.store",
            "org.videolan.vlc",
            "a.b",
            "com.Foo_Bar.x9",
        ] {
            assert!(is_valid_package(ok), "{ok}");
        }
        for bad in [
            "", "single", "com..x", ".com.x", "com.x.", "com.9x", "com.x-y", "-c.x", "com.x;rm",
            "com/x.y", "com.x\n",
        ] {
            assert!(!is_valid_package(bad), "{bad:?}");
        }
    }

    const RUNNING: &str = "Session:\tRUNNING
Container:\tRUNNING
Vendor type:\tMAINLINE
IP address:\t192.168.240.112
Session user:\tcatostv(1000)
Wayland display:\twayland-0
";
    const STOPPED: &str = "Session:\tSTOPPED\nVendor type:\tMAINLINE\n";
    const UNINIT: &str = "Waydroid is not initialized, run \"waydroid init\"\n";

    #[test]
    fn parses_status() {
        let s = parse_status(RUNNING);
        assert!(s.initialized && s.session);
        assert_eq!(s.container_state.as_deref(), Some("RUNNING"));
        assert_eq!(s.vendor_type.as_deref(), Some("MAINLINE"));
        let s = parse_status(STOPPED);
        assert!(s.initialized && !s.session && s.container_state.is_none());
        let s = parse_status(UNINIT);
        assert!(!s.initialized && !s.session);
        assert_eq!(parse_status(""), AndroidStatus::default());
        let r = parse_status(RUNNING);
        assert_eq!(r.wayland_display.as_deref(), Some("wayland-0"));
        assert!(!r.through_filter());
        let f = parse_status(&RUNNING.replace("wayland-0", "wayland-android"));
        assert!(f.through_filter());
        assert!(parse_status(STOPPED).wayland_display.is_none());
    }

    #[test]
    fn stop_uses_the_template_unit_and_validates() {
        assert_eq!(
            stop_cmd("com.aurora.store").unwrap().display(),
            "systemctl --no-ask-password start --no-block catostv-android-stop@com.aurora.store.service"
        );
        assert_eq!(
            density_cmd().display(),
            "systemctl --no-ask-password start --no-block catostv-android-density.service"
        );
        assert!(stop_cmd("x; rm -rf /").is_err());
        assert!(stop_cmd("../etc").is_err());
    }

    #[test]
    fn waits_until_android_answers() {
        let mut r = FakeRunner::new();
        r.push(Output {
            timed_out: true,
            ..Output::default()
        });
        // Answers, but with no apps yet: not ready either.
        r.push(Output::ok(
            "[gbinder] Service manager /dev/binder has appeared\n",
        ));
        r.push(Output::ok(
            "Name: Google Play Store\npackageName: com.android.vending\ncategories:\n\tandroid.intent.category.LEANBACK_LAUNCHER\n",
        ));
        let mut slept = Duration::ZERO;
        let apps = wait_until_ready(&mut r, READY_TIMEOUT, &mut |d| slept += d).unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(slept, READY_POLL * 2);
        assert_eq!(r.seen[0].display(), "waydroid app list");

        // Never answers: gives up after the budget.
        let mut r = FakeRunner::new();
        for _ in 0..10 {
            r.push(Output {
                timed_out: true,
                ..Output::default()
            });
        }
        let err = wait_until_ready(&mut r, READY_POLL * 2, &mut |_| {}).unwrap_err();
        assert!(err.to_string().contains("still starting"), "{err}");
    }

    #[test]
    fn status_combines_waydroid_and_systemd() {
        let mut r = FakeRunner::new();
        r.push(Output::ok(STOPPED));
        r.push(Output::ok("active\n"));
        let s = status(&mut r);
        assert!(s.installed && s.initialized && s.container && !s.session && !s.ready());
        assert_eq!(r.seen[0].display(), "waydroid status");
        assert_eq!(
            r.seen[1].display(),
            "systemctl is-active waydroid-container.service"
        );

        let mut r = FakeRunner::new();
        r.push(Output::ok(RUNNING));
        r.push(Output::ok("active\n"));
        assert!(status(&mut r).ready());

        let mut r = FakeRunner::new();
        r.push(Output::ok(UNINIT));
        r.push(Output {
            code: Some(3),
            stdout: "inactive\n".into(),
            ..Output::default()
        });
        let s = status(&mut r);
        assert!(s.installed && !s.initialized && !s.container);

        // waydroid missing: the real runner returns Err("waydroid is not installed").
        struct Missing;
        impl Runner for Missing {
            fn run(&mut self, cmd: &Cmd) -> Result<Output> {
                Err(Error::msg(format!("{} is not installed", cmd.program)))
            }
        }
        assert_eq!(status(&mut Missing), AndroidStatus::default());

        let mut r = FakeRunner::new();
        r.push(Output {
            timed_out: true,
            ..Output::default()
        });
        let s = status(&mut r);
        assert!(s.installed && !s.initialized && !s.session);
        assert_eq!(r.seen.len(), 1);
    }

    #[test]
    fn parses_setup_state() {
        let s = parse_setup_state("step=3/7 Downloading Android (about 800 MB)\n");
        assert_eq!((s.step, s.total), (3, 7));
        assert_eq!(s.message, "Downloading Android (about 800 MB)");
        assert!(s.running());
        assert!((s.fraction() - 2.0 / 7.0).abs() < 1e-6);

        let s = parse_setup_state("step=7/7 Getting Aurora Store\nresult=ok\n");
        assert_eq!(s.result, Some(SetupResult::Ok));
        assert_eq!(s.fraction(), 1.0);

        let s = parse_setup_state(
            "step=2/7 Downloading Android\nresult=failed:No internet connection\n",
        );
        assert_eq!(
            s.result,
            Some(SetupResult::Failed("No internet connection".into()))
        );
        assert_eq!(s.message, "Downloading Android");
        let s = parse_setup_state("result=failed:");
        assert_eq!(s.result, Some(SetupResult::Failed("unknown error".into())));
        let s = parse_setup_state("garbage\nstep=x/y nope\n");
        assert_eq!(s, SetupState::default());
        assert_eq!(s.fraction(), 0.0);
    }

    #[test]
    fn reads_state_file() {
        let t = TempDir::new("setup-state");
        let p = t.write("android-setup.state", "step=1/7 Checking this PC\n");
        assert_eq!(read_setup_state_from(&p).unwrap().step, 1);
        assert!(read_setup_state_from(&t.path().join("missing")).is_none());
    }

    #[test]
    fn start_setup_command_and_errors() {
        let mut r = FakeRunner::new();
        start_setup(&mut r).unwrap();
        assert_eq!(
            r.seen[0].display(),
            "systemctl --no-ask-password start --no-block catostv-android-setup.service"
        );
        r.push(Output::failed(
            1,
            "Failed to start catostv-android-setup.service: Interactive authentication required.\n",
        ));
        assert!(
            start_setup(&mut r)
                .unwrap_err()
                .message()
                .contains("polkit")
        );
        r.push(Output::failed(
            5,
            "Unit catostv-android-setup.service not found.\n",
        ));
        assert!(
            start_setup(&mut r)
                .unwrap_err()
                .message()
                .contains("not found")
        );
    }

    #[test]
    fn session_entry_is_a_valid_app() {
        let e = session_app_entry();
        // Through the wrapper, which execs `waydroid session start`.
        assert_eq!(e.argv().unwrap(), [SESSION_SCRIPT]);
        assert_eq!(session_start_cmd().display(), "waydroid session start");
        assert_eq!(session_app_entry().command, SESSION_SCRIPT);
        let script = include_str!("../../../target-root/usr/local/lib/catostv/android-session");
        assert!(script.contains("exec waydroid session start"));
        assert!(script.contains(crate::wlfilter::ANDROID_DISPLAY));
        assert_eq!(session_stop_cmd().display(), "waydroid session stop");
    }

    const APP_LIST: &str = "Name: Settings
packageName: com.android.settings
categories:
\tandroid.intent.category.LAUNCHER
Name: Aurora Store
packageName: com.aurora.store
categories:
\tandroid.intent.category.LAUNCHER
\tandroid.intent.category.DEFAULT
Name: microG Services: Core
packageName: com.google.android.gms
categories:
Name: Broken
packageName: not a package
categories:
";

    #[test]
    fn parses_app_list() {
        let apps = parse_app_list(APP_LIST);
        assert_eq!(apps.len(), 3);
        assert_eq!(apps[1].name, "Aurora Store");
        assert_eq!(apps[1].package, "com.aurora.store");
        assert_eq!(apps[1].categories.len(), 2);
        assert!(apps[1].launchable());
        assert_eq!(apps[2].name, "microG Services: Core");
        assert!(!apps[2].launchable());
    }

    #[test]
    fn waydroid_errors_are_detected_despite_exit_code_zero() {
        let stopped = Output {
            code: Some(0),
            stderr: "[12:00:01] WayDroid session is stopped\n".into(),
            ..Output::default()
        };
        assert_eq!(
            waydroid_error(&stopped).as_deref(),
            Some("Android is not running")
        );
        let mut r = FakeRunner::new();
        r.push(stopped);
        assert!(list_apps(&mut r).is_err());
        let err = Output::ok("[12:00:01] ERROR: [Errno 2] No such file\n");
        assert_eq!(
            waydroid_error(&err).as_deref(),
            Some("[Errno 2] No such file")
        );
        assert_eq!(waydroid_error(&Output::ok(APP_LIST)), None);
        assert!(waydroid_error(&Output::failed(1, "")).is_some());
    }

    #[test]
    fn launch_and_remove_commands() {
        assert_eq!(
            launch_cmd("com.aurora.store").unwrap().display(),
            "waydroid app launch com.aurora.store"
        );
        assert!(launch_cmd("--help").is_err());
        assert!(launch_cmd("com.x; reboot").is_err());
        let mut r = FakeRunner::new();
        remove_app(&mut r, "com.aurora.store").unwrap();
        assert_eq!(r.seen[0].program, "systemctl");
        assert_eq!(
            r.seen[0].args,
            [
                "--no-ask-password",
                "start",
                "catostv-android-remove@com.aurora.store.service"
            ]
        );
        assert!(remove_app(&mut r, "../x").is_err());
    }

    #[test]
    fn apk_path_validation() {
        assert!(validate_apk_path(Path::new("/run/media/catostv/USB STICK/My App.APK")).is_ok());
        assert!(validate_apk_path(Path::new("relative/app.apk")).is_err());
        assert!(validate_apk_path(Path::new("/a/../etc/app.apk")).is_err());
        assert!(validate_apk_path(Path::new("/a/app.zip")).is_err());
        assert!(validate_apk_path(Path::new("/a/app")).is_err());
        assert!(validate_apk_path(Path::new("/a/ap\np.apk")).is_err());
        assert_eq!(
            install_apk_cmd(Path::new("/media/x y/app.apk"))
                .unwrap()
                .args,
            ["app", "install", "/media/x y/app.apk"]
        );
    }

    #[test]
    fn install_apk_checks_file_and_errors() {
        let t = TempDir::new("apk");
        let apk = t.write("Aurora.apk", "PK");
        let mut r = FakeRunner::new();
        install_apk(&mut r, &apk).unwrap();
        assert_eq!(r.seen[0].timeout, APK_INSTALL_TIMEOUT);
        assert!(install_apk(&mut r, &t.path().join("missing.apk")).is_err());
        assert_eq!(r.seen.len(), 1, "missing file must not run waydroid");
        r.push(Output {
            code: Some(0),
            stderr: "[10:00:00] WayDroid session is stopped\n".into(),
            ..Output::default()
        });
        assert!(install_apk(&mut r, &apk).is_err());
    }

    #[test]
    fn apk_install_reports_the_new_app() {
        let t = TempDir::new("apk-wait");
        let apk = t.write("SmartTube.apk", "PK");
        let mut r = FakeRunner::new();
        r.push(Output::ok(
            "Name: Settings\npackageName: com.android.settings\n",
        ));
        r.push(Output::ok("")); // app install
        r.push(Output::ok(
            "Name: Settings\npackageName: com.android.settings\n",
        ));
        r.push(Output::ok(
            "Name: Settings\npackageName: com.android.settings\nName: SmartTube\npackageName: org.smarttube.stable\n",
        ));
        let mut slept = 0;
        let new = install_apk_and_wait(&mut r, &apk, &mut |_| slept += 1).unwrap();
        assert_eq!(new.map(|a| a.name).as_deref(), Some("SmartTube"));
        assert_eq!(slept, 2);
        // Nothing new: None after the watch window, without sleeping forever.
        let mut r = FakeRunner::new();
        for _ in 0..40 {
            r.push(Output::ok(
                "Name: Settings\npackageName: com.android.settings\n",
            ));
        }
        assert!(
            install_apk_and_wait(&mut r, &apk, &mut |_| {})
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn aurora_installed_once() {
        let t = TempDir::new("aurora");
        let apk = t.write("AuroraStore.apk", "PK");
        let mut r = FakeRunner::new();
        r.push(Output::ok(APP_LIST));
        assert!(!install_aurora_from(&mut r, &apk).unwrap());
        assert_eq!(r.seen.len(), 1);
        r.push(Output::ok(
            "Name: Settings\npackageName: com.android.settings\ncategories:\n",
        ));
        assert!(install_aurora_from(&mut r, &apk).unwrap());
        assert_eq!(r.seen[2].args[..2], ["app", "install"]);
    }

    #[test]
    fn arm_translation_choice() {
        assert_eq!(
            arm_translation_for("vendor_id\t: GenuineIntel"),
            "libhoudini"
        );
        assert_eq!(arm_translation_for("vendor_id\t: AuthenticAMD"), "libndk");
        assert_eq!(arm_translation_for(""), "libndk");
    }

    // lsblk -P -b -o NAME,KNAME,PATH,PKNAME,TYPE,SIZE,RO,RM,TRAN,MODEL,SERIAL,FSTYPE,LABEL,MOUNTPOINTS
    const LSBLK: &str = r#"NAME="nvme0n1" KNAME="nvme0n1" PATH="/dev/nvme0n1" PKNAME="" TYPE="disk" SIZE="512110190592" RO="0" RM="0" TRAN="nvme" MODEL="Samsung SSD" SERIAL="S4" FSTYPE="" LABEL="" MOUNTPOINTS=""
NAME="nvme0n1p1" KNAME="nvme0n1p1" PATH="/dev/nvme0n1p1" PKNAME="nvme0n1" TYPE="part" SIZE="1073741824" RO="0" RM="0" TRAN="" MODEL="" SERIAL="" FSTYPE="vfat" LABEL="CATOSTV_EFI" MOUNTPOINTS="/boot"
NAME="nvme0n1p2" KNAME="nvme0n1p2" PATH="/dev/nvme0n1p2" PKNAME="nvme0n1" TYPE="part" SIZE="511035179008" RO="0" RM="0" TRAN="" MODEL="" SERIAL="" FSTYPE="ext4" LABEL="CATOSTV_ROOT" MOUNTPOINTS="/"
NAME="sda" KNAME="sda" PATH="/dev/sda" PKNAME="" TYPE="disk" SIZE="32015679488" RO="0" RM="1" TRAN="usb" MODEL="Cruzer" SERIAL="4C53" FSTYPE="" LABEL="" MOUNTPOINTS=""
NAME="sda1" KNAME="sda1" PATH="/dev/sda1" PKNAME="sda" TYPE="part" SIZE="32014630912" RO="0" RM="1" TRAN="" MODEL="" SERIAL="" FSTYPE="exfat" LABEL="USB STICK" MOUNTPOINTS=""
NAME="sdb" KNAME="sdb" PATH="/dev/sdb" PKNAME="" TYPE="disk" SIZE="1000204886016" RO="0" RM="0" TRAN="usb" MODEL="Portable" SERIAL="NA" FSTYPE="ntfs" LABEL="Backup" MOUNTPOINTS="/run/media/catostv/Backup"
NAME="sdc" KNAME="sdc" PATH="/dev/sdc" PKNAME="" TYPE="disk" SIZE="4000787030016" RO="0" RM="0" TRAN="sata" MODEL="HDD" SERIAL="Z1" FSTYPE="" LABEL="" MOUNTPOINTS=""
NAME="sdc1" KNAME="sdc1" PATH="/dev/sdc1" PKNAME="sdc" TYPE="part" SIZE="4000785104896" RO="0" RM="0" TRAN="" MODEL="" SERIAL="" FSTYPE="ext4" LABEL="Media" MOUNTPOINTS=""
NAME="sdd" KNAME="sdd" PATH="/dev/sdd" PKNAME="" TYPE="disk" SIZE="8000000000" RO="0" RM="1" TRAN="usb" MODEL="Stick" SERIAL="X" FSTYPE="" LABEL="" MOUNTPOINTS=""
NAME="sdd1" KNAME="sdd1" PATH="/dev/sdd1" PKNAME="sdd" TYPE="part" SIZE="8000000000" RO="0" RM="1" TRAN="" MODEL="" SERIAL="" FSTYPE="crypto_LUKS" LABEL="" MOUNTPOINTS=""
NAME="sr0" KNAME="sr0" PATH="/dev/sr0" PKNAME="" TYPE="rom" SIZE="1073741312" RO="0" RM="1" TRAN="sata" MODEL="DVD" SERIAL="" FSTYPE="iso9660" LABEL="" MOUNTPOINTS=""
"#;

    #[test]
    fn selects_removable_filesystems_only() {
        let devs = apk_source_devices(&disk::parse_lsblk_pairs(LSBLK));
        let paths: Vec<&str> = devs.iter().map(|d| d.path.as_str()).collect();
        // exFAT partition on a USB stick + a partitionless NTFS USB drive.
        // Not: system NVMe, internal SATA ext4, LUKS stick, DVD.
        assert_eq!(paths, ["/dev/sda1", "/dev/sdb"]);
    }

    #[test]
    fn a_usb_boot_disk_is_never_scanned() {
        let text = LSBLK.replace("TRAN=\"nvme\"", "TRAN=\"usb\"");
        let devs = apk_source_devices(&disk::parse_lsblk_pairs(&text));
        assert!(devs.iter().all(|d| !d.path.starts_with("/dev/nvme")));
    }

    #[test]
    fn parses_udisks_output() {
        assert_eq!(
            parse_udisks_mount("Mounted /dev/sda1 at /run/media/catostv/USB STICK\n").as_deref(),
            Some("/run/media/catostv/USB STICK")
        );
        assert_eq!(
            parse_udisks_mount("Mounted /dev/sda1 at /run/media/catostv/Look at me.\n").as_deref(),
            Some("/run/media/catostv/Look at me.")
        );
        assert_eq!(parse_udisks_mount("Error mounting\n"), None);
        assert_eq!(
            parse_already_mounted(
                "Error mounting /dev/sda1: GDBus.Error:org.freedesktop.UDisks2.Error.AlreadyMounted: Device /dev/sda1 is already mounted at `/run/media/catostv/USB'.\n"
            )
            .as_deref(),
            Some("/run/media/catostv/USB")
        );
        let t = TempDir::new("mnt");
        let real = t.path().join("USB");
        std::fs::create_dir_all(&real).unwrap();
        let raw = format!("{}.", real.display());
        assert_eq!(resolve_mount_point(&raw), real);
        assert_eq!(resolve_mount_point(&real.to_string_lossy()), real);
    }

    #[test]
    fn mount_command_is_read_only_and_validated() {
        assert_eq!(
            mount_readonly_cmd("/dev/sda1").unwrap().display(),
            "udisksctl mount --no-user-interaction -b /dev/sda1 --options ro"
        );
        assert!(mount_readonly_cmd("sda1").is_err());
        assert!(mount_readonly_cmd("/dev/../etc/passwd").is_err());
        assert!(mount_readonly_cmd("/dev/sda1 --options rw").is_err());
    }

    #[test]
    fn finds_apks_with_depth_limit() {
        let t = TempDir::new("apks");
        t.write("Top.apk", "PK");
        t.write("UPPER.APK", "PK");
        t.write("a/b/c/deep3.apk", "PK"); // depth 3: found
        t.write("a/b/c/d/deep4.apk", "PK"); // depth 4: not found
        t.write(".hidden/x.apk", "PK");
        t.write("$RECYCLE.BIN/y.apk", "PK");
        t.write("System Volume Information/z.apk", "PK");
        t.write("notes.txt", "x");
        std::os::unix::fs::symlink(t.path().join("a"), t.path().join("loop")).unwrap();
        let listed = list_apk_dir(t.path());
        assert!(listed.iter().all(|e| !e.name.starts_with('.')));
        assert!(
            listed
                .iter()
                .all(|e| e.dir || e.name.to_lowercase().ends_with(".apk"))
        );
        let first_file = listed.iter().position(|e| !e.dir).unwrap_or(listed.len());
        assert!(listed[first_file..].iter().all(|e| !e.dir), "folders first");
        let found: Vec<String> = find_apks(t.path(), APK_SEARCH_DEPTH)
            .iter()
            .map(|p| {
                p.strip_prefix(t.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(found, ["Top.apk", "UPPER.APK", "a/b/c/deep3.apk"]);
    }

    #[test]
    fn removable_apks_mounts_and_scans() {
        let t = TempDir::new("usb");
        let stick = t.path().join("stick");
        let backup = t.path().join("backup");
        t.write("stick/apps/NewPipe.apk", "PK");
        t.write("backup/old.apk", "PK");
        let lsblk = LSBLK.replace("/run/media/catostv/Backup", &backup.to_string_lossy());
        let mut r = FakeRunner::new();
        r.push(Output::ok(lsblk));
        r.push(Output::ok(format!(
            "Mounted /dev/sda1 at {}\n",
            stick.display()
        )));
        let apks = removable_apks(&mut r).unwrap();
        assert_eq!(apks.len(), 2, "{apks:?}");
        assert_eq!(r.seen[0].program, "lsblk");
        assert_eq!(
            r.seen[1].display(),
            "udisksctl mount --no-user-interaction -b /dev/sda1 --options ro"
        );
        assert_eq!(r.seen.len(), 2, "already-mounted sdb is not remounted");

        // Only drive fails to mount → error is reported.
        let only_stick = LSBLK
            .lines()
            .filter(|l| !l.contains("NAME=\"sdb\""))
            .collect::<Vec<_>>()
            .join("\n");
        let mut r = FakeRunner::new();
        r.push(Output::ok(only_stick));
        r.push(Output::failed(
            1,
            "Error mounting /dev/sda1: Not authorized\n",
        ));
        assert!(
            removable_apks(&mut r)
                .unwrap_err()
                .message()
                .contains("USB drive")
        );

        // No drives at all → empty list, not an error.
        let mut r = FakeRunner::new();
        r.push(Output::ok(
            LSBLK.lines().take(3).collect::<Vec<_>>().join("\n"),
        ));
        assert!(removable_apks(&mut r).unwrap().is_empty());
    }

    fn app(pkg: &str, cats: &[&str]) -> AndroidApp {
        AndroidApp {
            name: format!("App {pkg}"),
            package: pkg.into(),
            categories: cats.iter().map(|c| c.to_string()).collect(),
        }
    }

    #[test]
    fn tv_only_apps_get_their_own_desktop_files() {
        let t = TempDir::new("tvonly");
        let home = t.path();
        let launcher = "android.intent.category.LAUNCHER";
        let apps = vec![
            app("org.smarttube.stable", &[LEANBACK]),
            app("com.aurora.store", &[launcher]),
            app("com.both.app", &[launcher, LEANBACK]),
            app("com.android.service", &[]),
        ];
        assert!(apps[0].tv_only());
        assert!(!apps[1].tv_only() && !apps[2].tv_only() && !apps[3].tv_only());
        assert!(sync_tv_desktops(home, &apps, true));
        assert!(is_tv_only(home, "org.smarttube.stable"));
        // Phone-launcher apps get a file too, and keep it when Waydroid
        // publishes its own (which vanishes with every session).
        assert!(is_tv_only(home, "com.aurora.store"));
        std::fs::write(
            home.join(".local/share/applications/waydroid.com.aurora.store.desktop"),
            "[Desktop Entry]\nType=Application\nName=Aurora Store\nExec=waydroid app launch com.aurora.store\n",
        )
        .unwrap();
        assert!(!sync_tv_desktops(home, &apps, true));
        assert!(is_tv_only(home, "com.aurora.store"));
        assert!(!is_tv_only(home, "com.android.service"));
        // Nothing changes the second time; an empty answer removes nothing.
        assert!(!sync_tv_desktops(home, &apps, true));
        assert!(!sync_tv_desktops(home, &[], true));
        assert!(is_tv_only(home, "org.smarttube.stable"));
        // Google's TV services never get an icon.
        let svc = app(
            "com.google.android.tv.remote.service",
            &[launcher, LEANBACK],
        );
        let mut with_svc = apps.clone();
        with_svc.push(svc);
        assert!(!sync_tv_desktops(home, &with_svc, true));
        assert!(!is_tv_only(home, "com.google.android.tv.remote.service"));
        assert!(is_hidden_package("com.google.android.katniss"));
        assert!(is_hidden_package("com.android.tv.settings"));
        assert!(!is_hidden_package("com.android.vending"));
        assert!(!is_hidden_package("com.google.android.youtube.tv"));
        assert!(!is_hidden_package("org.smarttube.stable"));
        // The shell discovers it as an Android app.
        let found = crate::desktop::discover(home);
        let st = found
            .iter()
            .find(|a| a.name == "App org.smarttube.stable")
            .expect("discovered");
        assert!(
            matches!(&st.source, AppSource::Android { package } if package == "org.smarttube.stable")
        );
        // Answers from an Android still booting or stopping never remove
        // anything and do not count as "the previous answer" (the
        // Play Store icon went while Android was down).
        assert!(!sync_tv_desktops(home, &apps[1..], false));
        assert!(!sync_tv_desktops(home, &apps[1..], false));
        assert!(is_tv_only(home, "org.smarttube.stable"));
        // Uninstalled: the file goes away, on the second answer without
        // it (one short answer must not wipe the icons).
        assert!(!sync_tv_desktops(home, &apps[1..], true));
        assert!(is_tv_only(home, "org.smarttube.stable"));
        assert!(sync_tv_desktops(home, &apps[1..], true));
        assert!(!is_tv_only(home, "org.smarttube.stable"));
    }

    #[test]
    fn apps_installed_after_boot_get_an_icon_from_their_apk() {
        let t = TempDir::new("icons");
        let home = t.path();
        let apps = vec![
            app("org.smarttube.stable", &[LEANBACK]),
            app(
                "com.refuelgames.rally",
                &["android.intent.category.LAUNCHER"],
            ),
            app("com.google.android.tv.remote.service", &[LEANBACK]),
        ];
        // SmartTube has Waydroid's boot-time icon; the game has none.
        t.write(
            ".local/share/waydroid/data/icons/org.smarttube.stable.png",
            "png",
        );
        let mut r = FakeRunner::new();
        r.push(Output::ok(""));
        // The fake script "writes" the icon.
        t.write(
            ".local/share/catostv/android-icons/com.refuelgames.rally.png",
            "png",
        );
        std::fs::remove_file(
            home.join(".local/share/catostv/android-icons/com.refuelgames.rally.png"),
        )
        .unwrap();
        assert!(!ensure_icons(&mut r, home, &apps), "fake made no file");
        assert_eq!(r.seen.len(), 1, "only the game, never hidden services");
        assert_eq!(r.seen[0].program, ICON_SCRIPT);
        assert_eq!(r.seen[0].args[0], "com.refuelgames.rally");
        assert!(
            r.seen[0].args[1]
                .ends_with(".local/share/catostv/android-icons/com.refuelgames.rally.png")
        );
        // With CatosTV's icon there, the desktop file points at it; Waydroid's
        // own icon wins once it exists.
        t.write(
            ".local/share/catostv/android-icons/com.refuelgames.rally.png",
            "png",
        );
        assert!(
            icon_path(home, "com.refuelgames.rally")
                .starts_with(home.join(CATOSTV_ICONS_REL_FROM_HOME))
        );
        assert!(
            icon_path(home, "org.smarttube.stable")
                .starts_with(home.join(WAYDROID_ICONS_REL_FROM_HOME))
        );
        let mut r = FakeRunner::new();
        assert!(!ensure_icons(&mut r, home, &apps));
        assert!(r.seen.is_empty(), "nothing left to make");
        let script = include_str!("../../../target-root/usr/local/lib/catostv/android-icon");
        assert!(script.starts_with("#!/usr/bin/python3 -I"));
        assert!(script.contains("split_*.apk"));
    }

    #[test]
    fn root_install_stages_and_reads_results() {
        let t = TempDir::new("stage");
        let home = t.path();
        let apk = home.join("Downloads").join("app.apk");
        std::fs::create_dir_all(apk.parent().unwrap()).unwrap();
        std::fs::write(&apk, b"PK\x03\x04 not really").unwrap();
        let id = stage_apk(home, &apk).unwrap();
        assert!(!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()));
        let dir = home.join(INSTALL_STAGE_DIR);
        assert_eq!(
            std::fs::read(dir.join("install.apk")).unwrap(),
            b"PK\x03\x04 not really"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("request")).unwrap().trim(),
            id
        );
        assert!(stage_apk(home, &home.join("Downloads/missing.apk")).is_err());
        assert!(stage_apk(home, Path::new("relative.apk")).is_err());
        assert_eq!(
            install_unit_cmd().display(),
            "systemctl --no-ask-password start --no-block catostv-android-install.service"
        );
        let ok = format!("id={id}\nresult=ok\n");
        assert_eq!(parse_install_result(&ok, &id), Some(Ok(())));
        let bad = format!("id={id}\nresult=failed:this APK is for another processor\n");
        assert_eq!(
            parse_install_result(&bad, &id),
            Some(Err("this APK is for another processor".into()))
        );
        assert_eq!(parse_install_result("id=other\nresult=ok\n", &id), None);
        assert_eq!(parse_install_result(&format!("id={id}\n"), &id), None);
        // The root script and unit exist and read the same paths.
        let script = include_str!("../../../target-root/usr/local/lib/catostv/android-install");
        assert!(script.contains("STAGE=/home/catostv/.cache/catostv/android-install"));
        assert!(script.contains("RESULT=/run/catostv/android-install.result"));
        assert!(script.contains("pm install -r --user 0"));
        assert_eq!(INSTALL_RESULT_PATH, "/run/catostv/android-install.result");
        let unit =
            include_str!("../../../target-root/etc/systemd/system/catostv-android-install.service");
        assert!(unit.contains("ExecStart=/usr/bin/bash /usr/local/lib/catostv/android-install"));
        let rules =
            include_str!("../../../target-root/etc/polkit-1/rules.d/50-catostv-android.rules");
        assert!(rules.contains(INSTALL_UNIT));
        // The GSF helper knows the newer Play services' store.
        let gsf = include_str!("../../../target-root/usr/local/lib/catostv/android-gsf-id");
        assert!(gsf.contains("shared_prefs/Checkin.xml"));
    }

    #[test]
    fn image_labels() {
        assert_eq!(image_label("atv16-gapps"), "Android TV 16 · Google Play");
        assert_eq!(image_label("atv16-vanilla"), "Android TV 16 · Aurora Store");
        assert_eq!(
            image_label("atv16-custom-20261016-1234-gapps"),
            "Android TV 16 · CatosTV build 20261016-1234 · Google Play"
        );
        assert_eq!(image_label("atv13-gapps"), "Android TV 13 · Google Play");
        assert_eq!(image_label("lineage"), "lineage");
    }

    #[test]
    fn download_progress_from_the_setup_log() {
        let log = "step=2/5 Downloading\n[Downloading] 184.81 MB/193.87 MB       6.81 MB/s(approx.) ==> next\n";
        assert_eq!(parse_download_progress(log), Some((185, 194)));
        assert_eq!(parse_download_progress("no marker here"), None);
        assert_eq!(parse_download_progress("[Downloading] junk"), None);
        assert_eq!(crate::settings::setup_percent(2, 5, 0.5), 30);
        assert_eq!(crate::settings::setup_percent(5, 5, 1.0), 100);
        assert_eq!(crate::settings::setup_percent(1, 0, 0.0), 0);
    }

    #[test]
    fn tv_launch_and_home_detection() {
        assert_eq!(
            tv_launch_cmd("org.smarttube.stable").unwrap().args,
            [
                "--no-ask-password",
                "start",
                "catostv-android-launch@org.smarttube.stable.service"
            ]
        );
        assert!(tv_launch_cmd("bad;rm").is_err());
        assert!(is_android_home("com.android.launcher3"));
        assert!(is_android_home("org.lineageos.trebuchet"));
        assert!(!is_android_home("org.smarttube.stable"));
    }

    #[test]
    fn android_always_uses_the_full_screen_window() {
        // the Android TV 16 image sends no task names to the display
        // HAL, so only "Waydroid" (one full-screen window) ever shows.
        assert_eq!(window_of("org.smarttube.stable"), "Waydroid");
        assert_eq!(window_of("com.android.vending"), WINDOW);
        assert!(crate::toplevel::matches("Waydroid", &window_of("x.y")));
        assert!(crate::toplevel::matches("Waydroid", "waydroid*"));
        let launch = include_str!("../../../target-root/usr/local/lib/catostv/android-launch");
        assert!(launch.contains("setprop waydroid.active_apps Waydroid "));
        assert!(!launch.contains("waydroid.active_apps \"$pkg\""));
        let watch = include_str!("../../../target-root/usr/local/lib/catostv/android-watch");
        assert!(watch.contains("setprop waydroid.active_apps Waydroid'"));
        assert!(!watch.contains("setprop waydroid.active_apps $pkg"));
    }

    #[test]
    fn back_past_the_app_returns_home_only_for_older_apps() {
        let l = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        let launched = parse_launched(
            "com.refuelgames.rally\norg.smarttube.stable\ncom.android.vending\ncom.android.systemui\n",
        )
        .unwrap();
        assert_eq!(launched.package, "com.refuelgames.rally");
        assert_eq!(launched.before.len(), 3);
        // Back past the game's first screen: SmartTube (running before)
        // comes forward and the game's task went to the back.
        assert!(backed_out(
            "org.smarttube.stable",
            &launched,
            &l("org.smarttube.stable com.android.vending com.refuelgames.rally")
        ));
        // The game is still in front, or Android's system UI has focus.
        assert!(!backed_out(
            "com.refuelgames.rally",
            &launched,
            &l("com.refuelgames.rally org.smarttube.stable")
        ));
        assert!(!backed_out("com.android.systemui", &launched, &[]));
        // The game opened something new (sign-in browser, installer).
        assert!(!backed_out(
            "com.android.chrome",
            &launched,
            &l("com.android.chrome com.refuelgames.rally")
        ));
        // The game brought an existing app forward: its task is right
        // beneath that app.
        assert!(!backed_out(
            "com.android.vending",
            &launched,
            &l("com.android.vending com.refuelgames.rally org.smarttube.stable")
        ));
        // Garbage lines are skipped; an empty record is no record.
        assert_eq!(parse_launched("\n\n"), None);
        assert_eq!(
            parse_launched("x.y\nbad;rm\na.b\n").unwrap().before,
            vec!["a.b".to_string()]
        );
        let launch = include_str!("../../../target-root/usr/local/lib/catostv/android-launch");
        assert!(launch.contains("/run/catostv/android-launched"));
        let watch = include_str!("../../../target-root/usr/local/lib/catostv/android-watch");
        assert!(watch.contains("TASKS=/run/catostv/android-tasks"));
    }

    #[test]
    fn stale_session_is_one_bound_to_a_newer_or_missing_socket() {
        let cfg = "lxc.mount.entry = tmpfs /run/xdg none create=dir 0 0\n\
                   lxc.mount.entry = /run/user/1000/wayland-android run/xdg/wayland-0 none rbind,create=file 0 0\n";
        assert_eq!(
            session_socket(cfg),
            Some(PathBuf::from("/run/user/1000/wayland-android"))
        );
        assert_eq!(
            session_socket("lxc.mount.entry = /x run/xdg/pulse/native none"),
            None
        );
        let t = TempDir::new("stale");
        let sock = t.path().join("wayland-android");
        let conf = t.path().join("config_session");
        let write_cfg = || {
            std::fs::write(
                &conf,
                format!(
                    "lxc.mount.entry = {} run/xdg/wayland-0 none rbind 0 0\n",
                    sock.display()
                ),
            )
            .unwrap()
        };
        // Socket missing: stale.
        write_cfg();
        assert!(session_is_stale(&conf));
        // Socket made before the session: fine.
        std::fs::write(&sock, "").unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(600);
        std::fs::File::options()
            .write(true)
            .open(&sock)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(!session_is_stale(&conf));
        // Socket made again after the session started (TV session
        // restart): stale.
        std::fs::File::options()
            .write(true)
            .open(&conf)
            .unwrap()
            .set_modified(old)
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&sock)
            .unwrap()
            .set_modified(std::time::SystemTime::now())
            .unwrap();
        assert!(session_is_stale(&conf));
        // No config: nothing to say.
        assert!(!session_is_stale(&t.path().join("none")));
    }

    #[test]
    fn apps_start_like_the_tv_launcher_and_dumps_are_read_to_the_end() {
        let launch = include_str!("../../../target-root/usr/local/lib/catostv/android-launch");
        // the launcher's own intent, so a running app's task comes
        // to the front as it was (a bare `am start -n` stacked a new splash
        // screen on SmartTube's player and its decoder deadlocked).
        assert!(launch.contains("am start -a android.intent.action.MAIN -c \"$category\""));
        assert!(launch.contains("-f 0x10200000 -n \"$comp\""));
        assert!(launch.contains("category=android.intent.category.LAUNCHER"));
        // Never close a dumpsys pipe early: system_server then throws a
        // broken-pipe exception under its window-manager lock, every second.
        let watch = include_str!("../../../target-root/usr/local/lib/catostv/android-watch");
        for script in [launch, watch] {
            assert!(!script.contains("grep -m1 \"mCurrentFocus"));
            assert!(!script.contains("head -n 40"));
        }
    }

    #[test]
    fn controllers_only_go_to_android() {
        let pads = include_str!("../../../target-root/usr/local/lib/catostv/android-pads");
        // Re-made on every plug-in so Android sees a new node.
        assert!(pads.contains("rm -f /dev/input/$name; mknod /dev/input/$name"));
        // Joysticks only, never virtual devices or CatosTV's own keyboards.
        assert!(pads.contains("^ID_INPUT_JOYSTICK=1"));
        assert!(pads.contains("^DEVPATH=/devices/virtual/"));
        assert!(pads.contains("CatosTV*"));
        let rules = include_str!("../../../target-root/etc/udev/rules.d/70-catostv-pads.rules");
        assert!(rules.contains("ACTION==\"remove\""));
        assert!(rules.contains("android-pads remove %k"));
    }

    #[test]
    fn fresh_android_finishes_tv_setup_every_boot() {
        let density = include_str!("../../../target-root/usr/local/lib/catostv/android-tv-density");
        for key in [
            "secure user_setup_complete",
            "global device_provisioned",
            "secure tv_user_setup_complete",
        ] {
            assert!(density.contains(key), "{key}");
        }
        assert!(density.contains("pm disable-user --user 0 \"$wraith\""));
        // Only when the package exists and is not already off.
        assert!(density.contains("pm list packages \"$wraith\""));
        assert!(density.contains("pm list packages -d \"$wraith\""));
        let call = density.find("\nfinish_tv_setup\n").expect("called");
        assert!(call < density.find("exec waydroid shell -- wm density").unwrap());
        // The setup's first boot runs the same script.
        let setup = include_str!("../../../target-root/usr/local/lib/catostv/android-setup");
        assert!(setup.contains("/usr/local/lib/catostv/android-tv-density"));
    }
}
