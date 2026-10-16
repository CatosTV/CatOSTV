//! catostv-shell, the TV interface (workspace 1 of the appliance session).
//!
//! Responsibilities of this file: hold the app state, feed input to
//! `catostv_core::shell::Shell`, perform the `Effect`s it returns and feed
//! results back. Navigation rules live in `crates/catostv-core/src/shell.rs`,
//! drawing in `src/ui/shell.rs`.
//!
//! Threading: every system tool runs on a background job (`catostv::jobs`).
//! `App::logic` (called even while the window is hidden behind an app)
//! handles IPC, job results and app-lifecycle polling; `App::ui` handles
//! input and drawing.

use catostv::canvas::Canvas;
use catostv::images::Images;
use catostv::pointer::Pointer;
use catostv::ui::shell::{self as view, ShellView};
use catostv::{catfyn, config, jobs::Jobs, keys, theme};
use catostv_core::apps::{AppEntry, AppSource};
use catostv_core::audio::{self, VolumeState};
use catostv_core::bluetooth::{self, Device};
use catostv_core::display::{self, DisplayPrefs};
use catostv_core::fin::{self, Fin, FinEffect};
use catostv_core::flatpak::{self, InstallPhase, InstallProgress};
use catostv_core::input::{Action, InputMapper};
use catostv_core::ipc::{self, ShellCommand};
use catostv_core::jellyfin::{self as jf, Item};
use catostv_core::lifecycle::{AppManager, LaunchResult, LifecycleEvent};
use catostv_core::net::{self, WifiNetwork};
use catostv_core::prefs::{self, UiPrefs};
use catostv_core::process::{Cmd, Runner, SystemRunner, find_executable};
use catostv_core::settings::{AppView, RemoteInfo, ShellContext, SystemSnapshot};
use catostv_core::shell::{Effect, Page, Shell, StoreItem, StoreJob, Target};
use catostv_core::{android, compositor, desktop, env, kdeconnect, power, remote, uinput};
use eframe::egui::{self, Rect};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How often system status is refreshed in the background.
const REFRESH_SECS: f64 = 10.0;
/// Faster refresh while something is changing (Android setup, pairing…).
const FAST_REFRESH_SECS: f64 = 2.0;
const TOAST_SECS: f64 = 2.6;
/// Time to confirm HDR after the session came back with it (the start-up
/// animation takes the first seconds).
const HDR_CONFIRM: Duration = Duration::from_secs(20);
/// Interface size is applied this long after the last change.
const SCALE_SETTLE: Duration = Duration::from_millis(700);
enum Msg {
    /// KDE Connect took the name "CatosTV" (or not yet: daemon not up).
    KdeNamed(bool),
    /// Settings → Updates: a check finished.
    UpdateCheck(Result<catostv_core::updates::Check, String>),
    /// A fresh Recents preview of an app: app id, the capture and
    /// its blurred copy.
    Thumb(String, String, String),
    /// A command from catostv-ctl / catostv-inputd, with when it arrived.
    Ipc(ShellCommand, Instant),
    System(Box<SystemSnapshot>, Vec<AppEntry>),
    WifiScan(Result<Vec<WifiNetwork>, String>),
    WifiConnect(String, Result<(), String>),
    Volume(Result<VolumeState, String>),
    BtScan(Result<Vec<Device>, String>),
    BtPaired(String, Result<String, String>),
    StoreResults(Result<Vec<StoreItem>, String>),
    StoreJob(String, Option<StoreJob>),
    StoreDone(String, Result<String, String>),
    Apks(Result<Vec<android::FileEntry>, String>),
    /// An Android app was launched: now switch to the app desktop.
    AndroidOpened(String),
    /// A stale Android session was stopped: run the task again.
    AndroidRetry(AndroidTask),
    /// Back on an Android app's first screen: CatosTV took over again.
    AndroidHome,
    /// The Android app CatosTV opened never came to the front.
    AndroidDidNotStart(String),
    /// Android opened no window for the app (named) in time: CatosTV stays
    /// on Home instead of showing an empty app desktop.
    AndroidNoWindow(String),
    /// Android's screen service restarted while an Android app was in
    /// front: its window shows a frozen last frame.
    AndroidScreenRestart,
    /// android-watch closed an Android app that hung.
    AndroidHung(String),
    /// An Android app was uninstalled: (package, name).
    AndroidRemoved(String, String),
    /// Android can't start apps any more (android-watch): restart it.
    AndroidStuck,
    /// The stuck Android session has stopped: start a fresh one.
    AndroidRestarted,
    /// Quick settings key while an app was in front.
    QuickOverApp,
    /// Catfyn results.
    FinServers(Vec<jf::Server>),
    FinLogin(Result<jf::Account, String>),
    FinHome(String, Result<fin::HomeData, String>),
    FinLibrary(String, Result<Vec<Item>, String>),
    FinSeasons(String, Result<Vec<Item>, String>),
    FinEpisodes(String, Result<Vec<Item>, String>),
    /// CatosTV Home's media rows (home account).
    FinHomeMedia(Vec<fin::Row>),
    /// A Catfyn picture (URL) is in the cache at the path (None: failed).
    FinImage(String, Option<String>),
    /// Playback ended and was reported: refresh the rows.
    /// mpv is gone; `Some` = the Up next countdown ran out, play this
    /// episode with that account.
    FinPlaybackDone(Option<Box<(jf::Account, Item)>>),
    /// A folder listing for the Files app (empty dir = fresh places).
    FilesListed(String, Result<Vec<catostv_core::files::Entry>, String>),
    /// A file operation in `dir` failed: toast, then list again.
    FilesFailed(String, String),
    /// Catfyn search results for the Search page.
    SearchMedia(String, Vec<Item>),
    /// An app started/ended (from the app watcher thread).
    Lifecycle(LifecycleEvent),
    /// An APK install finished (toast text); refresh the app list.
    ApkDone(String),
    /// App `id` opened a window with this Wayland app id.
    WindowSeen(String, String),
    /// Android's package set changed (Play Store install/uninstall).
    AndroidAppsChanged,
    /// Appstream details for the Store page of app `id`.
    StoreDetails(String, Result<flatpak::AppDetails, String>),
    /// A Store screenshot (URL) was downloaded to the path.
    Screenshot(String, String),
    ModeFailed(String),
    Toast(String),
    Refresh,
    Nothing,
}

/// What the app watcher thread needs to know about the UI. The shell's
/// own frames stop while its window is on the hidden desktop (Wayland sends
/// no frame callbacks there), so everything that must happen while an app
/// is in front, noticing the app exit and switching back, Recents
/// captures, runs on that thread (closing Jellyfin
/// left a black App desktop).
#[derive(Default)]
struct Watch {
    /// The app on the App desktop (see `ShellApp::foreground`).
    foreground: Option<String>,
    /// `foreground` is an Android app (so the Android session ending counts).
    android_front: bool,
    /// Last time the shell drew a frame; old = the shell is hidden.
    last_frame: Option<Instant>,
    appliance: bool,
    /// The Android app CatosTV opened last, and when: an app that
    /// never comes to the front is noticed after [`ANDROID_START_GRACE`].
    android_opened: Option<(String, Instant)>,
}

/// How long an Android app may take to come to the front after CatosTV
/// opened it before Android's launcher or an older app showing instead
/// means it did not start (Android's own start timeout is 10 s).
const ANDROID_START_GRACE: Duration = Duration::from_secs(12);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Poll apps (and follow Android's front app) independently of the UI loop.
/// Put Waydroid's window away while CatosTV is in front (it stayed
/// on the App desktop holding its last frame and showed through whenever
/// the compositor switched, "ghost" app screens). Opening an Android app
/// activates it again (`toplevel::activate` unminimizes).
/// Copy a file, or a folder with everything in it (symlinks skipped).
fn copy_tree(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir(dest)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_tree(&e.path(), &dest.join(e.file_name()))?;
        }
        Ok(())
    } else if meta.is_file() {
        std::fs::copy(src, dest).map(|_| ())
    } else {
        Ok(())
    }
}

/// Leaving an Android app: nothing is done to its window. The CatosTV
/// workspace switch hides it, and any minimize would be a configure event
/// a display re-plug to Waydroid's HAL on the Android TV image, which
/// resets Android's screen and leaves a stale window behind (the
/// old minimize-on-Home was behind "Android restarted" and the duplicate
/// windows).
fn hide_android_window() {}

fn spawn_app_watcher(
    manager: Arc<Mutex<AppManager>>,
    watch: Arc<Mutex<Watch>>,
    (tx, ctx): (std::sync::mpsc::Sender<Msg>, egui::Context),
) {
    const TICK: Duration = Duration::from_millis(300);
    // The shell repaints at least every 250 ms while visible.
    const HIDDEN_AFTER: Duration = Duration::from_millis(1500);
    // How often the app in front is captured for Recents.
    const THUMB_EVERY: Duration = Duration::from_secs(4);
    let run = move || {
        let mut front = None;
        // The Android app on screen has been seen in front since it was
        // opened (so Android's home screen showing afterwards means the user
        // backed out of it, not that it is still starting).
        let mut android_seen: Option<String> = None;
        // The launch record (`android-launch`) whose app has been seen in
        // front since; only then can a change of front app mean "backed
        // out".
        let mut launch_seen: Option<android::Launched> = None;
        // The app CatosTV opened last (and when), and whether it has been
        // Android's front app since.
        let mut opened_key: Option<(String, Instant)> = None;
        let mut opened_seen = false;
        let mut sf_restarts: Option<u32> = None;
        let mut hung_count: Option<u32> = None;
        let mut stuck_count: Option<u32> = None;
        let mut was_hidden = false;
        let mut tidy_tick = 0u8;
        let mut apps_stamp: Option<String> = None;
        let mut last_top: Option<String> = None;
        let mut last_thumb: Option<Instant> = None;
        let mut generation: HashMap<String, u32> = HashMap::new();
        loop {
            std::thread::sleep(TICK);
            let events = lock(&manager).poll();
            let (fg, android_front, appliance, hidden) = {
                let w = lock(&watch);
                (
                    w.foreground.clone(),
                    w.android_front,
                    w.appliance,
                    w.last_frame.is_none_or(|t| t.elapsed() > HIDDEN_AFTER),
                )
            };
            // What the remotes are talking to (catostv_core::uinput): Android
            // gets Android TV keys (Back = KEY_BACK).
            let now_front = match (hidden, android_front) {
                (false, _) => uinput::Front::Shell,
                (true, true) => uinput::Front::Android,
                (true, false) => uinput::Front::App,
            };
            if front != Some(now_front) && appliance {
                uinput::set_front(now_front);
                front = Some(now_front);
            }
            // CatosTV just came to the front: Waydroid's window goes away.
            if appliance && was_hidden && !hidden {
                hide_android_window();
            }
            was_hidden = hidden;
            // An Android app in front: Waydroid's full-screen window (every
            // Android app shows in it) is kept in front; it is never
            // minimized or closed.
            tidy_tick = (tidy_tick + 1) % 4;
            if appliance && hidden && android_front && tidy_tick == 0 {
                // Only when Android's front app changes (every
                // window touch is a re-plug), and only a raise.
                let top = android::read_top().filter(|t| !android::is_android_home(t));
                if top != last_top {
                    if let Some(t) = &top {
                        let _ = catostv_core::toplevel::raise_if_needed(&android::window_of(t));
                    }
                    last_top = top;
                }
            }
            // Something installed or removed inside Android (Play Store):
            // the app list is re-read.
            if appliance && tidy_tick == 2 {
                let stamp = android::read_apps_stamp();
                if stamp != apps_stamp {
                    // The first fingerprint after a boot counts too: it is
                    // the moment Android can list its apps, and the icons
                    // are made from that list.
                    if stamp.is_some() {
                        let _ = tx.send(Msg::AndroidAppsChanged);
                        ctx.request_repaint();
                    }
                    apps_stamp = stamp;
                }
            }
            // Android's screen service died (its window keeps the last
            // frame): back to CatosTV, which tells the user.
            if let Some((_, restarts)) = android::read_sf() {
                // Whether or not the shell already counts as hidden: the
                // crash can come within a second of the launch.
                if sf_restarts.is_some_and(|r| restarts > r) && appliance && android_front {
                    let _ = compositor::show_shell_workspace(&mut SystemRunner, true);
                    hide_android_window();
                    let _ = tx.send(Msg::AndroidScreenRestart);
                    ctx.request_repaint();
                }
                sf_restarts = Some(restarts);
            }
            // Android stopped being able to start apps (android-watch).
            if let Some(count) = android::read_stuck() {
                if stuck_count.is_some_and(|c| count > c) && appliance {
                    let _ = tx.send(Msg::AndroidStuck);
                    ctx.request_repaint();
                }
                stuck_count = Some(count);
            } else {
                stuck_count = Some(0);
            }
            // An Android app hung and android-watch closed it.
            if let Some((count, pkg)) = android::read_hung() {
                if hung_count.is_some_and(|c| count > c) && appliance {
                    let _ = tx.send(Msg::AndroidHung(pkg));
                    ctx.request_repaint();
                }
                hung_count = Some(count);
            } else {
                hung_count = Some(0);
            }
            // Back on an Android app's first screen shows Android's own home
            // screen: come back to CatosTV instead, like any other app.
            // In full-screen mode Back past the app's first screen shows
            // Android's previous task instead: an app that was
            // running before CatosTV opened this one means the user backed
            // out too, unless the app itself brought it forward.
            if appliance && hidden && android_front {
                let launched = android::read_launched();
                if launch_seen.as_ref() != launched.as_ref() {
                    launch_seen = None;
                }
                let opened = lock(&watch).android_opened.clone();
                if opened_key != opened {
                    opened_seen = false;
                    opened_key = opened.clone();
                }
                let grace_over = opened
                    .as_ref()
                    .is_none_or(|(_, at)| at.elapsed() > ANDROID_START_GRACE);
                let opened_pkg = opened.map(|(p, _)| p);
                // `Some(None)`: back Home; `Some(Some(pkg))`: back Home, the
                // app CatosTV opened never came up.
                let mut go_home: Option<Option<String>> = None;
                match android::read_top() {
                    // Android's launcher: the app was left (Back past its
                    // first screen), died, or never started, always back
                    // to CatosTV. Right after opening, only once the
                    // app has shown or its start time is over.
                    Some(top) if android::is_android_home(&top) => {
                        if opened_seen || (opened_pkg.is_none() && android_seen.is_some()) {
                            go_home = Some(None);
                        } else if grace_over {
                            go_home = Some(opened_pkg.clone());
                        }
                    }
                    Some(top) => {
                        if opened_pkg.as_deref() == Some(top.as_str()) {
                            opened_seen = true;
                        }
                        if launched.as_ref().is_some_and(|l| l.package == top) {
                            launch_seen = launched.clone();
                            android_seen = Some(top);
                        } else if let Some(l) = &launch_seen
                            && android::backed_out(&top, l, &android::read_tasks())
                        {
                            go_home = Some(None);
                        } else if !opened_seen
                            && grace_over
                            && opened_pkg.as_deref().is_some_and(|p| p != top)
                            && launched.as_ref().is_some_and(|l| l.before.contains(&top))
                        {
                            // The opened app never came up and Android fell
                            // back to an app that was running before
                            // ("SmartTube opened YouTube").
                            go_home = Some(opened_pkg.clone());
                        } else {
                            android_seen = Some(top);
                        }
                    }
                    None => {}
                }
                if let Some(failed) = go_home {
                    launch_seen = None;
                    android_seen = None;
                    lock(&watch).android_opened = None;
                    let _ = compositor::show_shell_workspace(&mut SystemRunner, true);
                    hide_android_window();
                    let _ = tx.send(match failed {
                        Some(pkg) => Msg::AndroidDidNotStart(pkg),
                        None => Msg::AndroidHome,
                    });
                    ctx.request_repaint();
                }
            } else {
                android_seen = None;
                launch_seen = None;
            }
            if !events.is_empty() {
                let front_ended = events.iter().any(|ev| {
                    fg.as_deref() == Some(ev.id())
                        || (android_front && ev.id() == android::SESSION_APP_ID)
                });
                if front_ended && appliance {
                    // Back to CatosTV right away; the UI catches up (and
                    // handles the event) once it is visible again.
                    let _ = compositor::show_shell_workspace(&mut SystemRunner, true);
                    hide_android_window();
                }
                for ev in events {
                    if tx.send(Msg::Lifecycle(ev)).is_err() {
                        return;
                    }
                }
                ctx.request_repaint();
                continue;
            }
            // Recents preview of the app in front: a small
            // screenshot every few seconds while CatosTV itself is hidden
            // (so a capture never shows CatosTV), plus a blurred copy for
            // the Recents backdrop. Android apps are named by the package
            // Android shows, so each Android app gets its own preview.
            let id = if android_front {
                android::read_top()
                    .filter(|t| !android::is_android_home(t))
                    .map(|t| format!("android-{}", catostv_core::apps::slug(&t)))
            } else {
                fg
            };
            let Some(id) = id else { continue };
            if !appliance || !hidden || last_thumb.is_some_and(|t| t.elapsed() < THUMB_EVERY) {
                continue;
            }
            last_thumb = Some(Instant::now());
            let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
                continue;
            };
            let dir = PathBuf::from(dir).join("catostv-recents");
            let _ = std::fs::create_dir_all(&dir);
            let g = generation.entry(id.clone()).or_insert(0);
            *g = g.wrapping_add(1);
            let shot = dir
                .join(format!("{id}-{}.png", *g % 2))
                .to_string_lossy()
                .into_owned();
            let blur = dir
                .join(format!("{id}-{}-blur.png", *g % 2))
                .to_string_lossy()
                .into_owned();
            let cmd = Cmd::new("grim")
                .args(["-s", "0.25", "-l", "1", "-t", "png", &shot])
                .timeout(Duration::from_secs(5));
            if SystemRunner.run(&cmd).is_ok_and(|o| o.success())
                && catostv::images::blurred_copy(&shot, &blur)
            {
                let _ = tx.send(Msg::Thumb(id, shot, blur));
                ctx.request_repaint();
            }
        }
    };
    if let Err(e) = std::thread::Builder::new()
        .name("catostv-apps".into())
        .spawn(run)
    {
        eprintln!("catostv-shell: could not start the app watcher: {e}");
    }
}

/// Something to do once the Android session is up.
#[derive(Clone)]
enum AndroidTask {
    Launch(String),
    InstallApk(String),
    OpenAurora,
    /// Put Aurora Store on Home after setup (no launch).
    InstallAurora,
}

struct ShellApp {
    /// KDE Connect announces "CatosTV" (set once the daemon answers).
    kde_named: bool,
    /// An update finished: restart the interface shortly.
    restart_after_update: Option<Instant>,
    /// Recents previews: app id → (capture, blurred copy).
    thumbs: HashMap<String, (String, String)>,
    /// CPU/memory per app for Recents, sampled while it is open.
    usage: catostv_core::usage::Usage,
    sampler: catostv_core::usage::Sampler,
    usage_at: Option<Instant>,
    shell: Shell,
    mapper: InputMapper,
    pointer: Pointer,
    hits: Vec<(Rect, Target)>,
    hover: Option<Target>,
    configured: Vec<AppEntry>,
    discovered: Vec<AppEntry>,
    apps: Vec<AppEntry>,
    installed: Vec<bool>,
    /// Shared with the app watcher thread, which polls it.
    manager: Arc<Mutex<AppManager>>,
    watch: Arc<Mutex<Watch>>,
    sys: SystemSnapshot,
    prefs: UiPrefs,
    display: DisplayPrefs,
    images: Images,
    store_icons: HashMap<String, String>,
    jobs: Jobs<Msg>,
    /// Toast text and wall-clock expiry. Wall-clock (not egui time) because
    /// egui's clock is frozen while the shell is hidden behind an app.
    toast: Option<(String, Instant)>,
    last_refresh: Option<Instant>,
    refreshing: bool,
    /// The app most recently launched/resumed, the one on the App desktop.
    /// Only its exit switches the TV back to the shell.
    foreground: Option<String>,
    pending_restart: HashSet<String>,
    /// The Waydroid session was started by this shell instance (once, at
    /// start-up or right after setup) so Android apps open without delay.
    android_started: bool,
    /// Android apps launched since the session started (shown as running,
    /// closable from Recents). Cleared when the Android session ends.
    android_running: Vec<String>,
    /// App id → its window's Wayland app id (learned at launch).
    windows: HashMap<String, String>,
    /// Store screenshots: URL → cached PNG.
    store_shots: HashMap<String, String>,
    /// The app being opened (launch bounce on its tile) and since when.
    launching: Option<(String, Instant)>,
    /// When the shell started (start-up animation).
    started: Instant,
    /// Notices waking from Sleep (keys right after are ignored).
    wake: power::WakeGuard,
    /// Catfyn accounts and the Home "Continue watching" row.
    media: fin::Media,
    /// Catfyn pictures: URL → cached PNG (None = failed, do not retry).
    fin_images: HashMap<String, Option<String>>,
    /// Pictures the last frame wanted but did not have.
    fin_wanted: RefCell<Vec<String>>,
    fin_fetching: HashSet<String>,
    last_media_refresh: Option<Instant>,
    /// When the current page was opened (page transition animation).
    page_since: Instant,
    last_page: Page,
    confirm_deadline: Option<Instant>,
    /// Controller navigation waiting for the next frame.
    pending_nav: Vec<catostv_core::input::Action>,
    /// Last time the shell asked to be full screen again, and how many
    /// times in a row.
    refit_at: Option<Instant>,
    refits: u32,
    /// Interface size waiting to be applied: (output, scale, when).
    pending_scale: Option<(String, f32, Instant)>,
    first_snapshot: bool,
    was_focused: bool,
    appliance: bool,
    home: PathBuf,
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/catostv"))
}

/// True if a process with this exact name (`/proc/<pid>/comm`) is running.
/// Whether a process called `name` runs. The kernel keeps only the first
/// 15 characters of a process name (`catostv-wlfilter` shows as
/// `catostv-wlfilte`), so compare those.
fn process_running(name: &str) -> bool {
    let name: String = name.chars().take(15).collect();
    std::fs::read_dir("/proc")
        .map(|d| {
            d.flatten().any(|e| {
                std::fs::read_to_string(e.path().join("comm"))
                    .map(|c| c.trim() == name)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn snapshot(appliance: bool, home: &Path) -> (SystemSnapshot, Vec<AppEntry>) {
    let mut r = SystemRunner;
    let bt = bluetooth::status(&mut r);
    let has = |p: &str| find_executable(p).is_some();
    let flatpak_available = has("flatpak");
    let android = if has("waydroid") {
        android::status(&mut r)
    } else {
        android::AndroidStatus::default()
    };
    let remote = remote::read_pin().map(|pin| RemoteInfo {
        url: remote::local_ipv4()
            .map(remote::url_hint)
            .unwrap_or_else(|| format!("port {}", remote::PORT)),
        pin,
    });
    let bt_devices = if bt.available {
        bluetooth::paired_devices(&mut r)
    } else {
        Vec::new()
    };
    let bt_details = bt_devices
        .iter()
        .map(|d| {
            bluetooth::info(&mut r, &d.address).unwrap_or_else(|_| bluetooth::DeviceInfo {
                address: d.address.clone(),
                name: d.name.clone(),
                paired: true,
                ..Default::default()
            })
        })
        .collect();
    let sys = SystemSnapshot {
        wifi_on: net::wifi_enabled(&mut r).ok(),
        net: net::status(&mut r).ok(),
        bluetooth: bt,
        bt_devices,
        bt_details,
        volume: audio::get(&mut r).ok(),
        appliance,
        outputs: if has("wlr-randr") {
            display::outputs(&mut r).unwrap_or_default()
        } else {
            Vec::new()
        },
        sinks: audio::sinks(&mut r).unwrap_or_default(),
        hdmi_hint: audio::cards(&mut r)
            .ok()
            .and_then(|cards| audio::hdmi_profile_hint(&cards)),
        android,
        android_setup: android::read_setup_state(),
        android_repairing: android::repair_running(&mut r),
        gsf_id: android::read_gsf_id(),
        android_download: android::read_download_progress(),
        android_image: android::installed_image(),
        flatpaks: if flatpak_available {
            flatpak::installed(&mut r).unwrap_or_default()
        } else {
            Vec::new()
        },
        flatpak_available,
        remote,
        kde: if process_running("kdeconnectd") {
            kdeconnect::devices(&mut r).unwrap_or_default()
        } else {
            Vec::new()
        },
        cast_running: process_running("catostv-castd"),
        ip_address: remote::local_ipv4().map(|ip| ip.to_string()),
        storage: catostv_core::sysinfo::storage(&mut r),
        ethernet: net::ethernet(&mut r),
        updates: catostv_core::settings::UpdatesView {
            install: catostv_core::updates::read_state(),
            ..Default::default()
        },
        debug: catostv_core::debug::status(process_running("catostv-wlfilter")),
        controllers: catostv_core::evdev::scan_devices(
            Path::new("/dev/input"),
            Path::new("/sys/class/input"),
        )
        .into_iter()
        .filter(|d| d.gamepad)
        .map(|d| d.name)
        .collect(),
    };
    (sys, desktop::discover(home))
}

/// Run a flatpak transaction, streaming progress to the shell.
fn run_store_job(tx: std::sync::mpsc::Sender<Msg>, ctx: egui::Context, id: String, remove: bool) {
    let send = |m: Msg| {
        let _ = tx.send(m);
        ctx.request_repaint();
    };
    let mut r = SystemRunner;
    if !remove && let Err(e) = flatpak::ensure_flathub_user(&mut r) {
        return send(Msg::StoreDone(id, Err(e.to_string())));
    }
    let cmd = if remove {
        flatpak::uninstall_cmd(&id)
    } else {
        flatpak::install_progress_cmd(&id)
    };
    let cmd = match cmd {
        Ok(c) => c,
        Err(e) => return send(Msg::StoreDone(id, Err(e.to_string()))),
    };
    let child = std::process::Command::new(&cmd.program)
        .args(&cmd.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return send(Msg::StoreDone(id, Err(format!("flatpak: {e}")))),
    };
    // stderr is drained on its own thread so a full pipe never blocks.
    let mut stderr = child.stderr.take();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let mut progress = InstallProgress::new();
    let mut buf = [0u8; 4096];
    let mut line = Vec::new();
    let mut last_sent = Instant::now();
    if let Some(mut out) = child.stdout.take() {
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 {
                break;
            }
            for &b in &buf[..n] {
                if b == b'\n' || b == b'\r' {
                    progress.feed(&String::from_utf8_lossy(&line));
                    line.clear();
                } else {
                    line.push(b);
                }
            }
            if !remove && last_sent.elapsed() > Duration::from_millis(300) {
                last_sent = Instant::now();
                send(Msg::StoreJob(
                    id.clone(),
                    Some(StoreJob::Installing(progress.fraction())),
                ));
            }
        }
    }
    progress.feed(&String::from_utf8_lossy(&line));
    let status = child.wait();
    let stderr = err_thread.join().unwrap_or_default();
    for l in stderr.lines() {
        progress.feed(l);
    }
    let ok = status.map(|s| s.success()).unwrap_or(false);
    let result = match (&progress.phase, ok) {
        (InstallPhase::Failed(e), _) => Err(e.clone()),
        (_, true) => Ok(id.clone()),
        (_, false) => Err(stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("flatpak failed")
            .trim()
            .to_string()),
    };
    send(Msg::StoreDone(id, result));
}

impl ShellApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // A reload of the interface starts clean: apps the previous one left
        // running (it was restarted or killed: a deploy, a crash) are
        // stopped, a Catfyn player nothing could control any more stayed
        // stuck on screen. Android's session is one of them, so Android
        // apps stop too and Android starts again with this interface.
        let left = catostv_core::lifecycle::stop_leftovers(&catostv_core::lifecycle::record_path());
        if !left.is_empty() {
            eprintln!(
                "catostv: stopped apps the last interface left running: {}",
                left.join(", ")
            );
        }
        theme::setup(&cc.egui_ctx);
        let mut prefs = prefs::load();
        // HDR lasts until the next reboot only (runtime flag).
        prefs.hdr = prefs::hdr_flag_path().exists();
        theme::set_accent(&prefs.accent);
        let jobs = Jobs::new(&cc.egui_ctx);
        let appliance = env::is_appliance();
        let mut app = Self {
            shell: Shell::new(),
            restart_after_update: None,
            kde_named: false,
            thumbs: HashMap::new(),
            usage: Default::default(),
            sampler: catostv_core::usage::Sampler::new(),
            usage_at: None,
            mapper: InputMapper::new(),
            pointer: Pointer::new(),
            hits: Vec::new(),
            hover: None,
            configured: Vec::new(),
            discovered: Vec::new(),
            apps: Vec::new(),
            installed: Vec::new(),
            manager: Arc::new(Mutex::new(
                AppManager::new(Some(env::state_dir().join("apps")))
                    .with_record(catostv_core::lifecycle::record_path()),
            )),
            watch: Arc::new(Mutex::new(Watch {
                appliance,
                ..Watch::default()
            })),
            sys: SystemSnapshot {
                appliance,
                ..SystemSnapshot::default()
            },
            prefs,
            display: display::load_prefs(),
            images: Images::new(),
            store_icons: HashMap::new(),
            jobs,
            toast: None,
            last_refresh: None,
            refreshing: false,
            foreground: None,
            pending_restart: HashSet::new(),
            android_started: false,
            android_running: Vec::new(),
            store_shots: HashMap::new(),
            launching: None,
            started: Instant::now(),
            wake: power::WakeGuard::default(),
            media: fin::Media {
                accounts: jf::load_accounts(&home_dir()),
                home: Vec::new(),
            },
            fin_images: HashMap::new(),
            fin_wanted: RefCell::new(Vec::new()),
            fin_fetching: HashSet::new(),
            last_media_refresh: None,
            windows: HashMap::new(),
            page_since: Instant::now(),
            last_page: Page::Home,
            confirm_deadline: None,
            pending_scale: None,
            pending_nav: Vec::new(),
            refit_at: None,
            refits: 0,
            first_snapshot: true,
            was_focused: true,
            appliance,
            home: home_dir(),
        };
        let (tx, ctx) = app.jobs.sender();
        let watch = app.watch.clone();
        if let Err(e) = ipc::spawn_listener(ipc::socket_path(), move |cmd| {
            // Back ×3 / hold Back inside an app (catostv-inputd): switch to
            // CatosTV right here, the UI loop is paused while an app is in
            // front, so queuing it did nothing.
            if matches!(cmd, ShellCommand::Escape) {
                let (appliance, hidden) = {
                    let w = lock(&watch);
                    (
                        w.appliance,
                        w.last_frame
                            .is_none_or(|t| t.elapsed() > Duration::from_millis(1500)),
                    )
                };
                if appliance && hidden {
                    let _ = compositor::show_shell_workspace(&mut SystemRunner, true);
                    hide_android_window();
                }
            }
            // Quick settings asked for while an app is in front: Back
            // returns to that app.
            if matches!(cmd, ShellCommand::Quick) {
                let hidden = lock(&watch)
                    .last_frame
                    .is_none_or(|t| t.elapsed() > Duration::from_millis(1500));
                if hidden {
                    let _ = tx.send(Msg::QuickOverApp);
                    ctx.request_repaint();
                    return;
                }
            }
            let _ = tx.send(Msg::Ipc(cmd, Instant::now()));
            ctx.request_repaint();
        }) {
            eprintln!("catostv-shell: {e}");
            app.show_toast(&cc.egui_ctx, "Remote shortcuts are unavailable");
        }
        // Android apps the last interface left open close too (Android
        // itself keeps running, so they reopen quickly): a reload stops all
        // apps.
        if appliance {
            app.jobs.spawn(|| {
                let mut r = SystemRunner;
                for pkg in android::read_tasks() {
                    if android::is_android_home(&pkg) || pkg == "com.android.systemui" {
                        continue;
                    }
                    if let Ok(cmd) = android::stop_cmd(&pkg) {
                        let _ = r.run_ok(&cmd);
                    }
                }
                Msg::Nothing
            });
        }
        spawn_app_watcher(app.manager.clone(), app.watch.clone(), app.jobs.sender());
        app.reload_apps(&cc.egui_ctx);
        if appliance && !app.prefs.key_guide_seen {
            app.shell.show_key_guide();
        }
        // HDR was just switched on: confirm it, or it turns off by itself.
        if appliance
            && std::fs::read_to_string(prefs::hdr_flag_path()).is_ok_and(|t| t.trim() == "pending")
        {
            app.shell.confirm_hdr();
            app.confirm_deadline = Some(Instant::now() + HDR_CONFIRM);
        }
        app
    }

    fn reload_apps(&mut self, ctx: &egui::Context) {
        let (apps, warnings) = config::load_apps(&env::apps_config_path());
        self.configured = apps;
        self.rebuild_apps();
        if let Some(w) = warnings.first() {
            eprintln!("catostv-shell: {}", warnings.join("; "));
            self.show_toast(ctx, w);
        }
    }

    /// Configured apps first, then installed store / Android apps
    /// (store copies of configured apps are merged, see `apps::merge`).
    fn rebuild_apps(&mut self) {
        let mut apps =
            catostv_core::apps::merge(&self.configured, &self.discovered, |a| a.is_installed());
        // Apps whose program is missing are not shown at all;
        // a running one stays so it can be resumed/closed.
        apps.retain(|a| a.is_installed() || self.mgr().is_running(&a.id));
        // Papirus icons where the theme has one.
        for a in &mut apps {
            if let Some(p) = catostv_core::icon_theme::papirus_for(a) {
                a.icon_path = Some(p.to_string_lossy().into_owned());
            }
        }
        self.installed = apps.iter().map(AppEntry::is_installed).collect();
        self.apps = apps;
    }

    fn views(&self) -> Vec<AppView> {
        self.apps
            .iter()
            .zip(&self.installed)
            .map(|(entry, &installed)| AppView {
                entry: entry.clone(),
                installed,
                running: self.mgr().is_running(&entry.id)
                    || (entry.command == catostv_core::apps::FIN_COMMAND
                        && self.mgr().is_running(catfyn::PLAYER_ID))
                    || matches!(&entry.source, AppSource::Android { package }
                        if self.android_running.contains(package)),
            })
            .collect()
    }

    fn show_toast(&mut self, ctx: &egui::Context, text: &str) {
        // In shell.log too: what the TV said is what a report needs.
        eprintln!("catostv: notice: {text}");
        self.toast = Some((
            text.to_string(),
            Instant::now() + Duration::from_secs_f64(TOAST_SECS),
        ));
        ctx.request_repaint();
    }

    fn refresh_system(&mut self) {
        if self.refreshing {
            return;
        }
        let appliance = self.appliance;
        let home = self.home.clone();
        self.refreshing = self.jobs.try_spawn(move || {
            let (s, d) = snapshot(appliance, &home);
            Msg::System(Box::new(s), d)
        });
    }

    fn refresh_soon(&mut self) {
        self.last_refresh = None;
    }

    /// You left Catfyn's player for Home: pause it (it keeps running in the
    /// background, and its tile resumes it). Nothing when no film plays.
    fn pause_catfyn_player(&self) {
        if !self.mgr().is_running(catfyn::PLAYER_ID) {
            return;
        }
        self.jobs.spawn(|| {
            let _ = catostv_core::mpv::ipc_request(
                &catostv_core::jellyfin::player_socket(),
                &[
                    catostv_core::mpv::Arg::Str("set_property"),
                    catostv_core::mpv::Arg::Str("pause"),
                    catostv_core::mpv::Arg::Bool(true),
                ],
            );
            Msg::Nothing
        });
    }

    /// You left an Android app for CatosTV: Android goes to its own home
    /// screen, so the app pauses (see android-home).
    fn send_android_home(&self) {
        if self.appliance && self.sys.android.session {
            self.job(|r| r.run_ok(&android::home_cmd()).map(|_| ()));
        }
    }

    fn return_to_shell(&self) {
        if self.appliance {
            self.jobs.spawn(|| {
                let r = compositor::show_shell_workspace(&mut SystemRunner, true);
                hide_android_window();
                match r {
                    Ok(()) => Msg::Nothing,
                    Err(e) => Msg::Toast(e.to_string()),
                }
            });
        }
    }

    /// Show the App desktop with `app`'s window in front of the others
    /// (the desktop alone showed whichever app was on
    /// top, FreeTube instead of the Android app just opened).
    fn show_app(&self, app_id: &str) {
        if !self.appliance {
            return;
        }
        let window = self.window_pattern(app_id);
        let name = self
            .apps
            .iter()
            .find(|a| a.id == app_id)
            .map_or_else(|| "Android".to_string(), |a| a.name.clone());
        // The Android app being opened, to wait until Android shows it.
        let package = self
            .apps
            .iter()
            .find(|a| a.id == app_id)
            .and_then(|a| match &a.source {
                AppSource::Android { package } => Some(package.clone()),
                _ => None,
            });
        self.jobs.spawn(move || {
            // Android shows every app in Waydroid's one full-screen window
            // which takes seconds to appear on a cold start;
            // switching before it exists shows whatever else is on the app
            // desktop (Catfyn's player stood in for Play Store),
            // or an empty desktop when it never comes. CatosTV stays
            // in front until the window is there, up to ANDROID_WINDOW_WAIT;
            // without one it stays on Home and says so.
            let android = window.as_deref().is_some_and(is_android_window);
            if let Some(w) = window.as_deref().filter(|_| android)
                && !wait_for_window(w, ANDROID_WINDOW_WAIT)
            {
                return Msg::AndroidNoWindow(name);
            }
            // That one window still shows the app that was in front before
            // until Android has switched (Play Store flashed for a
            // second when SmartTube was opened from Recents, and the other
            // way round). Stay on CatosTV until Android reports the app in
            // front, up to ANDROID_TOP_WAIT; then switch anyway.
            if let Some(pkg) = package.as_deref().filter(|_| android) {
                wait_for_android_top(pkg, ANDROID_TOP_WAIT);
            }
            let switched = compositor::show_app_workspace(&mut SystemRunner, true);
            if let Some(w) = window {
                if android {
                    // Raised only if it is not in front already; never
                    // minimized or closed.
                    let _ = catostv_core::toplevel::raise_if_needed(&w);
                } else {
                    let _ = catostv_core::toplevel::activate(&w);
                }
            }
            match switched {
                Ok(()) => Msg::Nothing,
                Err(e) => Msg::Toast(e.to_string()),
            }
        });
    }

    /// Which compositor window belongs to app `id`: Android apps share
    /// Waydroid's window; a Flatpak's window carries its app id; others are
    /// learned when they first open a window (`Msg::WindowSeen`).
    /// Android is the foreground app (an Android app, or Android itself):
    /// `Some(package)` with the app to stop on Close, `Some(None)` when it
    /// is Android's own screen. `None` for every other app.
    fn foreground_android(&self) -> Option<Option<String>> {
        let f = self.foreground.as_deref()?;
        if f == android::SESSION_APP_ID {
            return Some(android::read_top().filter(|t| !android::is_android_home(t)));
        }
        match &self.apps.iter().find(|a| a.id == f)?.source {
            AppSource::Android { package } => Some(Some(package.clone())),
            _ => None,
        }
    }

    fn window_pattern(&self, id: &str) -> Option<String> {
        if id == android::SESSION_APP_ID {
            return Some("waydroid*".into());
        }
        if id == catfyn::PLAYER_ID {
            return Some(catfyn::PLAYER_ID.into());
        }
        let app = self.apps.iter().find(|a| a.id == id)?;
        match &app.source {
            // Its own window on the Android TV image; `show_app` falls back
            // to any Waydroid window when there is none by that name.
            AppSource::Android { package } => Some(android::window_of(package)),
            AppSource::Flatpak { app_id } => Some(
                self.windows
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| app_id.clone()),
            ),
            AppSource::Configured => self.windows.get(id).cloned(),
        }
    }

    /// After launching app `id`, note which new window appears (its
    /// Wayland app id), so Resume can bring exactly that window forward.
    fn learn_window(&self, id: String) {
        if !self.appliance {
            return;
        }
        self.jobs.spawn(move || {
            let before: HashSet<String> = catostv_core::toplevel::list()
                .map(|v| v.into_iter().map(|w| w.app_id).collect())
                .unwrap_or_default();
            for _ in 0..60 {
                std::thread::sleep(Duration::from_millis(500));
                if let Ok(now) = catostv_core::toplevel::list()
                    && let Some(w) = now
                        .into_iter()
                        .find(|w| !w.app_id.is_empty() && !before.contains(&w.app_id))
                {
                    return Msg::WindowSeen(id, w.app_id);
                }
            }
            Msg::Nothing
        });
    }

    fn launch_entry(&mut self, ctx: &egui::Context, app: &AppEntry) {
        if app.is_builtin() {
            return self.open_fin(ctx, None);
        }
        if let AppSource::Android { package } = &app.source {
            return self.android(ctx, AndroidTask::Launch(package.clone()));
        }
        let launched = self.mgr().launch(app);
        match launched {
            // No "Opening…" banner: the app simply appears.
            Ok(LaunchResult::Started) => {
                self.foreground = Some(app.id.clone());
                if !self.windows.contains_key(&app.id) {
                    self.learn_window(app.id.clone());
                }
            }
            // Tracked but not Running means it is still closing; never
            // call Resume here (Resume falls back to launch → recursion).
            Ok(LaunchResult::AlreadyRunning) if self.mgr().is_running(&app.id) => {
                self.foreground = Some(app.id.clone());
                self.show_app(&app.id);
            }
            Ok(LaunchResult::AlreadyRunning) => {
                self.show_toast(ctx, &format!("{} is still closing…", app.name))
            }
            Err(e) => self.show_toast(ctx, e.message()),
        }
    }

    fn launch(&mut self, ctx: &egui::Context, index: usize) {
        if let Some(app) = self.apps.get(index).cloned() {
            self.launch_entry(ctx, &app);
        }
    }

    /// Start the Waydroid session if it is not running. Returns false (and
    /// shows why) when Android cannot start.
    fn ensure_android_session(&mut self, ctx: &egui::Context, quiet: bool) -> bool {
        if !self.sys.android.initialized {
            if !quiet {
                self.show_toast(ctx, "Set up Android first (Settings → Android apps)");
            }
            return false;
        }
        let session = android::session_app_entry();
        if !self.mgr().is_running(&session.id) && !self.sys.android.session {
            let started = self.mgr().launch(&session);
            if let Err(e) = started {
                if !quiet {
                    self.show_toast(ctx, e.message());
                }
                return false;
            }
            if !quiet {
                self.show_toast(ctx, "Starting Android…");
            }
        }
        self.android_started = true;
        true
    }

    /// Run an Android task once Android has really booted (it answers
    /// `waydroid app list`), on a background job. CatosTV stays in front
    /// until the app has been launched (switching
    /// early showed whatever app was last on the app desktop).
    fn android(&mut self, ctx: &egui::Context, task: AndroidTask) {
        let quiet = matches!(task, AndroidTask::InstallAurora);
        // Android still running on a display a TV session restart left
        // behind: stop it, then do the task on a fresh session.
        if self.sys.android.session && android::session_is_stale(Path::new(android::SESSION_CONFIG))
        {
            eprintln!("catostv: Android is on a stale display; restarting it");
            if !quiet {
                self.show_toast(ctx, "Restarting Android…");
            }
            self.jobs.spawn(move || {
                let mut sleep = |d: Duration| std::thread::sleep(d);
                match android::stop_stale_session(&mut SystemRunner, &mut sleep) {
                    Ok(()) => Msg::AndroidRetry(task),
                    Err(e) => Msg::Toast(e.to_string()),
                }
            });
            return;
        }
        if !self.ensure_android_session(ctx, quiet) {
            return;
        }
        let home = self.home.clone();
        self.jobs.spawn(move || {
            let mut r = SystemRunner;
            let mut sleep = |d: Duration| std::thread::sleep(d);
            let apps = match android::wait_until_ready(&mut r, android::READY_TIMEOUT, &mut sleep) {
                Ok(apps) => apps,
                Err(e) => {
                    // Logged either way: a silent miss here is why Home had
                    // no Android icons.
                    eprintln!("catostv: Android task skipped: {e}");
                    return if quiet {
                        Msg::Nothing
                    } else {
                        Msg::Toast(e.to_string())
                    };
                }
            };
            // Every Android app gets a Home icon through CatosTV's own
            // desktop files.
            // Icons for apps installed since Android booted; icon
            // removal counts only answers from a booted Android.
            let made = android::ensure_icons(&mut r, &home, &apps);
            let tv_changed = android::sync_tv_desktops(&home, &apps, android::is_booted()) || made;
            if tv_changed {
                eprintln!("catostv: Android icons updated ({} apps)", apps.len());
            }
            let launch = |pkg: &str| -> Msg {
                // Already Android's front app (CatosTV Home was over it):
                // just show it. A fresh `am start` stacked a new splash on
                // SmartTube's running player, and its decoder deadlocked
                // in the restart.
                if android::read_top().as_deref() == Some(pkg) {
                    return Msg::AndroidOpened(pkg.to_string());
                }
                // Always through android-launch: it keeps Android in
                // full-screen mode (`waydroid app launch` asks for the
                // app's own window, which never opens on the Android TV 16
                // image), falls back to the phone launcher entry,
                // and records what was running before.
                match android::tv_launch_cmd(pkg).and_then(|c| SystemRunner.run_ok(&c).map(|_| ()))
                {
                    Ok(()) => Msg::AndroidOpened(pkg.to_string()),
                    Err(e) if e.to_string().contains("timed out") => Msg::Toast(
                        "Android is not responding. Settings → Android → Reset Android".into(),
                    ),
                    Err(e) => Msg::Toast(e.to_string()),
                }
            };
            match task {
                AndroidTask::Launch(pkg) => launch(&pkg),
                AndroidTask::InstallApk(path) => {
                    // Through Android's package manager as root
                    // (Waydroid's own install service is broken on the TV
                    // 16 image).
                    let mut sleep = |d: Duration| std::thread::sleep(d);
                    let installed =
                        android::install_apk_as_root(&mut r, &home, Path::new(&path), &mut sleep);
                    if let Ok(apps) = android::list_apps(&mut r) {
                        android::ensure_icons(&mut r, &home, &apps);
                        android::sync_tv_desktops(&home, &apps, android::is_booted());
                    }
                    match installed {
                        Ok(Some(app)) => Msg::ApkDone(format!("{} installed", app.name)),
                        Ok(None) => Msg::ApkDone(
                            "Android added no new app. Already installed, or it refused this APK"
                                .into(),
                        ),
                        Err(e) => Msg::ApkDone(e.to_string()),
                    }
                }
                AndroidTask::OpenAurora => match android::install_aurora_if_missing(&mut r) {
                    Ok(_) => launch(android::AURORA_PACKAGE),
                    Err(e) => Msg::Toast(e.to_string()),
                },
                // Installing it makes Waydroid write its .desktop file, so
                // the next refresh shows Aurora Store on Home.
                AndroidTask::InstallAurora => {
                    // Android just booted: size its layout for the TV first.
                    let _ = SystemRunner.run(&android::density_cmd());
                    match android::install_aurora_if_missing(&mut r) {
                        Ok(_) => Msg::Refresh,
                        Err(_) if tv_changed => Msg::Refresh,
                        Err(_) => Msg::Nothing,
                    }
                }
            }
        });
    }

    /// Once per shell start: when Android is set up, boot it in the
    /// background so Android apps open instantly, and make sure Aurora
    /// Store is installed (it then appears on Home).
    fn autostart_android(&mut self, ctx: &egui::Context) {
        if self.android_started
            || !self.sys.android.initialized
            || self.sys.android_setup.as_ref().is_some_and(|s| s.running())
        {
            return;
        }
        self.android(ctx, AndroidTask::InstallAurora);
        self.android_started = true;
    }

    fn save_prefs(&mut self, ctx: &egui::Context) {
        if let Err(e) = prefs::save(&self.prefs) {
            self.show_toast(ctx, &format!("Could not save: {e}"));
        }
    }

    fn save_display(&mut self, ctx: &egui::Context) {
        if let Err(e) = display::save_prefs(&self.display) {
            self.show_toast(ctx, &format!("Could not save display settings: {e}"));
        }
    }

    /// Spawn a job whose result is "refresh" on success, a toast on error.
    fn job(&self, f: impl FnOnce(&mut SystemRunner) -> catostv_core::Result<()> + Send + 'static) {
        self.jobs.spawn(move || match f(&mut SystemRunner) {
            Ok(()) => Msg::Refresh,
            Err(e) => Msg::Toast(e.to_string()),
        });
    }

    fn apply(&mut self, ctx: &egui::Context, effect: Effect) {
        if let Effect::Launch(i) | Effect::Resume(i) = &effect
            && let Some(app) = self.apps.get(*i)
        {
            self.launching = Some((app.id.clone(), Instant::now()));
        }
        match effect {
            Effect::Launch(i) => self.launch(ctx, i),
            Effect::Resume(i) => {
                let Some(app) = self.apps.get(i).cloned() else {
                    return;
                };
                // Launching an Android app that runs brings it to the front.
                if let AppSource::Android { package } = &app.source {
                    return self.android(ctx, AndroidTask::Launch(package.clone()));
                }
                // Catfyn's tile while a video plays: back to the video.
                if app.is_builtin() {
                    if self.mgr().is_running(catfyn::PLAYER_ID) {
                        self.foreground = Some(catfyn::PLAYER_ID.into());
                        self.show_app(catfyn::PLAYER_ID);
                    } else {
                        self.open_fin(ctx, None);
                    }
                    return;
                }
                if !self.mgr().is_running(&app.id) {
                    return self.launch(ctx, i);
                }
                self.foreground = Some(app.id.clone());
                if self.appliance {
                    self.show_app(&app.id);
                } else {
                    self.show_toast(ctx, &format!("{} is in its own window", app.name));
                }
            }
            Effect::Close(i) => {
                let Some(app) = self.apps.get(i).cloned() else {
                    return;
                };
                if app.is_builtin() {
                    self.mgr().close(catfyn::PLAYER_ID);
                    return;
                }
                if let AppSource::Android { package } = &app.source {
                    self.android_running.retain(|p| p != package);
                    let package = package.clone();
                    self.job(move |r| {
                        android::stop_cmd(&package).and_then(|c| r.run_ok(&c).map(|_| ()))
                    });
                    return;
                }
                self.mgr().close(&app.id);
            }
            Effect::Restart(i) => {
                let Some(app) = self.apps.get(i).cloned() else {
                    return;
                };
                if self.mgr().close(&app.id) {
                    self.pending_restart.insert(app.id.clone());
                } else {
                    self.launch(ctx, i);
                }
            }
            Effect::Uninstall(i) => {
                let Some(app) = self.apps.get(i).cloned() else {
                    return;
                };
                match app.source {
                    AppSource::Flatpak { app_id } => {
                        self.shell.on_store_job(&app_id, Some(StoreJob::Removing));
                        self.store_job(app_id, true);
                    }
                    AppSource::Android { package } => {
                        self.show_toast(ctx, &format!("Removing {}…", app.name));
                        let home = self.home.clone();
                        self.jobs.spawn(move || {
                            match android::remove_app(&mut SystemRunner, &package) {
                                Ok(()) => {
                                    android::forget_app_desktops(&home, &package);
                                    Msg::AndroidRemoved(package, app.name)
                                }
                                Err(e) => Msg::Toast(e.to_string()),
                            }
                        });
                    }
                    AppSource::Configured => {}
                }
            }
            Effect::SetWifi(on) => self.job(move |r| net::set_wifi(r, on)),
            Effect::SetBluetooth(on) => self.job(move |r| bluetooth::set_power(r, on)),
            Effect::VolumeStep(delta) => self.jobs.spawn(move || {
                Msg::Volume(audio::step(&mut SystemRunner, delta).map_err(|e| e.to_string()))
            }),
            Effect::ToggleMute => self.jobs.spawn(|| {
                Msg::Volume(audio::toggle_mute(&mut SystemRunner).map_err(|e| e.to_string()))
            }),
            Effect::Power(action) => {
                if action == power::PowerAction::Suspend {
                    if self
                        .wake
                        .sleep_blocked(self.started.elapsed().as_secs_f64())
                    {
                        eprintln!("catostv: just woke up; not going back to sleep");
                        return;
                    }
                    // Wake up on Home, not on Quick settings' "Sleep".
                    self.shell.go_home();
                    self.mapper.reset();
                }
                let appliance = self.appliance;
                self.job(move |r| power::perform(r, action, appliance));
            }
            Effect::RestartShell => {
                // Apps started by this shell would become untracked, so close
                // them; the session's autostart loop restarts the shell.
                self.mgr().close_all();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Effect::CheckUpdates => {
                if !self.sys.updates.checking {
                    self.sys.updates.checking = true;
                    self.jobs.spawn(|| {
                        Msg::UpdateCheck(catostv_core::updates::check(&mut SystemRunner))
                    });
                }
            }
            Effect::InstallUpdate => {
                // Never during playback: any app counts (Catfyn's player
                // included), but not Android's background session, which
                // runs whenever Android is set up.
                let busy = self
                    .mgr()
                    .running_ids()
                    .iter()
                    .any(|id| id != android::SESSION_APP_ID);
                if busy {
                    self.show_toast(ctx, "Close what's playing first, then install the update");
                } else {
                    self.sys.updates.install = Some(catostv_core::updates::InstallState::Running(
                        "Starting the update".into(),
                    ));
                    self.job(|r| r.run_ok(&catostv_core::updates::install_cmd()).map(|_| ()));
                }
            }
            Effect::SetEthernetIp { connection, config } => {
                self.job(move |r| net::set_ethernet_ip(r, &connection, &config))
            }
            Effect::DebugSsh(on) => self.job(move |r| {
                use catostv_core::debug;
                if on {
                    debug::run_unit(r, debug::SSH_ON_UNIT, "Turning Debug SSH on")
                } else {
                    debug::run_unit(r, debug::SSH_OFF_UNIT, "Turning Debug SSH off")
                }
            }),
            Effect::DebugPassword(pw) => self.jobs.spawn(move || {
                use catostv_core::debug;
                let staged = debug::stage_password(Path::new(debug::PASSWORD_STAGE), &pw)
                    .map_err(|e| {
                        catostv_core::Error::msg(format!("Could not set the password: {e}"))
                    })
                    .and_then(|()| {
                        debug::run_unit(
                            &mut SystemRunner,
                            debug::PASSWORD_UNIT,
                            "Setting the password",
                        )
                    });
                let _ = std::fs::remove_file(debug::PASSWORD_STAGE);
                match staged {
                    Ok(()) => Msg::Toast("Password changed".into()),
                    Err(e) => Msg::Toast(e.to_string()),
                }
            }),
            Effect::DebugRemoveOld => self.job(|r| {
                use catostv_core::debug;
                debug::run_unit(r, debug::REMOVE_OK_UNIT, "Removing the old login")
            }),
            Effect::ScanWifi => self
                .jobs
                .spawn(|| Msg::WifiScan(net::scan(&mut SystemRunner).map_err(|e| e.to_string()))),
            Effect::ConnectWifi { ssid, password } => self.jobs.spawn(move || {
                let r = net::connect(&mut SystemRunner, &ssid, password.as_deref())
                    .map_err(|e| e.to_string());
                Msg::WifiConnect(ssid, r)
            }),
            Effect::ReloadApps => {
                self.reload_apps(ctx);
                self.show_toast(ctx, "App list reloaded");
            }
            Effect::LeaveApp => {
                // Escape gesture from catostv-inputd. Only act when an app is
                // in front: inside CatosTV, Back already navigates.
                if !keys::window_focused(ctx) {
                    if self.foreground_android().is_some() {
                        self.send_android_home();
                    }
                    self.pause_catfyn_player();
                    self.shell.go_home();
                    self.return_to_shell();
                }
            }
            Effect::BtScan => self.jobs.spawn(|| {
                Msg::BtScan(bluetooth::scan(&mut SystemRunner, 12).map_err(|e| e.to_string()))
            }),
            Effect::BtPair(mac) => self.jobs.spawn(move || {
                let mut r = SystemRunner;
                let res = bluetooth::pair_trust_connect(&mut r, &mac)
                    .map(|_| {
                        bluetooth::info(&mut r, &mac)
                            .map(|i| i.name)
                            .unwrap_or_else(|_| mac.clone())
                    })
                    .map_err(|e| e.to_string());
                Msg::BtPaired(mac, res)
            }),
            Effect::BtConnect(mac) => self.job(move |r| bluetooth::connect(r, &mac)),
            Effect::BtDisconnect(mac) => self.job(move |r| bluetooth::disconnect(r, &mac)),
            Effect::BtForget(mac) => self.job(move |r| bluetooth::remove(r, &mac)),
            Effect::SetMode { output, mode } => {
                self.confirm_deadline = Some(
                    Instant::now() + Duration::from_secs(catostv_core::shell::DISPLAY_CONFIRM_SECS),
                );
                self.jobs.spawn(move || {
                    match display::set_mode(&mut SystemRunner, &output, &mode) {
                        Ok(()) => Msg::Refresh,
                        Err(e) => Msg::ModeFailed(e.to_string()),
                    }
                });
            }
            Effect::RevertMode { output, mode } => {
                self.confirm_deadline = None;
                self.job(move |r| match mode {
                    Some(m) => display::set_mode(r, &output, &m),
                    None => display::set_preferred(r, &output),
                });
            }
            Effect::KeepMode { output, mode } => {
                self.confirm_deadline = None;
                self.display.output = output;
                self.display.mode = Some(mode.key());
                self.save_display(ctx);
            }
            Effect::SetScale { output, percent } => {
                // Saved (and shown) at once; applied when the presses stop,
                // so stepping 100 → 200 % reconfigures the TV once, not four
                // times ("glitchy").
                let scale = percent as f32 / 100.0;
                self.display.output = output.clone();
                self.display.scale = Some(scale);
                self.save_display(ctx);
                self.pending_scale = Some((output, scale, Instant::now() + SCALE_SETTLE));
            }
            Effect::SetSafeArea(p) => {
                self.display.safe_area_pct = p;
                self.save_display(ctx);
            }
            Effect::SetAdaptiveSync { output, on } => {
                self.job(move |r| display::set_adaptive_sync(r, &output, on))
            }
            Effect::SetSink(id) => {
                // Shown at once; the switch itself is quick, the next full
                // refresh is not.
                self.sys
                    .sinks
                    .iter_mut()
                    .for_each(|x| x.is_default = x.id == id);
                self.job(move |r| audio::set_default_sink(r, id));
            }
            Effect::ApplyHdmiProfile => {
                if let Some(h) = self.sys.hdmi_hint.clone() {
                    self.job(move |r| audio::set_card_profile(r, &h.card, &h.profile));
                }
            }
            Effect::SetAccent(name) => {
                theme::set_accent(&name);
                self.prefs.accent = name;
                self.save_prefs(ctx);
            }
            Effect::SetClock24(on) => {
                self.prefs.clock_24h = on;
                self.save_prefs(ctx);
            }
            Effect::SetGlow(on) => {
                self.prefs.glow = on;
                self.save_prefs(ctx);
            }
            Effect::SetFinAutoplay(on) => {
                self.prefs.fin_autoplay = on;
                self.save_prefs(ctx);
            }
            Effect::SetFinCountdown(secs) => {
                self.prefs.fin_countdown = secs;
                self.save_prefs(ctx);
            }
            Effect::SetPadNav(on) => {
                self.prefs.pad_nav = on;
                self.save_prefs(ctx);
            }
            Effect::SetPadLayout { device, nintendo } => {
                self.prefs.set_nintendo_for(&device, nintendo);
                self.save_prefs(ctx);
            }
            Effect::SearchMedia(q) => {
                if let Some(a) = self.media.accounts.home().cloned() {
                    let device = self.media.accounts.device_id.clone();
                    self.jobs.spawn(move || {
                        // A short pause: the next letter usually follows.
                        std::thread::sleep(Duration::from_millis(250));
                        let items = catfyn::items(&device, &a, &jf::search_url(&a, &q, 8))
                            .unwrap_or_default();
                        Msg::SearchMedia(q, items)
                    });
                }
            }
            Effect::SetAndroidVariant(v) => {
                self.prefs.android_variant = v;
                self.save_prefs(ctx);
            }
            Effect::PadCapture(on) => {
                let flag = catostv_core::gamepad::capture_flag();
                let _ = if on {
                    std::fs::write(&flag, b"1")
                } else {
                    std::fs::remove_file(&flag).or_else(|e| {
                        if e.kind() == std::io::ErrorKind::NotFound {
                            Ok(())
                        } else {
                            Err(e)
                        }
                    })
                };
            }
            Effect::SetPadBinding(action, code) => {
                self.prefs.set_pad_binding(action.key(), code);
                self.save_prefs(ctx);
                self.show_toast(
                    ctx,
                    &format!(
                        "{} is now {}",
                        catostv_core::gamepad::button_name(code),
                        action.label()
                    ),
                );
            }
            Effect::ClearPadBindings => {
                self.prefs.clear_pad_bindings();
                self.save_prefs(ctx);
            }
            Effect::SetHdr(on) => {
                self.prefs.hdr = on;
                self.save_prefs(ctx);
                let flag = prefs::hdr_flag_path();
                // "pending" until confirmed after the restart (KeepHdr).
                let written = if on {
                    std::fs::write(&flag, "pending\n")
                } else {
                    std::fs::remove_file(&flag).or_else(|e| {
                        if e.kind() == std::io::ErrorKind::NotFound {
                            Ok(())
                        } else {
                            Err(e)
                        }
                    })
                };
                if let Err(e) = written {
                    self.show_toast(ctx, &format!("Could not switch HDR: {e}"));
                    return;
                }
                if self.appliance {
                    // catostv-session reads the flag when it (re)starts labwc and
                    // falls back to SDR by itself if labwc then fails.
                    self.show_toast(
                        ctx,
                        if on {
                            "Turning HDR on…"
                        } else {
                            "Turning HDR off…"
                        },
                    );
                    self.mgr().close_all();
                    self.job(|r| r.run_ok(&Cmd::new("labwc").arg("--exit")).map(|_| ()));
                }
            }
            Effect::KeepHdr => {
                self.confirm_deadline = None;
                if let Err(e) = std::fs::write(prefs::hdr_flag_path(), "1\n") {
                    self.show_toast(ctx, &format!("Could not keep HDR: {e}"));
                }
            }
            Effect::StoreSearch(q) => self.jobs.spawn(move || {
                let mut r = SystemRunner;
                let res = flatpak::ensure_flathub_user(&mut r)
                    .and_then(|_| flatpak::search(&mut r, &q))
                    .map(|v| {
                        v.into_iter()
                            .filter(|a| !flatpak::is_addon_id(&a.id))
                            .map(|a| StoreItem {
                                id: a.id,
                                name: a.name,
                                blurb: a.description,
                            })
                            .collect()
                    })
                    .map_err(|e| e.to_string());
                Msg::StoreResults(res)
            }),
            Effect::StoreDetails(id) => {
                let home = self.home.clone();
                self.jobs.spawn(move || {
                    let res = flatpak::read_appstream(&mut SystemRunner, &home)
                        .map_err(|e| e.to_string())
                        .and_then(|xml| {
                            flatpak::parse_app_details(&xml, &id)
                                .ok_or_else(|| "No details for this app yet".to_string())
                        });
                    Msg::StoreDetails(id, res)
                });
            }
            Effect::StoreInstall(id) => self.store_job(id, false),
            Effect::StoreUninstall(id) => self.store_job(id, true),
            Effect::StoreOpen(id) => {
                let entry = self
                    .apps
                    .iter()
                    .find(|a| matches!(&a.source, AppSource::Flatpak { app_id } if *app_id == id))
                    .cloned();
                match entry {
                    Some(e) => self.launch_entry(ctx, &e),
                    None => self.show_toast(ctx, "The app is not ready yet. Try again"),
                }
            }
            Effect::UpdateApps => {
                self.show_toast(ctx, "Updating store apps…");
                self.jobs
                    .spawn(|| match flatpak::update_all(&mut SystemRunner) {
                        Ok(()) => Msg::Toast("Store apps are up to date".into()),
                        Err(e) => Msg::Toast(e.to_string()),
                    });
            }
            Effect::AndroidSetup => {
                self.show_toast(ctx, "Setting up Android…");
                self.job(|r| android::start_setup(r));
            }
            Effect::BrowseApks(dir) => {
                let home = self.home.clone();
                self.jobs.spawn(move || {
                    Msg::Apks(Ok(match dir {
                        Some(d) => android::list_apk_dir(Path::new(&d)),
                        None => {
                            android::mount_removable(&mut SystemRunner);
                            android::apk_roots(&home)
                        }
                    }))
                })
            }
            Effect::InstallApk(path) => {
                let file = path.rsplit('/').next().unwrap_or(&path).to_string();
                self.show_toast(ctx, &format!("Installing {file}…"));
                self.android(ctx, AndroidTask::InstallApk(path));
            }
            Effect::AndroidOpenStore => self.android(ctx, AndroidTask::OpenAurora),
            Effect::AndroidRepair => {
                self.sys.android_repairing = true;
                self.android_running.clear();
                self.android_started = false;
                self.job(|r| r.run_ok(&android::repair_cmd()).map(|_| ()));
            }
            Effect::KdeAccept(id) => self.job(move |r| kdeconnect::accept_pairing(r, &id)),
            Effect::KdeReject(id) => self.job(move |r| kdeconnect::reject_pairing(r, &id)),
            Effect::KdePing(id) => self.job(move |r| kdeconnect::ping(r, &id)),
            Effect::KdeUnpair(id) => self.job(move |r| kdeconnect::unpair(r, &id)),
            Effect::AppWindow(action) => {
                // Android's one full-screen window is never closed or
                // resized from CatosTV: once the compositor closes it, the
                // display HAL ignores that Android task until it ends, and
                // every sized configure is a display re-plug.
                // Close stops the app inside Android instead.
                if let Some(package) = self.foreground_android() {
                    if action == catostv_core::toplevel::WindowAction::Close {
                        if let Some(package) = package {
                            self.android_running.retain(|p| p != &package);
                            self.job(move |r| {
                                android::stop_cmd(&package).and_then(|c| r.run_ok(&c).map(|_| ()))
                            });
                        }
                        self.shell.go_home();
                    }
                    return;
                }
                let Some(window) = self
                    .foreground
                    .as_deref()
                    .and_then(|f| self.window_pattern(f))
                else {
                    return;
                };
                self.jobs.spawn(move || {
                    use catostv_core::toplevel::WindowAction;
                    match catostv_core::toplevel::control(&window, action) {
                        Ok(true) => match action {
                            WindowAction::Fullscreen(true) => Msg::Toast(
                                "Full screen. The Control Center key brings these controls back"
                                    .into(),
                            ),
                            WindowAction::Close => Msg::Nothing,
                            _ => Msg::Nothing,
                        },
                        Ok(false) => Msg::Toast("The app's window is gone".into()),
                        Err(e) => Msg::Toast(e.to_string()),
                    }
                });
                if action == catostv_core::toplevel::WindowAction::Close {
                    self.shell.go_home();
                }
            }
            Effect::OpenFiles => {
                let home = self.home.clone();
                let (f, effects) =
                    catostv_core::files::Files::open(catostv_core::files::places(&home));
                self.shell.files = Some(f);
                for e in effects {
                    self.files_effect(ctx, e);
                }
                // Drives: udisks mounts what is plugged in, then the places
                // are refreshed.
                self.jobs.spawn(move || {
                    android::mount_removable(&mut SystemRunner);
                    Msg::FilesListed(String::new(), Ok(catostv_core::files::places(&home)))
                });
            }
            Effect::Files(e) => self.files_effect(ctx, e),
            Effect::OpenFin => self.open_fin(ctx, None),
            Effect::FinDetails(item) => {
                // Home's rows belong to the home account: open Catfyn on it.
                if let Some(i) = self.media.accounts.home().and_then(|h| {
                    self.media
                        .accounts
                        .list
                        .iter()
                        .position(|a| a.user_id == h.user_id && a.server == h.server)
                }) && i != self.media.accounts.active
                {
                    self.media.accounts.active = i;
                    self.save_accounts(ctx);
                }
                self.open_fin(ctx, Some(item))
            }
            Effect::FinPlayHome(item) => match self.media.accounts.home().cloned() {
                Some(a) => self.fin_play(ctx, a, item, false),
                None => self.open_fin(ctx, None),
            },
            Effect::Fin(e) => self.fin_effect(ctx, e),
            Effect::ReturnToApp => match self.foreground.clone() {
                Some(id) => self.show_app(&id),
                None => self.shell.go_home(),
            },
            Effect::KdePair(id) => {
                self.show_toast(ctx, "Accept the request on your phone");
                self.job(move |r| kdeconnect::pair(r, &id));
            }
            Effect::KeyGuideSeen => {
                if !self.prefs.key_guide_seen {
                    self.prefs.key_guide_seen = true;
                    let _ = prefs::save(&self.prefs);
                }
            }
            Effect::Toast(t) => self.show_toast(ctx, &t),
        }
    }

    // ------------------------------------------------------------ Catfyn

    fn files_effect(&mut self, ctx: &egui::Context, e: catostv_core::files::FilesEffect) {
        use catostv_core::files::{self, FilesEffect};
        match e {
            FilesEffect::List(dir) => self.jobs.spawn(move || {
                let p = std::path::Path::new(&dir);
                let r = if p.is_dir() {
                    Ok(files::list_dir(p))
                } else {
                    Err("This folder is gone".to_string())
                };
                Msg::FilesListed(dir, r)
            }),
            FilesEffect::Play(entry) => {
                const PLAYER_ID: &str = "catostv-files-player";
                if self.mgr().is_running(PLAYER_ID) {
                    self.mgr().close(PLAYER_ID);
                    std::thread::sleep(Duration::from_millis(300));
                }
                let mut argv: Vec<String> = [
                    "mpv",
                    "--fullscreen",
                    "--force-window=immediate",
                    "--idle=no",
                    "--keep-open=no",
                    "--no-terminal",
                    "--ytdl=no",
                    "--image-display-duration=inf",
                    "--wayland-app-id=catostv-files-player",
                    "--input-conf=~/.config/catostv/catfyn-input.conf",
                    "--script=~/.config/catostv/catfyn-osd.lua",
                    "--osc=no",
                    "--osd-duration=1500",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect();
                let (r, g, b) = catostv::theme::accent_rgb();
                argv.push(format!(
                    "--script-opts=catfyn_osd-accent={r:02x}{g:02x}{b:02x}"
                ));
                argv.push(format!("--force-media-title={}", entry.name));
                argv.push("--".into());
                argv.push(entry.path.clone());
                let app = AppEntry {
                    id: PLAYER_ID.into(),
                    name: entry.name.clone(),
                    subtitle: String::new(),
                    icon: "folder".into(),
                    accent: "orange".into(),
                    command: catostv_core::desktop::join_command(&argv),
                    singleton: true,
                    icon_path: None,
                    source: AppSource::Configured,
                };
                let launched = self.mgr().launch(&app);
                match launched {
                    Ok(_) => {
                        self.foreground = Some(PLAYER_ID.into());
                        self.windows.insert(PLAYER_ID.into(), PLAYER_ID.into());
                    }
                    Err(e) => self.show_toast(ctx, e.message()),
                }
            }
            FilesEffect::InstallApk(path) => {
                let file = path.rsplit('/').next().unwrap_or(&path).to_string();
                self.show_toast(ctx, &format!("Installing {file}…"));
                self.android(ctx, AndroidTask::InstallApk(path));
            }
            FilesEffect::Delete(path) => {
                let p = std::path::PathBuf::from(&path);
                if !files::writable(&self.home, &p) {
                    return self.show_toast(ctx, "This cannot be deleted here");
                }
                self.files_job(ctx, move || {
                    let meta = std::fs::symlink_metadata(&p)?;
                    if meta.is_dir() {
                        std::fs::remove_dir_all(&p)
                    } else {
                        std::fs::remove_file(&p)
                    }
                });
            }
            FilesEffect::Rename { path, new_name } => {
                let p = std::path::PathBuf::from(&path);
                if !files::writable(&self.home, &p) || !files::valid_name(&new_name) {
                    return self.show_toast(ctx, "This cannot be renamed here");
                }
                let to = p.with_file_name(new_name.trim());
                self.files_job(ctx, move || {
                    if to.exists() {
                        return Err(std::io::Error::other("A file with that name exists"));
                    }
                    std::fs::rename(&p, &to)
                });
            }
            FilesEffect::NewFolder { dir, name } => {
                let d = std::path::PathBuf::from(&dir);
                if !(files::writable(&self.home, &d)
                    || d == self.home
                    || d.starts_with("/run/media")
                    || d.starts_with("/media")
                    || d.starts_with("/mnt"))
                    || !files::valid_name(&name)
                {
                    return self.show_toast(ctx, "A folder cannot be made here");
                }
                let p = d.join(name.trim());
                self.files_job(ctx, move || std::fs::create_dir(&p));
            }
            FilesEffect::Paste { source, dir, cut } => {
                let src = std::path::PathBuf::from(&source);
                let d = std::path::PathBuf::from(&dir);
                let Some(name) = src.file_name().map(|n| n.to_os_string()) else {
                    return;
                };
                let dest = d.join(&name);
                if dest == src || dest.starts_with(&src) {
                    return self.show_toast(ctx, "Already here");
                }
                if cut && !files::writable(&self.home, &src) {
                    return self.show_toast(ctx, "This cannot be moved from here");
                }
                let verb = if cut { "Moving" } else { "Copying" };
                self.show_toast(ctx, &format!("{verb} {}…", name.to_string_lossy()));
                self.files_job(ctx, move || {
                    if dest.exists() {
                        return Err(std::io::Error::other("A file with that name exists"));
                    }
                    if cut && std::fs::rename(&src, &dest).is_ok() {
                        return Ok(());
                    }
                    copy_tree(&src, &dest)?;
                    if cut {
                        if src.is_dir() {
                            std::fs::remove_dir_all(&src)?;
                        } else {
                            std::fs::remove_file(&src)?;
                        }
                    }
                    Ok(())
                });
            }
            FilesEffect::CannotOpen(entry) => self.show_toast(
                ctx,
                &format!(
                    "Nothing on the TV opens {} files",
                    entry.kind.label().to_lowercase()
                ),
            ),
        }
    }

    /// Run a file operation off the UI, then list the folder shown again
    /// (or show the error).
    fn files_job(
        &mut self,
        ctx: &egui::Context,
        op: impl FnOnce() -> std::io::Result<()> + Send + 'static,
    ) {
        let _ = ctx;
        let Some(dir) = self
            .shell
            .files
            .as_ref()
            .and_then(|f| f.current_dir())
            .map(|d| d.path.clone())
        else {
            return;
        };
        self.jobs.spawn(move || {
            let r = op().map_err(|e| e.to_string());
            match r {
                Ok(()) => {
                    let listing = catostv_core::files::list_dir(std::path::Path::new(&dir));
                    Msg::FilesListed(dir, Ok(listing))
                }
                Err(e) => Msg::FilesFailed(dir, e),
            }
        });
    }

    fn open_fin(&mut self, ctx: &egui::Context, details: Option<Item>) {
        let (mut f, effects) = Fin::open(&self.media.accounts);
        if let Some(item) = details
            && self.media.accounts.active().is_some()
        {
            f.stack.push(fin::Screen::Details { item, index: 0 });
        }
        self.shell.fin = Some(f);
        for e in effects {
            self.fin_effect(ctx, e);
        }
    }

    fn save_accounts(&mut self, ctx: &egui::Context) {
        if let Err(e) = jf::save_accounts(&self.home, &self.media.accounts) {
            self.show_toast(ctx, &format!("Could not save the account: {e}"));
        }
    }

    fn fin_effect(&mut self, ctx: &egui::Context, e: FinEffect) {
        let device = self.media.accounts.device_id.clone();
        let active = self.media.accounts.active().cloned();
        match e {
            FinEffect::Discover => self
                .jobs
                .spawn(|| Msg::FinServers(jf::discover(Duration::from_millis(2500)))),
            FinEffect::Login {
                server,
                user,
                password,
            } => self
                .jobs
                .spawn(move || Msg::FinLogin(catfyn::login(&device, &server, &user, &password))),
            FinEffect::LoadHome => {
                if let Some(a) = active {
                    let id = a.user_id.clone();
                    self.jobs
                        .spawn(move || Msg::FinHome(id, catfyn::home(&device, &a)));
                }
            }
            FinEffect::LoadLibrary(lib) => {
                if let Some(a) = active {
                    self.jobs.spawn(move || {
                        let r = catfyn::items(&device, &a, &jf::library_url(&a, &lib));
                        Msg::FinLibrary(lib, r)
                    });
                }
            }
            FinEffect::LoadSeasons(series) => {
                if let Some(a) = active {
                    self.jobs.spawn(move || {
                        let r = catfyn::items(&device, &a, &jf::seasons_url(&a, &series));
                        Msg::FinSeasons(series, r)
                    });
                }
            }
            FinEffect::LoadEpisodes { series, season } => {
                if let Some(a) = active {
                    self.jobs.spawn(move || {
                        let r = catfyn::items(&device, &a, &jf::episodes_url(&a, &series, &season));
                        Msg::FinEpisodes(season, r)
                    });
                }
            }
            FinEffect::Play { item, from_start } => {
                if let Some(a) = active {
                    self.fin_play(ctx, a, item, from_start);
                }
            }
            FinEffect::SwitchAccount(i) => {
                if i < self.media.accounts.list.len() {
                    self.media.accounts.active = i;
                    self.save_accounts(ctx);
                    self.last_media_refresh = None;
                }
            }
            FinEffect::SetIncognito(i, on) => {
                if let Some(a) = self.media.accounts.list.get_mut(i) {
                    a.incognito = on;
                    self.save_accounts(ctx);
                    self.last_media_refresh = None;
                    self.show_toast(
                        ctx,
                        if on {
                            "Incognito: kept off Home"
                        } else {
                            "Shown on Home again"
                        },
                    );
                }
            }
            FinEffect::SignOut(i) => {
                self.media.accounts.remove(i);
                self.save_accounts(ctx);
                self.last_media_refresh = None;
                self.media.home.clear();
            }
            FinEffect::Toast(t) => self.show_toast(ctx, &t),
            FinEffect::ToggleAutoplay => {
                let on = !self.prefs.fin_autoplay;
                self.apply(ctx, Effect::SetFinAutoplay(on));
            }
            FinEffect::CountdownStep(d) => {
                let secs = catostv_core::prefs::cycle_countdown(self.prefs.fin_countdown, d as i32);
                if secs != self.prefs.fin_countdown {
                    self.apply(ctx, Effect::SetFinCountdown(secs));
                }
            }
            FinEffect::Close => self.shell.fin = None,
        }
    }

    /// Play `item` in mpv (full screen, resumes where it was left unless
    /// `from_start`), and report progress to the server while it plays.
    fn fin_play(&mut self, ctx: &egui::Context, a: jf::Account, item: Item, from_start: bool) {
        let device = self.media.accounts.device_id.clone();
        if self.mgr().is_running(catfyn::PLAYER_ID) {
            self.mgr().close(catfyn::PLAYER_ID);
            std::thread::sleep(Duration::from_millis(300));
        }
        let entry = match catfyn::player_entry(&device, &a, &item, from_start) {
            Ok(e) => e,
            Err(e) => return self.show_toast(ctx, &e),
        };
        let launched = self.mgr().launch(&entry);
        match launched {
            Ok(_) => {
                self.foreground = Some(catfyn::PLAYER_ID.into());
                self.windows
                    .insert(catfyn::PLAYER_ID.into(), catfyn::PLAYER_ID.into());
                let (tx, rctx) = self.jobs.sender();
                let autoplay = catfyn::Autoplay {
                    enabled: self.prefs.fin_autoplay,
                    countdown: self.prefs.fin_countdown,
                };
                catfyn::spawn_reporter(
                    device,
                    a.clone(),
                    item.clone(),
                    self.home.clone(),
                    autoplay,
                    move |next| {
                        let _ = tx.send(Msg::FinPlaybackDone(next.map(|n| Box::new((a, n)))));
                        rctx.request_repaint();
                    },
                );
            }
            Err(e) => self.show_toast(ctx, e.message()),
        }
    }

    /// Refresh CatosTV Home's "Continue watching" row now and then.
    fn refresh_home_media(&mut self) {
        let due = self
            .last_media_refresh
            .is_none_or(|t| t.elapsed() > Duration::from_secs(120));
        if !due {
            return;
        }
        self.last_media_refresh = Some(Instant::now());
        let device = self.media.accounts.device_id.clone();
        match self.media.accounts.home().cloned() {
            Some(a) => self
                .jobs
                .spawn(move || Msg::FinHomeMedia(catfyn::home_rows(&device, &a))),
            None => self.media.home.clear(),
        }
    }

    /// Download the pictures the last frame asked for (a few at a time).
    fn fetch_fin_images(&mut self) {
        let wanted = std::mem::take(&mut *self.fin_wanted.borrow_mut());
        for url in wanted {
            if self.fin_images.contains_key(&url)
                || self.fin_fetching.contains(&url)
                || self.fin_fetching.len() >= 6
            {
                continue;
            }
            self.fin_fetching.insert(url.clone());
            let home = self.home.clone();
            self.jobs.spawn(move || {
                let path = catfyn::fetch_image(&home, &url);
                Msg::FinImage(url, path)
            });
        }
    }

    fn store_job(&mut self, id: String, remove: bool) {
        let (tx, ctx) = self.jobs.sender();
        let started = std::thread::Builder::new()
            .name("catostv-store".into())
            .spawn(move || run_store_job(tx, ctx, id, remove));
        if let Err(e) = started {
            eprintln!("catostv-shell: store job: {e}");
        }
    }

    fn handle_message(&mut self, ctx: &egui::Context, msg: Msg) {
        match msg {
            // Controller navigation is queued for the next frame (it needs
            // the shell context). A press older than a moment was made
            // while an app was in front (the shell's loop was paused):
            // drop it rather than replay it into CatosTV later.
            Msg::Ipc(cmd, at) if cmd.nav_action().is_some() => {
                if at.elapsed() < Duration::from_millis(400)
                    && let Some(a) = cmd.nav_action()
                {
                    self.pending_nav.push(a);
                    ctx.request_repaint();
                }
            }
            Msg::Ipc(ShellCommand::PadCaptured, _) => {
                if let Some(code) = catostv_core::gamepad::read_captured() {
                    for e in self.shell.on_pad_captured(code) {
                        self.apply(ctx, e);
                    }
                }
            }
            Msg::Ipc(cmd, _) => {
                self.mapper.reset();
                // Home (key, remote, controller Guide) from an Android app:
                // tell Android too, or the app keeps playing unseen.
                if cmd == ShellCommand::Home && self.foreground_android().is_some() {
                    self.send_android_home();
                }
                // …and Catfyn's player pauses, so the film waits for you.
                if cmd == ShellCommand::Home {
                    self.pause_catfyn_player();
                }
                for e in self.shell.handle_ipc(cmd) {
                    self.apply(ctx, e);
                }
            }
            Msg::System(s, discovered) => self.on_snapshot(ctx, *s, discovered),
            Msg::WifiScan(r) => self.shell.on_wifi_scan(r),
            Msg::WifiConnect(ssid, r) => {
                for e in self.shell.on_wifi_connect(&ssid, r) {
                    self.apply(ctx, e);
                }
                self.refresh_soon();
            }
            Msg::Volume(Ok(v)) => {
                self.sys.volume = Some(v);
                let text = if v.muted {
                    "Muted".to_string()
                } else {
                    format!("Volume {}%", v.percent)
                };
                self.show_toast(ctx, &text);
            }
            Msg::BtScan(r) => self.shell.on_bt_scan(r),
            Msg::BtPaired(mac, r) => {
                for e in self.shell.on_bt_paired(&mac, r) {
                    self.apply(ctx, e);
                }
                self.refresh_soon();
            }
            Msg::StoreResults(r) => {
                // Search results need their icons too
                // ("lots of apps don't have icons").
                if let Ok(items) = &r {
                    let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
                    self.fill_store_icons(ids);
                }
                self.shell.on_store_search(r)
            }
            Msg::StoreJob(id, job) => self.shell.on_store_job(&id, job),
            Msg::StoreDone(id, r) => {
                match &r {
                    Ok(_) => self.images.clear(),
                    Err(e) => self.show_toast(ctx, e),
                }
                self.shell.on_store_done(&id, r.map(|_| ()));
                self.refresh_soon();
            }
            Msg::Apks(r) => self.shell.on_apks(r),
            Msg::AndroidOpened(pkg) => {
                lock(&self.watch).android_opened = Some((pkg.clone(), Instant::now()));
                if !self.android_running.contains(&pkg) {
                    self.android_running.push(pkg.clone());
                }
                // The Android app's own id, so Recents thumbnails attach to it.
                self.foreground = self
                    .apps
                    .iter()
                    .find(
                        |a| matches!(&a.source, AppSource::Android { package } if *package == pkg),
                    )
                    .map(|a| a.id.clone())
                    .or_else(|| Some(android::SESSION_APP_ID.to_string()));
                let front = self.foreground.clone().unwrap_or_default();
                self.show_app(&front);
            }
            Msg::Lifecycle(ev) => self.handle_lifecycle(ctx, ev),
            Msg::ApkDone(text) => {
                self.show_toast(ctx, &text);
                self.refresh_soon();
            }
            Msg::WindowSeen(id, window) => {
                self.windows.insert(id, window);
            }
            Msg::AndroidAppsChanged => {
                // Re-read Android's apps and make Home icons for TV-only ones;
                // the usual refresh then picks up the desktop files. The first
                // fingerprint after a boot can come before Android's platform
                // service answers, so this waits like a launch does.
                let home = self.home.clone();
                self.jobs.spawn(move || {
                    let mut r = SystemRunner;
                    let mut sleep = |d: Duration| std::thread::sleep(d);
                    match android::wait_until_ready(&mut r, android::READY_TIMEOUT, &mut sleep) {
                        Ok(apps) => {
                            let made = android::ensure_icons(&mut r, &home, &apps);
                            if android::sync_tv_desktops(&home, &apps, android::is_booted()) || made
                            {
                                eprintln!("catostv: Android icons updated ({} apps)", apps.len());
                            }
                        }
                        Err(e) => eprintln!("catostv: Android apps not listed: {e}"),
                    }
                    Msg::Refresh
                });
            }
            Msg::StoreDetails(id, res) => {
                if let Ok(d) = &res {
                    for url in d.screenshots.clone() {
                        self.fetch_screenshot(url);
                    }
                }
                self.shell.on_store_details(&id, res);
            }
            Msg::Screenshot(url, path) => {
                self.images.forget(&path);
                self.store_shots.insert(url, path);
            }
            Msg::FinServers(v) => {
                if let Some(f) = self.shell.fin.as_mut() {
                    f.on_discovered(v);
                }
            }
            Msg::FinLogin(r) => {
                let ok = match r {
                    Ok(a) => {
                        self.media.accounts.add(a);
                        self.save_accounts(ctx);
                        self.last_media_refresh = None;
                        Ok(())
                    }
                    Err(e) => Err(e),
                };
                if let Some(f) = self.shell.fin.as_mut() {
                    for e in f.on_login(ok) {
                        self.fin_effect(ctx, e);
                    }
                }
            }
            Msg::FinHome(user, r) => {
                let current = self
                    .media
                    .accounts
                    .active()
                    .is_some_and(|a| a.user_id == user);
                if current {
                    // Same data feeds CatosTV Home when this is the home account.
                    if let Ok(d) = &r
                        && self
                            .media
                            .accounts
                            .home()
                            .is_some_and(|h| h.user_id == user)
                    {
                        self.media.home = d.home_rows();
                    }
                    if let Some(f) = self.shell.fin.as_mut() {
                        f.on_home(r);
                    }
                }
            }
            Msg::FinLibrary(id, r) => {
                if let Some(f) = self.shell.fin.as_mut() {
                    f.on_library(&id, r);
                }
            }
            Msg::FinSeasons(id, r) => {
                let effects = self
                    .shell
                    .fin
                    .as_mut()
                    .map(|f| f.on_seasons(&id, r))
                    .unwrap_or_default();
                for e in effects {
                    self.fin_effect(ctx, e);
                }
            }
            Msg::FinEpisodes(season, r) => {
                if let Some(f) = self.shell.fin.as_mut() {
                    f.on_episodes(&season, r);
                }
            }
            Msg::FinHomeMedia(items) => self.media.home = items,
            Msg::FinImage(url, path) => {
                self.fin_fetching.remove(&url);
                self.fin_images.insert(url, path);
            }
            Msg::FilesListed(dir, r) => {
                if let Some(f) = self.shell.files.as_mut() {
                    if dir.is_empty() {
                        if let Ok(places) = r {
                            f.on_places(places);
                        }
                    } else {
                        f.on_listed(&dir, r);
                    }
                }
            }
            Msg::SearchMedia(q, items) => self.shell.on_search_media(q, items),
            Msg::FilesFailed(dir, e) => {
                self.show_toast(ctx, &e);
                self.files_effect(ctx, catostv_core::files::FilesEffect::List(dir));
            }
            Msg::FinPlaybackDone(next) => {
                if let Some(b) = next {
                    let (a, item) = *b;
                    self.fin_play(ctx, a, item, false);
                }
                self.last_media_refresh = None;
                self.refresh_home_media();
                if self
                    .shell
                    .fin
                    .as_ref()
                    .is_some_and(|f| matches!(f.screen(), Some(fin::Screen::Browse)))
                {
                    self.fin_effect(ctx, FinEffect::LoadHome);
                }
            }
            Msg::QuickOverApp => {
                self.mapper.reset();
                self.shell.open_quick(true);
                // Which app, and is it full screen (for the controls row).
                let name = self
                    .foreground
                    .as_deref()
                    .and_then(|f| self.apps.iter().find(|a| a.id == f))
                    .map(|a| a.name.clone())
                    .or_else(|| self.foreground.clone())
                    .unwrap_or_else(|| "App".into());
                let fullscreen = self
                    .foreground
                    .as_deref()
                    .and_then(|f| self.window_pattern(f))
                    .and_then(|w| catostv_core::toplevel::is_fullscreen(&w))
                    .unwrap_or(false);
                self.shell.quick_app = Some((name, fullscreen));
            }
            Msg::AndroidHome => {
                // Backed out of an Android app: an app that was open
                // underneath (shown now) goes to the background too.
                self.send_android_home();
                self.foreground = None;
                self.shell.go_home();
            }
            Msg::AndroidDidNotStart(pkg) => {
                self.foreground = None;
                self.shell.go_home();
                let name = self
                    .apps
                    .iter()
                    .find(
                        |a| matches!(&a.source, AppSource::Android { package } if *package == pkg),
                    )
                    .map_or(pkg.clone(), |a| a.name.clone());
                self.show_toast(ctx, &format!("{name} didn't start. Try again"));
            }
            Msg::KdeNamed(ok) => self.kde_named = ok,
            Msg::UpdateCheck(r) => {
                self.sys.updates.checking = false;
                self.sys.updates.check = Some(r);
            }
            Msg::Thumb(id, shot, blur) => {
                // Same file names alternate: drop the old pictures first.
                if let Some((s, b)) = self.thumbs.insert(id, (shot.clone(), blur.clone())) {
                    self.images.forget(&s);
                    self.images.forget(&b);
                }
                self.images.forget(&shot);
                self.images.forget(&blur);
            }
            Msg::AndroidHung(pkg) => {
                let name = self
                    .apps
                    .iter()
                    .find(
                        |a| matches!(&a.source, AppSource::Android { package } if *package == pkg),
                    )
                    .map_or(pkg.clone(), |a| a.name.clone());
                // The app CatosTV opened last is gone: CatosTV takes over
                // again rather than leaving whatever Android shows next.
                let ours = android::read_launched().is_some_and(|l| l.package == pkg);
                if ours && self.foreground.is_some() {
                    let _ = compositor::show_shell_workspace(&mut SystemRunner, true);
                    hide_android_window();
                    self.foreground = None;
                    self.shell.go_home();
                }
                self.show_toast(
                    ctx,
                    &format!("{name} stopped responding and was closed. Open it again"),
                );
            }
            Msg::AndroidRemoved(pkg, name) => {
                // Its tile goes now, not on Android's next app list.
                self.discovered.retain(
                    |a| !matches!(&a.source, AppSource::Android { package } if *package == pkg),
                );
                self.rebuild_apps();
                self.refresh_soon();
                self.show_toast(ctx, &format!("{name} removed"));
            }
            Msg::AndroidStuck => {
                if self.foreground.as_deref().is_some_and(|f| {
                    f == android::SESSION_APP_ID
                        || self
                            .apps
                            .iter()
                            .any(|a| a.id == f && matches!(a.source, AppSource::Android { .. }))
                }) {
                    let _ = compositor::show_shell_workspace(&mut SystemRunner, true);
                    hide_android_window();
                    self.foreground = None;
                    self.shell.go_home();
                }
                self.show_toast(ctx, "Android stopped opening apps. Restarting it");
                self.jobs.spawn(|| {
                    let mut sleep = |d: Duration| std::thread::sleep(d);
                    match android::stop_stale_session(&mut SystemRunner, &mut sleep) {
                        Ok(()) => Msg::AndroidRestarted,
                        Err(e) => Msg::Toast(e.to_string()),
                    }
                });
            }
            Msg::AndroidRestarted => {
                self.sys.android.session = false;
                self.android_started = false;
                self.ensure_android_session(ctx, true);
            }
            Msg::AndroidRetry(task) => {
                self.sys.android.session = false;
                self.android_started = false;
                self.android(ctx, task);
            }
            Msg::AndroidNoWindow(name) => {
                self.foreground = None;
                self.shell.go_home();
                self.show_toast(
                    ctx,
                    &format!("Android didn't show {name}. Try opening it again"),
                );
            }
            Msg::AndroidScreenRestart => {
                self.foreground = None;
                self.shell.go_home();
                self.show_toast(
                    ctx,
                    "Android's screen service restarted. Open the app again",
                );
            }
            Msg::ModeFailed(e) => {
                self.show_toast(ctx, &e);
                for eff in self.shell.on_display_timeout() {
                    self.apply(ctx, eff);
                }
            }
            Msg::Volume(Err(e)) | Msg::Toast(e) => self.show_toast(ctx, &e),
            Msg::Refresh => self.refresh_soon(),
            Msg::Nothing => {}
        }
    }

    /// A new install state, from the progress poll or a refresh. When an
    /// update has just finished, the new interface takes over. Both paths
    /// come here: the poll often sees "done" first, and the refresh after
    /// it would then find nothing new and never restart.
    fn follow_install(
        &mut self,
        ctx: &egui::Context,
        st: Option<catostv_core::updates::InstallState>,
    ) {
        if let Some(catostv_core::updates::InstallState::Done(v)) = &st
            && catostv_core::updates::newer(v, catostv_core::env::version())
            // Once per update: a "done" left over from an earlier update
            // (another version) must not stop this one.
            && self.sys.updates.install != st
            && self.restart_after_update.is_none()
        {
            self.show_toast(ctx, &format!("Updated to {v}. Restarting the interface"));
            self.restart_after_update = Some(Instant::now());
        }
    }

    fn on_snapshot(
        &mut self,
        ctx: &egui::Context,
        mut s: SystemSnapshot,
        discovered: Vec<AppEntry>,
    ) {
        self.refreshing = false;
        // Phones should see "CatosTV", not the hostname (KDE Connect's
        // default). Tried each refresh until the daemon answers.
        if !self.kde_named && self.appliance {
            self.kde_named = true;
            self.jobs.spawn(|| {
                Msg::KdeNamed(kdeconnect::set_name(&mut SystemRunner, "CatosTV").is_ok())
            });
        }
        // The last update check lives here, not in the snapshot.
        s.updates.checking = self.sys.updates.checking;
        s.updates.check = self.sys.updates.check.clone();
        self.follow_install(ctx, s.updates.install.clone());
        let first = std::mem::take(&mut self.first_snapshot);
        if !first && self.appliance && self.pending_scale.is_none() {
            // A TV connected or switched to 4K later still gets its size.
            let prefs = self.display.clone();
            let outputs = s.outputs.clone();
            if outputs.iter().any(|o| {
                o.enabled
                    && !(prefs.output == o.name && prefs.scale.is_some())
                    && display::default_scale(o).is_some_and(|d| (o.scale - d).abs() > 0.001)
            }) {
                self.job(move |r| display::apply_default_scale(r, &prefs, &outputs));
            }
        }
        if first && self.appliance {
            // Re-apply the saved display mode/scale, and prefer the TV's
            // own speakers (HDMI) once per start.
            let prefs = self.display.clone();
            let outputs = s.outputs.clone();
            // Not while Bluetooth headphones are connected (they get the
            // sound, see route_on_change).
            let sink = if s.sinks.iter().any(|x| x.kind == audio::SinkKind::Bluetooth) {
                None
            } else {
                audio::preferred_tv_sink(&s.sinks)
            };
            self.jobs.spawn(move || {
                let mut r = SystemRunner;
                if let Err(e) = display::apply_prefs(&mut r, &prefs, &outputs) {
                    eprintln!("catostv-shell: display prefs: {e}");
                }
                if let Some(id) = sink {
                    let _ = audio::set_default_sink(&mut r, id);
                }
                Msg::Nothing
            });
        }
        // Store icons for the curated catalogue, installed apps and the
        // current search results (retried each refresh: the appstream cache
        // may arrive after the first search).
        let ids: Vec<String> = flatpak::CURATED
            .iter()
            .map(|c| c.id.to_string())
            .chain(s.flatpaks.iter().map(|f| f.id.clone()))
            .chain(
                self.shell
                    .store_results
                    .items()
                    .iter()
                    .map(|i| i.id.clone()),
            )
            .collect();
        self.fill_store_icons(ids);
        // Rebuilt on every refresh (cheap PATH lookups), so an app installed
        // or removed outside CatosTV appears/disappears within one refresh.
        self.discovered = discovered;
        self.rebuild_apps();
        // The green dots follow Android's own list of open apps
        // (android-watch, every second), plus the app in front (just opened,
        // before that list catches up). Apps Android closed lose their dot;
        // the dots are cleared only when that list is empty and Android's
        // session reports stopped, one failed status check used to wipe them.
        let tasks = android::read_tasks();
        if !tasks.is_empty() {
            let front = self.foreground_android().flatten();
            self.android_running = tasks
                .into_iter()
                .filter(|p| !android::is_android_home(p))
                .collect();
            if let Some(f) = front
                && !self.android_running.contains(&f)
            {
                self.android_running.push(f);
            }
        } else if !s.android.session {
            self.android_running.clear();
        }
        // Bluetooth headphones or a speaker connected: the sound goes there;
        // disconnected: back to the TV.
        if self.appliance
            && let Some(id) = audio::route_on_change(&self.sys.sinks, &s.sinks)
        {
            s.sinks.iter_mut().for_each(|x| x.is_default = x.id == id);
            self.jobs.spawn(move || {
                let _ = audio::set_default_sink(&mut SystemRunner, id);
                Msg::Nothing
            });
        }
        self.sys = s;
        self.autostart_android(ctx);
        self.refresh_home_media();
        // A phone asking to pair gets a pop-up right away.
        {
            let views = self.views();
            let sctx = ShellContext {
                apps: &views,
                sys: &self.sys,
                prefs: &self.prefs,
                display: &self.display,
                media: &self.media,
            };
            self.shell.on_kde_requests(&sctx);
            self.shell.settle_store_log(&sctx);
        }
        ctx.request_repaint();
    }

    fn handle_lifecycle(&mut self, ctx: &egui::Context, ev: LifecycleEvent) {
        let id = ev.id().to_string();
        let name = self
            .apps
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.name.clone())
            .unwrap_or_else(|| id.clone());
        match ev {
            LifecycleEvent::Closed { .. } if self.pending_restart.remove(&id) => {
                if let Some(i) = self.apps.iter().position(|a| a.id == id) {
                    self.launch(ctx, i);
                }
            }
            LifecycleEvent::Crashed { reason, .. } => {
                self.show_toast(ctx, &format!("{name} closed unexpectedly ({reason})"));
                self.return_to_foreground_exit(&id);
            }
            _ => self.return_to_foreground_exit(&id),
        }
        // Android itself stopped (or crashed) while an Android app was in
        // front: come back to CatosTV instead of revealing whatever other app
        // is still on the app desktop.
        let android_in_front = self.foreground.as_deref().is_some_and(|f| {
            f == android::SESSION_APP_ID
                || self
                    .apps
                    .iter()
                    .any(|a| a.id == f && matches!(a.source, AppSource::Android { .. }))
        });
        if id == android::SESSION_APP_ID && android_in_front {
            self.foreground = None;
            self.android_running.clear();
            self.return_to_shell();
        }
    }

    /// Switch back to CatosTV only when the app that was in front ended, so a
    /// background app exiting never yanks the user out of what they watch.
    fn return_to_foreground_exit(&mut self, id: &str) {
        if self.foreground.as_deref() == Some(id) {
            self.foreground = None;
            self.return_to_shell();
        }
    }

    /// A Store screenshot from the cache, or download it in the background.
    fn fetch_screenshot(&mut self, url: String) {
        if self.store_shots.contains_key(&url) {
            return;
        }
        let path = flatpak::screenshot_cache_path(&self.home, &url);
        if path.is_file() {
            self.store_shots
                .insert(url, path.to_string_lossy().into_owned());
            return;
        }
        self.jobs.spawn(move || {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let tmp = path.with_extension("part");
            let ok = flatpak::screenshot_cmd(&url, &tmp)
                .is_ok_and(|cmd| SystemRunner.run(&cmd).is_ok_and(|o| o.success()))
                && std::fs::rename(&tmp, &path).is_ok();
            if ok {
                Msg::Screenshot(url, path.to_string_lossy().into_owned())
            } else {
                let _ = std::fs::remove_file(&tmp);
                Msg::Nothing
            }
        });
    }

    /// Store icons (Flathub appstream cache) for `ids` not looked up yet.
    fn fill_store_icons(&mut self, ids: Vec<String>) {
        for id in ids {
            if !self.store_icons.contains_key(&id)
                && let Some(p) = flatpak::store_icon_path(&self.home, &id)
            {
                self.store_icons.insert(id, p.display().to_string());
            }
        }
    }

    fn mgr(&self) -> MutexGuard<'_, AppManager> {
        lock(&self.manager)
    }

    /// Tell the app watcher which app is in front.
    fn sync_watch(&self) {
        let android_front = self.foreground.as_deref().is_some_and(|f| {
            f == android::SESSION_APP_ID
                || self
                    .apps
                    .iter()
                    .any(|a| a.id == f && matches!(a.source, AppSource::Android { .. }))
        });
        let mut w = lock(&self.watch);
        w.foreground = self.foreground.clone();
        w.android_front = android_front;
    }

    fn busy(&self) -> bool {
        self.sys.android_setup.as_ref().is_some_and(|s| s.running())
            || self.sys.android_repairing
            || self.shell.bt_busy.is_some()
            || !self.shell.store_jobs.is_empty()
            || self.bluetooth_on_screen()
    }

    /// Quick settings or Settings → Bluetooth / Audio is open: a device
    /// that connects shows up within a couple of seconds.
    fn bluetooth_on_screen(&self) -> bool {
        use catostv_core::settings::Category;
        matches!(
            self.shell.overlay(),
            Some(
                catostv_core::shell::Overlay::Quick { .. }
                    | catostv_core::shell::Overlay::QuickBluetooth { .. }
            )
        ) || (self.shell.page == Page::Settings
            && matches!(
                self.shell.current_category(),
                Category::Bluetooth | Category::Sound
            ))
    }

    /// Something on screen is animating (spinners, search radar).
    fn animating(&self) -> bool {
        use catostv_core::installer::Scan;
        let s = &self.shell;
        (s.overlay().is_some()
            && (matches!(s.bt_scan, Scan::Busy)
                || matches!(s.wifi_scan, Scan::Busy)
                || s.wifi_connecting.is_some()
                || matches!(s.apks, Scan::Busy)))
            || matches!(s.store_results, Scan::Busy)
    }
}

impl eframe::App for ShellApp {
    /// Runs every frame while the shell is visible. While its window is on
    /// the hidden desktop it may not run at all (no frame callbacks), which
    /// is why app exits are handled by the app watcher thread.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for msg in self.jobs.drain() {
            self.handle_message(ctx, msg);
        }
        let every = if self.busy() {
            FAST_REFRESH_SECS
        } else {
            REFRESH_SECS
        };
        let due = self
            .last_refresh
            .is_none_or(|t| t.elapsed().as_secs_f64() > every);
        if due {
            self.last_refresh = Some(Instant::now());
            self.refresh_system();
        }
        if self
            .pending_scale
            .as_ref()
            .is_some_and(|(_, _, at)| Instant::now() >= *at)
            && let Some((output, scale, _)) = self.pending_scale.take()
        {
            self.job(move |r| display::set_scale(r, &output, scale));
        }
        if let Some(d) = self.confirm_deadline
            && Instant::now() >= d
        {
            self.confirm_deadline = None;
            for e in self.shell.on_display_timeout() {
                self.apply(ctx, e);
            }
        }
        if self.appliance {
            refit_full_screen(ctx, &mut self.refit_at, &mut self.refits);
        }
        self.sync_watch();
        // Keep polling app processes and the clock even when idle.
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        let focused = keys::window_focused(&ctx);
        if !focused && self.was_focused {
            self.mapper.reset();
        }
        self.was_focused = focused;

        // Just woke from Sleep: the waking key (and any pressed while the
        // screen comes back) must not act, it used to land on Quick
        // settings' "Sleep" and put the TV straight back to sleep.
        let mono = self.started.elapsed().as_secs_f64();
        if let Some(boot) = power::boot_seconds()
            && self.wake.observe(boot, mono)
        {
            eprintln!("catostv: woke from sleep");
            self.mapper.reset();
        }
        let quiet = self.wake.input_blocked(mono);
        let events = keys::collect(&ctx);
        let actions = if quiet {
            Vec::new()
        } else {
            self.mapper
                .map(&events, keys::now(&ctx), self.shell.text_mode())
        };
        let mut p = self.pointer.collect(&ctx);
        if quiet {
            p = Default::default();
            self.pending_nav.clear();
        }
        let mut effects = Vec::new();
        {
            let views = self.views();
            let sctx = ShellContext {
                apps: &views,
                sys: &self.sys,
                prefs: &self.prefs,
                display: &self.display,
                media: &self.media,
            };
            let nav = std::mem::take(&mut self.pending_nav);
            for action in actions.into_iter().chain(nav) {
                effects.extend(self.shell.handle(action, &sctx));
            }
            // Mouse: click = focus + OK, right click = Back, wheel =
            // Up/Down. Moving the mouse no longer moves the focus: resting
            // the pointer over a list made it scroll by itself.
            // Hit rects are from the previous frame.
            if let Some(pos) = p.moved_to {
                self.hover = view::hit_test(&self.hits, pos);
            }
            if let Some(pos) = p.click
                && let Some(t) = view::hit_test(&self.hits, pos)
            {
                effects.extend(self.shell.pointer(t, true, &sctx));
            }
            if p.back {
                effects.extend(self.shell.handle(Action::Back, &sctx));
            }
            let (n, action) = if p.wheel > 0 {
                (p.wheel, Action::Up)
            } else {
                (-p.wheel, Action::Down)
            };
            for _ in 0..n.min(5) {
                effects.extend(self.shell.handle(action, &sctx));
            }
        }
        for e in effects {
            self.apply(&ctx, e);
        }
        if self.mapper.needs_tick() {
            ctx.request_repaint();
        }

        // Settings → Updates opened: check once.
        if self.shell.page == Page::Settings
            && catostv_core::settings::Category::ALL.get(self.shell.settings_cat)
                == Some(&catostv_core::settings::Category::Updates)
            && self.sys.updates.check.is_none()
            && !self.sys.updates.checking
        {
            self.apply(&ctx, Effect::CheckUpdates);
        }
        // While an update installs, follow its progress.
        if matches!(
            self.sys.updates.install,
            Some(catostv_core::updates::InstallState::Running(_))
        ) {
            if let Some(st) = catostv_core::updates::read_state() {
                self.follow_install(&ctx, Some(st.clone()));
                self.sys.updates.install = Some(st);
            }
            ctx.request_repaint_after(Duration::from_secs(2));
        }
        if self
            .restart_after_update
            .is_some_and(|t| t.elapsed() > Duration::from_secs(4))
        {
            self.restart_after_update = None;
            self.apply(&ctx, Effect::RestartShell);
        }
        let views = self.views();
        let sctx = ShellContext {
            apps: &views,
            sys: &self.sys,
            prefs: &self.prefs,
            display: &self.display,
            media: &self.media,
        };
        self.shell.clamp(&sctx);
        let c = Canvas::new(ui, self.display.safe_area_pct);
        let now = Instant::now();
        if matches!(&self.toast, Some((_, until)) if now >= *until) {
            self.toast = None;
        }
        let toast = self.toast.as_ref().map(|(text, until)| {
            let left = until.saturating_duration_since(now).as_secs_f32();
            (text.as_str(), (left / 0.3).clamp(0.0, 1.0))
        });
        if self
            .launching
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() > Duration::from_millis(2200))
        {
            self.launching = None;
        }
        let confirm_left = self
            .confirm_deadline
            .map(|d| d.saturating_duration_since(now).as_secs() + 1);
        // CPU and memory for Recents: every 2 s while it is shown.
        if self.shell.page == Page::Recents
            && self
                .usage_at
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(2))
        {
            let packages: Vec<(String, String)> = self
                .apps
                .iter()
                .filter_map(|a| match &a.source {
                    AppSource::Android { package } => Some((a.id.clone(), package.clone())),
                    _ => None,
                })
                .collect();
            self.usage = self.sampler.sample(&packages);
            self.usage_at = Some(now);
            ctx.request_repaint_after(Duration::from_secs(2));
        }
        view::draw(
            &c,
            &ShellView {
                thumbs: &self.thumbs,
                usage: &self.usage,
                shots: &self.store_shots,
                fin_cache: &self.fin_images,
                fin_wanted: &self.fin_wanted,
                page_t: {
                    if self.shell.page != self.last_page {
                        self.last_page = self.shell.page;
                        self.page_since = now;
                    }
                    (now.duration_since(self.page_since).as_secs_f32() / 0.28).min(1.0)
                },
                launching: self
                    .launching
                    .as_ref()
                    .map(|(id, t)| (id.as_str(), t.elapsed().as_secs_f32())),
                since_start: self.started.elapsed().as_secs_f32(),
                shell: &self.shell,
                ctx: &sctx,
                images: &self.images,
                store_icons: &self.store_icons,
                toast,
                confirm_left,
                now: keys::now(&ctx),
            },
        );
        // The Home backdrop drifts slowly: keep frames coming while
        // Home is on screen; ~30 fps is plenty for slow light.
        if self.prefs.glow && self.shell.page == catostv_core::shell::Page::Home {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
        let fin_loading = self.shell.fin.as_ref().is_some_and(|f| {
            matches!(f.home, catostv_core::installer::Scan::Busy)
                || matches!(
                    f.screen(),
                    Some(
                        fin::Screen::Connect {
                            servers: catostv_core::installer::Scan::Busy,
                            ..
                        } | fin::Screen::SignIn { busy: true, .. }
                            | fin::Screen::Library {
                                items: catostv_core::installer::Scan::Busy,
                                ..
                            }
                    )
                )
        });
        let busy = toast.is_some()
            || fin_loading
            || !self.fin_fetching.is_empty()
            || self.launching.is_some()
            || self.started.elapsed().as_secs_f32() < view::INTRO_SECS
            || confirm_left.is_some()
            || self.busy()
            || self.animating()
            || now.duration_since(self.page_since).as_secs_f32() < 0.3;
        self.hits = c.take_hits();
        self.fetch_fin_images();
        lock(&self.watch).last_frame = Some(Instant::now());
        self.sync_watch();
        if busy {
            ctx.request_repaint();
        }
    }
}

fn main() -> eframe::Result {
    catostv::run_app("catostv-shell", "CatosTV", ShellApp::new)
}

/// How long a just-launched Android app gets to show its window before
/// the shell switches to the app desktop anyway.
const ANDROID_WINDOW_WAIT: Duration = Duration::from_secs(15);

/// Block until a compositor window matches `want` (see `toplevel::matches`)
/// or `budget` is used up. Returns whether it appeared.
/// The shell's window fills its screen: when the TV's output went
/// away and came back (TV off/on, wake, a mode or scale change), labwc
/// dropped the window's full-screen geometry and Home showed in a
/// 1920×1080 corner of the 4K screen until the interface was restarted.
/// Not full screen, or smaller than the screen → ask again (every 2 s at
/// most, toggling off first when labwc still calls it full screen).
fn refit_full_screen(ctx: &egui::Context, last: &mut Option<Instant>, tries: &mut u32) {
    let (full, monitor, screen) = ctx.input(|i| {
        let v = i.viewport();
        (v.fullscreen, v.monitor_size, i.viewport_rect().size())
    });
    let smaller = monitor.is_some_and(|m| {
        m.x > 0.0 && m.y > 0.0 && (screen.x < m.x * 0.98 || screen.y < m.y * 0.98)
    });
    let not_full = full == Some(false);
    if !(smaller || not_full) {
        *tries = 0;
        return;
    }
    // Three quick tries, then once a minute: a size check that never
    // agrees (an unusual output) must not toggle full screen forever.
    let wait = if *tries < 3 { 2 } else { 60 };
    if last.is_some_and(|t| t.elapsed() < Duration::from_secs(wait)) {
        return;
    }
    *last = Some(Instant::now());
    *tries += 1;
    eprintln!(
        "catostv: window {:.0}×{:.0} on a {} screen (full screen: {:?}); filling it again",
        screen.x,
        screen.y,
        monitor.map_or("?".into(), |m| format!("{:.0}×{:.0}", m.x, m.y)),
        full
    );
    if full == Some(true) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
}

/// How long to wait for Android to report the opened app in front.
const ANDROID_TOP_WAIT: Duration = Duration::from_secs(6);

/// Wait until `android-watch` reports `pkg` as Android's front app.
fn wait_for_android_top(pkg: &str, budget: Duration) -> bool {
    let started = Instant::now();
    loop {
        if android::read_top().as_deref() == Some(pkg) {
            return true;
        }
        if started.elapsed() >= budget {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Is the window pattern Android's (Waydroid's full-screen window, or any
/// Waydroid window)? Case-insensitive, as window matching is.
fn is_android_window(pattern: &str) -> bool {
    pattern.to_ascii_lowercase().starts_with("waydroid")
}

fn wait_for_window(want: &str, budget: Duration) -> bool {
    let started = Instant::now();
    loop {
        if let Ok(list) = catostv_core::toplevel::list()
            && list
                .iter()
                .any(|t| catostv_core::toplevel::matches(&t.app_id, want))
        {
            return true;
        }
        if started.elapsed() >= budget {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
