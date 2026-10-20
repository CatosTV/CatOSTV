//! Catfyn's Plex side: sign-in with a code at plex.tv/link, the account's
//! servers, libraries, playback and progress, turned into the same
//! [`Item`]s as Jellyfin so the screens need not care which server they
//! show.
//!
//! Pure, like [`crate::jellyfin`]: it builds URLs and header files and
//! parses replies; the shell's jobs run `curl`. API facts (Plex Media
//! Server 1.4x, plex.tv API v2, JSON with `Accept: application/json`):
//!
//! | What | Request |
//! |---|---|
//! | link code | `POST https://plex.tv/api/v2/pins` → `{id, code}`; the user enters the code at plex.tv/link |
//! | linked? | `GET https://plex.tv/api/v2/pins/{id}` → `authToken` once linked |
//! | who | `GET https://plex.tv/api/v2/user` → `{uuid, username}` |
//! | servers | `GET https://plex.tv/api/v2/resources?includeHttps=1&includeRelay=1` → `[{name, clientIdentifier, accessToken, provides, connections}]` |
//! | server id | `GET /identity` → `MediaContainer.machineIdentifier` |
//! | libraries | `GET /library/sections` (`Directory`, `type` movie / show) |
//! | a library | `GET /library/sections/{key}/all` |
//! | continue | `GET /library/onDeck` (half-watched and next episodes) |
//! | newest | `GET /library/sections/{key}/recentlyAdded` |
//! | seasons / episodes | `GET /library/metadata/{key}/children`, `…/allLeaves` (whole show) |
//! | one item | `GET /library/metadata/{key}?includeMarkers=1` (files, subtitle streams, credits marker) |
//! | search | `GET /hubs/search?query=…` (hubs per type) |
//! | pictures | `GET /photo/:/transcode?url={thumb}&width&height&format=png[&blur]` |
//! | play | `GET {Part.key}` (the file as it is: mpv decodes it) |
//! | progress | `GET /:/timeline?ratingKey&key&state&time&duration` (ms) |
//! | watched | `GET /:/scrobble?key={key}&identifier=com.plexapp.plugins.library` |
//!
//! The token is sent as the `X-Plex-Token` header from a 0600 file, never
//! on a command line or in an address, the same way as Jellyfin's.

use crate::jellyfin::{Account, Backend, ExternalSub, Item, Server, is_token_like};
use crate::mpv::{Json, parse_json};
use std::path::PathBuf;

pub const PRODUCT: &str = "Catfyn";
pub const PORT: u16 = 32400;
const PLEX_TV: &str = "https://plex.tv/api/v2";
/// Where the user enters the code.
pub const LINK_PAGE: &str = "plex.tv/link";
/// Plex positions and lengths are milliseconds.
const MS: f64 = 1000.0;

// ------------------------------------------------------------------ headers

/// The request headers: who is asking (Plex wants a stable client id and a
/// product name) and, once signed in, the token.
pub fn header_text(device_id: &str, token: Option<&str>) -> String {
    let mut s = format!(
        "Accept: application/json\nX-Plex-Product: {PRODUCT}\nX-Plex-Version: {}\nX-Plex-Client-Identifier: {device_id}\nX-Plex-Device: CatosTV\nX-Plex-Device-Name: CatosTV\nX-Plex-Platform: Linux\n",
        crate::env::version()
    );
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        s.push_str(&format!("X-Plex-Token: {t}\n"));
    }
    s
}

/// [`header_text`] in a private file for `curl -H @file`.
pub fn header_file(device_id: &str, token: Option<&str>) -> crate::error::Result<PathBuf> {
    crate::jellyfin::write_headers("plex", &header_text(device_id, token))
}

/// The mpv option that sends the token with the stream and subtitle files
/// (in the player's 0600 config, see [`crate::jellyfin::player_config`]).
pub fn player_header(token: &str) -> Option<String> {
    (!token.is_empty()).then(|| format!("X-Plex-Token: {token}"))
}

// ------------------------------------------------------------------ sign in

pub fn pin_url() -> String {
    format!("{PLEX_TV}/pins")
}

pub fn pin_check_url(id: u64) -> String {
    format!("{PLEX_TV}/pins/{id}")
}

/// A new link code: (pin id, code to type at plex.tv/link).
pub fn parse_pin(text: &str) -> Option<(u64, String)> {
    let v = parse_json(text.trim())?;
    let id = v.get("id")?.as_f64()? as u64;
    let code = v.get("code")?.as_str()?.to_string();
    (id > 0
        && !code.is_empty()
        && code.len() <= 16
        && code.chars().all(|c| c.is_ascii_alphanumeric()))
    .then_some((id, code))
}

/// The plex.tv token once the code was entered, else `None`.
pub fn parse_pin_token(text: &str) -> Option<String> {
    let v = parse_json(text.trim())?;
    let t = v.get("authToken")?.as_str()?.to_string();
    is_token_like(&t).then_some(t)
}

pub fn user_url() -> String {
    format!("{PLEX_TV}/user")
}

/// (uuid, name) of the signed-in plex.tv user.
pub fn parse_user(text: &str) -> Option<(String, String)> {
    let v = parse_json(text.trim())?;
    let uuid = v.get("uuid")?.as_str()?.to_string();
    let name = ["username", "title", "friendlyName"]
        .iter()
        .find_map(|k| v.get(k).and_then(Json::as_str).filter(|s| !s.is_empty()))
        .unwrap_or("Plex")
        .to_string();
    is_token_like(&uuid).then_some((uuid, name))
}

pub fn resources_url() -> String {
    format!("{PLEX_TV}/resources?includeHttps=1&includeRelay=1")
}

/// A server the plex.tv account can use, with the ways to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexServer {
    pub name: String,
    pub id: String,
    /// The server's own token for this user (shared servers have one each).
    pub token: String,
    /// Addresses to try, best first: on the home network, then direct over
    /// the internet, then Plex's relay.
    pub addresses: Vec<String>,
    pub owned: bool,
}

/// The servers among an account's resources.
pub fn parse_resources(text: &str) -> Vec<PlexServer> {
    let Some(Json::Arr(list)) = parse_json(text.trim()) else {
        return Vec::new();
    };
    let mut v = Vec::new();
    for r in &list {
        let provides = r.get("provides").and_then(Json::as_str).unwrap_or("");
        if !provides.split(',').any(|p| p.trim() == "server") {
            continue;
        }
        let id = r
            .get("clientIdentifier")
            .and_then(Json::as_str)
            .unwrap_or("");
        let token = r.get("accessToken").and_then(Json::as_str).unwrap_or("");
        if !is_token_like(id) || !(token.is_empty() || is_token_like(token)) {
            continue;
        }
        let mut conns: Vec<(u8, String)> = r
            .get("connections")
            .and_then(Json::as_arr)
            .map(|cs| {
                cs.iter()
                    .filter_map(|c| {
                        let uri = c.get("uri").and_then(Json::as_str)?;
                        let uri = normalize_address(uri)?;
                        let local = c.get("local").and_then(Json::as_bool) == Some(true);
                        let relay = c.get("relay").and_then(Json::as_bool) == Some(true);
                        Some((
                            if relay {
                                2
                            } else if local {
                                0
                            } else {
                                1
                            },
                            uri,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        conns.sort_by_key(|(rank, _)| *rank);
        let mut addresses: Vec<String> = Vec::new();
        for (_, u) in conns {
            if !addresses.contains(&u) {
                addresses.push(u);
            }
        }
        if addresses.is_empty() {
            continue;
        }
        v.push(PlexServer {
            name: text_of(r, "name"),
            id: id.to_string(),
            token: token.to_string(),
            addresses,
            owned: r.get("owned").and_then(Json::as_bool) == Some(true),
        });
    }
    v
}

pub fn identity_url(server: &str) -> String {
    format!("{server}/identity")
}

/// `machineIdentifier` from `/identity` (answers without a token).
pub fn parse_identity(text: &str) -> Option<String> {
    let v = parse_json(text.trim())?;
    let id = v
        .get("MediaContainer")?
        .get("machineIdentifier")?
        .as_str()?;
    is_token_like(id).then(|| id.to_string())
}

/// Plex servers on this network (port 32400 open and answering
/// `/identity`), named as they call themselves when they say.
pub fn discover_local(timeout: std::time::Duration) -> Vec<Server> {
    use std::time::Duration;
    crate::http::scan_lan(PORT, timeout)
        .into_iter()
        .filter_map(|ip| {
            let addr = std::net::SocketAddrV4::new(ip, PORT);
            let get = |path: &str| {
                crate::http::get_small(
                    addr,
                    path,
                    &[("Accept", "application/json")],
                    Duration::from_secs(2),
                    64 * 1024,
                )
            };
            let id = parse_identity(&get("/identity").ok()?)?;
            let name = get("/")
                .ok()
                .and_then(|b| parse_friendly_name(&b))
                .unwrap_or_else(|| format!("Plex at {ip}"));
            Some(Server {
                id,
                name,
                address: format!("http://{ip}:{PORT}"),
            })
        })
        .collect()
}

/// The server's own name from its root (`MediaContainer.friendlyName`).
pub fn parse_friendly_name(text: &str) -> Option<String> {
    let v = parse_json(text.trim())?;
    let n = v.get("MediaContainer")?.get("friendlyName")?.as_str()?;
    (!n.trim().is_empty()).then(|| n.trim().to_string())
}

/// A typed-in Plex address: `http://` and port 32400 when left out.
pub fn normalize_address(input: &str) -> Option<String> {
    let s = input.trim().trim_end_matches('/');
    let has_scheme = s.starts_with("http://") || s.starts_with("https://");
    let host_has_port = s
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .is_some_and(|h| h.contains(':'));
    let full = match (has_scheme, host_has_port) {
        (true, _) => s.to_string(),
        (false, true) => format!("http://{s}"),
        (false, false) => format!("http://{s}:{PORT}"),
    };
    crate::jellyfin::normalize_address(&full)
}

/// The account for a Plex server reached at `address`.
pub fn account(
    server: &PlexServer,
    address: &str,
    user_id: &str,
    user_name: &str,
    cloud_token: &str,
) -> Account {
    Account {
        backend: Backend::Plex,
        server: address.to_string(),
        server_name: server.name.clone(),
        user_id: user_id.to_string(),
        user_name: user_name.to_string(),
        token: server.token.clone(),
        incognito: false,
        cloud_token: cloud_token.to_string(),
        server_id: server.id.clone(),
    }
}

/// The account for a server on the home network that needs no sign-in.
pub fn local_account(server: &Server) -> Account {
    Account {
        backend: Backend::Plex,
        server: server.address.clone(),
        server_name: server.name.clone(),
        user_id: server.id.clone(),
        user_name: "Home network".into(),
        server_id: server.id.clone(),
        ..Account::default()
    }
}

// --------------------------------------------------------------- libraries

fn q(s: &str) -> String {
    crate::http::percent_encode(s)
}

/// Item keys are numbers (`ratingKey`); anything else is refused before it
/// becomes part of an address.
fn key_ok(k: &str) -> bool {
    !k.is_empty() && k.len() <= 20 && k.bytes().all(|b| b.is_ascii_digit())
}

pub fn sections_url(a: &Account) -> String {
    format!("{}/library/sections", a.server)
}

pub fn library_url(a: &Account, section: &str) -> String {
    format!(
        "{}/library/sections/{}/all?sort=titleSort",
        a.server,
        if key_ok(section) { section } else { "0" }
    )
}

pub fn on_deck_url(a: &Account, limit: usize) -> String {
    format!(
        "{}/library/onDeck?X-Plex-Container-Start=0&X-Plex-Container-Size={limit}",
        a.server
    )
}

pub fn recent_url(a: &Account, section: &str, limit: usize) -> String {
    format!(
        "{}/library/sections/{}/recentlyAdded?X-Plex-Container-Start=0&X-Plex-Container-Size={limit}",
        a.server,
        if key_ok(section) { section } else { "0" }
    )
}

pub fn children_url(a: &Account, key: &str) -> String {
    format!(
        "{}/library/metadata/{}/children",
        a.server,
        if key_ok(key) { key } else { "0" }
    )
}

pub fn all_episodes_url(a: &Account, show: &str) -> String {
    format!(
        "{}/library/metadata/{}/allLeaves",
        a.server,
        if key_ok(show) { show } else { "0" }
    )
}

pub fn item_url(a: &Account, key: &str) -> String {
    format!(
        "{}/library/metadata/{}?includeMarkers=1",
        a.server,
        if key_ok(key) { key } else { "0" }
    )
}

pub fn search_url(a: &Account, term: &str, limit: u32) -> String {
    format!("{}/hubs/search?query={}&limit={limit}", a.server, q(term))
}

fn text_of(v: &Json, k: &str) -> String {
    v.get(k).and_then(Json::as_str).unwrap_or("").to_string()
}

fn num(v: &Json, k: &str) -> Option<f64> {
    v.get(k).and_then(|n| {
        n.as_f64()
            .or_else(|| n.as_str().and_then(|s| s.parse::<f64>().ok()))
    })
}

/// Picture paths are the server's own (`/library/…`, `/:/…`).
fn path_of(v: &Json, k: &str) -> String {
    let p = text_of(v, k);
    if p.starts_with('/') && !p.contains("..") && !p.chars().any(|c| c.is_whitespace()) {
        p
    } else {
        String::new()
    }
}

/// One `Metadata` (or `Directory`, a library) entry as an [`Item`].
pub fn parse_item(v: &Json, server: &str) -> Option<Item> {
    let kind = text_of(v, "type");
    // A library from /library/sections: `key` is its number.
    if v.get("ratingKey").is_none() {
        let key = text_of(v, "key");
        if !key_ok(&key) {
            return None;
        }
        return Some(Item {
            server: server.to_string(),
            backend: Backend::Plex,
            id: key,
            name: text_of(v, "title"),
            kind_raw: "CollectionFolder".into(),
            collection: match kind.as_str() {
                "movie" => "movies".into(),
                "show" => "tvshows".into(),
                other => other.to_string(),
            },
            has_primary: false,
            ..Item::default()
        });
    }
    let id = text_of(v, "ratingKey");
    if !key_ok(&id) {
        return None;
    }
    let kind_raw = match kind.as_str() {
        "movie" => "Movie",
        "show" => "Series",
        "season" => "Season",
        "episode" => "Episode",
        "clip" => "Video",
        _ => "Other",
    }
    .to_string();
    let duration = num(v, "duration").map(|d| d / MS);
    let viewed = num(v, "viewCount").unwrap_or(0.0) > 0.0;
    let part = v
        .get("Media")
        .and_then(Json::as_arr)
        .and_then(|m| m.first())
        .and_then(|m| m.get("Part"))
        .and_then(Json::as_arr)
        .and_then(|p| p.first())
        .map(|p| path_of(p, "key"))
        .unwrap_or_default();
    let (series_name, series_id, season, episode) = match kind.as_str() {
        "episode" => (
            text_of(v, "grandparentTitle"),
            text_of(v, "grandparentRatingKey"),
            num(v, "parentIndex").map(|n| n as u32),
            num(v, "index").map(|n| n as u32),
        ),
        "season" => (
            text_of(v, "parentTitle"),
            text_of(v, "parentRatingKey"),
            num(v, "index").map(|n| n as u32),
            None,
        ),
        _ => (String::new(), String::new(), None, None),
    };
    // A season counts as watched when every episode is.
    let leaves = num(v, "leafCount");
    let viewed_leaves = num(v, "viewedLeafCount");
    let played = match (leaves, viewed_leaves) {
        (Some(l), Some(w)) if l > 0.0 => w >= l,
        _ => viewed,
    };
    let thumb = path_of(v, "thumb");
    let art = match path_of(v, "art") {
        a if a.is_empty() => path_of(v, "grandparentArt"),
        a => a,
    };
    Some(Item {
        server: server.to_string(),
        backend: Backend::Plex,
        name: text_of(v, "title"),
        kind_raw,
        series_name,
        series_id: if key_ok(&series_id) {
            series_id
        } else {
            String::new()
        },
        season,
        episode,
        year: num(v, "year").map(|n| n as u32),
        overview: text_of(v, "summary"),
        runtime: duration,
        position: num(v, "viewOffset").map_or(0.0, |o| o / MS),
        played_pct: None,
        played,
        has_primary: !thumb.is_empty(),
        backdrop_item: (!art.is_empty()).then(|| id.clone()),
        collection: String::new(),
        rating: num(v, "audienceRating")
            .or_else(|| num(v, "rating"))
            .map(|r| r as f32),
        age_rating: text_of(v, "contentRating"),
        thumb,
        art,
        series_thumb: path_of(v, "grandparentThumb"),
        part,
        id,
    })
}

/// Items of a reply: `Metadata` (items), `Directory` (libraries) or
/// `Hub` (search: films and shows).
pub fn parse_items(text: &str, server: &str) -> crate::error::Result<Vec<Item>> {
    let v = parse_json(text.trim())
        .ok_or_else(|| crate::error::Error::msg("unreadable server reply"))?;
    let mc = v
        .get("MediaContainer")
        .ok_or_else(|| crate::error::Error::msg("unexpected server reply"))?;
    let arr = |k: &str| {
        mc.get(k)
            .and_then(Json::as_arr)
            .map(<[Json]>::to_vec)
            .unwrap_or_default()
    };
    let mut out: Vec<Item> = arr("Metadata")
        .iter()
        .chain(arr("Directory").iter())
        .filter_map(|m| parse_item(m, server))
        .collect();
    for hub in arr("Hub") {
        if !matches!(
            hub.get("type").and_then(Json::as_str),
            Some("movie" | "show")
        ) {
            continue;
        }
        if let Some(ms) = hub.get("Metadata").and_then(Json::as_arr) {
            out.extend(ms.iter().filter_map(|m| parse_item(m, server)));
        }
    }
    Ok(out)
}

/// Libraries Catfyn plays (films and shows).
pub fn video_libraries(libs: Vec<Item>) -> Vec<Item> {
    libs.into_iter()
        .filter(|l| matches!(l.collection.as_str(), "movies" | "tvshows"))
        .collect()
}

/// Continue watching from On Deck: what is half watched. The rest of On
/// Deck (the next episode of a show) is "Next up". Plex keeps the old
/// position of something already watched to the end; that is done, not
/// half watched, and goes.
pub fn split_on_deck(items: Vec<Item>) -> (Vec<Item>, Vec<Item>) {
    let finished = |i: &Item| i.played && i.runtime.is_some_and(|r| r - i.position < 60.0);
    items
        .into_iter()
        .filter(|i| !finished(i))
        .partition(|i| i.position > 0.0)
}

// ------------------------------------------------------------------ pictures

/// A picture, scaled and as PNG by the server; `blur` softens it (sigma
/// in pixels). The token goes as a header ([`header_file`]).
pub fn image_url(server: &str, path: &str, width: u32, height: u32, blur: u32) -> String {
    let mut s = format!(
        "{server}/photo/:/transcode?width={width}&height={height}&minSize=1&upscale=1&format=png&url={}",
        q(path)
    );
    if blur > 0 {
        s.push_str(&format!("&blur={blur}"));
    }
    s
}

// ------------------------------------------------------------------- playback

/// The file itself (direct play: mpv decodes 4K / HDR / any codec).
pub fn stream_url(a: &Account, part: &str) -> Option<String> {
    (part.starts_with("/library/parts/") && !part.contains(".."))
        .then(|| format!("{}{part}", a.server))
}

/// The first file of an item reply (`/library/metadata/{key}`).
pub fn parse_part(body: &str) -> Option<String> {
    let items = parse_items(body, "").ok()?;
    items
        .into_iter()
        .next()
        .map(|i| i.part)
        .filter(|p| !p.is_empty())
}

/// Subtitle files the server keeps next to the video (streams of type 3
/// with a `key`); embedded ones mpv finds in the file.
pub fn parse_external_subs(a: &Account, body: &str) -> Vec<ExternalSub> {
    let Some(v) = parse_json(body.trim()) else {
        return Vec::new();
    };
    let parts = v
        .get("MediaContainer")
        .and_then(|m| m.get("Metadata"))
        .and_then(Json::as_arr)
        .and_then(|m| m.first())
        .and_then(|m| m.get("Media"))
        .and_then(Json::as_arr)
        .and_then(|m| m.first())
        .and_then(|m| m.get("Part"))
        .and_then(Json::as_arr)
        .and_then(|p| p.first())
        .and_then(|p| p.get("Stream"))
        .and_then(Json::as_arr)
        .map(<[Json]>::to_vec)
        .unwrap_or_default();
    parts
        .iter()
        .filter(|s| num(s, "streamType") == Some(3.0))
        .filter_map(|s| {
            let key = path_of(s, "key");
            key.starts_with("/library/streams/").then(|| ExternalSub {
                url: format!("{}{key}", a.server),
                title: match text_of(s, "displayTitle") {
                    t if t.is_empty() => "Subtitles".into(),
                    t => t,
                },
                language: text_of(s, "languageCode"),
            })
        })
        .collect()
}

/// Where the end credits start, as a Jellyfin-style segments reply so
/// [`crate::upnext::credits_start`] reads both servers the same way.
pub fn segments_json(body: &str) -> Option<String> {
    let v = parse_json(body.trim())?;
    let m = v
        .get("MediaContainer")?
        .get("Metadata")?
        .as_arr()?
        .first()?
        .clone();
    let start = m
        .get("Marker")?
        .as_arr()?
        .iter()
        .filter(|k| k.get("type").and_then(Json::as_str) == Some("credits"))
        .filter_map(|k| num(k, "startTimeOffset"))
        .reduce(f64::min)?;
    let ticks = (start / MS * crate::jellyfin::TICKS) as u64;
    Some(format!(
        "{{\"Items\":[{{\"Type\":\"Outro\",\"StartTicks\":{ticks}}}]}}"
    ))
}

/// Playing, paused or stopped at `seconds` (the server's Continue watching
/// and other Plex apps follow it).
pub fn timeline_url(a: &Account, item: &Item, state: &str, seconds: f64) -> String {
    let ms = (seconds.max(0.0) * MS) as u64;
    let dur = item.runtime.map_or(0, |r| (r * MS) as u64);
    let key = if key_ok(&item.id) {
        item.id.as_str()
    } else {
        "0"
    };
    format!(
        "{}/:/timeline?ratingKey={key}&key={}&state={state}&time={ms}&duration={dur}",
        a.server,
        q(&format!("/library/metadata/{key}"))
    )
}

/// Mark watched.
pub fn scrobble_url(a: &Account, key: &str) -> String {
    format!(
        "{}/:/scrobble?key={}&identifier=com.plexapp.plugins.library",
        a.server,
        if key_ok(key) { key } else { "0" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = "http://10.0.0.9:32400";

    fn acct() -> Account {
        Account {
            backend: Backend::Plex,
            server: S.into(),
            token: "tok_en-1".into(),
            user_id: "u1".into(),
            ..Account::default()
        }
    }

    fn fixture(name: &str) -> String {
        let path = format!("{}/testdata/plex/{name}.json", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path).expect("fixture")
    }

    #[test]
    fn headers_carry_the_token_only_when_there_is_one() {
        let h = header_text("dev1", Some("abc"));
        assert!(h.contains("X-Plex-Client-Identifier: dev1\n"));
        assert!(h.contains("X-Plex-Product: Catfyn\n"));
        assert!(h.contains("X-Plex-Token: abc\n"));
        assert!(!header_text("dev1", Some("")).contains("Token"));
        assert!(!header_text("dev1", None).contains("Token"));
    }

    #[test]
    fn link_code_and_token() {
        assert_eq!(
            parse_pin(r#"{"id":4242,"code":"A1B2","expiresIn":900,"authToken":null}"#),
            Some((4242, "A1B2".into()))
        );
        assert_eq!(parse_pin(r#"{"id":1,"code":"a b"}"#), None);
        assert_eq!(parse_pin_token(r#"{"id":4242,"authToken":null}"#), None);
        assert_eq!(
            parse_pin_token(r#"{"id":4242,"authToken":"xY_z-9"}"#),
            Some("xY_z-9".into())
        );
        assert_eq!(
            parse_user(r#"{"id":7,"uuid":"ab12cd","username":"kat"}"#),
            Some(("ab12cd".into(), "kat".into()))
        );
    }

    #[test]
    fn servers_and_their_addresses_best_first() {
        let body = r#"[
          {"name":"Phone","provides":"client,player","clientIdentifier":"p1","connections":[]},
          {"name":"Den","provides":"server","clientIdentifier":"abc123","accessToken":"srvtok","owned":true,
           "connections":[
             {"protocol":"https","address":"1.2.3.4","port":32400,"uri":"https://1-2-3-4.x.plex.direct:32400","local":false,"relay":false},
             {"protocol":"https","address":"10.0.0.9","port":32400,"uri":"https://10-0-0-9.x.plex.direct:32400","local":true,"relay":false},
             {"protocol":"https","uri":"https://relay.plex.direct:8443","local":false,"relay":true}
           ]}
        ]"#;
        let s = parse_resources(body);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "Den");
        assert_eq!(s[0].token, "srvtok");
        assert_eq!(
            s[0].addresses,
            [
                "https://10-0-0-9.x.plex.direct:32400",
                "https://1-2-3-4.x.plex.direct:32400",
                "https://relay.plex.direct:8443"
            ]
        );
        let a = account(&s[0], &s[0].addresses[0], "u9", "kat", "cloud1");
        assert_eq!(
            (a.backend, a.token.as_str(), a.server_id.as_str()),
            (Backend::Plex, "srvtok", "abc123")
        );
    }

    #[test]
    fn typed_addresses_get_plex_port() {
        assert_eq!(
            normalize_address("10.0.0.9").as_deref(),
            Some("http://10.0.0.9:32400")
        );
        assert_eq!(
            normalize_address("10.0.0.9:8000/").as_deref(),
            Some("http://10.0.0.9:8000")
        );
        assert_eq!(
            normalize_address("https://plex.example.org").as_deref(),
            Some("https://plex.example.org")
        );
        assert_eq!(normalize_address("bad address"), None);
    }

    #[test]
    fn libraries_from_sections() {
        let libs = video_libraries(parse_items(&fixture("sections"), S).unwrap());
        let got: Vec<(&str, &str, &str)> = libs
            .iter()
            .map(|l| (l.id.as_str(), l.name.as_str(), l.collection.as_str()))
            .collect();
        assert_eq!(
            got,
            [("1", "Movies", "movies"), ("2", "TV Shows", "tvshows")]
        );
        assert!(
            libs.iter()
                .all(|l| l.kind() == crate::jellyfin::Kind::Library)
        );
        assert_eq!(
            library_url(&acct(), "1"),
            format!("{S}/library/sections/1/all?sort=titleSort")
        );
        assert_eq!(
            library_url(&acct(), "1/../x"),
            format!("{S}/library/sections/0/all?sort=titleSort")
        );
    }

    #[test]
    fn films_with_pictures_and_files() {
        let films = parse_items(&fixture("movies"), S).unwrap();
        assert_eq!(films.len(), 2);
        let f = &films[0];
        assert_eq!(
            (f.name.as_str(), f.kind(), f.year),
            ("Big Buck Bunny", crate::jellyfin::Kind::Movie, Some(2008))
        );
        assert_eq!((f.server.as_str(), f.backend), (S, Backend::Plex));
        assert!(f.thumb.starts_with("/library/metadata/1/thumb/"));
        assert!(f.part.starts_with("/library/parts/"));
        assert!(f.runtime.is_some_and(|r| r > 100.0));
        assert!(f.has_primary);
        assert_eq!(
            stream_url(&acct(), &f.part).unwrap(),
            format!("{S}{}", f.part)
        );
        assert_eq!(stream_url(&acct(), "/etc/passwd"), None);
    }

    #[test]
    fn shows_seasons_and_episodes() {
        let shows = parse_items(&fixture("shows"), S).unwrap();
        assert_eq!(shows[0].kind(), crate::jellyfin::Kind::Series);
        let seasons = parse_items(&fixture("seasons"), S).unwrap();
        assert_eq!(
            seasons.iter().map(|s| s.season).collect::<Vec<_>>(),
            [Some(1), Some(2)]
        );
        assert_eq!(seasons[0].series_id, "4");
        let eps = parse_items(&fixture("episodes"), S).unwrap();
        assert_eq!(eps.len(), 3);
        let e = &eps[1];
        assert_eq!(
            (e.season, e.episode, e.series_id.as_str()),
            (Some(1), Some(2), "4")
        );
        assert_eq!(e.episode_label().as_deref(), Some("S1 · E2"));
        // The first one was marked watched on the test server.
        assert!(eps[0].played && !eps[1].played);
        assert_eq!(
            children_url(&acct(), "4"),
            format!("{S}/library/metadata/4/children")
        );
    }

    #[test]
    fn on_deck_splits_into_continue_and_next() {
        let deck = parse_items(&fixture("ondeck"), S).unwrap();
        assert_eq!(deck.len(), 1);
        let mut half = deck[0].clone();
        half.position = 40.0;
        half.played = false;
        // Watched to the end (Plex keeps its old position): not listed.
        let mut done = deck[0].clone();
        done.played = true;
        done.position = done.runtime.unwrap() - 5.0;
        let (cont, next) = split_on_deck(vec![half, deck[0].clone(), done]);
        assert_eq!((cont.len(), next.len()), (1, 1));
    }

    #[test]
    fn search_takes_films_and_shows_from_the_hubs() {
        let hits = parse_items(&fixture("search"), S).unwrap();
        assert_eq!(
            hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            ["Sintel"]
        );
        assert_eq!(
            search_url(&acct(), "a b", 5),
            format!("{S}/hubs/search?query=a%20b&limit=5")
        );
    }

    #[test]
    fn subtitle_files_and_the_file_to_play() {
        let body = fixture("sintel");
        let subs = parse_external_subs(&acct(), &body);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].url, format!("{S}/library/streams/7"));
        assert_eq!(
            (subs[0].title.as_str(), subs[0].language.as_str()),
            ("English", "eng")
        );
        assert!(parse_part(&body).is_some_and(|p| p.starts_with("/library/parts/2/")));
    }

    #[test]
    fn credits_marker_becomes_a_segment() {
        let body = r#"{"MediaContainer":{"Metadata":[{"ratingKey":"7","Marker":[
            {"type":"intro","startTimeOffset":1000,"endTimeOffset":30000},
            {"type":"credits","startTimeOffset":1320500,"endTimeOffset":1400000}]}]}}"#;
        let seg = segments_json(body).unwrap();
        assert_eq!(
            crate::upnext::credits_start(Some(&seg), Some(1400.0)),
            Some(1320.5)
        );
        assert_eq!(segments_json(&fixture("sintel")), None);
    }

    #[test]
    fn progress_watched_and_pictures() {
        let item = Item {
            id: "7".into(),
            runtime: Some(120.0),
            ..Item::default()
        };
        assert_eq!(
            timeline_url(&acct(), &item, "playing", 12.5),
            format!(
                "{S}/:/timeline?ratingKey=7&key=%2Flibrary%2Fmetadata%2F7&state=playing&time=12500&duration=120000"
            )
        );
        assert_eq!(
            scrobble_url(&acct(), "7"),
            format!("{S}/:/scrobble?key=7&identifier=com.plexapp.plugins.library")
        );
        let u = image_url(S, "/library/metadata/1/thumb/9", 300, 450, 0);
        assert_eq!(
            u,
            format!(
                "{S}/photo/:/transcode?width=300&height=450&minSize=1&upscale=1&format=png&url=%2Flibrary%2Fmetadata%2F1%2Fthumb%2F9"
            )
        );
        assert!(image_url(S, "/x", 1, 1, 6).ends_with("&blur=6"));
        assert!(!u.contains("tok"));
    }

    #[test]
    fn friendly_name() {
        assert_eq!(
            parse_friendly_name(r#"{"MediaContainer":{"friendlyName":"Den","claimed":true}}"#),
            Some("Den".into())
        );
        assert_eq!(parse_friendly_name(r#"{"MediaContainer":{}}"#), None);
    }

    #[test]
    fn identity() {
        assert_eq!(
            parse_identity(
                r#"{"MediaContainer":{"machineIdentifier":"59377ef4","claimed":false}}"#
            ),
            Some("59377ef4".into())
        );
    }
}
