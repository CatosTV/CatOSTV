//! CatosTV shell navigation state machine.
//!
//! The GUI owns rendering and side effects; this module owns *where focus
//! is* and *what a key press or pointer click means*. Every interaction is
//! therefore deterministic and unit-tested without a display.
//!
//! Flow per frame (see `src/bin/catostv-shell.rs`):
//!
//! ```text
//! keys/pointer → InputMapper / Target → Shell::handle / Shell::pointer → Vec<Effect>
//!                                                          ↓
//!                         GUI performs effects (launch app, nmcli, ...)
//!                         and reports results back (on_wifi_scan, ...)
//! ```
//!
//! Screen map:
//!
//! ```text
//! Home      top bar · app row (the focused app is shown large above it)
//! Apps      top bar · app grid (APPS_GRID_COLS wide)
//! Store     top bar · sections (a list on the left, Up/Down) · the focused
//!           app's spotlight · store grid (STORE_COLS wide, Right of the list)
//! Search    top bar · on-screen keyboard · results (apps, settings, store)
//! Settings  categories · rows
//! Overlays  a stack; Back closes the top one (see [`Overlay`])
//! ```
//!
//! Typing a visible character on any non-text screen opens Search with that
//! character (type-to-search).

use crate::android;
use crate::bluetooth::Device;
use crate::display::{self, Mode};
use crate::flatpak;
use crate::input::Action;
use crate::installer::Scan;
use crate::ipc::ShellCommand;
use crate::net::WifiNetwork;
use crate::osk::{Edge, Osk, OskEvent};
use crate::power::PowerAction;
use crate::prefs;
use crate::settings::{self, Category, RowId, RowKind, ShellContext};

pub const APPS_GRID_COLS: usize = 6;
pub const STORE_COLS: usize = 5;
pub const MAX_SEARCH_RESULTS: usize = 8;
pub const VOLUME_STEP: i8 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Home,
    /// Apps running in the background, as cards with a close button.
    Recents,
    Apps,
    Store,
    Search,
    Settings,
}

/// Top navigation bar entries, left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopItem {
    Home,
    Recents,
    Apps,
    Store,
    Search,
    /// The status/clock pill; opens the Control Center. Settings lives
    /// there (no Settings tab, like an Android TV).
    Quick,
}

pub const TOP_ITEMS: [TopItem; 6] = [
    TopItem::Home,
    TopItem::Recents,
    TopItem::Apps,
    TopItem::Store,
    TopItem::Search,
    TopItem::Quick,
];

impl TopItem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Recents => "Recents",
            Self::Apps => "Apps",
            Self::Store => "Store",
            Self::Search => "Search",
            Self::Quick => "Control Center",
        }
    }

    pub fn page(self) -> Option<Page> {
        match self {
            Self::Home => Some(Page::Home),
            Self::Recents => Some(Page::Recents),
            Self::Apps => Some(Page::Apps),
            Self::Store => Some(Page::Store),
            Self::Search => Some(Page::Search),
            Self::Quick => None,
        }
    }
}

pub fn top_index_of(page: Page) -> usize {
    TOP_ITEMS
        .iter()
        .position(|t| t.page() == Some(page))
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickItem {
    /// Wi-Fi and Ethernet: OK opens the Network panel.
    Network,
    Bluetooth,
    Mute,
    Settings,
    Sleep,
    Power,
}

/// Control Center grid, row-major, [`QUICK_COLS`] wide. Below it is the
/// volume row (index `QUICK_ITEMS.len()`): Left/Right change the volume, OK
/// mutes. No separate Volume −/+ tiles.
pub const QUICK_ITEMS: [QuickItem; 6] = [
    QuickItem::Network,
    QuickItem::Bluetooth,
    QuickItem::Mute,
    QuickItem::Settings,
    QuickItem::Sleep,
    QuickItem::Power,
];
pub const QUICK_COLS: usize = 2;

/// What can be done with a KDE Connect device (its options sheet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KdeOption {
    Accept,
    Decline,
    Pair,
    /// Make the phone ring / show a notification ("find my phone").
    Ping,
    Unpair,
}

impl KdeOption {
    pub fn label(self) -> &'static str {
        match self {
            KdeOption::Accept => "Accept pairing",
            KdeOption::Decline => "Decline",
            KdeOption::Pair => "Pair",
            KdeOption::Ping => "Ring phone",
            KdeOption::Unpair => "Unpair",
        }
    }
}

/// The options for a device, by its state.
pub fn kde_options(d: &crate::kdeconnect::KdeDevice) -> Vec<KdeOption> {
    if d.pair_requested_by_peer {
        vec![KdeOption::Accept, KdeOption::Decline]
    } else if d.paired && d.reachable {
        vec![KdeOption::Ping, KdeOption::Unpair]
    } else if d.paired {
        vec![KdeOption::Unpair]
    } else {
        vec![KdeOption::Pair]
    }
}

/// Rows of Quick settings → Network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetRow {
    WifiSwitch,
    /// Shown when the TV has an Ethernet port; opens its details.
    Ethernet,
    /// Index into the Wi-Fi scan results.
    Wifi(usize),
    ScanAgain,
}

/// Rows of Quick settings → Bluetooth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtRow {
    Switch,
    /// Index into `SystemSnapshot::bt_devices` (paired devices).
    Device(usize),
    Pair,
}

/// A paired device is connected now (from `bt_details`).
pub fn bt_connected(sys: &crate::settings::SystemSnapshot, mac: &str) -> bool {
    sys.bt_details
        .iter()
        .any(|d| d.address == mac && d.connected)
}

/// A remote, keyboard or controller: listed under Navigation & controllers
/// only, not with the other Bluetooth devices.
pub fn bt_is_input(sys: &crate::settings::SystemSnapshot, mac: &str) -> bool {
    sys.bt_details
        .iter()
        .any(|d| d.address == mac && d.is_input())
}

/// A device the user added that BlueZ no longer has as paired (see
/// `bluetooth::known_devices`): it has to be paired again.
pub fn bt_unpaired(sys: &crate::settings::SystemSnapshot, mac: &str) -> bool {
    sys.bt_details.iter().any(|d| d.address == mac && !d.paired)
}

/// Connect a known device, or pair it again first when it lost its pairing
/// (the device has to be in pairing mode for that).
fn bt_connect_effect(sys: &crate::settings::SystemSnapshot, mac: String) -> Effect {
    if bt_unpaired(sys, &mac) {
        Effect::BtPair(mac)
    } else {
        Effect::BtConnect(mac)
    }
}

/// "Connected", with the battery level when the device reports one.
pub fn bt_connected_label(sys: &crate::settings::SystemSnapshot, mac: &str) -> String {
    match sys
        .bt_details
        .iter()
        .find(|d| d.address == mac)
        .and_then(|d| d.battery)
    {
        Some(b) => format!("Connected · {b}%"),
        None => "Connected".into(),
    }
}

/// The rows of Quick settings → Bluetooth: the switch, then (when on) the
/// paired devices and "Pair a new device".
pub fn quick_bt_rows(ctx: &ShellContext) -> Vec<BtRow> {
    let mut v = vec![BtRow::Switch];
    if ctx.sys.bluetooth.powered {
        v.extend(
            (0..ctx.sys.bt_devices.len())
                .filter(|&i| !bt_is_input(ctx.sys, &ctx.sys.bt_devices[i].address))
                .map(BtRow::Device),
        );
        v.push(BtRow::Pair);
    }
    v
}

/// An editable Ethernet field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EthField {
    Address,
    Gateway,
    Dns,
}

impl EthField {
    pub fn label(self) -> &'static str {
        match self {
            EthField::Address => "IP address",
            EthField::Gateway => "Gateway",
            EthField::Dns => "DNS servers",
        }
    }

    pub fn value(self, c: &crate::net::IpConfig) -> &str {
        match self {
            EthField::Address => &c.address,
            EthField::Gateway => &c.gateway,
            EthField::Dns => &c.dns,
        }
    }

    pub fn set(self, c: &mut crate::net::IpConfig, v: String) {
        match self {
            EthField::Address => c.address = crate::net::cidr(&v).unwrap_or(v),
            EthField::Gateway => c.gateway = v,
            EthField::Dns => c.dns = v,
        }
    }

    /// A typed value this field accepts (the gateway may be empty).
    pub fn check(self, v: &str) -> std::result::Result<(), &'static str> {
        let ok = match self {
            EthField::Address => crate::net::cidr(v).is_some(),
            EthField::Gateway => v.is_empty() || crate::net::is_ipv4(v),
            EthField::Dns => crate::net::dns_list(v).is_some(),
        };
        if ok {
            Ok(())
        } else {
            Err(match self {
                EthField::Address => "Enter an address like 192.168.1.50/24",
                EthField::Gateway => "Enter an address like 192.168.1.1",
                EthField::Dns => "Enter addresses like 1.1.1.1 8.8.8.8",
            })
        }
    }
}

/// Focusable rows of the Ethernet panel: the IP method, the fields while
/// it is manual, and Save once something changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EthRow {
    Method,
    Field(EthField),
    Save,
}

/// Controls for the app behind Quick settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickAppItem {
    BackToApp,
    Fullscreen,
    /// Mouse mode for this app (`crate::cursor`): not for Android apps.
    Mouse,
    Close,
}

/// The app behind Quick settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickApp {
    pub name: String,
    pub fullscreen: bool,
    /// Mouse mode is on in this app right now; `None`: not offered
    /// (Android).
    pub mouse: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerItem {
    Sleep,
    Restart,
    PowerOff,
    Cancel,
}

pub const POWER_ITEMS: [PowerItem; 4] = [
    PowerItem::Sleep,
    PowerItem::Restart,
    PowerItem::PowerOff,
    PowerItem::Cancel,
];

impl PowerItem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sleep => "Sleep",
            Self::Restart => "Restart",
            Self::PowerOff => "Power off",
            Self::Cancel => "Cancel",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppOption {
    Open,
    /// Pick the app up to move it along Home's row.
    Move,
    Resume,
    Restart,
    Close,
    Uninstall,
    Cancel,
}

impl AppOption {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Move => "Move",
            Self::Resume => "Resume",
            Self::Restart => "Restart",
            Self::Close => "Close",
            Self::Uninstall => "Uninstall",
            Self::Cancel => "Cancel",
        }
    }
}

/// Options offered for an app. `removable`: a Flatpak or Android app that
/// CatosTV can uninstall. `movable`: opened from Home's row.
pub fn app_options(running: bool, removable: bool, movable: bool) -> Vec<AppOption> {
    let mut v = if running {
        vec![AppOption::Resume, AppOption::Restart, AppOption::Close]
    } else {
        vec![AppOption::Open]
    };
    if movable {
        v.push(AppOption::Move);
    }
    if removable && !running {
        v.push(AppOption::Uninstall);
    }
    v.push(AppOption::Cancel);
    v
}

/// True for apps CatosTV can uninstall (Flatpak / Android).
pub fn is_removable(ctx: &ShellContext, app: usize) -> bool {
    ctx.apps
        .get(app)
        .is_some_and(|a| a.entry.source != crate::apps::AppSource::Configured)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtOption {
    Connect,
    Disconnect,
    Forget,
    Cancel,
}

pub const BT_OPTIONS: [BtOption; 4] = [
    BtOption::Connect,
    BtOption::Disconnect,
    BtOption::Forget,
    BtOption::Cancel,
];

impl BtOption {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connect => "Connect",
            Self::Disconnect => "Disconnect",
            Self::Forget => "Forget",
            Self::Cancel => "Cancel",
        }
    }
}

// ----------------------------------------------------------------- store

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreCat {
    Featured,
    Watch,
    Listen,
    Play,
    Stream,
    Tools,
    /// Running and finished installs of this session.
    Downloads,
    Installed,
    Search,
}

pub const STORE_CATS: [StoreCat; 9] = [
    StoreCat::Featured,
    StoreCat::Watch,
    StoreCat::Listen,
    StoreCat::Play,
    StoreCat::Stream,
    StoreCat::Tools,
    StoreCat::Downloads,
    StoreCat::Installed,
    StoreCat::Search,
];

impl StoreCat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Featured => "Featured",
            Self::Watch => "Watch",
            Self::Listen => "Listen",
            Self::Play => "Play",
            Self::Stream => "Stream",
            Self::Tools => "Tools",
            Self::Downloads => "Downloads",
            Self::Installed => "Installed",
            Self::Search => "Search",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Featured => "spark",
            Self::Watch => "play",
            Self::Listen => "music",
            Self::Play => "game",
            Self::Stream => "stream",
            Self::Tools => "gear",
            Self::Downloads => "download",
            Self::Installed => "check",
            Self::Search => "search",
        }
    }
}

/// Appstream details for the full-screen Store page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Details {
    Loading,
    Ready(flatpak::AppDetails),
    Failed(String),
}

/// One app shown in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreItem {
    /// Flathub app id.
    pub id: String,
    pub name: String,
    pub blurb: String,
}

impl StoreItem {
    fn from_curated(c: &flatpak::CuratedApp) -> Self {
        Self {
            id: c.id.into(),
            name: c.name.into(),
            blurb: c.blurb.into(),
        }
    }
}

/// A running store transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreJob {
    /// Installing; fraction when known.
    Installing(Option<f32>),
    Removing,
}

/// One line of the Store's Downloads section: an app this session
/// installed or removed (or is doing so), newest first.
#[derive(Debug, Clone, PartialEq)]
pub struct StoreLogEntry {
    pub item: StoreItem,
    pub state: StoreLogState,
    /// System lists seen since the job finished (see `settle_store_log`).
    pub snapshots: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StoreLogState {
    Installing,
    Removing,
    /// Installed (the tile says so until the system list catches up).
    Installed,
    Removed,
    /// Finished and confirmed by the system list: that list decides now.
    Done,
    Failed(String),
}

/// Buttons of an app's Store page (Back leaves the page; while an app
/// installs or is removed there are none).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreAction {
    Install,
    Open,
    Remove,
}

impl StoreAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Install => "Install",
            Self::Open => "Open",
            Self::Remove => "Remove",
        }
    }
}

/// How a Store app works on the TV, for the apps CatosTV knows
/// (`flatpak::CURATED`); `None` for the rest.
pub fn remote_fit(id: &str) -> Option<&'static str> {
    let app = flatpak::CURATED.iter().find(|a| a.id == id)?;
    Some(if flatpak::NEEDS_MOUSE.contains(&id) {
        "The remote works as a mouse"
    } else if app.category == "Play" || app.category == "Stream" {
        "Best with a game controller"
    } else {
        "Works with the remote"
    })
}

/// Buttons of the store detail panel.
pub fn store_actions(
    jobs: &[(String, StoreJob)],
    item: &StoreItem,
    ctx: &ShellContext,
) -> Vec<StoreAction> {
    store_actions_installed(jobs, item, ctx.sys.flatpaks.iter().any(|f| f.id == item.id))
}

fn installed_per_log(log: &[StoreLogEntry], ctx: &ShellContext, id: &str) -> bool {
    match log.iter().find(|e| e.item.id == id) {
        Some(e) if e.state == StoreLogState::Installed => true,
        Some(e) if e.state == StoreLogState::Removed => false,
        _ => ctx.sys.flatpaks.iter().any(|f| f.id == id),
    }
}

fn log_store(log: &mut Vec<StoreLogEntry>, item: StoreItem, state: StoreLogState) {
    log.retain(|e| e.item.id != item.id);
    log.insert(
        0,
        StoreLogEntry {
            item,
            state,
            snapshots: 0,
        },
    );
}

pub fn store_actions_installed(
    jobs: &[(String, StoreJob)],
    item: &StoreItem,
    installed: bool,
) -> Vec<StoreAction> {
    if jobs.iter().any(|(i, _)| *i == item.id) {
        return Vec::new();
    }
    if installed {
        vec![StoreAction::Open, StoreAction::Remove]
    } else {
        vec![StoreAction::Install]
    }
}

// --------------------------------------------------------------- display

/// A display mode applied but not yet confirmed by the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMode {
    pub output: String,
    /// What to go back to; `None` → the display's preferred mode.
    pub previous: Option<Mode>,
    pub new: Mode,
}

/// Seconds the user has to confirm a new display mode.
pub const DISPLAY_CONFIRM_SECS: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmItem {
    Keep,
    Revert,
}
pub const CONFIRM_ITEMS: [ConfirmItem; 2] = [ConfirmItem::Keep, ConfirmItem::Revert];

// -------------------------------------------------------------- overlays

#[derive(Debug, Clone)]
pub enum Overlay {
    Quick {
        index: usize,
    },
    AppOptions {
        app: usize,
        index: usize,
        /// From Home's row: offers Move.
        movable: bool,
    },
    Power {
        index: usize,
    },
    WifiPassword {
        ssid: String,
        osk: Box<Osk>,
    },
    /// Options of KDE Connect device `device` (index into `sys.kde`).
    KdeDevice {
        device: usize,
        index: usize,
    },
    /// Quick settings → Network: Wi-Fi switch, Ethernet, networks.
    QuickNetwork {
        index: usize,
    },
    /// Quick settings → Bluetooth: switch, paired devices, pair.
    QuickBluetooth {
        index: usize,
    },
    /// Ethernet details and IP settings, from Quick settings or
    /// Settings → Network; the draft is `Shell::eth_draft`.
    Ethernet {
        index: usize,
    },
    /// Typing one Ethernet field (address, gateway or DNS).
    EthField {
        field: EthField,
        osk: Box<Osk>,
    },
    /// Settings → Debug → Change password: typed twice (`first` holds the
    /// first entry while the second is typed).
    DebugPassword {
        osk: Box<Osk>,
        first: Option<String>,
    },
    /// Nearby Bluetooth devices; `len` is "Search again".
    BtScan {
        index: usize,
    },
    /// Options for paired device `device` (index into `sys.bt_devices`).
    BtDevice {
        device: usize,
        index: usize,
    },
    /// Resolution list (`display::menu_modes` of the managed output).
    DisplayModes {
        index: usize,
    },
    /// "Keep this resolution?" with a countdown run by the GUI.
    /// "<phone> wants to pair", Accept (0) / Decline (1).
    KdeRequest {
        id: String,
        name: String,
        index: usize,
    },
    DisplayConfirm {
        index: usize,
    },
    /// "Press the controller button for <action>"; inputd is in
    /// capture mode while this is open.
    PadCapture {
        action: crate::gamepad::PadOut,
    },
    /// Google Play certification instructions with the device id.
    Certify,
    /// "Which app store?" before the Android TV setup: two cards,
    /// Google Play (0) / Aurora Store (1); OK picks and starts the setup.
    StoreChoice {
        index: usize,
    },
    /// "Reset Android?" before Reset Android: two buttons,
    /// Cancel (0, the default) / Reset Android (1); OK on the second resets.
    RepairConfirm {
        index: usize,
    },
    AudioOutputs {
        index: usize,
    },
    StoreApp {
        item: StoreItem,
        index: usize,
    },
    StoreSearch {
        osk: Box<Osk>,
    },
    /// Settings → Apps: typing filters the list live.
    AppsSearch {
        osk: Box<Osk>,
    },
    KeyGuide,
    /// APKs found on USB drives; `len` is "Search again".
    ApkList {
        index: usize,
    },
}

impl Overlay {
    /// The list/button focus index, for overlays that have one.
    pub fn index_mut(&mut self) -> Option<&mut usize> {
        match self {
            Overlay::Quick { index }
            | Overlay::AppOptions { index, .. }
            | Overlay::Power { index }
            | Overlay::BtScan { index }
            | Overlay::BtDevice { index, .. }
            | Overlay::DisplayModes { index }
            | Overlay::DisplayConfirm { index }
            | Overlay::KdeRequest { index, .. }
            | Overlay::AudioOutputs { index }
            | Overlay::StoreApp { index, .. }
            | Overlay::StoreChoice { index }
            | Overlay::RepairConfirm { index }
            | Overlay::ApkList { index }
            | Overlay::KdeDevice { index, .. }
            | Overlay::QuickNetwork { index }
            | Overlay::QuickBluetooth { index }
            | Overlay::Ethernet { index } => Some(index),
            Overlay::EthField { .. }
            | Overlay::WifiPassword { .. }
            | Overlay::DebugPassword { .. }
            | Overlay::StoreSearch { .. }
            | Overlay::AppsSearch { .. }
            | Overlay::PadCapture { .. }
            | Overlay::Certify
            | Overlay::KeyGuide => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFocus {
    Keyboard,
    Results,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SearchHit {
    App(usize),
    Setting {
        category: Category,
        row: RowId,
        title: String,
    },
    /// A store app that is not installed yet.
    Store(StoreItem),
    /// A movie or show on the Jellyfin server (Catfyn).
    Media(Box<crate::jellyfin::Item>),
}

/// What the pointer is over. The GUI registers a target for every
/// clickable rectangle while drawing and reports hover/click here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Top(usize),
    Rail(usize),
    Grid(usize),
    StoreCat(usize),
    StoreItem(usize),
    SearchKey(usize, usize),
    SearchHit(usize),
    SettingsCat(usize),
    SettingsRow(usize),
    /// Recents: card `i` (an index into the running apps) …
    RecentCard(usize),
    /// … its close (X) button …
    RecentClose(usize),
    /// … and "Close all".
    RecentCloseAll,
    /// Item `i` of the top overlay's list or button row.
    Item(usize),
    /// A key of the top overlay's on-screen keyboard.
    OverlayKey(usize, usize),
    /// The dimmed area around an overlay: a click closes it.
    Outside,
    /// Something in Catfyn: (row, column), meaning per screen
    /// (`fin::Fin::focus`).
    Fin(usize, usize),
    /// A row of the Files app.
    Files(usize),
    /// A place (left pane) of the Files app.
    FilesPlace(usize),
    /// Home's media rows: tile `col` of row `row`.
    HomeMedia(usize, usize),
}

/// Side effects requested by the state machine. The GUI performs them.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Start the app (index into `ctx.apps`).
    Launch(usize),
    /// Home: the picked-up app swapped places with its neighbour; keep the
    /// new order.
    MoveApp {
        from: usize,
        to: usize,
    },
    /// Bring the running app to the front.
    Resume(usize),
    Close(usize),
    /// Close, then launch again once it has exited.
    Restart(usize),
    /// Remove a Flatpak / Android app (index into `ctx.apps`).
    Uninstall(usize),
    SetWifi(bool),
    SetBluetooth(bool),
    VolumeStep(i8),
    ToggleMute,
    Power(PowerAction),
    /// Exit the shell process; the session restarts it.
    RestartShell,
    /// Settings → Updates: look for an update.
    CheckUpdates,
    /// Install the update the last check found (the GUI refuses while
    /// something plays).
    InstallUpdate,
    /// Apply IPv4 settings to the wired connection.
    SetEthernetIp {
        connection: String,
        config: crate::net::IpConfig,
    },
    /// Settings → Debug: SSH on/off until the next boot.
    DebugSsh(bool),
    /// Set the `catostv` login's password.
    DebugPassword(String),
    /// Remove the old `ok` development login.
    DebugRemoveOld,
    ScanWifi,
    ConnectWifi {
        ssid: String,
        password: Option<String>,
    },
    ReloadApps,
    /// Escape gesture from `catostv-inputd`: if an app is in front, show
    /// CatosTV Home (the app keeps running); otherwise do nothing.
    LeaveApp,
    BtScan,
    BtPair(String),
    BtConnect(String),
    BtDisconnect(String),
    BtForget(String),
    SetMode {
        output: String,
        mode: Mode,
    },
    /// Go back to `mode` (`None` → the display's preferred mode).
    RevertMode {
        output: String,
        mode: Option<Mode>,
    },
    /// The user kept a new mode: persist it.
    KeepMode {
        output: String,
        mode: Mode,
    },
    /// Interface scale in percent (125 = 1.25×).
    SetScale {
        output: String,
        percent: u32,
    },
    /// Picture fit (overscan margin) in percent.
    SetSafeArea(u8),
    SetAdaptiveSync {
        output: String,
        on: bool,
    },
    SetSink(u32),
    ApplyHdmiProfile,
    SetAccent(String),
    SetClock24(bool),
    /// Settings → Catfyn.
    SetFinAutoplay(bool),
    SetFinCountdown(u32),
    SetScreensaver(bool),
    SetSaverPictures(String),
    /// Settings → Customization → Home background (a `prefs` key).
    SetHomeBackground(String),
    SetSaverMinutes(u32),
    /// Show the screensaver now (Sleep).
    Screensaver,
    /// Controllers navigate CatosTV (read by catostv-inputd).
    SetPadNav(bool),
    /// Nintendo controller layout (right button = OK).
    /// Button layout of one controller (by device name).
    SetPadLayout {
        device: String,
        nintendo: bool,
    },
    /// Experimental HDR on/off: saved, then the compositor restarts.
    SetHdr(bool),
    /// HDR came up fine: keep it (until the next reboot).
    KeepHdr,
    /// Back to the app that was in front (Quick settings closed).
    ReturnToApp,
    /// Close / minimise / (un)fullscreen the app behind Quick settings
    ///.
    AppWindow(crate::toplevel::WindowAction),
    /// Mouse mode for the app in front, on or off (kept for that app).
    AppMouse(bool),
    /// Open Catfyn (its Home tile).
    OpenFin,
    /// Open Catfyn on an item's details (Options on a Home media tile).
    FinDetails(crate::jellyfin::Item),
    /// Play from CatosTV Home's "Continue watching" row (home account).
    FinPlayHome(crate::jellyfin::Item),
    /// Something Catfyn asked for.
    Fin(crate::fin::FinEffect),
    /// Open the built-in Files app.
    OpenFiles,
    /// inputd capture mode on/off (Controller navigation).
    PadCapture(bool),
    /// "gapps" or "vanilla" for the Android setup.
    SetAndroidVariant(String),
    /// Search the Jellyfin server for this (lower-cased) query.
    SearchMedia(String),
    /// Bind a controller button to a CatosTV action.
    SetPadBinding(crate::gamepad::PadOut, u16),
    ClearPadBindings,
    Files(crate::files::FilesEffect),
    /// Decline a KDE Connect pairing request.
    KdeReject(String),
    KdePing(String),
    KdeUnpair(String),
    StoreSearch(String),
    StoreInstall(String),
    StoreUninstall(String),
    /// Launch the installed Flatpak `id`.
    StoreOpen(String),
    UpdateApps,
    /// Load appstream details (and screenshots) for the Store page.
    StoreDetails(String),
    AndroidSetup,
    /// Restart Android with a fresh keystore.
    AndroidRepair,
    /// List a folder for the APK picker (`None` = the starting places,
    /// after mounting removable drives).
    BrowseApks(Option<String>),
    InstallApk(String),
    AndroidOpenStore,
    KdeAccept(String),
    KdePair(String),
    /// The key guide was closed: remember it was seen.
    KeyGuideSeen,
    Toast(String),
}

#[derive(Debug, Clone)]
pub struct Shell {
    pub page: Page,
    /// Focus is in the top bar (Home/Apps/Store/Search pages).
    pub top_focus: bool,
    pub top_index: usize,
    pub rail_index: usize,
    /// The app at `rail_index` is picked up and moves along Home's row.
    pub moving: bool,
    pub grid_index: usize,
    pub store_cat: usize,
    /// Focus is in the store grid (else in the category list).
    pub store_grid: bool,
    pub store_index: usize,
    pub store_query: String,
    pub store_results: Scan<StoreItem>,
    /// Details for the open Store page: (app id, state).
    pub store_details: Option<(String, Details)>,
    pub store_jobs: Vec<(String, StoreJob)>,
    /// Downloads section: what this session installed / removed.
    pub store_log: Vec<StoreLogEntry>,
    pub settings_cat: usize,
    pub settings_row: usize,
    pub settings_rows_focus: bool,
    /// Settings → Customization: the swatch the selection ring is on, before
    /// OK picks it (`None`: on the current colour).
    pub accent_pick: Option<usize>,
    /// The row being changed with Left/Right (category, row): OK on a
    /// slider, a choice or the accent colours starts it, OK or Back ends it.
    /// Until then Left/Right only move, so arriving on a row never changes it.
    pub settings_editing: Option<(usize, usize)>,
    pub search_osk: Osk,
    pub search_focus: SearchFocus,
    pub search_index: usize,
    /// Recents: focused card (index into [`ShellContext::running`]); the
    /// card count itself means "Close all".
    pub recents_index: usize,
    /// Recents: the focused card's close (X) button is focused.
    pub recents_close: bool,
    /// The card to return to from "Close all" (Up).
    pub recents_card: usize,
    /// Quick settings were opened from inside an app: closing them with
    /// Back goes back into that app, and a row of controls for
    /// that app sits at the top: back, full screen, close.
    pub quick_over_app: bool,
    /// The volume panel, on top of everything while it shows.
    pub volume: Option<crate::volume_panel::VolumePanel>,
    /// The app behind Quick settings.
    pub quick_app: Option<QuickApp>,
    /// KDE Connect pairing requests already shown as a pop-up.
    pub kde_prompted: Vec<String>,
    /// Catfyn, while it is open (drawn full screen over the page).
    pub fin: Option<crate::fin::Fin>,
    /// The Files app while open (full screen, like Catfyn).
    pub files: Option<crate::files::Files>,
    /// Catfyn results for the Search page: (query, results).
    pub search_media: (String, Vec<crate::jellyfin::Item>),
    /// Listings asked for by pointer moves (drained by `handle`).
    files_pending: Vec<crate::files::FilesEffect>,
    /// Home: focus is in the media rows under the apps: (row, column).
    pub home_media: Option<(usize, usize)>,
    /// Settings → Apps filter text.
    pub apps_filter: String,
    pub overlays: Vec<Overlay>,
    pub wifi_scan: Scan<WifiNetwork>,
    /// SSID currently being joined.
    pub wifi_connecting: Option<String>,
    /// Ethernet IP settings being edited.
    pub eth_draft: Option<crate::net::IpConfig>,
    pub bt_scan: Scan<Device>,
    /// Address of the device currently being paired.
    pub bt_busy: Option<String>,
    /// Debug SSH is being switched on (`true`) or off; starting the SSH
    /// service takes a few seconds.
    pub debug_ssh_busy: Option<bool>,
    pub display_pending: Option<PendingMode>,
    /// The confirm dialog is about HDR (just switched on), not a mode.
    pub hdr_pending: bool,
    /// APK picker: the current folder's entries.
    pub apks: Scan<android::FileEntry>,
    /// APK picker: folders entered, each with the row it was opened from
    /// (Back returns there). Empty = the starting places.
    pub apk_stack: Vec<(String, usize)>,
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}

/// Move `i` by `d` within `0..len` (clamped; `len == 0` → 0).
/// Sleep: the screensaver while it is on, else real sleep.
fn sleep_effect(ctx: &ShellContext) -> Effect {
    if ctx.prefs.screensaver {
        Effect::Screensaver
    } else {
        Effect::Power(PowerAction::Suspend)
    }
}

fn step(i: usize, d: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (i as isize + d).clamp(0, len as isize - 1) as usize
}

/// 2-D grid move. Returns `None` when the move leaves the grid upwards
/// (the caller decides what is above), otherwise the new index.
pub fn grid_move(i: usize, action: Action, cols: usize, n: usize) -> Option<usize> {
    if n == 0 {
        return if action == Action::Up { None } else { Some(0) };
    }
    Some(match action {
        Action::Left if !i.is_multiple_of(cols) => i - 1,
        Action::Right if i % cols < cols - 1 && i + 1 < n => i + 1,
        Action::Up if i >= cols => i - cols,
        Action::Up => return None,
        Action::Down if i + cols < n => i + cols,
        // Last partial row: Down from above moves to the last item.
        Action::Down if i / cols < (n - 1) / cols => n - 1,
        _ => i,
    })
}

impl Shell {
    pub fn new() -> Self {
        Self {
            page: Page::Home,
            top_focus: false,
            top_index: 0,
            rail_index: 0,
            moving: false,
            grid_index: 0,
            store_cat: 0,
            store_grid: false,
            store_index: 0,
            store_query: String::new(),
            store_details: None,
            store_results: Scan::Idle,
            store_jobs: Vec::new(),
            store_log: Vec::new(),
            settings_cat: 0,
            settings_row: 0,
            settings_rows_focus: false,
            accent_pick: None,
            settings_editing: None,
            search_osk: Osk::new(false, "Search"),
            search_focus: SearchFocus::Keyboard,
            search_index: 0,
            recents_index: 0,
            recents_close: false,
            recents_card: 0,
            quick_over_app: false,
            volume: None,
            quick_app: None,
            kde_prompted: Vec::new(),
            fin: None,
            files: None,
            files_pending: Vec::new(),
            search_media: (String::new(), Vec::new()),
            home_media: None,
            apps_filter: String::new(),
            overlays: Vec::new(),
            wifi_scan: Scan::Idle,
            wifi_connecting: None,
            eth_draft: None,
            bt_scan: Scan::Idle,
            bt_busy: None,
            debug_ssh_busy: None,
            display_pending: None,
            hdr_pending: false,
            apks: Scan::Idle,
            apk_stack: Vec::new(),
        }
    }

    /// The rows of the current Settings category (Apps filtered).
    pub fn settings_rows(&self, ctx: &ShellContext) -> Vec<settings::Row> {
        let mut rows = settings::rows_for(self.current_category(), ctx, &self.apps_filter);
        // Wi-Fi on: the networks nearby are right there to pick.
        if self.current_category() == Category::Network && ctx.sys.wifi_on == Some(true) {
            rows.extend(settings::wifi_rows(
                &self.wifi_scan,
                self.wifi_connecting.as_deref(),
            ));
        }
        if let Some(on) = self.debug_ssh_busy
            && let Some(r) = rows.iter_mut().find(|r| r.id == RowId::DebugSsh)
        {
            r.subtitle = if on {
                "Turning on…"
            } else {
                "Turning off…"
            }
            .into();
            r.kind = RowKind::Toggle(Some(on));
            r.busy = true;
        }
        rows
    }

    pub fn overlay(&self) -> Option<&Overlay> {
        self.overlays.last()
    }

    /// True while an on-screen keyboard has focus (typed characters and
    /// Backspace edit text instead of navigating).
    pub fn text_mode(&self) -> bool {
        if self.volume.is_some() {
            return false;
        }
        if self.overlay().is_none()
            && let Some(f) = &self.fin
        {
            return f.text_mode();
        }
        if self.overlay().is_none()
            && let Some(f) = &self.files
        {
            return f.text_mode();
        }
        match self.overlay() {
            Some(
                Overlay::WifiPassword { .. }
                | Overlay::DebugPassword { .. }
                | Overlay::EthField { .. }
                | Overlay::StoreSearch { .. }
                | Overlay::AppsSearch { .. },
            ) => true,
            Some(_) => false,
            None => {
                self.page == Page::Search
                    && !self.top_focus
                    && self.search_focus == SearchFocus::Keyboard
            }
        }
    }

    pub fn current_category(&self) -> Category {
        Category::ALL[self.settings_cat.min(Category::ALL.len() - 1)]
    }

    pub fn current_store_cat(&self) -> StoreCat {
        STORE_CATS[self.store_cat.min(STORE_CATS.len() - 1)]
    }

    /// Open Quick settings over the current page (not Home:).
    /// `over_app`: opened from inside an app, so Back returns to it.
    /// Number of app-control buttons at the top of Quick settings (0 when
    /// not opened from an app).
    pub fn quick_offset(&self) -> usize {
        if self.quick_over_app {
            self.quick_app_items().len()
        } else {
            0
        }
    }

    /// The app controls, in order: Back · Full screen · Mouse · Close
    /// (no Mouse for Android apps).
    pub fn quick_app_items(&self) -> Vec<QuickAppItem> {
        let mouse = self.quick_app.as_ref().is_some_and(|a| a.mouse.is_some());
        [
            Some(QuickAppItem::BackToApp),
            Some(QuickAppItem::Fullscreen),
            mouse.then_some(QuickAppItem::Mouse),
            Some(QuickAppItem::Close),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    pub fn open_quick(&mut self, over_app: bool) {
        self.overlays
            .retain(|o| !matches!(o, Overlay::Quick { .. }));
        self.overlays.push(Overlay::Quick { index: 0 });
        self.quick_over_app = over_app;
    }

    pub fn go_home(&mut self) {
        self.quick_over_app = false;
        self.volume = None;
        self.fin = None;
        self.files = None;
        self.home_media = None;
        self.overlays.clear();
        self.page = Page::Home;
        self.top_focus = false;
        self.top_index = 0;
    }

    fn open_page(&mut self, page: Page) {
        self.overlays.clear();
        self.fin = None;
        self.files = None;
        self.home_media = None;
        self.page = page;
        self.top_focus = false;
        self.top_index = top_index_of(page);
        match page {
            Page::Search => self.search_focus = SearchFocus::Keyboard,
            // Settings opens on its category list, at the top.
            Page::Settings => {
                self.settings_rows_focus = false;
                self.settings_cat = 0;
                self.settings_row = 0;
                self.settings_editing = None;
            }
            Page::Store => self.store_grid = false,
            Page::Recents => {
                self.recents_index = 0;
                self.recents_close = false;
                self.recents_card = 0;
            }
            Page::Home | Page::Apps => {}
        }
    }

    /// Show the key guide (first boot, or Settings → Remote & keys).
    pub fn show_key_guide(&mut self) {
        if !matches!(self.overlay(), Some(Overlay::KeyGuide)) {
            self.overlays.push(Overlay::KeyGuide);
        }
    }

    /// Number of selectable entries in an overlay (for clamping/pointer).
    pub fn overlay_len(&self, o: &Overlay, ctx: &ShellContext) -> usize {
        match o {
            Overlay::Quick { .. } => self.quick_offset() + QUICK_ITEMS.len() + 1,
            Overlay::AppOptions { app, movable, .. } => {
                let running = ctx.apps.get(*app).is_some_and(|a| a.running);
                app_options(running, is_removable(ctx, *app), *movable).len()
            }
            Overlay::Power { .. } => POWER_ITEMS.len(),
            Overlay::BtScan { .. } => self.bt_scan.items().len() + 1,
            Overlay::BtDevice { .. } => BT_OPTIONS.len(),
            Overlay::DisplayModes { .. } => self.menu_modes(ctx).len(),
            Overlay::DisplayConfirm { .. } | Overlay::KdeRequest { .. } => 2,
            Overlay::AudioOutputs { .. } => ctx.sys.sinks.len(),
            Overlay::StoreApp { item, .. } => {
                self.actions_for(item, ctx).len() + self.store_shots(&item.id).len()
            }
            // While a folder loads, keep the row index (Back restores it).
            Overlay::ApkList { .. } if matches!(self.apks, Scan::Busy) => usize::MAX,
            Overlay::ApkList { .. } => self.apks.items().len().max(1),
            Overlay::StoreChoice { .. } | Overlay::RepairConfirm { .. } => 2,
            Overlay::WifiPassword { .. }
            | Overlay::DebugPassword { .. }
            | Overlay::EthField { .. }
            | Overlay::StoreSearch { .. }
            | Overlay::AppsSearch { .. }
            | Overlay::PadCapture { .. }
            | Overlay::Certify
            | Overlay::KeyGuide => 1,
            Overlay::QuickNetwork { .. } => self.quick_net_rows(ctx).len(),
            Overlay::KdeDevice { device, .. } => {
                ctx.sys.kde.get(*device).map_or(1, |d| kde_options(d).len())
            }
            Overlay::QuickBluetooth { .. } => quick_bt_rows(ctx).len(),
            Overlay::Ethernet { .. } => self.eth_rows().len(),
        }
    }

    pub fn menu_modes(&self, ctx: &ShellContext) -> Vec<Mode> {
        ctx.output().map(display::menu_modes).unwrap_or_default()
    }

    /// Buttons of the store page for `item` (install log considered).
    pub fn actions_for(&self, item: &StoreItem, ctx: &ShellContext) -> Vec<StoreAction> {
        store_actions_installed(&self.store_jobs, item, self.store_installed(ctx, &item.id))
    }

    pub fn store_job(&self, id: &str) -> Option<&StoreJob> {
        self.store_jobs
            .iter()
            .find(|(i, _)| i == id)
            .map(|(_, j)| j)
    }

    /// Store items for the current category.
    pub fn store_items(&self, ctx: &ShellContext) -> Vec<StoreItem> {
        match self.current_store_cat() {
            StoreCat::Featured => flatpak::CURATED
                .iter()
                .map(StoreItem::from_curated)
                .collect(),
            StoreCat::Downloads => self.store_log.iter().map(|e| e.item.clone()).collect(),
            StoreCat::Installed => ctx
                .sys
                .flatpaks
                .iter()
                .map(|f| StoreItem {
                    id: f.id.clone(),
                    name: f.name.clone(),
                    blurb: String::new(),
                })
                .collect(),
            StoreCat::Search => self.store_results.items().to_vec(),
            cat => flatpak::curated_in(cat.label())
                .into_iter()
                .map(StoreItem::from_curated)
                .collect(),
        }
    }

    /// Keep every index inside its list after apps/system state changed.
    pub fn clamp(&mut self, ctx: &ShellContext) {
        let n_apps = ctx.apps.len();
        self.rail_index = self.rail_index.min(n_apps.saturating_sub(1));
        // A picked-up app is put down when Home is left or covered.
        if self.page != Page::Home || self.top_focus || n_apps == 0 || !self.overlays.is_empty() {
            self.moving = false;
        }
        self.grid_index = self.grid_index.min(n_apps.saturating_sub(1));
        let rows = self.settings_rows(ctx);
        self.settings_row = self.settings_row.min(rows.len().saturating_sub(1));
        // Never resting on a heading: the row after it, else before it.
        if rows
            .get(self.settings_row)
            .is_some_and(|r| r.kind == RowKind::Heading)
        {
            let after = rows[self.settings_row..]
                .iter()
                .position(|r| r.kind != RowKind::Heading)
                .map(|k| self.settings_row + k);
            let before = rows[..self.settings_row]
                .iter()
                .rposition(|r| r.kind != RowKind::Heading);
            self.settings_row = after.or(before).unwrap_or(0);
        }
        let n_hits = self.search_results(ctx).len();
        if n_hits == 0 {
            self.search_focus = SearchFocus::Keyboard;
        }
        self.search_index = self.search_index.min(n_hits.saturating_sub(1));
        let n_recent = ctx.running().len();
        // n_recent itself is the "Close all" button (only when there are apps).
        self.recents_index = self.recents_index.min(n_recent);
        if self.recents_index >= n_recent {
            self.recents_close = false;
        }
        // Nothing to focus on an empty Recents page: focus stays in
        // the top bar, so Down never leaves you on a blank page.
        if n_recent == 0 && self.page == Page::Recents && !self.top_focus {
            self.top_focus = true;
            self.top_index = top_index_of(Page::Recents);
        }
        let n_store = self.store_items(ctx).len();
        self.store_index = self.store_index.min(n_store.saturating_sub(1));
        if n_store == 0 {
            self.store_grid = false;
        }
        // Drop overlays that point at things that disappeared.
        self.overlays.retain(|o| match o {
            Overlay::AppOptions { app, .. } => *app < n_apps,
            Overlay::BtDevice { device, .. } => *device < ctx.sys.bt_devices.len(),
            _ => true,
        });
        let lens: Vec<usize> = self
            .overlays
            .iter()
            .map(|o| self.overlay_len(o, ctx))
            .collect();
        for (o, len) in self.overlays.iter_mut().zip(lens) {
            if let Some(index) = o.index_mut() {
                *index = (*index).min(len.saturating_sub(1));
            }
        }
    }

    pub fn wifi_results(&self) -> &[WifiNetwork] {
        self.wifi_scan.items()
    }

    /// Search results for the current query. An empty query lists apps.
    pub fn search_results(&self, ctx: &ShellContext) -> Vec<SearchHit> {
        let q = self.search_osk.text().trim().to_lowercase();
        let mut hits: Vec<SearchHit> = ctx
            .apps
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                q.is_empty()
                    || a.entry.name.to_lowercase().contains(&q)
                    || a.entry.subtitle.to_lowercase().contains(&q)
            })
            .map(|(i, _)| SearchHit::App(i))
            .collect();
        if !q.is_empty() {
            for (category, row) in settings::all_rows(ctx) {
                if matches!(
                    row.id,
                    RowId::App(_) | RowId::BtDevice(_) | RowId::KdeDevice(_)
                ) {
                    continue; // apps are already listed; devices are noise
                }
                let hay = format!("{} {}", row.title, category.title()).to_lowercase();
                if hay.contains(&q) {
                    hits.push(SearchHit::Setting {
                        category,
                        row: row.id,
                        title: row.title,
                    });
                }
            }
            // Movies and shows from Catfyn, for the query they were fetched
            // for.
            if self.search_media.0 == q {
                for it in self.search_media.1.iter().take(6) {
                    hits.push(SearchHit::Media(Box::new(it.clone())));
                }
            }
            let installed = |id: &str| ctx.sys.flatpaks.iter().any(|f| f.id == id);
            for c in flatpak::CURATED {
                if !installed(c.id) && c.name.to_lowercase().contains(&q) {
                    hits.push(SearchHit::Store(StoreItem::from_curated(c)));
                }
            }
        }
        hits.truncate(MAX_SEARCH_RESULTS);
        hits
    }

    // ------------------------------------------------------------------
    // Input
    // ------------------------------------------------------------------

    pub fn handle(&mut self, action: Action, ctx: &ShellContext) -> Vec<Effect> {
        self.clamp(ctx);
        if action != Action::Home
            && let Some(p) = self.volume.as_mut()
        {
            use crate::volume_panel::PanelEffect;
            let mut out = Vec::new();
            for e in p.handle(action, &ctx.sys.sinks) {
                match e {
                    PanelEffect::Step(d) => out.push(Effect::VolumeStep(d)),
                    PanelEffect::ToggleMute => out.push(Effect::ToggleMute),
                    PanelEffect::SetSink(id) => out.push(Effect::SetSink(id)),
                    PanelEffect::Close => self.volume = None,
                }
            }
            return out;
        }
        if action == Action::Home {
            self.go_home();
            return vec![];
        }
        if action == Action::Quick {
            self.open_quick(false);
            return vec![];
        }
        // Type-to-search from any screen without a text field.
        if let Action::Char(c) = action
            && !self.text_mode()
        {
            if self.overlays.is_empty() && self.fin.is_none() && self.files.is_none() {
                self.open_page(Page::Search);
                self.search_osk.clear();
                self.search_osk.handle(Action::Char(c));
                self.search_index = 0;
            }
            return vec![];
        }
        let effects = if self.overlays.is_empty()
            && let Some(f) = self.fin.as_mut()
        {
            let mut out = Vec::new();
            for e in f.handle(action, &ctx.media.accounts) {
                if e == crate::fin::FinEffect::Close {
                    self.fin = None;
                } else {
                    out.push(Effect::Fin(e));
                }
            }
            out
        } else if self.overlays.is_empty()
            && let Some(f) = self.files.as_mut()
        {
            let (effects, left) = f.handle(action);
            if left {
                self.files = None;
            }
            let pending = std::mem::take(&mut self.files_pending);
            pending
                .into_iter()
                .chain(effects)
                .map(Effect::Files)
                .collect()
        } else if !self.overlays.is_empty() {
            self.handle_overlay(action, ctx)
        } else if self.top_focus && self.page != Page::Settings {
            self.handle_top(action, ctx)
        } else {
            match self.page {
                Page::Home => self.handle_home(action, ctx),
                Page::Recents => self.handle_recents(action, ctx),
                Page::Apps => self.handle_apps(action, ctx),
                Page::Store => self.handle_store(action, ctx),
                Page::Search => self.handle_search(action, ctx),
                Page::Settings => self.handle_settings(action, ctx),
            }
        };
        if !self
            .overlays
            .iter()
            .any(|o| matches!(o, Overlay::Quick { .. }))
        {
            self.quick_over_app = false;
        }
        self.clamp(ctx);
        effects
    }

    /// Catfyn search results for `query` (stale ones are ignored).
    pub fn on_search_media(&mut self, query: String, items: Vec<crate::jellyfin::Item>) {
        if self.search_osk.text().trim().to_lowercase() == query {
            self.search_media = (query, items);
        }
    }

    /// inputd captured controller button `code` while "Press the button
    /// for …" is open: bind it and close.
    pub fn on_pad_captured(&mut self, code: u16) -> Vec<Effect> {
        match self.overlays.last() {
            Some(Overlay::PadCapture { action }) => {
                let action = *action;
                self.overlays.pop();
                vec![
                    Effect::SetPadBinding(action, code),
                    Effect::PadCapture(false),
                ]
            }
            _ => vec![Effect::PadCapture(false)],
        }
    }

    /// A phone asked to pair over KDE Connect: ask right away, wherever
    /// the user is, once per request.
    pub fn on_kde_requests(&mut self, ctx: &ShellContext) {
        for d in ctx.sys.kde.iter().filter(|d| d.pair_requested_by_peer) {
            if self.kde_prompted.contains(&d.id) {
                continue;
            }
            self.kde_prompted.push(d.id.clone());
            self.overlays.push(Overlay::KdeRequest {
                id: d.id.clone(),
                name: d.name.clone(),
                index: 0,
            });
        }
        // Forget answered / withdrawn requests so a new one asks again.
        self.kde_prompted.retain(|id| {
            ctx.sys
                .kde
                .iter()
                .any(|d| &d.id == id && d.pair_requested_by_peer)
        });
    }

    pub fn handle_ipc(&mut self, cmd: ShellCommand) -> Vec<Effect> {
        match cmd {
            ShellCommand::Home => self.go_home(),
            ShellCommand::Quick => self.open_quick(false),
            ShellCommand::Settings => self.open_page(Page::Settings),
            ShellCommand::Search => self.open_page(Page::Search),
            ShellCommand::Apps => self.open_page(Page::Apps),
            ShellCommand::Power => {
                if !matches!(self.overlay(), Some(Overlay::Power { .. })) {
                    self.overlays.push(Overlay::Power { index: 0 });
                }
            }
            ShellCommand::Reload => return vec![Effect::ReloadApps],
            // The GUI knows whether an app is in front; it decides.
            ShellCommand::Escape => return vec![Effect::LeaveApp],
            // Controller navigation needs the context: the GUI turns it
            // into `handle(action, ctx)` (see `ShellCommand::nav_action`).
            ShellCommand::NavUp
            | ShellCommand::NavDown
            | ShellCommand::NavLeft
            | ShellCommand::NavRight
            | ShellCommand::NavOk
            | ShellCommand::NavBack
            | ShellCommand::NavMenu => {}
            // Handled by the GUI (it reads the captured code, it changes
            // the volume and shows the panel).
            ShellCommand::PadCaptured
            | ShellCommand::VolumeUp
            | ShellCommand::VolumeDown
            | ShellCommand::Mute
            | ShellCommand::VolumeOutputs => {}
        }
        vec![]
    }

    /// Pointer hover (`click == false`) or click on a registered target.
    pub fn pointer(&mut self, target: Target, click: bool, ctx: &ShellContext) -> Vec<Effect> {
        self.clamp(ctx);
        let focused = self.focus_target(target, ctx);
        if !click || !focused {
            return vec![];
        }
        if target == Target::Outside {
            self.handle(Action::Back, ctx)
        } else if let Target::SettingsRow(i) = target
            && let Some(row) = self.settings_rows(ctx).get(i)
            && row.kind == RowKind::Choice
            && row.id != RowId::Accent
            && !row.disabled
        {
            // A click steps a choice on; the remote's OK-then-arrows is for
            // keys.
            self.settings_editing = None;
            self.adjust_row(row.id, 1, ctx).unwrap_or_default()
        } else {
            self.handle(Action::Ok, ctx)
        }
    }

    /// Move focus to `target`. Returns false if the target does not belong
    /// to what is currently on screen (stale hit-test).
    fn focus_target(&mut self, target: Target, ctx: &ShellContext) -> bool {
        let overlay_open = !self.overlays.is_empty();
        if let Some(f) = self.fin.as_mut()
            && !overlay_open
        {
            return match target {
                Target::Fin(r, c) => f.focus(r, c, &ctx.media.accounts),
                _ => false,
            };
        }
        if let Some(f) = self.files.as_mut()
            && !overlay_open
        {
            return match target {
                Target::Files(i) => {
                    f.focus_row(i);
                    true
                }
                Target::FilesPlace(i) => {
                    // The listing follows the pointer onto a place.
                    self.files_pending.extend(f.focus_place(i));
                    true
                }
                _ => false,
            };
        }
        match target {
            Target::HomeMedia(r, i)
                if self.page == Page::Home && ctx.media.item(r, i).is_some() =>
            {
                self.top_focus = false;
                self.home_media = Some((r, i));
                true
            }
            Target::Outside => overlay_open,
            Target::Item(i) => {
                let Some(top) = self.overlays.last() else {
                    return false;
                };
                if i >= self.overlay_len(top, ctx) {
                    return false;
                }
                match self.overlays.last_mut() {
                    Some(Overlay::KeyGuide) => true,
                    Some(o) => match o.index_mut() {
                        Some(index) => {
                            *index = i;
                            true
                        }
                        None => false,
                    },
                    None => false,
                }
            }
            Target::OverlayKey(r, c) => match self.overlays.last_mut() {
                Some(
                    Overlay::WifiPassword { osk, .. }
                    | Overlay::EthField { osk, .. }
                    | Overlay::DebugPassword { osk, .. }
                    | Overlay::StoreSearch { osk }
                    | Overlay::AppsSearch { osk },
                ) => {
                    osk.set_focus(r, c);
                    true
                }
                _ => false,
            },
            _ if overlay_open => false,
            Target::Top(i) if i < TOP_ITEMS.len() && self.page != Page::Settings => {
                // A tab shows its page as soon as the pointer rests on it
                // the Control Center pill waits for the click.
                if let Some(p) = TOP_ITEMS[i].page()
                    && p != self.page
                {
                    self.open_page(p);
                }
                self.top_focus = true;
                self.top_index = i;
                true
            }
            Target::Rail(i) if self.page == Page::Home && i < ctx.apps.len() => {
                self.top_focus = false;
                self.rail_index = i;
                true
            }
            Target::RecentCard(i) | Target::RecentClose(i)
                if self.page == Page::Recents && i < ctx.running().len() =>
            {
                self.top_focus = false;
                self.recents_index = i;
                self.recents_close = matches!(target, Target::RecentClose(_));
                true
            }
            Target::RecentCloseAll if self.page == Page::Recents && !ctx.running().is_empty() => {
                self.top_focus = false;
                if self.recents_index < ctx.running().len() {
                    self.recents_card = self.recents_index;
                }
                self.recents_index = ctx.running().len();
                self.recents_close = false;
                true
            }
            Target::Grid(i) if self.page == Page::Apps && i < ctx.apps.len() => {
                self.top_focus = false;
                self.grid_index = i;
                true
            }
            Target::StoreCat(i) if self.page == Page::Store && i < STORE_CATS.len() => {
                self.top_focus = false;
                self.store_grid = false;
                if self.store_cat != i {
                    self.store_cat = i;
                    self.store_index = 0;
                }
                true
            }
            Target::StoreItem(i) if self.page == Page::Store && i < self.store_items(ctx).len() => {
                self.top_focus = false;
                self.store_grid = true;
                self.store_index = i;
                true
            }
            Target::SearchKey(r, c) if self.page == Page::Search => {
                self.top_focus = false;
                self.search_focus = SearchFocus::Keyboard;
                self.search_osk.set_focus(r, c);
                true
            }
            Target::SearchHit(i)
                if self.page == Page::Search && i < self.search_results(ctx).len() =>
            {
                self.top_focus = false;
                self.search_focus = SearchFocus::Results;
                self.search_index = i;
                true
            }
            Target::SettingsCat(i) if self.page == Page::Settings && i < Category::ALL.len() => {
                if self.settings_cat != i {
                    self.settings_cat = i;
                    self.settings_row = 0;
                }
                self.settings_rows_focus = false;
                self.settings_editing = None;
                true
            }
            Target::SettingsRow(i)
                if self.page == Page::Settings
                    && self
                        .settings_rows(ctx)
                        .get(i)
                        .is_some_and(|r| r.kind != RowKind::Heading) =>
            {
                if self.settings_row != i {
                    self.settings_editing = None;
                }
                self.settings_rows_focus = true;
                self.settings_row = i;
                true
            }
            _ => false,
        }
    }

    /// The tab bar follows focus (like an Android TV): moving
    /// onto a tab shows its page at once; OK is only needed for the
    /// Control Center pill. Down leaves the bar into the page.
    fn handle_top(&mut self, action: Action, ctx: &ShellContext) -> Vec<Effect> {
        match action {
            Action::Left | Action::Right => {
                let i = if action == Action::Left {
                    self.top_index.saturating_sub(1)
                } else {
                    (self.top_index + 1).min(TOP_ITEMS.len() - 1)
                };
                if let Some(p) = TOP_ITEMS[i].page() {
                    self.open_page(p);
                    self.top_focus = true;
                }
                self.top_index = i;
            }
            Action::Down => self.top_focus = false,
            Action::Back => {
                if self.page == Page::Recents && ctx.running().is_empty() {
                    self.go_home();
                } else {
                    self.top_focus = false;
                }
            }
            Action::Ok | Action::OkLong => match TOP_ITEMS[self.top_index].page() {
                Some(_) => self.top_focus = false,
                None => self.overlays.push(Overlay::Quick { index: 0 }),
            },
            _ => {}
        }
        vec![]
    }

    /// OK on an app: launch it, or resume it when already running.
    fn activate_app(&self, app: usize, ctx: &ShellContext) -> Vec<Effect> {
        match ctx.apps.get(app) {
            Some(a) if a.running => vec![Effect::Resume(app)],
            Some(a) if a.entry.command == crate::apps::FIN_COMMAND => vec![Effect::OpenFin],
            Some(a) if a.entry.command == crate::apps::FILES_COMMAND => vec![Effect::OpenFiles],
            Some(_) => vec![Effect::Launch(app)],
            None => vec![],
        }
    }

    fn open_app_options(&mut self, app: usize, ctx: &ShellContext, movable: bool) {
        if app < ctx.apps.len() {
            self.overlays.push(Overlay::AppOptions {
                app,
                index: 0,
                movable,
            });
        }
    }

    /// Home's row while an app is picked up (hold OK): Left / Right carry
    /// it along, OK or Back puts it down, Down puts it down and opens its
    /// options.
    fn handle_moving(&mut self, action: Action, ctx: &ShellContext) -> Vec<Effect> {
        let n = ctx.apps.len();
        let i = self.rail_index;
        match action {
            Action::Left if i > 0 => {
                self.rail_index = i - 1;
                return vec![Effect::MoveApp { from: i, to: i - 1 }];
            }
            Action::Right if i + 1 < n => {
                self.rail_index = i + 1;
                return vec![Effect::MoveApp { from: i, to: i + 1 }];
            }
            Action::Ok | Action::OkLong | Action::Back => self.moving = false,
            Action::Down => {
                self.moving = false;
                self.open_app_options(i, ctx, true);
            }
            _ => {}
        }
        vec![]
    }

    fn handle_home(&mut self, action: Action, ctx: &ShellContext) -> Vec<Effect> {
        let n_apps = ctx.apps.len();
        if self.moving {
            return self.handle_moving(action, ctx);
        }
        let media = &ctx.media.home;
        // Media rows under the apps (Catfyn): Continue
        // watching, Movies, TV shows.
        if let Some((r, i)) = self.home_media {
            if media.is_empty() {
                self.home_media = None;
            } else {
                let r = r.min(media.len() - 1);
                let n = media[r].items.len();
                let i = i.min(n.saturating_sub(1));
                match action {
                    Action::Left => self.home_media = Some((r, i.saturating_sub(1))),
                    Action::Right => self.home_media = Some((r, (i + 1).min(n.saturating_sub(1)))),
                    Action::Down if r + 1 < media.len() => self.home_media = Some((r + 1, 0)),
                    Action::Up if r > 0 => self.home_media = Some((r - 1, 0)),
                    Action::Up | Action::Back => self.home_media = None,
                    Action::Ok => {
                        if let Some(it) = media[r].items.get(i) {
                            // Continue watching plays; other rows open details.
                            return if r == 0 && media[0].title == "Continue watching" {
                                vec![Effect::FinPlayHome(it.clone())]
                            } else {
                                vec![Effect::FinDetails(it.clone())]
                            };
                        }
                    }
                    Action::OkLong | Action::Options => {
                        if let Some(it) = media[r].items.get(i) {
                            return vec![Effect::FinDetails(it.clone())];
                        }
                    }
                    _ => {}
                }
                return vec![];
            }
        }
        if action == Action::Down && !media.is_empty() {
            self.home_media = Some((0, 0));
            return vec![];
        }
        match action {
            Action::Left => self.rail_index = self.rail_index.saturating_sub(1),
            Action::Right => self.rail_index = step(self.rail_index, 1, n_apps),
            Action::Up => {
                self.top_focus = true;
                self.top_index = 0;
            }
            Action::Ok => return self.activate_app(self.rail_index, ctx),
            // Hold OK picks the app up (see handle_moving); the Menu key
            // opens its options.
            Action::OkLong if n_apps > 0 => self.moving = true,
            Action::Options => self.open_app_options(self.rail_index, ctx, true),
            _ => {}
        }
        vec![]
    }

    /// Recents: a sliding row of running-app cards, an X above each card
    /// and a big "Close all" button centred under the row.
    /// Up from a card focuses its X, Up from the X reaches
    /// the top bar; Down from a card reaches Close all, Up comes back.
    fn handle_recents(&mut self, action: Action, ctx: &ShellContext) -> Vec<Effect> {
        let running = ctx.running();
        let n = running.len();
        let to_top = |s: &mut Self| {
            s.top_focus = true;
            s.top_index = top_index_of(Page::Recents);
        };
        if n == 0 {
            match action {
                Action::Up => to_top(self),
                Action::Back => self.go_home(),
                _ => {}
            }
            return vec![];
        }
        self.recents_card = self.recents_card.min(n - 1);
        if self.recents_index >= n {
            match action {
                Action::Ok | Action::OkLong => {
                    return running.into_iter().map(Effect::Close).collect();
                }
                Action::Up => {
                    self.recents_index = self.recents_card;
                    self.recents_close = false;
                }
                Action::Back => self.go_home(),
                _ => {}
            }
            return vec![];
        }
        match action {
            Action::Left if self.recents_index > 0 => self.recents_index -= 1,
            Action::Right if self.recents_index + 1 < n => self.recents_index += 1,
            Action::Up if !self.recents_close => self.recents_close = true,
            Action::Up => to_top(self),
            Action::Down if self.recents_close => self.recents_close = false,
            Action::Down => {
                self.recents_card = self.recents_index;
                self.recents_index = n;
            }
            Action::Ok | Action::OkLong if self.recents_close => {
                self.recents_close = false;
                return vec![Effect::Close(running[self.recents_index])];
            }
            Action::Ok => return vec![Effect::Resume(running[self.recents_index])],
            Action::Options => self.open_app_options(running[self.recents_index], ctx, false),
            Action::Back => self.go_home(),
            _ => {}
        }
        vec![]
    }

    fn handle_apps(&mut self, action: Action, ctx: &ShellContext) -> Vec<Effect> {
        let n = ctx.apps.len();
        match action {
            Action::Ok => return self.activate_app(self.grid_index, ctx),
            Action::OkLong | Action::Options => self.open_app_options(self.grid_index, ctx, false),
            Action::Back => self.go_home(),
            Action::Up | Action::Down | Action::Left | Action::Right => {
                match grid_move(self.grid_index, action, APPS_GRID_COLS, n) {
                    Some(i) => self.grid_index = i,
                    None => {
                        self.top_focus = true;
                        self.top_index = top_index_of(Page::Apps);
                    }
                }
            }
            _ => {}
        }
        vec![]
    }

    fn open_store_item(&mut self, i: usize, ctx: &ShellContext) -> Vec<Effect> {
        match self.store_items(ctx).get(i).cloned() {
            Some(item) => self.open_store_app(item),
            None => vec![],
        }
    }

    /// The full-screen Store page for `item`; its details load in the
    /// background ([`Effect::StoreDetails`]).
    fn open_store_app(&mut self, item: StoreItem) -> Vec<Effect> {
        let id = item.id.clone();
        self.overlays.push(Overlay::StoreApp { item, index: 0 });
        self.store_details = Some((id.clone(), Details::Loading));
        vec![Effect::StoreDetails(id)]
    }

    /// Screenshots of the Store page's app (0 while loading).
    pub fn store_shots(&self, id: &str) -> &[String] {
        match &self.store_details {
            Some((i, Details::Ready(d))) if i == id => &d.screenshots,
            _ => &[],
        }
    }

    pub fn on_store_details(&mut self, id: &str, result: Result<flatpak::AppDetails, String>) {
        if let Some((i, d)) = &mut self.store_details
            && i == id
        {
            *d = match result {
                Ok(v) => Details::Ready(v),
                Err(e) => Details::Failed(e),
            };
        }
    }

    fn handle_store(&mut self, action: Action, ctx: &ShellContext) -> Vec<Effect> {
        let n_items = self.store_items(ctx).len();
        if !self.store_grid {
            // The sections, a list on the left; moving along it switches
            // the section at once (like tabs). Right goes to its apps.
            match action {
                Action::Up if self.store_cat == 0 => {
                    self.top_focus = true;
                    self.top_index = top_index_of(Page::Store);
                }
                Action::Up => {
                    self.store_cat -= 1;
                    self.store_index = 0;
                }
                Action::Down if self.store_cat + 1 < STORE_CATS.len() => {
                    self.store_cat += 1;
                    self.store_index = 0;
                }
                Action::Right if n_items > 0 => self.store_grid = true,
                Action::Ok | Action::OkLong => {
                    if self.current_store_cat() == StoreCat::Search {
                        self.open_store_search();
                    } else if n_items > 0 {
                        self.store_grid = true;
                    }
                }
                Action::Back => self.go_home(),
                _ => {}
            }
            return vec![];
        }
        match action {
            Action::Ok | Action::OkLong => return self.open_store_item(self.store_index, ctx),
            Action::Back => self.store_grid = false,
            // Left of the first column: the sections.
            Action::Left if self.store_index.is_multiple_of(STORE_COLS) => self.store_grid = false,
            Action::Up | Action::Down | Action::Left | Action::Right => {
                match grid_move(self.store_index, action, STORE_COLS, n_items) {
                    Some(i) => self.store_index = i,
                    // Up from the first row: the top bar.
                    None if action == Action::Up => {
                        self.top_focus = true;
                        self.top_index = top_index_of(Page::Store);
                    }
                    None => {}
                }
            }
            _ => {}
        }
        vec![]
    }

    fn open_store_search(&mut self) {
        let mut osk = Osk::new(false, "Search");
        osk.set_text(&self.store_query);
        self.overlays
            .push(Overlay::StoreSearch { osk: Box::new(osk) });
    }

    fn handle_search(&mut self, action: Action, ctx: &ShellContext) -> Vec<Effect> {
        let hits = self.search_results(ctx);
        if action == Action::Back {
            self.go_home();
            return vec![];
        }
        let top = top_index_of(Page::Search);
        match self.search_focus {
            SearchFocus::Keyboard => match self.search_osk.handle(action) {
                OskEvent::Edited => {
                    self.search_index = 0;
                    let q = self.search_osk.text().trim().to_lowercase();
                    if q.chars().count() >= 2 && ctx.media.accounts.home().is_some() {
                        return vec![Effect::SearchMedia(q)];
                    }
                }
                OskEvent::Edge(Edge::Up) => {
                    self.top_focus = true;
                    self.top_index = top;
                }
                OskEvent::Edge(Edge::Right) | OskEvent::Submitted if !hits.is_empty() => {
                    self.search_focus = SearchFocus::Results
                }
                _ => {}
            },
            SearchFocus::Results => match action {
                Action::Up if self.search_index == 0 => {
                    self.top_focus = true;
                    self.top_index = top;
                }
                Action::Up => self.search_index -= 1,
                Action::Down => self.search_index = step(self.search_index, 1, hits.len()),
                Action::Left => self.search_focus = SearchFocus::Keyboard,
                Action::Ok => match hits.get(self.search_index) {
                    Some(SearchHit::App(i)) => return self.activate_app(*i, ctx),
                    Some(SearchHit::Setting { category, row, .. }) => {
                        let (category, row) = (*category, *row);
                        self.open_page(Page::Settings);
                        self.settings_cat = Category::ALL
                            .iter()
                            .position(|c| *c == category)
                            .unwrap_or(0);
                        let rows = settings::rows(category, ctx);
                        self.settings_row = rows.iter().position(|r| r.id == row).unwrap_or(0);
                        self.settings_rows_focus = true;
                    }
                    Some(SearchHit::Store(item)) => {
                        return self.open_store_app(item.clone());
                    }
                    Some(SearchHit::Media(item)) => {
                        return vec![Effect::FinDetails((**item).clone())];
                    }
                    None => {}
                },
                Action::OkLong | Action::Options => {
                    if let Some(SearchHit::App(i)) = hits.get(self.search_index) {
                        self.open_app_options(*i, ctx, false);
                    }
                }
                _ => {}
            },
        }
        vec![]
    }

    /// Left/Right on an adjustable row. `None` → the row is not adjustable.
    fn adjust_row(&self, id: RowId, delta: i32, ctx: &ShellContext) -> Option<Vec<Effect>> {
        match id {
            RowId::Volume if ctx.sys.volume.is_some() => {
                Some(vec![Effect::VolumeStep(VOLUME_STEP * delta.signum() as i8)])
            }
            RowId::PictureFit => {
                let cur = ctx.display.safe_area_pct as i32;
                let new = (cur + delta).clamp(0, display::MAX_SAFE_AREA_PCT as i32) as u8;
                Some(if new as i32 == cur {
                    vec![]
                } else {
                    vec![Effect::SetSafeArea(new)]
                })
            }
            RowId::Scale => {
                let out = ctx.output()?;
                let cur = ctx.ui_scale().unwrap_or(out.scale);
                let new = settings::cycle_scale(cur, delta);
                Some(if (new - cur).abs() < 0.01 {
                    vec![]
                } else {
                    vec![Effect::SetScale {
                        output: out.name.clone(),
                        percent: (new * 100.0).round() as u32,
                    }]
                })
            }
            RowId::Accent => Some(vec![Effect::SetAccent(
                prefs::cycle_accent(&ctx.prefs.accent, delta).to_string(),
            )]),
            RowId::ClockFormat => Some(vec![Effect::SetClock24(!ctx.prefs.clock_24h)]),
            RowId::HomeBackground => Some(vec![Effect::SetHomeBackground(
                prefs::cycle_home_background(&ctx.prefs.home_background, delta).to_string(),
            )]),
            RowId::SaverPictures => Some(vec![Effect::SetSaverPictures(
                crate::screensaver::Pictures::from_key(&ctx.prefs.saver_pictures)
                    .other()
                    .key()
                    .to_string(),
            )]),
            RowId::SaverMinutes => {
                let m = crate::screensaver::cycle_minutes(ctx.prefs.saver_minutes, delta);
                Some(if m == ctx.prefs.saver_minutes {
                    vec![]
                } else {
                    vec![Effect::SetSaverMinutes(m)]
                })
            }
            RowId::AndroidVariant => Some(vec![Effect::SetAndroidVariant(
                if ctx.prefs.android_variant == "vanilla" {
                    "gapps".into()
                } else {
                    "vanilla".into()
                },
            )]),
            RowId::Controller(i) => ctx.sys.controllers.get(i).map(|name| {
                vec![Effect::SetPadLayout {
                    device: name.clone(),
                    nintendo: !ctx.prefs.nintendo_for(name),
                }]
            }),
            _ => None,
        }
    }

    fn handle_settings(&mut self, action: Action, ctx: &ShellContext) -> Vec<Effect> {
        let rows = self.settings_rows(ctx);
        if !self.settings_rows_focus {
            match action {
                Action::Up => {
                    self.settings_cat = self.settings_cat.saturating_sub(1);
                    self.settings_row = 0;
                }
                Action::Down => {
                    self.settings_cat = step(self.settings_cat, 1, Category::ALL.len());
                    self.settings_row = 0;
                }
                Action::Right | Action::Ok | Action::OkLong if !rows.is_empty() => {
                    self.settings_rows_focus = true
                }
                Action::Back | Action::Left => self.go_home(),
                _ => {}
            }
            return vec![];
        }
        let Some(row) = rows.get(self.settings_row) else {
            self.settings_rows_focus = false;
            return vec![];
        };
        // A greyed-out row can be passed over, nothing more.
        if row.disabled
            && !matches!(
                action,
                Action::Up | Action::Down | Action::Left | Action::Back
            )
        {
            return vec![];
        }
        let accent = row.id == RowId::Accent;
        let adjustable =
            !row.disabled && (accent || matches!(row.kind, RowKind::Slider(_) | RowKind::Choice));
        let here = (self.settings_cat, self.settings_row);
        if adjustable && self.settings_editing == Some(here) {
            match action {
                Action::Left | Action::Right if accent => {
                    let n = prefs::ACCENTS.len();
                    let from = self
                        .accent_pick
                        .unwrap_or_else(|| prefs::accent_index(&ctx.prefs.accent));
                    self.accent_pick = Some(if action == Action::Left {
                        (from + n - 1) % n
                    } else {
                        (from + 1) % n
                    });
                    return vec![];
                }
                Action::Left | Action::Right => {
                    let d = if action == Action::Left { -1 } else { 1 };
                    return self.adjust_row(row.id, d, ctx).unwrap_or_default();
                }
                // Done. The accent colour is only taken on OK.
                Action::Ok | Action::OkLong => {
                    self.settings_editing = None;
                    return match self.accent_pick.take() {
                        Some(i) if accent && prefs::ACCENTS[i].0 != ctx.prefs.accent => {
                            vec![Effect::SetAccent(prefs::ACCENTS[i].0.to_string())]
                        }
                        _ => vec![],
                    };
                }
                Action::Back => {
                    self.settings_editing = None;
                    self.accent_pick = None;
                    return vec![];
                }
                // Up/Down end it and move on.
                Action::Up | Action::Down => {}
                _ => return vec![],
            }
        }
        self.settings_editing = None;
        self.accent_pick = None;
        match action {
            Action::Left | Action::Back => {
                self.settings_rows_focus = false;
                vec![]
            }
            // Headings are passed over.
            Action::Up | Action::Down => {
                let down = action == Action::Down;
                let mut i = self.settings_row;
                while let Some(next) = if down {
                    (i + 1 < rows.len()).then_some(i + 1)
                } else {
                    i.checked_sub(1)
                } {
                    i = next;
                    if rows[i].kind != RowKind::Heading {
                        self.settings_row = i;
                        break;
                    }
                }
                vec![]
            }
            Action::Ok | Action::OkLong if adjustable => {
                self.settings_editing = Some(here);
                if accent {
                    self.accent_pick = Some(prefs::accent_index(&ctx.prefs.accent));
                }
                vec![]
            }
            Action::Ok | Action::OkLong => self.activate_row(row.id, row.kind, &row.title, ctx),
            _ => vec![],
        }
    }

    fn activate_row(
        &mut self,
        id: RowId,
        kind: RowKind,
        title: &str,
        ctx: &ShellContext,
    ) -> Vec<Effect> {
        if kind == RowKind::Soon {
            return vec![Effect::Toast(format!("{title} is not available yet"))];
        }
        let sys = ctx.sys;
        match id {
            RowId::WifiRadio => match sys.wifi_on {
                Some(on) => vec![Effect::SetWifi(!on)],
                None => vec![],
            },
            RowId::WifiNet(i) => self.choose_wifi(i),
            RowId::Ethernet => self.open_ethernet(ctx),
            RowId::UpdateCheck => vec![Effect::CheckUpdates],
            RowId::UpdateInstall => vec![Effect::InstallUpdate],
            RowId::BtRadio if sys.bluetooth.available => {
                vec![Effect::SetBluetooth(!sys.bluetooth.powered)]
            }
            RowId::BtDevice(i) if i < sys.bt_devices.len() => {
                self.overlays.push(Overlay::BtDevice {
                    device: i,
                    index: 0,
                });
                vec![]
            }
            RowId::BtPair => {
                if !sys.bluetooth.powered {
                    return vec![Effect::Toast("Turn Bluetooth on first".into())];
                }
                self.overlays.push(Overlay::BtScan { index: 0 });
                self.bt_scan = Scan::Busy;
                vec![Effect::BtScan]
            }
            RowId::Resolution => {
                let modes = self.menu_modes(ctx);
                if modes.is_empty() {
                    return vec![];
                }
                let index = modes.iter().position(|m| m.current).unwrap_or(0);
                self.overlays.push(Overlay::DisplayModes { index });
                vec![]
            }
            RowId::AdaptiveSync => match ctx.output() {
                Some(o) => vec![Effect::SetAdaptiveSync {
                    output: o.name.clone(),
                    on: !o.adaptive_sync.unwrap_or(false),
                }],
                None => vec![],
            },
            RowId::Mute => vec![Effect::ToggleMute],
            RowId::Screensaver => vec![Effect::SetScreensaver(!ctx.prefs.screensaver)],
            RowId::PadNav => vec![Effect::SetPadNav(!ctx.prefs.pad_nav)],
            RowId::Controller(i) => match ctx.sys.controllers.get(i) {
                Some(name) => vec![Effect::SetPadLayout {
                    device: name.clone(),
                    nintendo: !ctx.prefs.nintendo_for(name),
                }],
                None => vec![],
            },
            RowId::Hdr => vec![Effect::SetHdr(!ctx.prefs.hdr)],
            RowId::AudioOutput if !sys.sinks.is_empty() => {
                let index = sys.sinks.iter().position(|s| s.is_default).unwrap_or(0);
                self.overlays.push(Overlay::AudioOutputs { index });
                vec![]
            }
            RowId::HdmiAudio => vec![Effect::ApplyHdmiProfile],
            RowId::DebugSsh if self.debug_ssh_busy.is_some() => vec![],
            RowId::DebugSsh => {
                let on = !sys.debug.ssh_on;
                self.debug_ssh_busy = Some(on);
                vec![Effect::DebugSsh(on)]
            }
            RowId::DebugPassword => {
                self.overlays.push(Overlay::DebugPassword {
                    osk: Box::new(Osk::new(true, "Next")),
                    first: None,
                });
                vec![]
            }
            RowId::DebugRemoveOld => vec![Effect::DebugRemoveOld],
            RowId::App(i) => {
                self.open_app_options(i, ctx, false);
                vec![]
            }
            RowId::AppSearch => {
                let mut osk = Osk::new(false, "Done");
                osk.set_text(&self.apps_filter);
                self.overlays
                    .push(Overlay::AppsSearch { osk: Box::new(osk) });
                vec![]
            }
            RowId::UpdateApps => vec![Effect::UpdateApps],
            // First setup: the store chooser; "Try again" after a
            // failure keeps the choice (its own row: after a failure no image
            // is installed yet either).
            RowId::AndroidRetry => vec![Effect::AndroidSetup],
            RowId::AndroidSetup if ctx.sys.android_image.is_none() => {
                self.overlays.push(Overlay::StoreChoice {
                    index: usize::from(ctx.prefs.android_variant == "vanilla"),
                });
                vec![]
            }
            RowId::AndroidSetup => vec![Effect::AndroidSetup],
            RowId::AndroidCertify => {
                self.overlays.push(Overlay::Certify);
                vec![]
            }
            RowId::AndroidApk => {
                self.overlays.push(Overlay::ApkList { index: 0 });
                self.apks = Scan::Busy;
                self.apk_stack.clear();
                vec![Effect::BrowseApks(None)]
            }
            RowId::AndroidStore => vec![Effect::AndroidOpenStore],
            RowId::AndroidRepair => {
                self.overlays.push(Overlay::RepairConfirm { index: 0 });
                vec![]
            }
            RowId::KdeDevice(i) if i < sys.kde.len() => {
                self.overlays.push(Overlay::KdeDevice {
                    device: i,
                    index: 0,
                });
                vec![]
            }
            RowId::KeyGuide => {
                self.show_key_guide();
                vec![]
            }
            RowId::PadBind(action) => {
                self.overlays.push(Overlay::PadCapture { action });
                vec![Effect::PadCapture(true)]
            }
            RowId::PadBindReset => vec![Effect::ClearPadBindings],
            RowId::RestartShell => vec![Effect::RestartShell],
            RowId::Reboot => {
                self.overlays.push(Overlay::Power { index: 1 });
                vec![]
            }
            RowId::PowerOff => {
                self.overlays.push(Overlay::Power { index: 2 });
                vec![]
            }
            _ => vec![],
        }
    }

    fn handle_overlay(&mut self, action: Action, ctx: &ShellContext) -> Vec<Effect> {
        let len = match self.overlays.last() {
            Some(o) => self.overlay_len(o, ctx),
            None => return vec![],
        };
        // Overlays that handle Back themselves.
        let keeps_back = matches!(
            self.overlays.last(),
            Some(
                Overlay::WifiPassword { .. }
                    | Overlay::DebugPassword { .. }
                    | Overlay::EthField { .. }
                    | Overlay::StoreSearch { .. }
                    | Overlay::AppsSearch { .. }
                    | Overlay::DisplayConfirm { .. }
                    | Overlay::KeyGuide
                    | Overlay::PadCapture { .. }
                    | Overlay::ApkList { .. }
            )
        );
        if action == Action::Back && !keeps_back {
            let was_quick = matches!(self.overlays.pop(), Some(Overlay::Quick { .. }));
            if was_quick && self.overlays.is_empty() && std::mem::take(&mut self.quick_over_app) {
                return vec![Effect::ReturnToApp];
            }
            return vec![];
        }
        let is_ok = matches!(action, Action::Ok | Action::OkLong);
        // Generic vertical list navigation. Returns true if it moved.
        let vertical = |index: &mut usize| match action {
            Action::Up => {
                *index = index.saturating_sub(1);
                true
            }
            Action::Down => {
                *index = step(*index, 1, len);
                true
            }
            _ => false,
        };
        let app_items = self.quick_app_items();
        let Some(top) = self.overlays.last_mut() else {
            return vec![];
        };
        match top {
            Overlay::Quick { index } => {
                let off = if self.quick_over_app {
                    app_items.len()
                } else {
                    0
                };
                let n = off + QUICK_ITEMS.len();
                // App controls row (opened from inside an app).
                if *index < off {
                    match action {
                        Action::Left => *index = index.saturating_sub(1),
                        Action::Right => *index = (*index + 1).min(off - 1),
                        Action::Down => *index = off,
                        Action::Ok | Action::OkLong => {
                            let item = app_items[*index];
                            let fullscreen = self.quick_app.as_ref().is_some_and(|a| a.fullscreen);
                            let mouse = self.quick_app.as_ref().and_then(|a| a.mouse);
                            self.overlays.pop();
                            self.quick_over_app = false;
                            return match item {
                                QuickAppItem::BackToApp => vec![Effect::ReturnToApp],
                                QuickAppItem::Fullscreen => vec![
                                    Effect::AppWindow(crate::toplevel::WindowAction::Fullscreen(
                                        !fullscreen,
                                    )),
                                    Effect::ReturnToApp,
                                ],
                                QuickAppItem::Mouse => {
                                    vec![
                                        Effect::AppMouse(!mouse.unwrap_or(false)),
                                        Effect::ReturnToApp,
                                    ]
                                }
                                QuickAppItem::Close => {
                                    vec![Effect::AppWindow(crate::toplevel::WindowAction::Close)]
                                }
                            };
                        }
                        _ => {}
                    }
                    return vec![];
                }
                if *index >= n {
                    // Volume row.
                    return match action {
                        Action::Left => vec![Effect::VolumeStep(-VOLUME_STEP)],
                        Action::Right => vec![Effect::VolumeStep(VOLUME_STEP)],
                        Action::Up => {
                            *index = n.saturating_sub(QUICK_COLS);
                            vec![]
                        }
                        Action::Ok | Action::OkLong => vec![Effect::ToggleMute],
                        _ => vec![],
                    };
                }
                let i = *index - off;
                match action {
                    Action::Left if !i.is_multiple_of(QUICK_COLS) => *index -= 1,
                    Action::Right if i % QUICK_COLS < QUICK_COLS - 1 => *index += 1,
                    Action::Up if i >= QUICK_COLS => *index -= QUICK_COLS,
                    Action::Up if off > 0 => *index = 0,
                    Action::Down if i + QUICK_COLS < QUICK_ITEMS.len() => *index += QUICK_COLS,
                    // The volume row exists only when the volume is known.
                    Action::Down if ctx.sys.volume.is_some() => *index = n,
                    Action::Ok | Action::OkLong => {
                        let item = QUICK_ITEMS[i];
                        return self.activate_quick(item, ctx);
                    }
                    _ => {}
                }
                vec![]
            }
            Overlay::AppOptions {
                app,
                index,
                movable,
            } => {
                if vertical(index) || !is_ok {
                    return vec![];
                }
                let (app, index, movable) = (*app, *index, *movable);
                let running = ctx.apps.get(app).is_some_and(|a| a.running);
                let choice = app_options(running, is_removable(ctx, app), movable)[index];
                self.overlays.pop();
                match choice {
                    AppOption::Open => vec![Effect::Launch(app)],
                    AppOption::Move => {
                        self.rail_index = app;
                        self.moving = true;
                        vec![]
                    }
                    AppOption::Resume => vec![Effect::Resume(app)],
                    AppOption::Restart => vec![Effect::Restart(app)],
                    AppOption::Close => vec![Effect::Close(app)],
                    AppOption::Uninstall => vec![Effect::Uninstall(app)],
                    AppOption::Cancel => vec![],
                }
            }
            Overlay::Power { index } => {
                if vertical(index) || !is_ok {
                    return vec![];
                }
                let item = POWER_ITEMS[*index];
                self.overlays.pop();
                match item {
                    PowerItem::Sleep => vec![sleep_effect(ctx)],
                    PowerItem::Restart => vec![Effect::Power(PowerAction::Reboot)],
                    PowerItem::PowerOff => vec![Effect::Power(PowerAction::PowerOff)],
                    PowerItem::Cancel => vec![],
                }
            }
            Overlay::KdeDevice { device, index } => {
                if vertical(index) || !is_ok {
                    return vec![];
                }
                let Some(d) = ctx.sys.kde.get(*device) else {
                    self.overlays.pop();
                    return vec![];
                };
                let Some(opt) = kde_options(d).get(*index).copied() else {
                    return vec![];
                };
                let id = d.id.clone();
                self.overlays.pop();
                vec![match opt {
                    KdeOption::Accept => Effect::KdeAccept(id),
                    KdeOption::Decline => Effect::KdeReject(id),
                    KdeOption::Pair => Effect::KdePair(id),
                    KdeOption::Ping => Effect::KdePing(id),
                    KdeOption::Unpair => Effect::KdeUnpair(id),
                }]
            }
            Overlay::QuickNetwork { index } => {
                if vertical(index) {
                    return vec![];
                }
                if action == Action::Back {
                    self.overlays.pop();
                    return vec![];
                }
                if !is_ok {
                    return vec![];
                }
                let i = *index;
                let Some(row) = self.quick_net_rows(ctx).get(i).copied() else {
                    return vec![];
                };
                match row {
                    NetRow::WifiSwitch => {
                        let on = ctx.sys.wifi_on != Some(true);
                        let mut v = vec![Effect::SetWifi(on)];
                        if on {
                            self.wifi_scan = Scan::Busy;
                            v.push(Effect::ScanWifi);
                        }
                        v
                    }
                    NetRow::Ethernet => self.open_ethernet(ctx),
                    NetRow::Wifi(i) => self.choose_wifi(i),
                    NetRow::ScanAgain => self.choose_wifi(self.wifi_scan.items().len()),
                }
            }
            Overlay::QuickBluetooth { index } => {
                if vertical(index) {
                    return vec![];
                }
                if action == Action::Back {
                    self.overlays.pop();
                    return vec![];
                }
                if !is_ok || self.bt_busy.is_some() {
                    return vec![];
                }
                let i = *index;
                let Some(row) = quick_bt_rows(ctx).get(i).copied() else {
                    return vec![];
                };
                let sys = ctx.sys;
                match row {
                    BtRow::Switch => vec![Effect::SetBluetooth(!sys.bluetooth.powered)],
                    BtRow::Device(d) => {
                        let Some(d) = sys.bt_devices.get(d) else {
                            return vec![];
                        };
                        let mac = d.address.clone();
                        self.bt_busy = Some(mac.clone());
                        if bt_connected(sys, &mac) {
                            vec![Effect::BtDisconnect(mac)]
                        } else {
                            vec![bt_connect_effect(sys, mac)]
                        }
                    }
                    BtRow::Pair => {
                        self.overlays.push(Overlay::BtScan { index: 0 });
                        self.bt_scan = Scan::Busy;
                        vec![Effect::BtScan]
                    }
                }
            }
            Overlay::Ethernet { index } => {
                if vertical(index) {
                    return vec![];
                }
                if action == Action::Back {
                    self.eth_draft = None;
                    self.overlays.pop();
                    return vec![];
                }
                let i = *index;
                let Some(row) = self.eth_rows().get(i).copied() else {
                    return vec![];
                };
                let Some(draft) = self.eth_draft.as_mut() else {
                    return vec![];
                };
                match (row, action) {
                    (EthRow::Method, Action::Ok | Action::Left | Action::Right) => {
                        draft.manual = !draft.manual;
                        vec![]
                    }
                    (EthRow::Field(field), Action::Ok) => {
                        let mut osk = Osk::new(false, "Done");
                        osk.set_text(field.value(draft));
                        self.overlays.push(Overlay::EthField {
                            field,
                            osk: Box::new(osk),
                        });
                        vec![]
                    }
                    (EthRow::Save, Action::Ok) => {
                        if let Err(e) = draft.validate() {
                            return vec![Effect::Toast(e.into())];
                        }
                        let Some(connection) =
                            ctx.sys.ethernet.as_ref().and_then(|e| e.connection.clone())
                        else {
                            return vec![Effect::Toast("Plug in the Ethernet cable first".into())];
                        };
                        let config = draft.clone();
                        self.eth_draft = None;
                        self.overlays.pop();
                        vec![
                            Effect::SetEthernetIp { connection, config },
                            Effect::Toast("Saving the Ethernet settings…".into()),
                        ]
                    }
                    _ => vec![],
                }
            }
            Overlay::EthField { field, osk } => match osk.handle(action) {
                OskEvent::Submitted => {
                    let text = osk.text().trim().to_string();
                    let field = *field;
                    if let Err(e) = field.check(&text) {
                        return vec![Effect::Toast(e.into())];
                    }
                    if let Some(d) = self.eth_draft.as_mut() {
                        field.set(d, text);
                    }
                    self.overlays.pop();
                    vec![]
                }
                OskEvent::Unhandled if action == Action::Back => {
                    self.overlays.pop();
                    vec![]
                }
                _ => vec![],
            },
            Overlay::DebugPassword { osk, first } => match osk.handle(action) {
                OskEvent::Submitted => {
                    let typed = osk.text().to_string();
                    match first.take() {
                        None => {
                            if let Err(e) = crate::debug::password_ok(&typed) {
                                return vec![Effect::Toast(e.into())];
                            }
                            *first = Some(typed);
                            **osk = Osk::new(true, "Set password");
                            vec![]
                        }
                        Some(f) if f == typed => {
                            self.overlays.pop();
                            vec![Effect::DebugPassword(typed)]
                        }
                        Some(_) => {
                            **osk = Osk::new(true, "Next");
                            vec![Effect::Toast(
                                "The passwords don't match. Type it again".into(),
                            )]
                        }
                    }
                }
                OskEvent::Unhandled if action == Action::Back => {
                    self.overlays.pop();
                    vec![]
                }
                _ => vec![],
            },
            Overlay::WifiPassword { ssid, osk } => {
                if self.wifi_connecting.is_some() {
                    return vec![]; // ignore input while connecting
                }
                match osk.handle(action) {
                    OskEvent::Submitted => {
                        let ssid = ssid.clone();
                        let password = osk.text().to_string();
                        if password.chars().count() < 8 {
                            return vec![Effect::Toast(
                                "Wi-Fi passwords have at least 8 characters".into(),
                            )];
                        }
                        self.wifi_connecting = Some(ssid.clone());
                        vec![Effect::ConnectWifi {
                            ssid,
                            password: Some(password),
                        }]
                    }
                    OskEvent::Unhandled if action == Action::Back => {
                        self.overlays.pop();
                        vec![]
                    }
                    _ => vec![],
                }
            }
            Overlay::BtScan { index } => {
                if vertical(index) || !is_ok || self.bt_busy.is_some() {
                    return vec![];
                }
                let i = *index;
                if i == self.bt_scan.items().len() {
                    if self.bt_scan != Scan::Busy {
                        self.bt_scan = Scan::Busy;
                        return vec![Effect::BtScan];
                    }
                    return vec![];
                }
                let mac = self.bt_scan.items()[i].address.clone();
                self.bt_busy = Some(mac.clone());
                vec![Effect::BtPair(mac)]
            }
            Overlay::BtDevice { device, index } => {
                if vertical(index) || !is_ok {
                    return vec![];
                }
                let (device, choice) = (*device, BT_OPTIONS[*index]);
                self.overlays.pop();
                let Some(mac) = ctx.sys.bt_devices.get(device).map(|d| d.address.clone()) else {
                    return vec![];
                };
                match choice {
                    BtOption::Connect => vec![bt_connect_effect(ctx.sys, mac)],
                    BtOption::Disconnect => vec![Effect::BtDisconnect(mac)],
                    BtOption::Forget => vec![Effect::BtForget(mac)],
                    BtOption::Cancel => vec![],
                }
            }
            Overlay::DisplayModes { index } => {
                if vertical(index) || !is_ok {
                    return vec![];
                }
                let i = *index;
                let mode = self.menu_modes(ctx).get(i).copied();
                let (Some(out), Some(mode)) = (ctx.output(), mode) else {
                    return vec![];
                };
                self.overlays.pop();
                if mode.current {
                    return vec![];
                }
                self.display_pending = Some(PendingMode {
                    output: out.name.clone(),
                    previous: out.current_mode().copied(),
                    new: mode,
                });
                // Revert is focused: a blind OK on a blank screen must not
                // keep a mode the TV cannot show.
                self.overlays.push(Overlay::DisplayConfirm { index: 1 });
                vec![Effect::SetMode {
                    output: out.name.clone(),
                    mode,
                }]
            }
            Overlay::KdeRequest { id, index, .. } => {
                match action {
                    Action::Left | Action::Up => *index = 0,
                    Action::Right | Action::Down => *index = 1,
                    Action::Ok | Action::OkLong => {
                        let (id, accept) = (id.clone(), *index == 0);
                        self.overlays.pop();
                        return vec![if accept {
                            Effect::KdeAccept(id)
                        } else {
                            Effect::KdeReject(id)
                        }];
                    }
                    _ => {}
                }
                vec![]
            }
            Overlay::DisplayConfirm { index } => {
                let choice = match action {
                    Action::Left | Action::Up => {
                        *index = 0;
                        return vec![];
                    }
                    Action::Right | Action::Down => {
                        *index = 1;
                        return vec![];
                    }
                    Action::Ok | Action::OkLong => CONFIRM_ITEMS[(*index).min(1)],
                    Action::Back => ConfirmItem::Revert,
                    _ => return vec![],
                };
                self.overlays.pop();
                if std::mem::take(&mut self.hdr_pending) {
                    return match choice {
                        ConfirmItem::Keep => vec![Effect::KeepHdr],
                        ConfirmItem::Revert => vec![Effect::SetHdr(false)],
                    };
                }
                let Some(p) = self.display_pending.take() else {
                    return vec![];
                };
                match choice {
                    ConfirmItem::Keep => vec![Effect::KeepMode {
                        output: p.output,
                        mode: p.new,
                    }],
                    ConfirmItem::Revert => vec![Effect::RevertMode {
                        output: p.output,
                        mode: p.previous,
                    }],
                }
            }
            Overlay::AudioOutputs { index } => {
                if vertical(index) || !is_ok {
                    return vec![];
                }
                let i = *index;
                self.overlays.pop();
                match ctx.sys.sinks.get(i) {
                    Some(s) if !s.is_default => vec![Effect::SetSink(s.id)],
                    _ => vec![],
                }
            }
            Overlay::StoreApp { item, index } => {
                // Buttons in one row, screenshots in a row below them.
                let installed = installed_per_log(&self.store_log, ctx, &item.id);
                let acts = store_actions_installed(&self.store_jobs, item, installed);
                let n_acts = acts.len();
                let n_shots = match &self.store_details {
                    Some((i, Details::Ready(d))) if *i == item.id => d.screenshots.len(),
                    _ => 0,
                };
                let on_shot = *index >= n_acts;
                match action {
                    Action::Left if on_shot => *index = (*index).max(n_acts + 1) - 1,
                    Action::Right if on_shot && n_shots > 0 => {
                        *index = (*index + 1).min(n_acts + n_shots - 1)
                    }
                    Action::Right if on_shot => {}
                    Action::Up if on_shot && n_acts > 0 => *index = 0,
                    Action::Up if on_shot => {}
                    Action::Down if !on_shot && n_shots > 0 => *index = n_acts,
                    Action::Left => *index = index.saturating_sub(1),
                    Action::Right => *index = step(*index, 1, n_acts),
                    Action::Up | Action::Down => {}
                    Action::Ok | Action::OkLong if on_shot => {}
                    Action::Ok | Action::OkLong => {
                        let id = item.id.clone();
                        let choice = acts[(*index).min(acts.len() - 1)];
                        return match choice {
                            StoreAction::Install => {
                                self.store_jobs
                                    .push((id.clone(), StoreJob::Installing(None)));
                                log_store(
                                    &mut self.store_log,
                                    item.clone(),
                                    StoreLogState::Installing,
                                );
                                vec![Effect::StoreInstall(id)]
                            }
                            StoreAction::Open => {
                                self.overlays.pop();
                                vec![Effect::StoreOpen(id)]
                            }
                            StoreAction::Remove => {
                                self.store_jobs.push((id.clone(), StoreJob::Removing));
                                log_store(
                                    &mut self.store_log,
                                    item.clone(),
                                    StoreLogState::Removing,
                                );
                                vec![Effect::StoreUninstall(id)]
                            }
                        };
                    }
                    _ => {}
                }
                vec![]
            }
            Overlay::StoreSearch { osk } => match osk.handle(action) {
                OskEvent::Submitted => {
                    let q = osk.text().trim().to_string();
                    self.overlays.pop();
                    if q.is_empty() {
                        return vec![];
                    }
                    self.store_query = q.clone();
                    self.store_results = Scan::Busy;
                    self.store_cat = STORE_CATS
                        .iter()
                        .position(|c| *c == StoreCat::Search)
                        .unwrap_or(0);
                    self.store_index = 0;
                    self.store_grid = false;
                    vec![Effect::StoreSearch(q)]
                }
                OskEvent::Unhandled if action == Action::Back => {
                    self.overlays.pop();
                    vec![]
                }
                _ => vec![],
            },
            Overlay::AppsSearch { osk } => {
                let ev = osk.handle(action);
                self.apps_filter = osk.text().to_string();
                match ev {
                    OskEvent::Submitted => {
                        self.overlays.pop();
                    }
                    OskEvent::Unhandled if action == Action::Back => {
                        self.overlays.pop();
                    }
                    _ => {}
                }
                vec![]
            }
            Overlay::KeyGuide => {
                if is_ok || action == Action::Back {
                    self.overlays.pop();
                    vec![Effect::KeyGuideSeen]
                } else {
                    vec![]
                }
            }
            Overlay::Certify => {
                if is_ok {
                    self.overlays.pop();
                }
                vec![]
            }
            Overlay::StoreChoice { index } => {
                let i = *index;
                match action {
                    Action::Left | Action::Up => {
                        *index = 0;
                        vec![]
                    }
                    Action::Right | Action::Down => {
                        *index = 1;
                        vec![]
                    }
                    Action::Back => {
                        self.overlays.pop();
                        vec![]
                    }
                    _ if is_ok => {
                        self.overlays.pop();
                        vec![
                            Effect::SetAndroidVariant(
                                if i == 1 { "vanilla" } else { "gapps" }.to_string(),
                            ),
                            Effect::AndroidSetup,
                        ]
                    }
                    _ => vec![],
                }
            }
            Overlay::RepairConfirm { index } => {
                let i = *index;
                match action {
                    Action::Left | Action::Up => {
                        *index = 0;
                        vec![]
                    }
                    Action::Right | Action::Down => {
                        *index = 1;
                        vec![]
                    }
                    Action::Back => {
                        self.overlays.pop();
                        vec![]
                    }
                    _ if is_ok => {
                        self.overlays.pop();
                        if i == 1 {
                            vec![Effect::AndroidRepair]
                        } else {
                            vec![]
                        }
                    }
                    _ => vec![],
                }
            }
            Overlay::PadCapture { .. } => {
                // Only the controller (through inputd) or Back closes it.
                if action == Action::Back {
                    self.overlays.pop();
                    vec![Effect::PadCapture(false)]
                } else {
                    vec![]
                }
            }
            Overlay::ApkList { index } => {
                // A folder/APK picker: OK opens a folder or installs an APK,
                // Back goes up one folder (and closes at the top).
                if action == Action::Back {
                    return match self.apk_stack.pop() {
                        Some((_, from)) => {
                            *index = from;
                            self.apks = Scan::Busy;
                            vec![Effect::BrowseApks(
                                self.apk_stack.last().map(|(d, _)| d.clone()),
                            )]
                        }
                        None => {
                            self.overlays.pop();
                            vec![]
                        }
                    };
                }
                if vertical(index) || !is_ok {
                    return vec![];
                }
                let Some(e) = self.apks.items().get(*index).cloned() else {
                    return vec![];
                };
                if e.dir {
                    self.apk_stack.push((e.path.clone(), *index));
                    *index = 0;
                    self.apks = Scan::Busy;
                    vec![Effect::BrowseApks(Some(e.path))]
                } else {
                    self.overlays.pop();
                    vec![Effect::InstallApk(e.path)]
                }
            }
        }
    }

    /// The rows of Quick settings → Network.
    pub fn quick_net_rows(&self, ctx: &ShellContext) -> Vec<NetRow> {
        // No Wi-Fi adapter: no switch.
        let mut v: Vec<NetRow> = ctx
            .sys
            .wifi_on
            .map(|_| NetRow::WifiSwitch)
            .into_iter()
            .collect();
        if ctx.sys.ethernet.is_some() {
            v.push(NetRow::Ethernet);
        }
        if ctx.sys.wifi_on == Some(true) {
            v.extend((0..self.wifi_scan.items().len()).map(NetRow::Wifi));
            v.push(NetRow::ScanAgain);
        }
        v
    }

    /// The focusable rows of the Ethernet panel for the current draft.
    pub fn eth_rows(&self) -> Vec<EthRow> {
        let mut v = vec![EthRow::Method];
        if self.eth_draft.as_ref().is_some_and(|d| d.manual) {
            v.extend([EthField::Address, EthField::Gateway, EthField::Dns].map(EthRow::Field));
        }
        v.push(EthRow::Save);
        v
    }

    /// Open the Ethernet panel with the current settings as the draft.
    pub fn open_ethernet(&mut self, ctx: &ShellContext) -> Vec<Effect> {
        let Some(e) = &ctx.sys.ethernet else {
            return vec![Effect::Toast("This TV has no Ethernet port".into())];
        };
        self.eth_draft = Some(crate::net::IpConfig::from_ethernet(e));
        self.overlays.push(Overlay::Ethernet { index: 0 });
        vec![]
    }

    /// Wi-Fi list row `i` chosen (Settings or Quick settings → Network):
    /// join it (asking for the password) or, past the end, scan again.
    fn choose_wifi(&mut self, i: usize) -> Vec<Effect> {
        if i == self.wifi_scan.items().len() {
            if self.wifi_scan != Scan::Busy {
                self.wifi_scan = Scan::Busy;
                return vec![Effect::ScanWifi];
            }
            return vec![];
        }
        let net = self.wifi_scan.items()[i].clone();
        if net.in_use {
            return vec![Effect::Toast(format!("Already connected to {}", net.ssid))];
        }
        if net.secure {
            self.overlays.push(Overlay::WifiPassword {
                ssid: net.ssid,
                osk: Box::new(Osk::new(true, "Connect")),
            });
            return vec![];
        }
        self.wifi_connecting = Some(net.ssid.clone());
        vec![Effect::ConnectWifi {
            ssid: net.ssid,
            password: None,
        }]
    }

    fn activate_quick(&mut self, item: QuickItem, ctx: &ShellContext) -> Vec<Effect> {
        match item {
            QuickItem::Network => {
                self.overlays.push(Overlay::QuickNetwork { index: 0 });
                if ctx.sys.wifi_on == Some(true) && self.wifi_scan != Scan::Busy {
                    self.wifi_scan = Scan::Busy;
                    return vec![Effect::ScanWifi];
                }
                vec![]
            }
            QuickItem::Bluetooth => {
                if ctx.sys.bluetooth.available {
                    self.overlays.push(Overlay::QuickBluetooth { index: 0 });
                    vec![]
                } else {
                    vec![Effect::Toast("No Bluetooth adapter".into())]
                }
            }
            QuickItem::Mute => vec![Effect::ToggleMute],
            QuickItem::Settings => {
                self.open_page(Page::Settings);
                vec![]
            }
            QuickItem::Sleep => vec![sleep_effect(ctx)],
            QuickItem::Power => {
                self.overlays.push(Overlay::Power { index: 0 });
                vec![]
            }
        }
    }

    // ------------------------------------------------------------------
    // Async results reported by the GUI
    // ------------------------------------------------------------------

    pub fn on_wifi_scan(&mut self, result: Result<Vec<WifiNetwork>, String>) {
        self.wifi_scan = match result {
            Ok(n) => Scan::Done(n),
            Err(e) => Scan::Failed(e),
        };
    }

    pub fn on_wifi_connect(&mut self, ssid: &str, result: Result<(), String>) -> Vec<Effect> {
        self.wifi_connecting = None;
        match result {
            Ok(()) => {
                self.overlays
                    .retain(|o| !matches!(o, Overlay::WifiPassword { .. }));
                vec![Effect::Toast(format!("Connected to {ssid}"))]
            }
            Err(e) => vec![Effect::Toast(e)],
        }
    }

    pub fn on_bt_scan(&mut self, result: Result<Vec<Device>, String>) {
        self.bt_scan = match result {
            Ok(d) => Scan::Done(d),
            Err(e) => Scan::Failed(e),
        };
    }

    /// Result of pairing `mac` (pair + trust + connect); `Ok(name)`.
    pub fn on_bt_paired(&mut self, mac: &str, result: Result<String, String>) -> Vec<Effect> {
        if self.bt_busy.as_deref() == Some(mac) {
            self.bt_busy = None;
        }
        match result {
            Ok(name) => {
                self.overlays
                    .retain(|o| !matches!(o, Overlay::BtScan { .. }));
                vec![Effect::Toast(format!("Connected to {name}"))]
            }
            Err(e) => vec![Effect::Toast(e)],
        }
    }

    /// HDR was just switched on (the TV session restarted with it): ask
    /// whether the picture is fine; no answer turns it off again, so a TV
    /// that goes black with HDR recovers by itself.
    pub fn confirm_hdr(&mut self) {
        self.hdr_pending = true;
        self.overlays.push(Overlay::DisplayConfirm { index: 1 });
    }

    /// The confirmation countdown ran out: revert to the previous mode
    /// (or turn HDR off again).
    pub fn on_display_timeout(&mut self) -> Vec<Effect> {
        self.overlays
            .retain(|o| !matches!(o, Overlay::DisplayConfirm { .. }));
        if std::mem::take(&mut self.hdr_pending) {
            return vec![Effect::SetHdr(false)];
        }
        match self.display_pending.take() {
            Some(p) => vec![Effect::RevertMode {
                output: p.output,
                mode: p.previous,
            }],
            None => vec![],
        }
    }

    pub fn on_store_search(&mut self, result: Result<Vec<StoreItem>, String>) {
        self.store_results = match result {
            Ok(v) => Scan::Done(v),
            Err(e) => Scan::Failed(e),
        };
        self.store_index = 0;
    }

    /// Progress of a store job; `None` = finished (success or failure).
    pub fn on_store_job(&mut self, id: &str, job: Option<StoreJob>) {
        self.store_jobs.retain(|(i, _)| i != id);
        if let Some(j) = job {
            self.store_jobs.push((id.to_string(), j));
        }
    }

    /// A store job ended: the Downloads line shows the outcome, and an
    /// installed app reads "Installed" right away (the tile fell
    /// back to "Get" until the next system refresh).
    pub fn on_store_done(&mut self, id: &str, result: Result<(), String>) {
        self.on_store_job(id, None);
        // Its page is open: focus goes back to the buttons that came back
        // (Open after an install, Install after a removal).
        if let Some(Overlay::StoreApp { item, index }) = self.overlays.last_mut()
            && item.id == id
        {
            *index = 0;
        }
        if let Some(e) = self.store_log.iter_mut().find(|e| e.item.id == id) {
            e.snapshots = 0;
            e.state = match (&e.state, result) {
                (_, Err(err)) => StoreLogState::Failed(err),
                (StoreLogState::Removing, Ok(())) => StoreLogState::Removed,
                (_, Ok(())) => StoreLogState::Installed,
            };
        }
    }

    /// Is `id` installed, counting installs that finished before the
    /// system list caught up (and removals that did)?
    pub fn store_installed(&self, ctx: &ShellContext, id: &str) -> bool {
        installed_per_log(&self.store_log, ctx, id)
    }

    /// A new system list arrived: lines it confirms (or that it has had
    /// three chances to confirm) stop overriding it.
    pub fn settle_store_log(&mut self, ctx: &ShellContext) {
        for e in &mut self.store_log {
            let listed = ctx.sys.flatpaks.iter().any(|f| f.id == e.item.id);
            let confirmed = match e.state {
                StoreLogState::Installed => listed,
                StoreLogState::Removed => !listed,
                _ => continue,
            };
            e.snapshots += 1;
            if confirmed || e.snapshots >= 3 {
                e.state = StoreLogState::Done;
            }
        }
    }

    pub fn on_apks(&mut self, result: Result<Vec<android::FileEntry>, String>) {
        self.apks = match result {
            Ok(v) => Scan::Done(v),
            Err(e) => Scan::Failed(e),
        };
    }
}

#[cfg(test)]
mod tests;
