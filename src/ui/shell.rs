//! Drawing for `catostv-shell`. Pure presentation of `catostv_core::shell`
//! state: every function reads the state and paints; nothing here decides
//! navigation. Clickable rectangles are registered with `Canvas::hit` so
//! the mouse works everywhere.
//!
//! Layout is on the 1280 × 720 design canvas (see `canvas.rs`).

use crate::canvas::Canvas;
use crate::icons;
use crate::images::Images;
use crate::theme::{self, Weight};
use crate::widgets::{self, ButtonStyle, ListRow};
use catostv_core::installer::Scan;
use catostv_core::prefs;
use catostv_core::settings::{self, AppView, Category, RowKind, ShellContext};
use catostv_core::shell::{
    self, AppOption, BT_OPTIONS, CONFIRM_ITEMS, Overlay, POWER_ITEMS, Page, QUICK_COLS,
    QUICK_ITEMS, QuickItem, STORE_CATS, STORE_COLS, SearchFocus, SearchHit, Shell, StoreCat,
    StoreItem, StoreJob, TOP_ITEMS, Target, TopItem,
};
use eframe::egui::{Align2, Color32, Rect};
use std::collections::HashMap;

type C = Canvas<Target>;

/// Everything the shell screens need besides the core state.
pub struct ShellView<'a> {
    pub shell: &'a Shell,
    pub ctx: &'a ShellContext<'a>,
    pub images: &'a Images,
    /// Flathub app id → appstream icon PNG.
    pub store_icons: &'a HashMap<String, String>,
    /// Toast text and opacity (0–1).
    pub toast: Option<(&'a str, f32)>,
    /// Seconds left on the "keep this resolution?" countdown.
    pub confirm_left: Option<u64>,
    /// Monotonic seconds, for spinners.
    pub now: f64,
    /// The app being opened and seconds since OK (launch bounce).
    pub launching: Option<(&'a str, f32)>,
    /// Seconds since the shell started (start-up animation).
    pub since_start: f32,
    /// Page-change transition, 0 → 1 over a fraction of a second.
    pub page_t: f32,
    /// Store screenshot URL → downloaded PNG.
    pub shots: &'a HashMap<String, String>,
    /// Recents previews: app id → (capture, blurred copy).
    pub thumbs: &'a HashMap<String, (String, String)>,
    /// CPU and memory per app.
    pub usage: &'a catostv_core::usage::Usage,
    /// Catfyn pictures (URL → cached PNG) and the ones this frame wanted.
    pub fin_cache: &'a HashMap<String, Option<String>>,
    pub fin_wanted: &'a std::cell::RefCell<Vec<String>>,
}

pub struct Clock {
    pub time: String,
    pub date: String,
    pub hour: u32,
}

/// The wall clock, in 24-hour or 12-hour form (Settings → Customization).
pub fn clock(h24: bool) -> Clock {
    let now = chrono::Local::now();
    Clock {
        time: now
            .format(if h24 { "%H:%M" } else { "%-I:%M %p" })
            .to_string(),
        date: now.format("%A %-d %B").to_string(),
        hour: chrono::Timelike::hour(&now),
    }
}

fn focus_t(c: &C, id: impl std::hash::Hash + std::fmt::Debug, focused: bool) -> f32 {
    c.anim(id, if focused { 1.0 } else { 0.0 })
}

/// Draw one shell frame.
pub fn draw(c: &C, v: &ShellView) {
    let s = v.shell;
    let tint = match s.page {
        Page::Home => v
            .ctx
            .apps
            .get(s.rail_index)
            .map(|a| theme::app_rgb(&a.entry.accent)),
        _ => None,
    };
    // Same clock on every page (no jump when leaving Home); only Home
    // keeps frames coming, so elsewhere the light simply holds still.
    c.background_at(tint, v.ctx.prefs.glow, v.now);
    // Page transitions: the new page rises a little and fades in.
    let ease = 1.0 - (1.0 - v.page_t).powi(3);
    let fin_open = s.fin.is_some() || s.files.is_some();
    if let Some(f) = &s.fin {
        let pics = crate::ui::fin::FinPics {
            images: v.images,
            cache: v.fin_cache,
            wanted: v.fin_wanted,
        };
        crate::ui::fin::draw(c, f, &v.ctx.media.accounts, v.ctx.prefs, &pics, v.now);
    } else if let Some(f) = &s.files {
        crate::ui::files::draw(c, f, v.images, v.now);
    } else {
        match s.page {
            Page::Home => draw_home(c, v),
            Page::Recents => draw_recents(c, v),
            Page::Apps => draw_apps(c, v),
            Page::Store => draw_store(c, v),
            Page::Search => draw_search(c, v),
            Page::Settings => draw_settings(c, v),
        }
    }
    if ease < 0.999 && !fin_open {
        // Fade the page content in from the backdrop colour (the top bar is
        // drawn after this, so it stays put while pages change beneath it).
        c.fill(
            c.screen,
            0.0,
            theme::rgba(theme::BG_TOP, (210.0 * (1.0 - ease)) as u8),
        );
    }
    if s.page != Page::Settings && !fin_open {
        draw_top_bar(c, v);
    }
    if let Some(o) = s.overlay() {
        c.hit(c.screen, Target::Outside);
        draw_overlay(c, v, o);
    }
    if let Some((text, alpha)) = v.toast {
        widgets::toast(c, text, alpha);
    }
    if v.since_start < INTRO_SECS {
        draw_intro(c, v.since_start);
    }
}

/// How long the start-up animation lasts (seconds).
pub const INTRO_SECS: f32 = 2.6;

/// Start-up: the mark rises out of a soft glow, the name and "a libre TV
/// experience" fade in under it, then everything dissolves into Home
///
fn draw_intro(c: &C, secs: f32) {
    let ease = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        1.0 - (1.0 - x).powi(3)
    };
    // Whole overlay fades out over the last 0.6 s.
    let out = 1.0 - ease((secs - (INTRO_SECS - 0.6)) / 0.6);
    if out <= 0.0 {
        return;
    }
    let a = |v: f32| (v * out).clamp(0.0, 255.0) as u8;
    c.fill(c.screen, 0.0, theme::rgba(theme::BG_BOTTOM, a(255.0)));
    let accent = theme::accent_rgb();
    let bloom = ease(secs / 1.2);
    c.glow(640.0, 330.0, 260.0 + 220.0 * bloom, accent, a(70.0 * bloom));
    c.glow_ellipse(
        640.0,
        360.0,
        700.0,
        160.0,
        -0.25,
        (232, 116, 52),
        a(26.0 * bloom),
    );
    // The mark: scales up from 70 % with a little overshoot.
    let m = ease(secs / 0.9);
    let size = 96.0 * (0.7 + 0.3 * m + 0.05 * (m * std::f32::consts::PI).sin());
    let rise = 18.0 * (1.0 - m);
    let rect = c.r(640.0 - size / 2.0, 250.0 - size / 2.0 + rise, size, size);
    c.shadow(
        rect,
        size * 0.3,
        10.0,
        50.0,
        theme::rgba(accent, a(120.0 * m)),
    );
    c.fill(rect, size * 0.3, theme::rgba(accent, a(255.0 * m)));
    icons::draw(
        c,
        "tv",
        640.0,
        250.0 + rise + size * 0.04,
        size * 0.55,
        theme::rgba(theme::WHITE, a(255.0 * m)),
    );
    let name = ease((secs - 0.45) / 0.7);
    c.text(
        640.0,
        340.0 + 10.0 * (1.0 - name),
        Align2::CENTER_TOP,
        "CatosTV",
        54.0,
        theme::rgba(theme::INK, a(255.0 * name)),
        Weight::Semibold,
    );
    let line = ease((secs - 0.9) / 0.7);
    c.text(
        640.0,
        414.0 + 8.0 * (1.0 - line),
        Align2::CENTER_TOP,
        "A libre TV experience",
        20.0,
        theme::rgba(theme::INK, a(150.0 * line)),
        Weight::Regular,
    );
    // A thin shimmer that runs under the text while it settles.
    let sweep = ((secs - 0.9) / 1.2).clamp(0.0, 1.0);
    if sweep > 0.0 && sweep < 1.0 {
        let x = 520.0 + 240.0 * sweep;
        c.glow_ellipse(
            x,
            462.0,
            90.0,
            3.0,
            0.0,
            theme::WHITE,
            a(120.0 * (1.0 - sweep)),
        );
    }
}

// ------------------------------------------------------------------ top bar

fn draw_top_bar(c: &C, v: &ShellView) {
    let s = v.shell;
    let (y, h) = (24.0, 52.0);
    // Brand pill.
    let brand = c.r(52.0, y, 146.0, h);
    c.glass(brand, h / 2.0);
    icons::logo(c, 64.0, y + (h - 32.0) / 2.0, 32.0);
    c.text(
        108.0,
        y + h / 2.0,
        Align2::LEFT_CENTER,
        "CatosTV",
        18.0,
        theme::text(),
        Weight::Semibold,
    );

    // Navigation pill, centred.
    let no_overlay = s.overlay().is_none();
    let items: Vec<(usize, TopItem)> = TOP_ITEMS
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, it)| *it != TopItem::Quick)
        .collect();
    let labels: Vec<&str> = items.iter().map(|(_, it)| it.label()).collect();
    let selected = items.iter().position(|(_, it)| it.page() == Some(s.page));
    let focus = (no_overlay && s.top_focus)
        .then(|| items.iter().position(|(i, _)| *i == s.top_index))
        .flatten();
    for ((i, _), rect) in items.iter().zip(widgets::tab_bar(
        c, "top", 640.0, y, h, &labels, selected, focus,
    )) {
        c.hit(rect, Target::Top(*i));
    }

    // Status pill = Control Center: clock, then the network, Bluetooth and
    // sound icons evenly spaced; the pill fits its contents (the
    // clock used to touch the first icon while the others drifted apart).
    let quick = TOP_ITEMS.len() - 1;
    let clock = clock(v.ctx.prefs.clock_24h);
    let time_w = c.text_width(&clock.time, 17.0, Weight::Semibold);
    let (pad, gap, icon_step) = (22.0, 22.0, 30.0);
    let w = pad + time_w + gap + 3.0 * icon_step - 6.0 + pad;
    let rect = c.r(1228.0 - w, y, w, h);
    let t = focus_t(
        c,
        ("top", quick),
        no_overlay && s.top_focus && s.top_index == quick,
    );
    c.glass(rect, h / 2.0);
    if t > 0.01 {
        widgets::bar_pill(c, rect, h / 2.0, t);
    }
    c.hit(rect, Target::Top(quick));
    let ink = widgets::mix(theme::text(), theme::on_accent(), t);
    let cy = y + h / 2.0;
    let x0 = 1228.0 - w + pad;
    c.text(
        x0,
        cy,
        Align2::LEFT_CENTER,
        &clock.time,
        17.0,
        ink,
        Weight::Semibold,
    );
    let sys = v.ctx.sys;
    let online = sys.net.as_ref().is_some_and(|n| n.online);
    let muted = sys.volume.is_some_and(|v| v.muted);
    let dim = |on: bool| {
        let base = if on { theme::text2() } else { theme::text3() };
        widgets::mix(base, theme::on_accent(), t)
    };
    let wired = sys.net.as_ref().is_some_and(|n| n.wired());
    let ix = x0 + time_w + gap + 12.0;
    icons::draw(
        c,
        if wired { "ethernet" } else { "wifi" },
        ix,
        cy,
        16.0,
        dim(online),
    );
    icons::draw(
        c,
        "bluetooth",
        ix + icon_step,
        cy,
        15.0,
        dim(sys.bluetooth.powered),
    );
    icons::draw(
        c,
        if muted { "muted" } else { "speaker" },
        ix + 2.0 * icon_step,
        cy,
        16.0,
        dim(!muted),
    );
}

// --------------------------------------------------------------------- home

fn draw_home(c: &C, v: &ShellView) {
    let s = v.shell;
    let apps = v.ctx.apps;
    let ease = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        1.0 - (1.0 - x).powi(3)
    };
    let bounce_of = |app: &AppView| {
        v.launching
            .filter(|(id, _)| *id == app.entry.id)
            .map_or(0.0, |(_, secs)| widgets::launch_bounce(secs))
    };
    // Down from the apps: the "Continue watching" row (Catfyn)
    // slides up, the apps slide away, and the focused show fills the hero.
    let media = &v.ctx.media.home;
    let server = v
        .ctx
        .media
        .accounts
        .home()
        .map(|a| a.server.as_str())
        .unwrap_or("");
    let pics = crate::ui::fin::FinPics {
        images: v.images,
        cache: v.fin_cache,
        wanted: v.fin_wanted,
    };
    let in_media = s.home_media.is_some() && !media.is_empty();
    let shift = c.anim("home-shift", if in_media { 1.0 } else { 0.0 });
    if let Some(it) = s.home_media.and_then(|(r, i)| v.ctx.media.item(r, i)) {
        crate::ui::fin::home_backdrop(c, &pics, server, it);
        crate::ui::fin::home_hero(c, it);
    }
    // Hero: the focused app, large. No marketing copy, no "X is running"
    // banner and no "Android" / "Store app" label
    // an app is an app.
    if shift < 0.5
        && let Some(app) = apps.get(s.rail_index)
    {
        let k = ease(c.appear(("hero", &app.entry.id), 0.35));
        let name = c.fit(&app.entry.name, 760.0, 58.0, Weight::Semibold);
        c.text(
            64.0 - 14.0 * (1.0 - k),
            150.0,
            Align2::LEFT_TOP,
            &name,
            58.0,
            theme::rgba(theme::INK, (255.0 * k) as u8),
            Weight::Semibold,
        );
        let sub = &app.entry.subtitle;
        if !sub.is_empty() && sub != "Android" {
            c.text(
                66.0 - 14.0 * (1.0 - k),
                226.0,
                Align2::LEFT_TOP,
                sub,
                19.0,
                theme::rgba(theme::INK, (165.0 * k) as u8),
                Weight::Regular,
            );
        }
        // Open apps: a green dot after the name, no label.
        if app.running {
            let nw = c.text_width(&name, 58.0, Weight::Semibold);
            widgets::running_dot(c, 64.0 + nw + 22.0, 190.0);
        }
        if !app.installed {
            widgets::chip(c, 64.0, 272.0, "Not installed", Some(theme::WARNING));
        }
        // Big frosted icon card on the right; it bounces with the tile
        // while the app opens.
        let lift = bounce_of(app);
        let card = c.r(946.0, 118.0 - 26.0 * lift, 220.0, 220.0);
        let tint = theme::app_rgb(&app.entry.accent);
        c.shadow(card, 52.0, 18.0 + 10.0 * lift, 60.0, theme::rgba(tint, 80));
        c.fill(card, 52.0, theme::rgba(theme::BG_TOP, 150));
        c.fill(card, 52.0, theme::rgba(tint, 52));
        c.frost_edge(card, 52.0, 1.6);
        widgets::app_icon(
            c,
            v.images,
            app,
            1056.0,
            228.0 - 26.0 * lift,
            104.0 * (0.92 + 0.08 * k),
            255,
        );
    }

    let up = 540.0 * shift;
    let rail = c.clipped(0.0, 90.0, 1280.0, 630.0);
    widgets::section(&rail, 64.0, 404.0 - up, "Apps");
    let (tw, th, gap) = (208.0, 117.0, 24.0);
    let step = tw + gap;
    let visible = 5usize;
    let first = s.rail_index.saturating_sub(visible - 1) as f32;
    let scroll = c.anim("rail-scroll", first * step);
    let rail_focus = s.overlay().is_none() && !s.top_focus && !in_media;
    for (i, app) in apps.iter().enumerate() {
        let x = 64.0 + i as f32 * step - scroll;
        if x < -tw || x > 1280.0 {
            continue;
        }
        // Tiles rise into place one after another when Home opens.
        let shown = ease(v.page_t * 2.2 - (i as f32 - first).max(0.0) * 0.12);
        let y = 428.0 + 26.0 * (1.0 - shown) - up;
        let rect = rail.r(x, y, tw, th);
        let t = focus_t(c, ("rail", i), rail_focus && s.rail_index == i);
        widgets::home_glow(
            &rail,
            widgets::lifted(&rail, rect, t),
            22.0,
            t,
            theme::app_rgb(&app.entry.accent),
        );
        widgets::app_tile_lift(&rail, v.images, rect, app, t, bounce_of(app));
        if !in_media {
            c.hit(c.r(x, 428.0, tw, th + 36.0), Target::Rail(i));
        }
    }
    if !media.is_empty() {
        let focus = if in_media { s.home_media } else { None };
        crate::ui::fin::home_rows(c, &pics, server, media, focus, 604.0 - 254.0 * shift);
    }
    if apps.is_empty() {
        c.text(
            64.0,
            470.0,
            Align2::LEFT_TOP,
            "Nothing here yet. Find apps in the Store",
            17.0,
            theme::text3(),
            Weight::Regular,
        );
    }
}

// ------------------------------------------------------------------ recents

/// Recents: the open apps as cards in a row you slide through.
/// A card is a preview of what the app showed last
/// (captured while it was in front), in the screen's own 16:9 shape,
/// never stretched, and the focused app's preview, softly blurred, fills
/// the page behind. An app not seen yet shows its icon on a glow of its own
/// colour instead. Under each card: name, CPU and memory; the
/// whole system's use sits by the title. An X above each card, "Close all"
/// under the row.
fn draw_recents(c: &C, v: &ShellView) {
    let s = v.shell;
    let ctx = v.ctx;
    let running = ctx.running();
    let n = running.len();
    let active = s.overlay().is_none() && !s.top_focus;
    let on_all = s.recents_index >= n;
    let card_i = if on_all {
        s.recents_card
    } else {
        s.recents_index
    }
    .min(n.saturating_sub(1));
    let thumb = |id: &str| {
        v.thumbs
            .get(id)
            .map(|(shot, blur)| (v.images.get(&c.ctx, shot), v.images.get(&c.ctx, blur)))
    };

    // Backdrop: the focused app's preview, blurred; else its colour as a
    // big soft light.
    if let Some(&i) = running.get(card_i) {
        let a = &ctx.apps[i];
        let fade = c.appear(("recents-bg", &a.entry.id), 0.4);
        if let Some((_, Some(blur))) = thumb(&a.entry.id) {
            c.image(
                c.r(-20.0, -20.0, 1320.0, 760.0),
                blur,
                theme::rgba(theme::WHITE, (255.0 * fade) as u8),
            );
            c.fill(c.screen, 0.0, theme::rgba(theme::BG_TOP, 150));
        } else {
            let tint = theme::app_rgb(&a.entry.accent);
            c.glow_ellipse(640.0, 420.0, 760.0, 360.0, 0.0, tint, (90.0 * fade) as u8);
            c.glow_ellipse(
                640.0,
                420.0,
                380.0,
                200.0,
                0.0,
                widgets::lighten(tint, 0.3),
                (50.0 * fade) as u8,
            );
            c.fill(c.screen, 0.0, theme::rgba(theme::BG_TOP, 60));
        }
    }

    widgets::title(c, 64.0, 112.0, "Recents", 40.0);
    // The whole system's use, by the title.
    if v.usage.mem_total > 0 {
        let text = format!(
            "CPU {:.0}%   ·   Memory {} of {}",
            v.usage.cpu,
            catostv_core::usage::bytes_short(v.usage.mem_used),
            catostv_core::usage::bytes_short(v.usage.mem_total)
        );
        let tw = c.text_width(&text, 16.0, Weight::Medium);
        let rect = c.r(1216.0 - tw - 40.0, 117.0, tw + 40.0, 40.0);
        c.glass(rect, 20.0);
        c.text(
            1216.0 - 20.0,
            137.0,
            Align2::RIGHT_CENTER,
            &text,
            16.0,
            theme::text2(),
            Weight::Medium,
        );
    }

    if n == 0 {
        let cy = 400.0;
        c.circle(640.0, cy, 58.0, theme::rgba(theme::BG_TOP, 120));
        c.circle(640.0, cy, 58.0, theme::glass());
        c.ring(640.0, cy, 58.0, 1.0, theme::border());
        icons::draw(c, "grid", 640.0, cy, 34.0, theme::text3());
        c.text(
            640.0,
            cy + 98.0,
            Align2::CENTER_CENTER,
            "No background apps",
            22.0,
            theme::text2(),
            Weight::Medium,
        );
        return;
    }

    // The row: the focused card in the middle, others slide past.
    let (cw, ch, gap, y) = (416.0, 234.0, 44.0, 216.0);
    let target = 640.0 - (card_i as f32 * (cw + gap) + cw / 2.0);
    let x0 = c.anim("recents-scroll", target);

    for (k, &i) in running.iter().enumerate() {
        let a = &ctx.apps[i];
        let x = x0 + k as f32 * (cw + gap);
        if x > 1320.0 || x + cw < -40.0 {
            continue;
        }
        let here = s.recents_index == k && active;
        let t = focus_t(c, ("recent", k), here && !s.recents_close);
        let near = focus_t(c, ("recent-near", k), card_i == k);
        // Cards away from the centre shrink and dim a little.
        let scale = 0.86 + 0.14 * near;
        let (w, h) = (cw * scale, ch * scale);
        let (rx, ry) = (x + (cw - w) / 2.0, y + (ch - h) / 2.0);
        // Focus: lifted with a soft shadow and a sheen.
        let rect = widgets::lifted(c, c.r(rx, ry, w, h), t);
        let radius = 26.0;
        let tint = theme::app_rgb(&a.entry.accent);
        widgets::focus_lift(c, rect, radius, t);
        let (dx, dy) = c.to_design(rect.min);
        let (dw, dh) = c.dsize(rect);
        let (cx, cy) = (dx + dw / 2.0, dy + dh / 2.0);
        let shade = (150.0 + 105.0 * near) as u8;
        if let Some((Some(shot), _)) = thumb(&a.entry.id) {
            c.fill(rect, radius, theme::rgba(theme::BLACK, 255));
            c.image_rounded(rect, radius, shot, theme::rgba(theme::WHITE, shade));
            // The app's icon as a small badge in the corner.
            let (bx, by) = (dx + 34.0, dy + dh - 34.0);
            c.circle(bx, by, 22.0, theme::rgba(theme::BG_TOP, 200));
            c.ring(bx, by, 22.0, 1.0, theme::rgba(theme::WHITE, 40));
            widgets::app_icon(c, v.images, a, bx, by, 28.0, shade);
        } else {
            // Frosted colour: the app's colour as soft light inside the card.
            c.fill(rect, radius, theme::rgba(theme::BG_TOP, 170));
            c.fill(rect, radius, theme::rgba(tint, (48.0 + 30.0 * near) as u8));
            c.glow_ellipse(
                cx,
                cy,
                dw * 0.42,
                dh * 0.42,
                0.0,
                widgets::lighten(tint, 0.35),
                (70.0 * near) as u8,
            );
            widgets::app_icon(c, v.images, a, cx, cy, 96.0 * scale, shade);
        }
        c.frost_edge(rect, radius, 1.0);
        widgets::focus_sheen(c, rect, radius, t);
        // Name, then CPU and memory, under the card.
        let name = c.fit(&a.entry.name, cw - 20.0, 19.0, Weight::Semibold);
        c.text(
            x + cw / 2.0,
            y + ch + 30.0,
            Align2::CENTER_CENTER,
            &name,
            19.0,
            theme::rgba(theme::INK, (120.0 + 135.0 * near) as u8),
            Weight::Semibold,
        );
        if let Some(u) = v.usage.apps.get(&a.entry.id) {
            let line = format!(
                "CPU {:.0}%   ·   {}",
                u.cpu.max(0.0),
                catostv_core::usage::bytes_short(u.rss)
            );
            c.text(
                x + cw / 2.0,
                y + ch + 58.0,
                Align2::CENTER_CENTER,
                &line,
                15.0,
                theme::rgba(theme::INK, (70.0 + 110.0 * near) as u8),
                Weight::Medium,
            );
        }
        c.hit(rect, Target::RecentCard(k));

        // The X above the card.
        let xt = focus_t(c, ("recent-x", k), here && s.recents_close);
        let show = (0.35 + 0.65 * near.max(xt)).min(1.0);
        let (bx, by, br) = (cx, dy - 32.0, 20.0 + 4.0 * xt);
        if xt > 0.01 {
            c.glow(bx, by, br * 3.0, theme::DANGER, (70.0 * xt) as u8);
        }
        c.circle(
            bx,
            by,
            br,
            if xt > 0.5 {
                theme::rgb(theme::DANGER)
            } else {
                theme::rgba(theme::BG_TOP, (200.0 * show) as u8)
            },
        );
        c.ring(
            bx,
            by,
            br,
            1.0,
            theme::rgba(theme::WHITE, (50.0 * show) as u8),
        );
        icons::draw(
            c,
            "close",
            bx,
            by,
            15.0,
            theme::rgba(theme::WHITE, (255.0 * show) as u8),
        );
        c.hit(
            c.r(bx - br, by - br, br * 2.0, br * 2.0),
            Target::RecentClose(k),
        );
    }

    // "Close all": a big frosted pill centred under the row.
    {
        let t = focus_t(c, "recent-all", active && on_all);
        let (bw, bh) = (260.0, 64.0);
        let rect = c.r(640.0 - bw / 2.0, 604.0, bw, bh);
        c.fill(rect, bh / 2.0, theme::rgba(theme::BG_TOP, 160));
        c.fill(
            rect,
            bh / 2.0,
            theme::rgba(theme::DANGER, (40.0 + 150.0 * t) as u8),
        );
        c.frost_edge(rect, bh / 2.0, 1.0);
        c.top_highlight(rect, bh / 2.0, theme::rgba(theme::WHITE, (130.0 * t) as u8));
        let label = "Close all";
        let lw = c.text_width(label, 19.0, Weight::Semibold);
        let x = 640.0 - (lw + 30.0) / 2.0;
        icons::draw(c, "close", x + 9.0, 636.0, 15.0, theme::text());
        c.text(
            x + 30.0,
            636.0,
            Align2::LEFT_CENTER,
            label,
            19.0,
            theme::text(),
            Weight::Semibold,
        );
        c.hit(rect, Target::RecentCloseAll);
    }

    // Where you are in the row: small dots under it.
    if n > 1 {
        let dot_w = 18.0;
        let x = 640.0 - (n as f32 - 1.0) * dot_w / 2.0;
        for k in 0..n {
            let on = k == card_i;
            let w = if on { 22.0 } else { 7.0 };
            c.fill(
                c.r(x + k as f32 * dot_w - w / 2.0, 560.0, w, 7.0),
                3.5,
                if on {
                    theme::text()
                } else {
                    theme::rgba(theme::INK, 70)
                },
            );
        }
    }
}

// --------------------------------------------------------------------- apps

fn draw_apps(c: &C, v: &ShellView) {
    let s = v.shell;
    widgets::title(c, 64.0, 96.0, "Apps", 30.0);
    let cols = shell::APPS_GRID_COLS;
    let (w, h, gx) = (174.0, 98.0, 26.0);
    let row_h = h + 58.0;
    let row = s.grid_index / cols;
    let first_row = row.saturating_sub(2) as f32;
    let scroll = c.anim("grid-scroll", first_row * row_h);
    let focus_ok = s.overlay().is_none() && !s.top_focus;
    for (i, app) in v.ctx.apps.iter().enumerate() {
        let x = 52.0 + (i % cols) as f32 * (w + gx);
        let y = 160.0 + (i / cols) as f32 * row_h - scroll;
        if !(90.0..=700.0).contains(&y) {
            continue;
        }
        let t = focus_t(c, ("grid", i), focus_ok && s.grid_index == i);
        widgets::app_tile(c, v.images, c.r(x, y, w, h), app, t);
        c.hit(c.r(x, y, w, h + 34.0), Target::Grid(i));
    }
}

// -------------------------------------------------------------------- store

fn store_icon_or_mono(c: &C, v: &ShellView, item: &StoreItem, cx: f32, cy: f32, size: f32) {
    if let Some(tex) = v
        .store_icons
        .get(&item.id)
        .and_then(|p| v.images.get(&c.ctx, p))
    {
        let r = c.r(cx - size / 2.0, cy - size / 2.0, size, size);
        c.image(r, tex, eframe::egui::Color32::WHITE);
        return;
    }
    let tint = theme::app_rgb(catostv_core::desktop::accent_for(&item.id));
    let r = c.r(cx - size / 2.0, cy - size / 2.0, size, size);
    c.fill(r, size * 0.26, theme::rgba(tint, 70));
    let mono: String = item.name.chars().take(1).collect();
    c.text(
        cx,
        cy,
        Align2::CENTER_CENTER,
        &mono.to_uppercase(),
        size * 0.5,
        theme::text(),
        Weight::Semibold,
    );
}

fn store_status(v: &ShellView, item: &StoreItem) -> (Option<String>, Option<f32>, bool) {
    match v.shell.store_job(&item.id) {
        Some(StoreJob::Installing(f)) => (
            Some(match f {
                Some(f) => format!("Installing {}%", (f * 100.0).round() as u32),
                None => "Installing…".into(),
            }),
            Some(f.unwrap_or(0.0)),
            false,
        ),
        Some(StoreJob::Removing) => (Some("Removing…".into()), None, false),
        None if v.shell.store_installed(v.ctx, &item.id) => (Some("Installed".into()), None, true),
        None => match v.shell.store_log.iter().find(|e| e.item.id == item.id) {
            Some(e) if matches!(e.state, shell::StoreLogState::Failed(_)) => {
                (Some("Failed".into()), None, false)
            }
            Some(e) if e.state == shell::StoreLogState::Removed => {
                (Some("Removed".into()), None, false)
            }
            _ => (None, None, false),
        },
    }
}

fn draw_store(c: &C, v: &ShellView) {
    let s = v.shell;
    let no_overlay = s.overlay().is_none() && !s.top_focus;
    // Category chips, left to right.
    let mut x = 52.0;
    for (i, cat) in STORE_CATS.iter().enumerate() {
        let w = widgets::filter_chip_width(c, cat.label());
        let rect = c.r(x, 100.0, w, 44.0);
        let selected = s.store_cat == i;
        let t = focus_t(c, ("scat", i), no_overlay && !s.store_grid && selected);
        widgets::filter_chip(c, rect, cat.label(), selected, t);
        c.hit(rect, Target::StoreCat(i));
        x += w + 10.0;
    }

    let cat = s.current_store_cat();
    let items = s.store_items(v.ctx);
    let status = match (cat, &s.store_results) {
        (_, _) if !v.ctx.sys.flatpak_available && v.ctx.sys.appliance => {
            Some("Flatpak is not installed".to_string())
        }
        (StoreCat::Search, Scan::Busy) => Some("Searching Flathub…".into()),
        (StoreCat::Search, Scan::Failed(e)) => Some(e.clone()),
        (StoreCat::Search, Scan::Idle) => Some("Press OK to search Flathub".into()),
        (StoreCat::Search, Scan::Done(r)) if r.is_empty() => Some("No apps found".into()),
        (StoreCat::Installed, _) if items.is_empty() => Some("Nothing installed yet".into()),
        (StoreCat::Downloads, _) if items.is_empty() => {
            Some("Apps you install show up here".into())
        }
        _ => None,
    };
    let mut top = 176.0;
    if cat == StoreCat::Search && !s.store_query.is_empty() {
        widgets::section(c, 56.0, 178.0, &format!("Results for “{}”", s.store_query));
        top = 200.0;
    }
    if let Some(st) = status {
        let busy = cat == StoreCat::Search && matches!(s.store_results, Scan::Busy);
        let mut tx = 56.0;
        if busy {
            widgets::spinner(c, 68.0, top + 16.0, 11.0, v.now);
            tx += 34.0;
        }
        c.text(
            tx,
            top + 6.0,
            Align2::LEFT_TOP,
            &st,
            17.0,
            if busy { theme::text2() } else { theme::text3() },
            Weight::Regular,
        );
    }
    let cols = STORE_COLS as f32;
    let gap = 20.0;
    let w = (1176.0 - gap * (cols - 1.0)) / cols;
    let h = 168.0;
    let row = s.store_index / STORE_COLS;
    let scroll = c.anim("store-scroll", row.saturating_sub(1) as f32 * (h + gap));
    for (i, item) in items.iter().enumerate() {
        let x = 52.0 + (i % STORE_COLS) as f32 * (w + gap);
        let y = top + (i / STORE_COLS) as f32 * (h + gap) - scroll;
        if !(150.0..=700.0).contains(&y) {
            continue;
        }
        let t = focus_t(
            c,
            ("sitem", i),
            no_overlay && s.store_grid && s.store_index == i,
        );
        let rect = widgets::grow(c.r(x, y, w, h), t);
        widgets::card(c, rect, 26.0, t);
        c.hit(c.r(x, y, w, h), Target::StoreItem(i));
        store_icon_or_mono(c, v, item, x + 50.0, y + 50.0, 56.0);
        c.text(
            x + 92.0,
            y + 38.0,
            Align2::LEFT_CENTER,
            &c.fit(&item.name, w - 108.0, 18.0, Weight::Semibold),
            18.0,
            theme::text(),
            Weight::Semibold,
        );
        if !item.blurb.is_empty() {
            c.paragraph(
                x + 92.0,
                y + 52.0,
                w - 112.0,
                &c.fit(&item.blurb, (w - 112.0) * 1.8, 13.5, Weight::Regular),
                13.5,
                theme::text3(),
                Weight::Regular,
            );
        }
        // Action pill along the bottom.
        let pill = c.r(x + 18.0, y + h - 50.0, w - 36.0, 34.0);
        let (label, frac, installed) = store_status(v, item);
        c.fill(
            pill,
            17.0,
            theme::rgba(theme::WHITE, (14.0 + 16.0 * t) as u8),
        );
        if let Some(f) = frac {
            let pw = (w - 36.0) * f.clamp(0.0, 1.0);
            if pw > 1.0 {
                c.fill(
                    c.r(x + 18.0, y + h - 50.0, pw.max(34.0), 34.0),
                    17.0,
                    theme::accent_a(120),
                );
            }
        }
        let py = y + h - 33.0;
        let (left, left_col) = match &label {
            Some(l) if installed => (l.as_str(), theme::rgb(theme::SUCCESS)),
            Some(l) => (l.as_str(), theme::text()),
            None => ("Flathub", theme::text3()),
        };
        c.text(
            x + 34.0,
            py,
            Align2::LEFT_CENTER,
            left,
            13.5,
            left_col,
            Weight::Medium,
        );
        if frac.is_none() {
            let right = if installed { "Open" } else { "Get" };
            let rc = if t > 0.5 {
                theme::text()
            } else {
                theme::text2()
            };
            icons::draw(c, "chevron", x + w - 38.0, py, 12.0, rc);
            c.text(
                x + w - 50.0,
                py,
                Align2::RIGHT_CENTER,
                right,
                13.5,
                rc,
                Weight::Semibold,
            );
        }
    }
}

// ------------------------------------------------------------------- search

fn draw_search(c: &C, v: &ShellView) {
    let s = v.shell;
    let kb_focus = s.overlay().is_none() && !s.top_focus && s.search_focus == SearchFocus::Keyboard;
    widgets::text_field(
        c,
        c.r(52.0, 96.0, 1176.0, 64.0),
        &s.search_osk,
        "Search apps, settings and the store",
        kb_focus,
        true,
    );
    let panel = c.r(52.0, 184.0, 600.0, widgets::keys_height() + 48.0);
    c.glass(panel, 30.0);
    widgets::keys(
        c,
        &s.search_osk,
        76.0,
        208.0,
        552.0,
        kb_focus,
        "search-osk",
        &|r, col| Target::SearchKey(r, col),
    );

    let hits = s.search_results(v.ctx);
    let title = if s.search_osk.text().is_empty() {
        "All apps"
    } else {
        "Results"
    };
    widgets::section(c, 684.0, 196.0, title);
    if hits.is_empty() {
        c.text(
            684.0,
            222.0,
            Align2::LEFT_TOP,
            "No matches",
            17.0,
            theme::text3(),
            Weight::Regular,
        );
    }
    let res_focus = s.overlay().is_none() && !s.top_focus && s.search_focus == SearchFocus::Results;
    let first = s.search_index.saturating_sub(6);
    for (k, hit) in hits.iter().enumerate().skip(first).take(7) {
        let y = 218.0 + (k - first) as f32 * 68.0;
        let t = focus_t(c, ("hit", k), res_focus && s.search_index == k);
        let (title, subtitle, icon, value) = match hit {
            SearchHit::App(i) => {
                let a = &v.ctx.apps[*i];
                let state = if !a.installed { "Not installed" } else { "" };
                (
                    a.entry.name.clone(),
                    state.to_string(),
                    a.entry.icon.clone(),
                    "",
                )
            }
            SearchHit::Setting {
                category, title, ..
            } => (
                title.clone(),
                format!("Settings · {}", category.title()),
                category.icon().to_string(),
                "",
            ),
            // Not installed yet: OK opens its page with Install first.
            SearchHit::Store(item) => (
                item.name.clone(),
                "Flathub".to_string(),
                "store".to_string(),
                "Install",
            ),
            SearchHit::Media(item) => {
                let what = match item.kind() {
                    catostv_core::jellyfin::Kind::Series => "Show",
                    _ => "Movie",
                };
                let sub = match item.year {
                    Some(y) => format!("Catfyn · {what} · {y}"),
                    None => format!("Catfyn · {what}"),
                };
                (item.title().to_string(), sub, "media".to_string(), "Open")
            }
        };
        let rect = c.r(676.0, y, 552.0, 60.0);
        widgets::list_row(
            c,
            rect,
            &ListRow {
                title: &title,
                subtitle: &subtitle,
                value,
                kind: None,
                icon: Some(&icon),
                disabled: false,
            },
            t,
        );
        if let SearchHit::App(i) = hit
            && v.ctx.apps[*i].running
        {
            widgets::running_dot(c, 1228.0 - 26.0, y + 30.0);
        }
        c.hit(rect, Target::SearchHit(k));
    }
}

// ----------------------------------------------------------------- settings

/// Icon for a settings row (drawn in a round badge).
fn row_icon(id: settings::RowId) -> Option<&'static str> {
    use settings::RowId as R;
    Some(match id {
        R::WifiRadio => "wifi",
        R::NetworkStatus => "globe",
        R::Ethernet => "ethernet",
        R::JoinWifi => "plus",
        R::BtRadio | R::BtDevice(_) => "bluetooth",
        R::BtPair => "plus",
        R::Controller(_) => "game",
        R::Resolution | R::AdaptiveSync | R::Hdr => "display",
        R::Scale => "grid",
        R::PictureFit => "tv",
        R::Volume | R::AudioOutput | R::HdmiAudio => "speaker",
        R::Mute => "muted",
        R::Accent => "palette",
        R::App(_) => return None,
        R::AppSearch => "search",
        R::UpdateApps => "download",
        R::AndroidState | R::AndroidSetup | R::AndroidVariant | R::AndroidCertify => "android",
        R::AndroidRetry => "refresh",
        R::AndroidApk => "usb",
        R::AndroidStore => "store",
        R::AndroidRepair => "refresh",
        R::CastReceiver => "cast",
        R::PhoneRemote | R::KdeConnect | R::KdeDevice(_) => "phone",
        R::KeyGuide => "keyboard",
        R::PadNav | R::PadBind(_) | R::PadBindReset => "game",
        R::About => "spark",
        R::RestartShell => "refresh",
        R::Reboot => "refresh",
        R::PowerOff => "power",
        R::UpdateStatus | R::UpdateCheck | R::UpdateInstall => "download",
        R::Storage => "disk",
        R::ClockFormat => "spark",
        R::Glow => "palette",
        R::IpAddress => "globe",
        R::DebugSsh | R::DebugLogin | R::DebugPassword | R::DebugRemoveOld => "lock",
        R::DebugRelay => "android",
        R::DebugGpu => "display",
        R::DebugBuild => "spark",
    })
}

/// The short status chip at the top right of a settings page (only where
/// it adds something the rows below do not already say).
fn category_status(cat: Category, ctx: &ShellContext) -> Option<(String, Option<theme::Rgb>)> {
    let sys = ctx.sys;
    match cat {
        Category::Network => Some(match sys.net.as_ref().map(|n| n.online) {
            Some(true) => ("Online".into(), Some(theme::SUCCESS)),
            _ => ("Offline".into(), Some(theme::WARNING)),
        }),
        Category::Display => ctx.output().map(|o| (o.display_name(), None)),
        Category::Sound => sys
            .sinks
            .iter()
            .find(|k| k.is_default)
            .map(|k| (k.name.clone(), None)),
        Category::Android => Some(if sys.android.initialized {
            ("Ready".into(), Some(theme::SUCCESS))
        } else {
            ("Not set up".into(), None)
        }),
        _ => None,
    }
}

/// Settings' category list: row pitch and height (design units).
const CAT_STEP: f32 = 49.0;
const CAT_H: f32 = 43.0;

fn draw_settings(c: &C, v: &ShellView) {
    let s = v.shell;
    let no_overlay = s.overlay().is_none();
    widgets::title(c, 64.0, 40.0, "Settings", 30.0);
    for (i, cat) in Category::ALL.iter().enumerate() {
        // All 12 categories (Debug included) end at 678, inside the
        // right panel's 688 edge: 49 px apart.
        let rect = c.r(52.0, 96.0 + i as f32 * CAT_STEP, 268.0, CAT_H);
        let selected = s.settings_cat == i;
        let t = focus_t(
            c,
            ("cat", i),
            no_overlay && !s.settings_rows_focus && selected,
        );
        // The open category keeps a quiet pane; focus is glass, inside
        // the pill.
        if selected {
            c.fill(rect, CAT_H / 2.0, theme::rgba(theme::WHITE, 18));
        }
        widgets::focus_pill(c, rect, CAT_H / 2.0, t);
        c.hit(rect, Target::SettingsCat(i));
        let y = 96.0 + CAT_H / 2.0 + i as f32 * CAT_STEP;
        icons::draw(
            c,
            cat.icon(),
            82.0,
            y,
            18.0,
            if selected {
                theme::accent()
            } else {
                theme::text3()
            },
        );
        c.text(
            108.0,
            y,
            Align2::LEFT_CENTER,
            cat.title(),
            16.0,
            if selected {
                theme::text()
            } else {
                theme::text2()
            },
            // One weight for every entry: the pill and colour mark the
            // selected one, so the list doesn't change size as you move.
            Weight::Medium,
        );
    }

    let right = c.r(344.0, 32.0, 884.0, 656.0);
    c.glass(right, 32.0);
    let cat = s.current_category();
    widgets::title(c, 384.0, 60.0, cat.title(), 28.0);
    if let Some((label, dot)) = category_status(cat, v.ctx) {
        let label = c.fit(&label, 300.0, 13.0, Weight::Medium);
        let w = widgets::chip_width(c, &label, dot.is_some());
        widgets::chip(c, 1188.0 - w, 66.0, &label, dot);
    }
    let rows = s.settings_rows(v.ctx);
    // 8 rows end at 670, inside the 688 panel edge (nothing on or
    // past the glass edge).
    let (row_h, gap, top) = (60.0, 10.0, 120.0);
    let step = row_h + gap;
    let visible = 8usize;
    // Smooth scrolling: the list glides to keep the focused row on screen,
    // drawn through a clip so rows slide in and out at the edges.
    let first = s.settings_row.saturating_sub(visible - 1);
    let scroll = c.anim(("settings-scroll", cat.title()), first as f32 * step);
    // Clipped to the glass: rows slide in and out under the panel's top
    // and bottom margins, never past its edge.
    let list = c.clipped(356.0, top - 8.0, 860.0, 682.0 - (top - 8.0));
    // One focus highlight that glides from row to row, drawn under
    // the rows' text; it fades when focus leaves the list.
    let rows_on = c.anim(
        ("settings-rows-focus", cat.title()),
        if no_overlay && s.settings_rows_focus {
            1.0
        } else {
            0.0
        },
    );
    if rows_on > 0.01 && !rows.is_empty() {
        let at = c.anim(
            ("settings-highlight", cat.title()),
            s.settings_row.min(rows.len() - 1) as f32 * step,
        );
        let primary = rows
            .get(s.settings_row)
            .is_some_and(|r| r.kind == RowKind::Primary);
        if !primary {
            let hl = list.r(368.0, top + at - scroll, 836.0, row_h);
            widgets::focus_pill(&list, hl, widgets::ROW_RADIUS, rows_on);
        }
    }
    let from = (scroll / step).floor().max(0.0) as usize;
    for (k, row) in rows.iter().enumerate().skip(from).take(visible + 1) {
        let y = top + k as f32 * step - scroll;
        let t = focus_t(
            c,
            ("row", cat.title(), k),
            no_overlay && s.settings_rows_focus && s.settings_row == k,
        );
        let rect = list.r(368.0, y, 836.0, row_h);
        let accent_row = row.id == settings::RowId::Accent;
        // Apps show their own icon.
        let app = match row.id {
            settings::RowId::App(i) => v.ctx.apps.get(i),
            _ => None,
        };
        widgets::list_row_under_highlight(
            &list,
            rect,
            &ListRow {
                title: &row.title,
                subtitle: &row.subtitle,
                value: if accent_row { "" } else { &row.value },
                kind: Some(if accent_row { RowKind::Info } else { row.kind }),
                icon: if app.is_some() {
                    Some("")
                } else {
                    row_icon(row.id)
                },
                disabled: row.kind == RowKind::Soon,
            },
            t,
        );
        if let Some(a) = app {
            widgets::app_icon(
                &list,
                v.images,
                a,
                368.0 + 20.0 + 19.84,
                y + row_h / 2.0,
                26.0,
                255,
            );
        }
        if accent_row {
            let pick = if s.settings_rows_focus && s.settings_row == k {
                s.accent_pick
            } else {
                None
            };
            draw_accent_swatches(&list, v, 1180.0, y + row_h / 2.0, pick);
        }
        if y > top - row_h * 0.5 && y < top + visible as f32 * step - row_h * 0.5 {
            c.hit(rect, Target::SettingsRow(k));
        }
    }
    // Updates: the release notes as one box under the buttons.
    if cat == Category::Updates
        && let Some(Ok(check)) = &v.ctx.sys.updates.check
        && let Some((version, notes)) = catostv_core::updates::shown_notes(check)
        && !notes.is_empty()
    {
        let y0 = top + rows.len() as f32 * step + 14.0;
        let h = (58.0 + notes.len() as f32 * 34.0).min(668.0 - y0);
        if h > 70.0 {
            let rect = list.r(368.0, y0, 836.0, h);
            list.fill(rect, widgets::ROW_RADIUS, theme::rgba(theme::WHITE, 10));
            list.frost_edge(rect, widgets::ROW_RADIUS, 0.5);
            list.text(
                392.0,
                y0 + 28.0,
                Align2::LEFT_CENTER,
                &format!("What's new in {version}"),
                17.0,
                theme::text(),
                Weight::Semibold,
            );
            for (i, n) in notes.iter().enumerate() {
                let ny = y0 + 62.0 + i as f32 * 34.0;
                if ny > y0 + h - 12.0 {
                    break;
                }
                list.circle(400.0, ny, 3.5, theme::accent());
                list.text(
                    416.0,
                    ny,
                    Align2::LEFT_CENTER,
                    &list.fit(n, 770.0, 15.5, Weight::Regular),
                    15.5,
                    theme::text2(),
                    Weight::Regular,
                );
            }
        }
    }
}

/// The accent swatches: the current colour has a check; `pick` (Left/Right
/// on the row, before OK) a ring.
fn draw_accent_swatches(c: &C, v: &ShellView, right: f32, cy: f32, pick: Option<usize>) {
    let current = &v.ctx.prefs.accent;
    let n = prefs::ACCENTS.len() as f32;
    let step = 34.0;
    let x0 = right - (n - 1.0) * step - 12.0;
    for (i, (name, _)) in prefs::ACCENTS.iter().enumerate() {
        let x = x0 + i as f32 * step;
        let col = theme::accent_rgb_of(name);
        if name == current {
            c.shadow(
                c.r(x - 12.0, cy - 12.0, 24.0, 24.0),
                12.0,
                0.0,
                16.0,
                theme::rgba(col, 140),
            );
            c.circle(x, cy, 12.0, theme::rgb(col));
            c.ring(x, cy, 16.0, 2.0, theme::text());
            icons::draw(c, "check", x, cy, 12.0, theme::rgb((12, 14, 22)));
        } else if pick == Some(i) {
            c.circle(x, cy, 12.0, theme::rgb(col));
            c.ring(x, cy, 16.0, 2.0, theme::text());
        } else {
            c.circle(x, cy, 10.0, theme::rgb(col));
        }
    }
}

// ----------------------------------------------------------------- overlays

fn quick_tile(item: QuickItem, ctx: &ShellContext) -> (&'static str, &'static str, String, bool) {
    let sys = ctx.sys;
    let on_off = |v: Option<bool>| match v {
        Some(true) => "On".to_string(),
        Some(false) => "Off".to_string(),
        None => "-".to_string(),
    };
    match item {
        // what it is connected to, like Android's tiles.
        QuickItem::Network => {
            let wired = sys.ethernet.as_ref().is_some_and(|e| e.connected);
            let wifi = sys
                .net
                .as_ref()
                .and_then(|n| {
                    n.connections.iter().find(|c| {
                        c.kind != "ethernet" && c.kind != "loopback" && c.kind != "bridge"
                    })
                })
                .map(|c| c.name.clone());
            (
                if wired { "ethernet" } else { "wifi" },
                "Network",
                match (wired, wifi, sys.wifi_on) {
                    (true, _, _) => "Ethernet".into(),
                    (false, Some(n), _) => n,
                    (false, None, Some(false)) => "Wi-Fi off".into(),
                    _ => "Not connected".into(),
                },
                wired || sys.net.as_ref().is_some_and(|n| n.online),
            )
        }
        QuickItem::Bluetooth => {
            let connected: Vec<_> = sys.bt_details.iter().filter(|d| d.connected).collect();
            (
                "bluetooth",
                "Bluetooth",
                if !sys.bluetooth.available {
                    "Unavailable".into()
                } else if !sys.bluetooth.powered {
                    "Off".into()
                } else {
                    match connected.as_slice() {
                        [] => "On".into(),
                        [one] => match one.battery {
                            Some(b) => format!("{} · {b}%", one.name),
                            None => one.name.clone(),
                        },
                        many => format!("{} devices", many.len()),
                    }
                },
                sys.bluetooth.powered,
            )
        }
        QuickItem::Mute => {
            let m = sys.volume.map(|v| v.muted);
            ("muted", "Mute", on_off(m), m == Some(true))
        }
        QuickItem::Settings => ("gear", "Settings", String::new(), false),
        QuickItem::Sleep => ("moon", "Sleep", String::new(), false),
        QuickItem::Power => ("power", "Power", String::new(), false),
    }
}

/// Modal header: optional icon + title. Returns the y below it.
/// Reset Android's confirmation. The copy sentence matches
/// `android-keystore-check --force`: the data folder is moved to
/// `data.previous-<stamp>`, and the copy before it is deleted.
const RESET_TITLE: &str = "Reset Android?";
const RESET_TEXT: &str = "This removes all Android apps, sign-ins and app data, and sets Android up again. It takes a few minutes. CatosTV, your media and your settings aren't affected. A copy of the previous Android data is kept until the next reset. Afterwards, Google Play needs this TV registered again; Settings → Android shows the new ID.";

fn modal_header(c: &C, mx: f32, my: f32, icon: Option<&str>, title: &str, w: f32) -> f32 {
    let mut tx = mx + 36.0;
    if let Some(i) = icon {
        icons::draw(c, i, mx + 50.0, my + 50.0, 24.0, theme::accent());
        tx = mx + 76.0;
    }
    c.text(
        tx,
        my + 50.0,
        Align2::LEFT_CENTER,
        &c.fit(title, w - (tx - mx) - 36.0, 24.0, Weight::Semibold),
        24.0,
        theme::text(),
        Weight::Semibold,
    );
    my + 92.0
}

/// Vertical list of buttons; registers `Target::Item(k)`.
fn button_column(
    c: &C,
    id: &str,
    x: f32,
    y: f32,
    w: f32,
    labels: &[(&str, ButtonStyle)],
    index: usize,
) {
    for (k, (label, style)) in labels.iter().enumerate() {
        let t = focus_t(c, (id, k), index == k);
        let rect = c.r(x, y + k as f32 * 62.0, w, 52.0);
        widgets::button(c, rect, label, *style, t);
        c.hit(rect, Target::Item(k));
    }
}

/// Height of a list modal showing `rows` rows (at most `visible`).
/// Height of the "searching" radar shown in an empty Wi-Fi / Bluetooth list.
const RADAR_H: f32 = 220.0;

fn list_modal_h(rows: usize, visible: usize) -> f32 {
    132.0 + rows.clamp(1, visible) as f32 * 60.0
}

/// Scrollable list of rows in a modal; registers `Target::Item(k)`.
/// A Quick settings panel: the same sheet as Quick settings, with
/// a title and a back hint. Returns the y where its rows start.
fn qs_panel(c: &C, icon: &str, title: &str) -> f32 {
    c.scrim();
    let panel = c.r(836.0, 20.0, 424.0, 680.0);
    c.sheet(panel, 32.0);
    c.hit(panel, Target::Item(usize::MAX));
    icons::draw(c, "back", 880.0, 66.0, 16.0, theme::text3());
    icons::draw(c, icon, 916.0, 66.0, 20.0, theme::accent());
    c.text(
        940.0,
        66.0,
        Align2::LEFT_CENTER,
        title,
        24.0,
        theme::text(),
        Weight::Semibold,
    );
    112.0
}

#[allow(clippy::too_many_arguments)]
fn modal_list(
    c: &C,
    id: &str,
    x: f32,
    y: f32,
    w: f32,
    rows: &[ListRow],
    index: usize,
    visible: usize,
) {
    let first = index.saturating_sub(visible - 1);
    // One highlight gliding between rows, like Settings.
    if index < rows.len() && rows[index].kind != Some(RowKind::Primary) {
        let at = c.anim((id, "highlight"), (index - first) as f32 * 60.0);
        widgets::focus_pill(c, c.r(x, y + at, w, 52.0), widgets::ROW_RADIUS, 1.0);
    }
    for (k, row) in rows.iter().enumerate().skip(first).take(visible) {
        let t = focus_t(c, (id, k), index == k);
        let rect = c.r(x, y + (k - first) as f32 * 60.0, w, 52.0);
        widgets::list_row_under_highlight(c, rect, row, t);
        c.hit(rect, Target::Item(k));
    }
}

fn status_line(c: &C, x: f32, y: f32, text: &str) {
    if !text.is_empty() {
        c.text(
            x,
            y,
            Align2::LEFT_CENTER,
            text,
            14.0,
            theme::text3(),
            Weight::Regular,
        );
    }
}

fn draw_overlay(c: &C, v: &ShellView, overlay: &Overlay) {
    let s = v.shell;
    let ctx = v.ctx;
    match overlay {
        Overlay::Quick { index } => {
            c.scrim();
            let panel = c.r(
                836.0,
                20.0,
                424.0,
                if s.quick_offset() > 0 { 668.0 } else { 572.0 },
            );
            c.sheet(panel, 32.0);
            c.hit(panel, Target::Item(usize::MAX));
            let clock = clock(v.ctx.prefs.clock_24h);
            c.text(
                872.0,
                52.0,
                Align2::LEFT_TOP,
                &clock.time,
                46.0,
                theme::text(),
                Weight::Semibold,
            );
            c.text(
                874.0,
                110.0,
                Align2::LEFT_TOP,
                &clock.date,
                15.0,
                theme::text2(),
                Weight::Regular,
            );
            // Opened from inside an app: its controls first.
            let off = s.quick_offset();
            let mut top = 150.0;
            if off > 0 {
                let (name, fullscreen) =
                    s.quick_app.clone().unwrap_or_else(|| ("App".into(), false));
                c.text(
                    870.0,
                    146.0,
                    Align2::LEFT_CENTER,
                    &c.fit(&name, 340.0, 13.0, Weight::Semibold),
                    13.0,
                    theme::text3(),
                    Weight::Semibold,
                );
                let items = [
                    ("back", "Back"),
                    (
                        if fullscreen { "shrink" } else { "expand" },
                        if fullscreen {
                            "Windowed"
                        } else {
                            "Full screen"
                        },
                    ),
                    ("close", "Close"),
                ];
                for (k, (icon, label)) in items.iter().enumerate() {
                    let x = 868.0 + k as f32 * 120.0;
                    let t = focus_t(c, ("quick-app", k), *index == k);
                    let base = c.r(x, 162.0, 112.0, 64.0);
                    let rect = widgets::grow(base, t);
                    widgets::card(c, rect, 20.0, t);
                    c.hit(base, Target::Item(k));
                    let col = if k == 2 && t > 0.5 {
                        theme::rgb(theme::DANGER)
                    } else {
                        theme::text()
                    };
                    icons::draw(c, icon, x + 56.0, 182.0, 16.0, col);
                    c.text(
                        x + 56.0,
                        208.0,
                        Align2::CENTER_CENTER,
                        label,
                        12.5,
                        if t > 0.5 {
                            theme::text()
                        } else {
                            theme::text2()
                        },
                        Weight::Medium,
                    );
                }
                top = 246.0;
            }
            for (i, item) in QUICK_ITEMS.iter().enumerate() {
                let (icon, label, state, active) = quick_tile(*item, ctx);
                let x = 868.0 + (i % QUICK_COLS) as f32 * 184.0;
                let y = top + (i / QUICK_COLS) as f32 * 108.0;
                let t = focus_t(c, ("quick", i), *index == off + i);
                let base = c.r(x, y, 172.0, 96.0);
                let rect = widgets::grow(base, t);
                widgets::card(c, rect, 24.0, t);
                c.hit(base, Target::Item(off + i));
                widgets::icon_badge(c, x + 36.0, y + 48.0, 20.0, icon, active, t);
                let ty = if state.is_empty() { y + 48.0 } else { y + 38.0 };
                c.text(
                    x + 66.0,
                    ty,
                    Align2::LEFT_CENTER,
                    &c.fit(label, 96.0, 15.0, Weight::Medium),
                    15.0,
                    theme::text(),
                    Weight::Medium,
                );
                if !state.is_empty() {
                    c.text(
                        x + 66.0,
                        y + 60.0,
                        Align2::LEFT_CENTER,
                        &c.fit(&state, 96.0, 12.5, Weight::Regular),
                        12.5,
                        theme::text3(),
                        Weight::Regular,
                    );
                }
            }
            if let Some(vol) = ctx.sys.volume {
                let vi = off + QUICK_ITEMS.len();
                let t = focus_t(c, ("quick", vi), *index == vi);
                let vy = top + 338.0;
                let base = c.r(868.0, vy, 360.0, 56.0);
                let bar = widgets::grow(base, t * 0.4);
                widgets::card(c, bar, 28.0, t);
                c.hit(base, Target::Item(vi));
                icons::draw(
                    c,
                    if vol.muted { "muted" } else { "speaker" },
                    896.0,
                    vy + 28.0,
                    17.0,
                    theme::text2(),
                );
                widgets::level(
                    c,
                    920.0,
                    vy + 25.0,
                    240.0,
                    vol.percent as f32 / 100.0,
                    if vol.muted {
                        theme::text3()
                    } else {
                        theme::accent()
                    },
                );
                c.text(
                    1206.0,
                    vy + 28.0,
                    Align2::RIGHT_CENTER,
                    &format!("{}%", vol.percent),
                    14.0,
                    theme::text2(),
                    Weight::Medium,
                );
            }
        }
        Overlay::AppOptions { app, index } => {
            let Some(a) = ctx.apps.get(*app) else { return };
            let opts = shell::app_options(a.running, shell::is_removable(ctx, *app));
            let h = 150.0 + opts.len() as f32 * 62.0;
            let m = widgets::modal(c, 440.0, h);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let tile = c.r(mx + 32.0, my + 30.0, 64.0, 64.0);
            c.fill(tile, 18.0, theme::rgba(theme::app_rgb(&a.entry.accent), 70));
            widgets::app_icon(c, v.images, a, mx + 64.0, my + 62.0, 34.0, 255);
            c.text(
                mx + 114.0,
                my + 50.0,
                Align2::LEFT_CENTER,
                &c.fit(&a.entry.name, 290.0, 22.0, Weight::Semibold),
                22.0,
                theme::text(),
                Weight::Semibold,
            );
            let state = if a.installed { "" } else { "Not installed" };
            let mut sx = mx + 114.0;
            if a.running {
                widgets::running_dot(c, sx + 5.0, my + 76.0);
                sx += 18.0;
            }
            status_line(c, sx, my + 76.0, state);
            let labels: Vec<(&str, ButtonStyle)> = opts
                .iter()
                .map(|o| {
                    (
                        o.label(),
                        match o {
                            AppOption::Close | AppOption::Uninstall => ButtonStyle::Danger,
                            AppOption::Open | AppOption::Resume => ButtonStyle::Primary,
                            _ => ButtonStyle::Secondary,
                        },
                    )
                })
                .collect();
            button_column(c, "appopt", mx + 32.0, my + 120.0, 376.0, &labels, *index);
        }
        Overlay::Power { index } => {
            let m = widgets::modal(c, 400.0, 130.0 + POWER_ITEMS.len() as f32 * 62.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let y = modal_header(c, mx, my, Some("power"), "Power", 400.0);
            let labels: Vec<(&str, ButtonStyle)> = POWER_ITEMS
                .iter()
                .map(|p| {
                    (
                        p.label(),
                        match p {
                            shell::PowerItem::PowerOff => ButtonStyle::Danger,
                            shell::PowerItem::Cancel => ButtonStyle::Secondary,
                            _ => ButtonStyle::Primary,
                        },
                    )
                })
                .collect();
            button_column(c, "power", mx + 32.0, y, 336.0, &labels, *index);
        }
        Overlay::WifiList { index } => {
            // Nothing found yet and still looking: a big radar in the body.
            let radar = matches!(s.wifi_scan, Scan::Busy) && s.wifi_results().is_empty();
            let extra = if radar { RADAR_H } else { 0.0 };
            let m = widgets::modal(
                c,
                640.0,
                list_modal_h(s.wifi_results().len() + 1, 7) + extra,
            );
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let y = modal_header(c, mx, my, Some("wifi"), "Wi-Fi", 640.0);
            let status = match (&s.wifi_scan, &s.wifi_connecting) {
                (_, Some(ssid)) => format!("Joining {ssid}…"),
                (Scan::Busy, _) => "Searching…".to_string(),
                (Scan::Failed(e), _) => e.clone(),
                (Scan::Done(n), _) if n.is_empty() => "No networks found".to_string(),
                _ => String::new(),
            };
            let busy = s.wifi_connecting.is_some() || matches!(s.wifi_scan, Scan::Busy);
            widgets::busy_line(c, mx + 36.0, y - 18.0, &status, busy && !radar, v.now);
            if radar {
                widgets::radar(c, mx + 320.0, y + RADAR_H / 2.0 - 6.0, "wifi", v.now);
            }
            let y = y + extra;
            let nets = s.wifi_results();
            let subs: Vec<&str> = nets
                .iter()
                .map(|n| match (n.in_use, n.secure) {
                    (true, _) => "Connected",
                    (false, true) => "Secured",
                    (false, false) => "Open",
                })
                .collect();
            let mut rows: Vec<ListRow> = nets
                .iter()
                .zip(&subs)
                .map(|(n, sub)| ListRow {
                    title: &n.ssid,
                    subtitle: sub,
                    value: "",
                    kind: None,
                    icon: Some("wifi"),
                    disabled: false,
                })
                .collect();
            rows.push(ListRow {
                icon: Some("refresh"),
                kind: Some(RowKind::Action),
                ..ListRow::simple("Search again")
            });
            modal_list(c, "wifi", mx + 32.0, y, 576.0, &rows, *index, 7);
        }
        Overlay::QuickNetwork { index } => {
            let y = qs_panel(c, "globe", "Network");
            let sys = ctx.sys;
            let eth = sys.ethernet.as_ref().map(settings::ethernet_summary);
            let nets = s.wifi_results();
            let rows_ids = s.quick_net_rows(ctx);
            let status = match (&s.wifi_scan, &s.wifi_connecting) {
                (_, Some(ssid)) => format!("Joining {ssid}…"),
                _ if sys.wifi_on != Some(true) => String::new(),
                (Scan::Busy, _) => "Searching…".to_string(),
                (Scan::Failed(e), _) => e.clone(),
                (Scan::Done(n), _) if n.is_empty() => "No Wi-Fi networks found".to_string(),
                _ => String::new(),
            };
            let rows: Vec<ListRow> = rows_ids
                .iter()
                .map(|r| match r {
                    shell::NetRow::WifiSwitch => ListRow {
                        icon: Some("wifi"),
                        kind: Some(RowKind::Toggle(sys.wifi_on)),
                        ..ListRow::simple("Wi-Fi")
                    },
                    shell::NetRow::Ethernet => ListRow {
                        icon: Some("ethernet"),
                        subtitle: eth.as_deref().unwrap_or(""),
                        kind: Some(RowKind::Action),
                        ..ListRow::simple("Ethernet")
                    },
                    shell::NetRow::Wifi(i) => {
                        let n = &nets[*i];
                        ListRow {
                            icon: Some(if n.secure { "lock" } else { "wifi" }),
                            subtitle: if n.in_use { "Connected" } else { "" },
                            ..ListRow::simple(&n.ssid)
                        }
                    }
                    shell::NetRow::ScanAgain => ListRow {
                        icon: Some("refresh"),
                        kind: Some(RowKind::Action),
                        ..ListRow::simple("Search again")
                    },
                })
                .collect();
            let busy = s.wifi_connecting.is_some() || matches!(s.wifi_scan, Scan::Busy);
            let top = if status.is_empty() {
                y
            } else {
                widgets::busy_line(c, 872.0, y + 4.0, &status, busy, v.now);
                y + 30.0
            };
            modal_list(c, "qs-net", 868.0, top, 360.0, &rows, *index, 9);
        }
        Overlay::QuickBluetooth { index } => {
            let y = qs_panel(c, "bluetooth", "Bluetooth");
            let sys = ctx.sys;
            let rows_ids = shell::quick_bt_rows(ctx);
            let names: Vec<String> = sys.bt_devices.iter().map(|d| d.name.clone()).collect();
            let states: Vec<String> = sys
                .bt_devices
                .iter()
                .map(|d| {
                    if s.bt_busy.as_deref() == Some(d.address.as_str()) {
                        "…".into()
                    } else if shell::bt_connected(sys, &d.address) {
                        shell::bt_connected_label(sys, &d.address)
                    } else {
                        "Not connected".into()
                    }
                })
                .collect();
            let rows: Vec<ListRow> = rows_ids
                .iter()
                .map(|r| match r {
                    shell::BtRow::Switch => ListRow {
                        icon: Some("bluetooth"),
                        kind: Some(RowKind::Toggle(Some(sys.bluetooth.powered))),
                        ..ListRow::simple("Bluetooth")
                    },
                    shell::BtRow::Device(i) => ListRow {
                        icon: Some("bluetooth"),
                        subtitle: &states[*i],
                        ..ListRow::simple(&names[*i])
                    },
                    shell::BtRow::Pair => ListRow {
                        icon: Some("plus"),
                        kind: Some(RowKind::Action),
                        ..ListRow::simple("Pair a new device")
                    },
                })
                .collect();
            modal_list(c, "qs-bt", 868.0, y, 360.0, &rows, *index, 9);
        }
        Overlay::Ethernet { index } => {
            let y = qs_panel(c, "ethernet", "Ethernet");
            let Some(e) = ctx.sys.ethernet.as_ref() else {
                return;
            };
            let Some(d) = s.eth_draft.as_ref() else {
                return;
            };
            // What it has now.
            let facts = [
                ("Status", settings::ethernet_summary(e)),
                ("Gateway", e.gateway.clone().unwrap_or_else(|| "-".into())),
                (
                    "DNS",
                    if e.dns.is_empty() {
                        "-".into()
                    } else {
                        e.dns.join(", ")
                    },
                ),
                (
                    "MAC",
                    if e.mac.is_empty() {
                        "-".into()
                    } else {
                        e.mac.clone()
                    },
                ),
            ];
            let mut fy = y + 8.0;
            for (k, val) in facts {
                c.text(
                    872.0,
                    fy,
                    Align2::LEFT_CENTER,
                    k,
                    14.0,
                    theme::text3(),
                    Weight::Medium,
                );
                c.text(
                    1224.0,
                    fy,
                    Align2::RIGHT_CENTER,
                    &c.fit(&val, 250.0, 14.0, Weight::Medium),
                    14.0,
                    theme::text2(),
                    Weight::Medium,
                );
                fy += 26.0;
            }
            let rows: Vec<ListRow> = s
                .eth_rows()
                .iter()
                .map(|r| match r {
                    shell::EthRow::Method => ListRow {
                        value: if d.manual { "Manual" } else { "Automatic" },
                        kind: Some(RowKind::Choice),
                        ..ListRow::simple("IP settings")
                    },
                    shell::EthRow::Field(f) => ListRow {
                        value: f.value(d),
                        kind: Some(RowKind::Action),
                        ..ListRow::simple(f.label())
                    },
                    shell::EthRow::Save => ListRow {
                        icon: Some("check"),
                        kind: Some(RowKind::Action),
                        ..ListRow::simple("Save")
                    },
                })
                .collect();
            modal_list(c, "qs-eth", 868.0, fy + 12.0, 360.0, &rows, *index, 6);
        }
        Overlay::EthField { field, osk } => {
            let m = widgets::modal(c, 760.0, 470.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let y = modal_header(c, mx, my, Some("ethernet"), field.label(), 760.0);
            widgets::keyboard(
                c,
                osk,
                mx + 40.0,
                y - 8.0,
                680.0,
                field.label(),
                true,
                "eth-osk",
                &|r, col| Target::OverlayKey(r, col),
            );
        }
        Overlay::WifiPassword { ssid, osk } => {
            let m = widgets::modal(c, 760.0, 470.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let y = modal_header(c, mx, my, Some("lock"), ssid, 760.0);
            widgets::keyboard(
                c,
                osk,
                mx + 40.0,
                y - 8.0,
                680.0,
                "Password",
                s.wifi_connecting.is_none(),
                "wifi-osk",
                &|r, col| Target::OverlayKey(r, col),
            );
            if s.wifi_connecting.is_some() {
                widgets::busy_line(c, mx + 40.0, my + 446.0, "Joining…", true, v.now);
            }
        }
        Overlay::DebugPassword { osk, first } => {
            let m = widgets::modal(c, 760.0, 470.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let title = if first.is_some() {
                "Type the new password again"
            } else {
                "New password for catostv"
            };
            let y = modal_header(c, mx, my, Some("lock"), title, 760.0);
            widgets::keyboard(
                c,
                osk,
                mx + 40.0,
                y - 8.0,
                680.0,
                "Password",
                true,
                "debug-osk",
                &|r, col| Target::OverlayKey(r, col),
            );
        }
        Overlay::BtScan { index } => {
            let radar = matches!(s.bt_scan, Scan::Busy) && s.bt_scan.items().is_empty();
            let extra = if radar { RADAR_H } else { 0.0 };
            let m = widgets::modal(
                c,
                640.0,
                list_modal_h(s.bt_scan.items().len() + 1, 6) + extra,
            );
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let busy = s.bt_busy.is_some() || matches!(s.bt_scan, Scan::Busy);
            let y = modal_header(c, mx, my, Some("bluetooth"), "Pair a device", 640.0);
            let status = match (&s.bt_scan, &s.bt_busy) {
                (_, Some(_)) => "Pairing…".to_string(),
                (Scan::Busy, _) => "Searching. Put your device in pairing mode".into(),
                (Scan::Failed(e), _) => e.clone(),
                (Scan::Done(d), _) if d.is_empty() => "No devices found".into(),
                _ => String::new(),
            };
            widgets::busy_line(c, mx + 36.0, y - 18.0, &status, busy && !radar, v.now);
            if radar {
                // Discovery radar: rings sweep out from the Bluetooth glyph.
                widgets::radar(c, mx + 320.0, y + RADAR_H / 2.0 - 6.0, "bluetooth", v.now);
            }
            let y = y + extra;
            let found = s.bt_scan.items();
            let mut rows: Vec<ListRow> = found
                .iter()
                .map(|d| ListRow {
                    icon: Some("bluetooth"),
                    value: if s.bt_busy.as_deref() == Some(d.address.as_str()) {
                        "Pairing…"
                    } else {
                        ""
                    },
                    ..ListRow::simple(&d.name)
                })
                .collect();
            rows.push(ListRow {
                icon: Some("refresh"),
                kind: Some(RowKind::Action),
                ..ListRow::simple("Search again")
            });
            modal_list(c, "bt", mx + 32.0, y, 576.0, &rows, *index, 6);
        }
        Overlay::BtDevice { device, index } => {
            let name = ctx
                .sys
                .bt_devices
                .get(*device)
                .map(|d| d.name.clone())
                .unwrap_or_default();
            let m = widgets::modal(c, 420.0, 130.0 + BT_OPTIONS.len() as f32 * 62.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let y = modal_header(c, mx, my, Some("bluetooth"), &name, 420.0);
            let labels: Vec<(&str, ButtonStyle)> = BT_OPTIONS
                .iter()
                .map(|o| {
                    (
                        o.label(),
                        match o {
                            shell::BtOption::Forget => ButtonStyle::Danger,
                            shell::BtOption::Connect => ButtonStyle::Primary,
                            _ => ButtonStyle::Secondary,
                        },
                    )
                })
                .collect();
            button_column(c, "btopt", mx + 32.0, y, 356.0, &labels, *index);
        }
        Overlay::KdeDevice { device, index } => {
            let Some(d) = ctx.sys.kde.get(*device) else {
                return;
            };
            let opts = shell::kde_options(d);
            let m = widgets::modal(c, 420.0, 130.0 + opts.len() as f32 * 62.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let y = modal_header(c, mx, my, Some("phone"), &d.name, 420.0);
            let labels: Vec<(&str, ButtonStyle)> = opts
                .iter()
                .map(|o| {
                    (
                        o.label(),
                        match o {
                            shell::KdeOption::Unpair | shell::KdeOption::Decline => {
                                ButtonStyle::Danger
                            }
                            shell::KdeOption::Accept | shell::KdeOption::Pair => {
                                ButtonStyle::Primary
                            }
                            shell::KdeOption::Ping => ButtonStyle::Secondary,
                        },
                    )
                })
                .collect();
            button_column(c, "kdeopt", mx + 32.0, y, 356.0, &labels, *index);
        }
        Overlay::DisplayModes { index } => {
            let m = widgets::modal(c, 560.0, list_modal_h(s.menu_modes(ctx).len(), 8));
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let name = ctx.output().map(|o| o.display_name()).unwrap_or_default();
            let y = modal_header(c, mx, my, Some("display"), "Resolution", 560.0);
            status_line(c, mx + 36.0, y - 18.0, &name);
            let modes = s.menu_modes(ctx);
            let labels: Vec<String> = modes.iter().map(|m| m.label()).collect();
            let rows: Vec<ListRow> = modes
                .iter()
                .zip(&labels)
                .map(|(m, l)| ListRow {
                    value: if m.current {
                        "Current"
                    } else if m.preferred {
                        "Recommended"
                    } else {
                        ""
                    },
                    ..ListRow::simple(l)
                })
                .collect();
            modal_list(c, "modes", mx + 32.0, y, 496.0, &rows, *index, 8);
        }
        Overlay::Certify => {
            let m = widgets::modal(c, 640.0, 300.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let y = modal_header(
                c,
                mx,
                my,
                Some("android"),
                "Google Play certification",
                640.0,
            );
            let id = v
                .ctx
                .sys
                .gsf_id
                .clone()
                .unwrap_or_else(|| "not available yet".into());
            c.paragraph(
                mx + 36.0,
                y - 8.0,
                568.0,
                "Google only lets certified devices sign in. Once, on a phone or computer, open google.com/android/uncertified, sign in with your Google account and enter this id. Then sign in to Google Play on the TV (a restart may be needed).",
                15.0,
                theme::text2(),
                Weight::Regular,
            );
            let pill = c.r(mx + 36.0, y + 92.0, 568.0, 52.0);
            c.glass(pill, 26.0);
            c.text(
                mx + 320.0,
                y + 118.0,
                Align2::CENTER_CENTER,
                &id,
                24.0,
                theme::text(),
                Weight::Semibold,
            );
            let rect = c.r(mx + 36.0, y + 160.0, 216.0, 52.0);
            widgets::button(c, rect, "OK", ButtonStyle::Primary, 1.0);
            c.hit(rect, Target::Item(0));
        }
        Overlay::StoreChoice { index } => {
            // Two cards, nothing else ("nice, clutter-free").
            choice_sheet(
                c,
                "store-choice",
                *index,
                "Which app store?",
                "You can switch later from Settings → Android (Android is set up again).",
                &[
                    (
                        "store",
                        "Google Play",
                        "Google's TV store. Needs a Google account and a one-time device registration (CatosTV shows the id).",
                        "Recommended",
                    ),
                    (
                        "android",
                        "Aurora Store",
                        "The Play catalogue without a Google account. Some apps that need Google services will not sign in.",
                        "No account",
                    ),
                ],
            );
        }
        Overlay::RepairConfirm { index } => {
            // Resetting erases Android's apps and sign-ins: plain
            // words, Cancel first and focused by default.
            let m = widgets::modal(c, 640.0, 296.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let y = modal_header(c, mx, my, Some("refresh"), RESET_TITLE, 640.0);
            c.paragraph(
                mx + 36.0,
                y - 14.0,
                568.0,
                RESET_TEXT,
                15.0,
                theme::text2(),
                Weight::Regular,
            );
            for (k, label) in ["Cancel", "Reset Android"].iter().enumerate() {
                let focused = *index == k;
                let t = focus_t(c, ("reset", k), focused);
                let rect = c.r(mx + 36.0 + k as f32 * 292.0, my + 214.0, 276.0, 52.0);
                let style = match (focused, k) {
                    (false, _) => ButtonStyle::Secondary,
                    (true, 0) => ButtonStyle::Primary,
                    (true, _) => ButtonStyle::Danger,
                };
                widgets::button(c, rect, label, style, t);
                c.hit(rect, Target::Item(k));
            }
        }
        Overlay::PadCapture { action } => {
            let m = widgets::modal(c, 560.0, 240.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            widgets::pulse(c, mx + 280.0, my + 86.0, 30.0, v.now);
            icons::draw(c, "game", mx + 280.0, my + 86.0, 22.0, theme::accent());
            c.text(
                mx + 280.0,
                my + 150.0,
                Align2::CENTER_CENTER,
                &format!("Press the button for {}", action.label()),
                22.0,
                theme::text(),
                Weight::Semibold,
            );
            c.text(
                mx + 280.0,
                my + 188.0,
                Align2::CENTER_CENTER,
                "On the controller · Back cancels",
                14.0,
                theme::text3(),
                Weight::Regular,
            );
        }
        Overlay::KdeRequest { name, index, .. } => {
            let m = widgets::modal(c, 520.0, 224.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let title = format!("Pair with {}?", c.fit(name, 300.0, 22.0, Weight::Semibold));
            let y = modal_header(c, mx, my, Some("phone"), &title, 520.0);
            status_line(c, mx + 36.0, y - 14.0, "KDE Connect");
            for (k, label) in ["Accept", "Decline"].iter().enumerate() {
                let focused = *index == k;
                let t = focus_t(c, ("kde", k), focused);
                let rect = c.r(mx + 36.0 + k as f32 * 232.0, y + 30.0, 216.0, 52.0);
                let style = if focused {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Secondary
                };
                widgets::button(c, rect, label, style, t);
                c.hit(rect, Target::Item(k));
            }
        }
        Overlay::DisplayConfirm { index } => {
            let m = widgets::modal(c, 520.0, 224.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let title = if s.hdr_pending {
                "Keep HDR?"
            } else {
                "Keep this resolution?"
            };
            let y = modal_header(c, mx, my, Some("display"), title, 520.0);
            let left = v.confirm_left.unwrap_or(shell::DISPLAY_CONFIRM_SECS);
            let new = s
                .display_pending
                .as_ref()
                .map(|p| p.new.label())
                .unwrap_or_default();
            let line = if s.hdr_pending {
                format!("Turning HDR off in {left} s")
            } else if new.is_empty() {
                format!("Reverting in {left} s")
            } else {
                format!("{new} · reverting in {left} s")
            };
            status_line(c, mx + 36.0, y - 14.0, &line);
            for (k, item) in CONFIRM_ITEMS.iter().enumerate() {
                let focused = *index == k;
                let t = focus_t(c, ("confirm", k), focused);
                let rect = c.r(mx + 36.0 + k as f32 * 232.0, y + 30.0, 216.0, 52.0);
                let label = match item {
                    shell::ConfirmItem::Keep => "Keep",
                    shell::ConfirmItem::Revert if s.hdr_pending => "Turn off",
                    shell::ConfirmItem::Revert => "Revert",
                };
                // The focused choice carries the accent; Revert starts focused.
                let style = if focused {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Secondary
                };
                widgets::button(c, rect, label, style, t);
                c.hit(rect, Target::Item(k));
            }
        }
        Overlay::AudioOutputs { index } => {
            let m = widgets::modal(c, 560.0, list_modal_h(ctx.sys.sinks.len(), 6));
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let y = modal_header(c, mx, my, Some("speaker"), "Sound output", 560.0);
            let rows: Vec<ListRow> = ctx
                .sys
                .sinks
                .iter()
                .map(|snk| ListRow {
                    subtitle: snk.kind.label(),
                    value: if snk.is_default { "Current" } else { "" },
                    ..ListRow::simple(&snk.name)
                })
                .collect();
            modal_list(c, "sinks", mx + 32.0, y, 496.0, &rows, *index, 6);
        }
        Overlay::StoreApp { item, index } => draw_store_app(c, v, item, *index),
        Overlay::StoreSearch { osk } => {
            let m = widgets::modal(c, 760.0, 470.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let y = modal_header(c, mx, my, Some("search"), "Search Flathub", 760.0);
            widgets::keyboard(
                c,
                osk,
                mx + 40.0,
                y - 8.0,
                680.0,
                "App name",
                true,
                "store-osk",
                &|r, col| Target::OverlayKey(r, col),
            );
        }
        Overlay::AppsSearch { osk } => {
            let m = widgets::modal(c, 760.0, 470.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let y = modal_header(c, mx, my, Some("search"), "Search apps", 760.0);
            widgets::keyboard(
                c,
                osk,
                mx + 40.0,
                y - 8.0,
                680.0,
                "App name",
                true,
                "apps-osk",
                &|r, col| Target::OverlayKey(r, col),
            );
        }
        Overlay::KeyGuide => {
            let m = widgets::modal(c, 700.0, 520.0);
            c.hit(m, Target::Item(0));
            let (mx, my) = c.to_design(m.min);
            let y = modal_header(c, mx, my, Some("keyboard"), "Keys", 700.0);
            let h = widgets::key_guide(c, mx + 40.0, y - 10.0, 620.0, 48.0);
            let t = focus_t(c, "keys-ok", true);
            let rect = c.r(mx + 40.0, y + h, 180.0, 50.0);
            widgets::button(c, rect, "Got it", ButtonStyle::Primary, t);
        }
        Overlay::ApkList { index } => {
            // Folder / APK picker: any storage, only
            // folders and .apk files; Back goes up a folder.
            let items = s.apks.items();
            let m = widgets::modal(c, 760.0, list_modal_h(items.len(), 7) + 24.0);
            c.hit(m, Target::Item(usize::MAX));
            let (mx, my) = c.to_design(m.min);
            let title = s
                .apk_stack
                .last()
                .map(|(d, _)| d.rsplit('/').next().unwrap_or(d).to_string())
                .unwrap_or_else(|| "Install an Android app".to_string());
            let y = modal_header(
                c,
                mx,
                my,
                Some(if s.apk_stack.is_empty() {
                    "android"
                } else {
                    "folder"
                }),
                &title,
                760.0,
            );
            let status = match &s.apks {
                Scan::Busy => "Looking…".to_string(),
                Scan::Failed(e) => e.clone(),
                Scan::Done(found) if found.is_empty() => {
                    "No folders or APK files here. Back goes up".into()
                }
                _ if s.apk_stack.is_empty() => "Choose where your APK is".into(),
                _ => "OK opens a folder or installs an APK · Back goes up".into(),
            };
            let busy = matches!(s.apks, Scan::Busy);
            widgets::busy_line(c, mx + 36.0, y - 18.0, &status, busy, v.now);
            let rows: Vec<ListRow> = items
                .iter()
                .map(|e| ListRow {
                    subtitle: if s.apk_stack.is_empty() { &e.path } else { "" },
                    icon: Some(if e.dir { "folder" } else { "android" }),
                    kind: Some(if e.dir {
                        RowKind::Action
                    } else {
                        RowKind::Info
                    }),
                    ..ListRow::simple(&e.name)
                })
                .collect();
            modal_list(c, "apk", mx + 32.0, y + 6.0, 696.0, &rows, *index, 7);
        }
    }
}

/// Full-screen Store page: icon, name, developer,
/// summary, actions, description and a row of screenshots. Back returns.
fn draw_store_app(c: &C, v: &ShellView, item: &StoreItem, index: usize) {
    let s = v.shell;
    let tint = theme::app_rgb(catostv_core::desktop::accent_for(&item.id));
    c.fill(c.screen, 0.0, theme::rgb(theme::BG_BOTTOM));
    c.background(Some(tint), v.ctx.prefs.glow);
    c.hit(c.screen, Target::Item(usize::MAX));
    let details = match &s.store_details {
        Some((id, d)) if *id == item.id => Some(d),
        _ => None,
    };
    let ready = match details {
        Some(shell::Details::Ready(d)) => Some(d),
        _ => None,
    };

    // Back hint.
    let kw = widgets::keycap(c, 64.0, 46.0, "Back");
    c.text(
        64.0 + kw + 10.0,
        46.0,
        Align2::LEFT_CENTER,
        "Store",
        15.0,
        theme::text3(),
        Weight::Regular,
    );

    // Header: icon tile, name, developer, summary.
    let tile = c.r(64.0, 84.0, 124.0, 124.0);
    c.shadow(tile, 30.0, 14.0, 40.0, theme::rgba(theme::BLACK, 110));
    c.fill(tile, 30.0, theme::glass_hi());
    c.outline(tile, 30.0, 1.0, theme::border());
    store_icon_or_mono(c, v, item, 126.0, 146.0, 92.0);
    let name = ready
        .map(|d| d.name.as_str())
        .filter(|n| !n.is_empty())
        .unwrap_or(&item.name);
    c.text(
        212.0,
        84.0,
        Align2::LEFT_TOP,
        &c.fit(name, 1000.0, 42.0, Weight::Semibold),
        42.0,
        theme::text(),
        Weight::Semibold,
    );
    let developer = ready
        .map(|d| d.developer.clone())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| item.id.clone());
    c.text(
        214.0,
        140.0,
        Align2::LEFT_TOP,
        &c.fit(&developer, 1000.0, 16.0, Weight::Medium),
        16.0,
        theme::text3(),
        Weight::Medium,
    );
    let summary = ready
        .map(|d| d.summary.as_str())
        .filter(|t| !t.is_empty())
        .unwrap_or(&item.blurb);
    c.text(
        214.0,
        168.0,
        Align2::LEFT_TOP,
        &c.fit(summary, 1000.0, 19.0, Weight::Regular),
        19.0,
        theme::text2(),
        Weight::Regular,
    );

    // Actions.
    let acts = s.actions_for(item, v.ctx);
    for (k, a) in acts.iter().enumerate() {
        let t = focus_t(c, ("storeact", k), index == k);
        let rect = c.r(212.0 + k as f32 * 180.0, 214.0, 164.0, 52.0);
        let style = match a {
            shell::StoreAction::Install | shell::StoreAction::Open => ButtonStyle::Primary,
            shell::StoreAction::Remove => ButtonStyle::Danger,
            shell::StoreAction::Close => ButtonStyle::Secondary,
        };
        widgets::button(c, rect, a.label(), style, t);
        c.hit(rect, Target::Item(k));
    }
    let after_buttons = 212.0 + acts.len() as f32 * 180.0 + 8.0;
    let (label, frac, _) = store_status(v, item);
    if let Some(f) = frac {
        widgets::progress_bar(c, after_buttons, 232.0, 260.0, f, v.now);
    }
    if let Some(l) = label {
        status_line(c, after_buttons, 254.0, &l);
    }

    // Description (a few lines) or the loading state.
    let mut y = 300.0;
    match details {
        Some(shell::Details::Loading) | None => {
            widgets::busy_line(c, 64.0, y + 10.0, "Loading details…", true, v.now);
        }
        Some(shell::Details::Failed(e)) => status_line(c, 64.0, y + 10.0, e),
        Some(shell::Details::Ready(d)) => {
            let mut text = String::new();
            for p in &d.description {
                if text.len() + p.len() > 520 {
                    if text.is_empty() {
                        text = p.chars().take(500).collect::<String>() + "…";
                    }
                    break;
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(p);
            }
            if text.is_empty() {
                text = item.blurb.clone();
            }
            y += c.paragraph(
                64.0,
                y,
                1150.0,
                &text,
                17.0,
                theme::text2(),
                Weight::Regular,
            );
        }
    }

    // Screenshots: one row, the focused one lifted; glides to stay visible.
    let shots = s.store_shots(&item.id);
    if shots.is_empty() {
        return;
    }
    let (w, h, gap) = (448.0, 252.0, 28.0);
    let row_y = (y + 24.0).clamp(420.0, 440.0);
    let focus = index.checked_sub(acts.len());
    let target = match focus {
        Some(k) => (64.0 - (k as f32 * (w + gap) - (1216.0 - 64.0 - w) / 2.0).max(0.0)).min(64.0),
        None => 64.0,
    };
    let x0 = c.anim(("shots-scroll", &item.id), target);
    for (k, url) in shots.iter().enumerate() {
        let x = x0 + k as f32 * (w + gap);
        if x > 1290.0 || x + w < -10.0 {
            continue;
        }
        let here = focus == Some(k);
        let t = focus_t(c, ("shot", k), here);
        let rect = widgets::lifted(c, c.r(x, row_y, w, h), t);
        widgets::focus_lift(c, rect, 18.0, t);
        match v.shots.get(url).and_then(|p| v.images.get(&c.ctx, p)) {
            Some(tex) => c.image_rounded(rect, 18.0, tex, Color32::WHITE),
            None => {
                c.fill(rect, 18.0, theme::glass());
                let (rx, ry) = c.to_design(rect.min);
                widgets::spinner(c, rx + w / 2.0, ry + h / 2.0, 14.0, v.now);
            }
        }
        c.outline(rect, 18.0, 1.0, theme::rgba(theme::WHITE, 24));
        widgets::focus_sheen(c, rect, 18.0, t);
        c.hit(rect, Target::Item(acts.len() + k));
    }
}

/// Hit-test helper: the topmost target under `pos`.
pub fn hit_test(hits: &[(Rect, Target)], pos: eframe::egui::Pos2) -> Option<Target> {
    hits.iter()
        .rev()
        .find(|(r, _)| r.contains(pos))
        .map(|(_, t)| *t)
        .filter(|t| *t != Target::Item(usize::MAX))
}

/// A sheet with a title, a line of context and two cards; OK picks the
/// focused card (the store chooser, the repair confirmation).
/// `cards`: (icon, title, blurb, tag).
fn choice_sheet(
    c: &C,
    key: &'static str,
    index: usize,
    title: &str,
    subtitle: &str,
    cards: &[(&str, &str, &str, &str); 2],
) {
    let m = widgets::modal(c, 880.0, 400.0);
    c.hit(m, Target::Item(usize::MAX));
    let (mx, my) = c.to_design(m.min);
    c.text(
        mx + 440.0,
        my + 46.0,
        Align2::CENTER_CENTER,
        title,
        26.0,
        theme::text(),
        Weight::Semibold,
    );
    c.text(
        mx + 440.0,
        my + 78.0,
        Align2::CENTER_CENTER,
        subtitle,
        14.0,
        theme::text3(),
        Weight::Regular,
    );
    for (i, (icon, title, blurb, tag)) in cards.iter().enumerate() {
        let x = mx + 44.0 + i as f32 * 408.0;
        let rect = c.r(x, my + 108.0, 384.0, 232.0);
        let t = focus_t(c, (key, i), index == i);
        c.fill(rect, 24.0, theme::rgba(theme::WHITE, 14));
        c.frost_edge(rect, 24.0, 0.7);
        widgets::focus_pill(c, rect, 24.0, t);
        c.circle(
            x + 56.0,
            my + 160.0,
            26.0,
            theme::rgba(theme::accent_rgb(), 70),
        );
        icons::draw(c, icon, x + 56.0, my + 160.0, 20.0, theme::accent());
        c.text(
            x + 98.0,
            my + 160.0,
            Align2::LEFT_CENTER,
            title,
            22.0,
            theme::text(),
            Weight::Semibold,
        );
        let pill_w = c.text_width(tag, 12.5, Weight::Semibold) + 20.0;
        let px = x + 384.0 - 24.0 - pill_w;
        let pill = c.r(px, my + 148.0, pill_w, 24.0);
        c.fill(
            pill,
            12.0,
            theme::rgba(theme::accent_rgb(), (40.0 + 60.0 * t) as u8),
        );
        c.text(
            px + pill_w / 2.0,
            my + 160.0,
            Align2::CENTER_CENTER,
            tag,
            12.5,
            theme::text(),
            Weight::Semibold,
        );
        c.paragraph(
            x + 28.0,
            my + 204.0,
            328.0,
            blurb,
            15.0,
            theme::text2(),
            Weight::Regular,
        );
        c.text(
            x + 192.0,
            my + 306.0,
            Align2::CENTER_CENTER,
            if index == i { "OK to choose" } else { "" },
            13.5,
            theme::text3(),
            Weight::Medium,
        );
        c.hit(rect, Target::Item(i));
    }
}
