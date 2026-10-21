//! Shared TV components drawn on a [`Canvas`]. All sizes in design units.
//!
//! Focus language: the focused element lifts
//! ~5 %, its glass brightens, it gets a bright specular ring and a soft
//! coloured bloom underneath (the accent, or the app's colour on app
//! tiles). Everything else keeps a quiet hairline. Controls are pills;
//! cards are rounded squircles. Motion is a 0.18 s ease.

use crate::canvas::Canvas;
use crate::icons;
use crate::images::Images;
use crate::theme::{self, Weight};
use catostv_core::osk::{KeyKind, Osk, ROW_UNITS};
use catostv_core::settings::{AppView, RowKind};
use eframe::egui::{Align2, Color32, Rect};

// ---------------------------------------------------------------- focus
//
// One focus language for every screen ("subtle, like
// tvOS"): nothing is drawn outside the focused element, there are no
// coloured glows, rings or thick outlines, and every soft edge is a real
// Gaussian (`Canvas::soft_shadow`), never stacked translucent shapes.
//
// * Rows, list items, buttons, keys, chips and sheet cards: [`focus_pill`]
//   a lighter pane inside the element's own bounds (white ~14 % at the
//   top easing to ~10 %) and a 1-px highlight along its top edge. The
//   focused label goes full white; the others stay dimmer.
// * Tiles and posters: [`lifted`] the rect (×1.06 and up a little), draw
//   [`focus_lift`] (a soft shadow) before the tile and [`focus_sheen`]
//   (a white 12 % → 0 % sheen and a hairline) after its fill. Unfocused
//   tiles stay flat.
// * Motion: `Canvas::anim` eases out over 0.18 s; the scale and the
//   gliding row highlight move together.

/// Focus inside a row / button / key / chip / card (see the module notes).
pub fn focus_pill<H: Copy>(c: &Canvas<H>, rect: Rect, radius: f32, t: f32) {
    if t <= 0.01 {
        return;
    }
    c.rounded_gradient(
        rect,
        radius,
        theme::WHITE,
        (36.0 * t) as u8,
        (26.0 * t) as u8,
    );
    c.top_highlight(rect, radius, theme::rgba(theme::WHITE, (120.0 * t) as u8));
}

/// Colour between `a` and `b` (t = 0 → a, 1 → b).
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    Color32::from_rgba_unmultiplied(
        lerp(a.r(), b.r(), t),
        lerp(a.g(), b.g(), t),
        lerp(a.b(), b.b(), t),
        lerp(a.a(), b.a(), t),
    )
}

/// The top bar's focus: a pill that is solid accent with a soft
/// accent glow while focused (`t` = 1) and quiet frosted glass otherwise,
/// so moving up from the rows into the bar is obvious.
pub fn bar_pill<H: Copy>(c: &Canvas<H>, rect: Rect, radius: f32, t: f32) {
    if t > 0.01 {
        c.soft_shadow(rect, radius, 18.0, 2.0, theme::accent_a((130.0 * t) as u8));
    }
    c.fill(
        rect,
        radius,
        theme::rgba(theme::WHITE, (40.0 * (1.0 - t)) as u8),
    );
    c.fill(rect, radius, theme::accent_a((255.0 * t) as u8));
    c.rounded_gradient(rect, radius, theme::WHITE, (40.0 * t) as u8, 0);
    c.top_highlight(
        rect,
        radius,
        theme::rgba(theme::WHITE, (60.0 + 100.0 * t) as u8),
    );
}

/// A focused tile's rect: scaled up 6 % and lifted.
pub fn lifted<H: Copy>(c: &Canvas<H>, rect: Rect, t: f32) -> Rect {
    lifted_by(c, rect, t, LIFT)
}

/// How much a focused tile grows: in lists, and on Home, where the
/// focused tile is the one thing to look at.
pub const LIFT: f32 = 0.06;
pub const HOME_LIFT: f32 = 0.14;

/// [`lifted`], growing by `grow` (×1.14 for 0.14) when focused.
pub fn lifted_by<H: Copy>(c: &Canvas<H>, rect: Rect, t: f32, grow: f32) -> Rect {
    rect.scale_from_center(1.0 + grow * t)
        .translate(eframe::egui::vec2(0.0, -c.len(4.0 + 40.0 * grow) * t))
}

/// The soft shadow under a focused tile (draw before the tile).
pub fn focus_lift<H: Copy>(c: &Canvas<H>, rect: Rect, radius: f32, t: f32) {
    if t <= 0.01 {
        return;
    }
    c.soft_shadow(
        rect,
        radius,
        16.0,
        12.0 * t,
        theme::rgba(theme::BLACK, (150.0 * t) as u8),
    );
}

/// Home's rail only: a soft glow in the app's own colour around
/// the focused tile, so the eye finds the focus when swiping along the
/// rail. A real Gaussian (`Canvas::soft_shadow` in colour), no rings; it
/// eases in and out with the focus. Draw before the tile.
pub fn home_glow<H: Copy>(c: &Canvas<H>, rect: Rect, radius: f32, t: f32, tint: theme::Rgb) {
    if t <= 0.01 {
        return;
    }
    c.soft_shadow(
        rect,
        radius,
        20.0,
        4.0,
        theme::rgba(lighten(tint, 0.3), (110.0 * t) as u8),
    );
}

/// The light on a focused tile (draw after its fill).
pub fn focus_sheen<H: Copy>(c: &Canvas<H>, rect: Rect, radius: f32, t: f32) {
    if t <= 0.01 {
        return;
    }
    c.rounded_gradient(rect, radius, theme::WHITE, (31.0 * t) as u8, 0);
    c.outline(
        rect,
        radius,
        1.0,
        theme::rgba(theme::WHITE, (34.0 * t) as u8),
    );
    c.top_highlight(rect, radius, theme::rgba(theme::WHITE, (90.0 * t) as u8));
}

/// Scale a rect around its centre by the focus animation.
pub fn grow(rect: Rect, t: f32) -> Rect {
    rect.scale_from_center(1.0 + 0.05 * t)
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t.clamp(0.0, 1.0)) as u8
}

/// Brighter version of an RGB colour (towards white by `k`).
pub fn lighten(c: theme::Rgb, k: f32) -> theme::Rgb {
    (lerp(c.0, 255, k), lerp(c.1, 255, k), lerp(c.2, 255, k))
}

/// A glass card: a quiet pane and hairline; focus is [`focus_pill`].
pub fn card<H: Copy>(c: &Canvas<H>, rect: Rect, radius: f32, t: f32) {
    c.fill(rect, radius, theme::rgba(theme::WHITE, 14));
    c.outline(rect, radius, 1.0, theme::rgba(theme::WHITE, 20));
    focus_pill(c, rect, radius, t);
}

/// Small caps-style section label ("AVAILABLE NETWORKS").
pub fn section<H: Copy>(c: &Canvas<H>, x: f32, y: f32, text: &str) {
    c.text(
        x,
        y,
        Align2::LEFT_CENTER,
        &text.to_uppercase(),
        12.5,
        theme::text3(),
        Weight::Semibold,
    );
}

/// Status chip (pill with an optional coloured dot). Returns its width.
pub fn chip<H: Copy>(c: &Canvas<H>, x: f32, y: f32, label: &str, dot: Option<theme::Rgb>) -> f32 {
    let tw = c.text_width(label, 13.0, Weight::Medium);
    let w = tw + if dot.is_some() { 38.0 } else { 26.0 };
    let r = c.r(x, y, w, 28.0);
    c.fill(r, 14.0, theme::rgba(theme::WHITE, 18));
    c.outline(r, 14.0, 1.0, theme::border());
    let mut tx = x + 13.0;
    if let Some(d) = dot {
        c.circle(x + 15.0, y + 14.0, 4.0, theme::rgb(d));
        tx = x + 26.0;
    }
    c.text(
        tx,
        y + 14.0,
        Align2::LEFT_CENTER,
        label,
        13.0,
        theme::text2(),
        Weight::Medium,
    );
    w
}

/// Width of a [`chip`] for `label`.
pub fn chip_width<H: Copy>(c: &Canvas<H>, label: &str, dot: bool) -> f32 {
    c.text_width(label, 13.0, Weight::Medium) + if dot { 38.0 } else { 26.0 }
}

/// Selectable filter chip (Store categories): selected = solid light pill
/// with dark text; focus = [`focus_pill`].
pub fn filter_chip<H: Copy>(c: &Canvas<H>, rect: Rect, label: &str, selected: bool, t: f32) {
    let r = rect.height() / c.s / 2.0;
    if selected {
        c.fill(rect, r, theme::rgba(theme::INK, 245));
        c.top_highlight(rect, r, theme::rgba(theme::WHITE, (150.0 * t) as u8));
    } else {
        c.fill(rect, r, theme::rgba(theme::WHITE, 14));
        c.outline(rect, r, 1.0, theme::border());
        focus_pill(c, rect, r, t);
    }
    c.painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        c.font(
            15.0,
            if selected {
                Weight::Semibold
            } else {
                Weight::Medium
            },
        ),
        if selected {
            theme::rgb((12, 14, 22))
        } else if t > 0.5 {
            theme::text()
        } else {
            theme::text2()
        },
    );
}

/// Width a [`filter_chip`] needs for `label`.
pub fn filter_chip_width<H: Copy>(c: &Canvas<H>, label: &str) -> f32 {
    c.text_width(label, 15.0, Weight::Semibold) + 44.0
}

/// Round icon badge (settings rows, quick settings, store cards).
pub fn icon_badge<H: Copy>(
    c: &Canvas<H>,
    cx: f32,
    cy: f32,
    r: f32,
    icon: &str,
    active: bool,
    t: f32,
) {
    if active {
        c.circle(cx, cy, r, theme::accent());
        icons::draw(c, icon, cx, cy, r * 0.95, theme::on_accent());
    } else {
        c.circle(cx, cy, r, theme::rgba(theme::WHITE, lerp(16, 34, t)));
        icons::draw(
            c,
            icon,
            cx,
            cy,
            r * 0.95,
            if t > 0.5 {
                theme::text()
            } else {
                theme::text2()
            },
        );
    }
}

/// The app's icon (PNG when it has one, else the vector icon / monogram).
pub fn app_icon<H: Copy>(
    c: &Canvas<H>,
    images: &Images,
    app: &AppView,
    cx: f32,
    cy: f32,
    size: f32,
    alpha: u8,
) {
    if let Some(tex) = app
        .entry
        .icon_path
        .as_deref()
        .and_then(|p| images.get(&c.ctx, p))
    {
        let r = c.r(cx - size / 2.0, cy - size / 2.0, size, size);
        c.image(r, tex, Color32::from_white_alpha(alpha));
        return;
    }
    let col = lighten(theme::app_rgb(&app.entry.accent), 0.25);
    icons::draw(c, &app.entry.icon, cx, cy, size, theme::rgba(col, alpha));
}

/// "This app is open", a small green dot with a soft halo. Used instead of
/// a "Running" label everywhere.
pub fn running_dot<H: Copy>(c: &Canvas<H>, x: f32, y: f32) {
    c.circle(x, y, 9.0, theme::rgba(theme::SUCCESS, 40));
    c.circle(x, y, 4.5, theme::rgb(theme::SUCCESS));
}

/// App tile (16:9) with the name underneath. `rect` is the tile only.
pub fn app_tile<H: Copy>(c: &Canvas<H>, images: &Images, rect: Rect, app: &AppView, t: f32) {
    app_tile_lift(c, images, rect, app, t, 0.0, LIFT);
}

/// Height of the macOS-style "launching" bounce `secs` after OK: three
/// eased hops that get smaller, then rest.
pub fn launch_bounce(secs: f32) -> f32 {
    const HOP: f32 = 0.62;
    if !(0.0..HOP * 3.0).contains(&secs) {
        return 0.0;
    }
    let hop = (secs / HOP).floor();
    let f = secs / HOP - hop;
    let height = [1.0, 0.55, 0.25][hop as usize];
    // Up fast, down soft: sin of an eased phase.
    (std::f32::consts::PI * (1.0 - (1.0 - f).powi(2))).sin() * height
}

/// [`app_tile`] whose icon is lifted by `lift` (0–1, the launch bounce).
pub fn app_tile_lift<H: Copy>(
    c: &Canvas<H>,
    images: &Images,
    rect: Rect,
    app: &AppView,
    t: f32,
    lift: f32,
    grow: f32,
) {
    // Focus: grown by `grow` and lifted, a soft shadow under it, a sheen
    // on it; unfocused tiles stay flat.
    let rect = lifted_by(c, rect, t, grow);
    let (x, y) = c.to_design(rect.min);
    let (w, h) = c.dsize(rect);
    let radius = 22.0;
    let tint = theme::app_rgb(&app.entry.accent);
    focus_lift(c, rect, radius, t);
    // Frosted glass tinted with the app's own colour.
    c.fill(rect, radius, theme::rgba(theme::BG_TOP, 175));
    c.fill(rect, radius, theme::rgba(tint, (34.0 + 26.0 * t) as u8));
    c.frost_edge(rect, radius, 1.0);
    focus_sheen(c, rect, radius, t);
    let alpha = if app.installed { 255 } else { 110 };
    let size = h * 0.42 * (1.0 + 0.08 * lift);
    app_icon(
        c,
        images,
        app,
        x + w / 2.0,
        y + h / 2.0 - lift * h * 0.16,
        size,
        alpha,
    );
    if app.running {
        running_dot(c, x + w - 18.0, y + 18.0);
    }
    let ink = if t > 0.5 {
        theme::text()
    } else {
        theme::text2()
    };
    c.text(
        x + w / 2.0,
        y + h + 12.0 + 4.0 * t,
        Align2::CENTER_TOP,
        &c.fit(&app.entry.name, w + 10.0, 15.0, Weight::Medium),
        15.0,
        ink,
        Weight::Medium,
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    Primary,
    Secondary,
    Danger,
}

pub fn button<H: Copy>(c: &Canvas<H>, rect: Rect, label: &str, style: ButtonStyle, t: f32) {
    let radius = rect.height() / c.s / 2.0;
    let (fill, ink) = match style {
        ButtonStyle::Primary => (theme::accent(), theme::on_accent()),
        ButtonStyle::Secondary => (
            theme::rgba(theme::WHITE, lerp(12, 44, t)),
            if t > 0.5 {
                theme::text()
            } else {
                theme::text2()
            },
        ),
        ButtonStyle::Danger => (
            theme::rgba(theme::DANGER, lerp(40, 255, t)),
            if t > 0.5 {
                theme::rgb((24, 8, 8))
            } else {
                theme::rgb(theme::DANGER)
            },
        ),
    };
    // Focus stays inside the pill: the filled styles brighten and
    // get the frosted band and a light edge; Secondary is glass focus.
    c.fill(rect, radius, fill);
    match style {
        ButtonStyle::Secondary => {
            c.outline(rect, radius, 1.0, theme::rgba(theme::WHITE, 40));
            focus_pill(c, rect, radius, t);
        }
        _ if t > 0.01 => {
            c.rounded_gradient(rect, radius, theme::WHITE, (34.0 * t) as u8, 0);
            c.top_highlight(rect, radius, theme::rgba(theme::WHITE, (150.0 * t) as u8));
        }
        _ => {}
    }
    c.painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        c.font(16.0, Weight::Semibold),
        ink,
    );
}

/// One item of the top navigation pill. The open page is a solid accent
/// pill; focus adds brighter glass and the specular ring.
pub fn tab<H: Copy>(c: &Canvas<H>, rect: Rect, label: &str, selected: bool, t: f32) {
    let (x, y) = c.to_design(rect.min);
    let (w, h) = c.dsize(rect);
    let r = h / 2.0;
    if selected {
        c.fill(rect, r, theme::accent());
        c.top_highlight(rect, r, theme::rgba(theme::WHITE, (150.0 * t) as u8));
    } else {
        focus_pill(c, rect, r, t);
    }
    let ink = if selected {
        theme::on_accent()
    } else if t > 0.5 {
        theme::text()
    } else {
        theme::text2()
    };
    c.text(
        x + w / 2.0,
        y + h / 2.0,
        Align2::CENTER_CENTER,
        label,
        16.0,
        ink,
        if selected {
            Weight::Semibold
        } else {
            Weight::Medium
        },
    );
}

/// Width a tab needs for `label`.
pub fn tab_width<H: Copy>(c: &Canvas<H>, label: &str) -> f32 {
    c.text_width(label, 16.0, Weight::Semibold) + 40.0
}

/// A frosted pill tab bar centred on `cx` with one accent pill that
/// slides between tabs (Abyss-style, "the
/// blue should move"). The pill sits on `focus` while the bar has focus,
/// otherwise on `selected` (the page you are on). Returns each tab's
/// rect, for hit areas.
#[allow(clippy::too_many_arguments)]
pub fn tab_bar<H: Copy>(
    c: &Canvas<H>,
    id: impl std::hash::Hash + std::fmt::Debug + Copy,
    cx: f32,
    y: f32,
    h: f32,
    labels: &[&str],
    selected: Option<usize>,
    focus: Option<usize>,
    // Focus is on another control of the same bar (the account button,
    // the Control Center pill): the open page keeps only its dot, so the
    // pill does not look like it is still there.
    elsewhere: bool,
) -> Vec<Rect> {
    let widths: Vec<f32> = labels.iter().map(|l| tab_width(c, l)).collect();
    let pad = 6.0;
    let total: f32 = widths.iter().sum::<f32>() + pad * (widths.len() as f32 + 1.0);
    let nav = c.r(cx - total / 2.0, y, total, h);
    c.glass(nav, h / 2.0);
    let tf = c.anim((id, "tab-focus"), if focus.is_some() { 1.0 } else { 0.0 });
    if tf > 0.01 {
        c.outline(nav, h / 2.0, 1.5, theme::accent_a((150.0 * tf) as u8));
    }
    let mut rects = Vec::with_capacity(labels.len());
    let mut x = cx - total / 2.0 + pad;
    for w in &widths {
        rects.push(c.r(x, y + pad, *w, h - 2.0 * pad));
        x += w + pad;
    }
    let r = (h - 2.0 * pad) / 2.0;
    let cur = focus
        .or(selected.filter(|_| !elsewhere))
        .filter(|i| *i < rects.len());
    let pill = cur.map(|i| {
        let target = rects[i];
        let (tx, _) = c.to_design(target.min);
        let (tw, _) = c.dsize(target);
        // Slide (and stretch) from wherever it was.
        let px = c.anim((id, "tab-pill-x"), tx);
        let pw = c.anim((id, "tab-pill-w"), tw);
        let pill = c.r(px, y + pad, pw, h - 2.0 * pad);
        bar_pill(c, pill, r, tf);
        pill
    });
    for (i, (label, rect)) in labels.iter().zip(&rects).enumerate() {
        let (x, y) = c.to_design(rect.min);
        let (w, h) = c.dsize(*rect);
        // How much of this tab the pill covers decides its ink.
        let cover = pill
            .map(|p| {
                let o = (p.max.x.min(rect.max.x) - p.min.x.max(rect.min.x)).max(0.0);
                (o / rect.width().max(1.0)).clamp(0.0, 1.0)
            })
            .unwrap_or(0.0);
        let on = mix(theme::text(), theme::on_accent(), tf);
        let off = if Some(i) == selected && focus.is_some() {
            theme::text()
        } else {
            theme::text2()
        };
        let ink = Color32::from_rgba_unmultiplied(
            lerp(off.r(), on.r(), cover),
            lerp(off.g(), on.g(), cover),
            lerp(off.b(), on.b(), cover),
            255,
        );
        c.text(
            x + w / 2.0,
            y + h / 2.0,
            Align2::CENTER_CENTER,
            label,
            16.0,
            ink,
            if cover > 0.5 {
                Weight::Semibold
            } else {
                Weight::Medium
            },
        );
        // Where you are, while the pill is away: a small dot.
        if Some(i) == selected && cover < 0.5 {
            c.circle(
                x + w / 2.0,
                y + h - 5.0,
                2.0,
                theme::rgba(theme::WHITE, (160.0 * (1.0 - cover * 2.0)) as u8),
            );
        }
    }
    rects
}

/// iOS/Material-style switch. `None` draws a neutral unknown state.
pub fn toggle<H: Copy>(c: &Canvas<H>, x: f32, y: f32, on: Option<bool>) {
    let track = c.r(x, y, 44.0, 26.0);
    let (fill, knob_x) = match on {
        Some(true) => (theme::accent(), x + 31.0),
        Some(false) => (theme::rgba(theme::WHITE, 40), x + 13.0),
        None => (theme::rgba(theme::WHITE, 20), x + 22.0),
    };
    if on == Some(true) {
        c.shadow(track, 13.0, 0.0, 14.0, theme::accent_a(90));
    }
    c.fill(track, 13.0, fill);
    c.circle(knob_x, y + 13.0, 10.0, theme::rgb(theme::WHITE));
}

/// Horizontal level bar (volume, picture fit, progress).
pub fn level<H: Copy>(c: &Canvas<H>, x: f32, y: f32, w: f32, value: f32, color: Color32) {
    c.fill(c.r(x, y, w, 6.0), 3.0, theme::rgba(theme::WHITE, 30));
    let v = value.clamp(0.0, 1.0);
    if v > 0.0 {
        c.fill(c.r(x, y, (w * v).max(6.0), 6.0), 3.0, color);
    }
}

/// A row in a vertical list (settings rows, Wi-Fi networks, disks...).
pub struct ListRow<'a> {
    pub title: &'a str,
    pub subtitle: &'a str,
    pub value: &'a str,
    pub kind: Option<RowKind>,
    pub icon: Option<&'a str>,
    pub disabled: bool,
}

impl<'a> ListRow<'a> {
    pub fn simple(title: &'a str) -> Self {
        Self {
            title,
            subtitle: "",
            value: "",
            kind: None,
            icon: None,
            disabled: false,
        }
    }
}

/// Corner radius of list rows (and of their focus).
pub const ROW_RADIUS: f32 = 20.0;

pub fn list_row<H: Copy>(c: &Canvas<H>, rect: Rect, row: &ListRow, t: f32) {
    list_row_focus(c, rect, row, t, true);
}

/// A row whose focus is drawn by the list (one highlight that glides from
/// row to row, see [`glass_focus`]): `t` still brightens its text.
pub fn list_row_under_highlight<H: Copy>(c: &Canvas<H>, rect: Rect, row: &ListRow, t: f32) {
    list_row_focus(c, rect, row, t, false);
}

fn list_row_focus<H: Copy>(c: &Canvas<H>, rect: Rect, row: &ListRow, t: f32, own_focus: bool) {
    if row.kind == Some(RowKind::Primary) {
        return primary_row(c, rect, row, t);
    }
    if row.kind == Some(RowKind::Heading) {
        return heading_row(c, rect, row);
    }
    let radius = ROW_RADIUS;
    // Resting glass: a faint pane and hairline (no outer glow, no
    // lift; the focus stays inside the row and the panel).
    c.fill(rect, radius, theme::rgba(theme::WHITE, 12));
    c.outline(rect, radius, 1.0, theme::rgba(theme::WHITE, 14));
    if own_focus {
        focus_pill(c, rect, radius, t);
    }
    let (x, y) = c.to_design(rect.min);
    let (w, h) = c.dsize(rect);
    // The focused row's text is full white; the others a little dimmer.
    let ink = if row.disabled {
        theme::text3()
    } else if t > 0.5 {
        theme::text()
    } else {
        theme::text2()
    };
    let mut tx = x + 22.0;
    if let Some(icon) = row.icon {
        let r = (h * 0.32).min(20.0);
        icon_badge(
            c,
            x + 20.0 + r,
            y + h / 2.0,
            r,
            icon,
            false,
            if row.disabled { 0.0 } else { t },
        );
        tx = x + 36.0 + 2.0 * r;
    }
    let value_w = if row.value.is_empty() {
        0.0
    } else {
        c.text_width(row.value, 15.0, Weight::Medium) + 24.0
    };
    let right_w = match row.kind {
        Some(RowKind::Toggle(_)) => 64.0,
        Some(RowKind::Slider(_)) => 210.0,
        Some(RowKind::Choice) => value_w + 40.0,
        Some(RowKind::Action) => 30.0 + value_w,
        Some(RowKind::Soon) => 120.0,
        _ => value_w,
    };
    let text_w = (w - (tx - x) - right_w - 16.0).max(60.0);
    let title_y = if row.subtitle.is_empty() {
        y + h / 2.0
    } else {
        y + h / 2.0 - 9.0
    };
    c.text(
        tx,
        title_y,
        Align2::LEFT_CENTER,
        &c.fit(row.title, text_w, 16.5, Weight::Medium),
        16.5,
        ink,
        Weight::Medium,
    );
    if !row.subtitle.is_empty() {
        c.text(
            tx,
            y + h / 2.0 + 11.0,
            Align2::LEFT_CENTER,
            &c.fit(row.subtitle, text_w, 13.0, Weight::Regular),
            13.0,
            theme::text3(),
            Weight::Regular,
        );
    }
    let rx = x + w - 18.0;
    let cy = y + h / 2.0;
    let value_text = |c: &Canvas<H>, right: f32, col: Color32| {
        c.text(
            right,
            cy,
            Align2::RIGHT_CENTER,
            row.value,
            15.0,
            col,
            Weight::Medium,
        );
    };
    match row.kind {
        Some(RowKind::Toggle(on)) => toggle(c, rx - 44.0, cy - 13.0, on),
        Some(RowKind::Slider(v)) => {
            value_text(c, rx, theme::text2());
            level(
                c,
                rx - 200.0,
                cy - 3.0,
                130.0,
                v as f32 / 100.0,
                theme::accent(),
            );
        }
        Some(RowKind::Choice) => {
            let col = if t > 0.5 {
                theme::text()
            } else {
                theme::text2()
            };
            icons::draw(c, "chevron", rx - 6.0, cy, 13.0, col);
            value_text(c, rx - 22.0, col);
            let vw = c.text_width(row.value, 15.0, Weight::Medium);
            icons::draw(c, "back", rx - 36.0 - vw, cy, 13.0, col);
        }
        Some(RowKind::Soon) => {
            let pill = c.r(rx - 104.0, cy - 12.0, 104.0, 24.0);
            c.fill(pill, 12.0, theme::rgba(theme::WHITE, 14));
            c.text(
                rx - 52.0,
                cy,
                Align2::CENTER_CENTER,
                "Coming soon",
                12.0,
                theme::text3(),
                Weight::Medium,
            );
        }
        Some(RowKind::Action) => {
            icons::draw(c, "chevron", rx - 6.0, cy, 14.0, theme::text3());
            if !row.value.is_empty() {
                value_text(c, rx - 24.0, theme::text2());
            }
        }
        _ => {
            if !row.value.is_empty() {
                value_text(c, rx, theme::text2());
            }
        }
    }
    // Greyed out (it depends on another setting): a veil over all of it.
    // Focus still shows on it, as an outline over the veil.
    if row.disabled && row.kind != Some(RowKind::Soon) {
        c.fill(rect, radius, theme::rgba(theme::BG_TOP, 150));
        if t > 0.01 {
            c.outline(
                rect,
                radius,
                1.5,
                theme::rgba(theme::WHITE, (130.0 * t) as u8),
            );
        }
    }
}

/// A heading between rows, like the page title but smaller, with its
/// status beside it. No pane: it is not something to pick.
fn heading_row<H: Copy>(c: &Canvas<H>, rect: Rect, row: &ListRow) {
    let (x, y) = c.to_design(rect.min);
    let (_, h) = c.dsize(rect);
    let base = y + h - 14.0;
    c.text(
        x + 6.0,
        base,
        Align2::LEFT_BOTTOM,
        row.title,
        21.0,
        theme::text(),
        Weight::Semibold,
    );
    if !row.value.is_empty() {
        let tw = c.text_width(row.title, 21.0, Weight::Semibold);
        c.text(
            x + 6.0 + tw + 14.0,
            base - 2.0,
            Align2::LEFT_BOTTOM,
            row.value,
            15.0,
            theme::text3(),
            Weight::Medium,
        );
    }
}

/// A page's main action ("Pair a controller or remote"): an accent glass
/// button with a + badge, so it stands apart from the list below it.
fn primary_row<H: Copy>(c: &Canvas<H>, rect: Rect, row: &ListRow, t: f32) {
    let radius = ROW_RADIUS;
    let accent = theme::accent_rgb();
    c.fill(rect, radius, theme::rgba(theme::BG_TOP, 120));
    c.fill(rect, radius, theme::rgba(accent, lerp(46, 92, t)));
    c.frost_edge(rect, radius, 1.0 + t);
    c.outline(
        rect,
        radius,
        1.0 + 0.5 * t,
        theme::rgba(lighten(accent, 0.45 * t), lerp(90, 210, t)),
    );
    let (x, y) = c.to_design(rect.min);
    let (w, h) = c.dsize(rect);
    let r = (h * 0.32).min(20.0);
    let (cx, cy) = (x + 20.0 + r, y + h / 2.0);
    c.circle(cx, cy, r, theme::accent());
    icons::draw(
        c,
        row.icon.unwrap_or("plus"),
        cx,
        cy,
        r * 0.95,
        theme::on_accent(),
    );
    c.text(
        x + 36.0 + 2.0 * r,
        cy,
        Align2::LEFT_CENTER,
        &c.fit(row.title, w - 120.0, 16.5, Weight::Semibold),
        16.5,
        theme::text(),
        Weight::Semibold,
    );
    icons::draw(c, "chevron", x + w - 24.0, cy, 14.0, theme::text2());
}

/// Centred modal sheet over a scrim. Returns its rect (screen space).
pub fn modal<H: Copy>(c: &Canvas<H>, w: f32, h: f32) -> Rect {
    c.scrim();
    let rect = c.r((1280.0 - w) / 2.0, (720.0 - h) / 2.0, w, h);
    c.sheet(rect, 30.0);
    rect
}

/// Bottom-centre toast.
pub fn toast<H: Copy>(c: &Canvas<H>, text: &str, alpha: f32) {
    if alpha <= 0.01 {
        return;
    }
    let w = (c.text_width(text, 15.0, Weight::Medium) + 64.0).min(1000.0);
    let rect = c.r(640.0 - w / 2.0, 632.0, w, 48.0);
    let a = |v: f32| (alpha * v) as u8;
    c.shadow(rect, 24.0, 10.0, 30.0, theme::rgba(theme::BLACK, a(120.0)));
    c.fill(rect, 24.0, theme::rgba((28, 30, 42), a(240.0)));
    c.outline(rect, 24.0, 1.0, theme::rgba(theme::WHITE, a(36.0)));
    c.circle(
        640.0 - w / 2.0 + 26.0,
        656.0,
        4.0,
        theme::accent_a(a(255.0)),
    );
    c.text(
        640.0 - w / 2.0 + 40.0,
        656.0,
        Align2::LEFT_CENTER,
        &c.fit(text, w - 56.0, 15.0, Weight::Medium),
        15.0,
        theme::rgba(theme::INK, a(255.0)),
        Weight::Medium,
    );
}

/// Text field showing the on-screen keyboard's text with a caret.
/// `pill`: fully rounded with a leading search icon (Search page).
pub fn text_field<H: Copy>(
    c: &Canvas<H>,
    rect: Rect,
    osk: &Osk,
    placeholder: &str,
    focused: bool,
    pill: bool,
) {
    let (x, y) = c.to_design(rect.min);
    let (w, h) = c.dsize(rect);
    let radius = if pill { h / 2.0 } else { 16.0 };
    if focused {
        c.shadow(rect, radius, 0.0, 30.0, theme::accent_a(55));
    }
    c.fill(rect, radius, theme::rgba(theme::BLACK, 70));
    c.fill(
        rect,
        radius,
        theme::rgba(theme::WHITE, if focused { 16 } else { 10 }),
    );
    c.outline(
        rect,
        radius,
        if focused { 1.5 } else { 1.0 },
        if focused {
            theme::accent_a(190)
        } else {
            theme::border()
        },
    );
    let size = if pill { 22.0 } else { 19.0 };
    let mut tx = x + 20.0;
    if pill {
        icons::draw(c, "search", x + 34.0, y + h / 2.0, 22.0, theme::text2());
        tx = x + 62.0;
    }
    let cy = y + h / 2.0;
    let caret = |c: &Canvas<H>, cx: f32| {
        if focused {
            c.segment(
                (cx, cy - size * 0.62),
                (cx, cy + size * 0.62),
                2.0,
                theme::accent(),
            );
        }
    };
    let (before, after) = osk.display_split();
    let shown = osk.display_text();
    let field_focus = focused && osk.in_field();
    if field_focus {
        c.outline(rect, radius, 2.0, theme::accent());
    }
    if shown.is_empty() {
        c.text(
            tx,
            cy,
            Align2::LEFT_CENTER,
            placeholder,
            size,
            theme::text3(),
            Weight::Regular,
        );
        caret(c, tx - 2.0);
    } else {
        let fitted = c.fit(&shown, w - (tx - x) - 40.0, size, Weight::Medium);
        c.text(
            tx,
            cy,
            Align2::LEFT_CENTER,
            &fitted,
            size,
            theme::text(),
            Weight::Medium,
        );
        // The caret sits after the text before it; clamp when the
        // field was shortened with an ellipsis.
        let fitted_chars = fitted.chars().count();
        let shown_before: String = before.chars().take(fitted_chars).collect();
        let cx = tx + c.text_width(&shown_before, size, Weight::Medium) + 2.0;
        let _ = after;
        caret(c, cx);
    }
}

/// The on-screen keyboard's keys. Pill keys; the focused key is a solid
/// accent pill. `key_hit` maps a key position to a pointer target.
#[allow(clippy::too_many_arguments)]
pub fn keys<H: Copy>(
    c: &Canvas<H>,
    osk: &Osk,
    x: f32,
    y: f32,
    w: f32,
    focused: bool,
    id: &str,
    key_hit: &dyn Fn(usize, usize) -> H,
) {
    let rows = osk.rows();
    let (fr, fc) = osk.focus();
    let gap = 7.0;
    let unit = (w - gap * (ROW_UNITS as f32 - 1.0)) / ROW_UNITS as f32;
    let key_h = 44.0;
    let mut ky = y;
    for (ri, row) in rows.iter().enumerate() {
        let mut kx = x;
        for (ci, key) in row.iter().enumerate() {
            let kw = unit * key.width as f32 + gap * (key.width as f32 - 1.0);
            let is_focus = focused && ri == fr && ci == fc;
            let t = c.anim((id, ri, ci), if is_focus { 1.0 } else { 0.0 });
            let base = c.r(kx, ky, kw, key_h);
            c.hit(base, key_hit(ri, ci));
            let rect = base;
            let r = key_h / 2.0;
            let special = !matches!(key.kind, KeyKind::Char(_));
            let done = key.kind == KeyKind::Done;
            let fill = if done {
                theme::accent_a(70)
            } else if special {
                theme::rgba(theme::WHITE, 12)
            } else {
                theme::rgba(theme::WHITE, 24)
            };
            c.fill(rect, r, fill);
            focus_pill(c, rect, r, t);
            let ink = if t > 0.5 {
                theme::text()
            } else {
                theme::text2()
            };
            let (cx, cy) = c.to_design(rect.center());
            if key.kind == KeyKind::Erase {
                c.closed(
                    &[
                        (cx + 11.0, cy - 8.0),
                        (cx - 4.0, cy - 8.0),
                        (cx - 12.0, cy),
                        (cx - 4.0, cy + 8.0),
                        (cx + 11.0, cy + 8.0),
                    ],
                    1.8,
                    ink,
                );
            } else {
                let size = if special { 14.0 } else { 18.0 };
                c.text(
                    cx,
                    cy,
                    Align2::CENTER_CENTER,
                    &key.label,
                    size,
                    ink,
                    if special {
                        Weight::Semibold
                    } else {
                        Weight::Medium
                    },
                );
            }
            kx += kw + gap;
        }
        ky += key_h + gap;
    }
}

/// Height of [`keys`].
pub fn keys_height() -> f32 {
    5.0 * 44.0 + 4.0 * 7.0
}

/// On-screen keyboard with its text field (modals: Wi-Fi password, Store
/// search). `focused` = keyboard has focus.
#[allow(clippy::too_many_arguments)]
pub fn keyboard<H: Copy>(
    c: &Canvas<H>,
    osk: &Osk,
    x: f32,
    y: f32,
    w: f32,
    placeholder: &str,
    focused: bool,
    id: &str,
    key_hit: &dyn Fn(usize, usize) -> H,
) {
    text_field(c, c.r(x, y, w, 54.0), osk, placeholder, focused, false);
    keys(c, osk, x, y + 68.0, w, focused, id, key_hit);
}

/// Height used by [`keyboard`].
pub fn keyboard_height() -> f32 {
    68.0 + keys_height()
}

/// Material 3 Expressive–style wavy progress bar: the filled part is a
/// travelling sine wave in the accent, the rest a flat track ending in a
/// small stop dot. `now` (seconds) moves the wave.
pub fn progress_bar<H: Copy>(c: &Canvas<H>, x: f32, y: f32, w: f32, fraction: f32, now: f64) {
    let f = fraction.clamp(0.0, 1.0);
    let cy = y + 4.0;
    let stroke = 4.0;
    let amp = 3.0;
    let wave = 26.0;
    let gap = 8.0;
    let fw = (w * f).max(0.0);
    // Track (flat), after a small gap.
    let track_start = (x + fw + gap).min(x + w);
    if track_start < x + w - 1.0 {
        c.segment(
            (track_start, cy),
            (x + w - 3.0, cy),
            stroke,
            theme::rgba(theme::WHITE, 40),
        );
    }
    c.circle(x + w - 2.0, cy, 2.2, theme::accent_a(200));
    if fw < 2.0 {
        return;
    }
    let phase = (now * 2.4) as f32;
    let steps = ((fw / 3.0) as usize).max(2);
    let pts: Vec<(f32, f32)> = (0..=steps)
        .map(|i| {
            let px = x + fw * i as f32 / steps as f32;
            // Taper the wave near both ends so the line starts and ends flat.
            let edge = ((px - x) / 12.0).min((x + fw - px) / 12.0).clamp(0.0, 1.0);
            let py = cy + amp * edge * ((px - x) / wave * std::f32::consts::TAU - phase).sin();
            (px, py)
        })
        .collect();
    c.polyline(&pts, stroke, theme::accent());
    // Round caps.
    for &(px, py) in [pts[0], pts[pts.len() - 1]].iter() {
        c.circle(px, py, stroke / 2.0, theme::accent());
    }
}

/// Material 3 Expressive–style indeterminate indicator centred on (x, y):
/// a "snake" arc whose head races ahead and tail catches up while the
/// whole thing rotates, drawn as a gentle wave around the circle.
pub fn spinner<H: Copy>(c: &Canvas<H>, x: f32, y: f32, r: f32, now: f64) {
    use std::f32::consts::TAU;
    let period = 1.6_f64;
    let cycle = (now / period).floor() as f32;
    let p = ((now % period) / period) as f32;
    let ease = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let span = 0.72 * TAU; // how far the head runs ahead per cycle
    let base = cycle * span + now as f32 * TAU * 0.22;
    let head = base + ease(p * 2.0) * span;
    let tail = base + ease(p * 2.0 - 1.0) * span;
    let (a0, a1) = (tail, head + 0.12 * TAU);
    let stroke = (r * 0.2).max(2.5);
    let amp = r * 0.07;
    let waves = 9.0;
    let steps = 48;
    let pts: Vec<(f32, f32)> = (0..=steps)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            let rr = r + amp * (a * waves - now as f32 * 6.0).sin();
            (x + rr * a.cos(), y + rr * a.sin())
        })
        .collect();
    c.ring(x, y, r, stroke * 0.55, theme::rgba(theme::WHITE, 16));
    c.polyline(&pts, stroke, theme::accent());
    for &(px, py) in [pts[0], pts[pts.len() - 1]].iter() {
        c.circle(px, py, stroke / 2.0, theme::accent());
    }
}

/// "Searching" ripples behind an icon (Bluetooth / Wi-Fi discovery): soft
/// light that swells out of the centre and fades, three waves apart, plus
/// a hairline edge on each wave. No fixed rings (it looked like a
/// target).
pub fn pulse<H: Copy>(c: &Canvas<H>, x: f32, y: f32, r: f32, now: f64) {
    let period = 2.4_f64;
    for k in 0..3 {
        let p = (((now / period) + k as f64 / 3.0) % 1.0) as f32;
        let ease = 1.0 - (1.0 - p).powi(3);
        let rr = r * (1.0 + 1.7 * ease);
        let fade = (1.0 - p) * (1.0 - p);
        c.glow(x, y, rr, theme::accent_rgb(), (60.0 * fade) as u8);
        c.ring(x, y, rr * 0.72, 1.0, theme::accent_a((70.0 * fade) as u8));
    }
}

/// Big "searching" indicator: ripples around a glass disc with `icon`.
pub fn radar<H: Copy>(c: &Canvas<H>, x: f32, y: f32, icon: &str, now: f64) {
    pulse(c, x, y, 40.0, now);
    // Gentle breathing of the disc itself.
    let b = (0.5 + 0.5 * (now * 2.6).sin()) as f32;
    c.shadow(
        c.r(x - 40.0, y - 40.0, 80.0, 80.0),
        40.0,
        10.0,
        30.0,
        theme::rgba(theme::BLACK, 90),
    );
    c.circle(x, y, 40.0, theme::rgba(theme::PANEL, 235));
    c.circle(x, y, 40.0, theme::accent_a((26.0 + 18.0 * b) as u8));
    c.ring(x, y, 40.0, 1.0, theme::rgba(theme::WHITE, 40));
    icons::draw(c, icon, x, y, 30.0, theme::accent());
}

/// A status line with a small spinner in front while `busy`.
pub fn busy_line<H: Copy>(c: &Canvas<H>, x: f32, y: f32, text: &str, busy: bool, now: f64) {
    let mut tx = x;
    if busy {
        spinner(c, x + 8.0, y, 8.0, now);
        tx += 26.0;
    }
    if !text.is_empty() {
        c.text(
            tx,
            y,
            Align2::LEFT_CENTER,
            text,
            14.0,
            if busy { theme::text2() } else { theme::text3() },
            Weight::Regular,
        );
    }
}

/// Page title.
pub fn title<H: Copy>(c: &Canvas<H>, x: f32, y: f32, text: &str, size: f32) {
    c.text(
        x,
        y,
        Align2::LEFT_TOP,
        text,
        size,
        theme::text(),
        Weight::Semibold,
    );
}

/// A keycap label (`Esc`, `Home`, `↑↓←→`) drawn as a small glass key.
/// Returns its width.
pub fn keycap<H: Copy>(c: &Canvas<H>, x: f32, cy: f32, label: &str) -> f32 {
    let w = (c.text_width(label, 14.0, Weight::Semibold) + 22.0).max(34.0);
    let r = c.r(x, cy - 17.0, w, 34.0);
    c.fill(r, 9.0, theme::rgba(theme::WHITE, 26));
    c.outline(r, 9.0, 1.0, theme::border_hi());
    c.fill(
        c.r(x + 2.0, cy + 13.0, w - 4.0, 2.0),
        1.0,
        theme::rgba(theme::BLACK, 60),
    );
    c.text(
        x + w / 2.0,
        cy - 1.0,
        Align2::CENTER_CENTER,
        label,
        14.0,
        theme::text(),
        Weight::Semibold,
    );
    w
}

/// The key guide (installer "Done" page and the shell's Keys overlay):
/// keycaps on the left, what they do on the right. Returns the height.
pub fn key_guide<H: Copy>(c: &Canvas<H>, x: f32, y: f32, w: f32, line: f32) -> f32 {
    let rows: [(&[&str], &str); 7] = [
        (&["↑", "↓", "←", "→"], "Move"),
        (&["Enter"], "Select · hold for options"),
        (&["Esc"], "Back"),
        (&["Esc", "Esc", "Esc"], "Leave an app (or hold Esc)"),
        (&["Home"], "CatosTV Home, from anywhere"),
        (&["F10"], "Quick settings"),
        (&["Type"], "Search"),
    ];
    for (i, (keys, what)) in rows.iter().enumerate() {
        let cy = y + 20.0 + i as f32 * line;
        let mut kx = x;
        for k in keys.iter() {
            kx += keycap(c, kx, cy, k) + 6.0;
        }
        c.text(
            x + w * 0.42,
            cy,
            Align2::LEFT_CENTER,
            what,
            16.0,
            theme::text2(),
            Weight::Regular,
        );
    }
    rows.len() as f32 * line
}
