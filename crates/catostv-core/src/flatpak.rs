//! Flatpak "app store" backend (Flathub, **per-user** installation).
//!
//! CatosTV installs Flatpaks with `--user` into `~/.local/share/flatpak`, so
//! installing, updating and removing apps never needs root or polkit. The
//! store UI uses:
//!
//! * [`ensure_flathub_user`] once (adds the Flathub remote for the user);
//! * [`CURATED`] for the front page and [`search`] for everything else;
//! * [`installed`] for "My apps", [`install`] / [`uninstall`] / [`update_all`];
//! * [`store_icon_path`] for result icons (Flathub's appstream icon cache).
//!
//! Output formats were checked against the flatpak source (1.18/1.19
//! `app/flatpak-table-printer.c`, `flatpak-quiet-transaction.c`,
//! `flatpak-cli-transaction.c`): when stdout is not a terminal, tables are
//! printed **without a header**, one row per line, cells separated by a
//! single TAB and never ellipsised; trailing empty cells may be cut off.
//! Installed apps then appear in the drawer through [`crate::desktop`].

use crate::error::{Error, Result};
use crate::process::{Cmd, Output, Runner, SLOW};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Name of the remote CatosTV manages.
pub const REMOTE: &str = "flathub";
/// Flathub's repository description (as used by flathub.org/setup).
pub const FLATHUB_REPO_URL: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";
/// System-wide Flatpak installation (`flatpak --system`).
pub const SYSTEM_DIR: &str = "/var/lib/flatpak";
/// Per-user installation relative to `$HOME`.
pub const USER_DIR_REL: &str = ".local/share/flatpak";

/// `flatpak search` may first download Flathub's appstream data (~10 MB).
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(120);
/// Installing an app may download a 1–2 GB runtime first.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(60 * 60 * 2);
/// Updating everything may download several runtimes.
pub const UPDATE_TIMEOUT: Duration = Duration::from_secs(60 * 60 * 3);

/// A store catalogue entry. Every id was checked against
/// `https://flathub.org/api/v2/appstream/<id>` on **2026-10-14** (exists,
/// `type: desktop-application`, not end-of-life).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CuratedApp {
    pub id: &'static str,
    pub name: &'static str,
    pub blurb: &'static str,
    pub category: &'static str,
}

/// Store categories in display order.
pub const CATEGORIES: &[&str] = &["Watch", "Listen", "Play", "Stream", "Tools"];

const fn c(
    id: &'static str,
    name: &'static str,
    blurb: &'static str,
    category: &'static str,
) -> CuratedApp {
    CuratedApp {
        id,
        name,
        blurb,
        category,
    }
}

/// TV-focused front page of the store.
pub const CURATED: &[CuratedApp] = &[
    // Watch
    c(
        "tv.kodi.Kodi",
        "Kodi",
        "The classic TV media center",
        "Watch",
    ),
    c(
        "org.jellyfin.JellyfinDesktop",
        "Jellyfin",
        "Your Jellyfin server on the TV",
        "Watch",
    ),
    c(
        "tv.plex.PlexHTPC",
        "Plex HTPC",
        "Plex, made for the big screen",
        "Watch",
    ),
    c(
        "com.stremio.Stremio",
        "Stremio",
        "Movies and series from add-ons",
        "Watch",
    ),
    c(
        "io.freetubeapp.FreeTube",
        "FreeTube",
        "YouTube without ads or tracking",
        "Watch",
    ),
    c(
        "app.grayjay.Grayjay",
        "Grayjay",
        "Many video platforms in one app",
        "Watch",
    ),
    c(
        "org.videolan.VLC",
        "VLC",
        "Plays almost any video file",
        "Watch",
    ),
    c("io.mpv.Mpv", "mpv", "Lightweight video player", "Watch"),
    // Listen
    c(
        "com.spotify.Client",
        "Spotify",
        "Music and podcasts",
        "Listen",
    ),
    c(
        "com.plexamp.Plexamp",
        "Plexamp",
        "Music from your Plex server",
        "Listen",
    ),
    c(
        "de.haeckerfelix.Shortwave",
        "Shortwave",
        "Internet radio stations",
        "Listen",
    ),
    c(
        "org.gnome.Podcasts",
        "Podcasts",
        "Follow your favourite shows",
        "Listen",
    ),
    // Play
    c(
        "com.valvesoftware.Steam",
        "Steam",
        "Your PC game library",
        "Play",
    ),
    c(
        "org.libretro.RetroArch",
        "RetroArch",
        "Retro consoles in one app",
        "Play",
    ),
    c(
        "com.heroicgameslauncher.hgl",
        "Heroic",
        "Epic, GOG and Amazon games",
        "Play",
    ),
    c(
        "net.lutris.Lutris",
        "Lutris",
        "Game library manager",
        "Play",
    ),
    c(
        "org.DolphinEmu.dolphin-emu",
        "Dolphin",
        "GameCube and Wii games",
        "Play",
    ),
    c(
        "org.ppsspp.PPSSPP",
        "PPSSPP",
        "PlayStation Portable games",
        "Play",
    ),
    // Stream
    c(
        "com.moonlight_stream.Moonlight",
        "Moonlight",
        "Stream games from your PC",
        "Stream",
    ),
    c(
        "com.valvesoftware.SteamLink",
        "Steam Link",
        "Stream Steam from another PC",
        "Stream",
    ),
    c(
        "io.github.streetpea.Chiaki4deck",
        "chiaki-ng",
        "PlayStation Remote Play",
        "Stream",
    ),
    // Tools
    c(
        "com.github.tchx84.Flatseal",
        "Flatseal",
        "Review app permissions",
        "Tools",
    ),
];

/// Curated apps of one category, in catalogue order.
pub fn curated_in(category: &str) -> Vec<&'static CuratedApp> {
    CURATED.iter().filter(|a| a.category == category).collect()
}

/// Validate a Flatpak application id before it is used as an argument:
/// reverse-DNS, at least 3 components of `[A-Za-z0-9_-]`, no empty
/// component, no component starting with a digit or `-` (so it can never
/// look like an option), at most 255 characters.
pub fn is_valid_app_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 255 {
        return false;
    }
    let parts: Vec<&str> = id.split('.').collect();
    parts.len() >= 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && !p.starts_with(|c: char| c.is_ascii_digit() || c == '-')
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

fn check_id(id: &str) -> Result<()> {
    if is_valid_app_id(id) {
        Ok(())
    } else {
        Err(Error::msg(format!("{id:?} is not a valid Flatpak app id")))
    }
}

/// True for search results that are add-ons rather than apps (locales,
/// debug info, plugins, runtimes and their extensions).
pub fn is_addon_id(id: &str) -> bool {
    let last = id.rsplit('.').next().unwrap_or("");
    matches!(last, "Locale" | "Debug" | "Sources" | "Platform" | "Sdk")
        || [
            ".Plugin.",
            ".Plugins.",
            ".Extension.",
            ".Codecs",
            ".Platform.",
            ".Sdk.",
        ]
        .iter()
        .any(|p| id.contains(p))
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreApp {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub remotes: Vec<String>,
}

/// Columns requested from `flatpak search` (order matters for parsing).
pub const SEARCH_COLUMNS: &str = "application,name,description,version,remotes";
/// Columns requested from `flatpak list`.
pub const LIST_COLUMNS: &str = "application,name,version,installation";

fn cells(line: &str) -> Vec<String> {
    line.split('\t').map(|c| c.trim().to_string()).collect()
}

/// Parse `flatpak search --columns=application,name,description,version,remotes`
/// (non-tty output). Invalid ids and add-ons are skipped; duplicates merged.
pub fn parse_search(text: &str) -> Vec<StoreApp> {
    let mut out: Vec<StoreApp> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.trim() == "No matches found" {
            continue;
        }
        let c = cells(line);
        let id = c[0].clone();
        if !is_valid_app_id(&id) || is_addon_id(&id) {
            continue;
        }
        let get = |i: usize| c.get(i).cloned().unwrap_or_default();
        let remotes: Vec<String> = get(4)
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if let Some(existing) = out.iter_mut().find(|a| a.id == id) {
            for r in remotes {
                if !existing.remotes.contains(&r) {
                    existing.remotes.push(r);
                }
            }
            continue;
        }
        let name = get(1);
        out.push(StoreApp {
            name: if name.is_empty() { id.clone() } else { name },
            id,
            description: get(2),
            version: get(3),
            remotes,
        });
    }
    out
}

/// Normalise a search query typed on the on-screen keyboard. Leading dashes
/// are removed so the query can never be read as an option.
pub fn clean_query(query: &str) -> Option<String> {
    let q: String = query
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .trim_start_matches('-')
        .trim()
        .chars()
        .take(64)
        .collect();
    (!q.is_empty()).then_some(q)
}

pub fn search_cmd(query: &str) -> Result<Cmd> {
    let q = clean_query(query).ok_or_else(|| Error::msg("Type something to search for"))?;
    Ok(Cmd::new("flatpak")
        .args([
            "search".to_string(),
            format!("--columns={SEARCH_COLUMNS}"),
            q,
        ])
        .timeout(SEARCH_TIMEOUT))
}

/// Search Flathub (and any other configured remote). Results containing
/// [`REMOTE`] come first, otherwise flatpak's relevance order is kept.
pub fn search(r: &mut dyn Runner, query: &str) -> Result<Vec<StoreApp>> {
    let out = r.run_ok(&search_cmd(query)?)?;
    let mut apps = parse_search(&out.stdout);
    apps.sort_by_key(|a| !a.remotes.iter().any(|r| r == REMOTE));
    Ok(apps)
}

/// One installed app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
    pub version: String,
    /// `user`, `system` or `system (<name>)`.
    pub installation: String,
}

impl InstalledApp {
    pub fn is_user(&self) -> bool {
        self.installation == "user"
    }
}

/// Parse `flatpak list --app --columns=application,name,version,installation`.
pub fn parse_list(text: &str) -> Vec<InstalledApp> {
    let mut out: Vec<InstalledApp> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let c = cells(line);
        if !is_valid_app_id(&c[0]) {
            continue;
        }
        let get = |i: usize| c.get(i).cloned().unwrap_or_default();
        let app = InstalledApp {
            id: c[0].clone(),
            name: if get(1).is_empty() {
                c[0].clone()
            } else {
                get(1)
            },
            version: get(2),
            installation: get(3),
        };
        // An app installed with several branches is listed once.
        if !out
            .iter()
            .any(|a| a.id == app.id && a.installation == app.installation)
        {
            out.push(app);
        }
    }
    out
}

pub fn list_cmd() -> Cmd {
    Cmd::new("flatpak")
        .args([
            "list".to_string(),
            "--app".into(),
            format!("--columns={LIST_COLUMNS}"),
        ])
        .timeout(SLOW)
}

/// Installed apps of every installation (user and system).
pub fn installed(r: &mut dyn Runner) -> Result<Vec<InstalledApp>> {
    Ok(parse_list(&r.run_ok(&list_cmd())?.stdout))
}

/// `flatpak remote-add --user --if-not-exists flathub <url>`
pub fn remote_add_cmd() -> Cmd {
    Cmd::new("flatpak")
        .args([
            "remote-add",
            "--user",
            "--if-not-exists",
            REMOTE,
            FLATHUB_REPO_URL,
        ])
        .timeout(SLOW)
}

/// Make sure the user has the Flathub remote. Idempotent; needs internet the
/// first time (flatpak fetches the repo description and its GPG key).
pub fn ensure_flathub_user(r: &mut dyn Runner) -> Result<()> {
    r.run_ok(&remote_add_cmd())
        .map(|_| ())
        .map_err(|e| e.context("Could not add Flathub"))
}

/// Download fresh appstream data (store names, descriptions, icons).
pub fn refresh_appstream_cmd() -> Cmd {
    Cmd::new("flatpak")
        .args(["update", "--user", "--appstream", REMOTE])
        .timeout(SEARCH_TIMEOUT)
}

/// `flatpak install --user --noninteractive -y flathub <id>`.
///
/// `--noninteractive` prints one `Installing <ref>` line per operation and
/// no percentages. For a percentage, run [`install_progress_cmd`] instead
/// and feed its stdout lines to [`InstallProgress::feed`].
pub fn install_cmd(app_id: &str) -> Result<Cmd> {
    check_id(app_id)?;
    Ok(Cmd::new("flatpak")
        .args([
            "install",
            "--user",
            "--noninteractive",
            "-y",
            REMOTE,
            app_id,
        ])
        .timeout(INSTALL_TIMEOUT))
}

/// Like [`install_cmd`] but without `--noninteractive`: with stdout not a
/// terminal flatpak prints `Installing 2/3… <bar>  45%  1.2 MB/s` lines
/// (every ~300 ms). `-y` answers every question; stdin is closed, so an
/// unexpected prompt aborts instead of hanging.
pub fn install_progress_cmd(app_id: &str) -> Result<Cmd> {
    check_id(app_id)?;
    Ok(Cmd::new("flatpak")
        .args(["install", "--user", "-y", REMOTE, app_id])
        .timeout(INSTALL_TIMEOUT))
}

pub fn uninstall_cmd(app_id: &str) -> Result<Cmd> {
    check_id(app_id)?;
    Ok(Cmd::new("flatpak")
        .args(["uninstall", "--user", "--noninteractive", "-y", app_id])
        .timeout(SLOW))
}

/// Update every user-installed app and runtime.
pub fn update_all_cmd() -> Cmd {
    Cmd::new("flatpak")
        .args(["update", "--user", "--noninteractive", "-y"])
        .timeout(UPDATE_TIMEOUT)
}

/// Remove runtimes no installed app needs any more (after uninstalling).
pub fn remove_unused_cmd() -> Cmd {
    Cmd::new("flatpak")
        .args(["uninstall", "--user", "--unused", "--noninteractive", "-y"])
        .timeout(SLOW)
}

/// Coarse state of an install/update/uninstall.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InstallPhase {
    /// Nothing printed yet (resolving refs, fetching metadata).
    #[default]
    Starting,
    /// Downloading / deploying an app or runtime.
    Working,
    /// Finished (including "already installed").
    Done,
    Failed(String),
}

/// Progress of a flatpak transaction, built from its output lines. Works
/// for both [`install_cmd`] (`Installing <ref>`) and
/// [`install_progress_cmd`] (`Installing 2/3… ▊▊▊  45%`) output.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InstallProgress {
    pub phase: InstallPhase,
    /// 1-based operation number (app, runtime, extension …).
    pub step: u32,
    /// Number of operations, when flatpak said so.
    pub total: Option<u32>,
    /// Download percentage of the current operation, when known.
    pub percent: Option<u8>,
    /// Ref being processed (`app/tv.kodi.Kodi/x86_64/stable`), quiet mode only.
    pub current: Option<String>,
    pub already_installed: bool,
}

const OP_WORDS: &[&str] = &["Installing", "Updating", "Uninstalling"];

impl InstallProgress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Overall fraction 0.0–1.0 when it can be estimated.
    pub fn fraction(&self) -> Option<f32> {
        match self.phase {
            InstallPhase::Done => return Some(1.0),
            InstallPhase::Starting => return Some(0.0),
            _ => {}
        }
        let total = self.total? as f32;
        let done = self.step.saturating_sub(1) as f32;
        let cur = f32::from(self.percent.unwrap_or(0)) / 100.0;
        Some(((done + cur) / total).clamp(0.0, 1.0))
    }

    /// Feed one line of stdout or stderr.
    pub fn feed(&mut self, line: &str) {
        // Progress may be redrawn with \r; only the last segment counts.
        let line = line.rsplit('\r').next().unwrap_or("").trim();
        if line.is_empty() || matches!(self.phase, InstallPhase::Failed(_)) {
            return;
        }
        if matches!(
            line,
            "Installation complete."
                | "Updates complete."
                | "Uninstall complete."
                | "Changes complete."
        ) {
            self.phase = InstallPhase::Done;
            self.percent = Some(100);
            return;
        }
        if let Some(rest) = line.strip_prefix("Skipping: ") {
            if rest.contains("already installed") {
                self.already_installed = true;
                if self.phase == InstallPhase::Starting {
                    self.phase = InstallPhase::Done;
                }
            }
            return;
        }
        for prefix in ["Error: ", "error: "] {
            if let Some(msg) = line.strip_prefix(prefix) {
                if msg.contains("already installed") {
                    self.already_installed = true;
                    self.phase = InstallPhase::Done;
                } else {
                    self.phase = InstallPhase::Failed(msg.trim().to_string());
                }
                return;
            }
        }
        let Some((word, rest)) = line.split_once([' ', '…']) else {
            if OP_WORDS.iter().any(|w| line == format!("{w}…")) {
                self.start_op(1, Some(1), None);
            }
            return;
        };
        if !OP_WORDS.contains(&word) {
            return;
        }
        let rest = rest.trim();
        if line.starts_with(&format!("{word}…")) {
            // "Installing… <bar>  45%" (single operation)
            if self.step != 1 || self.phase != InstallPhase::Working {
                self.start_op(1, Some(1), None);
            }
            self.percent = parse_percent(rest).or(self.percent);
            return;
        }
        // "Installing 2/3… <bar>  45%"
        if let Some((counter, tail)) = rest.split_once('…')
            && let Some((n, m)) = counter.split_once('/')
            && let (Ok(n), Ok(m)) = (n.trim().parse::<u32>(), m.trim().parse::<u32>())
        {
            if self.step != n || self.phase != InstallPhase::Working {
                self.start_op(n, Some(m), None);
            }
            self.percent = parse_percent(tail).or(self.percent);
            return;
        }
        // Quiet mode: "Installing app/tv.kodi.Kodi/x86_64/stable"
        if !rest.is_empty() && !rest.contains(' ') {
            let next = self.step + 1;
            self.start_op(next, self.total, Some(rest.to_string()));
        }
    }

    fn start_op(&mut self, step: u32, total: Option<u32>, current: Option<String>) {
        self.phase = InstallPhase::Working;
        self.step = step;
        self.total = total;
        self.percent = None;
        self.current = current;
    }

    /// Feed a whole captured output and the final exit status.
    pub fn from_output(out: &Output) -> Self {
        let mut p = Self::new();
        for line in out.stdout.lines().chain(out.stderr.lines()) {
            p.feed(line);
        }
        if out.success() {
            if !matches!(p.phase, InstallPhase::Failed(_)) {
                p.phase = InstallPhase::Done;
            }
        } else if !matches!(p.phase, InstallPhase::Failed(_)) {
            p.phase = InstallPhase::Failed(out.failure_reason());
        }
        p
    }
}

/// The last `NN%` token of a progress line.
fn parse_percent(s: &str) -> Option<u8> {
    s.split_whitespace()
        .rev()
        .filter_map(|t| t.strip_suffix('%'))
        .find_map(|n| n.parse::<u8>().ok().filter(|p| *p <= 100))
}

fn outcome(out: &Output, what: &str, id: &str) -> Result<InstallProgress> {
    let p = InstallProgress::from_output(out);
    match &p.phase {
        InstallPhase::Failed(msg) => Err(Error::msg(format!("Could not {what} {id}: {msg}"))),
        _ => Ok(p),
    }
}

/// Install an app from Flathub for the user (blocking; run off the UI thread).
pub fn install(r: &mut dyn Runner, app_id: &str) -> Result<InstallProgress> {
    let out = r.run(&install_cmd(app_id)?)?;
    outcome(&out, "install", app_id)
}

pub fn uninstall(r: &mut dyn Runner, app_id: &str) -> Result<()> {
    let out = r.run(&uninstall_cmd(app_id)?)?;
    outcome(&out, "remove", app_id).map(|_| ())
}

pub fn update_all(r: &mut dyn Runner) -> Result<()> {
    let out = r.run(&update_all_cmd())?;
    outcome(&out, "update", "apps").map(|_| ())
}

/// Appstream icon directories (`…/appstream/flathub/<arch>/active/icons`),
/// user installation first.
pub fn appstream_icon_dirs(home: &Path) -> Vec<PathBuf> {
    let arch = std::env::consts::ARCH;
    [home.join(USER_DIR_REL), PathBuf::from(SYSTEM_DIR)]
        .into_iter()
        .map(|base| {
            base.join("appstream")
                .join(REMOTE)
                .join(arch)
                .join("active/icons")
        })
        .collect()
}

/// Icon of a store result, from Flathub's appstream cache (populated by
/// [`ensure_flathub_user`] + [`refresh_appstream_cmd`] or a first search).
pub fn store_icon_path(home: &Path, app_id: &str) -> Option<PathBuf> {
    store_icon_path_in(&appstream_icon_dirs(home), app_id)
}

pub fn store_icon_path_in(dirs: &[PathBuf], app_id: &str) -> Option<PathBuf> {
    if !is_valid_app_id(app_id) {
        return None;
    }
    ["128x128", "64x64"].iter().find_map(|size| {
        dirs.iter()
            .map(|d| d.join(size).join(format!("{app_id}.png")))
            .find(|p| p.is_file())
    })
}

// ------------------------------------------------------------ app details

/// What the full-screen Store page shows about an app,
/// read from Flathub's appstream catalogue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppDetails {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub developer: String,
    /// Paragraphs and list items, plain text.
    pub description: Vec<String>,
    /// Screenshot image URLs (https, PNG), at most [`MAX_SCREENSHOTS`].
    pub screenshots: Vec<String>,
}

pub const MAX_SCREENSHOTS: usize = 6;

/// The appstream catalogue files to try, plain XML before gzip, user
/// installation before system.
pub fn appstream_xml_paths(home: &Path) -> Vec<PathBuf> {
    let arch = std::env::consts::ARCH;
    let mut v = Vec::new();
    for base in [home.join(USER_DIR_REL), PathBuf::from(SYSTEM_DIR)] {
        let dir = base
            .join("appstream")
            .join(REMOTE)
            .join(arch)
            .join("active");
        v.push(dir.join("appstream.xml"));
        v.push(dir.join("appstream.xml.gz"));
    }
    v
}

/// Read the catalogue (`gzip -dc` for the compressed one).
pub fn read_appstream(r: &mut dyn Runner, home: &Path) -> Result<String> {
    for p in appstream_xml_paths(home) {
        if !p.is_file() {
            continue;
        }
        if p.extension().is_some_and(|e| e == "gz") {
            let out = r.run_ok(
                &Cmd::new("gzip")
                    .arg("-dc")
                    .arg(p.to_string_lossy().as_ref())
                    .timeout(Duration::from_secs(30)),
            )?;
            return Ok(out.stdout);
        }
        return std::fs::read_to_string(&p).map_err(|e| Error::msg(e.to_string()));
    }
    Err(Error::msg(
        "Store details are not downloaded yet. Search the Store once",
    ))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Text of `xml` with tags removed and whitespace collapsed.
fn plain(xml: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in xml.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    unescape(&out.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Content of the first `<tag>` (exactly, i.e. without attributes such as
/// `xml:lang`) in `xml`.
fn first_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let a = xml.find(&open)? + open.len();
    let b = a + xml[a..].find(&close)?;
    Some(&xml[a..b])
}

/// Every `<tag …>…</tag>` in `xml` as (attributes, content).
fn all_tags<'a>(xml: &'a str, tag: &str) -> Vec<(&'a str, &'a str)> {
    let mut v = Vec::new();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        // `<p` must not match `<pre`, etc.
        if !after.starts_with(['>', ' ', '\t', '\n']) {
            rest = after;
            continue;
        }
        let Some(gt) = after.find('>') else { break };
        let attrs = &after[..gt];
        let body = &after[gt + 1..];
        let Some(end) = body.find(&close) else { break };
        v.push((attrs, &body[..end]));
        rest = &body[end + close.len()..];
    }
    v
}

fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let a = attrs.find(&key)? + key.len();
    let b = a + attrs[a..].find('"')?;
    Some(&attrs[a..b])
}

/// The `<component>` for `app_id` in an appstream catalogue.
pub fn parse_app_details(xml: &str, app_id: &str) -> Option<AppDetails> {
    let id_tag = format!("<id>{app_id}</id>");
    let at = xml.find(&id_tag)?;
    let start = xml[..at].rfind("<component")?;
    let end = at + xml[at..].find("</component>")?;
    let comp = &xml[start..end];
    let name = first_tag(comp, "name").map(plain).unwrap_or_default();
    let summary = first_tag(comp, "summary").map(plain).unwrap_or_default();
    let developer = first_tag(comp, "developer_name")
        .map(plain)
        .or_else(|| {
            let d = comp.find("<developer")?;
            first_tag(&comp[d..], "name").map(plain)
        })
        .unwrap_or_default();
    let description = first_tag(comp, "description")
        .map(|d| {
            let mut items = Vec::new();
            for (tag, prefix) in [("p", ""), ("li", "• ")] {
                for (attrs, body) in all_tags(d, tag) {
                    if attrs.contains("xml:lang") {
                        continue;
                    }
                    let t = plain(body);
                    if !t.is_empty() {
                        items.push((d.find(body).unwrap_or(0), format!("{prefix}{t}")));
                    }
                }
            }
            items.sort_by_key(|(pos, _)| *pos);
            items.into_iter().map(|(_, t)| t).collect()
        })
        .unwrap_or_default();
    let mut screenshots = Vec::new();
    if let Some(shots) = first_tag(comp, "screenshots") {
        for (_, shot) in all_tags(shots, "screenshot") {
            let images = all_tags(shot, "image");
            let usable = |url: &str| {
                url.starts_with("https://") && url.to_ascii_lowercase().ends_with(".png")
            };
            // A thumbnail about 1248 px wide looks sharp on a TV card;
            // otherwise the source image.
            let thumb = images
                .iter()
                .filter(|(a, u)| attr(a, "type") == Some("thumbnail") && usable(u.trim()))
                .min_by_key(|(a, _)| {
                    let w: i64 = attr(a, "width").and_then(|w| w.parse().ok()).unwrap_or(0);
                    (w - 1248).abs()
                })
                .map(|(_, u)| u.trim().to_string());
            let source = images
                .iter()
                .find(|(a, u)| attr(a, "type") != Some("thumbnail") && usable(u.trim()))
                .map(|(_, u)| u.trim().to_string());
            if let Some(url) = thumb.or(source) {
                screenshots.push(url);
            }
            if screenshots.len() >= MAX_SCREENSHOTS {
                break;
            }
        }
    }
    Some(AppDetails {
        id: app_id.to_string(),
        name,
        summary,
        developer,
        description,
        screenshots,
    })
}

/// Download one screenshot to `dest` (written via a temporary file).
pub fn screenshot_cmd(url: &str, tmp: &Path) -> Result<Cmd> {
    if !url.starts_with("https://") || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::msg("not an https URL"));
    }
    Ok(Cmd::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "30",
            "--max-filesize",
            "20000000",
            "-o",
        ])
        .arg(tmp.to_string_lossy().as_ref())
        .arg(url)
        .timeout(Duration::from_secs(40)))
}

/// Cache file for a screenshot URL (`~/.cache/catostv/store/<hash>.png`).
pub fn screenshot_cache_path(home: &Path, url: &str) -> PathBuf {
    // FNV-1a: a stable, dependency-free file name for the URL.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    home.join(".cache/catostv/store")
        .join(format!("{h:016x}.png"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::fake::FakeRunner;
    use crate::testutil::TempDir;

    #[test]
    fn app_id_validation() {
        for ok in [
            "tv.kodi.Kodi",
            "com.github.iwalton3.jellyfin-media-player",
            "com.moonlight_stream.Moonlight",
            "org.DolphinEmu.dolphin-emu",
        ] {
            assert!(is_valid_app_id(ok), "{ok}");
        }
        for bad in [
            "",
            "kodi",
            "tv.kodi",
            "tv..Kodi",
            "-tv.kodi.Kodi",
            "tv.kodi.-Kodi",
            "tv.kodi.Kodi;rm",
            "tv.kodi.Ko di",
            "tv/kodi/Kodi",
            "tv.1kodi.Kodi",
            "tv.kodi.Kodi\n",
        ] {
            assert!(!is_valid_app_id(bad), "{bad:?}");
        }
        assert!(!is_valid_app_id(&format!("a.b.{}", "c".repeat(300))));
    }

    #[test]
    fn curated_catalogue_is_well_formed() {
        let mut ids = std::collections::HashSet::new();
        for a in CURATED {
            assert!(is_valid_app_id(a.id), "{}", a.id);
            assert!(CATEGORIES.contains(&a.category), "{}", a.id);
            assert!(!a.name.is_empty() && !a.blurb.is_empty());
            assert!(ids.insert(a.id), "duplicate {}", a.id);
        }
        assert!(CURATED.len() >= 20);
        for cat in CATEGORIES {
            assert!(!curated_in(cat).is_empty(), "{cat}");
        }
    }

    // Realistic non-tty output of
    // `flatpak search --columns=application,name,description,version,remotes kodi`.
    const SEARCH_OUT: &str = "tv.kodi.Kodi\tKodi\tUltimate entertainment center\t21.2\tflathub
tv.kodi.Kodi.Locale\tKodi Locale\tTranslations\t\tflathub
com.obsproject.Studio.Plugin.Kodi\tKodi plugin\tSomething\t1.0\tflathub
io.github.example.Remote\tKore Remote\tControl Kodi from here\t\tflathub,other
io.github.example.Remote\tKore Remote\tControl Kodi from here\t\tthird
not-an-id\tBad\tskip me\t1\tflathub
";

    #[test]
    fn parses_search_output() {
        let apps = parse_search(SEARCH_OUT);
        assert_eq!(apps.len(), 2, "{apps:#?}");
        assert_eq!(apps[0].id, "tv.kodi.Kodi");
        assert_eq!(apps[0].name, "Kodi");
        assert_eq!(apps[0].description, "Ultimate entertainment center");
        assert_eq!(apps[0].version, "21.2");
        assert_eq!(apps[0].remotes, ["flathub"]);
        assert_eq!(apps[1].version, "");
        assert_eq!(apps[1].remotes, ["flathub", "other", "third"]);
        assert!(parse_search("No matches found\n").is_empty());
        assert!(parse_search("").is_empty());
        // Trailing empty cells cut off by flatpak's g_strchomp.
        let short = parse_search("org.example.App\tApp\n");
        assert_eq!(short[0].description, "");
        assert!(short[0].remotes.is_empty());
    }

    #[test]
    fn search_builds_safe_command_and_orders_flathub_first() {
        let mut r = FakeRunner::new();
        r.push(Output::ok(
            "org.other.App\tOther\tx\t1\tother\ntv.kodi.Kodi\tKodi\ty\t2\tflathub\n",
        ));
        let apps = search(&mut r, "  --kodi\n").unwrap();
        assert_eq!(apps[0].id, "tv.kodi.Kodi");
        assert_eq!(
            r.seen[0].args,
            [
                "search",
                "--columns=application,name,description,version,remotes",
                "kodi"
            ]
        );
        assert_eq!(r.seen[0].timeout, SEARCH_TIMEOUT);
        assert!(search(&mut r, " -- ").is_err());
        assert_eq!(r.seen.len(), 1, "empty query must not run flatpak");
        assert_eq!(clean_query(&"x".repeat(100)).unwrap().len(), 64);
    }

    #[test]
    fn parses_list_output() {
        let text = "tv.kodi.Kodi\tKodi\t21.2\tuser
com.spotify.Client\tSpotify\t1.2.50\tsystem
tv.kodi.Kodi\tKodi\t21.2\tuser
org.example.NoVersion\tNoVersion\t\tuser
";
        let apps = parse_list(text);
        assert_eq!(apps.len(), 3);
        assert!(apps[0].is_user());
        assert_eq!(apps[1].installation, "system");
        assert_eq!(apps[2].version, "");
        let mut r = FakeRunner::new();
        r.push(Output::ok(text));
        assert_eq!(installed(&mut r).unwrap().len(), 3);
        assert_eq!(
            r.seen[0].args,
            [
                "list",
                "--app",
                "--columns=application,name,version,installation"
            ]
        );
    }

    #[test]
    fn command_construction() {
        assert_eq!(
            remote_add_cmd().display(),
            "flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo"
        );
        let c = install_cmd("tv.kodi.Kodi").unwrap();
        assert_eq!(
            c.display(),
            "flatpak install --user --noninteractive -y flathub tv.kodi.Kodi"
        );
        assert_eq!(c.timeout, INSTALL_TIMEOUT);
        assert_eq!(
            install_progress_cmd("tv.kodi.Kodi").unwrap().display(),
            "flatpak install --user -y flathub tv.kodi.Kodi"
        );
        assert_eq!(
            uninstall_cmd("tv.kodi.Kodi").unwrap().display(),
            "flatpak uninstall --user --noninteractive -y tv.kodi.Kodi"
        );
        assert_eq!(
            update_all_cmd().display(),
            "flatpak update --user --noninteractive -y"
        );
        assert_eq!(
            refresh_appstream_cmd().display(),
            "flatpak update --user --appstream flathub"
        );
        assert!(install_cmd("--help").is_err());
        assert!(install_cmd("tv.kodi.Kodi --system").is_err());
        assert!(uninstall_cmd("x").is_err());
    }

    #[test]
    fn progress_quiet_mode() {
        let mut p = InstallProgress::new();
        assert_eq!(p.fraction(), Some(0.0));
        p.feed("Installing runtime/org.freedesktop.Platform.GL.default/x86_64/24.08");
        assert_eq!(p.phase, InstallPhase::Working);
        assert_eq!(p.step, 1);
        p.feed("Installing runtime/org.freedesktop.Platform/x86_64/24.08");
        p.feed("Installing app/tv.kodi.Kodi/x86_64/stable");
        assert_eq!(p.step, 3);
        assert_eq!(p.current.as_deref(), Some("app/tv.kodi.Kodi/x86_64/stable"));
        assert_eq!(p.fraction(), None, "total unknown in quiet mode");
        p.feed("Error: Failed to install org.freedesktop.Platform: Not enough disk space to complete this operation");
        assert_eq!(
            p.phase,
            InstallPhase::Failed(
                "Failed to install org.freedesktop.Platform: Not enough disk space to complete this operation".into()
            )
        );
        // Failure is sticky.
        p.feed("Installing app/x/y/z");
        assert!(matches!(p.phase, InstallPhase::Failed(_)));
    }

    #[test]
    fn progress_plain_mode_with_percentages() {
        let mut p = InstallProgress::new();
        p.feed("Installing 1/2…");
        assert_eq!((p.step, p.total, p.percent), (1, Some(2), None));
        p.feed("Installing 1/2… ████████▍            45%  1.2 MB/s  00:12");
        assert_eq!(p.percent, Some(45));
        assert!((p.fraction().unwrap() - 0.225).abs() < 1e-4);
        p.feed("Installing 1/2… ████████████████████ 100%  2.0 MB/s  00:00");
        p.feed("Installing 2/2… ▍                      3%  800.1 kB/s  01:40");
        assert_eq!((p.step, p.percent), (2, Some(3)));
        p.feed("Installation complete.");
        assert_eq!(p.phase, InstallPhase::Done);
        assert_eq!(p.fraction(), Some(1.0));

        let mut single = InstallProgress::new();
        single.feed("Installing…");
        single.feed("\rInstalling… ███        30%  1.0 MB/s  00:10");
        assert_eq!(
            (single.step, single.total, single.percent),
            (1, Some(1), Some(30))
        );
        // The operation table printed before the transaction is ignored.
        let mut t = InstallProgress::new();
        t.feed(" 1.\ttv.kodi.Kodi\tstable\ti\tflathub\t< 80.6 MB");
        assert_eq!(t.phase, InstallPhase::Starting);
    }

    #[test]
    fn install_outcomes() {
        let mut r = FakeRunner::new();
        r.push(Output::ok("Installing app/tv.kodi.Kodi/x86_64/stable\n"));
        let p = install(&mut r, "tv.kodi.Kodi").unwrap();
        assert_eq!(p.phase, InstallPhase::Done);

        // Already installed: flatpak prints "Skipping:" on stderr, exit 0.
        r.push(Output {
            code: Some(0),
            stderr: "Skipping: tv.kodi.Kodi/x86_64/stable is already installed\n".into(),
            ..Output::default()
        });
        assert!(install(&mut r, "tv.kodi.Kodi").unwrap().already_installed);

        r.push(Output::failed(
            1,
            "error: Nothing matches org.no.Such in remote flathub\n",
        ));
        let e = install(&mut r, "org.no.Such").unwrap_err();
        assert_eq!(
            e.message(),
            "Could not install org.no.Such: Nothing matches org.no.Such in remote flathub"
        );

        r.push(Output {
            timed_out: true,
            ..Output::default()
        });
        assert!(
            install(&mut r, "tv.kodi.Kodi")
                .unwrap_err()
                .message()
                .contains("timed out")
        );

        r.push(Output::failed(
            1,
            "error: tv.kodi.Kodi/*unspecified*/* not installed\n",
        ));
        assert!(uninstall(&mut r, "tv.kodi.Kodi").is_err());
        r.push(Output::ok("Updating app/tv.kodi.Kodi/x86_64/stable\n"));
        update_all(&mut r).unwrap();
        assert_eq!(r.seen.len(), 6);
    }

    #[test]
    fn ensure_remote_reports_errors() {
        let mut r = FakeRunner::new();
        ensure_flathub_user(&mut r).unwrap();
        r.push(Output::failed(
            1,
            "error: Can't load uri https://dl.flathub.org/repo/flathub.flatpakrepo",
        ));
        let e = ensure_flathub_user(&mut r).unwrap_err();
        assert!(
            e.message()
                .starts_with("Could not add Flathub: flatpak failed")
        );
    }

    #[test]
    fn store_icons() {
        let t = TempDir::new("store-icons");
        let dirs = vec![t.path().join("user"), t.path().join("system")];
        assert_eq!(store_icon_path_in(&dirs, "tv.kodi.Kodi"), None);
        let sys64 = t.write("system/64x64/tv.kodi.Kodi.png", "png");
        assert_eq!(store_icon_path_in(&dirs, "tv.kodi.Kodi"), Some(sys64));
        let user128 = t.write("user/128x128/tv.kodi.Kodi.png", "png");
        assert_eq!(store_icon_path_in(&dirs, "tv.kodi.Kodi"), Some(user128));
        assert_eq!(store_icon_path_in(&dirs, "../../etc/passwd"), None);
        let home = Path::new("/home/catostv");
        let d = appstream_icon_dirs(home);
        assert_eq!(
            d[0],
            PathBuf::from(format!(
                "/home/catostv/.local/share/flatpak/appstream/flathub/{}/active/icons",
                std::env::consts::ARCH
            ))
        );
        assert!(d[1].starts_with("/var/lib/flatpak/appstream/flathub"));
    }

    const APPSTREAM: &str = r#"<?xml version="1.0"?>
<components origin="flathub">
  <component type="desktop-application">
    <id>org.other.App</id><name>Other</name>
  </component>
  <component type="desktop-application">
    <id>tv.kodi.Kodi</id>
    <name>Kodi</name>
    <name xml:lang="de">Kodi DE</name>
    <summary>Ultimate entertainment center</summary>
    <developer_name>Team Kodi</developer_name>
    <description>
      <p>Kodi &amp; friends play <em>everything</em>.</p>
      <p xml:lang="de">Alles abspielen.</p>
      <ul><li>Movies</li><li>Music</li></ul>
      <pre>not a paragraph</pre>
    </description>
    <screenshots>
      <screenshot type="default">
        <image type="source" width="1920" height="1080">https://dl.flathub.org/media/kodi/a.png</image>
        <image type="thumbnail" width="624" height="351">https://dl.flathub.org/media/kodi/a-624.png</image>
        <image type="thumbnail" width="1248" height="702">https://dl.flathub.org/media/kodi/a-1248.png</image>
      </screenshot>
      <screenshot>
        <image type="source">https://example.org/b.jpg</image>
      </screenshot>
      <screenshot>
        <image type="source">http://insecure.example/c.png</image>
      </screenshot>
    </screenshots>
  </component>
</components>"#;

    #[test]
    fn app_details_from_appstream() {
        let d = parse_app_details(APPSTREAM, "tv.kodi.Kodi").unwrap();
        assert_eq!(d.name, "Kodi");
        assert_eq!(d.summary, "Ultimate entertainment center");
        assert_eq!(d.developer, "Team Kodi");
        assert_eq!(
            d.description,
            ["Kodi & friends play everything.", "• Movies", "• Music"]
        );
        assert_eq!(
            d.screenshots,
            ["https://dl.flathub.org/media/kodi/a-1248.png"]
        );
        assert!(parse_app_details(APPSTREAM, "missing.App").is_none());
        let other = parse_app_details(APPSTREAM, "org.other.App").unwrap();
        assert_eq!(other.name, "Other");
        assert!(other.screenshots.is_empty());
    }

    #[test]
    fn screenshot_download_is_https_only() {
        let tmp = Path::new("/tmp/x.png");
        assert_eq!(
            screenshot_cmd("https://dl.flathub.org/a.png", tmp)
                .unwrap()
                .display(),
            "curl -fsSL --max-time 30 --max-filesize 20000000 -o /tmp/x.png https://dl.flathub.org/a.png"
        );
        assert!(screenshot_cmd("http://x/a.png", tmp).is_err());
        assert!(screenshot_cmd("https://x/a b.png", tmp).is_err());
        let a = screenshot_cache_path(Path::new("/h"), "https://a/1.png");
        assert_ne!(a, screenshot_cache_path(Path::new("/h"), "https://a/2.png"));
        assert!(a.starts_with("/h/.cache/catostv/store"));
    }
}
