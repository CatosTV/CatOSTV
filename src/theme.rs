//! Shared visual tokens and global egui setup for shell and installer.
//!
//! Design direction (docs/BRANDING_AND_UI.md): **dark glass**. A deep
//! obsidian backdrop lit by soft colour glows; translucent "glass"
//! surfaces with hairline borders; pill-shaped controls; focus = a lift,
//! brighter glass, a bright specular ring and a coloured bloom underneath;
//! one user-selectable accent colour (Settings → Customization) for selection,
//! progress and blooms. Calm, label-first copy, no marketing sentences.
//!
//! Colours are built with [`rgba`] from `(r, g, b)` tuples. Never derive a
//! new colour from `Color32::r()/g()/b()` of a translucent colour: egui
//! stores colours premultiplied, so those channels are already darkened.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

pub type Rgb = (u8, u8, u8);

/// Unmultiplied colour from an RGB tuple and alpha.
pub fn rgba(c: Rgb, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.0, c.1, c.2, a)
}

pub fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

// ---------------------------------------------------------------- base

/// Backdrop, top and bottom of the vertical gradient.
pub const BG_TOP: Rgb = (15, 18, 28);
pub const BG_BOTTOM: Rgb = (6, 8, 13);
/// Primary text.
pub const INK: Rgb = (245, 246, 250);
pub const WHITE: Rgb = (255, 255, 255);
pub const BLACK: Rgb = (0, 0, 0);
pub const SUCCESS: Rgb = (74, 222, 128);
pub const WARNING: Rgb = (251, 191, 36);
pub const DANGER: Rgb = (248, 113, 113);

pub fn text() -> Color32 {
    rgb(INK)
}
/// Secondary text (labels, values).
pub fn text2() -> Color32 {
    rgba(INK, 165)
}
/// Tertiary text (hints, disabled).
pub fn text3() -> Color32 {
    rgba(INK, 105)
}
/// Glass surface fill (panels).
pub fn glass() -> Color32 {
    rgba(WHITE, 15)
}
/// Raised glass (rows, tiles at rest).
pub fn glass_hi() -> Color32 {
    rgba(WHITE, 22)
}
/// Focused glass.
pub fn glass_focus() -> Color32 {
    rgba(WHITE, 44)
}
/// Bright edge of a focused element ("specular ring").
pub fn specular(a: f32) -> Color32 {
    rgba(WHITE, (a.clamp(0.0, 1.0) * 190.0) as u8)
}
/// Opaque panel colour for sheets and side panels.
pub const PANEL: Rgb = (20, 24, 36);
pub fn border() -> Color32 {
    rgba(WHITE, 22)
}
pub fn border_hi() -> Color32 {
    rgba(WHITE, 48)
}
/// Modal scrim.
pub fn scrim() -> Color32 {
    rgba(BLACK, 165)
}
pub fn shadow() -> Color32 {
    rgba(BLACK, 110)
}

// -------------------------------------------------------------- accent

/// Interface accents selectable in Settings → Customization. Names match
/// `catostv_core::prefs::ACCENTS`.
pub const ACCENTS: &[(&str, Rgb)] = &[
    ("violet", (139, 124, 255)),
    ("periwinkle", (139, 164, 254)),
    ("blue", (77, 142, 255)),
    ("teal", (45, 212, 191)),
    ("green", (74, 222, 128)),
    ("amber", (251, 191, 36)),
    ("rose", (251, 113, 133)),
    ("red", (248, 102, 102)),
    ("silver", (214, 220, 232)),
];

static ACCENT: AtomicU32 = AtomicU32::new(0x8B7CFF);

pub fn accent_rgb_of(name: &str) -> Rgb {
    ACCENTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .unwrap_or(ACCENTS[0].1)
}

/// Switch the interface accent (takes effect on the next frame).
pub fn set_accent(name: &str) {
    let (r, g, b) = accent_rgb_of(name);
    ACCENT.store(u32::from_be_bytes([0, r, g, b]), Ordering::Relaxed);
}

pub fn accent_rgb() -> Rgb {
    let [_, r, g, b] = ACCENT.load(Ordering::Relaxed).to_be_bytes();
    (r, g, b)
}

pub fn accent() -> Color32 {
    rgb(accent_rgb())
}

pub fn accent_a(a: u8) -> Color32 {
    rgba(accent_rgb(), a)
}

/// Text colour that reads well on a solid accent fill.
pub fn on_accent() -> Color32 {
    let (r, g, b) = accent_rgb();
    let luma = 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
    if luma > 150.0 {
        rgb((12, 13, 20))
    } else {
        rgb(WHITE)
    }
}

/// Colour of an app tile accent (`apps.toml` `accent`).
pub fn app_rgb(accent: &str) -> Rgb {
    match accent {
        "violet" => (124, 92, 255),
        "red" => (239, 68, 68),
        "blue" => (59, 130, 246),
        "green" => (34, 197, 94),
        "sky" => (14, 165, 233),
        "orange" => (249, 115, 22),
        _ => accent_rgb(),
    }
}

// --------------------------------------------------------------- fonts

/// Font family names. Always defined by [`setup`] (falling back to the
/// regular face), so using them can never panic.
pub const MEDIUM: &str = "catostv-medium";
pub const SEMIBOLD: &str = "catostv-semibold";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Medium,
    Semibold,
}

/// Inter collection shipped by Arch's `inter-font`.
const INTER_TTC: &str = "/usr/share/fonts/inter/Inter.ttc";
/// Single-face fallbacks (other distros / development machines).
const SINGLE: &[(&str, Weight)] = &[
    (
        "/usr/share/fonts/opentype/inter/Inter-Regular.otf",
        Weight::Regular,
    ),
    (
        "/usr/share/fonts/opentype/inter/Inter-Medium.otf",
        Weight::Medium,
    ),
    (
        "/usr/share/fonts/opentype/inter/Inter-SemiBold.otf",
        Weight::Semibold,
    ),
    (
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        Weight::Regular,
    ),
    ("/usr/share/fonts/noto/NotoSans-Medium.ttf", Weight::Medium),
    (
        "/usr/share/fonts/noto/NotoSans-SemiBold.ttf",
        Weight::Semibold,
    ),
    ("/usr/share/fonts/noto/NotoSans-Bold.ttf", Weight::Semibold),
    (
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        Weight::Regular,
    ),
    (
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
        Weight::Semibold,
    ),
];

fn load_face(w: Weight) -> Option<FontData> {
    if let Ok(bytes) = std::fs::read(INTER_TTC) {
        let style = match w {
            Weight::Regular => "Regular",
            Weight::Medium => "Medium",
            Weight::Semibold => "SemiBold",
        };
        if let Some(index) = catostv_core::fonts::find_face(&bytes, "Inter", style) {
            let mut fd = FontData::from_owned(bytes);
            fd.index = index;
            return Some(fd);
        }
    }
    SINGLE
        .iter()
        .filter(|(_, sw)| *sw == w)
        .find_map(|(p, _)| std::fs::read(p).ok())
        .map(FontData::from_owned)
}

/// Install fonts and base visuals. Call once from the app constructor.
pub fn setup(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let base = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut family = |w: Weight, key: &str| -> Vec<String> {
        let mut list = base.clone();
        // Regular is the fallback for the heavier weights.
        for (k, weight) in [(key, w), ("catostv-regular", Weight::Regular)] {
            if fonts.font_data.contains_key(k) {
                list.insert(0, k.to_string());
                break;
            }
            if let Some(fd) = load_face(weight) {
                fonts.font_data.insert(k.to_string(), Arc::new(fd));
                list.insert(0, k.to_string());
                break;
            }
        }
        list
    };
    let regular = family(Weight::Regular, "catostv-regular");
    let medium = family(Weight::Medium, "catostv-medium-face");
    let semibold = family(Weight::Semibold, "catostv-semibold-face");
    fonts.families.insert(FontFamily::Proportional, regular);
    fonts
        .families
        .insert(FontFamily::Name(MEDIUM.into()), medium);
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), semibold);
    ctx.set_fonts(fonts);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = rgb(BG_BOTTOM);
    visuals.window_fill = rgb(BG_TOP);
    visuals.override_text_color = Some(text());
    ctx.set_visuals(visuals);
}

/// Greeting for the current hour.
pub fn greeting(hour: u32) -> &'static str {
    match hour {
        5..=11 => "Good morning",
        12..=16 => "Good afternoon",
        17..=21 => "Good evening",
        _ => "Good night",
    }
}
