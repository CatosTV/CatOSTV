//! Catfyn, CatosTV's own Jellyfin client: server
//! discovery, sign-in, several accounts (each may be "incognito": kept off
//! CatosTV Home), library browsing, playback in mpv and watch-progress
//! reporting.
//!
//! Everything here is pure or builds `curl` / `mpv` command lines, so it is
//! unit-tested without a server. Network access happens in the shell's
//! background jobs. API facts (Jellyfin 10.9/10.10 REST API):
//!
//! | What | Request |
//! |---|---|
//! | discovery | UDP broadcast `who is JellyfinServer?` to port 7359 → JSON `{Address, Id, Name}` |
//! | sign in | `POST /Users/AuthenticateByName {"Username","Pw"}` → `{User{Id,Name}, AccessToken}` |
//! | libraries | `GET /Users/{u}/Views` |
//! | continue watching | `GET /Users/{u}/Items/Resume` |
//! | next up | `GET /Shows/NextUp?UserId={u}` |
//! | latest | `GET /Users/{u}/Items/Latest?ParentId={lib}` (bare array) |
//! | a library | `GET /Users/{u}/Items?ParentId=…&Recursive=true&IncludeItemTypes=Movie,Series` |
//! | seasons / episodes | `GET /Shows/{s}/Seasons`, `GET /Shows/{s}/Episodes?SeasonId=…` |
//! | images | `GET /Items/{id}/Images/{Primary|Backdrop/0}?maxWidth=…&format=Png` (no auth) |
//! | play | `GET /Videos/{id}/stream?static=true` (direct play, any codec mpv can decode: 4K, HDR) |
//! | progress | `POST /Sessions/Playing`, `/Sessions/Playing/Progress`, `/Sessions/Playing/Stopped` |
//!
//! The access token never appears on a command line: requests read their
//! headers from a 0600 file in `$XDG_RUNTIME_DIR` (`curl -H @file`), and
//! mpv gets the stream through the same header file (`--http-header-fields`
//! is avoided for the same reason; mpv reads `--include` config).

use crate::error::{Error, Result};
use crate::mpv::{Json, json_string, parse_json};
use crate::process::Cmd;
use std::net::{Ipv4Addr, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const CLIENT: &str = "Catfyn";
pub const DEVICE: &str = "CatosTV";
pub const DISCOVERY_PORT: u16 = 7359;
pub const DISCOVERY_MESSAGE: &str = "who is JellyfinServer?";
/// Ticks per second (Jellyfin positions are 100 ns ticks).
pub const TICKS: f64 = 10_000_000.0;
const HTTP_TIMEOUT: Duration = Duration::from_secs(25);

// ------------------------------------------------------------------ servers

/// A server that answered discovery (or was typed in).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub id: String,
    pub name: String,
    /// Base URL, e.g. `http://192.168.1.20:8096` (no trailing slash).
    pub address: String,
}

/// Parse one discovery reply.
pub fn parse_discovery(text: &str) -> Option<Server> {
    let v = parse_json(text.trim())?;
    let address = normalize_address(v.get("Address")?.as_str()?)?;
    Some(Server {
        id: v.get("Id").and_then(Json::as_str).unwrap_or("").to_string(),
        name: v
            .get("Name")
            .and_then(Json::as_str)
            .unwrap_or("Jellyfin")
            .to_string(),
        address,
    })
}

/// Broadcast a discovery request and collect answers for `wait`.
pub fn discover(wait: Duration) -> Vec<Server> {
    let mut out: Vec<Server> = Vec::new();
    let Ok(sock) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
        return out;
    };
    let _ = sock.set_broadcast(true);
    let _ = sock.send_to(
        DISCOVERY_MESSAGE.as_bytes(),
        (Ipv4Addr::BROADCAST, DISCOVERY_PORT),
    );
    let deadline = Instant::now() + wait;
    let mut buf = [0u8; 2048];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if left.is_zero() || sock.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Some(s) = parse_discovery(&String::from_utf8_lossy(&buf[..n]))
                    && !out.iter().any(|o| o.address == s.address)
                {
                    out.push(s);
                }
            }
            Err(_) => break,
        }
    }
    if out.is_empty() {
        out = scan(Duration::from_millis(300));
    }
    out
}

/// Servers that did not answer discovery (often one in a container without
/// its discovery port): hosts of this network with Jellyfin's port open
/// that say who they are at `/System/Info/Public`.
fn scan(timeout: Duration) -> Vec<Server> {
    crate::http::scan_lan(8096, timeout)
        .into_iter()
        .filter_map(|ip| {
            let addr = std::net::SocketAddrV4::new(ip, 8096);
            let body = crate::http::get_small(
                addr,
                "/System/Info/Public",
                &[],
                Duration::from_secs(2),
                16 * 1024,
            )
            .ok()?;
            parse_public_info(&body, &format!("http://{ip}:8096"))
        })
        .collect()
}

/// `/System/Info/Public` → the server (name and id), at `address`.
pub fn parse_public_info(text: &str, address: &str) -> Option<Server> {
    let v = parse_json(text.trim())?;
    let name = v.get("ServerName")?.as_str()?.to_string();
    Some(Server {
        id: v.get("Id").and_then(Json::as_str).unwrap_or("").to_string(),
        name,
        address: address.to_string(),
    })
}

/// `192.168.1.5`, `jelly.lan:8096/`, `https://x` → a base URL, or `None`
/// for anything that is not a plain http(s) host[:port][/path].
pub fn normalize_address(input: &str) -> Option<String> {
    let s = input.trim().trim_end_matches('/');
    if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let with_scheme = if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else if s.contains("://") || s.split('/').next().is_some_and(|h| h.ends_with(':')) {
        // Another scheme, or "host:" without a port.
        return None;
    } else {
        // Jellyfin's default port when none is given.
        let host_has_port = s.split('/').next().is_some_and(|h| h.contains(':'));
        if host_has_port {
            format!("http://{s}")
        } else {
            let (host, path) = s.split_once('/').unwrap_or((s, ""));
            if path.is_empty() {
                format!("http://{host}:8096")
            } else {
                format!("http://{host}:8096/{path}")
            }
        }
    };
    crate::mpv::is_playable_url(&with_scheme).then_some(with_scheme)
}

// ----------------------------------------------------------------- accounts

/// The kind of media server an account (and an item) belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Backend {
    #[default]
    Jellyfin,
    Plex,
}

impl Backend {
    pub const ALL: [Backend; 2] = [Backend::Jellyfin, Backend::Plex];

    pub fn key(self) -> &'static str {
        match self {
            Self::Jellyfin => "jellyfin",
            Self::Plex => "plex",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Jellyfin => "Jellyfin",
            Self::Plex => "Plex",
        }
    }

    pub fn from_key(s: &str) -> Self {
        if s == "plex" {
            Self::Plex
        } else {
            Self::Jellyfin
        }
    }
}

/// A signed-in user on a server.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Account {
    pub backend: Backend,
    pub server: String,
    pub server_name: String,
    pub user_id: String,
    pub user_name: String,
    /// Jellyfin: the access token. Plex: the server's own token (empty for
    /// a server on the home network that lets everyone in).
    pub token: String,
    /// Kept off CatosTV Home (no Continue watching / Next up rows there).
    pub incognito: bool,
    /// Plex: the plex.tv sign-in, to find the server again when its
    /// address changes, and the server's id.
    pub cloud_token: String,
    pub server_id: String,
}

/// All Catfyn accounts on this TV (`~/.config/catostv/catfyn.conf`, 0600).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Accounts {
    pub device_id: String,
    pub list: Vec<Account>,
    /// The account Catfyn opens with.
    pub active: usize,
}

impl Accounts {
    pub fn active(&self) -> Option<&Account> {
        self.list.get(self.active)
    }

    /// The account whose shows appear on CatosTV Home: the active one if it
    /// is not incognito, else the first one that is not.
    pub fn home(&self) -> Option<&Account> {
        self.active()
            .filter(|a| !a.incognito)
            .or_else(|| self.list.iter().find(|a| !a.incognito))
    }

    /// The accounts whose shows appear on CatosTV Home: one per kind of
    /// server (Jellyfin and Plex side by side), the active one when it is of
    /// that kind, else the first; never an incognito one.
    pub fn home_accounts(&self) -> Vec<&Account> {
        Backend::ALL
            .iter()
            .filter_map(|&b| {
                self.active()
                    .filter(|a| a.backend == b && !a.incognito)
                    .or_else(|| self.list.iter().find(|a| a.backend == b && !a.incognito))
            })
            .collect()
    }

    /// The account an item was loaded from (its server and kind).
    pub fn for_item(&self, item: &Item) -> Option<&Account> {
        let mine = |a: &&Account| a.server == item.server && a.backend == item.backend;
        self.home_accounts()
            .into_iter()
            .find(mine)
            .or_else(|| self.list.iter().find(mine))
    }

    /// Kinds of server that have an account (for the Jellyfin | Plex
    /// switch).
    pub fn backends(&self) -> Vec<Backend> {
        Backend::ALL
            .into_iter()
            .filter(|&b| self.list.iter().any(|a| a.backend == b))
            .collect()
    }

    /// Add (or replace the same user on the same server) and make it active.
    pub fn add(&mut self, a: Account) {
        if let Some(i) = self
            .list
            .iter()
            .position(|x| x.backend == a.backend && x.server == a.server && x.user_id == a.user_id)
        {
            let incognito = self.list[i].incognito;
            self.list[i] = Account { incognito, ..a };
            self.active = i;
        } else {
            self.list.push(a);
            self.active = self.list.len() - 1;
        }
    }

    pub fn remove(&mut self, i: usize) {
        if i < self.list.len() {
            self.list.remove(i);
        }
        self.active = self.active.min(self.list.len().saturating_sub(1));
    }

    pub fn from_conf(text: &str) -> Self {
        let mut acc = Accounts::default();
        let mut cur: Option<Account> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "[account]" {
                if let Some(a) = cur.take().filter(has_sign_in) {
                    acc.list.push(a);
                }
                cur = Some(Account::default());
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim().to_string());
            match (&mut cur, k) {
                (None, "device_id") if is_token_like(&v) => acc.device_id = v,
                (None, "active") => acc.active = v.parse().unwrap_or(0),
                (Some(a), "server") => a.server = normalize_address(&v).unwrap_or_default(),
                (Some(a), "server_name") => a.server_name = v,
                (Some(a), "user_id") if is_token_like(&v) => a.user_id = v,
                (Some(a), "user_name") => a.user_name = v,
                (Some(a), "token") if is_token_like(&v) => a.token = v,
                (Some(a), "incognito") => a.incognito = v == "true",
                (Some(a), "backend") => a.backend = Backend::from_key(&v),
                (Some(a), "cloud_token") if is_token_like(&v) => a.cloud_token = v,
                (Some(a), "server_id") if is_token_like(&v) => a.server_id = v,
                _ => {}
            }
        }
        if let Some(a) = cur.filter(has_sign_in) {
            acc.list.push(a);
        }
        acc.list
            .retain(|a| !a.server.is_empty() && !a.user_id.is_empty());
        acc.active = acc.active.min(acc.list.len().saturating_sub(1));
        acc
    }

    pub fn to_conf(&self) -> String {
        let clean = |s: &str| s.chars().filter(|c| !c.is_control()).collect::<String>();
        let mut s = format!(
            "# Catfyn accounts (written by CatosTV; contains sign-in tokens)\ndevice_id = {}\nactive = {}\n",
            self.device_id, self.active
        );
        for a in &self.list {
            s.push_str(&format!(
                "\n[account]\nbackend = {}\nserver = {}\nserver_name = {}\nuser_id = {}\nuser_name = {}\ntoken = {}\nincognito = {}\n",
                a.backend.key(),
                clean(&a.server),
                clean(&a.server_name),
                clean(&a.user_id),
                clean(&a.user_name),
                clean(&a.token),
                a.incognito
            ));
            if !a.cloud_token.is_empty() {
                s.push_str(&format!("cloud_token = {}\n", clean(&a.cloud_token)));
            }
            if !a.server_id.is_empty() {
                s.push_str(&format!("server_id = {}\n", clean(&a.server_id)));
            }
        }
        s
    }
}

/// A Jellyfin account needs its token; a Plex one may have none (a
/// server on the home network that lets everyone in).
fn has_sign_in(a: &Account) -> bool {
    !a.token.is_empty() || a.backend == Backend::Plex
}

/// Ids and tokens are hex / GUID-like (Plex tokens also use `_`).
pub fn is_token_like(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn accounts_path(home: &Path) -> PathBuf {
    home.join(".config/catostv/catfyn.conf")
}

pub fn load_accounts(home: &Path) -> Accounts {
    let mut a = std::fs::read_to_string(accounts_path(home))
        .map(|t| Accounts::from_conf(&t))
        .unwrap_or_default();
    if a.device_id.is_empty() {
        a.device_id = crate::http::random_hex(16).unwrap_or_else(|_| "catostv".into());
    }
    a
}

/// Save with mode 0600 (tokens), atomically.
pub fn save_accounts(home: &Path, a: &Accounts) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = accounts_path(home);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("conf.tmp");
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(a.to_conf().as_bytes())?;
    }
    std::fs::rename(tmp, path)
}

// ------------------------------------------------------------------ requests

/// The `Authorization` header value (`Token` only once signed in).
pub fn auth_value(device_id: &str, token: Option<&str>) -> String {
    let mut s = format!(
        "MediaBrowser Client=\"{CLIENT}\", Device=\"{DEVICE}\", DeviceId=\"{device_id}\", Version=\"{}\"",
        crate::env::version()
    );
    if let Some(t) = token {
        s.push_str(&format!(", Token=\"{t}\""));
    }
    s
}

/// Write the request headers to a private file (0600) that `curl -H @file`
/// reads, so the token is never on a command line. Returns its path.
pub fn header_file(device_id: &str, token: Option<&str>) -> Result<PathBuf> {
    write_headers(
        if token.is_some() { "auth" } else { "anon" },
        &format!(
            "Authorization: {}\nAccept: application/json\nContent-Type: application/json\n",
            auth_value(device_id, token)
        ),
    )
}

/// Header lines in `$XDG_RUNTIME_DIR/catfyn-headers-{tag}` (0600).
pub fn write_headers(tag: &str, text: &str) -> Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = crate::env::runtime_dir();
    let path = dir.join(format!("catfyn-headers-{tag}"));
    // A temporary name of its own per call: requests run in parallel
    // (Browse rows, CatosTV Home's rows, the player), and a shared one let
    // one call delete another's half-written file, "No such file or
    // directory" on Catfyn's Home while scrolling. The rename is atomic, so curl always reads a whole file.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp-{}-{n}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    drop(f);
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// `curl` for one API call. `body` makes it a POST.
pub fn request_cmd(url: &str, headers: &Path, body: Option<&str>) -> Cmd {
    let mut args: Vec<String> = vec![
        "-sS".into(),
        "--fail-with-body".into(),
        "-m".into(),
        HTTP_TIMEOUT.as_secs().to_string(),
        "-H".into(),
        format!("@{}", headers.display()),
    ];
    if let Some(b) = body {
        args.push("--data-binary".into());
        args.push(b.into());
    }
    args.push("--".into());
    args.push(url.into());
    Cmd::new("curl")
        .args(args)
        .timeout(HTTP_TIMEOUT + Duration::from_secs(5))
}

/// Percent-encode a query value.
fn q(s: &str) -> String {
    crate::http::percent_encode(s)
}

pub fn login_body(user: &str, password: &str) -> String {
    format!(
        "{{\"Username\":{},\"Pw\":{}}}",
        json_string(user),
        json_string(password)
    )
}

pub fn login_url(server: &str) -> String {
    format!("{server}/Users/AuthenticateByName")
}

/// `AuthenticateByName` reply → (user id, user name, token).
pub fn parse_login(text: &str) -> Option<(String, String, String)> {
    let v = parse_json(text.trim())?;
    let token = v.get("AccessToken")?.as_str()?.to_string();
    let user = v.get("User")?;
    let id = user.get("Id")?.as_str()?.to_string();
    let name = user
        .get("Name")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();
    (is_token_like(&token) && is_token_like(&id)).then_some((id, name, token))
}

/// A server's error reply in a sentence for the TV.
pub fn friendly_error(status_text: &str) -> String {
    let t = status_text.to_ascii_lowercase();
    if t.contains("401") || t.contains("unauthor") || t.contains("invalid user") {
        "Wrong user name or password".into()
    } else if t.contains("could not resolve")
        || t.contains("couldn't connect")
        || t.contains("failed to connect")
    {
        "Can't reach the server".into()
    } else if t.contains("timed out") || t.contains("timeout") {
        "The server took too long to answer".into()
    } else {
        "The server did not accept that".into()
    }
}

const ITEM_FIELDS: &str =
    "Overview,PrimaryImageAspectRatio,ProductionYear,CommunityRating,OfficialRating";

pub fn views_url(a: &Account) -> String {
    format!("{}/Users/{}/Views", a.server, a.user_id)
}

pub fn resume_url(a: &Account, limit: usize) -> String {
    format!(
        "{}/Users/{}/Items/Resume?Limit={limit}&MediaTypes=Video&Fields={ITEM_FIELDS}&EnableImageTypes=Primary,Backdrop,Thumb",
        a.server, a.user_id
    )
}

pub fn next_up_url(a: &Account, limit: usize) -> String {
    format!(
        "{}/Shows/NextUp?UserId={}&Limit={limit}&Fields={ITEM_FIELDS}",
        a.server, a.user_id
    )
}

pub fn latest_url(a: &Account, library: &str, limit: usize) -> String {
    format!(
        "{}/Users/{}/Items/Latest?ParentId={}&Limit={limit}&Fields={ITEM_FIELDS}",
        a.server,
        a.user_id,
        q(library)
    )
}

pub fn library_url(a: &Account, library: &str) -> String {
    format!(
        "{}/Users/{}/Items?ParentId={}&Recursive=true&IncludeItemTypes=Movie,Series,Video,MusicVideo&SortBy=SortName&SortOrder=Ascending&Fields={ITEM_FIELDS}&Limit=600",
        a.server,
        a.user_id,
        q(library)
    )
}

/// Movies and shows whose name matches `term` (CatosTV Search).
pub fn search_url(a: &Account, term: &str, limit: u32) -> String {
    format!(
        "{}/Users/{}/Items?searchTerm={}&Recursive=true&IncludeItemTypes=Movie,Series&Fields={ITEM_FIELDS}&Limit={limit}",
        a.server,
        a.user_id,
        q(term)
    )
}

pub fn seasons_url(a: &Account, series: &str) -> String {
    format!(
        "{}/Shows/{}/Seasons?UserId={}",
        a.server,
        q(series),
        a.user_id
    )
}

pub fn episodes_url(a: &Account, series: &str, season: &str) -> String {
    format!(
        "{}/Shows/{}/Episodes?UserId={}&SeasonId={}&Fields={ITEM_FIELDS}",
        a.server,
        q(series),
        a.user_id,
        q(season)
    )
}

/// Picture of an item. `kind` is `Primary` (poster / episode still) or
/// `Backdrop`. PNG because CatosTV decodes PNG only.
pub fn image_url(server: &str, item_id: &str, kind: &str, max_width: u32) -> String {
    let kind = if kind == "Backdrop" {
        "Backdrop/0"
    } else {
        "Primary"
    };
    format!(
        "{server}/Items/{}/Images/{kind}?maxWidth={max_width}&quality=85&format=Png",
        q(item_id)
    )
}

/// An item's poster (an episode's series poster on Plex), from whichever
/// server it came from.
pub fn poster_url(it: &Item) -> String {
    if crate::cattube::is_cattube(it) {
        return it.thumb.clone();
    }
    match it.backend {
        Backend::Plex => {
            let path = if it.kind() == Kind::Episode && !it.series_thumb.is_empty() {
                &it.series_thumb
            } else {
                &it.thumb
            };
            crate::plex::image_url(&it.server, path, 2160, 3240, 0)
        }
        Backend::Jellyfin => image_url(&it.server, &it.id, "Primary", 2160),
    }
}

/// A landscape picture: the episode still, else the backdrop.
pub fn wide_url(it: &Item) -> String {
    if crate::cattube::is_cattube(it) {
        return it.thumb.clone();
    }
    match it.backend {
        Backend::Plex => {
            let path = if it.kind() == Kind::Episode || it.art.is_empty() {
                &it.thumb
            } else {
                &it.art
            };
            crate::plex::image_url(&it.server, path, 3840, 2160, 0)
        }
        Backend::Jellyfin => match (it.kind(), &it.backdrop_item) {
            (Kind::Episode, _) if it.has_primary => image_url(&it.server, &it.id, "Primary", 3840),
            (_, Some(b)) => image_url(&it.server, b, "Backdrop", 3840),
            _ => image_url(&it.server, &it.id, "Primary", 3840),
        },
    }
}

/// The softened backdrop behind a page, when the item has one.
pub fn backdrop_url(it: &Item, blur: u32) -> Option<String> {
    // YouTube pictures get a local blur (`#blur`: the image fetcher
    // softens its copy).
    if crate::cattube::is_cattube(it) {
        return (!it.thumb.is_empty()).then(|| format!("{}#blur", it.thumb));
    }
    match it.backend {
        // No backdrop art: the item's own picture, softened a little more.
        Backend::Plex => match (it.art.is_empty(), it.thumb.is_empty()) {
            (false, _) => Some(crate::plex::image_url(
                &it.server, &it.art, 3840, 2160, blur,
            )),
            (true, false) => Some(crate::plex::image_url(
                &it.server,
                &it.thumb,
                3840,
                2160,
                blur * 2,
            )),
            _ => None,
        },
        Backend::Jellyfin => it
            .backdrop_item
            .as_ref()
            .map(|b| image_url_blurred(&it.server, b, "Backdrop", 3840, blur)),
    }
}

/// The still or poster for the "Next episode" card.
pub fn still_url(it: &Item) -> String {
    match it.backend {
        Backend::Plex => crate::plex::image_url(&it.server, &it.thumb, 3840, 2160, 0),
        Backend::Jellyfin => image_url(&it.server, &it.id, "Primary", 3840),
    }
}

/// [`image_url`] with the server's own Gaussian blur (`blur` = sigma in
/// pixels of the scaled image): soft backdrops for the TV screens.
pub fn image_url_blurred(
    server: &str,
    item_id: &str,
    kind: &str,
    max_width: u32,
    blur: u32,
) -> String {
    format!(
        "{}&blur={blur}",
        image_url(server, item_id, kind, max_width)
    )
}

/// `POST /Items/{id}/PlaybackInfo`: the media sources with their streams
/// used for subtitles delivered as separate files.
pub fn playback_info_url(a: &Account, item_id: &str) -> String {
    format!(
        "{}/Items/{}/PlaybackInfo?UserId={}",
        a.server,
        q(item_id),
        q(&a.user_id)
    )
}

pub fn playback_info_body(a: &Account) -> String {
    format!("{{\"UserId\":{}}}", json_string(&a.user_id))
}

/// A subtitle track the server serves as its own file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSub {
    pub url: String,
    pub title: String,
    pub language: String,
}

/// External subtitle tracks of the first media source: Jellyfin delivers
/// them at `DeliveryUrl` (relative to the server); embedded tracks need
/// nothing (mpv reads them from the stream).
pub fn parse_external_subs(a: &Account, body: &str) -> Vec<ExternalSub> {
    let Some(j) = crate::mpv::parse_json(body) else {
        return vec![];
    };
    let Some(sources) = j.get("MediaSources").and_then(|m| m.as_arr()) else {
        return vec![];
    };
    let mut v = Vec::new();
    for src in sources.iter().take(1) {
        let Some(streams) = src.get("MediaStreams").and_then(|m| m.as_arr()) else {
            continue;
        };
        for st in streams {
            let is_sub = st.get("Type").and_then(|t| t.as_str()) == Some("Subtitle");
            let external = st
                .get("DeliveryMethod")
                .and_then(|t| t.as_str())
                .is_some_and(|d| d == "External")
                || st.get("IsExternal").and_then(|b| b.as_bool()) == Some(true);
            let Some(path) = st.get("DeliveryUrl").and_then(|t| t.as_str()) else {
                continue;
            };
            if !is_sub || !external || !path.starts_with('/') || path.contains("..") {
                continue;
            }
            v.push(ExternalSub {
                url: format!("{}{path}", a.server),
                title: st
                    .get("DisplayTitle")
                    .and_then(|t| t.as_str())
                    .unwrap_or("Subtitles")
                    .to_string(),
                language: st
                    .get("Language")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    v
}

/// The user's subtitle setting on the server (Jellyfin's "Subtitle mode").
/// CatosTV follows it so Catfyn behaves like other Jellyfin clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubMode {
    /// Jellyfin "Default"/"Smart": subtitles in the preferred language when
    /// the audio is in another; forced ones otherwise.
    #[default]
    Default,
    /// "Always": subtitles on whenever a track exists.
    Always,
    /// "Only forced".
    OnlyForced,
    /// "None": off until picked.
    None,
}

/// Language preferences and subtitle mode from `GET /Users/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlayPrefs {
    pub sub_mode: SubMode,
    /// ISO 639 codes as the server stores them ("eng"); empty = any.
    pub sub_lang: String,
    pub audio_lang: String,
}

pub fn user_url(a: &Account) -> String {
    format!("{}/Users/{}", a.server, q(&a.user_id))
}

fn lang_code(v: Option<&Json>) -> String {
    v.and_then(Json::as_str)
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// `Configuration.{SubtitleMode, SubtitleLanguagePreference,
/// AudioLanguagePreference}` of a user reply; defaults when missing.
pub fn parse_play_prefs(body: &str) -> PlayPrefs {
    let Some(j) = crate::mpv::parse_json(body) else {
        return PlayPrefs::default();
    };
    let Some(c) = j.get("Configuration") else {
        return PlayPrefs::default();
    };
    let sub_mode = match c.get("SubtitleMode").and_then(Json::as_str) {
        Some("Always") => SubMode::Always,
        Some("OnlyForced") => SubMode::OnlyForced,
        Some("None") => SubMode::None,
        _ => SubMode::Default,
    };
    PlayPrefs {
        sub_mode,
        sub_lang: lang_code(c.get("SubtitleLanguagePreference")),
        audio_lang: lang_code(c.get("AudioLanguagePreference")),
    }
}

/// Every episode of a series, in order (seasons and specials as the
/// server sorts them), for the "Next episode" card.
pub fn series_episodes_url(a: &Account, series_id: &str) -> String {
    format!(
        "{}/Shows/{}/Episodes?UserId={}&Fields={ITEM_FIELDS}",
        a.server,
        q(series_id),
        a.user_id
    )
}

/// An item's media segments (`Intro`, `Outro`, …; Jellyfin 10.10+, filled
/// by plugins such as intro-skipper).
pub fn segments_url(a: &Account, item_id: &str) -> String {
    format!("{}/MediaSegments/{}", a.server, q(item_id))
}

/// Mark an item watched for the user (`POST`).
pub fn played_url(a: &Account, item_id: &str) -> String {
    format!(
        "{}/Users/{}/PlayedItems/{}",
        a.server,
        a.user_id,
        q(item_id)
    )
}

/// One item with the user's data (`GET /Users/{uid}/Items/{id}`).
pub fn item_url(a: &Account, item_id: &str) -> String {
    format!("{}/Users/{}/Items/{}", a.server, a.user_id, q(item_id))
}

pub fn parse_single_item(body: &str) -> Option<Item> {
    parse_item(&crate::mpv::parse_json(body.trim())?)
}

/// Direct play (no transcoding: mpv decodes 4K / HDR / any codec itself).
pub fn stream_url(a: &Account, item_id: &str) -> String {
    format!(
        "{}/Videos/{}/stream?static=true&MediaSourceId={}",
        a.server,
        q(item_id),
        q(item_id)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    Start,
    Progress,
    Stopped,
}

/// URL and JSON body of a playback report.
pub fn report(
    a: &Account,
    kind: Report,
    item_id: &str,
    seconds: f64,
    paused: bool,
) -> (String, String) {
    let ticks = (seconds.max(0.0) * TICKS) as u64;
    let path = match kind {
        Report::Start => "Sessions/Playing",
        Report::Progress => "Sessions/Playing/Progress",
        Report::Stopped => "Sessions/Playing/Stopped",
    };
    (
        format!("{}/{path}", a.server),
        format!(
            "{{\"ItemId\":{},\"PositionTicks\":{ticks},\"IsPaused\":{paused},\"PlayMethod\":\"DirectPlay\",\"CanSeek\":true}}",
            json_string(item_id)
        ),
    )
}

// --------------------------------------------------------------------- items

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Movie,
    Series,
    Season,
    Episode,
    /// A library (Views).
    Library,
    Other,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Item {
    /// Where it came from: the server's address and kind (Home mixes
    /// items from several servers).
    pub server: String,
    pub backend: Backend,
    pub id: String,
    pub name: String,
    pub kind_raw: String,
    pub series_name: String,
    pub series_id: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub year: Option<u32>,
    pub overview: String,
    /// Seconds.
    pub runtime: Option<f64>,
    /// Seconds already watched.
    pub position: f64,
    /// The server's own 0–100 figure (set when the runtime is unknown too).
    pub played_pct: Option<f32>,
    pub played: bool,
    pub has_primary: bool,
    /// Item that holds the backdrop (the series for an episode).
    pub backdrop_item: Option<String>,
    /// `CollectionType` of a library: "movies", "tvshows", …
    pub collection: String,
    /// Community rating (0–10) and age rating ("PG-13"), for the pills.
    pub rating: Option<f32>,
    pub age_rating: String,
    /// Plex picture paths (`/library/metadata/1/thumb/…`): the poster or
    /// episode still, the backdrop, and an episode's series poster.
    pub thumb: String,
    pub art: String,
    pub series_thumb: String,
    /// Plex: the file to play (`/library/parts/…`), when the listing had it.
    pub part: String,
}

impl Item {
    pub fn kind(&self) -> Kind {
        match self.kind_raw.as_str() {
            "Movie" | "Video" | "MusicVideo" => Kind::Movie,
            "Series" => Kind::Series,
            "Season" => Kind::Season,
            "Episode" => Kind::Episode,
            "CollectionFolder" | "UserView" => Kind::Library,
            _ => Kind::Other,
        }
    }

    pub fn playable(&self) -> bool {
        matches!(self.kind(), Kind::Movie | Kind::Episode)
    }

    /// 0–1 watched, for progress bars.
    pub fn progress(&self) -> Option<f32> {
        if let Some(r) = self.runtime
            && r > 0.0
            && self.position > 0.0
        {
            return Some((self.position / r).clamp(0.0, 1.0) as f32);
        }
        self.played_pct
            .filter(|p| *p > 0.0)
            .map(|p| (p / 100.0).clamp(0.0, 1.0))
    }

    /// "12 min left" for something half watched.
    pub fn time_left(&self) -> Option<String> {
        let r = self.runtime?;
        if self.position <= 0.0 || self.position >= r {
            return None;
        }
        let mins = ((r - self.position) / 60.0).round().max(1.0) as u32;
        Some(if mins >= 60 {
            format!("{}h {:02}m left", mins / 60, mins % 60)
        } else {
            format!("{mins} min left")
        })
    }

    /// "S2 · E5" for episodes.
    pub fn episode_label(&self) -> Option<String> {
        match (self.season, self.episode) {
            (Some(s), Some(e)) => Some(format!("S{s} · E{e}")),
            (None, Some(e)) => Some(format!("E{e}")),
            _ => None,
        }
    }

    /// "1 h 45 min" / "45 min".
    pub fn runtime_label(&self) -> Option<String> {
        let mins = (self.runtime? / 60.0).round() as u32;
        Some(if mins >= 60 {
            format!("{} h {} min", mins / 60, mins % 60)
        } else {
            format!("{mins} min")
        })
    }

    /// Short facts for the pills under a title: episode or year, runtime,
    /// age rating, score.
    pub fn pills(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(l) = self.episode_label() {
            v.push(l);
        } else if let Some(y) = self.year {
            v.push(y.to_string());
        }
        if let Some(r) = self.runtime_label() {
            v.push(r);
        }
        if !self.age_rating.is_empty() {
            v.push(self.age_rating.clone());
        }
        if let Some(r) = self.rating {
            v.push(format!("★ {r:.1}"));
        }
        v
    }

    /// Title for a tile: the series name for episodes.
    pub fn title(&self) -> &str {
        if self.kind() == Kind::Episode && !self.series_name.is_empty() {
            &self.series_name
        } else {
            &self.name
        }
    }

    /// Line under the title: "S1 · E3 · Name", a year, or "".
    pub fn subtitle(&self) -> String {
        match self.kind() {
            Kind::Episode => match self.episode_label() {
                Some(l) => format!("{l} · {}", self.name),
                None => self.name.clone(),
            },
            _ => self.year.map(|y| y.to_string()).unwrap_or_default(),
        }
    }
}

fn num(v: &Json, k: &str) -> Option<f64> {
    v.get(k).and_then(Json::as_f64)
}

fn text(v: &Json, k: &str) -> String {
    v.get(k).and_then(Json::as_str).unwrap_or("").to_string()
}

pub fn parse_item(v: &Json) -> Option<Item> {
    let id = v.get("Id")?.as_str()?.to_string();
    if !is_token_like(&id) {
        return None;
    }
    let user = v.get("UserData");
    let has_backdrop = matches!(v.get("BackdropImageTags"), Some(Json::Arr(a)) if !a.is_empty());
    let parent_backdrop = v
        .get("ParentBackdropItemId")
        .and_then(Json::as_str)
        .map(str::to_string);
    let has_primary = v.get("ImageTags").and_then(|t| t.get("Primary")).is_some();
    Some(Item {
        name: text(v, "Name"),
        kind_raw: text(v, "Type"),
        series_name: text(v, "SeriesName"),
        series_id: text(v, "SeriesId"),
        season: num(v, "ParentIndexNumber").map(|n| n as u32),
        episode: num(v, "IndexNumber").map(|n| n as u32),
        year: num(v, "ProductionYear").map(|n| n as u32),
        overview: text(v, "Overview"),
        runtime: num(v, "RunTimeTicks").map(|t| t / TICKS),
        position: user
            .and_then(|u| num(u, "PlaybackPositionTicks"))
            .map_or(0.0, |t| t / TICKS),
        played_pct: user
            .and_then(|u| num(u, "PlayedPercentage"))
            .map(|p| p as f32),
        played: user
            .and_then(|u| u.get("Played"))
            .and_then(Json::as_bool)
            .unwrap_or(false),
        has_primary,
        backdrop_item: if has_backdrop {
            Some(id.clone())
        } else {
            parent_backdrop
        },
        collection: text(v, "CollectionType"),
        rating: num(v, "CommunityRating").map(|n| n as f32),
        age_rating: text(v, "OfficialRating"),
        id,
        ..Item::default()
    })
}

/// `{"Items":[…]}` or a bare `[…]` (Latest) → items.
pub fn parse_items(text: &str) -> Result<Vec<Item>> {
    let v = parse_json(text.trim()).ok_or_else(|| Error::msg("unreadable server reply"))?;
    let arr = match &v {
        Json::Arr(a) => a,
        Json::Obj(_) => match v.get("Items") {
            Some(Json::Arr(a)) => a,
            _ => return Err(Error::msg("unexpected server reply")),
        },
        _ => return Err(Error::msg("unexpected server reply")),
    };
    Ok(arr.iter().filter_map(parse_item).collect())
}

/// Libraries worth showing (video ones; music/books/photos are not
/// something Catfyn plays).
pub fn video_libraries(views: Vec<Item>) -> Vec<Item> {
    views
        .into_iter()
        .filter(|v| {
            matches!(
                v.collection.as_str(),
                "movies" | "tvshows" | "homevideos" | "musicvideos" | ""
            )
        })
        .collect()
}

// -------------------------------------------------------------------- cache

/// `~/.cache/catostv/fin/<hash>.png` for an image URL (FNV-1a).
pub fn image_cache_path(home: &Path, url: &str) -> PathBuf {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    home.join(".cache/catostv/fin")
        .join(format!("{h:016x}.png"))
}

/// Download an image URL to `out` (images need no sign-in).
pub fn image_cmd(url: &str, out: &Path) -> Result<Cmd> {
    if !crate::mpv::is_playable_url(url) {
        return Err(Error::msg("bad image address"));
    }
    Ok(Cmd::new("curl")
        .args([
            "-sS".to_string(),
            "--fail".to_string(),
            "-m".to_string(),
            "20".to_string(),
            "-o".to_string(),
            out.display().to_string(),
            "--".to_string(),
            url.to_string(),
        ])
        .timeout(Duration::from_secs(25)))
}

// --------------------------------------------------------------------- mpv

/// mpv control socket for Catfyn playback.
pub fn player_socket() -> PathBuf {
    crate::env::runtime_dir().join("catfyn-mpv.sock")
}

/// mpv config file carrying the auth header (0600), so the token is not on
/// mpv's command line.
pub fn player_config(device_id: &str, token: &str) -> Result<PathBuf> {
    player_config_with(Some(&format!(
        "Authorization: {}",
        auth_value(device_id, Some(token))
    )))
}

/// The player's config file with one HTTP header for the stream and
/// subtitle files (`None`: no header, a server that needs no sign-in).
pub fn player_config_with(header: Option<&str>) -> Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = crate::env::runtime_dir().join("catfyn-mpv.conf");
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    // `-append` adds exactly one header (its commas are not list
    // separators) and `%N%` quotes the value by its byte length, which
    // every mpv version reads the same way; backslash-escaped quotes do
    // not parse on mpv 0.40.
    if let Some(h) = header {
        f.write_all(
            format!(
                "http-header-fields-append={}\n",
                crate::mpv::option_value(h)
            )
            .as_bytes(),
        )?;
    }
    drop(f);
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// mpv command line: full screen, resume at `start` seconds, the user's
/// mpv.conf (4K/HDR presets) plus the header file.
pub fn player_argv(
    url: &str,
    title: &str,
    start: f64,
    config: &Path,
    socket: &Path,
) -> Vec<String> {
    player_argv_with(url, title, start, config, socket, "7c6cf0")
}

/// [`player_argv`] with the TV's accent colour (`RRGGBB`) for the control
/// bar.
pub fn player_argv_with(
    url: &str,
    title: &str,
    start: f64,
    config: &Path,
    socket: &Path,
    accent: &str,
) -> Vec<String> {
    player_argv_full(
        url,
        title,
        start,
        config,
        socket,
        accent,
        &[],
        &PlayPrefs::default(),
    )
}

/// [`player_argv_with`] plus the server's external subtitle files
/// (`--sub-file`, fetched with the same auth header;).
#[allow(clippy::too_many_arguments)]
pub fn player_argv_full(
    url: &str,
    title: &str,
    start: f64,
    config: &Path,
    socket: &Path,
    accent: &str,
    subs: &[ExternalSub],
    prefs: &PlayPrefs,
) -> Vec<String> {
    let mut v: Vec<String> = [
        "mpv",
        "--fullscreen",
        "--force-window=immediate",
        "--idle=no",
        "--keep-open=no",
        "--no-terminal",
        "--ytdl=no",
        "--wayland-app-id=catfyn-player",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.push(format!("--include={}", config.display()));
    v.push(format!("--input-ipc-server={}", socket.display()));
    // Remote-friendly keys (media keys, F2 = subtitles) and the TV control
    // bar (OK = controls, OK again = pause;).
    v.push("--input-conf=~/.config/catostv/catfyn-input.conf".into());
    v.push("--script=~/.config/catostv/catfyn-osd.lua".into());
    v.push("--osc=no".into());
    let accent: String = accent
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(6)
        .collect();
    let mut script_opts = Vec::new();
    if accent.len() == 6 {
        script_opts.push(format!("catfyn_osd-accent={accent}"));
    }
    // Names for the external subtitle tracks, in --sub-file order (mpv
    // would show the URL's file name): the control bar's Subtitles list
    // reads them.
    let names: Vec<String> = subs.iter().take(12).map(sub_display_name).collect();
    if !names.is_empty() {
        script_opts.push(format!("catfyn_osd-subs={}", names.join(";")));
    }
    if !script_opts.is_empty() {
        v.push(format!("--script-opts={}", script_opts.join(",")));
    }
    // A progress bar that stays for a moment after every seek / pause.
    v.push("--osd-duration=1500".into());
    v.push("--osd-bar-align-y=0.86".into());
    v.push("--osd-bar-h=1.2".into());
    let clean: String = title
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    v.push(format!("--force-media-title={clean}"));
    for sub in subs.iter().take(12) {
        v.push(format!("--sub-file={}", sub.url));
    }
    // Which subtitles start on follows the user's Jellyfin setting
    // the Subtitles button lists embedded and external tracks.
    v.extend(sub_args(prefs));
    if start > 1.0 {
        v.push(format!("--start={:.0}", start));
    }
    v.push("--".into());
    v.push(url.into());
    v
}

/// "English (eng)"-style label for the Subtitles list, without the
/// characters mpv's `--script-opts` parser or the list separator use.
pub fn sub_display_name(sub: &ExternalSub) -> String {
    let clean = |t: &str| -> String {
        t.chars()
            .filter(|c| !c.is_control() && !matches!(c, ',' | ';' | '=' | '|'))
            .take(40)
            .collect()
    };
    let title = clean(&sub.title);
    let title = if title.trim().is_empty() {
        "Subtitles".to_string()
    } else {
        title.trim().to_string()
    };
    let lang = clean(&sub.language);
    if lang.is_empty() || title.to_lowercase().contains(&lang.to_lowercase()) {
        title
    } else {
        format!("{title} ({lang})")
    }
}

/// mpv's track-selection options for a subtitle mode. mpv picks the
/// subtitle track at start from `--slang`, `--subs-fallback` (what to do
/// when no track matches) and `--subs-with-matching-audio` (what to show
/// when the audio already is in the preferred language).
pub fn sub_args(p: &PlayPrefs) -> Vec<String> {
    let mut v = Vec::new();
    if !p.audio_lang.is_empty() {
        v.push(format!("--alang={}", p.audio_lang));
    }
    match p.sub_mode {
        SubMode::None => v.push("--sid=no".into()),
        SubMode::OnlyForced => {
            v.push("--sid=auto".into());
            v.push("--subs-fallback=no".into());
            v.push("--subs-with-matching-audio=forced".into());
        }
        SubMode::Always => {
            v.push("--sid=auto".into());
            if !p.sub_lang.is_empty() {
                v.push(format!("--slang={}", p.sub_lang));
            }
            v.push("--subs-fallback=yes".into());
            v.push("--subs-with-matching-audio=yes".into());
        }
        SubMode::Default => {
            v.push("--sid=auto".into());
            if !p.sub_lang.is_empty() {
                v.push(format!("--slang={}", p.sub_lang));
            }
            v.push("--subs-fallback=default".into());
            v.push("--subs-with-matching-audio=forced".into());
        }
    }
    v
}

#[cfg(test)]
mod tests {

    #[test]
    fn header_file_survives_parallel_requests() {
        let threads: Vec<_> = (0..16)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..50 {
                        let p = header_file("dev", Some("tok")).unwrap();
                        let s = std::fs::read_to_string(&p).unwrap();
                        assert!(s.contains("Token=\"tok\""));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
    }
    use super::*;

    fn acct() -> Account {
        Account {
            server: "http://10.0.0.5:8096".into(),
            server_name: "Home".into(),
            user_id: "u1".into(),
            user_name: "ok".into(),
            token: "tok123".into(),
            incognito: false,
            ..Account::default()
        }
    }

    #[test]
    fn public_info_names_a_server_found_by_scanning() {
        let s = parse_public_info(
            r#"{"LocalAddress":"http://172.17.0.2:8096","ServerName":"Den","Version":"10.10.7","Id":"abc"}"#,
            "http://10.0.0.5:8096",
        )
        .unwrap();
        // The address it was reached at, not the container's own.
        assert_eq!(
            (s.name.as_str(), s.id.as_str(), s.address.as_str()),
            ("Den", "abc", "http://10.0.0.5:8096")
        );
        assert_eq!(parse_public_info("{}", "http://x"), None);
    }

    #[test]
    fn discovery_reply() {
        let s = parse_discovery(r#"{"Address":"http://192.168.1.20:8096","Id":"abc","Name":"Den","EndpointAddress":null}"#)
            .unwrap();
        assert_eq!(s.address, "http://192.168.1.20:8096");
        assert_eq!(s.name, "Den");
        assert!(parse_discovery("garbage").is_none());
        assert!(parse_discovery(r#"{"Address":"file:///etc"}"#).is_none());
    }

    #[test]
    fn addresses_are_normalized() {
        assert_eq!(
            normalize_address("192.168.1.5").unwrap(),
            "http://192.168.1.5:8096"
        );
        assert_eq!(
            normalize_address("jelly.lan:8920/").unwrap(),
            "http://jelly.lan:8920"
        );
        assert_eq!(
            normalize_address("https://media.example.org/jf/").unwrap(),
            "https://media.example.org/jf"
        );
        assert_eq!(
            normalize_address("nas/jellyfin").unwrap(),
            "http://nas:8096/jellyfin"
        );
        assert!(normalize_address("").is_none());
        assert!(normalize_address("a b").is_none());
    }

    #[test]
    fn accounts_round_trip_and_home_account() {
        let mut a = Accounts {
            device_id: "dev1".into(),
            ..Default::default()
        };
        a.add(acct());
        a.add(Account {
            user_id: "u2".into(),
            user_name: "guest".into(),
            incognito: true,
            ..acct()
        });
        assert_eq!(a.active, 1);
        // The incognito account is active: Home shows the other one.
        assert_eq!(a.home().unwrap().user_id, "u1");
        let b = Accounts::from_conf(&a.to_conf());
        assert_eq!(a, b);
        // Re-signing in keeps the incognito choice.
        a.add(Account {
            user_id: "u2".into(),
            token: "new".into(),
            ..acct()
        });
        assert!(a.list[1].incognito && a.list[1].token == "new");
        a.list[0].incognito = true;
        assert!(a.home().is_none());
        a.remove(0);
        assert_eq!(a.list.len(), 1);
        assert_eq!(
            Accounts::from_conf("[account]\nserver=\ntoken=x\n")
                .list
                .len(),
            0
        );
    }

    #[test]
    fn login_and_errors() {
        let body = login_body("o\"k", "p");
        assert_eq!(body, r#"{"Username":"o\"k","Pw":"p"}"#);
        let r = parse_login(
            r#"{"User":{"Name":"ok","Id":"a1b2"},"AccessToken":"f00d","ServerId":"s"}"#,
        )
        .unwrap();
        assert_eq!(r, ("a1b2".into(), "ok".into(), "f00d".into()));
        assert!(parse_login(r#"{"User":{"Id":"x y"},"AccessToken":"t"}"#).is_none());
        assert_eq!(
            friendly_error("curl: (22) The requested URL returned error: 401"),
            "Wrong user name or password"
        );
        assert_eq!(
            friendly_error("curl: (7) Failed to connect"),
            "Can't reach the server"
        );
    }

    #[test]
    fn items_parse() {
        let json = r#"{"Items":[
          {"Id":"e1","Name":"Pilot","Type":"Episode","SeriesName":"Show","SeriesId":"s1",
           "ParentIndexNumber":1,"IndexNumber":2,"RunTimeTicks":36000000000,
           "UserData":{"PlaybackPositionTicks":9000000000,"Played":false},
           "ImageTags":{"Primary":"t"},"ParentBackdropItemId":"s1"},
          {"Id":"m1","Name":"Film","Type":"Movie","ProductionYear":2024,"BackdropImageTags":["b"],
           "CommunityRating":7.84,"OfficialRating":"PG-13","RunTimeTicks":63000000000},
          {"Name":"no id"}
        ],"TotalRecordCount":3}"#;
        let items = parse_items(json).unwrap();
        assert_eq!(items.len(), 2);
        let e = &items[0];
        assert_eq!(e.kind(), Kind::Episode);
        assert_eq!(e.title(), "Show");
        assert_eq!(e.subtitle(), "S1 · E2 · Pilot");
        assert_eq!(e.progress(), Some(0.25));
        assert_eq!(e.backdrop_item.as_deref(), Some("s1"));
        assert!(e.playable() && e.has_primary);
        let m = &items[1];
        assert_eq!((m.kind(), m.subtitle().as_str()), (Kind::Movie, "2024"));
        assert_eq!(m.backdrop_item.as_deref(), Some("m1"));
        assert_eq!(m.pills(), ["2024", "1 h 45 min", "PG-13", "★ 7.8"]);
        // Latest replies are a bare array.
        assert_eq!(
            parse_items(r#"[{"Id":"x","Type":"Series"}]"#).unwrap()[0].kind(),
            Kind::Series
        );
        assert!(parse_items("{}").is_err());
        let libs = video_libraries(
            parse_items(r#"{"Items":[{"Id":"l1","Type":"CollectionFolder","CollectionType":"movies"},{"Id":"l2","Type":"CollectionFolder","CollectionType":"music"}]}"#).unwrap(),
        );
        assert_eq!(libs.len(), 1);
    }

    #[test]
    fn urls_and_reports() {
        let a = acct();
        assert_eq!(views_url(&a), "http://10.0.0.5:8096/Users/u1/Views");
        assert!(resume_url(&a, 12).contains("/Users/u1/Items/Resume?Limit=12"));
        assert!(next_up_url(&a, 8).contains("/Shows/NextUp?UserId=u1"));
        assert!(latest_url(&a, "lib 1", 5).contains("ParentId=lib%201"));
        assert_eq!(
            image_url(&a.server, "m1", "Backdrop", 1280),
            "http://10.0.0.5:8096/Items/m1/Images/Backdrop/0?maxWidth=1280&quality=85&format=Png"
        );
        assert!(stream_url(&a, "m1").ends_with("/Videos/m1/stream?static=true&MediaSourceId=m1"));
        let (url, body) = report(&a, Report::Progress, "m1", 90.5, true);
        assert_eq!(url, "http://10.0.0.5:8096/Sessions/Playing/Progress");
        assert!(body.contains("\"PositionTicks\":905000000") && body.contains("\"IsPaused\":true"));
        let auth = auth_value("dev", Some("tok"));
        assert!(
            auth.starts_with("MediaBrowser Client=\"Catfyn\"") && auth.ends_with("Token=\"tok\"")
        );
        // The token never appears in curl's argv.
        let cmd = request_cmd(&views_url(&a), Path::new("/run/h"), None);
        assert!(cmd.args.iter().all(|x| !x.contains("tok123")));
        assert!(cmd.args.contains(&"@/run/h".to_string()));
        let argv = player_argv(
            &stream_url(&a, "m1"),
            "Film",
            125.0,
            Path::new("/run/c"),
            Path::new("/run/s"),
        );
        assert!(argv.contains(&"--start=125".to_string()));
        assert!(argv.iter().all(|x| !x.contains("tok123")));
        assert!(argv.contains(&"--script=~/.config/catostv/catfyn-osd.lua".to_string()));
        let argv = player_argv_with(
            &stream_url(&a, "m1"),
            "Film",
            0.0,
            Path::new("/run/c"),
            Path::new("/run/s"),
            "8B7CFF; rm -rf /",
        );
        assert!(argv.contains(&"--script-opts=catfyn_osd-accent=8B7CFF".to_string()));
        // External subtitles.
        let info = r#"{"MediaSources":[{"MediaStreams":[
            {"Type":"Video"},
            {"Type":"Subtitle","IsExternal":true,"DeliveryMethod":"External","DeliveryUrl":"/Videos/m1/m1/Subtitles/2/0/Stream.srt","DisplayTitle":"English","Language":"eng"},
            {"Type":"Subtitle","IsExternal":false,"DeliveryMethod":"Embed"},
            {"Type":"Subtitle","IsExternal":true,"DeliveryUrl":"http://evil/x.srt"}
        ]}]}"#;
        let subs = parse_external_subs(&a, info);
        assert_eq!(subs.len(), 1);
        assert_eq!(
            subs[0].url,
            "http://10.0.0.5:8096/Videos/m1/m1/Subtitles/2/0/Stream.srt"
        );
        assert_eq!(subs[0].language, "eng");
        let argv = player_argv_full(
            &stream_url(&a, "m1"),
            "Film",
            0.0,
            Path::new("/run/c"),
            Path::new("/run/s"),
            "8B7CFF",
            &subs,
            &PlayPrefs::default(),
        );
        assert!(
            argv.iter()
                .any(|x| x.starts_with("--sub-file=http://10.0.0.5:8096/Videos/m1/"))
        );
        assert!(argv.contains(
            &"--script-opts=catfyn_osd-accent=8B7CFF,catfyn_osd-subs=English".to_string()
        ));
        assert!(
            argv.contains(&"--sid=auto".to_string()) && !argv.contains(&"--sid=no".to_string())
        );
        assert!(argv.contains(&"--subs-with-matching-audio=forced".to_string()));
        assert!(playback_info_url(&a, "m1").contains("/Items/m1/PlaybackInfo?UserId="));
        assert!(image_cmd("file:///etc/passwd", Path::new("/tmp/x")).is_err());
    }

    #[test]
    fn subtitle_prefs_follow_the_server_user() {
        let a = Account {
            server: "http://10.0.0.5:8096".into(),
            server_name: "s".into(),
            user_id: "u1".into(),
            user_name: "me".into(),
            token: "tok123".into(),
            incognito: false,
            ..Account::default()
        };
        assert_eq!(user_url(&a), "http://10.0.0.5:8096/Users/u1");
        let p = parse_play_prefs(
            r#"{"Name":"me","Configuration":{"SubtitleMode":"Always","SubtitleLanguagePreference":"eng","AudioLanguagePreference":"jpn; rm"}}"#,
        );
        assert_eq!(p.sub_mode, SubMode::Always);
        assert_eq!(p.sub_lang, "eng");
        assert_eq!(p.audio_lang, "jpnrm");
        assert_eq!(
            sub_args(&p),
            [
                "--alang=jpnrm",
                "--sid=auto",
                "--slang=eng",
                "--subs-fallback=yes",
                "--subs-with-matching-audio=yes"
            ]
        );
        assert_eq!(parse_play_prefs("junk"), PlayPrefs::default());
        assert_eq!(
            parse_play_prefs(r#"{"Configuration":{"SubtitleMode":"Smart"}}"#).sub_mode,
            SubMode::Default
        );
        assert_eq!(
            sub_args(&parse_play_prefs(
                r#"{"Configuration":{"SubtitleMode":"None"}}"#
            )),
            ["--sid=no"]
        );
        assert_eq!(
            sub_args(&parse_play_prefs(
                r#"{"Configuration":{"SubtitleMode":"OnlyForced"}}"#
            )),
            [
                "--sid=auto",
                "--subs-fallback=no",
                "--subs-with-matching-audio=forced"
            ]
        );
        let s = ExternalSub {
            url: String::new(),
            title: "Eng,lish; forced=1".into(),
            language: "eng".into(),
        };
        assert_eq!(sub_display_name(&s), "English forced1");
        let s = ExternalSub {
            url: String::new(),
            title: "  ".into(),
            language: "spa".into(),
        };
        assert_eq!(sub_display_name(&s), "Subtitles (spa)");
    }

    #[test]
    fn next_episode_lookup() {
        let a = Account {
            server: "http://10.0.0.5:8096".into(),
            server_name: "s".into(),
            user_id: "u1".into(),
            user_name: "me".into(),
            token: "tok123".into(),
            incognito: false,
            ..Account::default()
        };
        // the whole series in order, its credits segments, watched.
        assert_eq!(
            series_episodes_url(&a, "ser1"),
            format!("http://10.0.0.5:8096/Shows/ser1/Episodes?UserId=u1&Fields={ITEM_FIELDS}")
        );
        assert_eq!(
            segments_url(&a, "ep5"),
            "http://10.0.0.5:8096/MediaSegments/ep5"
        );
        assert_eq!(
            played_url(&a, "ep5"),
            "http://10.0.0.5:8096/Users/u1/PlayedItems/ep5"
        );
        assert_eq!(
            item_url(&a, "ep6"),
            "http://10.0.0.5:8096/Users/u1/Items/ep6"
        );
        let one = parse_single_item(r#"{"Id":"ep6","Name":"Six","Type":"Episode"}"#).unwrap();
        assert_eq!(one.name, "Six");
    }
}

#[cfg(test)]
mod artwork_tests {
    use super::*;

    #[test]
    fn shared_art_requests_4k_landscapes_and_high_resolution_posters() {
        let mut item = Item {
            id: "movie".into(),
            server: "https://media.example".into(),
            kind_raw: "Movie".into(),
            backdrop_item: Some("movie".into()),
            ..Item::default()
        };
        assert!(wide_url(&item).contains("Backdrop/0?maxWidth=3840"));
        assert!(poster_url(&item).contains("maxWidth=2160"));
        assert!(backdrop_url(&item, 6).unwrap().contains("maxWidth=3840"));
        item.kind_raw = "Episode".into();
        item.has_primary = true;
        assert!(wide_url(&item).contains("/movie/Images/Primary?maxWidth=3840"));
        assert!(still_url(&item).contains("maxWidth=3840"));
        item.backend = Backend::Plex;
        item.art = "/art".into();
        item.thumb = "/thumb".into();
        assert!(wide_url(&item).contains("width=3840&height=2160"));
        assert!(poster_url(&item).contains("width=2160&height=3240"));
    }
}
