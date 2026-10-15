//! Settings → Updates.
//!
//! A release is described by a signed **manifest** (JSON) at the address in
//! [`CONFIG`]:
//!
//! ```json
//! {"version": "0.5.0", "archive_date": "2026/10/15",
//!  "notes": ["Recents shows what each app was doing", "…"],
//!  "package": {"url": "https://…/catostv-0.5.0.tar.zst",
//!              "sha256": "<64 hex digits>"}}
//! ```
//!
//! next to `manifest.json.sig`, a detached GnuPG signature by the key in
//! [`KEYRING`]. `archive_date` pins every Arch package to that day of the
//! Arch Linux Archive, so each CatosTV version runs exactly the packages it
//! was tested with. The interface only *checks* (this module, as the TV
//! account); installing is the root unit [`INSTALL_UNIT`], which downloads
//! and verifies everything again itself and never uses files the interface
//! fetched.

use crate::mpv::{Json, parse_json};
use crate::process::{Cmd, Runner};
use std::path::Path;
use std::time::Duration;

/// `url = https://example.org/catostv/stable/manifest.json`. No file or no
/// URL: "Updates aren't set up yet".
pub const CONFIG: &str = "/etc/catostv/updates.conf";
/// The release signing key (a GnuPG keyring with the public key only).
pub const KEYRING: &str = "/usr/share/catostv/updates.gpg";
pub const INSTALL_UNIT: &str = "catostv-update.service";
/// "<phase>\t<detail>" written by the install unit: `running`, `done`
/// (detail = new version) or `failed` (detail = reason).
pub const STATE: &str = "/run/catostv/update-state";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Manifest {
    pub version: String,
    /// "YYYY/MM/DD" on archive.archlinux.org.
    pub archive_date: String,
    pub notes: Vec<String>,
    pub package_url: String,
    pub package_sha256: String,
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// No address configured, or no signing key installed.
    NotSetUp,
    UpToDate,
    Available(Manifest),
}

/// The install unit's progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallState {
    Running(String),
    Done(String),
    Failed(String),
}

pub fn parse_state(s: &str) -> Option<InstallState> {
    let line = s.lines().next()?;
    let (phase, detail) = line.split_once('\t').unwrap_or((line, ""));
    let detail = detail.to_string();
    match phase {
        "running" => Some(InstallState::Running(detail)),
        "done" => Some(InstallState::Done(detail)),
        "failed" => Some(InstallState::Failed(detail)),
        _ => None,
    }
}

pub fn read_state() -> Option<InstallState> {
    parse_state(&std::fs::read_to_string(STATE).ok()?)
}

/// The manifest address from the config text.
pub fn parse_config(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == "url").then(|| v.trim().to_string())
        })
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
}

pub fn configured_url() -> Option<String> {
    parse_config(&std::fs::read_to_string(CONFIG).ok()?)
}

fn is_archive_date(s: &str) -> bool {
    let p: Vec<&str> = s.split('/').collect();
    p.len() == 3
        && p[0].len() == 4
        && p[1].len() == 2
        && p[2].len() == 2
        && p.iter().all(|x| x.chars().all(|c| c.is_ascii_digit()))
}

fn is_version(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && s.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// Parse and check a manifest; `None` if anything is missing or malformed.
pub fn parse_manifest(text: &str) -> Option<Manifest> {
    let j = parse_json(text)?;
    let s = |v: Option<&Json>| v.and_then(Json::as_str).map(str::to_string);
    let pkg = j.get("package")?;
    let m = Manifest {
        version: s(j.get("version"))?,
        archive_date: s(j.get("archive_date"))?,
        notes: j
            .get("notes")
            .and_then(Json::as_arr)
            .map(|a| {
                a.iter()
                    .filter_map(|n| n.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        package_url: s(pkg.get("url"))?,
        package_sha256: s(pkg.get("sha256"))?.to_ascii_lowercase(),
    };
    let ok = is_version(&m.version)
        && is_archive_date(&m.archive_date)
        && (m.package_url.starts_with("https://") || m.package_url.starts_with("http://"))
        && m.package_sha256.len() == 64
        && m.package_sha256.chars().all(|c| c.is_ascii_hexdigit());
    ok.then_some(m)
}

/// Compare "0.4.0-dev" style versions: numbers part by part; a version with
/// a suffix ("-dev", "-rc1") comes before the same numbers without one.
pub fn newer(candidate: &str, current: &str) -> bool {
    fn split(v: &str) -> (Vec<u64>, bool) {
        let (nums, suffix) = v.split_once('-').map_or((v, false), |(n, _)| (n, true));
        (
            nums.split('.').map(|x| x.parse().unwrap_or(0)).collect(),
            suffix,
        )
    }
    let (a, a_pre) = split(candidate);
    let (b, b_pre) = split(current);
    let n = a.len().max(b.len());
    for i in 0..n {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    !a_pre && b_pre
}

/// pacman's mirror list for one day of the Arch Linux Archive.
pub fn archive_mirrorlist(date: &str) -> String {
    format!(
        "# CatosTV: packages as of {date} (Arch Linux Archive), set by the updater.\n\
         Server = https://archive.archlinux.org/repos/{date}/$repo/os/$arch\n"
    )
}

/// Fetch the manifest and its signature, verify, compare with the running
/// version. Runs `curl` and `gpgv`; call from a worker thread.
pub fn check(r: &mut dyn Runner) -> Result<Check, String> {
    let Some(url) = configured_url() else {
        return Ok(Check::NotSetUp);
    };
    if !Path::new(KEYRING).exists() {
        return Ok(Check::NotSetUp);
    }
    let dir = crate::env::runtime_dir().join("catostv-update-check");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (man, sig) = (dir.join("manifest.json"), dir.join("manifest.json.sig"));
    for (u, p) in [(url.clone(), &man), (format!("{url}.sig"), &sig)] {
        let out = r
            .run(
                &Cmd::new("curl")
                    .args(["-fsSL", "--max-filesize", "1048576", "-o"])
                    .arg(p.to_string_lossy().to_string())
                    .arg(u)
                    .timeout(Duration::from_secs(30)),
            )
            .map_err(|e| e.to_string())?;
        if !out.success() {
            return Err("Can't reach the update server".into());
        }
    }
    let out = r
        .run(
            &Cmd::new("gpgv")
                .args(["--keyring", KEYRING])
                .arg(sig.to_string_lossy().to_string())
                .arg(man.to_string_lossy().to_string())
                .timeout(Duration::from_secs(20)),
        )
        .map_err(|e| e.to_string())?;
    if !out.success() {
        return Err("The update is not signed by CatosTV. Ignored".into());
    }
    let text = std::fs::read_to_string(&man).map_err(|e| e.to_string())?;
    let m = parse_manifest(&text).ok_or("The update information is damaged")?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(if newer(&m.version, crate::env::version()) {
        Check::Available(m)
    } else {
        Check::UpToDate
    })
}

/// Start the install unit (polkit lets the TV account start exactly it).
pub fn install_cmd() -> Cmd {
    Cmd::new("systemctl").args(["--no-ask-password", "start", "--no-block", INSTALL_UNIT])
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"version": "0.5.0", "archive_date": "2026/10/15",
        "notes": ["Recents previews", "Debug page"],
        "package": {"url": "https://example.org/catostv-0.5.0.tar.zst",
                    "sha256": "AB12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12"}}"#;

    #[test]
    fn manifests_are_checked() {
        let m = parse_manifest(GOOD).unwrap();
        assert_eq!(m.version, "0.5.0");
        assert_eq!(m.notes.len(), 2);
        assert!(m.package_sha256.starts_with("ab12"));
        for bad in [
            GOOD.replace("2026/10/15", "2026-10-15"),
            GOOD.replace("\"0.5.0\"", "\"../x\""),
            GOOD.replace("https://example.org", "file:///etc"),
            GOOD.replace("AB12ab12", "zz"),
            "{}".to_string(),
        ] {
            assert!(parse_manifest(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn versions_compare() {
        assert!(newer("0.5.0", "0.4.0-dev"));
        assert!(newer("0.4.0", "0.4.0-dev"));
        assert!(!newer("0.4.0-dev", "0.4.0"));
        assert!(!newer("0.4.0", "0.4.0"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.3.9", "0.4.0-dev"));
    }

    #[test]
    fn config_state_and_mirrorlist() {
        assert_eq!(
            parse_config("# x\nurl = https://u.example/m.json\n").as_deref(),
            Some("https://u.example/m.json")
        );
        assert_eq!(parse_config("url = ftp://x"), None);
        assert_eq!(parse_config(""), None);
        assert_eq!(
            parse_state("running\tUpdating system packages\n"),
            Some(InstallState::Running("Updating system packages".into()))
        );
        assert_eq!(
            parse_state("done\t0.5.0"),
            Some(InstallState::Done("0.5.0".into()))
        );
        assert_eq!(parse_state("weird"), None);
        assert!(
            archive_mirrorlist("2026/10/15")
                .contains("Server = https://archive.archlinux.org/repos/2026/10/15/$repo/os/$arch")
        );
    }
}
