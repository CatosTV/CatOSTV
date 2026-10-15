//! catostv-installer, remote-first installer shown on the live ISO.
//!
//! Wizard logic: `crates/catostv-core/src/installer.rs`.
//! Install plan + safety checks: `crates/catostv-core/src/install.rs`, `disk.rs`.
//!
//! Modes:
//! * live ISO: `CATOSTV_INSTALLER_ALLOW_DESTRUCTIVE=1` → real install;
//! * `CATOSTV_INSTALLER_DRY_RUN=1` → every step is simulated, nothing is
//!   erased, but the target-disk safety check still runs for real;
//! * neither → the plan's first check refuses to touch any disk.

use catostv::canvas::Canvas;
use catostv::pointer::Pointer;
use catostv::ui::installer as view;
use catostv::{jobs::Jobs, keys, theme};
use catostv_core::disk::{self, DiskCandidate};
use catostv_core::input::{Action, InputMapper};
use catostv_core::install::{self, DryRunHost, InstallConfig, InstallEvent, RealHost};
use catostv_core::installer::{Effect, Installer, Step, Target};
use catostv_core::net::{self, WifiNetwork};
use catostv_core::process::{Cmd, Runner, SystemRunner};
use catostv_core::{env, keyboard};
use eframe::egui::{self, Rect};
use std::time::{Duration, Instant};

const NET_REFRESH_SECS: f64 = 4.0;

enum Msg {
    Wifi(Result<Vec<WifiNetwork>, String>),
    WifiConnect(Result<(), String>),
    Online(bool),
    Disks(Result<Vec<DiskCandidate>, String>),
    Install(InstallEvent),
    Notice(String),
}

struct InstallerApp {
    flow: Installer,
    mapper: InputMapper,
    pointer: Pointer,
    hits: Vec<(Rect, Target)>,
    hover: Option<Target>,
    jobs: Jobs<Msg>,
    last_net_refresh: Option<Instant>,
    net_refreshing: bool,
    was_focused: bool,
}

impl InstallerApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::setup(&cc.egui_ctx);
        Self {
            flow: Installer {
                // The boot menu's graphics choice and the cards
                // found.
                gpu: catostv_core::gpu::GpuChoice::current(),
                gpus: catostv_core::gpu::detect(&mut SystemRunner),
                ..Installer::new(env::is_dry_run())
            },
            mapper: InputMapper::new(),
            pointer: Pointer::new(),
            hits: Vec::new(),
            hover: None,
            jobs: Jobs::new(&cc.egui_ctx),
            last_net_refresh: None,
            net_refreshing: false,
            was_focused: true,
        }
    }

    fn refresh_network(&mut self) {
        if self.net_refreshing {
            return;
        }
        self.net_refreshing = self.jobs.try_spawn(|| {
            Msg::Online(
                net::status(&mut SystemRunner)
                    .map(|s| s.online)
                    .unwrap_or(false),
            )
        });
    }

    fn apply(&mut self, effect: Effect) {
        match effect {
            Effect::SetKeyboard(layout) => self.set_keyboard(layout),
            Effect::ScanWifi => self
                .jobs
                .spawn(|| Msg::Wifi(net::scan(&mut SystemRunner).map_err(|e| e.to_string()))),
            Effect::ConnectWifi { ssid, password } => self.jobs.spawn(move || {
                Msg::WifiConnect(
                    net::connect(&mut SystemRunner, &ssid, password.as_deref())
                        .map_err(|e| e.to_string()),
                )
            }),
            Effect::RefreshNetwork => self.refresh_network(),
            Effect::ScanDisks => self
                .jobs
                .spawn(|| Msg::Disks(disk::scan(&mut SystemRunner).map_err(|e| e.to_string()))),
            Effect::StartInstall {
                disk,
                timezone,
                keyboard,
            } => self.start_install(disk, timezone, keyboard),
            Effect::Reboot | Effect::PowerOff => {
                let verb = if effect == Effect::Reboot {
                    "reboot"
                } else {
                    "poweroff"
                };
                if self.flow.dry_run {
                    self.flow.notice = Some(format!("Dry run: the PC would {verb} now."));
                    return;
                }
                self.jobs.spawn(move || {
                    match SystemRunner.run_ok(&Cmd::new("systemctl").arg(verb)) {
                        Ok(_) => Msg::Notice(String::new()),
                        Err(e) => Msg::Notice(e.to_string()),
                    }
                });
            }
        }
    }

    /// Apply the chosen layout to the live session: rewrite labwc's
    /// environment file and ask labwc to reload it (labwc re-reads
    /// environment files on --reconfigure). Status: UNVERIFIED on the ISO.
    fn set_keyboard(&self, layout: String) {
        if self.flow.dry_run || !env::destructive_allowed() {
            return; // development: never touch the developer's labwc config
        }
        self.jobs.spawn(move || {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
            let path = format!("{home}/.config/labwc/environment");
            let old = std::fs::read_to_string(&path).unwrap_or_default();
            let new = keyboard::set_layout_in_environment(&old, &layout);
            if let Err(e) = std::fs::write(&path, new) {
                return Msg::Notice(format!("Could not set the keyboard: {e}"));
            }
            let _ = SystemRunner.run(&Cmd::new("labwc").arg("--reconfigure"));
            Msg::Notice(String::new())
        });
    }

    fn start_install(&mut self, disk: String, timezone: String, keyboard: String) {
        let (tx, ctx) = self.jobs.sender();
        let dry_run = self.flow.dry_run;
        let drivers = self.flow.drivers();
        let spawned = std::thread::Builder::new()
            .name("catostv-install".into())
            .spawn(move || {
                let send = |ev: InstallEvent| {
                    let _ = tx.send(Msg::Install(ev));
                    ctx.request_repaint();
                };
                let fail = |message: String| {
                    send(InstallEvent::Failed {
                        message,
                        disk_modified: false,
                    })
                };
                let cpu = install::detect_cpu(
                    &std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default(),
                );
                let plan = match InstallConfig::new(&disk, &timezone, &keyboard, cpu).and_then(
                    |mut cfg| {
                        cfg.drivers = drivers;
                        install::plan(&cfg)
                    },
                ) {
                    Ok(p) => p,
                    Err(e) => return fail(e.to_string()),
                };
                let mut emit = |ev: InstallEvent| send(ev);
                // Errors are reported through InstallEvent::Failed.
                let _ = if dry_run {
                    let mut host = DryRunHost::new(SystemRunner, Duration::from_millis(250));
                    install::execute(&plan, &mut host, &mut emit)
                } else {
                    let mut host = RealHost::new(env::destructive_allowed());
                    install::execute(&plan, &mut host, &mut emit)
                };
            });
        if let Err(e) = spawned {
            self.flow.on_install_event(InstallEvent::Failed {
                message: format!("could not start the installer thread: {e}"),
                disk_modified: false,
            });
        }
    }

    fn handle_message(&mut self, msg: Msg) {
        match msg {
            Msg::Wifi(r) => self.flow.on_wifi_scan(r),
            Msg::WifiConnect(r) => {
                for e in self.flow.on_wifi_connect(r) {
                    self.apply(e);
                }
            }
            Msg::Online(online) => {
                self.net_refreshing = false;
                self.flow.on_network_status(online);
            }
            Msg::Disks(r) => self.flow.on_disks(r),
            Msg::Install(ev) => self.flow.on_install_event(ev),
            Msg::Notice(n) if !n.is_empty() => self.flow.notice = Some(n),
            Msg::Notice(_) => {}
        }
    }
}

impl eframe::App for InstallerApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for msg in self.jobs.drain() {
            self.handle_message(msg);
        }
        let due = self
            .last_net_refresh
            .is_none_or(|t| t.elapsed().as_secs_f64() > NET_REFRESH_SECS);
        if self.flow.step == Step::Network && due {
            self.last_net_refresh = Some(Instant::now());
            self.refresh_network();
        }
        ctx.request_repaint_after(Duration::from_millis(300));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let focused = keys::window_focused(&ctx);
        if !focused && self.was_focused {
            self.mapper.reset();
        }
        self.was_focused = focused;

        let events = keys::collect(&ctx);
        let mut effects = Vec::new();
        for action in self
            .mapper
            .map(&events, keys::now(&ctx), self.flow.text_mode())
        {
            effects.extend(self.flow.handle(action));
        }
        // Mouse: hover focuses, click = OK, right click = Back.
        let p = self.pointer.collect(&ctx);
        if let Some(pos) = p.moved_to {
            let t = view::hit_test(&self.hits, pos);
            if t != self.hover {
                self.hover = t;
                if let Some(t) = t {
                    effects.extend(self.flow.pointer(t, false));
                }
            }
        }
        if let Some(pos) = p.click
            && let Some(t) = view::hit_test(&self.hits, pos)
        {
            effects.extend(self.flow.pointer(t, true));
        }
        if p.back {
            effects.extend(self.flow.handle(Action::Back));
        }
        for e in effects {
            self.apply(e);
        }
        let scanning = matches!(self.flow.disks, catostv_core::installer::Scan::Busy)
            || matches!(self.flow.wifi, catostv_core::installer::Scan::Busy);
        if self.mapper.needs_tick()
            || matches!(self.flow.step, Step::Installing | Step::Welcome)
            || scanning
        {
            ctx.request_repaint();
        }

        let c = Canvas::new(ui, 0);
        view::draw(&c, &self.flow, keys::now(&ctx));
        self.hits = c.take_hits();
    }
}

fn main() -> eframe::Result {
    catostv::run_app("catostv-installer", "Install CatosTV", InstallerApp::new)
}
