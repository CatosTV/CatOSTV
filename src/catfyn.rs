//! Catfyn's network and playback jobs (run on background threads by
//! `catostv-shell`). The API and the screens are in `catostv_core::jellyfin`
//! and `catostv_core::fin`; this file only runs `curl` / `mpv` and turns the
//! answers into data.

use catostv_core::apps::{AppEntry, AppSource};
use catostv_core::fin::HomeData;
use catostv_core::jellyfin::{self as jf, Account, Item, Report, Server};
use catostv_core::mpv::{self, Arg};
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

/// GET (or POST `body`) with the account's token.
pub fn call(device: &str, a: &Account, url: &str, body: Option<&str>) -> Result<String, String> {
    let headers = jf::header_file(device, Some(&a.token))
        .map_err(|e| format!("Catfyn could not prepare the request ({e})"))?;
    run(&jf::request_cmd(url, &headers, body))
}

pub fn items(device: &str, a: &Account, url: &str) -> Result<Vec<Item>, String> {
    jf::parse_items(&call(device, a, url, None)?).map_err(|e| e.to_string())
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
    })
}

/// Everything the Browse screen shows. A failing optional row is left
/// empty; only an unreachable server is an error.
pub fn home(device: &str, a: &Account) -> Result<HomeData, String> {
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

/// CatosTV Home's rows (Continue watching, Movies, TV shows).
pub fn home_rows(device: &str, a: &Account) -> Vec<catostv_core::fin::Row> {
    home(device, a).map(|d| d.home_rows()).unwrap_or_default()
}

/// Download an image into the cache; returns the PNG path.
pub fn fetch_image(home: &Path, url: &str) -> Option<String> {
    let path = jf::image_cache_path(home, url);
    if path.is_file() {
        return Some(path.to_string_lossy().into_owned());
    }
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension("part");
    let ok = jf::image_cmd(url, &tmp)
        .ok()
        .is_some_and(|c| SystemRunner.run(&c).is_ok_and(|o| o.success()))
        && std::fs::rename(&tmp, &path).is_ok();
    if ok {
        Some(path.to_string_lossy().into_owned())
    } else {
        let _ = std::fs::remove_file(&tmp);
        None
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
    let config = jf::player_config(device, &a.token).map_err(|e| e.to_string())?;
    let start = if from_start { 0.0 } else { item.position };
    let title = play_title(item);
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
    // The user's subtitle mode and languages.
    let prefs = call(device, a, &jf::user_url(a), None)
        .map(|body| jf::parse_play_prefs(&body))
        .unwrap_or_default();
    let argv = jf::player_argv_full(
        &jf::stream_url(a, &item.id),
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
fn card_thumbnail(
    home: &Path,
    a: &Account,
    next: &Item,
    osd_width: f64,
) -> Option<(String, u32, u32)> {
    let scale = (osd_width / 1920.0).clamp(0.5, 4.0);
    let (w, h) = ((256.0 * scale) as u32, (144.0 * scale) as u32);
    let png = fetch_image(home, &jf::image_url(&a.server, &next.id, "Primary", 640))?;
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

/// The episode after `item` in series order, across seasons: not
/// Jellyfin's "Next up", which can jump back to an older unwatched one.
fn next_episode(device: &str, a: &Account, item: &Item) -> Option<Item> {
    if item.kind() != jf::Kind::Episode || item.series_id.is_empty() {
        return None;
    }
    let list = items(device, a, &jf::series_episodes_url(a, &item.series_id)).ok()?;
    catostv_core::upnext::next_in_order(&list, &item.id)
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
    let _ = conn.command(&[
        Arg::Str("set_property"),
        Arg::Str("keep-open"),
        Arg::Str("no"),
    ]);
    conn.command(&[
        Arg::Str("loadfile"),
        Arg::Str(&jf::stream_url(a, &next.id)),
        Arg::Str("replace"),
        Arg::Num(-1.0),
        Arg::Str(&opts),
    ])
    .map_err(|e| e.to_string())?;
    Ok(call(
        device,
        a,
        &jf::playback_info_url(a, &next.id),
        Some(&jf::playback_info_body(a)),
    )
    .map(|body| jf::parse_external_subs(a, &body))
    .unwrap_or_default())
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
                let (url, body) = jf::report(&a, kind, &item.id, pos, paused);
                let _ = call(&device, &a, &url, Some(&body));
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
                let next = autoplay
                    .enabled
                    .then(|| next_episode(&device, &a, &item))
                    .flatten();
                let segments = next
                    .as_ref()
                    .and_then(|_| call(&device, &a, &jf::segments_url(&a, &item.id), None).ok());
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
                                    thumb = card_thumbnail(&home, &a, n, w);
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
                                let _ =
                                    call(&device, &a, &jf::played_url(&a, &item.id), Some("{}"));
                                send(&item, Report::Stopped, last.0, false);
                                card(&mut conn, &["hide"]);
                                let n = call(&device, &a, &jf::item_url(&a, &n.id), None)
                                    .ok()
                                    .and_then(|b| jf::parse_single_item(&b))
                                    .unwrap_or(n);
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
