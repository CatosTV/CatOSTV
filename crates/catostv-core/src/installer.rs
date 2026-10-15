//! Installer wizard state machine (remote-only).
//!
//! ```text
//! Welcome → Keyboard → Network ─┬→ Wi-Fi password ─┐
//!                               └──────────────────┴→ Disk → Time zone → Confirm → Installing → Done
//!                                                                                         └→ Failed
//! ```
//!
//! Like [`crate::shell`], this module only decides focus and meaning; the
//! GUI performs [`Effect`]s on worker threads and reports results back.
//!
//! Guarantees enforced here (and unit-tested):
//! * a blocked disk can never be selected;
//! * the confirmation page always opens with **Cancel** focused;
//! * Continue from Network requires an internet connection (CatosTV downloads
//!   its system with pacstrap), except in dry-run mode;
//! * no input is accepted while installing.

use crate::disk::DiskCandidate;
use crate::input::Action;
use crate::install::InstallEvent;
use crate::keyboard;
use crate::net::WifiNetwork;
use crate::osk::{Osk, OskEvent};
use crate::timezone;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Keyboard,
    Network,
    WifiPassword,
    Disk,
    Timezone,
    Confirm,
    Installing,
    Done,
    Failed,
}

impl Step {
    /// Index in the sidebar progress list (`STAGES`).
    pub fn stage(self) -> usize {
        match self {
            Self::Welcome => 0,
            Self::Keyboard => 1,
            Self::Network | Self::WifiPassword => 2,
            Self::Disk => 3,
            Self::Timezone => 4,
            Self::Confirm | Self::Installing | Self::Failed => 5,
            Self::Done => 6,
        }
    }
}

pub const STAGES: [&str; 7] = [
    "Welcome",
    "Keyboard",
    "Network",
    "Disk",
    "Time zone",
    "Install",
    "Done",
];

/// Characters kept in the keyboard step's "try typing" field.
pub const KB_TEST_MAX: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scan<T> {
    Idle,
    Busy,
    Done(Vec<T>),
    Failed(String),
}

impl<T> Scan<T> {
    pub fn items(&self) -> &[T] {
        match self {
            Scan::Done(v) => v,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Apply an XKB layout to the live installer session now.
    SetKeyboard(String),
    ScanWifi,
    ConnectWifi {
        ssid: String,
        password: Option<String>,
    },
    /// Re-check whether the PC is online (Ethernet may connect by itself).
    RefreshNetwork,
    ScanDisks,
    StartInstall {
        disk: String,
        timezone: String,
        keyboard: String,
    },
    Reboot,
    PowerOff,
}

/// What the mouse pointer is over (registered by the GUI while drawing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Entry `i` of the current step's list or button row.
    Item(usize),
    /// A key of the Wi-Fi password keyboard.
    Key(usize, usize),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Progress {
    pub index: usize,
    pub total: usize,
    pub label: String,
    /// What the current step is doing right now ("Installing mesa").
    pub detail: String,
    /// The current step's own progress, when it reports one.
    pub step_fraction: Option<f32>,
    /// Steps already finished, in order (shown ticked on the screen).
    pub done: Vec<String>,
}

impl Progress {
    /// 0.0–1.0 for a progress bar.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            ((self.index as f32 + self.step_fraction.unwrap_or(0.0).clamp(0.0, 1.0))
                / self.total as f32)
                .min(1.0)
        }
    }
}

#[derive(Debug, Clone)]
pub struct Installer {
    pub step: Step,
    pub dry_run: bool,
    /// Welcome: 0 = Start, 1 = Power off. Done: 0 = Restart, 1 = Power off.
    /// Failed: 0 = Try again, 1 = Power off.
    pub choice: usize,
    /// Index into [`keyboard::LAYOUTS`].
    pub kb_index: usize,
    /// Text typed on the keyboard step to try the layout.
    pub kb_test: String,
    pub wifi: Scan<WifiNetwork>,
    /// Network list focus. Items: networks…, "Search again", "Continue".
    pub net_index: usize,
    pub online: bool,
    pub connecting: Option<String>,
    pub ssid: String,
    pub osk: Osk,
    pub disks: Scan<DiskCandidate>,
    /// Disk list focus. Items: disks…, "Search again".
    pub disk_index: usize,
    pub tz_index: usize,
    /// 0 = Cancel, 1 = Erase & install.
    pub confirm_index: usize,
    pub progress: Progress,
    pub failure: Option<(String, bool)>,
    /// One-line message shown on the current page (validation, errors).
    pub notice: Option<String>,
    /// Graphics from the boot menu (`catostv.gpu=`).
    pub gpu: crate::gpu::GpuChoice,
    /// The graphics cards found.
    pub gpus: Vec<crate::gpu::Gpu>,
}

impl Installer {
    /// The graphics drivers this install gets.
    pub fn drivers(&self) -> crate::gpu::DriverPlan {
        crate::gpu::plan(self.gpu, &self.gpus)
    }

    pub fn new(dry_run: bool) -> Self {
        Self {
            step: Step::Welcome,
            dry_run,
            choice: 0,
            kb_index: 0,
            kb_test: String::new(),
            wifi: Scan::Idle,
            net_index: 0,
            online: false,
            connecting: None,
            ssid: String::new(),
            osk: Osk::new(true, "Connect"),
            disks: Scan::Idle,
            disk_index: 0,
            tz_index: 0,
            confirm_index: 0,
            progress: Progress::default(),
            failure: None,
            notice: None,
            gpu: crate::gpu::GpuChoice::default(),
            gpus: Vec::new(),
        }
    }

    pub fn text_mode(&self) -> bool {
        (self.step == Step::WifiPassword && self.connecting.is_none())
            || self.step == Step::Keyboard
    }

    pub fn keyboard(&self) -> &'static str {
        keyboard::LAYOUTS[self.kb_index.min(keyboard::LAYOUTS.len() - 1)].0
    }

    pub fn timezone(&self) -> &'static str {
        timezone::COMMON[self.tz_index.min(timezone::COMMON.len() - 1)]
    }

    pub fn selected_disk(&self) -> Option<&DiskCandidate> {
        self.disks.items().get(self.disk_index)
    }

    fn goto(&mut self, step: Step) {
        self.step = step;
        self.notice = None;
        self.choice = 0;
    }

    fn enter_network(&mut self) -> Vec<Effect> {
        self.goto(Step::Network);
        self.net_index = 0;
        self.wifi = Scan::Busy;
        vec![Effect::ScanWifi, Effect::RefreshNetwork]
    }

    fn enter_disks(&mut self) -> Vec<Effect> {
        self.goto(Step::Disk);
        self.disk_index = 0;
        self.disks = Scan::Busy;
        vec![Effect::ScanDisks]
    }

    pub fn handle(&mut self, action: Action) -> Vec<Effect> {
        if action == Action::Back {
            return self.back();
        }
        match self.step {
            Step::Welcome | Step::Done | Step::Failed => self.handle_choice(action),
            Step::Network => self.handle_network(action),
            Step::WifiPassword => self.handle_password(action),
            Step::Keyboard => {
                let n = keyboard::LAYOUTS.len();
                match action {
                    // Moving the highlight applies the layout right away so
                    // the "try typing" field tests the highlighted layout.
                    Action::Up | Action::Down => {
                        let before = self.kb_index;
                        self.kb_index = if action == Action::Up {
                            before.saturating_sub(1)
                        } else {
                            (before + 1).min(n - 1)
                        };
                        if self.kb_index != before {
                            return vec![Effect::SetKeyboard(self.keyboard().to_string())];
                        }
                    }
                    Action::Char(c) if self.kb_test.chars().count() < KB_TEST_MAX => {
                        self.kb_test.push(c)
                    }
                    Action::Erase => {
                        self.kb_test.pop();
                    }
                    Action::Ok => {
                        let layout = self.keyboard().to_string();
                        let mut effects = vec![Effect::SetKeyboard(layout)];
                        effects.extend(self.enter_network());
                        return effects;
                    }
                    _ => {}
                }
                vec![]
            }
            Step::Disk => self.handle_disk(action),
            Step::Timezone => {
                let n = timezone::COMMON.len();
                match action {
                    Action::Up => self.tz_index = self.tz_index.saturating_sub(1),
                    Action::Down => self.tz_index = (self.tz_index + 1).min(n - 1),
                    Action::Ok => {
                        self.goto(Step::Confirm);
                        self.confirm_index = 0;
                    }
                    _ => {}
                }
                vec![]
            }
            Step::Confirm => {
                match action {
                    Action::Left => self.confirm_index = 0,
                    Action::Right => self.confirm_index = 1,
                    Action::Ok if self.confirm_index == 0 => self.goto(Step::Disk),
                    Action::Ok => {
                        let Some(disk) = self.selected_disk().filter(|d| d.eligible()) else {
                            self.goto(Step::Disk);
                            return vec![];
                        };
                        let disk = disk.disk.path.clone();
                        self.goto(Step::Installing);
                        self.progress = Progress {
                            index: 0,
                            total: 1,
                            label: String::new(),
                            ..Progress::default()
                        };
                        return vec![Effect::StartInstall {
                            disk,
                            timezone: self.timezone().to_string(),
                            keyboard: self.keyboard().to_string(),
                        }];
                    }
                    _ => {}
                }
                vec![]
            }
            Step::Installing => vec![],
        }
    }

    fn back(&mut self) -> Vec<Effect> {
        match self.step {
            Step::Keyboard => self.goto(Step::Welcome),
            Step::Network => self.goto(Step::Keyboard),
            Step::WifiPassword if self.connecting.is_none() => self.step = Step::Network,
            Step::Disk => self.step = Step::Network,
            Step::Timezone => self.step = Step::Disk,
            Step::Confirm => self.step = Step::Timezone,
            _ => {}
        }
        self.notice = None;
        vec![]
    }

    fn handle_choice(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::Up | Action::Left => self.choice = 0,
            Action::Down | Action::Right => self.choice = 1,
            Action::Ok => {
                return match (self.step, self.choice) {
                    (Step::Welcome, 0) => {
                        self.goto(Step::Keyboard);
                        vec![]
                    }
                    (Step::Done, 0) => vec![Effect::Reboot],
                    (Step::Failed, 0) => self.enter_disks(),
                    _ => vec![Effect::PowerOff],
                };
            }
            _ => {}
        }
        vec![]
    }

    fn handle_network(&mut self, action: Action) -> Vec<Effect> {
        let nets = self.wifi.items().len();
        let rescan = nets;
        let cont = nets + 1;
        // The list can shrink under the highlight (a scan running or
        // finished with fewer networks): never point past "Continue".
        self.net_index = self.net_index.min(cont);
        match action {
            Action::Up => self.net_index = self.net_index.saturating_sub(1),
            Action::Down => self.net_index = (self.net_index + 1).min(cont),
            Action::Ok if self.connecting.is_some() => {}
            Action::Ok if self.net_index == rescan => {
                if self.wifi != Scan::Busy {
                    self.wifi = Scan::Busy;
                    return vec![Effect::ScanWifi, Effect::RefreshNetwork];
                }
            }
            Action::Ok if self.net_index == cont => {
                if self.online || self.dry_run {
                    return self.enter_disks();
                }
                self.notice = Some(
                    "Connect to the internet first. CatosTV downloads its system during installation."
                        .into(),
                );
                return vec![Effect::RefreshNetwork];
            }
            Action::Ok => {
                let Some(net) = self.wifi.items().get(self.net_index).cloned() else {
                    return vec![];
                };
                if net.in_use {
                    self.notice = Some(format!("Already connected to {}", net.ssid));
                } else if net.secure {
                    self.ssid = net.ssid;
                    self.osk = Osk::new(true, "Connect");
                    self.step = Step::WifiPassword;
                    self.notice = None;
                } else {
                    self.connecting = Some(net.ssid.clone());
                    self.notice = Some(format!("Joining {}…", net.ssid));
                    return vec![Effect::ConnectWifi {
                        ssid: net.ssid,
                        password: None,
                    }];
                }
            }
            _ => {}
        }
        vec![]
    }

    fn handle_password(&mut self, action: Action) -> Vec<Effect> {
        if self.connecting.is_some() {
            return vec![];
        }
        if self.osk.handle(action) == OskEvent::Submitted {
            let pw = self.osk.text().to_string();
            if pw.chars().count() < 8 {
                self.notice = Some("Wi-Fi passwords have at least 8 characters.".into());
                return vec![];
            }
            self.connecting = Some(self.ssid.clone());
            self.notice = Some(format!("Joining {}…", self.ssid));
            return vec![Effect::ConnectWifi {
                ssid: self.ssid.clone(),
                password: Some(pw),
            }];
        }
        vec![]
    }

    fn handle_disk(&mut self, action: Action) -> Vec<Effect> {
        let n = self.disks.items().len();
        let rescan = n;
        self.disk_index = self.disk_index.min(rescan);
        match action {
            Action::Up => self.disk_index = self.disk_index.saturating_sub(1),
            Action::Down => self.disk_index = (self.disk_index + 1).min(rescan),
            Action::Ok if self.disk_index == rescan => {
                if self.disks != Scan::Busy {
                    return self.enter_disks();
                }
            }
            Action::Ok => {
                let Some(d) = self.disks.items().get(self.disk_index) else {
                    return vec![];
                };
                match &d.blocked {
                    Some(reason) => self.notice = Some(format!("Can't use this disk: {reason}")),
                    None => self.goto(Step::Timezone),
                }
            }
            _ => {}
        }
        if self.step == Step::Disk && action != Action::Ok {
            self.notice = None;
        }
        vec![]
    }

    /// Pointer hover (`click == false`) or click. A click on an item is
    /// the same as focusing it and pressing OK; nothing else is possible
    /// with the mouse that the remote cannot do.
    pub fn pointer(&mut self, target: Target, click: bool) -> Vec<Effect> {
        if !self.focus_target(target) {
            return vec![];
        }
        if click {
            self.handle(Action::Ok)
        } else {
            vec![]
        }
    }

    fn focus_target(&mut self, target: Target) -> bool {
        match (self.step, target) {
            (Step::WifiPassword, Target::Key(r, c)) if self.connecting.is_none() => {
                self.osk.set_focus(r, c);
                true
            }
            (Step::Welcome | Step::Done | Step::Failed, Target::Item(i)) if i < 2 => {
                self.choice = i;
                true
            }
            (Step::Keyboard, Target::Item(i)) if i < keyboard::LAYOUTS.len() => {
                self.kb_index = i;
                true
            }
            (Step::Network, Target::Item(i)) if i < self.wifi.items().len() + 2 => {
                self.net_index = i;
                true
            }
            (Step::Disk, Target::Item(i)) if i <= self.disks.items().len() => {
                self.disk_index = i;
                true
            }
            (Step::Timezone, Target::Item(i)) if i < timezone::COMMON.len() => {
                self.tz_index = i;
                true
            }
            (Step::Confirm, Target::Item(i)) if i < 2 => {
                self.confirm_index = i;
                true
            }
            _ => false,
        }
    }

    // ---------------- results from the GUI ----------------

    pub fn on_wifi_scan(&mut self, result: Result<Vec<WifiNetwork>, String>) {
        self.wifi = match result {
            Ok(v) => Scan::Done(v),
            Err(e) => Scan::Failed(e),
        };
        let max = self.wifi.items().len() + 1;
        self.net_index = self.net_index.min(max);
    }

    pub fn on_network_status(&mut self, online: bool) {
        let was = self.online;
        self.online = online;
        if online && !was && self.step == Step::Network {
            self.notice = Some("Connected to the internet.".into());
            // Jump to Continue so one OK moves on.
            self.net_index = self.wifi.items().len() + 1;
        }
    }

    pub fn on_wifi_connect(&mut self, result: Result<(), String>) -> Vec<Effect> {
        self.connecting = None;
        match result {
            Ok(()) => {
                self.step = Step::Network;
                self.notice = Some(format!("Connected to {}.", self.ssid));
                self.net_index = self.wifi.items().len() + 1;
                vec![Effect::RefreshNetwork, Effect::ScanWifi]
            }
            Err(e) => {
                self.notice = Some(e);
                vec![]
            }
        }
    }

    pub fn on_disks(&mut self, result: Result<Vec<DiskCandidate>, String>) {
        self.disks = match result {
            Ok(v) => Scan::Done(v),
            Err(e) => Scan::Failed(e),
        };
        // Focus the first eligible disk.
        self.disk_index = self
            .disks
            .items()
            .iter()
            .position(DiskCandidate::eligible)
            .unwrap_or(0);
    }

    pub fn on_install_event(&mut self, ev: InstallEvent) {
        match ev {
            InstallEvent::Step {
                index,
                total,
                label,
            } => {
                let mut done = std::mem::take(&mut self.progress.done);
                let prev = std::mem::take(&mut self.progress.label);
                if index > 0 && !prev.is_empty() && !done.contains(&prev) && prev != label {
                    done.push(prev);
                }
                self.progress = Progress {
                    index,
                    total,
                    label,
                    detail: String::new(),
                    step_fraction: None,
                    done,
                }
            }
            InstallEvent::Detail { text, fraction } => {
                self.progress.detail = text;
                if fraction.is_some() {
                    self.progress.step_fraction = fraction;
                }
            }
            InstallEvent::Finished => {
                self.progress.index = self.progress.total;
                self.goto(Step::Done);
            }
            InstallEvent::Failed {
                message,
                disk_modified,
            } => {
                self.failure = Some((message, disk_modified));
                self.goto(Step::Failed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::{BlockDevice, DiskCandidate};
    use Action::*;

    #[test]
    fn finished_steps_are_listed() {
        let mut f = Installer::new(true);
        let step = |i: usize, l: &str| InstallEvent::Step {
            index: i,
            total: 4,
            label: l.into(),
        };
        f.on_install_event(step(0, "Checking your PC"));
        f.on_install_event(step(1, "Clearing the disk"));
        f.on_install_event(InstallEvent::Detail {
            text: "x".into(),
            fraction: None,
        });
        f.on_install_event(step(2, "Downloading CatosTV"));
        assert_eq!(f.progress.done, ["Checking your PC", "Clearing the disk"]);
        assert_eq!(f.progress.label, "Downloading CatosTV");
    }

    #[test]
    fn ok_during_a_rescan_never_points_past_the_list() {
        // Bug found in the 1.0 review: "Search again" with the highlight
        // low in the list, then OK while the scan runs, indexed past the
        // (now empty) list and crashed the installer.
        let mut f = Installer::new(false);
        f.handle(Ok); // Welcome → Keyboard
        f.handle(Ok); // → Network (scan starts)
        let net = |s: &str| WifiNetwork {
            ssid: s.into(),
            signal: 50,
            secure: true,
            in_use: false,
        };
        f.on_wifi_scan(Result::Ok(vec![net("a"), net("b"), net("c")]));
        f.net_index = 3; // "Search again"
        assert_eq!(f.handle(Ok), vec![Effect::ScanWifi, Effect::RefreshNetwork]);
        f.net_index = 2; // the highlight stays low while the list is empty
        let _ = f.handle(Ok);
        assert!(f.net_index <= 1);
        // Same for disks.
        f.disks = Scan::Done(vec![disk("/dev/vda", None), disk("/dev/vdb", None)]);
        f.step = Step::Disk;
        f.disk_index = 2;
        f.disks = Scan::Busy;
        let _ = f.handle(Ok);
        assert!(f.disk_index == 0);
    }
    fn disk(path: &str, blocked: Option<&str>) -> DiskCandidate {
        DiskCandidate {
            disk: BlockDevice {
                name: path.trim_start_matches("/dev/").into(),
                path: path.into(),
                kind: "disk".into(),
                size: 64_000_000_000,
                ..BlockDevice::default()
            },
            blocked: blocked.map(String::from),
            warnings: vec![],
        }
    }

    fn net(ssid: &str, secure: bool) -> WifiNetwork {
        WifiNetwork {
            ssid: ssid.into(),
            signal: 60,
            secure,
            in_use: false,
        }
    }

    fn press(i: &mut Installer, actions: &[Action]) -> Vec<Effect> {
        actions.iter().flat_map(|a| i.handle(*a)).collect()
    }

    #[test]
    fn happy_path_with_ethernet() {
        let mut i = Installer::new(false);
        assert!(press(&mut i, &[Ok]).is_empty());
        assert_eq!(i.step, Step::Keyboard);
        assert!(i.text_mode(), "the keyboard step has a try-typing field");
        assert_eq!(
            press(&mut i, &[Down]),
            vec![Effect::SetKeyboard("gb".into())],
            "moving the highlight applies the layout live"
        );
        assert!(
            press(&mut i, &[Up, Up]).len() == 1,
            "no effect at the top edge"
        );
        press(&mut i, &[Down, Char('ä'), Char('b'), Erase]);
        assert_eq!(i.kb_test, "ä");
        assert_eq!(
            press(&mut i, &[Ok]),
            vec![
                Effect::SetKeyboard("gb".into()),
                Effect::ScanWifi,
                Effect::RefreshNetwork
            ]
        );
        i.on_wifi_scan(Result::Ok(vec![net("Home", true)]));
        i.on_network_status(true);
        assert_eq!(i.net_index, 2, "focus jumps to Continue when online");
        assert_eq!(press(&mut i, &[Ok]), vec![Effect::ScanDisks]);
        i.on_disks(Result::Ok(vec![
            disk("/dev/nvme0n1", None),
            disk("/dev/sda", Some("This is the CatosTV installer drive")),
        ]));
        press(&mut i, &[Ok]);
        assert_eq!(i.step, Step::Timezone);
        press(&mut i, &[Down, Ok]);
        assert_eq!(i.step, Step::Confirm);
        assert_eq!(i.confirm_index, 0, "Cancel is focused first");
        assert_eq!(
            press(&mut i, &[Right, Ok]),
            vec![Effect::StartInstall {
                disk: "/dev/nvme0n1".into(),
                timezone: "UTC".into(),
                keyboard: "gb".into()
            }]
        );
        assert_eq!(i.step, Step::Installing);
        assert!(press(&mut i, &[Back, Ok, Left]).is_empty());
        i.on_install_event(InstallEvent::Step {
            index: 3,
            total: 11,
            label: "Downloading".into(),
        });
        assert!((i.progress.fraction() - 3.0 / 11.0).abs() < 1e-6);
        i.on_install_event(InstallEvent::Finished);
        assert_eq!(i.step, Step::Done);
        assert_eq!(press(&mut i, &[Ok]), vec![Effect::Reboot]);
    }

    #[test]
    fn blocked_disk_cannot_be_chosen() {
        let mut i = Installer::new(false);
        i.step = Step::Disk;
        i.on_disks(Result::Ok(vec![disk("/dev/sda", Some("Read-only disk"))]));
        press(&mut i, &[Ok]);
        assert_eq!(i.step, Step::Disk);
        assert_eq!(
            i.notice.as_deref(),
            Some("Can't use this disk: Read-only disk")
        );
        // Even if focus is forced onto it, Confirm refuses to start.
        i.step = Step::Confirm;
        i.confirm_index = 1;
        assert!(press(&mut i, &[Ok]).is_empty());
        assert_eq!(i.step, Step::Disk);
    }

    #[test]
    fn offline_continue_is_blocked_unless_dry_run() {
        let mut i = Installer::new(false);
        press(&mut i, &[Ok, Ok]);
        i.on_wifi_scan(Result::Ok(vec![]));
        let eff = press(&mut i, &[Down, Ok]);
        assert_eq!(eff, vec![Effect::RefreshNetwork]);
        assert_eq!(i.step, Step::Network);
        assert!(i.notice.as_ref().unwrap().contains("internet"));

        let mut d = Installer::new(true);
        press(&mut d, &[Ok, Ok]);
        d.on_wifi_scan(Result::Ok(vec![]));
        assert_eq!(press(&mut d, &[Down, Ok]), vec![Effect::ScanDisks]);
    }

    #[test]
    fn wifi_password_flow_and_back_navigation() {
        let mut i = Installer::new(false);
        press(&mut i, &[Ok, Ok]);
        i.on_wifi_scan(Result::Ok(vec![net("Home", true), net("Cafe", false)]));
        press(&mut i, &[Ok]);
        assert_eq!(i.step, Step::WifiPassword);
        assert!(i.text_mode());
        for c in "hunter22".chars() {
            i.handle(Char(c));
        }
        let eff = press(&mut i, &[Ok]);
        assert_eq!(
            eff,
            vec![Effect::ConnectWifi {
                ssid: "Home".into(),
                password: Some("hunter22".into())
            }]
        );
        assert!(!i.text_mode(), "no typing while connecting");
        assert!(press(&mut i, &[Back]).is_empty());
        assert_eq!(i.step, Step::WifiPassword, "cannot leave while connecting");
        i.on_wifi_connect(Err(
            "Could not join Home: the password was not accepted".into()
        ));
        assert_eq!(i.step, Step::WifiPassword);
        press(&mut i, &[Back]);
        assert_eq!(i.step, Step::Network);
        // Open network joins directly.
        let eff = press(&mut i, &[Down, Ok]);
        assert_eq!(
            eff,
            vec![Effect::ConnectWifi {
                ssid: "Cafe".into(),
                password: None
            }]
        );
        let eff = i.on_wifi_connect(Result::Ok(()));
        assert_eq!(eff, vec![Effect::RefreshNetwork, Effect::ScanWifi]);
        press(&mut i, &[Back]);
        assert_eq!(i.step, Step::Keyboard);
        press(&mut i, &[Back]);
        assert_eq!(i.step, Step::Welcome);
    }

    #[test]
    fn pointer_focuses_and_clicks() {
        let mut i = Installer::new(false);
        assert!(i.pointer(Target::Item(1), false).is_empty());
        assert_eq!(i.choice, 1);
        i.pointer(Target::Item(0), true);
        assert_eq!(i.step, Step::Keyboard);
        assert!(i.pointer(Target::Item(999), true).is_empty());
        let eff = i.pointer(Target::Item(2), true);
        assert_eq!(eff[0], Effect::SetKeyboard("de".into()));
        // Keys only work on the password page.
        assert!(i.pointer(Target::Key(1, 1), true).is_empty());
        // Nothing is clickable while installing.
        i.step = Step::Installing;
        for t in [Target::Item(0), Target::Item(1), Target::Key(0, 0)] {
            assert!(i.pointer(t, true).is_empty());
        }
    }

    #[test]
    fn failure_offers_retry_and_reports_disk_state() {
        let mut i = Installer::new(false);
        i.step = Step::Installing;
        i.on_install_event(InstallEvent::Failed {
            message: "Checking your PC: No internet".into(),
            disk_modified: false,
        });
        assert_eq!(i.step, Step::Failed);
        assert!(!i.failure.as_ref().unwrap().1);
        assert_eq!(press(&mut i, &[Down, Ok]), vec![Effect::PowerOff]);
        assert_eq!(press(&mut i, &[Up, Ok]), vec![Effect::ScanDisks]);
        assert_eq!(i.step, Step::Disk);
    }
}
