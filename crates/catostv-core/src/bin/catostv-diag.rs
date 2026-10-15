//! `catostv-diag`, read-only diagnostics for developers and support.
//!
//! Nothing in this tool modifies the system. `dry-run` executes an install
//! plan against the simulated host (only `lsblk`/`findmnt` queries run).
//! (`store QUERY` lets flatpak refresh its appstream cache, as any search does.)
//!
//! ```text
//! catostv-diag status                       system state as the shell sees it
//! catostv-diag disks                        install targets and why some are blocked
//! catostv-diag wifi                         nearby Wi-Fi networks (nmcli rescan)
//! catostv-diag plan --disk /dev/X [--timezone Area/City] [--keyboard us|de|…] [--cpu intel|amd|other]
//! catostv-diag dry-run --disk /dev/X [--timezone Area/City] [--keyboard us|de|…]
//! catostv-diag apps                         Flatpak/Android apps the drawer would show
//! catostv-diag store [QUERY]                installed Flatpaks + curated list, or a Flathub search
//! catostv-diag android                      Waydroid status and Android setup progress
//! catostv-diag packages                     packages an install of this version contains
//! catostv-diag windows                      app windows the compositor reports (run in the TV session)
//! catostv-diag android-sync                 redo the Android icon sync (run as the TV account)
//! catostv-diag version
//! ```

use catostv_core::install::{self, CpuVendor, DryRunHost, InstallConfig, InstallEvent};
use catostv_core::process::SystemRunner;
use catostv_core::{android, audio, bluetooth, desktop, disk, env, flatpak, ipc, net, timezone};
use std::time::Duration;

const USAGE: &str =
    "usage: catostv-diag <status|disks|wifi|plan|dry-run|apps|store|android|android-sync|packages|windows|version> [options]
  plan/dry-run options: --disk /dev/X  [--timezone Area/City]  [--keyboard us|de|…]  [--cpu intel|amd|other]";

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn config_from(args: &[String]) -> Result<InstallConfig, String> {
    let disk = opt(args, "--disk").ok_or("--disk /dev/X is required")?;
    let tz = opt(args, "--timezone").unwrap_or_else(|| timezone::COMMON[0].to_string());
    let kb = opt(args, "--keyboard").unwrap_or_else(|| catostv_core::keyboard::DEFAULT.to_string());
    let cpu = match opt(args, "--cpu").as_deref() {
        Some("intel") => CpuVendor::Intel,
        Some("amd") => CpuVendor::Amd,
        Some("other") => CpuVendor::Other,
        Some(x) => return Err(format!("unknown --cpu {x}")),
        None => install::detect_cpu(&std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default()),
    };
    InstallConfig::new(&disk, &tz, &kb, cpu).map_err(|e| e.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("version") => {
            println!("CatosTV {}", env::version());
            0
        }
        Some("status") => status(),
        Some("disks") => disks(),
        Some("wifi") => wifi(),
        Some("plan") => {
            match config_from(&args).and_then(|c| install::plan(&c).map_err(|e| e.to_string())) {
                Ok(p) => {
                    print!("{}", install::render(&p));
                    0
                }
                Err(e) => {
                    eprintln!("catostv-diag: {e}");
                    2
                }
            }
        }
        Some("dry-run") => dry_run(&args),
        Some("apps") => discovered_apps(),
        Some("store") => store(args.get(1).map(String::as_str)),
        Some("android") => android_status(),
        // The shell's Android icon sync, by hand (run as the TV account):
        // lists Android's apps and writes/removes CatosTV's desktop files.
        Some("android-sync") => match android::list_apps(&mut SystemRunner) {
            Ok(apps) => {
                let home = home_dir();
                let made = android::ensure_icons(&mut SystemRunner, &home, &apps);
                let changed = android::sync_tv_desktops(&home, &apps, android::is_booted()) || made;
                println!(
                    "{} Android apps; desktop files {}",
                    apps.len(),
                    if changed {
                        "updated"
                    } else {
                        "already current"
                    }
                );
                0
            }
            Err(e) => {
                eprintln!("catostv-diag: Android did not list its apps: {e}");
                1
            }
        },
        Some("windows") => match catostv_core::toplevel::list() {
            Ok(list) => {
                for w in list {
                    let state = match (w.activated, w.minimized) {
                        (true, _) => "front    ",
                        (_, true) => "minimized",
                        _ => "         ",
                    };
                    println!("{state} {:<40} {}", w.app_id, w.title);
                }
                0
            }
            Err(e) => {
                eprintln!("catostv-diag: {e}");
                1
            }
        },
        Some("packages") => {
            // One per line, for `pacman -T` in scripts/deploy.sh (which
            // installs only what an older install is missing).
            for p in install::BASE_PACKAGES.iter().chain(if install::DEBUG_SSH {
                install::DEBUG_PACKAGES
            } else {
                &[]
            }) {
                println!("{p}");
            }
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

fn status() -> i32 {
    let mut r = SystemRunner;
    println!("CatosTV {}", env::version());
    println!("appliance session : {}", env::is_appliance());
    println!("IPC socket        : {}", ipc::socket_path().display());
    println!("apps config       : {}", env::apps_config_path().display());
    println!("state/log dir     : {}", env::state_dir().display());
    match net::wifi_enabled(&mut r) {
        Ok(on) => println!("Wi-Fi radio       : {}", if on { "on" } else { "off" }),
        Err(e) => println!("Wi-Fi radio       : unknown ({e})"),
    }
    match net::status(&mut r) {
        Ok(s) => println!(
            "network           : {} ({})",
            if s.online { "online" } else { "offline" },
            s.summary()
        ),
        Err(e) => println!("network           : unknown ({e})"),
    }
    let bt = bluetooth::status(&mut r);
    println!(
        "bluetooth         : {}",
        match (bt.available, bt.powered) {
            (false, _) => "unavailable",
            (true, true) => "on",
            (true, false) => "off",
        }
    );
    match audio::get(&mut r) {
        Ok(v) => println!(
            "volume            : {}%{}",
            v.percent,
            if v.muted { " (muted)" } else { "" }
        ),
        Err(e) => println!("volume            : unknown ({e})"),
    }
    0
}

fn disks() -> i32 {
    let mut r = SystemRunner;
    match disk::scan(&mut r) {
        Ok(list) => {
            if list.is_empty() {
                println!("No whole disks found.");
            }
            for c in list {
                let state = match &c.blocked {
                    None => "ELIGIBLE".to_string(),
                    Some(reason) => format!("BLOCKED: {reason}"),
                };
                println!(
                    "{}  {}\n    {}\n    {}",
                    c.disk.path,
                    c.title(),
                    c.detail(),
                    state
                );
                for w in &c.warnings {
                    println!("    warning: {w}");
                }
            }
            0
        }
        Err(e) => {
            eprintln!("catostv-diag: {e}");
            1
        }
    }
}

fn wifi() -> i32 {
    match net::scan(&mut SystemRunner) {
        Ok(list) => {
            for n in list {
                println!(
                    "{} {:>3}%  {}{}",
                    if n.in_use { "*" } else { " " },
                    n.signal,
                    n.ssid,
                    if n.secure { "" } else { "  (open)" }
                );
            }
            0
        }
        Err(e) => {
            eprintln!("catostv-diag: {e}");
            1
        }
    }
}

fn dry_run(args: &[String]) -> i32 {
    let cfg = match config_from(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("catostv-diag: {e}");
            return 2;
        }
    };
    let plan = match install::plan(&cfg) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("catostv-diag: {e}");
            return 2;
        }
    };
    let mut host = DryRunHost::new(SystemRunner, Duration::ZERO);
    let result = install::execute(&plan, &mut host, &mut |ev| match ev {
        InstallEvent::Step {
            index,
            total,
            label,
        } => println!("[{}/{}] {label}", index + 1, total),
        InstallEvent::Detail { text, .. } => println!("      {text}"),
        InstallEvent::Finished => println!("finished (dry run, nothing changed)"),
        InstallEvent::Failed {
            message,
            disk_modified,
        } => {
            println!("FAILED: {message} (a real run would have modified the disk: {disk_modified})")
        }
    });
    for line in &host.transcript {
        println!("    {line}");
    }
    if result.is_ok() { 0 } else { 1 }
}

fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/home/catostv".into())
}

fn discovered_apps() -> i32 {
    let apps = desktop::discover(&home_dir());
    if apps.is_empty() {
        println!("No Flatpak or Android apps found.");
    }
    for a in apps {
        println!(
            "{:<32} {:<8} {}\n    command: {}\n    icon:    {}",
            a.id,
            a.subtitle,
            a.name,
            a.command,
            a.icon_path.as_deref().unwrap_or("(monogram)")
        );
    }
    0
}

fn store(query: Option<&str>) -> i32 {
    let mut r = SystemRunner;
    if let Some(q) = query {
        return match flatpak::search(&mut r, q) {
            Ok(list) => {
                for a in list {
                    println!(
                        "{:<44} {} - {} [{}]",
                        a.id,
                        a.name,
                        a.description,
                        a.remotes.join(",")
                    );
                }
                0
            }
            Err(e) => {
                eprintln!("catostv-diag: {e}");
                1
            }
        };
    }
    match flatpak::installed(&mut r) {
        Ok(list) => {
            println!("Installed Flatpak apps:");
            for a in list {
                println!(
                    "  {:<44} {} {} ({})",
                    a.id, a.name, a.version, a.installation
                );
            }
        }
        Err(e) => println!("Installed Flatpak apps: unknown ({e})"),
    }
    let home = home_dir();
    for cat in flatpak::CATEGORIES {
        println!("{cat}:");
        for a in flatpak::curated_in(cat) {
            let icon = flatpak::store_icon_path(&home, a.id)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(no cached icon)".into());
            println!("  {:<36} {:<12} {}  {icon}", a.id, a.name, a.blurb);
        }
    }
    0
}

fn android_status() -> i32 {
    let st = android::status(&mut SystemRunner);
    println!("waydroid installed : {}", st.installed);
    println!("initialized        : {}", st.initialized);
    println!("container service  : {}", st.container);
    println!(
        "session            : {}{}",
        st.session,
        st.container_state
            .as_deref()
            .map(|c| format!(" (container {c})"))
            .unwrap_or_default()
    );
    println!("setup done         : {}", android::is_setup_done());
    match android::read_setup_state() {
        Some(s) => println!(
            "setup state        : step {}/{} {} - {:?}",
            s.step, s.total, s.message, s.result
        ),
        None => println!("setup state        : not run since boot"),
    }
    println!(
        "front app          : {}",
        android::read_top().unwrap_or_else(|| "(none)".into())
    );
    match android::read_sf() {
        Some((pid, restarts)) => println!("screen service     : pid {pid}, {restarts} restart(s)"),
        None => println!("screen service     : not watched (catostv-android-watch not running?)"),
    }
    // The display filter: the relay socket in the TV account's
    // runtime dir (not the caller's: the debug `ok` user has its own, and
    // cannot look into catostv's without sudo), and which socket the running
    // session was started on.
    let relay = std::path::PathBuf::from(format!(
        "/run/user/{}/{}",
        catostv_uid().unwrap_or(1000),
        catostv_core::wlfilter::ANDROID_DISPLAY
    ));
    let relay_state = match std::fs::metadata(&relay) {
        Ok(_) => "up".to_string(),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            format!(
                "unknown ({} is not readable as this user: run with sudo)",
                relay.display()
            )
        }
        Err(_) => "MISSING (catostv-wlfilter not running?)".to_string(),
    };
    // `waydroid status` names the session's socket (this Waydroid keeps
    // the session in its container service, not in a file).
    let session_display = st.wayland_display.clone();
    println!(
        "display filter     : relay socket {relay_state}. Session on {}{}",
        session_display.as_deref().unwrap_or("(no session)"),
        match session_display.as_deref() {
            Some(d) if d == catostv_core::wlfilter::ANDROID_DISPLAY => " (deadlock protection on)",
            Some(_) => " (NOT through the filter: deadlock possible)",
            None => "",
        }
    );
    // The in-container checks need root (lxc-attach).
    // What the shell's app sync sees: the list as `waydroid app
    // list` answers it, or the error that keeps the sync waiting.
    if !is_root() {
        match android::list_apps(&mut SystemRunner) {
            Ok(apps) => {
                println!("android apps       : {}", apps.len());
                for a in &apps {
                    let kind = if android::is_hidden_package(&a.package) {
                        "hidden"
                    } else if a.tv_only() {
                        "TV icon"
                    } else if a.launchable() {
                        "icon"
                    } else {
                        "no launcher entry"
                    };
                    println!("  {:<44} {kind:<18} {}", a.package, a.name);
                }
            }
            Err(e) => println!("android apps       : not listed - {e}"),
        }
    }
    println!();
    if is_root() {
        match std::process::Command::new(android::DIAG_SCRIPT).status() {
            Ok(st) if st.success() => {}
            Ok(st) => println!("{} exited with {st}", android::DIAG_SCRIPT),
            Err(e) => println!("could not run {}: {e}", android::DIAG_SCRIPT),
        }
    } else {
        println!(
            "(run `sudo catostv-diag android` for the screen-service and network checks inside Android)"
        );
    }
    0
}

/// The TV account's uid from /etc/passwd (`catostv`), if it exists here.
fn catostv_uid() -> Option<u32> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == catostv_core::install::USER).then(|| f.nth(1)?.parse().ok())?
    })
}

fn is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1).map(|u| u == "0"))
        })
        .unwrap_or(false)
}
