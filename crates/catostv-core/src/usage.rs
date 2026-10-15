//! CPU and memory use per open app, for Recents.
//!
//! Every app CatosTV starts carries `CATOSTV_APP_ID=<id>` in its
//! environment ([`crate::lifecycle`]), and every process it starts inherits
//! it, also through Flatpak, Steam's launcher or a shell script, so an
//! app's processes are found by that marker, not by the process tree.
//! Android apps run in Android's container; on the host their processes
//! are named after the package (`org.smarttube.stable`,
//! `org.smarttube.stable:remote`).
//!
//! Only `/proc` is read: `stat` (CPU time), `statm` (resident memory),
//! `environ` (own processes only; others are skipped) and `cmdline`.
//! CPU is a share of the whole machine between two samples, like a task
//! manager's.

use std::collections::HashMap;
use std::path::Path;

/// Bytes per memory page on the TV's x86-64 kernels.
const PAGE: u64 = 4096;

/// One app's use.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AppUsage {
    /// Percent of the whole machine (all cores) since the last sample.
    pub cpu: f32,
    /// Resident memory in bytes.
    pub rss: u64,
}

/// The whole system and each app, by app id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub apps: HashMap<String, AppUsage>,
    /// Percent of the whole machine in use.
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_total: u64,
}

/// CPU ticks (user + system) from `/proc/<pid>/stat`. The command name in
/// parentheses may itself contain spaces and parentheses, so fields are
/// counted from the last `)`.
pub fn parse_stat_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // After ")": state(0) ppid(1) … utime(11) stime(12).
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// Resident pages from `/proc/<pid>/statm` (second field).
pub fn parse_statm_pages(statm: &str) -> Option<u64> {
    statm.split_whitespace().nth(1)?.parse().ok()
}

/// Total and idle ticks from the `cpu` line of `/proc/stat`.
pub fn parse_cpu_total(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|x| x.parse().ok())
        .collect();
    let total: u64 = v.iter().take(8).sum();
    // idle + iowait
    let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
    Some((total, idle))
}

/// (used, total) bytes from `/proc/meminfo` (used = total − available).
pub fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let kb = |key: &str| -> Option<u64> {
        let l = text.lines().find(|l| l.starts_with(key))?;
        l.split_whitespace().nth(1)?.parse().ok()
    };
    let total = kb("MemTotal:")? * 1024;
    let avail = kb("MemAvailable:")? * 1024;
    Some((total.saturating_sub(avail), total))
}

/// Which app a process belongs to: its `CATOSTV_APP_ID`, else an Android
/// package (`packages`: app id → package) named by its first argument.
pub fn app_of(environ: &[u8], cmdline: &[u8], packages: &[(String, String)]) -> Option<String> {
    const KEY: &[u8] = b"CATOSTV_APP_ID=";
    for var in environ.split(|b| *b == 0) {
        if let Some(id) = var.strip_prefix(KEY)
            && !id.is_empty()
        {
            return Some(String::from_utf8_lossy(id).into_owned());
        }
    }
    let argv0 = cmdline.split(|b| *b == 0).next().unwrap_or_default();
    let argv0 = std::str::from_utf8(argv0).ok()?;
    packages
        .iter()
        .find(|(_, pkg)| {
            argv0 == pkg
                || argv0
                    .strip_prefix(pkg.as_str())
                    .is_some_and(|r| r.starts_with(':'))
        })
        .map(|(id, _)| id.clone())
}

/// Share of `delta_total` machine ticks that `delta_app` used, in percent.
pub fn percent(delta_app: u64, delta_total: u64) -> f32 {
    if delta_total == 0 {
        return 0.0;
    }
    (delta_app as f32 / delta_total as f32 * 100.0).clamp(0.0, 100.0)
}

/// Keeps the previous sample so CPU can be computed from the difference.
#[derive(Debug, Default)]
pub struct Sampler {
    /// pid → (app id, ticks) at the last sample.
    prev: HashMap<u32, (String, u64)>,
    prev_cpu: Option<(u64, u64)>,
}

impl Sampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read `/proc` once. The first call reports 0 % CPU (no previous
    /// sample); memory is right from the start.
    pub fn sample(&mut self, packages: &[(String, String)]) -> Usage {
        self.sample_in(Path::new("/proc"), packages)
    }

    pub fn sample_in(&mut self, proc_dir: &Path, packages: &[(String, String)]) -> Usage {
        let mut usage = Usage::default();
        let cpu = std::fs::read_to_string(proc_dir.join("stat"))
            .ok()
            .and_then(|s| parse_cpu_total(&s));
        if let Some((used, total)) = std::fs::read_to_string(proc_dir.join("meminfo"))
            .ok()
            .and_then(|s| parse_meminfo(&s))
        {
            usage.mem_used = used;
            usage.mem_total = total;
        }
        let delta_total = match (cpu, self.prev_cpu) {
            (Some((t, _)), Some((pt, _))) => t.saturating_sub(pt),
            _ => 0,
        };
        if let (Some((t, i)), Some((pt, pi))) = (cpu, self.prev_cpu) {
            let dt = t.saturating_sub(pt);
            let di = i.saturating_sub(pi);
            usage.cpu = percent(dt.saturating_sub(di), dt);
        }
        let mut now: HashMap<u32, (String, u64)> = HashMap::new();
        let mut app_ticks: HashMap<String, u64> = HashMap::new();
        let Ok(dir) = std::fs::read_dir(proc_dir) else {
            return usage;
        };
        for entry in dir.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            let p = entry.path();
            let environ = std::fs::read(p.join("environ")).unwrap_or_default();
            let cmdline = std::fs::read(p.join("cmdline")).unwrap_or_default();
            let Some(app) = app_of(&environ, &cmdline, packages) else {
                continue;
            };
            let ticks = std::fs::read_to_string(p.join("stat"))
                .ok()
                .and_then(|s| parse_stat_ticks(&s));
            let pages = std::fs::read_to_string(p.join("statm"))
                .ok()
                .and_then(|s| parse_statm_pages(&s));
            let u = usage.apps.entry(app.clone()).or_default();
            u.rss += pages.unwrap_or(0) * PAGE;
            if let Some(ticks) = ticks {
                // Only time spent since the last sample; a new process
                // counts from now.
                let before = self
                    .prev
                    .get(&pid)
                    .filter(|(a, _)| *a == app)
                    .map_or(ticks, |(_, t)| *t);
                *app_ticks.entry(app.clone()).or_default() += ticks.saturating_sub(before);
                now.insert(pid, (app, ticks));
            }
        }
        for (app, ticks) in app_ticks {
            if let Some(u) = usage.apps.get_mut(&app) {
                u.cpu = percent(ticks, delta_total);
            }
        }
        self.prev = now;
        self.prev_cpu = cpu;
        usage
    }
}

/// "840 MB", "1.2 GB", short, for a TV.
pub fn bytes_short(b: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = b as f64 / MB;
    if mb >= 1000.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{} MB", mb.round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_ticks_survive_odd_names() {
        let s =
            "1409 (org.smart tube) (x)) S 103 103 0 0 -1 4194624 1 0 0 0 6179 2192 0 0 10 -10 120";
        assert_eq!(parse_stat_ticks(s), Some(6179 + 2192));
        assert_eq!(parse_stat_ticks("garbage"), None);
    }

    #[test]
    fn statm_meminfo_and_cpu_lines() {
        assert_eq!(parse_statm_pages("123456 2048 300 1 0 9000 0"), Some(2048));
        let mi = "MemTotal:       16000000 kB\nMemFree: 1 kB\nMemAvailable:   10000000 kB\n";
        assert_eq!(
            parse_meminfo(mi),
            Some((6_000_000 * 1024, 16_000_000 * 1024))
        );
        let st = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3\n";
        assert_eq!(parse_cpu_total(st), Some((1000, 850)));
    }

    #[test]
    fn processes_are_matched_to_apps() {
        let pk = vec![(
            "android-org-smarttube-stable".to_string(),
            "org.smarttube.stable".to_string(),
        )];
        assert_eq!(
            app_of(b"HOME=/x\0CATOSTV_APP_ID=steam\0", b"steam\0", &pk).as_deref(),
            Some("steam")
        );
        assert_eq!(
            app_of(b"", b"org.smarttube.stable:remote\0", &pk).as_deref(),
            Some("android-org-smarttube-stable")
        );
        assert_eq!(app_of(b"", b"org.smarttube.stablex\0", &pk), None);
        assert_eq!(app_of(b"CATOSTV_APP_ID=\0", b"bash\0", &pk), None);
    }

    #[test]
    fn percent_and_sizes() {
        assert_eq!(percent(50, 200), 25.0);
        assert_eq!(percent(5, 0), 0.0);
        assert_eq!(percent(500, 200), 100.0);
        assert_eq!(bytes_short(840 * 1024 * 1024), "840 MB");
        assert_eq!(bytes_short(1229 * 1024 * 1024), "1.2 GB");
    }

    #[test]
    fn sampler_reads_a_fake_proc() {
        let dir = std::env::temp_dir().join(format!("catostv-usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("42");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(dir.join("stat"), "cpu  100 0 0 900 0 0 0 0\n").unwrap();
        std::fs::write(
            dir.join("meminfo"),
            "MemTotal: 1000 kB\nMemAvailable: 400 kB\n",
        )
        .unwrap();
        std::fs::write(p.join("environ"), "CATOSTV_APP_ID=kodi\0").unwrap();
        std::fs::write(p.join("cmdline"), "kodi\0").unwrap();
        std::fs::write(p.join("stat"), "42 (kodi) S 1 1 0 0 -1 0 0 0 0 0 10 0 0").unwrap();
        std::fs::write(p.join("statm"), "100 25 0").unwrap();
        let mut s = Sampler::new();
        let first = s.sample_in(&dir, &[]);
        assert_eq!(first.apps["kodi"].rss, 25 * PAGE);
        assert_eq!(first.apps["kodi"].cpu, 0.0);
        assert_eq!((first.mem_used, first.mem_total), (600 * 1024, 1000 * 1024));
        // 100 more machine ticks, 20 of them kodi's.
        std::fs::write(dir.join("stat"), "cpu  150 0 0 950 0 0 0 0\n").unwrap();
        std::fs::write(p.join("stat"), "42 (kodi) S 1 1 0 0 -1 0 0 0 0 0 30 0 0").unwrap();
        let second = s.sample_in(&dir, &[]);
        assert_eq!(second.apps["kodi"].cpu, 20.0);
        assert_eq!(second.cpu, 50.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
