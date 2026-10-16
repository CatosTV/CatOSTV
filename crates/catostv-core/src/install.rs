//! Installer backend: a declarative install **plan** and an **executor**.
//!
//! [`plan`] turns an [`InstallConfig`] into an ordered list of [`Step`]s made
//! of plain-data [`Op`]s. Nothing is executed while planning, so the exact
//! plan can be printed (`catostv-diag plan`), reviewed in docs and unit-tested.
//!
//! [`execute`] runs a plan against a [`Host`]:
//!
//! * [`RealHost`] touches the real machine. It is only usable from the live
//!   ISO, where `CATOSTV_INSTALLER_ALLOW_DESTRUCTIVE=1` is set, and it writes a
//!   log to [`LOG_PATH`].
//! * [`DryRunHost`] records what would happen and changes nothing. The
//!   development installer uses it (`CATOSTV_INSTALLER_DRY_RUN=1`), so the
//!   whole wizard can be exercised on a workstation.
//!
//! Safety layers (all must pass before the first destructive command):
//! the environment guard, a UEFI check, a fresh re-evaluation of the target
//! disk with [`crate::disk`] rules, an internet check and a tool check.

use crate::disk;
use crate::error::{Error, Result};
use crate::keyboard;
use crate::net;
use crate::process::{Cmd, Output, Runner, SLOW, SystemRunner, VERY_LONG, find_executable};
use crate::timezone;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

pub const MOUNT_ROOT: &str = "/mnt/catostv";
/// Where the live ISO keeps the installed-system overlay (`target-root/`).
pub const OVERLAY_DIR: &str = "/usr/share/catostv/install-root";
/// Where the live ISO keeps the CatosTV binaries to copy.
pub const LIVE_BIN_DIR: &str = "/usr/local/bin";
/// Each release's own setup steps, run in the new system after its files
/// (and by the updater on TVs). See the script itself.
pub const POST_INSTALL: &str = "/usr/local/lib/catostv/post-install";
/// Install log on the live system (RAM-backed `/tmp`).
pub const LOG_PATH: &str = "/tmp/catostv-install.log";

pub const ESP_LABEL: &str = "CATOSTV_EFI";
pub const ROOT_LABEL: &str = "CATOSTV_ROOT";
pub const HOSTNAME: &str = "catostv";
pub const LOCALE: &str = "en_US.UTF-8";
pub const USER: &str = "catostv";

/// Settings → Debug can switch SSH on (`crate::debug`): the
/// packages it needs are installed, and the unit that turns it off again at
/// every boot is enabled. Nothing listens by default and there is no
/// development account: SSH logs in as the TV account `catostv`, which gets
/// its default password only when Debug SSH is first switched on.
pub const DEBUG_SSH: bool = true;
/// Packages only installed while [`DEBUG_SSH`] is on.
pub const DEBUG_PACKAGES: &[&str] = &["openssh", "sudo", "rsync"];

/// Binaries copied from the live ISO into the installed system.
pub const INSTALLED_BINARIES: &[&str] = &[
    "catostv-shell",
    "catostv-ctl",
    "catostv-diag",
    "catostv-inputd",
    "catostv-cecd",
    "catostv-castd",
    "catostv-remoted",
    "catostv-wlfilter",
];

/// Overlay copy of the appliance user's labwc `environment` file. The plan
/// writes it again with the chosen keyboard layout.
pub const LABWC_ENVIRONMENT: &str =
    include_str!("../../../target-root/home/catostv/.config/labwc/environment");

/// Packages installed with `pacstrap`. Every name here was checked against
/// archlinux.org (2026-10-13; v0.3 additions 2026-10-14).
/// A single wrong name makes pacstrap, and
/// therefore the whole install, fail.
pub const BASE_PACKAGES: &[&str] = &[
    // System
    "base",
    "linux",
    "linux-firmware",
    "mkinitcpio",
    // fsck.vfat for the EFI partition at boot; fatlabel.
    "dosfstools",
    "dbus-broker-units",
    "polkit",
    // Network / Bluetooth
    "networkmanager",
    "wpa_supplicant",
    "bluez",
    "bluez-utils",
    // Audio
    "pipewire",
    "pipewire-pulse",
    "pipewire-alsa",
    "wireplumber",
    // Session / compositor
    "greetd",
    "greetd-agreety",
    "labwc",
    "xorg-xwayland",
    "wtype",
    "wlr-randr",
    "xdg-desktop-portal-gtk",
    "udisks2",
    // Graphics (Intel + AMD; VA-API for AMD now ships inside `mesa`)
    "mesa",
    "vulkan-intel",
    "vulkan-radeon",
    "intel-media-driver",
    // `vainfo`: which codecs the GPU decodes (android-codecs)
    "libva-utils",
    // Fonts and cursor
    "inter-font",
    "noto-fonts",
    "noto-fonts-emoji",
    "adwaita-cursors",
    // HDMI-CEC remote (cec-client)
    "libcec",
    // App store, Android apps, phone integration
    "flatpak",
    "waydroid",
    "git",
    "lzip",
    "kdeconnect",
    // Icon theme for app icons (SVG, rasterised with rsvg-convert)
    "papirus-icon-theme",
    "librsvg",
    // Default TV apps available in the official repositories. Firefox is not
    // a TV app here but YouTube "link with TV" (DIAL)
    // opens the YouTube TV page in it.
    "firefox",
    "mpv",
    "moonlight-qt",
    // Phone media controls (KDE Connect) for Catfyn's player: MPRIS in mpv
    "mpv-mpris",
    // Recents thumbnails (screenshots)
    "grim",
    // On-screen notices over apps (volume bar) and the boot screen
    "mako",
    "libnotify",
    "plymouth",
    "spotify-launcher",
];

/// Tools the live ISO must provide for a real install.
pub const REQUIRED_TOOLS: &[&str] = &[
    "lsblk",
    "findmnt",
    "wipefs",
    "sgdisk",
    "partprobe",
    "udevadm",
    "mkfs.fat",
    "mkfs.ext4",
    "mount",
    "umount",
    "pacstrap",
    "genfstab",
    "arch-chroot",
    "nmcli",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuVendor {
    Intel,
    Amd,
    Other,
}

impl CpuVendor {
    pub fn microcode_package(self) -> Option<&'static str> {
        match self {
            Self::Intel => Some("intel-ucode"),
            Self::Amd => Some("amd-ucode"),
            Self::Other => None,
        }
    }
}

pub fn detect_cpu(cpuinfo: &str) -> CpuVendor {
    if cpuinfo.contains("GenuineIntel") {
        CpuVendor::Intel
    } else if cpuinfo.contains("AuthenticAMD") {
        CpuVendor::Amd
    } else {
        CpuVendor::Other
    }
}

/// A random RFC 4122 version-4 UUID from `/dev/urandom`.
pub fn random_uuid() -> Result<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        h[0..4].concat(),
        h[4..6].concat(),
        h[6..8].concat(),
        h[8..10].concat(),
        h[10..16].concat()
    ))
}

fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.iter().map(|p| p.len()).collect::<Vec<_>>() == [8, 4, 4, 4, 12]
        && parts
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallConfig {
    /// Whole-disk device path, e.g. `/dev/nvme0n1`.
    pub disk: String,
    pub timezone: String,
    /// XKB layout name (one of [`keyboard::LAYOUTS`]).
    pub keyboard: String,
    pub cpu: CpuVendor,
    /// Filesystem UUID given to the root partition, so the boot entry can be
    /// written without querying `blkid` afterwards.
    pub root_uuid: String,
    /// Graphics drivers (`gpu::plan` from the boot menu's choice
    /// and the detected cards). Default: Mesa only.
    pub drivers: crate::gpu::DriverPlan,
}

impl InstallConfig {
    pub fn new(disk: &str, timezone: &str, keyboard: &str, cpu: CpuVendor) -> Result<Self> {
        Ok(Self {
            disk: disk.to_string(),
            timezone: timezone.to_string(),
            keyboard: keyboard.to_string(),
            cpu,
            root_uuid: random_uuid()?,
            drivers: crate::gpu::DriverPlan::default(),
        })
    }

    pub fn esp(&self) -> String {
        disk::partition_path(&self.disk, 1)
    }

    pub fn root(&self) -> String {
        disk::partition_path(&self.disk, 2)
    }

    pub fn packages(&self) -> Vec<String> {
        let mut p: Vec<String> = BASE_PACKAGES.iter().map(|s| s.to_string()).collect();
        if let Some(u) = self.cpu.microcode_package() {
            p.push(u.to_string());
        }
        p.extend(self.drivers.packages.iter().map(|s| s.to_string()));
        p
    }
}

/// A precondition verified by the host before anything is erased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    DestructiveAllowed,
    Uefi,
    DiskEligible(String),
    Online,
    Tools(Vec<String>),
}

impl Check {
    pub fn describe(&self) -> String {
        match self {
            Self::DestructiveAllowed => "installer is allowed to erase disks".into(),
            Self::Uefi => "PC booted in UEFI mode".into(),
            Self::DiskEligible(d) => format!("{d} passes the target-disk safety rules"),
            Self::Online => "internet connection available".into(),
            Self::Tools(t) => format!("installer tools present ({})", t.join(", ")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Check(Check),
    Run {
        cmd: Cmd,
        allow_fail: bool,
    },
    /// Run a command and write its stdout to a file.
    Capture {
        cmd: Cmd,
        path: String,
    },
    Write {
        path: String,
        contents: String,
    },
    Mkdir(String),
    /// Uncomment `line` in a config file (append it if absent).
    EnableLine {
        path: String,
        line: String,
    },
    /// Wait for a device node to appear (udev can lag behind partprobe).
    WaitForPath {
        path: String,
        secs: u64,
    },
    AssertExists(String),
    /// Run a command and report its output lines as progress
    /// ([`InstallEvent::Detail`]); used for pacstrap.
    Stream(Cmd),
}

impl Op {
    fn run(cmd: Cmd) -> Self {
        Self::Run {
            cmd,
            allow_fail: false,
        }
    }
    fn try_run(cmd: Cmd) -> Self {
        Self::Run {
            cmd,
            allow_fail: true,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Check(c) => format!("check: {}", c.describe()),
            Self::Run { cmd, allow_fail } => format!(
                "$ {}{}",
                cmd.display_redacted(),
                if *allow_fail {
                    "   (failure tolerated)"
                } else {
                    ""
                }
            ),
            Self::Capture { cmd, path } => format!("$ {} > {path}", cmd.display()),
            Self::Write { path, contents } => {
                format!("write {path}: {}", contents.trim_end().replace('\n', " ⏎ "))
            }
            Self::Mkdir(p) => format!("mkdir -p {p}"),
            Self::EnableLine { path, line } => format!("enable \"{line}\" in {path}"),
            Self::WaitForPath { path, secs } => format!("wait up to {secs}s for {path}"),
            Self::AssertExists(p) => format!("verify {p} exists"),
            Self::Stream(cmd) => format!("$ {}   (progress shown)", cmd.display_redacted()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub label: String,
    /// True for steps that change the disk. Used to tell the user whether a
    /// failure left their disk untouched.
    pub destructive: bool,
    pub ops: Vec<Op>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub config: InstallConfig,
    pub steps: Vec<Step>,
}

fn chroot<I, S>(args: I) -> Cmd
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    Cmd::new("arch-chroot").arg(MOUNT_ROOT).args(args)
}

fn m(path: &str) -> String {
    format!("{MOUNT_ROOT}{path}")
}

pub fn loader_conf() -> String {
    // timeout 0: the menu stays hidden; hold Space during boot to show it.
    "default catostv.conf\ntimeout 0\nconsole-mode keep\neditor no\n".to_string()
}

/// systemd-boot entry. Microcode is embedded in the initramfs by
/// mkinitcpio's default `microcode` hook, so no separate initrd line is
/// needed for intel-ucode/amd-ucode.
pub fn loader_entry(root_uuid: &str, extra: &[&str]) -> String {
    let extra: String = extra.iter().map(|p| format!(" {p}")).collect();
    format!(
        "title   CatosTV\nlinux   /vmlinuz-linux\ninitrd  /initramfs-linux.img\n\
         options root=UUID={root_uuid} rw quiet splash loglevel=3 rd.udev.log_level=3 \
         systemd.show_status=auto vt.global_cursor_default=0{extra}\n"
    )
}

/// `reflector` ranking the mirrors by measured download speed and writing
/// the live system's mirror list (pacstrap copies it into the target).
pub fn reflector_cmd() -> Cmd {
    Cmd::new("reflector")
        .args([
            "--protocol",
            "https",
            "--age",
            "24",
            "--latest",
            "40",
            "--fastest",
            "12",
            "--threads",
            "8",
            "--download-timeout",
            "4",
            "--sort",
            "rate",
            "--save",
            "/etc/pacman.d/mirrorlist",
        ])
        .timeout(Duration::from_secs(150))
}

/// Settings → Debug's SSH (see [`DEBUG_SSH`]): off at every boot, so only
/// the boot unit is enabled, never `sshd` itself. Empty when it is off.
pub fn debug_ssh_ops() -> Vec<Op> {
    if !DEBUG_SSH {
        return vec![];
    }
    vec![Op::run(chroot([
        "systemctl",
        "enable",
        "catostv-debug-boot.service",
    ]))]
}

/// Progress text and fraction from one line of pacstrap/pacman output.
/// pacman without a terminal prints `( 54/312) installing linux-firmware`
/// while installing and `downloading foo-1.0-1-x86_64.pkg.tar.zst...` (or
/// ` foo-1.0 downloading...`) while downloading.
pub fn pacman_progress(line: &str) -> Option<(String, Option<f32>)> {
    let l = line.trim();
    if let Some(rest) = l.strip_prefix('(') {
        let (counts, tail) = rest.split_once(')')?;
        let (a, b) = counts.split_once('/')?;
        let a: f32 = a.trim().parse().ok()?;
        let b: f32 = b.trim().parse().ok()?;
        let tail = tail.trim();
        let pkg = tail
            .strip_prefix("installing ")
            .or_else(|| tail.strip_prefix("upgrading "))?;
        if b <= 0.0 {
            return None;
        }
        return Some((format!("Installing {}", pkg.trim()), Some(a / b)));
    }
    if l.starts_with("downloading ") || l.ends_with(" downloading...") {
        let name = l
            .trim_start_matches("downloading ")
            .trim_end_matches(" downloading...")
            .trim_end_matches("...");
        let name = name.split(".pkg.tar").next().unwrap_or(name);
        // `foo-1.0-1-x86_64` → `foo`
        let pkg = name
            .rsplitn(4, '-')
            .last()
            .filter(|p| !p.is_empty())
            .unwrap_or(name);
        return Some((format!("Downloading {pkg}"), None));
    }
    None
}

/// Build the full install plan. Validates inputs; executes nothing.
pub fn plan(cfg: &InstallConfig) -> Result<Plan> {
    if !cfg.disk.starts_with("/dev/")
        || cfg.disk.contains("..")
        || cfg.disk.chars().any(|c| c.is_whitespace())
    {
        return Err(Error::msg(format!("invalid disk path {:?}", cfg.disk)));
    }
    if !timezone::is_valid_name(&cfg.timezone) {
        return Err(Error::msg(format!("invalid time zone {:?}", cfg.timezone)));
    }
    if !keyboard::is_valid(&cfg.keyboard) {
        return Err(Error::msg(format!(
            "unknown keyboard layout {:?}",
            cfg.keyboard
        )));
    }
    if !is_uuid(&cfg.root_uuid) {
        return Err(Error::msg(format!("invalid root UUID {:?}", cfg.root_uuid)));
    }
    let disk = cfg.disk.as_str();
    let esp = cfg.esp();
    let root = cfg.root();
    let tz = cfg.timezone.as_str();

    let step = |label: &str, destructive: bool, ops: Vec<Op>| Step {
        label: label.to_string(),
        destructive,
        ops,
    };

    let mut pacstrap = Cmd::new("pacstrap")
        .args(["-K", MOUNT_ROOT])
        .timeout(VERY_LONG);
    pacstrap = pacstrap.args(cfg.packages());
    if DEBUG_SSH {
        pacstrap = pacstrap.args(DEBUG_PACKAGES.iter().copied());
    }

    // Every CatosTV program the live system has, after "Checking for
    // updates" those are the newest release's, so a program a release adds
    // is installed too (rolling: the ISO's own list is not the limit).
    let install_bins = vec![Op::run(Cmd::new("sh").args([
        "-c".to_string(),
        format!(
            "set -e; for b in {LIVE_BIN_DIR}/catostv-*; do case \"${{b##*/}}\" in \
             catostv-installer|catostv-live-session) ;; \
             *) install -Dm755 \"$b\" {MOUNT_ROOT}/usr/local/bin/\"${{b##*/}}\" ;; esac; done"
        ),
    ]))];

    let steps = vec![
        step(
            "Checking your PC",
            false,
            vec![
                Op::Check(Check::DestructiveAllowed),
                Op::Check(Check::Uefi),
                Op::Check(Check::DiskEligible(disk.to_string())),
                Op::Check(Check::Online),
                Op::Check(Check::Tools(
                    REQUIRED_TOOLS.iter().map(|s| s.to_string()).collect(),
                )),
            ],
        ),
        // Before anything is erased: ask the update server for the newest
        // release and pin pacman to its day of the Arch Linux Archive (else
        // this ISO's day, else the fastest mirrors); a newer release's files
        // replace the ISO's. pacstrap copies the pinned list into the new
        // system. See updates::prepare_live.
        step(
            "Checking for updates",
            false,
            vec![
                // releng's own reflector.service may still be rewriting the
                // same file from boot; stop it first.
                Op::try_run(Cmd::new("systemctl").args(["stop", "reflector.service"])),
                Op::try_run(
                    Cmd::new(format!("{LIVE_BIN_DIR}/catostv-diag"))
                        .arg("install-prepare")
                        .timeout(Duration::from_secs(2400)),
                ),
            ],
        ),
        step(
            "Clearing the disk",
            true,
            vec![
                // Leftovers from an earlier failed attempt.
                Op::try_run(Cmd::new("umount").args(["-R", MOUNT_ROOT])),
                Op::run(Cmd::new("wipefs").args(["--all", "--force", disk])),
                Op::run(Cmd::new("sgdisk").args(["--zap-all", disk])),
                Op::run(Cmd::new("sgdisk").args([
                    "-n",
                    "1:0:+1G",
                    "-t",
                    "1:ef00",
                    "-c",
                    &format!("1:{ESP_LABEL}"),
                    disk,
                ])),
                Op::run(Cmd::new("sgdisk").args([
                    "-n",
                    "2:0:0",
                    "-t",
                    "2:8304",
                    "-c",
                    &format!("2:{ROOT_LABEL}"),
                    disk,
                ])),
                Op::run(Cmd::new("partprobe").arg(disk)),
                Op::try_run(
                    Cmd::new("udevadm")
                        .args(["settle", "--timeout=10"])
                        .timeout(SLOW),
                ),
                Op::WaitForPath {
                    path: esp.clone(),
                    secs: 10,
                },
                Op::WaitForPath {
                    path: root.clone(),
                    secs: 10,
                },
            ],
        ),
        step(
            "Preparing the disk",
            true,
            vec![
                Op::run(
                    Cmd::new("mkfs.fat")
                        .args(["-F", "32", "-n", ESP_LABEL, &esp])
                        .timeout(SLOW),
                ),
                Op::run(
                    Cmd::new("mkfs.ext4")
                        .args(["-F", "-L", ROOT_LABEL, "-U", &cfg.root_uuid, &root])
                        .timeout(Duration::from_secs(600)),
                ),
                Op::Mkdir(MOUNT_ROOT.into()),
                Op::run(Cmd::new("mount").args([root.as_str(), MOUNT_ROOT])),
                Op::Mkdir(m("/boot")),
                Op::run(Cmd::new("mount").args([
                    "-o",
                    "fmask=0077,dmask=0077",
                    esp.as_str(),
                    &m("/boot"),
                ])),
            ],
        ),
        step(
            "Downloading CatosTV",
            true,
            vec![
                Op::Stream(pacstrap),
                // Packages a newer release (taken by "Checking for updates")
                // needs beyond this ISO's list.
                Op::run(
                    Cmd::new("sh")
                        .args([
                            "-c",
                            &format!(
                                "f={OVERLAY_DIR}{pk}; [ -s \"$f\" ] || exit 0; \
                                 arch-chroot {MOUNT_ROOT} pacman -S --needed --noconfirm $(cat \"$f\")",
                                pk = crate::updates::PACKAGES_FILE
                            ),
                        ])
                        .timeout(VERY_LONG),
                ),
            ],
        ),
        step(
            "Setting up your TV",
            true,
            vec![
                Op::run(chroot([
                    "useradd",
                    "-m",
                    "-G",
                    "audio,video,input,render,uucp",
                    "-s",
                    "/usr/bin/bash",
                    USER,
                ])),
                Op::run(chroot(["passwd", "-l", USER])),
                Op::run(chroot(["passwd", "-l", "root"])),
            ],
        ),
        step("Installing the TV interface", true, {
            // --no-preserve=mode,ownership: never change the mode/owner of
            // directories that already exist in the target (e.g. polkit's
            // rules.d is 750 root:polkitd, /home/catostv is 700). New files get
            // default modes; executables are fixed by the chmod below.
            let mut ops = vec![Op::run(Cmd::new("cp").args([
                "-a".to_string(),
                "--no-preserve=mode,ownership".to_string(),
                format!("{OVERLAY_DIR}/."),
                format!("{MOUNT_ROOT}/"),
            ]))];
            // Graphics: the session reads gpu.conf; Android draws in
            // software where its GPU path cannot work.
            ops.push(Op::Write {
                path: m("/etc/catostv/gpu.conf"),
                contents: crate::gpu::gpu_conf(&cfg.drivers),
            });
            if cfg.drivers.android_software {
                ops.push(Op::Write {
                    path: m("/etc/catostv/android-software-rendering"),
                    contents: "# NVIDIA driver, nouveau or safe graphics.\n".into(),
                });
            }
            // Keyboard layout for the TV session (chowned below).
            ops.push(Op::Write {
                path: m("/home/catostv/.config/labwc/environment"),
                contents: keyboard::set_layout_in_environment(LABWC_ENVIRONMENT, &cfg.keyboard),
            });
            ops.extend(install_bins);
            ops.push(Op::run(Cmd::new("chmod").args([
                "755".to_string(),
                m("/usr/local/bin/catostv-session"),
                m("/home/catostv/.config/labwc/autostart"),
            ])));
            // Every helper script: a fixed list missed new ones.
            ops.push(Op::run(Cmd::new("find").args([
                m("/usr/local/lib/catostv"),
                "-type".to_string(),
                "f".to_string(),
                "-exec".to_string(),
                "chmod".to_string(),
                "755".to_string(),
                "{}".to_string(),
                "+".to_string(),
            ])));
            ops.push(Op::run(chroot([
                "chown",
                "-R",
                "catostv:catostv",
                "/home/catostv",
            ])));
            // Compile the GTK / libadwaita TV defaults (/etc/dconf/db/local.d).
            ops.push(Op::try_run(chroot(["dconf", "update"])));
            // Start-up screen (Plymouth theme + initramfs hook). Optional:
            // the TV works without it.
            ops.push(Op::try_run(
                chroot(["/usr/local/lib/catostv/boot-splash-setup"]).timeout(VERY_LONG),
            ));
            ops
        }),
        step(
            "Applying your preferences",
            true,
            vec![
                Op::Write {
                    path: m("/etc/hostname"),
                    contents: format!("{HOSTNAME}\n"),
                },
                Op::Write {
                    path: m("/etc/locale.conf"),
                    contents: format!("LANG={LOCALE}\n"),
                },
                Op::Write {
                    path: m("/etc/vconsole.conf"),
                    contents: keyboard::vconsole_conf(&cfg.keyboard),
                },
                Op::EnableLine {
                    path: m("/etc/locale.gen"),
                    line: format!("{LOCALE} UTF-8"),
                },
                Op::run(chroot(["locale-gen"]).timeout(SLOW)),
                Op::AssertExists(m(&format!("/usr/share/zoneinfo/{tz}"))),
                Op::run(chroot([
                    "ln".to_string(),
                    "-sf".to_string(),
                    format!("/usr/share/zoneinfo/{tz}"),
                    "/etc/localtime".to_string(),
                ])),
                Op::try_run(chroot(["hwclock", "--systohc"])),
            ],
        ),
        step(
            "Tuning start-up",
            true,
            vec![
                Op::Capture {
                    cmd: Cmd::new("genfstab").args(["-U", MOUNT_ROOT]),
                    path: m("/etc/fstab"),
                },
                Op::run(chroot([
                    "systemctl",
                    "enable",
                    "NetworkManager.service",
                    "bluetooth.service",
                    "greetd.service",
                    "systemd-timesyncd.service",
                    // Runs only while Android's container runs (WantedBy=).
                    "catostv-android-watch.service",
                ])),
                Op::run(chroot(["systemctl", "set-default", "graphical.target"])),
            ]
            .into_iter()
            // After "Installing the TV interface": the unit file comes with
            // the overlay (enabling it earlier failed every install).
            .chain(debug_ssh_ops())
            // The release's own setup (rolling; see target-root's
            // usr/local/lib/catostv/post-install).
            .chain([Op::run(
                Cmd::new("sh")
                    .args([
                        "-c",
                        &format!(
                            "[ -x {MOUNT_ROOT}{POST_INSTALL} ] || exit 0; \
                             arch-chroot {MOUNT_ROOT} {POST_INSTALL}"
                        ),
                    ])
                    .timeout(VERY_LONG),
            )])
            .collect(),
        ),
        step(
            "Making it bootable",
            true,
            vec![
                Op::run(chroot(["bootctl", "install"]).timeout(SLOW)),
                Op::Write {
                    path: m("/boot/loader/loader.conf"),
                    contents: loader_conf(),
                },
                Op::Write {
                    path: m("/boot/loader/entries/catostv.conf"),
                    contents: loader_entry(&cfg.root_uuid, &cfg.drivers.kernel_params),
                },
            ],
        ),
        step(
            "Double-checking everything",
            false,
            [
                "/boot/vmlinuz-linux",
                "/boot/initramfs-linux.img",
                "/boot/EFI/systemd/systemd-bootx64.efi",
                "/boot/loader/entries/catostv.conf",
                "/etc/fstab",
                "/etc/greetd/config.toml",
                "/usr/local/bin/catostv-session",
                "/usr/local/lib/catostv/android-setup",
                "/home/catostv/.config/labwc/rc.xml",
                "/home/catostv/.config/labwc/autostart",
                "/home/catostv/.config/catostv/apps.toml",
            ]
            .iter()
            .map(|p| p.to_string())
            .chain(
                INSTALLED_BINARIES
                    .iter()
                    .map(|b| format!("/usr/local/bin/{b}")),
            )
            .map(|p| Op::AssertExists(m(&p)))
            .collect(),
        ),
        step(
            "Wrapping up",
            false,
            vec![
                Op::try_run(Cmd::new("sync")),
                Op::run(Cmd::new("umount").args(["-R", MOUNT_ROOT])),
            ],
        ),
    ];
    Ok(Plan {
        config: cfg.clone(),
        steps,
    })
}

/// Human-readable plan, as printed by `catostv-diag plan`.
pub fn render(plan: &Plan) -> String {
    let c = &plan.config;
    let mut out = format!(
        "CatosTV install plan\n  disk:      {}\n  ESP:       {}\n  root:      {} (ext4, UUID {})\n  time zone: {}\n  keyboard:  {}\n  CPU:       {:?}\n\n",
        c.disk,
        c.esp(),
        c.root(),
        c.root_uuid,
        c.timezone,
        c.keyboard,
        c.cpu
    );
    for (i, s) in plan.steps.iter().enumerate() {
        out.push_str(&format!(
            "{}. {}{}\n",
            i + 1,
            s.label,
            if s.destructive {
                "   [modifies disk]"
            } else {
                ""
            }
        ));
        for op in &s.ops {
            out.push_str(&format!("     {}\n", op.describe()));
        }
    }
    out
}

/// Progress reported by [`execute`].
#[derive(Debug, Clone, PartialEq)]
pub enum InstallEvent {
    Step {
        index: usize,
        total: usize,
        label: String,
    },
    /// Live detail inside the current step (package being downloaded or
    /// installed) and, when known, the step's own progress 0–1.
    Detail {
        text: String,
        fraction: Option<f32>,
    },
    Finished,
    Failed {
        message: String,
        /// False when the failure happened before anything was erased.
        disk_modified: bool,
    },
}

/// The machine the plan runs on.
pub trait Host {
    fn run(&mut self, cmd: &Cmd) -> Result<Output>;
    fn write_file(&mut self, path: &str, contents: &str) -> Result<()>;
    fn read_file(&mut self, path: &str) -> Result<String>;
    fn exists(&mut self, path: &str) -> bool;
    fn mkdir_all(&mut self, path: &str) -> Result<()>;
    fn check(&mut self, check: &Check) -> Result<()>;
    /// Run and report stdout lines as they appear. Default: run, then
    /// replay the captured lines.
    fn run_lines(&mut self, cmd: &Cmd, on_line: &mut dyn FnMut(&str)) -> Result<Output> {
        let out = self.run(cmd)?;
        for l in out.stdout.lines() {
            on_line(l);
        }
        Ok(out)
    }
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
    fn log(&mut self, _line: &str) {}
}

/// Ops run (best effort) after a failure so the disk is not left mounted.
pub fn cleanup_ops() -> Vec<Op> {
    vec![
        Op::try_run(Cmd::new("sync")),
        Op::try_run(Cmd::new("umount").args(["-R", MOUNT_ROOT])),
    ]
}

fn run_op(op: &Op, host: &mut dyn Host, on_event: &mut dyn FnMut(InstallEvent)) -> Result<()> {
    host.log(&op.describe());
    match op {
        Op::Stream(cmd) => {
            let mut last_fraction = None;
            let out = host.run_lines(cmd, &mut |line| {
                if let Some((text, fraction)) = pacman_progress(line) {
                    if fraction.is_some() {
                        last_fraction = fraction;
                    }
                    on_event(InstallEvent::Detail {
                        text,
                        fraction: fraction.or(last_fraction),
                    });
                }
            })?;
            if !out.success() {
                host.log(&format!(
                    "   -> {}\n   stderr: {}",
                    out.failure_reason(),
                    out.stderr.trim()
                ));
                return Err(Error::msg(format!(
                    "{} failed ({})",
                    cmd.program,
                    out.failure_reason()
                )));
            }
            Ok(())
        }
        Op::Check(c) => host.check(c),
        Op::Run { cmd, allow_fail } => {
            let out = host.run(cmd)?;
            if !out.success() {
                host.log(&format!(
                    "   -> {}\n   stdout: {}\n   stderr: {}",
                    out.failure_reason(),
                    out.stdout.trim(),
                    out.stderr.trim()
                ));
                if !allow_fail {
                    return Err(Error::msg(format!(
                        "{} failed ({})",
                        cmd.program,
                        out.failure_reason()
                    )));
                }
            }
            Ok(())
        }
        Op::Capture { cmd, path } => {
            let out = host.run(cmd)?;
            if !out.success() {
                return Err(Error::msg(format!(
                    "{} failed ({})",
                    cmd.program,
                    out.failure_reason()
                )));
            }
            if out.stdout.trim().is_empty() {
                return Err(Error::msg(format!("{} produced no output", cmd.program)));
            }
            let mut text = out.stdout;
            if !text.ends_with('\n') {
                text.push('\n');
            }
            host.write_file(path, &text)
        }
        Op::Write { path, contents } => host.write_file(path, contents),
        Op::Mkdir(p) => host.mkdir_all(p),
        Op::EnableLine { path, line } => {
            let text = host.read_file(path)?;
            let updated = enable_line(&text, line);
            host.write_file(path, &updated)
        }
        Op::WaitForPath { path, secs } => {
            for _ in 0..(secs * 5).max(1) {
                if host.exists(path) {
                    return Ok(());
                }
                host.sleep(Duration::from_millis(200));
            }
            Err(Error::msg(format!(
                "{path} did not appear after partitioning"
            )))
        }
        Op::AssertExists(p) => {
            if host.exists(p) {
                Ok(())
            } else {
                Err(Error::msg(format!("{p} is missing")))
            }
        }
    }
}

/// Uncomment `line` (`#line` or `# line`) or append it if not present.
pub fn enable_line(text: &str, line: &str) -> String {
    let mut found = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            let t = l.trim();
            let uncommented = t.trim_start_matches('#').trim_start();
            if !found && (t == line || (t.starts_with('#') && uncommented == line)) {
                found = true;
                line.to_string()
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        out.push(line.to_string());
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Run every step. On failure, best-effort cleanup runs and
/// `InstallEvent::Failed` is emitted.
pub fn execute(
    plan: &Plan,
    host: &mut dyn Host,
    on_event: &mut dyn FnMut(InstallEvent),
) -> Result<()> {
    let total = plan.steps.len();
    let mut disk_modified = false;
    for (index, step) in plan.steps.iter().enumerate() {
        on_event(InstallEvent::Step {
            index,
            total,
            label: step.label.clone(),
        });
        host.log(&format!("== [{}/{}] {}", index + 1, total, step.label));
        disk_modified |= step.destructive;
        for op in &step.ops {
            if let Err(e) = run_op(op, host, on_event) {
                let message = format!("{}: {}", step.label, e);
                host.log(&format!("FAILED: {message}"));
                for c in cleanup_ops() {
                    let _ = run_op(&c, host, &mut |_| {});
                }
                on_event(InstallEvent::Failed {
                    message: message.clone(),
                    disk_modified,
                });
                return Err(Error::msg(message));
            }
        }
    }
    host.log("== finished");
    on_event(InstallEvent::Finished);
    Ok(())
}

/// Shared implementation of [`Check`] for hosts that query the real system.
pub fn real_check(check: &Check, r: &mut dyn Runner, destructive_allowed: bool) -> Result<()> {
    match check {
        Check::DestructiveAllowed => {
            if destructive_allowed {
                Ok(())
            } else {
                Err(Error::msg(
                    "Installation is locked. Disks can only be erased from the CatosTV live installer \
                     (CATOSTV_INSTALLER_ALLOW_DESTRUCTIVE=1).",
                ))
            }
        }
        Check::Uefi => {
            if Path::new("/sys/firmware/efi").exists() {
                Ok(())
            } else {
                Err(Error::msg(
                    "CatosTV needs a PC started in UEFI mode. Turn off Legacy/CSM boot in the \
                     firmware settings and start the installer again.",
                ))
            }
        }
        Check::DiskEligible(path) => {
            let candidates = disk::scan(r)?;
            match candidates.iter().find(|c| &c.disk.path == path) {
                None => Err(Error::msg(format!("{path} is no longer connected"))),
                Some(c) => match &c.blocked {
                    Some(reason) => Err(Error::msg(format!("Refusing to erase {path}: {reason}"))),
                    None => Ok(()),
                },
            }
        }
        Check::Online => {
            if net::status(r).map(|s| s.online).unwrap_or(false) {
                Ok(())
            } else {
                Err(Error::msg(
                    "No internet connection. CatosTV downloads its system during installation. \
                     go back and connect to a network.",
                ))
            }
        }
        Check::Tools(tools) => {
            let missing: Vec<&str> = tools
                .iter()
                .filter(|t| find_executable(t).is_none())
                .map(String::as_str)
                .collect();
            if missing.is_empty() {
                Ok(())
            } else {
                Err(Error::msg(format!(
                    "The installer image is missing: {}",
                    missing.join(", ")
                )))
            }
        }
    }
}

/// Executes the plan on this machine. Writes a log to [`LOG_PATH`].
pub struct RealHost {
    runner: SystemRunner,
    log: Option<File>,
    destructive_allowed: bool,
}

impl RealHost {
    pub fn new(destructive_allowed: bool) -> Self {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(LOG_PATH)
            .ok();
        Self {
            runner: SystemRunner,
            log,
            destructive_allowed,
        }
    }
}

impl Host for RealHost {
    fn run(&mut self, cmd: &Cmd) -> Result<Output> {
        self.runner.run(cmd)
    }
    fn run_lines(&mut self, cmd: &Cmd, on_line: &mut dyn FnMut(&str)) -> Result<Output> {
        crate::process::run_with_lines(cmd, on_line)
    }
    fn write_file(&mut self, path: &str, contents: &str) -> Result<()> {
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents).map_err(|e| Error::msg(format!("writing {path}: {e}")))
    }
    fn read_file(&mut self, path: &str) -> Result<String> {
        std::fs::read_to_string(path).map_err(|e| Error::msg(format!("reading {path}: {e}")))
    }
    fn exists(&mut self, path: &str) -> bool {
        Path::new(path).exists()
    }
    fn mkdir_all(&mut self, path: &str) -> Result<()> {
        std::fs::create_dir_all(path).map_err(|e| Error::msg(format!("creating {path}: {e}")))
    }
    fn check(&mut self, check: &Check) -> Result<()> {
        real_check(check, &mut self.runner, self.destructive_allowed)
    }
    fn log(&mut self, line: &str) {
        if let Some(f) = self.log.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Simulates the plan. Only read-only queries are executed for real (the
/// target-disk check uses `lsblk`/`findmnt` through `probe`), so a dry run
/// still reports a disk that would be refused.
pub struct DryRunHost<R: Runner> {
    probe: R,
    pub transcript: Vec<String>,
    /// Pause per op so the progress UI can be observed during development.
    pub op_delay: Duration,
}

impl<R: Runner> DryRunHost<R> {
    pub fn new(probe: R, op_delay: Duration) -> Self {
        Self {
            probe,
            transcript: Vec::new(),
            op_delay,
        }
    }
}

impl<R: Runner> Host for DryRunHost<R> {
    fn run(&mut self, cmd: &Cmd) -> Result<Output> {
        self.transcript
            .push(format!("[dry-run] $ {}", cmd.display_redacted()));
        std::thread::sleep(self.op_delay);
        Ok(Output::ok("# dry run\n"))
    }
    fn write_file(&mut self, path: &str, _contents: &str) -> Result<()> {
        self.transcript.push(format!("[dry-run] write {path}"));
        Ok(())
    }
    fn read_file(&mut self, _path: &str) -> Result<String> {
        Ok(String::new())
    }
    fn exists(&mut self, _path: &str) -> bool {
        true
    }
    fn mkdir_all(&mut self, path: &str) -> Result<()> {
        self.transcript.push(format!("[dry-run] mkdir -p {path}"));
        Ok(())
    }
    fn check(&mut self, check: &Check) -> Result<()> {
        self.transcript
            .push(format!("[dry-run] check {}", check.describe()));
        match check {
            Check::DiskEligible(_) => real_check(check, &mut self.probe, false),
            _ => Ok(()),
        }
    }
    fn sleep(&mut self, _d: Duration) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::fake::FakeRunner;
    use std::collections::{HashMap, HashSet};

    const UUID: &str = "0b4a3c3e-9f1d-4c5e-8a7b-1234567890ab";

    fn cfg(disk: &str) -> InstallConfig {
        InstallConfig {
            disk: disk.into(),
            timezone: "Asia/Kolkata".into(),
            keyboard: "de".into(),
            cpu: CpuVendor::Intel,
            root_uuid: UUID.into(),
            drivers: Default::default(),
        }
    }

    /// In-memory host that records commands and can fail one program.
    #[derive(Default)]
    struct MemHost {
        files: HashMap<String, String>,
        existing: HashSet<String>,
        ran: Vec<String>,
        fail_program: Option<String>,
        fail_check: Option<Check>,
        log: Vec<String>,
    }

    impl Host for MemHost {
        fn run(&mut self, cmd: &Cmd) -> Result<Output> {
            self.ran.push(cmd.display());
            if self.fail_program.as_deref() == Some(cmd.program.as_str()) {
                return Ok(Output::failed(1, "boom"));
            }
            if cmd.program == "genfstab" {
                return Ok(Output::ok(format!("UUID={UUID} / ext4 rw 0 1")));
            }
            if cmd.program == "pacstrap" {
                return Ok(Output::ok(
                    "downloading mesa-1:24.2-1-x86_64.pkg.tar.zst...\n( 1/4) installing base\n( 2/4) installing mesa\n",
                ));
            }
            Ok(Output::ok(""))
        }
        fn write_file(&mut self, path: &str, contents: &str) -> Result<()> {
            self.files.insert(path.into(), contents.into());
            self.existing.insert(path.into());
            Ok(())
        }
        fn read_file(&mut self, path: &str) -> Result<String> {
            Ok(self.files.get(path).cloned().unwrap_or_default())
        }
        fn exists(&mut self, _path: &str) -> bool {
            true
        }
        fn mkdir_all(&mut self, _path: &str) -> Result<()> {
            Ok(())
        }
        fn check(&mut self, check: &Check) -> Result<()> {
            if self.fail_check.as_ref() == Some(check) {
                Err(Error::msg("check failed"))
            } else {
                Ok(())
            }
        }
        fn sleep(&mut self, _d: Duration) {}
        fn log(&mut self, line: &str) {
            self.log.push(line.into());
        }
    }

    #[test]
    fn plan_validates_inputs() {
        assert!(plan(&cfg("/dev/vda")).is_ok());
        assert!(plan(&cfg("vda")).is_err());
        assert!(plan(&cfg("/dev/../etc")).is_err());
        let mut bad = cfg("/dev/vda");
        bad.timezone = "../../etc/shadow".into();
        assert!(plan(&bad).is_err());
        bad = cfg("/dev/vda");
        bad.root_uuid = "nope".into();
        assert!(plan(&bad).is_err());
    }

    #[test]
    fn checks_come_before_anything_destructive() {
        let p = plan(&cfg("/dev/nvme0n1")).unwrap();
        assert!(!p.steps[0].destructive);
        assert!(p.steps[0].ops.iter().all(|o| matches!(o, Op::Check(_))));
        assert!(
            p.steps[0]
                .ops
                .contains(&Op::Check(Check::DestructiveAllowed))
        );
        assert!(
            p.steps[0]
                .ops
                .contains(&Op::Check(Check::DiskEligible("/dev/nvme0n1".into())))
        );
        // Before the first destructive step only the (tolerated) update
        // check runs.
        let first_destructive = p.steps.iter().position(|s| s.destructive).unwrap();
        assert_eq!(first_destructive, 2);
        assert!(matches!(
            p.steps[1].ops.last(),
            Some(Op::Run { cmd, allow_fail: true, .. })
                if cmd.program.ends_with("catostv-diag") && cmd.args == ["install-prepare"]
        ));
        assert!(p.steps[1].ops.iter().all(|o| matches!(
            o,
            Op::Run {
                allow_fail: true,
                ..
            }
        )));
    }

    #[test]
    fn pacman_lines_become_progress() {
        assert_eq!(
            pacman_progress("( 54/312) installing linux-firmware"),
            Some(("Installing linux-firmware".into(), Some(54.0 / 312.0)))
        );
        assert_eq!(
            pacman_progress("downloading linux-firmware-20241010.abc-1-any.pkg.tar.zst..."),
            Some(("Downloading linux-firmware".into(), None))
        );
        assert_eq!(
            pacman_progress(" mesa-1:24.2-1-x86_64 downloading..."),
            Some(("Downloading mesa".into(), None))
        );
        assert_eq!(pacman_progress("(3/9) checking keys in keyring"), None);
        assert_eq!(
            pacman_progress(":: Synchronizing package databases..."),
            None
        );
    }

    #[test]
    fn pacstrap_reports_detail_events() {
        let p = plan(&cfg("/dev/vda")).unwrap();
        let mut host = MemHost::default();
        let mut details = Vec::new();
        execute(&p, &mut host, &mut |e| {
            if let InstallEvent::Detail { text, fraction } = e {
                details.push((text, fraction));
            }
        })
        .unwrap();
        assert_eq!(details[0], ("Downloading mesa".to_string(), None));
        assert_eq!(
            details.last().unwrap(),
            &("Installing mesa".to_string(), Some(0.5))
        );
    }

    #[test]
    fn graphics_drivers_reach_the_plan() {
        let mut c = cfg("/dev/vda");
        let gpus = crate::gpu::parse_lspci(
            r#"01:00.0 "VGA compatible controller [0300]" "NVIDIA Corporation [10de]" "AD106 [GeForce RTX 4060 Ti] [2803]" -ra1 -p00 "Device [1462]" "Device [5170]""#,
        );
        c.drivers = crate::gpu::plan(crate::gpu::GpuChoice::Auto, &gpus);
        let text = render(&plan(&c).unwrap());
        assert!(
            text.contains(" nvidia-open nvidia-utils libva-nvidia-driver"),
            "{text}"
        );
        assert!(text.contains("nvidia_drm.modeset=1"));
        assert!(text.contains("/etc/catostv/android-software-rendering"));
        assert!(text.contains("/etc/catostv/gpu.conf"));
        // The default (Mesa only) writes no Android software switch.
        let text = render(&plan(&cfg("/dev/vda")).unwrap());
        assert!(!text.contains("android-software-rendering"));
    }

    #[test]
    fn units_are_enabled_after_their_files_are_copied() {
        // 1.0 review: a CatosTV unit enabled before the overlay that ships
        // it made `systemctl enable` fail and the install abort.
        let text = render(&plan(&cfg("/dev/vda")).unwrap());
        let copy = text.find("Installing the TV interface").unwrap();
        for (at, _) in text.match_indices("systemctl enable") {
            let line_end = text[at..].find('\n').map_or(text.len(), |e| at + e);
            if text[at..line_end].contains("catostv-") {
                assert!(
                    at > copy,
                    "enabled before the overlay: {}",
                    &text[at..line_end]
                );
            }
        }
    }

    #[test]
    fn debug_ssh_is_off_until_switched_on() {
        let text = render(&plan(&cfg("/dev/vda")).unwrap());
        // No development account, no passwordless sudo, sshd never enabled.
        assert!(!text.contains("useradd -m -s /usr/bin/bash -p"), "{text}");
        assert!(!text.contains("NOPASSWD"));
        assert!(!text.contains("enable sshd.service"));
        assert_eq!(
            DEBUG_SSH,
            text.contains("enable catostv-debug-boot.service")
        );
        if DEBUG_SSH {
            assert!(text.contains(" openssh sudo rsync"));
        }
        assert!(text.contains("catostv-diag install-prepare"));
        // Rolling: the release's programs and setup hook, not the ISO's list.
        assert!(text.contains("for b in /usr/local/bin/catostv-*"));
        assert!(text.contains("arch-chroot /mnt/catostv /usr/local/lib/catostv/post-install"));
        assert!(text.contains("pacman -S --needed --noconfirm"));
    }

    #[test]
    fn plan_uses_correct_partitions_and_uuid() {
        let p = plan(&cfg("/dev/nvme0n1")).unwrap();
        let text = render(&p);
        assert!(
            text.contains("mkfs.fat -F 32 -n CATOSTV_EFI /dev/nvme0n1p1"),
            "{text}"
        );
        assert!(text.contains(&format!(
            "mkfs.ext4 -F -L CATOSTV_ROOT -U {UUID} /dev/nvme0n1p2"
        )));
        assert!(text.contains("intel-ucode"));
        assert!(text.contains(
            "cp -a --no-preserve=mode,ownership /usr/share/catostv/install-root/. /mnt/catostv/"
        ));
        assert!(!text.contains("amd-ucode"));
        assert!(text.contains("keyboard:  de"));
        assert!(text.contains("XKB_DEFAULT_LAYOUT=de"), "{text}");
        assert!(text.contains("KEYMAP=de-latin1"));
        let mut bad = cfg("/dev/vda");
        bad.keyboard = "../x".into();
        assert!(plan(&bad).is_err());
        assert!(!text.contains("libva-mesa-driver"));
        assert!(loader_entry(UUID, &[]).contains(&format!("root=UUID={UUID} rw")));
        assert!(
            loader_entry(UUID, &["nvidia_drm.modeset=1"])
                .contains("vt.global_cursor_default=0 nvidia_drm.modeset=1\n")
        );
    }

    #[test]
    fn successful_run_writes_expected_files() {
        let p = plan(&cfg("/dev/vda")).unwrap();
        let mut host = MemHost::default();
        host.files.insert(
            m("/etc/locale.gen"),
            "# comment\n#en_US.UTF-8 UTF-8\n#en_GB.UTF-8 UTF-8\n".into(),
        );
        let mut events = Vec::new();
        execute(&p, &mut host, &mut |e| events.push(e)).unwrap();
        assert_eq!(events.last(), Some(&InstallEvent::Finished));
        assert_eq!(host.files[&m("/etc/hostname")], "catostv\n");
        assert_eq!(
            host.files[&m("/etc/locale.gen")],
            "# comment\nen_US.UTF-8 UTF-8\n#en_GB.UTF-8 UTF-8\n"
        );
        assert!(host.files[&m("/etc/fstab")].contains(UUID));
        assert!(host.files[&m("/boot/loader/entries/catostv.conf")].contains(UUID));
        // The account exists before the overlay is chowned.
        let useradd = host.ran.iter().position(|c| c.contains("useradd")).unwrap();
        let chown = host.ran.iter().position(|c| c.contains("chown")).unwrap();
        assert!(useradd < chown);
        assert_eq!(host.ran.last().unwrap(), "umount -R /mnt/catostv");
    }

    #[test]
    fn failed_check_reports_disk_untouched() {
        let p = plan(&cfg("/dev/vda")).unwrap();
        let mut host = MemHost {
            fail_check: Some(Check::Online),
            ..MemHost::default()
        };
        let mut events = Vec::new();
        assert!(execute(&p, &mut host, &mut |e| events.push(e)).is_err());
        match events.last().unwrap() {
            InstallEvent::Failed {
                disk_modified,
                message,
            } => {
                assert!(!disk_modified);
                assert!(message.starts_with("Checking your PC"));
            }
            e => panic!("{e:?}"),
        }
        assert!(
            !host
                .ran
                .iter()
                .any(|c| c.starts_with("sgdisk") || c.starts_with("wipefs"))
        );
    }

    #[test]
    fn failure_mid_install_cleans_up_and_reports_modification() {
        let p = plan(&cfg("/dev/vda")).unwrap();
        let mut host = MemHost {
            fail_program: Some("pacstrap".into()),
            ..MemHost::default()
        };
        let mut events = Vec::new();
        let err = execute(&p, &mut host, &mut |e| events.push(e)).unwrap_err();
        assert!(
            err.message()
                .contains("pacstrap failed (exit code 1: boom)"),
            "{err}"
        );
        assert!(matches!(
            events.last().unwrap(),
            InstallEvent::Failed {
                disk_modified: true,
                ..
            }
        ));
        assert_eq!(host.ran.last().unwrap(), "umount -R /mnt/catostv");
        assert!(host.log.iter().any(|l| l.starts_with("FAILED")));
    }

    #[test]
    fn dry_run_changes_nothing_but_still_checks_disk() {
        let p = plan(&cfg("/dev/sdz")).unwrap();
        let mut probe = FakeRunner::new();
        probe.push(Output::ok(
            "NAME=\"sdz\" PATH=\"/dev/sdz\" PKNAME=\"\" TYPE=\"disk\" SIZE=\"8000000000\" RO=\"0\" RM=\"0\" TRAN=\"sata\" MODEL=\"Tiny\" SERIAL=\"\" FSTYPE=\"\" LABEL=\"\" MOUNTPOINTS=\"\"",
        ));
        let mut host = DryRunHost::new(probe, Duration::ZERO);
        let mut events = Vec::new();
        let err = execute(&p, &mut host, &mut |e| events.push(e)).unwrap_err();
        assert!(
            err.message()
                .contains("Refusing to erase /dev/sdz: Too small"),
            "{err}"
        );
    }

    #[test]
    fn enable_line_variants() {
        assert_eq!(enable_line("#a b\n", "a b"), "a b\n");
        assert_eq!(enable_line("# a b\n", "a b"), "a b\n");
        assert_eq!(enable_line("a b\n", "a b"), "a b\n");
        assert_eq!(enable_line("x\n", "a b"), "x\na b\n");
    }

    #[test]
    fn uuid_and_cpu() {
        let u = random_uuid().unwrap();
        assert!(is_uuid(&u), "{u}");
        assert_eq!(&u[14..15], "4");
        assert_eq!(detect_cpu("vendor_id : AuthenticAMD"), CpuVendor::Amd);
        assert_eq!(detect_cpu("vendor_id\t: GenuineIntel"), CpuVendor::Intel);
        assert_eq!(detect_cpu(""), CpuVendor::Other);
    }
}
