//! Catfyn's network and playback jobs (run on background threads by
//! `catostv-shell`). The APIs and the screens are in `catostv_core::jellyfin`,
//! `catostv_core::plex` and `catostv_core::fin`; this file only runs `curl` /
//! `mpv` and turns the answers into data, for Jellyfin and Plex alike (each
//! account says which it is).

use catostv_core::apps::{AppEntry, AppSource};
use catostv_core::fin::HomeData;
use catostv_core::jellyfin::{self as jf, Account, Backend, Item, Report, Server};
use catostv_core::mpv::{self, Arg};
use catostv_core::plex;
use catostv_core::process::{Runner, SystemRunner};
use std::path::Path;
use std::time::{Duration, Instant};

/// Id of the player in the app manager (and its Wayland app id).
pub const PLAYER_ID: &str = "catfyn-player";

fn run(cmd: &catostv_core::process::Cmd) -> Result<String, String> {
    match SystemRunner.run(cmd) {
        Ok(o) if o.success() => Ok(o.stdout),
        Ok(o) => Err(jf::friendly_error(&format!("{} {}", o.stderr, o.stdout))),
        Err(e) => Err(jf::friendly_error(&e.to_string())),
    }
}

fn headers(device: &str, a: &Account) -> Result<std::path::PathBuf, String> {
    match a.backend {
        Backend::Jellyfin => jf::header_file(device, Some(&a.token)),
        Backend::Plex => plex::header_file(device, Some(&a.token)),
    }
    .map_err(|e| format!("Catfyn could not prepare the request ({e})"))
}

/// GET (or POST `body`) with the account's token.
pub fn call(device: &str, a: &Account, url: &str, body: Option<&str>) -> Result<String, String> {
    run(&jf::request_cmd(url, &headers(device, a)?, body))
}

/// Items of a list request, marked with the server they came from.
pub fn items(device: &str, a: &Account, url: &str) -> Result<Vec<Item>, String> {
    let body = call(device, a, url, None)?;
    let mut v = match a.backend {
        Backend::Jellyfin => jf::parse_items(&body),
        Backend::Plex => plex::parse_items(&body, &a.server),
    }
    .map_err(|e| e.to_string())?;
    for it in &mut v {
        it.server = a.server.clone();
        it.backend = a.backend;
    }
    Ok(v)
}

/// One item with the user's own data (position, watched), when the server
/// answers.
fn fresh(device: &str, a: &Account, item: &Item) -> Option<Item> {
    let url = match a.backend {
        Backend::Jellyfin => jf::item_url(a, &item.id),
        Backend::Plex => plex::item_url(a, &item.id),
    };
    items(device, a, &url).ok()?.into_iter().next()
}

pub fn login(device: &str, server: &Server, user: &str, password: &str) -> Result<Account, String> {
    let headers = jf::header_file(device, None).map_err(|e| e.to_string())?;
    let reply = run(&jf::request_cmd(
        &jf::login_url(&server.address),
        &headers,
        Some(&jf::login_body(user, password)),
    ))?;
    let (user_id, user_name, token) =
        jf::parse_login(&reply).ok_or_else(|| "The server did not accept that".to_string())?;
    Ok(Account {
        server: server.address.clone(),
        server_name: server.name.clone(),
        user_id,
        user_name,
        token,
        incognito: false,
        ..Account::default()
    })
}

// ------------------------------------------------------------- Plex sign-in

/// A new code to enter at plex.tv/link: (pin id, code).
pub fn plex_pin(device: &str) -> Result<(u64, String), String> {
    let h = plex::header_file(device, None).map_err(|e| e.to_string())?;
    let body = run(&jf::request_cmd(&plex::pin_url(), &h, Some("strong=false")))?;
    plex::parse_pin(&body).ok_or_else(|| "Plex did not give a code".into())
}

/// The plex.tv token once the code was entered (`None`: not yet).
pub fn plex_check(device: &str, pin: u64) -> Result<Option<String>, String> {
    let h = plex::header_file(device, None).map_err(|e| e.to_string())?;
    let body = run(&jf::request_cmd(&plex::pin_check_url(pin), &h, None))?;
    Ok(plex::parse_pin_token(&body))
}

/// Who signed in and the servers they can use.
pub fn plex_servers(
    device: &str,
    cloud_token: &str,
) -> Result<(String, String, Vec<plex::PlexServer>), String> {
    let h = plex::header_file(device, Some(cloud_token)).map_err(|e| e.to_string())?;
    let user = run(&jf::request_cmd(&plex::user_url(), &h, None))?;
    let (id, name) =
        plex::parse_user(&user).ok_or_else(|| "Plex did not say who you are".to_string())?;
    let list = run(&jf::request_cmd(&plex::resources_url(), &h, None))?;
    Ok((id, name, plex::parse_resources(&list)))
}

/// A quick request (a few seconds): is a server reachable this way?
fn probe(url: &str, headers: &Path) -> Result<String, String> {
    let mut cmd = jf::request_cmd(url, headers, None);
    if let Some(i) = cmd.args.iter().position(|a| a == "-m") {
        cmd.args[i + 1] = "4".into();
    }
    cmd = cmd.timeout(Duration::from_secs(6));
    run(&cmd)
}

/// Use a plex.tv server: the first of its addresses that answers.
pub fn plex_connect(
    device: &str,
    server: &plex::PlexServer,
    user_id: &str,
    user_name: &str,
    cloud_token: &str,
) -> Result<Account, String> {
    let h = plex::header_file(device, Some(&server.token)).map_err(|e| e.to_string())?;
    for address in &server.addresses {
        if probe(&plex::identity_url(address), &h).is_ok() {
            return Ok(plex::account(
                server,
                address,
                user_id,
                user_name,
                cloud_token,
            ));
        }
    }
    Err(format!("Can't reach {}", server.name))
}

/// A Plex server typed in by address, used without a sign-in (it must let
/// the home network in: Plex → Settings → Network → "List of IP addresses
/// and networks that are allowed without auth").
pub fn plex_local(device: &str, address: &str) -> Result<Account, String> {
    let h = plex::header_file(device, None).map_err(|e| e.to_string())?;
    let id = probe(&plex::identity_url(address), &h)
        .ok()
        .and_then(|b| plex::parse_identity(&b))
        .ok_or_else(|| "Can't reach a Plex server there".to_string())?;
    let root = probe(&format!("{address}/"), &h)
        .map_err(|_| "This server needs you to sign in with Plex".to_string())?;
    let name = catostv_core::mpv::parse_json(root.trim())
        .and_then(|v| {
            v.get("MediaContainer")?
                .get("friendlyName")?
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Plex".into());
    Ok(plex::local_account(&Server {
        id,
        name,
        address: address.to_string(),
    }))
}

// ------------------------------------------------------------------ browse

/// Everything the Browse screen shows. A failing optional row is left
/// empty; only an unreachable server is an error.
pub fn home(device: &str, a: &Account) -> Result<HomeData, String> {
    match a.backend {
        Backend::Jellyfin => {
            let libraries = jf::video_libraries(items(device, a, &jf::views_url(a))?);
            let resume = items(device, a, &jf::resume_url(a, 16)).unwrap_or_default();
            let next_up = items(device, a, &jf::next_up_url(a, 16)).unwrap_or_default();
            let latest = libraries
                .iter()
                .take(5)
                .map(|l| {
                    (
                        l.name.clone(),
                        items(device, a, &jf::latest_url(a, &l.id, 16)).unwrap_or_default(),
                    )
                })
                .collect();
            Ok(HomeData {
                resume,
                next_up,
                libraries,
                latest,
            })
        }
        Backend::Plex => {
            let libraries = plex::video_libraries(items(device, a, &plex::sections_url(a))?);
            let deck = items(device, a, &plex::on_deck_url(a, 24)).unwrap_or_default();
            let (resume, next_up) = plex::split_on_deck(deck);
            let latest = libraries
                .iter()
                .take(5)
                .map(|l| {
                    (
                        l.name.clone(),
                        items(device, a, &plex::recent_url(a, &l.id, 16)).unwrap_or_default(),
                    )
                })
                .collect();
            Ok(HomeData {
                resume,
                next_up,
                libraries,
                latest,
            })
        }
    }
}

pub fn library(device: &str, a: &Account, lib: &str) -> Result<Vec<Item>, String> {
    match a.backend {
        Backend::Jellyfin => items(device, a, &jf::library_url(a, lib)),
        Backend::Plex => items(device, a, &plex::library_url(a, lib)),
    }
}

pub fn seasons(device: &str, a: &Account, series: &str) -> Result<Vec<Item>, String> {
    match a.backend {
        Backend::Jellyfin => items(device, a, &jf::seasons_url(a, series)),
        Backend::Plex => items(device, a, &plex::children_url(a, series)),
    }
}

pub fn episodes(
    device: &str,
    a: &Account,
    series: &str,
    season: &str,
) -> Result<Vec<Item>, String> {
    match a.backend {
        Backend::Jellyfin => items(device, a, &jf::episodes_url(a, series, season)),
        Backend::Plex => items(device, a, &plex::children_url(a, season)),
    }
}

/// Films and shows whose name matches (CatosTV Search).
pub fn search(device: &str, a: &Account, term: &str, limit: u32) -> Result<Vec<Item>, String> {
    match a.backend {
        Backend::Jellyfin => items(device, a, &jf::search_url(a, term, limit)),
        Backend::Plex => items(device, a, &plex::search_url(a, term, limit)),
    }
}

/// CatosTV Home's rows (Continue watching, Movies, TV shows) from every
/// home account, Jellyfin and Plex side by side: each row takes its items
/// in turn from each server.
/// Home's rows from every account, and the servers that did not answer
/// (backend, name).
pub fn home_rows(
    device: &str,
    accounts: &[Account],
) -> (Vec<catostv_core::fin::Row>, Vec<(jf::Backend, String)>) {
    let mut failed = Vec::new();
    let per: Vec<Vec<catostv_core::fin::Row>> = accounts
        .iter()
        .map(|a| match home(device, a) {
            Ok(d) => d.home_rows(),
            Err(_) => {
                let name = if a.server_name.is_empty() {
                    &a.server
                } else {
                    &a.server_name
                };
                failed.push((a.backend, name.clone()));
                Vec::new()
            }
        })
        .collect();
    (catostv_core::fin::merge_rows(per), failed)
}

// ------------------------------------------------------------------ pictures

/// Plex servers need the token for pictures too; the shell keeps this list
/// up to date (`remember_accounts`) so a picture's address alone is enough.
static PLEX_SIGN_INS: std::sync::Mutex<Vec<(String, String, String)>> =
    std::sync::Mutex::new(Vec::new());

/// The Plex servers' addresses and tokens, for [`fetch_image`].
pub fn remember_accounts(accounts: &jf::Accounts) {
    let list = accounts
        .list
        .iter()
        .filter(|a| a.backend == Backend::Plex)
        .map(|a| {
            (
                a.server.clone(),
                accounts.device_id.clone(),
                a.token.clone(),
            )
        })
        .collect();
    *PLEX_SIGN_INS.lock().unwrap_or_else(|e| e.into_inner()) = list;
}

fn plex_headers_for(url: &str) -> Option<std::path::PathBuf> {
    let list = PLEX_SIGN_INS.lock().unwrap_or_else(|e| e.into_inner());
    let (_, device, token) = list
        .iter()
        .find(|(server, _, _)| url.starts_with(&format!("{server}/")))?;
    plex::header_file(device, Some(token)).ok()
}

/// A softened copy of a picture: scaled down and blurred (YouTube's
/// thumbnails behind a focused video; Jellyfin and Plex blur on the server).
pub fn blur_into(src: &Path, out: &Path) -> Option<()> {
    blur_cropped(src, out, false)
}

/// [`blur_into`]; `letterboxed`: a YouTube picture, 4:3 with the video
/// between black bars, whose middle 16:9 is kept (the bars would show as
/// dark bands across the screen).
fn blur_cropped(src: &Path, out: &Path, letterboxed: bool) -> Option<()> {
    let mut img = image::load_from_memory(&std::fs::read(src).ok()?).ok()?;
    let (w, h) = (img.width(), img.height());
    if letterboxed && w > 0 && h * 16 > w * 9 + w / 10 {
        let keep = w * 9 / 16;
        img = img.crop_imm(0, (h - keep) / 2, w, keep);
    }
    let small = img.resize(480, 270, image::imageops::FilterType::Triangle);
    let soft = image::imageops::blur(&small.to_rgba8(), 7.0);
    soft.save_with_format(out, image::ImageFormat::Png).ok()
}

/// Download an image into the cache; returns the PNG path. An address
/// ending in `#blur` gives the blurred copy.
pub fn fetch_image(home: &Path, url: &str) -> Option<String> {
    if let Some(base) = url.strip_suffix("#blur") {
        let path = jf::image_cache_path(home, url);
        if path.is_file() {
            return Some(path.to_string_lossy().into_owned());
        }
        let src = fetch_image(home, base)?;
        blur_cropped(
            Path::new(&src),
            &path,
            base.starts_with("https://i.ytimg.com/"),
        )?;
        return Some(path.to_string_lossy().into_owned());
    }
    let path = jf::image_cache_path(home, url);
    if path.is_file() {
        return Some(path.to_string_lossy().into_owned());
    }
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    // Each download its own partial file: a card and its blurred backdrop
    // can ask for the same picture at once.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("{n}.part"));
    let cmd = jf::image_cmd(url, &tmp).ok().map(|mut c| {
        if let Some(h) = plex_headers_for(url) {
            c.args.insert(0, format!("@{}", h.display()));
            c.args.insert(0, "-H".into());
        }
        c
    });
    let ok = cmd.is_some_and(|c| SystemRunner.run(&c).is_ok_and(|o| o.success()))
        && std::fs::rename(&tmp, &path).is_ok();
    if ok {
        Some(path.to_string_lossy().into_owned())
    } else {
        let _ = std::fs::remove_file(&tmp);
        None
    }
}

// ------------------------------------------------------------------ playback

/// The address of the file to play, and the server's subtitle files.
fn stream_and_subs(
    device: &str,
    a: &Account,
    item: &Item,
) -> Result<(String, Vec<jf::ExternalSub>), String> {
    match a.backend {
        Backend::Jellyfin => {
            // Subtitle files the server keeps next to the video: one
            // quick request; without an answer the embedded tracks still work.
            let subs = call(
                device,
                a,
                &jf::playback_info_url(a, &item.id),
                Some(&jf::playback_info_body(a)),
            )
            .map(|body| jf::parse_external_subs(a, &body))
            .unwrap_or_default();
            Ok((jf::stream_url(a, &item.id), subs))
        }
        Backend::Plex => {
            // The item itself: its file and subtitle streams.
            let body = call(device, a, &plex::item_url(a, &item.id), None)?;
            let part = plex::parse_part(&body)
                .or_else(|| Some(item.part.clone()).filter(|p| !p.is_empty()))
                .ok_or_else(|| "Plex has no file for this".to_string())?;
            let url = plex::stream_url(a, &part)
                .ok_or_else(|| "Plex has no file for this".to_string())?;
            Ok((url, plex::parse_external_subs(a, &body)))
        }
    }
}

/// The app-manager entry that plays `item` in mpv.
pub fn player_entry(
    device: &str,
    a: &Account,
    item: &Item,
    from_start: bool,
) -> Result<AppEntry, String> {
    let (r, g, b) = crate::theme::accent_rgb();
    player_entry_with(
        device,
        a,
        item,
        from_start,
        &format!("{r:02x}{g:02x}{b:02x}"),
    )
}

/// [`player_entry`] with the control bar's accent (`RRGGBB`).
pub fn player_entry_with(
    device: &str,
    a: &Account,
    item: &Item,
    from_start: bool,
    accent: &str,
) -> Result<AppEntry, String> {
    let config = match a.backend {
        Backend::Jellyfin => jf::player_config(device, &a.token),
        Backend::Plex => jf::player_config_with(plex::player_header(&a.token).as_deref()),
    }
    .map_err(|e| e.to_string())?;
    let start = if from_start { 0.0 } else { item.position };
    let title = play_title(item);
    let (url, subs) = stream_and_subs(device, a, item)?;
    // The user's subtitle mode and languages (Jellyfin keeps them per user;
    // Plex players choose their own, so mpv's defaults apply).
    let prefs = match a.backend {
        Backend::Jellyfin => call(device, a, &jf::user_url(a), None)
            .map(|body| jf::parse_play_prefs(&body))
            .unwrap_or_default(),
        Backend::Plex => jf::PlayPrefs::default(),
    };
    let argv = jf::player_argv_full(
        &url,
        &title,
        start,
        &config,
        &jf::player_socket(),
        accent,
        &subs,
        &prefs,
    );
    Ok(AppEntry {
        id: PLAYER_ID.into(),
        name: "Catfyn".into(),
        subtitle: String::new(),
        icon: "media".into(),
        accent: "violet".into(),
        command: catostv_core::desktop::join_command(&argv),
        singleton: true,
        icon_path: None,
        source: AppSource::Configured,
    })
}

/// Tell the server where playback is (Jellyfin's session reports, Plex's
/// timeline).
fn report(device: &str, a: &Account, item: &Item, kind: Report, pos: f64, paused: bool) {
    match a.backend {
        Backend::Jellyfin => {
            let (url, body) = jf::report(a, kind, &item.id, pos, paused);
            let _ = call(device, a, &url, Some(&body));
        }
        Backend::Plex => {
            let state = match (kind, paused) {
                (Report::Stopped, _) => "stopped",
                (_, true) => "paused",
                _ => "playing",
            };
            let _ = call(device, a, &plex::timeline_url(a, item, state, pos), None);
        }
    }
}

/// Mark an item watched.
fn mark_watched(device: &str, a: &Account, item: &Item) {
    let _ = match a.backend {
        Backend::Jellyfin => call(device, a, &jf::played_url(a, &item.id), Some("{}")),
        Backend::Plex => call(device, a, &plex::scrobble_url(a, &item.id), None),
    };
}

/// Every episode of the item's show in order (for Previous / Next).
fn series_episodes(device: &str, a: &Account, series: &str) -> Result<Vec<Item>, String> {
    match a.backend {
        Backend::Jellyfin => items(device, a, &jf::series_episodes_url(a, series)),
        Backend::Plex => items(device, a, &plex::all_episodes_url(a, series)),
    }
}

/// Where the end credits start, as segments for `upnext::credits_start`.
fn segments(device: &str, a: &Account, item: &Item) -> Option<String> {
    match a.backend {
        Backend::Jellyfin => call(device, a, &jf::segments_url(a, &item.id), None).ok(),
        Backend::Plex => call(device, a, &plex::item_url(a, &item.id), None)
            .ok()
            .and_then(|b| plex::segments_json(&b)),
    }
}

/// A volume key while a film plays: change the volume and have the
/// player's control bar draw the panel (`catfyn-volume PCT MUTED OUTPUT
/// MORE`, see catfyn-osd.lua). `volume-outputs` is not handled here (the
/// list is the shell's own). False when the player does not answer: the
/// shell's panel shows instead.
pub fn player_volume(cmd: catostv_core::ipc::ShellCommand) -> bool {
    use catostv_core::audio;
    use catostv_core::ipc::ShellCommand;
    use catostv_core::volume_panel::{STEP, sink_label};
    // One change at a time, so quick presses land in order.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let socket = jf::player_socket();
    // The player answers first: the volume changes only once.
    if mpv::ipc_request(&socket, &[Arg::Str("client_name")]).is_err() {
        return false;
    }
    let mut r = SystemRunner;
    let state = match cmd {
        ShellCommand::VolumeUp => audio::step(&mut r, STEP),
        ShellCommand::VolumeDown => audio::step(&mut r, -STEP),
        ShellCommand::Mute => audio::toggle_mute(&mut r),
        _ => return false,
    };
    let Ok(v) = state else {
        return false;
    };
    let sinks = audio::sinks(&mut r).unwrap_or_default();
    let output = sinks
        .iter()
        .position(|s| s.is_default)
        .map(|i| sink_label(&sinks, i))
        .unwrap_or_default();
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    let _ = mpv::ipc_request(
        &socket,
        &[
            Arg::Str("script-message-to"),
            Arg::Str("catfyn_osd"),
            Arg::Str("catfyn-volume"),
            Arg::Str(&v.percent.to_string()),
            Arg::Str(yes_no(v.muted)),
            Arg::Str(&output),
            Arg::Str(yes_no(sinks.len() > 1)),
        ],
    );
    true
}

/// "Series · S1 · E3 · Episode name", or the movie's name.
fn play_title(item: &Item) -> String {
    match item.episode_label() {
        Some(l) if !item.series_name.is_empty() => {
            format!("{} · {l} · {}", item.series_name, item.name)
        }
        _ => item.name.clone(),
    }
}

/// Settings → Catfyn: play the next episode when the credits start, and
/// how long the card counts down.
#[derive(Debug, Clone, Copy)]
pub struct Autoplay {
    pub enabled: bool,
    pub countdown: u32,
}

/// The next episode's thumbnail as raw BGRA for mpv's `overlay-add`, sized
/// for the card at the player's real resolution (the card is laid out on a
/// 1920×1080 canvas). `(path, width, height)`.
fn card_thumbnail(home: &Path, next: &Item, osd_width: f64) -> Option<(String, u32, u32)> {
    let scale = (osd_width / 1920.0).clamp(0.5, 4.0);
    let (w, h) = ((256.0 * scale) as u32, (144.0 * scale) as u32);
    let png = fetch_image(home, &jf::still_url(next))?;
    let img = image::load_from_memory(&std::fs::read(png).ok()?).ok()?;
    let mut px = img
        .resize_to_fill(w, h, image::imageops::FilterType::Triangle)
        .to_rgba8()
        .into_raw();
    for p in px.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
    let path = jf::player_socket().with_file_name("catfyn-next.bgra");
    std::fs::write(&path, px).ok()?;
    Some((path.to_string_lossy().into_owned(), w, h))
}

/// The episodes before and after `item` in series order, across seasons
/// not Jellyfin's "Next up", which can jump back to an older
/// unwatched one. One request for both.
fn neighbours(device: &str, a: &Account, item: &Item) -> (Option<Item>, Option<Item>) {
    if item.kind() != jf::Kind::Episode || item.series_id.is_empty() {
        return (None, None);
    }
    let Ok(list) = series_episodes(device, a, &item.series_id) else {
        return (None, None);
    };
    (
        catostv_core::upnext::prev_in_order(&list, &item.id),
        catostv_core::upnext::next_in_order(&list, &item.id),
    )
}

/// Tell the control bar what to draw (`catfyn-card …`).
fn card(conn: &mut mpv::IpcConn, args: &[&str]) {
    let mut v = vec![
        Arg::Str("script-message-to"),
        Arg::Str("catfyn_osd"),
        Arg::Str("catfyn-card"),
    ];
    v.extend(args.iter().map(|a| Arg::Str(a)));
    let _ = conn.command(&v);
}

/// Start `next` in the running player (same window, no trip to Home): the
/// stream with its title and resume position, then, once it has loaded,
/// the server's external subtitle files. Returns the subtitle tracks to add.
fn load_in_player(
    conn: &mut mpv::IpcConn,
    device: &str,
    a: &Account,
    next: &Item,
) -> Result<Vec<jf::ExternalSub>, String> {
    let opts = format!(
        "start={:.1},force-media-title={}",
        next.position.max(0.0),
        mpv::option_value(&play_title(next))
    );
    let (url, subs) = stream_and_subs(device, a, next)?;
    let _ = conn.command(&[
        Arg::Str("set_property"),
        Arg::Str("keep-open"),
        Arg::Str("no"),
    ]);
    conn.command(&[
        Arg::Str("loadfile"),
        Arg::Str(&url),
        Arg::Str("replace"),
        Arg::Num(-1.0),
        Arg::Str(&opts),
    ])
    .map_err(|e| e.to_string())?;
    Ok(subs)
}

/// Report playback to the server every 10 s and run the "Next episode"
/// card until mpv goes away: at the credits the card counts down
/// and the next episode plays in the same player (the finished one is
/// marked watched first); after three episodes in a row with no key
/// pressed it asks "Are you still watching?". Runs on its own thread;
/// `done` is called once the player has gone (the shell then refreshes its
/// rows and returns to the show page).
pub fn spawn_reporter(
    device: String,
    a: Account,
    item: Item,
    home: std::path::PathBuf,
    autoplay: Autoplay,
    done: impl FnOnce(Option<Item>) + Send + 'static,
) {
    use catostv_core::upnext::{self, Action, Event, UpNext};
    let _ = std::thread::Builder::new()
        .name("catfyn-report".into())
        .spawn(move || {
            let socket = jf::player_socket();
            let send = |item: &Item, kind: Report, pos: f64, paused: bool| {
                report(&device, &a, item, kind, pos, paused);
            };
            // Wait for mpv to answer (up to 30 s).
            let started = Instant::now();
            let mut conn = loop {
                if let Ok(mut c) = mpv::IpcConn::connect(&socket)
                    && c.get_f64("time-pos").is_some()
                {
                    break c;
                }
                if started.elapsed() > Duration::from_secs(30) {
                    done(None);
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            };
            let clock = Instant::now();
            let mut item = item;
            let mut streak = 0u32;
            let mut subs_to_add: Vec<jf::ExternalSub> = Vec::new();
            let mut last: (f64, bool);
            'episodes: loop {
                // The episode has loaded (time-pos answers).
                let load_start = Instant::now();
                last = loop {
                    if let Some(p) = conn.get_f64("time-pos") {
                        break (p, conn.get_bool("pause").unwrap_or(false));
                    }
                    if conn.closed() || load_start.elapsed() > Duration::from_secs(30) {
                        done(None);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                };
                if !subs_to_add.is_empty() {
                    let names: Vec<String> = subs_to_add.iter().map(jf::sub_display_name).collect();
                    for sub in subs_to_add.drain(..) {
                        let _ = conn.command(&[
                            Arg::Str("sub-add"),
                            Arg::Str(&sub.url),
                            Arg::Str("auto"),
                            Arg::Str(&sub.title),
                            Arg::Str(&sub.language),
                        ]);
                    }
                    let _ = conn.command(&[
                        Arg::Str("script-message-to"),
                        Arg::Str("catfyn_osd"),
                        Arg::Str("catfyn-subs"),
                        Arg::Str(&names.join(";")),
                    ]);
                }
                send(&item, Report::Start, last.0, last.1);
                // The card's plan: the next episode in order, and where the
                // credits start (server segments, else the last 30 s).
                let (prev, following) = neighbours(&device, &a, &item);
                let next = following.clone().filter(|_| autoplay.enabled);
                // The control bar's Previous / Next episode buttons show only
                // when there is one.
                let yes_no = |o: &Option<Item>| if o.is_some() { "yes" } else { "no" };
                let _ = conn.command(&[
                    Arg::Str("script-message-to"),
                    Arg::Str("catfyn_osd"),
                    Arg::Str("catfyn-episodes"),
                    Arg::Str(yes_no(&prev)),
                    Arg::Str(yes_no(&following)),
                ]);
                let segments = next.as_ref().and_then(|_| segments(&device, &a, &item));
                if next.is_some() {
                    // Stay on the last frame at the end: the card decides.
                    let _ = conn.command(&[
                        Arg::Str("set_property"),
                        Arg::Str("keep-open"),
                        Arg::Str("yes"),
                    ]);
                }
                let mut up: Option<UpNext> = None;
                let mut thumb: Option<(String, u32, u32)> = None;
                let mut last_sent = Instant::now();
                loop {
                    let msgs = conn.messages(Duration::from_millis(500));
                    let pos = conn.get_f64("time-pos");
                    let paused = conn.get_bool("pause").unwrap_or(false);
                    let eof = conn.get_bool("eof-reached").unwrap_or(false);
                    if conn.closed() {
                        break 'episodes;
                    }
                    if let Some(p) = pos {
                        last = (p, paused);
                    }
                    // "Previous episode" on the control bar: the one before
                    // starts here, from the beginning (where this one was is
                    // reported first).
                    // "Next episode" plays the following one the same way, from
                    // where it was left (usually the start).
                    let wants = |which: &str| {
                        msgs.iter().any(|m| {
                            m.first().map(String::as_str) == Some("catfyn-episode")
                                && m.get(1).map(String::as_str) == Some(which)
                        })
                    };
                    let jump = match (wants("prev"), wants("next")) {
                        (true, _) => prev.clone().map(|p| (p, true)),
                        (_, true) => following.clone().map(|n| (n, false)),
                        _ => None,
                    };
                    if let Some((p, from_start)) = jump {
                        send(&item, Report::Stopped, last.0, false);
                        card(&mut conn, &["hide"]);
                        let mut p = fresh(&device, &a, &p).unwrap_or(p);
                        if from_start {
                            p.position = 0.0;
                        }
                        match load_in_player(&mut conn, &device, &a, &p) {
                            Ok(subs) => {
                                subs_to_add = subs;
                                item = p;
                                streak = 0;
                                continue 'episodes;
                            }
                            Err(_) => {
                                let _ = conn.command(&[Arg::Str("quit")]);
                                done(None);
                                return;
                            }
                        }
                    }
                    if up.is_none() && next.is_some() {
                        let runtime = conn
                            .get_f64("duration")
                            .filter(|d| *d > 0.0)
                            .or(item.runtime);
                        if runtime.is_some() {
                            up = Some(UpNext::new(
                                upnext::credits_start(segments.as_deref(), runtime),
                                autoplay.countdown,
                                streak,
                            ));
                        }
                    }
                    let mut actions = Vec::new();
                    if let Some(u) = up.as_mut() {
                        for m in &msgs {
                            let ev =
                                match (m.first().map(String::as_str), m.get(1).map(String::as_str))
                                {
                                    (Some("catfyn-input"), _) => Some(Event::Input),
                                    (Some("catfyn-card-result"), Some("play")) => {
                                        Some(Event::PlayNow)
                                    }
                                    (Some("catfyn-card-result"), Some("cancel")) => {
                                        Some(Event::Cancel)
                                    }
                                    (Some("catfyn-card-result"), Some("continue")) => {
                                        Some(Event::Continue)
                                    }
                                    (Some("catfyn-card-result"), Some("stop")) => Some(Event::Stop),
                                    _ => None,
                                };
                            actions.extend(ev.and_then(|e| u.handle(e)));
                        }
                        if let Some(p) = pos {
                            let now = clock.elapsed().as_secs_f64();
                            actions.extend(u.handle(Event::Tick { pos: p, now }));
                        }
                        if eof {
                            actions.extend(u.handle(Event::Ended));
                        }
                    } else if eof && next.is_some() {
                        let _ = conn.command(&[Arg::Str("quit")]);
                    }
                    for act in actions {
                        let label = next.as_ref().map(upnext::card_label).unwrap_or_default();
                        match act {
                            Action::ShowCard { .. } | Action::AskStillWatching => {
                                if thumb.is_none()
                                    && let Some(n) = &next
                                {
                                    let w = conn.get_f64("osd-width").unwrap_or(1920.0);
                                    thumb = card_thumbnail(&home, n, w);
                                }
                                let mut args: Vec<String> = match act {
                                    Action::ShowCard { left } => {
                                        vec!["next".into(), label.clone(), left.to_string()]
                                    }
                                    _ => vec!["still".into(), label.clone()],
                                };
                                if let Some((p, w, h)) = &thumb {
                                    args.extend([p.clone(), w.to_string(), h.to_string()]);
                                }
                                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                                card(&mut conn, &args);
                            }
                            Action::Count { left } => {
                                card(&mut conn, &["count", &left.to_string()])
                            }
                            Action::Hide => card(&mut conn, &["hide"]),
                            Action::Quit => {
                                card(&mut conn, &["hide"]);
                                let _ = conn.command(&[Arg::Str("quit")]);
                            }
                            Action::PlayNext { .. } => {
                                let Some(n) = next.clone() else { continue };
                                streak = up.as_ref().map_or(0, |u| u.streak);
                                // The finished one is watched (as at any end),
                                // then the next one starts here.
                                mark_watched(&device, &a, &item);
                                send(&item, Report::Stopped, last.0, false);
                                card(&mut conn, &["hide"]);
                                let n = fresh(&device, &a, &n).unwrap_or(n);
                                match load_in_player(&mut conn, &device, &a, &n) {
                                    Ok(subs) => {
                                        subs_to_add = subs;
                                        item = n;
                                        continue 'episodes;
                                    }
                                    Err(_) => {
                                        let _ = conn.command(&[Arg::Str("quit")]);
                                        done(None);
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    if last_sent.elapsed() >= Duration::from_secs(10) && pos.is_some() {
                        send(&item, Report::Progress, last.0, last.1);
                        last_sent = Instant::now();
                    }
                }
            }
            send(&item, Report::Stopped, last.0, false);
            done(None);
        });
}
