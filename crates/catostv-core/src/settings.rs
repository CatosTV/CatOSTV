//! Settings model: categories and rows built from live system state.
//!
//! **Truthful-status rule:** a row may only show a value the system actually
//! reported. Features without a backend use [`RowKind::Soon`] and say
//! "Coming soon", never a made-up value. When you wire a new backend,
//! change the row kind here and update docs/IMPLEMENTATION_STATUS.md.
//!
//! **Copy rule:** labels, not sentences.
//! A subtitle is only shown when it carries state the user needs.

use crate::android::{AndroidStatus, SetupResult, SetupState};
use crate::apps::{AppEntry, AppSource};
use crate::audio::{ProfileSwitch, Sink, VolumeState};
use crate::bluetooth::{BluetoothStatus, Device};
use crate::display::{DisplayPrefs, Output};
use crate::flatpak::InstalledApp;
use crate::kdeconnect::KdeDevice;
use crate::net::NetStatus;
use crate::prefs::{self, UiPrefs};

/// Phone remote details published by `catostv-remoted`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteInfo {
    /// `http://192.168.1.20:8099`
    pub url: String,
    pub pin: String,
}

/// Live system facts, refreshed by the GUI off the UI thread.
/// `None` / empty means "not known yet / could not be read".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemSnapshot {
    pub wifi_on: Option<bool>,
    pub net: Option<NetStatus>,
    pub bluetooth: BluetoothStatus,
    pub bt_devices: Vec<Device>,
    /// `bluetoothctl info` of the paired devices: connected?, kind (icon).
    pub bt_details: Vec<crate::bluetooth::DeviceInfo>,
    pub volume: Option<VolumeState>,
    /// Running inside the installed appliance session.
    pub appliance: bool,
    /// Connected displays (`wlr-randr`). Empty outside a wlroots session.
    pub outputs: Vec<Output>,
    /// Audio outputs (`wpctl status`).
    pub sinks: Vec<Sink>,
    /// An HDMI card profile that would make an HDMI output appear.
    pub hdmi_hint: Option<ProfileSwitch>,
    pub android: AndroidStatus,
    /// Progress of the Android setup service, when it ran this boot.
    pub android_setup: Option<SetupState>,
    /// Settings → Android → Reset Android is running.
    pub android_repairing: bool,
    /// Google Play certification id (GApps image), once GMS created it.
    pub gsf_id: Option<String>,
    /// Which Android image the setup installed ("atv16-gapps", …).
    pub android_image: Option<String>,
    /// The setup's download so far: (megabytes done, megabytes total),
    /// read from its log while step 2 runs.
    pub android_download: Option<(u32, u32)>,
    /// Installed Flatpak apps (user + system).
    pub flatpaks: Vec<InstalledApp>,
    pub flatpak_available: bool,
    pub remote: Option<RemoteInfo>,
    /// KDE Connect devices (empty when kdeconnectd is not running).
    pub kde: Vec<KdeDevice>,
    pub cast_running: bool,
    /// This TV's IPv4 address on the LAN (System → About; SSH/remote).
    pub ip_address: Option<String>,
    /// Root filesystem `(available, total)` bytes.
    pub storage: Option<(u64, u64)>,
    /// The wired connection, if the TV has a port.
    pub ethernet: Option<crate::net::Ethernet>,
    /// Settings → Debug.
    pub debug: crate::debug::DebugStatus,
    /// Settings → Updates.
    pub updates: UpdatesView,
    /// Names of connected game controllers.
    pub controllers: Vec<String>,
}

/// An app plus its live state, as the shell sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppView {
    pub entry: AppEntry,
    pub installed: bool,
    pub running: bool,
}

/// What Settings → Updates shows: the last check (kept by
/// the GUI across snapshots) and the install unit's progress.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdatesView {
    pub checking: bool,
    pub check: Option<Result<crate::updates::Check, String>>,
    pub install: Option<crate::updates::InstallState>,
}

/// Everything the shell state machine needs to read.
#[derive(Debug, Clone, Copy)]
pub struct ShellContext<'a> {
    pub apps: &'a [AppView],
    pub sys: &'a SystemSnapshot,
    pub prefs: &'a UiPrefs,
    pub display: &'a DisplayPrefs,
    /// Catfyn accounts and the Home "Continue watching" row.
    pub media: &'a crate::fin::Media,
}

impl ShellContext<'_> {
    /// Indices (into `apps`) of running apps, in registry order.
    pub fn running(&self) -> Vec<usize> {
        self.apps
            .iter()
            .enumerate()
            .filter(|(_, a)| a.running)
            .map(|(i, _)| i)
            .collect()
    }

    /// The display CatosTV manages: the first enabled output.
    pub fn output(&self) -> Option<&Output> {
        self.sys.outputs.iter().find(|o| o.enabled)
    }

    /// The interface size to show and step from: the one just chosen (saved
    /// before the compositor reports it back) or what the output has now.
    /// Stepping from the reported value made quick presses repeat the same
    /// step.
    pub fn ui_scale(&self) -> Option<f32> {
        let out = self.output()?;
        Some(
            self.display
                .scale
                .filter(|_| self.display.output == out.name)
                .unwrap_or(out.scale),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Network,
    Bluetooth,
    Display,
    Sound,
    Customization,
    Apps,
    Android,
    Connect,
    Keys,
    System,
    Updates,
    Debug,
}

impl Category {
    pub const ALL: [Category; 12] = [
        Self::Network,
        Self::Bluetooth,
        Self::Display,
        Self::Sound,
        Self::Customization,
        Self::Apps,
        Self::Android,
        Self::Connect,
        Self::Keys,
        Self::System,
        Self::Updates,
        Self::Debug,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Network => "Network",
            Self::Bluetooth => "Bluetooth",
            Self::Display => "Display",
            Self::Sound => "Sound",
            Self::Customization => "Customization",
            Self::Apps => "Apps",
            Self::Android => "Android",
            Self::Connect => "Cast & remote",
            Self::Keys => "Navigation & controllers",
            Self::System => "System",
            Self::Updates => "Updates",
            Self::Debug => "Debug",
        }
    }

    /// Icon keyword understood by the GUI icon painter.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Network => "wifi",
            Self::Bluetooth => "bluetooth",
            Self::Display => "display",
            Self::Sound => "speaker",
            Self::Customization => "palette",
            Self::Apps => "grid",
            Self::Android => "android",
            Self::Connect => "cast",
            Self::Keys => "keyboard",
            Self::System => "gear",
            Self::Updates => "download",
            Self::Debug => "lock",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowId {
    WifiRadio,
    NetworkStatus,
    /// Ethernet details and IP settings.
    Ethernet,
    JoinWifi,
    BtRadio,
    BtDevice(usize),
    BtPair,
    Resolution,
    Scale,
    PictureFit,
    AdaptiveSync,
    Hdr,
    Volume,
    Mute,
    AudioOutput,
    HdmiAudio,
    Accent,
    ClockFormat,
    Glow,
    IpAddress,
    App(usize),
    /// Settings → Apps: filter the list.
    AppSearch,
    UpdateApps,
    AndroidState,
    AndroidSetup,
    /// After a failed setup: the same store again (Settings → Android).
    AndroidRetry,
    /// Before the setup: Google Play or Aurora Store.
    AndroidVariant,
    /// GApps: the id to register at google.com/android/uncertified.
    AndroidCertify,
    AndroidApk,
    AndroidRepair,
    AndroidStore,
    CastReceiver,
    PhoneRemote,
    KdeConnect,
    KdeDevice(usize),
    KeyGuide,
    /// A wired / connected controller (index into `SystemSnapshot::controllers`).
    Controller(usize),
    /// Controller navigation: bind a button to this action.
    PadBind(crate::gamepad::PadOut),
    PadBindReset,
    PadNav,
    About,
    DebugSsh,
    DebugLogin,
    DebugPassword,
    DebugRemoveOld,
    DebugRelay,
    DebugGpu,
    DebugBuild,
    RestartShell,
    Reboot,
    PowerOff,
    Storage,
    /// Settings → Updates.
    UpdateStatus,
    UpdateCheck,
    UpdateInstall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// On/off; OK toggles. `None` = state unknown.
    Toggle(Option<bool>),
    /// 0–100; Left/Right adjust while the row is focused.
    Slider(u8),
    /// A value picked with Left/Right (accent colour, interface size).
    Choice,
    /// OK does something (opens a panel, restarts, ...).
    Action,
    /// The main action of a page (e.g. "Pair a controller"): drawn as an
    /// accent button at the top; behaves like `Action`.
    Primary,
    /// Read-only information.
    Info,
    /// Not implemented yet. Shows "Coming soon".
    Soon,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: RowId,
    pub title: String,
    pub subtitle: String,
    pub value: String,
    pub kind: RowKind,
}

fn row(id: RowId, title: &str, subtitle: &str, value: impl Into<String>, kind: RowKind) -> Row {
    Row {
        id,
        title: title.into(),
        subtitle: subtitle.into(),
        value: value.into(),
        kind,
    }
}

fn on_off(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "On",
        Some(false) => "Off",
        None => "-",
    }
}

/// Interface scale steps offered in Display → Interface size.
pub const SCALES: &[f32] = &[1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// Label for a scale (`1.25` → "125 %").
pub fn scale_label(scale: f32) -> String {
    format!("{} %", (scale * 100.0).round() as u32)
}

/// The scale `delta` steps away from `current` within [`SCALES`].
pub fn cycle_scale(current: f32, delta: i32) -> f32 {
    let i = SCALES
        .iter()
        .position(|s| (s - current).abs() < 0.01)
        .unwrap_or(0) as i32;
    SCALES[(i + delta).clamp(0, SCALES.len() as i32 - 1) as usize]
}

/// Apps lists longer than this get a search row.
pub const APP_SEARCH_FROM: usize = 8;

pub fn rows(cat: Category, ctx: &ShellContext) -> Vec<Row> {
    rows_for(cat, ctx, "")
}

/// Like [`rows`], with Settings → Apps filtered by `apps_filter`
/// (case-insensitive, part of the name).
pub fn rows_for(cat: Category, ctx: &ShellContext, apps_filter: &str) -> Vec<Row> {
    let sys = ctx.sys;
    match cat {
        Category::Network => vec![
            row(
                RowId::WifiRadio,
                "Wi-Fi",
                "",
                on_off(sys.wifi_on),
                RowKind::Toggle(sys.wifi_on),
            ),
            row(
                RowId::NetworkStatus,
                "Connection",
                match &sys.net {
                    Some(n) if n.online => "Online",
                    Some(_) => "Offline",
                    None => "",
                },
                sys.net
                    .as_ref()
                    .map(|n| n.summary())
                    .unwrap_or_else(|| "-".into()),
                RowKind::Info,
            ),
            row(RowId::JoinWifi, "Choose a network", "", "", RowKind::Action),
        ]
        .into_iter()
        .chain(sys.ethernet.as_ref().map(|e| {
            row(
                RowId::Ethernet,
                "Ethernet",
                if e.manual {
                    "Manual IP"
                } else {
                    "Automatic IP"
                },
                ethernet_summary(e),
                RowKind::Action,
            )
        }))
        .collect(),
        Category::Bluetooth => {
            let bt = sys.bluetooth;
            if !bt.available {
                return vec![row(
                    RowId::BtRadio,
                    "Bluetooth",
                    "No adapter found",
                    "Unavailable",
                    RowKind::Info,
                )];
            }
            let mut v = vec![row(
                RowId::BtRadio,
                "Bluetooth",
                "",
                on_off(Some(bt.powered)),
                RowKind::Toggle(Some(bt.powered)),
            )];
            v.push(row(
                RowId::BtPair,
                "Pair a new device",
                "",
                "",
                RowKind::Primary,
            ));
            for (i, d) in sys.bt_devices.iter().enumerate() {
                let connected = sys
                    .bt_details
                    .iter()
                    .any(|x| x.address == d.address && x.connected);
                v.push(row(
                    RowId::BtDevice(i),
                    &d.name,
                    "",
                    if connected { "Connected" } else { "Paired" },
                    RowKind::Action,
                ));
            }
            v
        }
        Category::Display => display_rows(ctx),
        Category::Sound => {
            let vol = sys.volume;
            let mut v = vec![
                match vol {
                    Some(v) => row(
                        RowId::Volume,
                        "Volume",
                        "",
                        format!("{}%", v.percent),
                        RowKind::Slider(v.percent),
                    ),
                    None => row(
                        RowId::Volume,
                        "Volume",
                        "Audio service not available",
                        "-",
                        RowKind::Info,
                    ),
                },
                row(
                    RowId::Mute,
                    "Mute",
                    "",
                    on_off(vol.map(|v| v.muted)),
                    RowKind::Toggle(vol.map(|v| v.muted)),
                ),
            ];
            let current = sys
                .sinks
                .iter()
                .find(|s| s.is_default)
                .map(|s| s.name.clone());
            if sys.sinks.is_empty() {
                v.push(row(RowId::AudioOutput, "Output", "", "-", RowKind::Info));
            } else {
                v.push(row(
                    RowId::AudioOutput,
                    "Output",
                    "",
                    current.unwrap_or_else(|| "Choose".into()),
                    RowKind::Action,
                ));
            }
            if let Some(h) = &sys.hdmi_hint {
                v.push(row(
                    RowId::HdmiAudio,
                    "Use TV speakers (HDMI)",
                    &h.profile_description,
                    "",
                    RowKind::Action,
                ));
            }
            v
        }
        Category::Customization => vec![
            row(
                RowId::Accent,
                "Accent colour",
                "",
                prefs::accent_label(&ctx.prefs.accent),
                RowKind::Choice,
            ),
            row(
                RowId::ClockFormat,
                "Clock",
                "",
                if ctx.prefs.clock_24h {
                    "24-hour"
                } else {
                    "12-hour"
                },
                RowKind::Choice,
            ),
            row(
                RowId::Glow,
                "Background glow",
                "Soft colour behind the interface",
                on_off(Some(ctx.prefs.glow)),
                RowKind::Toggle(Some(ctx.prefs.glow)),
            ),
        ],
        Category::Apps => {
            let needle = apps_filter.trim().to_lowercase();
            let mut v: Vec<Row> = Vec::new();
            if ctx.apps.len() > APP_SEARCH_FROM || !needle.is_empty() {
                v.push(row(
                    RowId::AppSearch,
                    if needle.is_empty() {
                        "Search apps"
                    } else {
                        apps_filter.trim()
                    },
                    "",
                    "",
                    RowKind::Primary,
                ));
            }
            v.extend(
                ctx.apps
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| {
                        needle.is_empty() || a.entry.name.to_lowercase().contains(&needle)
                    })
                    .map(|(i, a)| {
                        // Open apps get a green dot on their tiles, not a label.
                        let state = if !a.installed {
                            "Not installed"
                        } else {
                            match a.entry.source {
                                AppSource::Flatpak { .. } => "Flatpak",
                                AppSource::Android { .. } => "Android",
                                AppSource::Configured => "",
                            }
                        };
                        row(RowId::App(i), &a.entry.name, "", state, RowKind::Action)
                    }),
            );
            if sys.flatpak_available && needle.is_empty() {
                v.push(row(
                    RowId::UpdateApps,
                    "Update store apps",
                    "",
                    "",
                    RowKind::Action,
                ));
            }
            v
        }
        Category::Android => android_rows(ctx),
        Category::Connect => {
            let mut v = vec![
                row(
                    RowId::CastReceiver,
                    "Cast to this TV",
                    "DLNA and YouTube",
                    if sys.cast_running { "On" } else { "Off" },
                    RowKind::Info,
                ),
                match &sys.remote {
                    Some(r) => row(
                        RowId::PhoneRemote,
                        "Phone remote",
                        &r.url,
                        format!("PIN {}", r.pin),
                        RowKind::Info,
                    ),
                    None => row(
                        RowId::PhoneRemote,
                        "Phone remote",
                        "",
                        "Not running",
                        RowKind::Info,
                    ),
                },
            ];
            if sys.kde.is_empty() {
                v.push(row(
                    RowId::KdeConnect,
                    "KDE Connect",
                    "Open KDE Connect on a phone on the same network",
                    "No devices",
                    RowKind::Info,
                ));
            }
            for (i, d) in sys.kde.iter().enumerate() {
                let state = if d.pair_requested_by_peer {
                    "Wants to pair"
                } else if d.paired && d.reachable {
                    "Connected"
                } else if d.paired {
                    "Not in range"
                } else {
                    "Not paired"
                };
                v.push(row(
                    RowId::KdeDevice(i),
                    &d.name,
                    "KDE Connect",
                    state,
                    RowKind::Action,
                ));
            }
            v
        }
        Category::Keys => {
            // Pairing first; then every connected controller with its
            // button layout (USB consoles pads too); then Bluetooth
            // remotes/keyboards and controllers that are not connected. No
            // rows for things that are not there, no explanations.
            let mut v = vec![row(
                RowId::BtPair,
                "Pair a controller or remote",
                "",
                "",
                RowKind::Primary,
            )];
            let same = |a: &str, b: &str| a.contains(b) || b.contains(a);
            for (i, name) in sys.controllers.iter().enumerate() {
                v.push(row(
                    RowId::Controller(i),
                    name,
                    "",
                    if ctx.prefs.nintendo_for(name) {
                        "Nintendo"
                    } else {
                        "Xbox / PlayStation"
                    },
                    RowKind::Choice,
                ));
            }
            for (i, d) in sys.bt_devices.iter().enumerate() {
                let info = sys.bt_details.iter().find(|x| x.address == d.address);
                let input = info
                    .and_then(|x| x.icon.as_deref())
                    .is_some_and(|ic| ic.starts_with("input-"));
                if !input || sys.controllers.iter().any(|c| same(c, &d.name)) {
                    continue;
                }
                let connected = info.is_some_and(|x| x.connected);
                v.push(row(
                    RowId::BtDevice(i),
                    &d.name,
                    "",
                    if connected {
                        "Connected"
                    } else {
                        "Not connected"
                    },
                    RowKind::Action,
                ));
            }
            if !sys.controllers.is_empty() {
                v.push(row(
                    RowId::PadNav,
                    "Use controllers in CatosTV",
                    "",
                    on_off(Some(ctx.prefs.pad_nav)),
                    RowKind::Toggle(Some(ctx.prefs.pad_nav)),
                ));
                // Controller navigation: which button does what in
                // CatosTV's own screens; OK on a row, then press the button.
                // Directions come from the d-pad and sticks.
                for action in crate::gamepad::BINDABLE {
                    let value = match ctx.prefs.pad_binding(action.key()) {
                        Some(code) => crate::gamepad::button_name(code),
                        None => "Default".to_string(),
                    };
                    v.push(row(
                        RowId::PadBind(action),
                        &format!("Controller: {}", action.label()),
                        "",
                        value,
                        RowKind::Action,
                    ));
                }
                if !ctx.prefs.pad_bindings.is_empty() {
                    v.push(row(
                        RowId::PadBindReset,
                        "Reset controller buttons",
                        "",
                        "",
                        RowKind::Action,
                    ));
                }
            }
            v.push(row(RowId::KeyGuide, "Key guide", "", "", RowKind::Action));
            v
        }
        Category::System => vec![
            row(
                RowId::About,
                "CatosTV",
                "",
                format!("Version {}", crate::env::version()),
                RowKind::Info,
            ),
            row(
                RowId::IpAddress,
                "IP address",
                "",
                sys.ip_address
                    .clone()
                    .unwrap_or_else(|| "Not connected".into()),
                RowKind::Info,
            ),
            row(
                RowId::Storage,
                "Storage",
                "",
                match sys.storage {
                    Some((avail, total)) => {
                        format!("{} free of {}", human_bytes(avail), human_bytes(total))
                    }
                    None => "-".into(),
                },
                RowKind::Info,
            ),
            row(RowId::Reboot, "Restart", "", "", RowKind::Action),
            row(RowId::PowerOff, "Power off", "", "", RowKind::Action),
        ],
        Category::Updates => update_rows(sys),
        Category::Debug => debug_rows(sys),
    }
}

/// Settings → Updates (`crate::updates`).
fn update_rows(sys: &SystemSnapshot) -> Vec<Row> {
    use crate::updates::{Check, InstallState};
    let u = &sys.updates;
    let running = matches!(u.install, Some(InstallState::Running(_)));
    let status = match (&u.install, u.checking, &u.check) {
        (Some(InstallState::Running(d)), _, _) => format!("{d}…"),
        (Some(InstallState::Failed(r)), _, _) => format!("Update failed: {r}"),
        (_, true, _) => "Checking…".into(),
        (_, _, Some(Ok(Check::NotSetUp))) => "Updates aren't set up yet".into(),
        (_, _, Some(Ok(Check::UpToDate(_)))) => "Up to date".into(),
        (_, _, Some(Ok(Check::Available(m)))) => format!("Version {} is available", m.version),
        (_, _, Some(Err(e))) => e.clone(),
        _ => String::new(),
    };
    let mut v = vec![row(
        RowId::UpdateStatus,
        &format!("CatosTV {}", crate::env::version()),
        "",
        status,
        RowKind::Info,
    )];
    if let Some(Ok(Check::Available(m))) = &u.check
        && !running
    {
        v.push(row(
            RowId::UpdateInstall,
            &format!("Install version {}", m.version),
            "Waits until nothing is playing; the TV restarts its interface",
            "",
            RowKind::Primary,
        ));
    }
    if !running {
        v.push(row(
            RowId::UpdateCheck,
            "Check for updates",
            "",
            "",
            RowKind::Action,
        ));
    }
    v
}

/// "Connected · 192.168.1.88" / "No cable".
pub fn ethernet_summary(e: &crate::net::Ethernet) -> String {
    match (&e.address, e.connected) {
        (Some(a), true) => format!("Connected · {}", a.split('/').next().unwrap_or(a)),
        (_, true) => "Connected".into(),
        _ => "No cable".into(),
    }
}

/// Settings → Debug (see `crate::debug`).
fn debug_rows(sys: &SystemSnapshot) -> Vec<Row> {
    use crate::debug::{DEFAULT_PASSWORD, LOGIN, OLD_LOGIN};
    let d = &sys.debug;
    let mut v = vec![
        row(
            RowId::DebugSsh,
            "Debug SSH",
            "Off again after every restart",
            on_off(Some(d.ssh_on)),
            RowKind::Toggle(Some(d.ssh_on)),
        ),
        row(
            RowId::DebugLogin,
            "Login",
            "",
            if d.password_changed {
                format!("{LOGIN} · your password")
            } else {
                format!("{LOGIN} · password {DEFAULT_PASSWORD} (change it)")
            },
            RowKind::Info,
        ),
        row(
            RowId::DebugPassword,
            "Change password",
            "",
            "",
            RowKind::Action,
        ),
    ];
    if d.old_login_present && d.ssh_login_seen {
        v.push(row(
            RowId::DebugRemoveOld,
            &format!("Remove the old {OLD_LOGIN} login"),
            &format!("{LOGIN} login over SSH works"),
            "",
            RowKind::Action,
        ));
    }
    v.push(row(
        RowId::RestartShell,
        "Restart the interface",
        "",
        "",
        RowKind::Action,
    ));
    v.push(row(
        RowId::DebugRelay,
        "Android display relay",
        "",
        if d.relay_running {
            "Running"
        } else {
            "Not running"
        },
        RowKind::Info,
    ));
    v.push(row(
        RowId::DebugGpu,
        "Graphics",
        "",
        if d.gpus.is_empty() {
            "-".to_string()
        } else {
            d.gpus.join(", ")
        },
        RowKind::Info,
    ));
    v.push(row(
        RowId::DebugBuild,
        "Build",
        "",
        match crate::env::commit() {
            Some(c) => format!("{} · {c}", crate::env::version()),
            None => crate::env::version().to_string(),
        },
        RowKind::Info,
    ));
    v
}

fn display_rows(ctx: &ShellContext) -> Vec<Row> {
    let fit = ctx.display.safe_area_pct;
    let fit_row = row(
        RowId::PictureFit,
        "Picture fit",
        "Shrink if the edges are cut off",
        if fit == 0 {
            "Full".to_string()
        } else {
            format!("−{fit} %")
        },
        RowKind::Slider(
            (fit as u32 * 100 / crate::display::MAX_SAFE_AREA_PCT.max(1) as u32).min(100) as u8,
        ),
    );
    let Some(out) = ctx.output() else {
        return vec![
            row(
                RowId::Resolution,
                "Resolution",
                "No display information",
                "-",
                RowKind::Info,
            ),
            fit_row,
        ];
    };
    let mode = out
        .current_mode()
        .map(|m| m.label())
        .unwrap_or_else(|| "-".into());
    let mut v = vec![
        row(
            RowId::Resolution,
            "Resolution",
            &out.display_name(),
            mode,
            RowKind::Action,
        ),
        row(
            RowId::Scale,
            "Interface size",
            "Size of apps on the TV",
            scale_label(ctx.ui_scale().unwrap_or(out.scale)),
            RowKind::Choice,
        ),
        fit_row,
    ];
    if let Some(on) = out.adaptive_sync {
        v.push(row(
            RowId::AdaptiveSync,
            "Variable refresh rate",
            "",
            on_off(Some(on)),
            RowKind::Toggle(Some(on)),
        ));
    }
    v.push(row(
        RowId::Hdr,
        "HDR",
        "Experimental · restarts the TV session · off again after a reboot",
        on_off(Some(ctx.prefs.hdr)),
        RowKind::Toggle(Some(ctx.prefs.hdr)),
    ));
    v
}

/// "512 GB" / "38.4 GB" / "900 MB" (decimal units, as disks are sold).
pub fn human_bytes(b: u64) -> String {
    let gb = b as f64 / 1e9;
    if gb >= 100.0 {
        format!("{gb:.0} GB")
    } else if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", b as f64 / 1e6)
    }
}

/// Overall setup progress, 0–100: finished steps plus the fraction of the
/// current one (`frac`, 0–1).
pub fn setup_percent(step: u32, total: u32, frac: f32) -> u8 {
    if total == 0 {
        return 0;
    }
    let done = (step.saturating_sub(1) as f32 + frac.clamp(0.0, 1.0)) / total as f32;
    (done * 100.0).round().clamp(0.0, 100.0) as u8
}

fn android_rows(ctx: &ShellContext) -> Vec<Row> {
    let a = &ctx.sys.android;
    if !a.installed {
        return vec![row(
            RowId::AndroidState,
            "Android",
            "Android apps aren't available on this TV",
            "Unavailable",
            RowKind::Info,
        )];
    }
    let mut v = Vec::new();
    match &ctx.sys.android_setup {
        Some(st) if st.running() => {
            // Real progress: the download's megabytes inside step
            // 2, the step count otherwise, as one bar over the whole setup.
            let (subtitle, percent) = match ctx.sys.android_download {
                Some((done, total)) if st.step == 2 && total > 0 => (
                    format!("{} · {done} MB of {total} MB", st.message),
                    setup_percent(st.step, st.total, done as f32 / total as f32),
                ),
                _ => (st.message.clone(), setup_percent(st.step, st.total, 0.0)),
            };
            v.push(row(
                RowId::AndroidState,
                "Setting up Android TV",
                &subtitle,
                format!("step {} of {}", st.step, st.total),
                RowKind::Slider(percent),
            ));
        }
        Some(SetupState {
            result: Some(SetupResult::Failed(reason)),
            ..
        }) if !a.initialized => {
            v.push(row(
                RowId::AndroidState,
                "Android setup didn't finish",
                reason,
                "",
                RowKind::Info,
            ));
            v.push(row(
                RowId::AndroidRetry,
                "Try again",
                "With the same app store",
                "",
                RowKind::Action,
            ));
        }
        _ if !a.initialized || ctx.sys.android_image.is_none() => {
            // One row; the store is chosen on the card sheet it opens.
            v.push(row(
                RowId::AndroidSetup,
                "Set up Android TV",
                "Google Play or Aurora Store · downloads about 1 GB",
                "",
                RowKind::Primary,
            ));
        }
        _ => {
            v.push(row(
                RowId::AndroidState,
                "Android",
                &ctx.sys
                    .android_image
                    .as_deref()
                    .map(crate::android::image_label)
                    .unwrap_or_default(),
                if ctx.sys.android_repairing {
                    "Resetting…"
                } else if a.keystore_broken {
                    "Needs a reset"
                } else if a.session {
                    "Running"
                } else {
                    "Ready"
                },
                RowKind::Info,
            ));
            if ctx
                .sys
                .android_image
                .as_deref()
                .is_some_and(|i| i.ends_with("-gapps"))
            {
                v.push(row(
                    RowId::AndroidCertify,
                    "Google Play certification",
                    "Register this TV so Google Play works",
                    ctx.sys
                        .gsf_id
                        .clone()
                        .unwrap_or_else(|| "Waiting for Android…".into()),
                    RowKind::Action,
                ));
            }
            v.push(row(
                RowId::AndroidApk,
                "Install an APK",
                "From a USB drive",
                "",
                RowKind::Action,
            ));
            v.push(row(
                RowId::AndroidRepair,
                RESET_LABEL,
                RESET_SUBTITLE,
                "",
                RowKind::Action,
            ));
        }
    }
    v
}

/// Settings → Android → Reset Android (worded plainly).
pub const RESET_LABEL: &str = "Reset Android";
pub const RESET_SUBTITLE: &str = "Fixes Android when it won't start or keeps crashing.";

/// Every row of every category, for Search.
pub fn all_rows(ctx: &ShellContext) -> Vec<(Category, Row)> {
    Category::ALL
        .iter()
        .flat_map(|&c| rows(c, ctx).into_iter().map(move |r| (c, r)))
        .filter(|(_, r)| r.id != RowId::AppSearch)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with<'a>(
        sys: &'a SystemSnapshot,
        prefs: &'a UiPrefs,
        display: &'a DisplayPrefs,
    ) -> ShellContext<'a> {
        ShellContext {
            media: &crate::fin::NO_MEDIA,
            apps: &[],
            sys,
            prefs,
            display,
        }
    }

    #[test]
    fn scales_cycle_and_label() {
        assert_eq!(cycle_scale(1.0, 1), 1.25);
        assert_eq!(cycle_scale(2.0, 1), 2.5);
        assert_eq!(cycle_scale(3.0, 1), 3.0);
        assert_eq!(cycle_scale(1.3, -1), 1.0, "unknown scale snaps to the list");
        assert_eq!(scale_label(1.25), "125 %");
    }

    #[test]
    fn display_rows_are_truthful() {
        let sys = SystemSnapshot::default();
        let (p, d) = (UiPrefs::default(), DisplayPrefs::default());
        let rows = rows(Category::Display, &ctx_with(&sys, &p, &d));
        assert_eq!(rows[0].kind, RowKind::Info, "no outputs → no fake mode");
        assert!(rows.iter().all(|r| r.id != RowId::Hdr));

        let sys = SystemSnapshot {
            outputs: crate::display::parse_wlr_randr(
                "HDMI-A-1 \"Samsung (HDMI-A-1)\"\n  Enabled: yes\n  Modes:\n    3840x2160 px, 60.000000 Hz (preferred, current)\n  Scale: 1.000000\n",
            ),
            ..SystemSnapshot::default()
        };
        let rows = super::rows(Category::Display, &ctx_with(&sys, &p, &d));
        assert_eq!(rows[0].value, "3840 × 2160 · 60 Hz");
        assert_eq!(rows[1].value, "100 %");
        let hdr = rows.iter().find(|r| r.id == RowId::Hdr).unwrap();
        assert_eq!(hdr.kind, RowKind::Toggle(Some(false)));
        assert_eq!(human_bytes(38_400_000_000), "38.4 GB");
        assert_eq!(human_bytes(512_000_000_000), "512 GB");
        assert_eq!(human_bytes(900_000_000), "900 MB");
    }

    #[test]
    fn android_rows_follow_state() {
        let (p, d) = (UiPrefs::default(), DisplayPrefs::default());
        let mut sys = SystemSnapshot::default();
        assert_eq!(
            rows(Category::Android, &ctx_with(&sys, &p, &d))[0].value,
            "Unavailable"
        );
        sys.android.installed = true;
        // One row before the setup (the store is chosen on the
        // sheet it opens).
        let r = rows(Category::Android, &ctx_with(&sys, &p, &d));
        assert_eq!(r[0].id, RowId::AndroidSetup);
        assert_eq!(r.len(), 1);
        sys.android_setup = Some(crate::android::parse_setup_state(
            "step=2/5 Downloading Android TV (about 1 GB)\n",
        ));
        let r = rows(Category::Android, &ctx_with(&sys, &p, &d));
        assert_eq!(r[0].value, "step 2 of 5");
        assert_eq!(r[0].kind, RowKind::Slider(20));
        // With the download's numbers the bar moves inside the step.
        sys.android_download = Some((500, 1000));
        let r = rows(Category::Android, &ctx_with(&sys, &p, &d));
        assert_eq!(r[0].kind, RowKind::Slider(30));
        assert!(r[0].subtitle.ends_with("500 MB of 1000 MB"));
        sys.android_download = None;
        sys.android_setup = None;
        sys.android.initialized = true;
        sys.android_image = Some("atv16-gapps".into());
        let ids: Vec<RowId> = rows(Category::Android, &ctx_with(&sys, &p, &d))
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                RowId::AndroidState,
                RowId::AndroidCertify,
                RowId::AndroidApk,
                RowId::AndroidRepair
            ]
        );
        // Plain wording.
        let rows = rows(Category::Android, &ctx_with(&sys, &p, &d));
        let reset = rows.iter().find(|r| r.id == RowId::AndroidRepair).unwrap();
        assert_eq!(reset.title, "Reset Android");
        assert_eq!(
            reset.subtitle,
            "Fixes Android when it won't start or keeps crashing."
        );
    }
}
