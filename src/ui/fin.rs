//! Drawing for Catfyn (CatosTV's Jellyfin client). Pure presentation of
//! `catostv_core::fin::Fin`; pictures come from the server through the
//! shell's image cache (`FinPics`).
//!
//! Look (Abyss-style): a full-bleed, softly
//! blurred backdrop of whatever has focus, cross-faded when it changes and
//! darkened with smooth gradients so the spotlight text reads; a frosted
//! pill tab bar whose accent pill slides with focus; rows of 24 px-rounded
//! cards that lift and glow under focus, fading up one after another.

use crate::canvas::Canvas;
use crate::icons;
use crate::images::Images;
use crate::theme::{self, Weight};
use crate::widgets::{self, ButtonStyle, ListRow};
use catostv_core::fin::{self, Fin, Screen, SignInStep};
use catostv_core::installer::Scan;
use catostv_core::jellyfin::{self as jf, Accounts, Item, Kind};
use catostv_core::prefs::UiPrefs;
use catostv_core::settings::RowKind;
use catostv_core::shell::Target;
use eframe::egui::{Align2, Color32, TextureId};
use std::cell::RefCell;
use std::collections::HashMap;

type C = Canvas<Target>;

/// Server pictures: URL → cached PNG (`None` = failed), and the URLs the
/// frame wanted but did not have (the shell downloads them).
pub struct FinPics<'a> {
    pub images: &'a Images,
    pub cache: &'a HashMap<String, Option<String>>,
    pub wanted: &'a RefCell<Vec<String>>,
}

impl FinPics<'_> {
    pub fn get(&self, c: &C, url: &str) -> Option<TextureId> {
        match self.cache.get(url) {
            Some(Some(path)) => self.images.get(&c.ctx, path),
            Some(None) => None,
            None => {
                let mut w = self.wanted.borrow_mut();
                if !w.iter().any(|u| u == url) {
                    w.push(url.to_string());
                }
                None
            }
        }
    }
}

/// Landscape picture of an item (episode still, else backdrop).
fn wide_url(server: &str, it: &Item) -> String {
    match (it.kind(), &it.backdrop_item) {
        (Kind::Episode, _) if it.has_primary => jf::image_url(server, &it.id, "Primary", 480),
        (_, Some(b)) => jf::image_url(server, b, "Backdrop", 480),
        _ => jf::image_url(server, &it.id, "Primary", 480),
    }
}

fn poster_url(server: &str, it: &Item) -> String {
    jf::image_url(server, &it.id, "Primary", 300)
}

fn backdrop_url(server: &str, it: &Item) -> Option<String> {
    it.backdrop_item
        .as_ref()
        .map(|b| jf::image_url_blurred(server, b, "Backdrop", 1280, BACKDROP_BLUR))
}

/// Soft blur of the backdrop art (server-side): enough that the
/// text floats on it, not so much that the picture is lost.
const BACKDROP_BLUR: u32 = 6;

/// Card corner radius (Abyss's default is 24 px).
const RADIUS: f32 = 24.0;

/// Space between a row's title and its cards: the focused card lifts and
/// grows, and must not touch the title.
const ROW_GAP: f32 = 40.0;

fn focus_t(c: &C, id: impl std::hash::Hash + std::fmt::Debug, focused: bool) -> f32 {
    c.anim(id, if focused { 1.0 } else { 0.0 })
}

/// Full-screen backdrop: the (blurred) art of what has focus, cross-faded
/// from the previous one, darkened with smooth left and bottom gradients
/// (one mesh each: the stepped fills used before drew a visible seam).
///
fn backdrop(c: &C, pics: &FinPics, server: &str, it: Option<&Item>) {
    let dark = theme::BG_BOTTOM;
    let url = it.and_then(|i| backdrop_url(server, i));
    let tex = url.as_deref().and_then(|u| pics.get(c, u));
    // Remember the last art that was actually drawn, to fade from it.
    let now = c.ctx.input(|i| i.time);
    let shown: (Option<String>, Option<String>, f64) = c.remember("fin-bd", |old| {
        let (cur, prev, since) = old.unwrap_or((None, None, now));
        match (&url, tex) {
            (Some(u), Some(_)) if cur.as_deref() != Some(u.as_str()) => (Some(u.clone()), cur, now),
            _ => (cur, prev, since),
        }
    });
    let k = ((now - shown.2) / 0.6).clamp(0.0, 1.0) as f32;
    let k = 1.0 - (1.0 - k).powi(2);
    if k < 1.0 {
        c.ctx.request_repaint();
    }
    let full = c.r(0.0, 0.0, 1280.0, 720.0);
    if let Some(t) = shown
        .1
        .as_deref()
        .filter(|_| k < 1.0)
        .and_then(|prev| pics.get(c, prev))
    {
        c.image(full, t, Color32::WHITE);
    }
    if let Some(t) = shown.0.as_deref().and_then(|cur| pics.get(c, cur)) {
        // A slow, barely-there drift so the picture breathes.
        let drift = ((now * 0.08).sin() * 6.0) as f32;
        c.image(
            c.r(-8.0 + drift, -6.0, 1296.0, 732.0),
            t,
            Color32::from_white_alpha((255.0 * k) as u8),
        );
    }
    // Reading light: dark on the left, dark at the bottom, a veil over all.
    c.hgradient(
        c.r(0.0, 0.0, 880.0, 720.0),
        theme::rgba(dark, 225),
        theme::rgba(dark, 0),
    );
    c.vgradient(
        c.r(0.0, 260.0, 1280.0, 460.0),
        theme::rgba(dark, 0),
        theme::rgba(dark, 235),
    );
    c.vgradient(
        c.r(0.0, 0.0, 1280.0, 120.0),
        theme::rgba(dark, 150),
        theme::rgba(dark, 0),
    );
    c.fill(c.screen, 0.0, theme::rgba(dark, 60));
}

/// A card with a picture (or a monogram while it loads): 24 px corners;
/// focus lifts it with a soft shadow and a sheen, unfocused it is
/// flat.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn card(
    c: &C,
    pics: &FinPics,
    url: &str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    t: f32,
    grow: f32,
    label: &str,
) {
    let rect = widgets::lifted_by(c, c.r(x, y, w, h), t, grow);
    let radius = RADIUS.min(h / 4.0);
    widgets::focus_lift(c, rect, radius, t);
    c.fill(rect, radius, theme::rgba(theme::BG_TOP, 210));
    if let Some(tex) = pics.get(c, url) {
        let k = c.appear(("fin-pic", url), 0.35);
        c.image_rounded(
            rect,
            radius,
            tex,
            Color32::from_white_alpha((255.0 * k) as u8),
        );
    } else {
        c.fill(rect, radius, theme::glass());
        let (rx, ry) = c.to_design(rect.min);
        let (rw, rh) = c.dsize(rect);
        let mono: String = label.chars().take(1).collect::<String>().to_uppercase();
        c.text(
            rx + rw / 2.0,
            ry + rh / 2.0,
            Align2::CENTER_CENTER,
            &mono,
            rh * 0.3,
            theme::text3(),
            Weight::Semibold,
        );
    }
    c.frost_edge(rect, radius, 0.5);
    widgets::focus_sheen(c, rect, radius, t);
}

fn progress_line(c: &C, x: f32, y: f32, w: f32, p: f32) {
    c.fill(c.r(x, y, w, 5.0), 2.5, theme::rgba(theme::WHITE, 50));
    c.fill(c.r(x, y, (w * p).max(5.0), 5.0), 2.5, theme::accent());
}

/// Accent disc with a check in a card's corner: watched to the end
/// (Jellyfin's own badge).
fn watched_badge(c: &C, cx: f32, cy: f32, r: f32) {
    c.circle(cx, cy, r + 2.0, theme::rgba(theme::BLACK, 90));
    c.circle(cx, cy, r, theme::accent());
    icons::draw(c, "check", cx, cy, r, theme::on_accent());
}

/// A card's watched state: the progress line along the bottom while part
/// way through, the badge once finished.
fn card_state(c: &C, it: &Item, x: f32, y: f32, w: f32, h: f32) {
    if it.played {
        watched_badge(c, x + w - 16.0, y + 16.0, 11.0);
    } else if let Some(p) = it.progress() {
        progress_line(c, x + 12.0, y + h - 14.0, w - 24.0, p);
    }
}

/// Brand, the pill tab bar (Home · each library; the accent pill slides
/// with focus) and the account button (Abyss-style).
/// `focus`: Some(index) while the header has focus: 0..n = libraries,
/// n = account. `selected`: the library screen you are on (None = Home).
fn header(
    c: &C,
    accounts: &Accounts,
    libs: &[Item],
    selected: Option<usize>,
    focus: Option<usize>,
) {
    let k = 1.0 - (1.0 - c.appear("fin-header", 0.4)).powi(3);
    let y = 26.0 - 14.0 * (1.0 - k);
    icons::draw(c, "catfyn", 76.0, y + 26.0, 28.0, theme::accent());
    c.text(
        100.0,
        y + 26.0,
        Align2::LEFT_CENTER,
        "Catfyn",
        22.0,
        theme::text(),
        Weight::Semibold,
    );
    let labels: Vec<&str> = std::iter::once("Home")
        .chain(libs.iter().map(|l| l.name.as_str()))
        .collect();
    let rects = widgets::tab_bar(
        c,
        "fin-tabs",
        640.0,
        y,
        52.0,
        &labels,
        Some(selected.map_or(0, |i| i + 1)),
        focus.filter(|i| *i < libs.len()).map(|i| i + 1),
    );
    for (i, rect) in rects.iter().enumerate().skip(1) {
        c.hit(*rect, Target::Fin(usize::MAX, i - 1));
    }
    // Account button, top right.
    let Some(a) = accounts.active() else { return };
    let name = if a.user_name.is_empty() {
        "Account"
    } else {
        &a.user_name
    };
    // The button opens Catfyn's settings: a gear says so.
    let w = c.text_width(name, 16.0, Weight::Semibold) + if a.incognito { 118.0 } else { 96.0 };
    let rect = c.r(1216.0 - w, y + 2.0, w, 48.0);
    let t = focus_t(c, "fin-acct", focus == Some(libs.len()));
    c.glass(rect, 24.0);
    widgets::focus_pill(c, rect, 24.0, t);
    let x = 1216.0 - w;
    let cy = y + 26.0;
    c.circle(x + 26.0, cy, 15.0, theme::accent());
    c.text(
        x + 26.0,
        cy,
        Align2::CENTER_CENTER,
        &name.chars().take(1).collect::<String>().to_uppercase(),
        15.0,
        theme::on_accent(),
        Weight::Semibold,
    );
    c.text(
        x + 50.0,
        cy,
        Align2::LEFT_CENTER,
        name,
        16.0,
        theme::text(),
        Weight::Semibold,
    );
    if a.incognito {
        icons::draw(c, "lock", x + w - 52.0, cy, 15.0, theme::text2());
    }
    icons::draw(c, "gear", x + w - 26.0, cy, 16.0, theme::text2());
    c.hit(rect, Target::Fin(usize::MAX, libs.len()));
}

/// Spotlight text: title, fact pills, overview, progress and the action
/// OK takes (Resume / Play / Open), at (x, y). `now_k` fades/slides it in.
fn hero_text(c: &C, it: &Item, x: f32, y: f32, w: f32, now_k: f32) {
    hero_text_with(c, it, x, y, w, now_k, true);
}

#[allow(clippy::too_many_arguments)]
fn hero_text_with(c: &C, it: &Item, x: f32, y: f32, w: f32, now_k: f32, action_chip: bool) {
    let a = |v: f32| (v * now_k) as u8;
    let slide = 16.0 * (1.0 - now_k);
    let title = c.fit(it.title(), w, 44.0, Weight::Semibold);
    c.text(
        x - slide,
        y,
        Align2::LEFT_TOP,
        &title,
        44.0,
        theme::rgba(theme::INK, a(255.0)),
        Weight::Semibold,
    );
    // Facts as small frosted pills (episode / year, runtime, age rating,
    // score), like Abyss's spotlight.
    let mut px = x - slide * 0.6;
    let py = y + 62.0;
    let mut pills = it.pills();
    if it.kind() == Kind::Episode && !it.name.is_empty() {
        pills.insert(1.min(pills.len()), it.name.clone());
    }
    for p in pills {
        let pw = c.text_width(&p, 14.0, Weight::Semibold) + 26.0;
        if px + pw > x + w {
            break;
        }
        let rect = c.r(px, py, pw, 30.0);
        c.fill(rect, 15.0, theme::rgba(theme::BG_TOP, a(140.0)));
        c.fill(rect, 15.0, theme::rgba(theme::WHITE, a(22.0)));
        c.outline(rect, 15.0, 1.0, theme::rgba(theme::WHITE, a(30.0)));
        c.text(
            px + pw / 2.0,
            py + 15.0,
            Align2::CENTER_CENTER,
            &p,
            14.0,
            theme::rgba(theme::INK, a(225.0)),
            Weight::Semibold,
        );
        px += pw + 8.0;
    }
    if !it.overview.is_empty() {
        let text: String = it.overview.chars().take(230).collect();
        c.paragraph(
            x - slide * 0.3,
            y + 106.0,
            w,
            &text,
            15.5,
            theme::rgba(theme::INK, a(150.0)),
            Weight::Regular,
        );
    }
    if !action_chip {
        if let Some(p) = it.progress() {
            progress_line(c, x, y + 186.0, 240.0, p);
        }
        return;
    }
    // What OK does: an accent chip, with the progress line beside it.
    let action = match (it.progress(), it.kind()) {
        (Some(_), _) => "Resume",
        (None, Kind::Series) => "Open",
        _ => "Play",
    };
    let aw = c.text_width(action, 14.0, Weight::Semibold) + 50.0;
    let ay = y + 172.0;
    let chip = c.r(x, ay, aw, 34.0);
    c.fill(chip, 17.0, theme::rgba(theme::accent_rgb(), a(235.0)));
    c.rounded_gradient(chip, 17.0, theme::WHITE, a(40.0), 0);
    let on = theme::on_accent();
    let on = Color32::from_rgba_unmultiplied(on.r(), on.g(), on.b(), a(255.0));
    icons::draw(c, "play", x + 20.0, ay + 17.0, 9.0, on);
    c.text(
        x + 32.0,
        ay + 17.0,
        Align2::LEFT_CENTER,
        action,
        14.0,
        on,
        Weight::Semibold,
    );
    if let Some(p) = it.progress() {
        progress_line(c, x + aw + 18.0, ay + 14.5, 200.0, p);
        if let Some(r) = it.runtime {
            let left = ((r - it.position) / 60.0).round().max(0.0) as u32;
            c.text(
                x + aw + 230.0,
                ay + 17.0,
                Align2::LEFT_CENTER,
                &format!("{left} min left"),
                13.0,
                theme::rgba(theme::INK, a(150.0)),
                Weight::Medium,
            );
        }
    }
}

pub fn draw(c: &C, f: &Fin, accounts: &Accounts, prefs: &UiPrefs, pics: &FinPics, now: f64) {
    let server = accounts.active().map(|a| a.server.as_str()).unwrap_or("");
    match f.screen() {
        Some(Screen::Browse) | None => draw_browse(c, f, accounts, pics, server, now),
        Some(Screen::Connect { servers, index }) => draw_connect(c, f, servers, *index, now),
        Some(Screen::Address { osk }) => draw_entry(
            c,
            f,
            "Server address",
            "e.g. 192.168.1.20 or jellyfin.lan:8096",
            osk,
            false,
            now,
        ),
        Some(Screen::SignIn {
            server: s,
            step,
            osk,
            busy,
            ..
        }) => {
            let (title, ph) = match step {
                SignInStep::User => (format!("Sign in to {}", s.name), "User name"),
                SignInStep::Password => (format!("Sign in to {}", s.name), "Password"),
            };
            draw_entry(c, f, &title, ph, osk, *busy, now)
        }
        Some(Screen::Library { lib, items, index }) => {
            let libs = f.header_libraries();
            draw_library(c, pics, server, accounts, &libs, lib, items, *index, now)
        }
        Some(Screen::Series {
            series,
            seasons,
            season,
            episodes,
            in_episodes,
            index,
        }) => draw_series(
            c,
            pics,
            server,
            series,
            seasons,
            *season,
            episodes,
            *in_episodes,
            *index,
            now,
        ),
        Some(Screen::Details { item, index }) => draw_details(c, pics, server, item, *index),
        Some(Screen::Settings { index, menu }) => draw_settings(c, accounts, prefs, *index, *menu),
    }
}

/// A load that failed: what went wrong, and how to try again.
fn failed(c: &C, e: &str) {
    icons::draw(c, "refresh", 640.0, 320.0, 30.0, theme::text3());
    c.text(
        640.0,
        370.0,
        Align2::CENTER_CENTER,
        e,
        20.0,
        theme::text2(),
        Weight::Medium,
    );
    c.text(
        640.0,
        404.0,
        Align2::CENTER_CENTER,
        "Press OK to try again",
        16.0,
        theme::text3(),
        Weight::Regular,
    );
}

fn draw_browse(c: &C, f: &Fin, accounts: &Accounts, pics: &FinPics, server: &str, now: f64) {
    let rows = f.rows();
    let focused = f.focused_item();
    backdrop(c, pics, server, focused.as_ref());
    let libs = f.header_libraries();
    header(c, accounts, &libs, None, f.header.then_some(f.header_index));
    match &f.home {
        Scan::Busy | Scan::Idle => {
            widgets::spinner(c, 640.0, 380.0, 22.0, now);
            return;
        }
        Scan::Failed(e) => {
            failed(c, e);
            return;
        }
        Scan::Done(_) => {}
    }
    if rows.is_empty() {
        c.text(
            640.0,
            360.0,
            Align2::CENTER_CENTER,
            "This account has nothing to watch yet",
            20.0,
            theme::text2(),
            Weight::Medium,
        );
        return;
    }
    if let Some(it) = &focused {
        let k = 1.0 - (1.0 - c.appear(("fin-hero", &it.id), 0.3)).powi(3);
        hero_text(c, it, 64.0, 104.0, 640.0, k);
    }
    // Rows: the focused row's top sits at y = 352; others scroll.
    let row_h = |r: &fin::Row| {
        if r.libraries || is_wide(r) {
            208.0
        } else {
            282.0
        }
    };
    let mut offsets = Vec::with_capacity(rows.len());
    let mut acc = 0.0;
    for r in &rows {
        offsets.push(acc);
        acc += row_h(r);
    }
    let target = offsets.get(f.row).copied().unwrap_or(0.0);
    let scroll = c.anim("fin-rows", target);
    let list = c.clipped(0.0, 330.0, 1280.0, 390.0);
    for (ri, r) in rows.iter().enumerate() {
        let y = 352.0 + offsets[ri] - scroll;
        if y > 720.0 || y + row_h(r) < 300.0 {
            continue;
        }
        let row_focus = !f.header && f.row == ri;
        // Rows fade up one after another when Browse opens (Abyss).
        let rise = 1.0 - (1.0 - list.appear(("fin-row-in", ri), 0.35 + 0.1 * ri as f32)).powi(3);
        let y = y + 24.0 * (1.0 - rise);
        let tr = list.anim(("fin-row-t", ri), if row_focus { 1.0 } else { 0.0 });
        list.text(
            64.0,
            y,
            Align2::LEFT_TOP,
            &r.title,
            17.0 + 1.5 * tr,
            Color32::from_rgba_unmultiplied(
                theme::INK.0,
                theme::INK.1,
                theme::INK.2,
                ((150.0 + 105.0 * tr) * rise) as u8,
            ),
            Weight::Semibold,
        );
        let wide = r.libraries || is_wide(r);
        let (w, h, gap) = if wide {
            (240.0, 135.0, 22.0)
        } else {
            (150.0, 225.0, 20.0)
        };
        let col = f.cols.get(ri).copied().unwrap_or(0);
        let visible = ((1216.0 - 64.0) / (w + gap)) as usize;
        let first = col.saturating_sub(visible.saturating_sub(1));
        let sx = list.anim(("fin-row-x", ri), first as f32 * (w + gap));
        for (ci, it) in r.items.iter().enumerate() {
            let x = 64.0 + ci as f32 * (w + gap) - sx;
            if x > 1280.0 || x + w < 0.0 {
                continue;
            }
            let t = focus_t(&list, ("fin-card", ri, ci), row_focus && col == ci);
            let url = if r.libraries || wide {
                wide_url(server, it)
            } else {
                poster_url(server, it)
            };
            card(
                &list,
                pics,
                &url,
                x,
                y + ROW_GAP,
                w,
                h,
                t,
                widgets::LIFT,
                &it.name,
            );
            if r.libraries {
                list.text(
                    x + 16.0,
                    y + ROW_GAP + h - 22.0,
                    Align2::LEFT_CENTER,
                    &list.fit(&it.name, w - 32.0, 17.0, Weight::Semibold),
                    17.0,
                    theme::text(),
                    Weight::Semibold,
                );
            } else {
                card_state(&list, it, x, y + ROW_GAP, w, h);
            }
            if y + ROW_GAP > 330.0 && y + ROW_GAP + h < 720.0 {
                c.hit(c.r(x, y + ROW_GAP, w, h), Target::Fin(ri, ci));
            }
        }
    }
}

/// Continue watching / Next up show landscape stills.
fn is_wide(r: &fin::Row) -> bool {
    r.title == "Continue watching" || r.title == "Next up"
}

fn draw_connect(c: &C, f: &Fin, servers: &Scan<jf::Server>, index: usize, now: f64) {
    let panel = c.r(300.0, 110.0, 680.0, 500.0);
    c.glass(panel, 32.0);
    icons::draw(c, "catfyn", 640.0, 170.0, 46.0, theme::accent());
    c.text(
        640.0,
        214.0,
        Align2::CENTER_CENTER,
        "Connect to Jellyfin",
        26.0,
        theme::text(),
        Weight::Semibold,
    );
    let mut rows: Vec<(String, String)> = servers
        .items()
        .iter()
        .map(|s| (s.name.clone(), s.address.clone()))
        .collect();
    rows.push(("Enter address".into(), String::new()));
    let busy = matches!(servers, Scan::Busy);
    let mut y = 256.0;
    if busy {
        widgets::busy_line(c, 340.0, y + 6.0, "Looking for servers…", true, now);
        y += 44.0;
    }
    for (k, (name, addr)) in rows.iter().enumerate().take(5) {
        let t = focus_t(c, ("fin-srv", k), index == k);
        let rect = c.r(340.0, y, 600.0, 62.0);
        widgets::list_row(
            c,
            rect,
            &ListRow {
                title: name,
                subtitle: addr,
                value: "",
                kind: Some(if addr.is_empty() {
                    RowKind::Primary
                } else {
                    RowKind::Action
                }),
                icon: Some(if addr.is_empty() { "plus" } else { "tv" }),
                disabled: false,
            },
            t,
        );
        c.hit(rect, Target::Fin(0, k));
        y += 72.0;
    }
    error_line(c, f, 580.0);
}

fn error_line(c: &C, f: &Fin, y: f32) {
    if let Some(e) = &f.error {
        c.text(
            640.0,
            y,
            Align2::CENTER_CENTER,
            e,
            17.0,
            theme::rgb(theme::DANGER),
            Weight::Medium,
        );
    }
}

fn draw_entry(
    c: &C,
    f: &Fin,
    title: &str,
    placeholder: &str,
    osk: &catostv_core::osk::Osk,
    busy: bool,
    now: f64,
) {
    let panel = c.r(240.0, 70.0, 800.0, 590.0);
    c.glass(panel, 32.0);
    c.text(
        640.0,
        118.0,
        Align2::CENTER_CENTER,
        &c.fit(title, 700.0, 24.0, Weight::Semibold),
        24.0,
        theme::text(),
        Weight::Semibold,
    );
    widgets::keyboard(
        c,
        osk,
        280.0,
        160.0,
        720.0,
        placeholder,
        !busy,
        "fin-osk",
        &|_, _| Target::Outside,
    );
    if busy {
        widgets::busy_line(c, 280.0, 620.0, "Signing in…", true, now);
    } else {
        error_line(c, f, 628.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_library(
    c: &C,
    pics: &FinPics,
    server: &str,
    accounts: &Accounts,
    accounts_libs: &[Item],
    lib: &Item,
    items: &Scan<Item>,
    index: usize,
    now: f64,
) {
    let all = items.items();
    backdrop(c, pics, server, all.get(index));
    // Same header as Browse, the pill on this library; nothing in it has
    // focus here (Back returns to Browse).
    let selected = accounts_libs.iter().position(|l| l.id == lib.id);
    header(c, accounts, accounts_libs, selected, None);
    let (y0, k) = {
        let k = 1.0 - (1.0 - c.appear(("fin-lib-in", &lib.id), 0.35)).powi(3);
        (112.0, k)
    };
    if let Some(it) = all.get(index) {
        let a = (255.0 * k) as u8;
        c.text(
            64.0,
            y0 - 10.0 * (1.0 - k),
            Align2::LEFT_TOP,
            &c.fit(it.title(), 700.0, 30.0, Weight::Semibold),
            30.0,
            theme::rgba(theme::INK, a),
            Weight::Semibold,
        );
        let mut px = 64.0;
        for p in it.pills().into_iter().take(4) {
            let pw = c.text_width(&p, 13.0, Weight::Semibold) + 22.0;
            let rect = c.r(px, y0 + 44.0, pw, 26.0);
            c.fill(rect, 13.0, theme::rgba(theme::WHITE, (26.0 * k) as u8));
            c.text(
                px + pw / 2.0,
                y0 + 57.0,
                Align2::CENTER_CENTER,
                &p,
                13.0,
                theme::rgba(theme::INK, (210.0 * k) as u8),
                Weight::Semibold,
            );
            px += pw + 8.0;
        }
    }
    match items {
        Scan::Busy | Scan::Idle => return widgets::spinner(c, 640.0, 380.0, 22.0, now),
        Scan::Failed(e) => {
            failed(c, e);
            return;
        }
        Scan::Done(v) if v.is_empty() => {
            c.text(
                640.0,
                360.0,
                Align2::CENTER_CENTER,
                "Nothing here yet",
                20.0,
                theme::text2(),
                Weight::Medium,
            );
            return;
        }
        _ => {}
    }
    let (w, h, gx, gy) = (150.0, 225.0, 42.0, 60.0);
    let cols = fin::GRID_COLS;
    let row = index / cols;
    let scroll = c.anim("fin-lib-scroll", row as f32 * (h + gy));
    let list = c.clipped(0.0, 180.0, 1280.0, 540.0);
    for (i, it) in all.iter().enumerate() {
        let x = 64.0 + (i % cols) as f32 * (w + gx);
        let y = 206.0 + (i / cols) as f32 * (h + gy) - scroll;
        if y > 720.0 || y + h < 160.0 {
            continue;
        }
        let rise = 1.0
            - (1.0 - list.appear(("fin-lib-row-in", i / cols), 0.3 + 0.08 * (i / cols) as f32))
                .powi(3);
        let y = y + 20.0 * (1.0 - rise);
        let t = focus_t(&list, ("fin-lib", i), i == index);
        card(
            &list,
            pics,
            &poster_url(server, it),
            x,
            y,
            w,
            h,
            t,
            widgets::LIFT,
            &it.name,
        );
        list.text(
            x + w / 2.0,
            y + h + 14.0,
            Align2::CENTER_TOP,
            &list.fit(&it.name, w + 20.0, 14.5, Weight::Medium),
            14.5,
            if i == index {
                theme::text()
            } else {
                theme::text2()
            },
            Weight::Medium,
        );
        card_state(&list, it, x, y, w, h);
        if y > 180.0 && y + h < 720.0 {
            c.hit(c.r(x, y, w, h), Target::Fin(0, i));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_series(
    c: &C,
    pics: &FinPics,
    server: &str,
    series: &Item,
    seasons: &Scan<Item>,
    season: usize,
    episodes: &Scan<Item>,
    in_episodes: bool,
    index: usize,
    now: f64,
) {
    let bd = Item {
        backdrop_item: Some(series.id.clone()),
        ..series.clone()
    };
    backdrop(c, pics, server, Some(&bd));
    let k = 1.0 - (1.0 - c.appear(("fin-series-in", &series.id), 0.35)).powi(3);
    brand(c, k);
    c.text(
        64.0 - 12.0 * (1.0 - k),
        100.0,
        Align2::LEFT_CENTER,
        &c.fit(series.title(), 900.0, 34.0, Weight::Semibold),
        34.0,
        theme::rgba(theme::INK, (255.0 * k) as u8),
        Weight::Semibold,
    );
    // Seasons as chips.
    let mut x = 64.0;
    for (k, s) in seasons.items().iter().enumerate() {
        let label = if s.name.is_empty() {
            format!("Season {}", k + 1)
        } else {
            s.name.clone()
        };
        let w = widgets::filter_chip_width(c, &label);
        let t = focus_t(c, ("fin-season", k), !in_episodes && k == season);
        let rect = c.r(x, 138.0, w, 44.0);
        widgets::filter_chip(c, rect, &label, k == season, t);
        c.hit(rect, Target::Fin(0, k));
        x += w + 12.0;
    }
    if matches!(seasons, Scan::Busy) || matches!(episodes, Scan::Busy) {
        widgets::spinner(c, 640.0, 420.0, 22.0, now);
        return;
    }
    let eps = episodes.items();
    let (rh, gap) = (126.0, 14.0);
    let scroll = c.anim("fin-ep-scroll", index.saturating_sub(2) as f32 * (rh + gap));
    let list = c.clipped(0.0, 200.0, 1280.0, 520.0);
    for (i, e) in eps.iter().enumerate() {
        let y = 210.0 + i as f32 * (rh + gap) - scroll;
        if y > 720.0 || y + rh < 190.0 {
            continue;
        }
        let t = focus_t(&list, ("fin-ep", i), in_episodes && i == index);
        let rise = 1.0 - (1.0 - list.appear(("fin-ep-in", i), 0.3 + 0.06 * i as f32)).powi(3);
        let y = y + 20.0 * (1.0 - rise);
        // Glass focus inside the row.
        let rect = list.r(64.0, y, 1152.0, rh);
        list.fill(rect, RADIUS, theme::rgba(theme::BG_TOP, 150));
        list.fill(rect, RADIUS, theme::rgba(theme::WHITE, 10));
        list.frost_edge(rect, RADIUS, 0.6);
        widgets::focus_pill(&list, rect, RADIUS, t);
        card(
            &list,
            pics,
            &wide_url(server, e),
            76.0,
            y + 12.0,
            182.0,
            102.0,
            0.0,
            widgets::LIFT,
            &e.name,
        );
        // Watched state: how far, as a line over the still and
        // "12 min left" after the title; a check badge once finished.
        let label = match e.episode {
            Some(n) => format!("{n}. {}", e.name),
            None => e.name.clone(),
        };
        let mut title_w = 860.0;
        if e.played {
            watched_badge(&list, 76.0 + 182.0 - 16.0, y + 12.0 + 16.0, 11.0);
        } else if let Some(p) = e.progress() {
            progress_line(&list, 88.0, y + 12.0 + 102.0 - 12.0, 158.0, p);
            if let Some(left) = e.time_left() {
                let lw = list.text_width(&left, 14.0, Weight::Medium) + 24.0;
                let fitted = list.fit(&label, 860.0 - lw - 12.0, 18.0, Weight::Semibold);
                let lx = 280.0 + list.text_width(&fitted, 18.0, Weight::Semibold) + 12.0;
                list.fill(
                    list.r(lx, y + 17.0, lw, 22.0),
                    11.0,
                    theme::rgba(theme::WHITE, 28),
                );
                list.text(
                    lx + lw / 2.0,
                    y + 28.0,
                    Align2::CENTER_CENTER,
                    &left,
                    14.0,
                    theme::text2(),
                    Weight::Medium,
                );
                title_w = 860.0 - lw - 12.0;
            }
        }
        list.text(
            280.0,
            y + 28.0,
            Align2::LEFT_CENTER,
            &list.fit(&label, title_w, 18.0, Weight::Semibold),
            18.0,
            theme::text(),
            Weight::Semibold,
        );
        let ov: String = e.overview.chars().take(150).collect();
        list.text(
            280.0,
            y + 60.0,
            Align2::LEFT_CENTER,
            &list.fit(&ov, 900.0, 14.5, Weight::Regular),
            14.5,
            theme::text2(),
            Weight::Regular,
        );
        if y > 200.0 && y + rh < 720.0 {
            c.hit(c.r(64.0, y, 1152.0, rh), Target::Fin(1, i));
        }
    }
}

fn draw_details(c: &C, pics: &FinPics, server: &str, item: &Item, index: usize) {
    backdrop(c, pics, server, Some(item));
    let k = 1.0 - (1.0 - c.appear(("fin-details-in", &item.id), 0.4)).powi(3);
    // Poster on the right, rising in.
    let dy = 24.0 * (1.0 - k);
    if item.kind() != Kind::Episode {
        card(
            c,
            pics,
            &poster_url(server, item),
            900.0,
            110.0 + dy,
            280.0,
            420.0,
            0.0,
            widgets::LIFT,
            &item.name,
        );
    } else {
        card(
            c,
            pics,
            &wide_url(server, item),
            820.0,
            150.0 + dy,
            400.0,
            225.0,
            0.0,
            widgets::LIFT,
            &item.name,
        );
    }
    brand(c, k);
    hero_text_with(c, item, 64.0, 120.0, 680.0, k, false);
    let acts = fin::detail_actions(item);
    let mut x = 64.0;
    for (i, label) in acts.iter().enumerate() {
        let w = c.text_width(label, 17.0, Weight::Semibold) + 64.0;
        let t = focus_t(c, ("fin-act", i), i == index);
        let rect = c.r(x, 350.0 + 16.0 * (1.0 - k), w, 58.0);
        widgets::button(
            c,
            rect,
            label,
            if i == index {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Secondary
            },
            t,
        );
        c.hit(rect, Target::Fin(0, i));
        x += w + 16.0;
    }
}

/// The small brand mark, top left, on screens without the tab bar.
fn brand(c: &C, k: f32) {
    let a = (255.0 * k) as u8;
    icons::draw(
        c,
        "catfyn",
        76.0,
        52.0,
        24.0,
        theme::rgba(theme::accent_rgb(), a),
    );
    c.text(
        98.0,
        52.0,
        Align2::LEFT_CENTER,
        "Catfyn",
        19.0,
        theme::rgba(theme::INK, (200.0 * k) as u8),
        Weight::Semibold,
    );
}

/// Catfyn's settings: the accounts (each opens Use / Incognito / Sign out),
/// "Add account", and playback, only what someone changes in a media app.
fn draw_settings(
    c: &C,
    accounts: &Accounts,
    prefs: &UiPrefs,
    index: usize,
    menu: Option<(usize, usize)>,
) {
    let k = 1.0 - (1.0 - c.appear("fin-settings-in", 0.35)).powi(3);
    brand(c, k);
    widgets::title(c, 64.0, 100.0, "Settings", 30.0);
    let accs = accounts.list.len();
    let (x, w, h, step) = (64.0, 760.0, 62.0, 72.0);
    let section = |y: f32, label: &str| widgets::section(c, x, y, label);
    let mut y = 166.0;
    section(y, "Accounts");
    y += 28.0;
    let item = |y: f32, i: usize, row: &ListRow| {
        let t = focus_t(c, ("fin-set", i), menu.is_none() && index == i);
        let rect = c.r(x, y, w, h);
        widgets::list_row(c, rect, row, t);
        c.hit(rect, Target::Fin(0, i));
    };
    for (i, a) in accounts.list.iter().enumerate() {
        let title = if a.user_name.is_empty() {
            "Account".to_string()
        } else {
            a.user_name.clone()
        };
        let sub = if a.server_name.is_empty() {
            a.server.clone()
        } else {
            a.server_name.clone()
        };
        let value = match (i == accounts.active, a.incognito) {
            (true, true) => "In use · Incognito",
            (true, false) => "In use",
            (false, true) => "Incognito",
            (false, false) => "",
        };
        item(
            y,
            i,
            &ListRow {
                title: &title,
                subtitle: &sub,
                value,
                kind: Some(RowKind::Action),
                icon: Some(if a.incognito { "lock" } else { "catfyn" }),
                disabled: false,
            },
        );
        y += step;
    }
    item(
        y,
        accs + fin::SETTINGS_ADD,
        &ListRow {
            title: "Add account",
            subtitle: "",
            value: "",
            kind: Some(RowKind::Action),
            icon: Some("plus"),
            disabled: false,
        },
    );
    y += step + 18.0;
    section(y, "Playback");
    y += 28.0;
    item(
        y,
        accs + fin::SETTINGS_AUTOPLAY,
        &ListRow {
            title: "Play next episode automatically",
            subtitle: "When the end credits start",
            value: if prefs.fin_autoplay { "On" } else { "Off" },
            kind: Some(RowKind::Toggle(Some(prefs.fin_autoplay))),
            icon: Some("play"),
            disabled: false,
        },
    );
    y += step;
    let secs = format!("{} s", prefs.fin_countdown);
    item(
        y,
        accs + fin::SETTINGS_COUNTDOWN,
        &ListRow {
            title: "Countdown",
            subtitle: "Before the next episode starts",
            value: &secs,
            kind: Some(RowKind::Choice),
            icon: Some("refresh"),
            disabled: !prefs.fin_autoplay,
        },
    );
    if let Some((i, opt)) = menu {
        let m = widgets::modal(c, 480.0, 300.0);
        let (mx, my) = c.to_design(m.min);
        let name = accounts
            .list
            .get(i)
            .map(|a| a.user_name.as_str())
            .unwrap_or("");
        c.text(
            mx + 36.0,
            my + 46.0,
            Align2::LEFT_CENTER,
            &c.fit(name, 400.0, 22.0, Weight::Semibold),
            22.0,
            theme::text(),
            Weight::Semibold,
        );
        let incognito = accounts.list.get(i).is_some_and(|a| a.incognito);
        for (k, label) in fin::ACCOUNT_OPTIONS.iter().enumerate() {
            let t = focus_t(c, ("fin-accm", k), k == opt);
            let rect = c.r(mx + 32.0, my + 84.0 + k as f32 * 64.0, 416.0, 54.0);
            widgets::list_row(
                c,
                rect,
                &ListRow {
                    title: label,
                    subtitle: "",
                    value: "",
                    kind: Some(if k == 1 {
                        RowKind::Toggle(Some(incognito))
                    } else {
                        RowKind::Action
                    }),
                    icon: None,
                    disabled: false,
                },
                t,
            );
        }
    }
}

/// CatosTV Home's media rows (Continue watching: landscape stills with
/// progress; Movies / TV shows: posters). `focus` = (row, column) while
/// the rows have focus; `y` is the top of the first row.
pub fn home_rows(
    c: &C,
    pics: &FinPics,
    server: &str,
    rows: &[fin::Row],
    focus: Option<(usize, usize)>,
    y: f32,
) {
    let (frow, fcol) = focus.unwrap_or((0, 0));
    let row_h = |r: &fin::Row| {
        if r.title == "Continue watching" {
            262.0
        } else {
            344.0
        }
    };
    let mut offsets = Vec::with_capacity(rows.len());
    let mut acc = 0.0;
    for r in rows {
        offsets.push(acc);
        acc += row_h(r);
    }
    let scroll = c.anim("home-rows", offsets.get(frow).copied().unwrap_or(0.0));
    // Clipped just above the focused row's title: the rows scrolled past
    // (titles included) are gone, not peeking out under the hero.
    let list = c.clipped(0.0, y - 14.0, 1280.0, 720.0 - y + 14.0);
    for (ri, r) in rows.iter().enumerate() {
        let ry = y + offsets[ri] - scroll;
        if ry > 720.0 || ry + row_h(r) < y - 14.0 {
            continue;
        }
        let wide = r.title == "Continue watching";
        widgets::section(&list, 64.0, ry, &r.title);
        let (w, h, gap) = if wide {
            (240.0, 135.0, 26.0)
        } else {
            (150.0, 225.0, 24.0)
        };
        let col = if ri == frow { fcol } else { 0 };
        let visible = ((1216.0 - 64.0) / (w + gap)) as usize;
        let first = col.saturating_sub(visible.saturating_sub(1));
        let sx = list.anim(("home-media-x", ri), first as f32 * (w + gap));
        let row_focus = focus.filter(|(r, _)| *r == ri).map(|(_, i)| i);
        for (i, it) in r.items.iter().enumerate() {
            // The focused card grows; its neighbours step aside for it.
            let push = list.anim(
                ("home-media-push", ri, i),
                match row_focus {
                    Some(f) if i < f => -w * 0.08,
                    Some(f) if i > f => w * 0.08,
                    _ => 0.0,
                },
            );
            let x = 64.0 + i as f32 * (w + gap) - sx + push;
            if x > 1280.0 || x + w < 0.0 {
                continue;
            }
            let t = focus_t(&list, ("home-media", ri, i), focus == Some((ri, i)));
            let url = if wide {
                wide_url(server, it)
            } else {
                poster_url(server, it)
            };
            let top = ry + ROW_GAP;
            card(
                &list,
                pics,
                &url,
                x,
                top,
                w,
                h,
                t,
                widgets::HOME_LIFT,
                it.title(),
            );
            let grown = widgets::lifted_by(&list, list.r(x, top, w, h), t, widgets::HOME_LIFT);
            let (gx, gy) = list.to_design(grown.min);
            let (gw, gh) = list.dsize(grown);
            card_state(&list, it, gx, gy, gw, gh);
            // Posters carry their own title: theirs shows only under the
            // focused one. Episode stills need theirs, dimmer unless focused.
            let alpha = if wide { 150.0 + 105.0 * t } else { 255.0 * t };
            if alpha > 2.0 {
                list.text(
                    x + w / 2.0,
                    gy + gh + 12.0,
                    Align2::CENTER_TOP,
                    &list.fit(it.title(), w + 40.0 * t + 10.0, 15.5, Weight::Semibold),
                    15.5,
                    theme::rgba(theme::INK, alpha as u8),
                    Weight::Semibold,
                );
            }
            if top > y - 14.0 && top + h < 720.0 {
                c.hit(c.r(x, top, w, h), Target::HomeMedia(ri, i));
            }
        }
    }
}

/// The hero picture behind Home when a "Continue watching" tile has focus.
pub fn home_backdrop(c: &C, pics: &FinPics, server: &str, it: &Item) {
    backdrop(c, pics, server, Some(it));
}

/// Hero text for a media tile on Home.
pub fn home_hero(c: &C, it: &Item) {
    let k = 1.0 - (1.0 - c.appear(("home-hero", &it.id), 0.3)).powi(3);
    hero_text(c, it, 64.0, 104.0, 700.0, k);
}
