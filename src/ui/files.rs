//! Drawing for the built-in Files app: a places pane on the
//! left, the folder's contents on the right under a breadcrumb, Papirus
//! icons by file type, and the sheets (options, delete, name entry).
//! Presentation of `catostv_core::files::Files` only.

use crate::canvas::Canvas;
use crate::icons;
use crate::images::Images;
use crate::theme::{self, Weight};
use crate::widgets::{self, ButtonStyle};
use catostv_core::files::{self, EditKind, Files, FilesMenu, Kind, Pane};
use catostv_core::installer::Scan;
use catostv_core::shell::Target;
use eframe::egui::{Align2, Color32};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

type C = Canvas<Target>;

const SIDEBAR_W: f32 = 300.0;
const LIST_X: f32 = 352.0;
const LIST_W: f32 = 864.0;
const TOP: f32 = 112.0;
const ROW_H: f32 = 60.0;
const ROW_GAP: f32 = 6.0;

/// Papirus icon file for a mime/place/device icon name, looked up once.
fn icon_path(name: &str) -> Option<String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().ok().and_then(|c| c.get(name).cloned()) {
        return v;
    }
    let found = ["Papirus-Dark", "Papirus"]
        .iter()
        .flat_map(|theme| {
            ["64x64", "48x48", "96x96"].iter().flat_map(move |size| {
                ["mimetypes", "places", "devices", "apps"]
                    .iter()
                    .map(move |cat| format!("/usr/share/icons/{theme}/{size}/{cat}/{name}.svg"))
            })
        })
        .find(|p| std::path::Path::new(p).is_file());
    if let Ok(mut c) = cache.lock() {
        c.insert(name.to_string(), found.clone());
    }
    found
}

/// Drawn stand-in while no Papirus icon is available.
fn fallback_icon(c: &C, kind: Kind, x: f32, y: f32, r: f32) {
    let (name, col) = match kind {
        Kind::Folder => ("folder", theme::app_rgb("orange")),
        Kind::Drive => ("disk", theme::app_rgb("sky")),
        Kind::Video => ("play", theme::app_rgb("violet")),
        Kind::Audio => ("music", theme::app_rgb("green")),
        Kind::Image => ("spark", theme::app_rgb("sky")),
        Kind::Apk => ("android", theme::app_rgb("green")),
        _ => ("download", theme::INK),
    };
    icons::draw(c, name, x, y, r, theme::rgb(col));
}

fn draw_icon(c: &C, images: &Images, kind: Kind, x: f32, y: f32, size: f32) {
    if let Some(tex) = icon_path(kind.icon()).and_then(|p| images.get(&c.ctx, &p)) {
        c.image(
            c.r(x - size / 2.0, y - size / 2.0, size, size),
            tex,
            Color32::WHITE,
        );
    } else {
        fallback_icon(c, kind, x, y, size * 0.42);
    }
}

pub fn draw(c: &C, f: &Files, images: &Images, now: f64) {
    let k = 1.0 - (1.0 - c.appear("files-in", 0.3)).powi(3);
    // Brand.
    icons::draw(
        c,
        "folder",
        76.0,
        52.0,
        22.0,
        theme::rgb(theme::app_rgb("orange")),
    );
    c.text(
        98.0 - 10.0 * (1.0 - k),
        52.0,
        Align2::LEFT_CENTER,
        "Files",
        22.0,
        theme::rgba(theme::INK, (255.0 * k) as u8),
        Weight::Semibold,
    );
    if let Some(cb) = &f.clipboard {
        let label = format!(
            "{} {} · Options → Paste",
            if cb.cut { "Moving" } else { "Copying" },
            cb.entry.name
        );
        c.text(
            1216.0,
            52.0,
            Align2::RIGHT_CENTER,
            &c.fit(&label, 600.0, 14.0, Weight::Medium),
            14.0,
            theme::accent(),
            Weight::Medium,
        );
    }

    draw_places(c, f, images);
    let key = f.current_dir().map(|e| e.path.clone());
    let enter = c.entrance("files-directory", key, crate::motion::PAGE);
    let list = c.translated(0.0, 12.0 * (1.0 - enter)).opacity(enter);
    draw_list(&list, f, images, now);
    let open = c.anim_ease(
        "files-menu-visible",
        if f.menu.is_some() { 1.0 } else { 0.0 },
        crate::motion::SHEET,
    );
    let shown = c.remember("files-menu", |old: Option<Option<FilesMenu>>| {
        f.menu.clone().or_else(|| old.flatten())
    });
    if let Some(menu) = shown.filter(|_| open > 0.001) {
        c.opacity(open).scrim();
        c.hit(c.screen, Target::Outside);
        let layer = c
            .translated(0.0, 16.0 * (1.0 - open))
            .opacity(open)
            .without_scrim();
        let layer = if f.menu.is_some() {
            layer
        } else {
            layer.passive()
        };
        if f.menu.is_some() {
            draw_menu(&layer, f);
        } else {
            let mut closing = f.clone();
            closing.menu = Some(menu);
            draw_menu(&layer, &closing);
        }
    }
}

fn draw_places(c: &C, f: &Files, images: &Images) {
    let pane_focus = f.pane == Pane::Places && f.menu.is_none();
    let panel = c.r(40.0, TOP - 16.0, SIDEBAR_W, 720.0 - TOP);
    c.fill(panel, 24.0, theme::rgba(theme::BG_TOP, 110));
    c.frost_edge(panel, 24.0, 0.4);
    let (h, gap) = (54.0, 4.0);
    for (i, p) in f.places.iter().enumerate() {
        let y = TOP + i as f32 * (h + gap);
        if y + h > 720.0 {
            break;
        }
        let current = i == f.place;
        let t = c.anim(
            ("files-place", i),
            if pane_focus && current { 1.0 } else { 0.0 },
        );
        let rect = c.r(52.0, y, SIDEBAR_W - 24.0, h);
        if current {
            // Where you are: a quiet fill; the ring only while the pane
            // has focus.
            c.fill(rect, 16.0, theme::rgba(theme::WHITE, 16));
            c.fill(
                c.r(52.0, y + 14.0, 4.0, h - 28.0),
                2.0,
                theme::accent_a((190.0 + 65.0 * t) as u8),
            );
        }
        widgets::focus_pill(c, rect, 16.0, t);
        draw_icon(c, images, p.kind, 84.0, y + h / 2.0, 30.0);
        c.text(
            110.0,
            y + h / 2.0,
            Align2::LEFT_CENTER,
            &c.fit(&p.name, SIDEBAR_W - 90.0, 17.0, Weight::Semibold),
            17.0,
            if current {
                theme::text()
            } else {
                theme::text2()
            },
            if current {
                Weight::Semibold
            } else {
                Weight::Medium
            },
        );
        c.hit(rect, Target::FilesPlace(i));
    }
}

fn draw_list(c: &C, f: &Files, images: &Images, now: f64) {
    // Breadcrumb over the list.
    let title = f.title();
    c.text(
        LIST_X,
        TOP - 30.0,
        Align2::LEFT_CENTER,
        &c.fit(&title, LIST_W - 220.0, 20.0, Weight::Semibold),
        20.0,
        theme::text(),
        Weight::Semibold,
    );
    let rows = f.rows();
    let count = match &f.entries {
        Scan::Done(v) if !v.is_empty() => {
            let folders = v.iter().filter(|e| e.is_dir()).count();
            let files = v.len() - folders;
            let plural = |n: usize, one: &str| {
                if n == 1 {
                    format!("1 {one}")
                } else {
                    format!("{n} {one}s")
                }
            };
            match (folders, files) {
                (0, n) => plural(n, "file"),
                (n, 0) => plural(n, "folder"),
                (a, b) => format!("{} · {}", plural(a, "folder"), plural(b, "file")),
            }
        }
        _ => String::new(),
    };
    c.text(
        LIST_X + LIST_W,
        TOP - 30.0,
        Align2::RIGHT_CENTER,
        &count,
        14.0,
        theme::text3(),
        Weight::Medium,
    );
    match &f.entries {
        Scan::Busy | Scan::Idle => {
            widgets::spinner(c, LIST_X + LIST_W / 2.0, 380.0, 22.0, now);
            return;
        }
        Scan::Failed(e) => {
            c.text(
                LIST_X + LIST_W / 2.0,
                360.0,
                Align2::CENTER_CENTER,
                e,
                18.0,
                theme::text2(),
                Weight::Medium,
            );
            return;
        }
        Scan::Done(v) if v.is_empty() => {
            c.text(
                LIST_X + LIST_W / 2.0,
                360.0,
                Align2::CENTER_CENTER,
                "Empty folder",
                18.0,
                theme::text2(),
                Weight::Medium,
            );
            c.text(
                LIST_X + LIST_W / 2.0,
                392.0,
                Align2::CENTER_CENTER,
                "Options makes a folder or pastes here",
                14.0,
                theme::text3(),
                Weight::Regular,
            );
            return;
        }
        Scan::Done(_) => {}
    }
    let list_focus = f.pane == Pane::List && f.menu.is_none();
    let index = f.index();
    let visible = ((720.0 - TOP) / (ROW_H + ROW_GAP)) as usize;
    let first = index.saturating_sub(visible.saturating_sub(1));
    let scroll = c.anim(
        ("files-scroll", f.stack.len()),
        first as f32 * (ROW_H + ROW_GAP),
    );
    let list = c.clipped(LIST_X - 8.0, TOP - 8.0, LIST_W + 16.0, 720.0 - TOP + 8.0);
    for (i, e) in rows.iter().enumerate() {
        let y = TOP + i as f32 * (ROW_H + ROW_GAP) - scroll;
        if y + ROW_H < TOP - 10.0 || y > 720.0 {
            continue;
        }
        let focused = list_focus && i == index;
        let t = c.anim(("files-t", i), if focused { 1.0 } else { 0.0 });
        let current = i == index && !list_focus;
        let rect = list.r(LIST_X, y, LIST_W, ROW_H);
        // Glass focus inside the row.
        list.fill(
            rect,
            16.0,
            theme::rgba(theme::WHITE, (6.0 + if current { 10.0 } else { 0.0 }) as u8),
        );
        widgets::focus_pill(&list, rect, 16.0, t);
        let cut_here = f
            .clipboard
            .as_ref()
            .is_some_and(|cb| cb.cut && cb.entry.path == e.path);
        let ink_a = if cut_here { 120 } else { 235 };
        draw_icon(&list, images, e.kind, LIST_X + 32.0, y + ROW_H / 2.0, 34.0);
        list.text(
            LIST_X + 64.0,
            y + ROW_H / 2.0,
            Align2::LEFT_CENTER,
            &list.fit(&e.name, LIST_W - 270.0, 17.0, Weight::Medium),
            17.0,
            theme::rgba(theme::INK, ink_a),
            if t > 0.5 {
                Weight::Semibold
            } else {
                Weight::Medium
            },
        );
        let right = if e.is_dir() {
            String::new()
        } else {
            files::human_size(e.size)
        };
        list.text(
            LIST_X + LIST_W - 56.0,
            y + ROW_H / 2.0,
            Align2::RIGHT_CENTER,
            &right,
            14.0,
            theme::text3(),
            Weight::Medium,
        );
        list.text(
            LIST_X + LIST_W - 130.0,
            y + ROW_H / 2.0,
            Align2::RIGHT_CENTER,
            e.kind.label(),
            13.0,
            theme::text3(),
            Weight::Regular,
        );
        if e.is_dir() {
            icons::draw(
                &list,
                "chevron",
                LIST_X + LIST_W - 26.0,
                y + ROW_H / 2.0,
                9.0,
                theme::text3(),
            );
        }
        if y >= TOP - 10.0 && y + ROW_H <= 720.0 {
            c.hit(c.r(LIST_X, y, LIST_W, ROW_H), Target::Files(i));
        }
    }
}

fn draw_menu(c: &C, f: &Files) {
    match &f.menu {
        None => {}
        Some(FilesMenu::Options(opt)) => {
            let opts = f.options();
            let name = f
                .focused()
                .map(|e| e.name.clone())
                .unwrap_or_else(|| f.title());
            let h = 96.0 + opts.len() as f32 * 58.0;
            let m = widgets::modal(c, 440.0, h);
            let (mx, my) = c.to_design(m.min);
            c.text(
                mx + 32.0,
                my + 44.0,
                Align2::LEFT_CENTER,
                &c.fit(&name, 376.0, 19.0, Weight::Semibold),
                19.0,
                theme::text(),
                Weight::Semibold,
            );
            for (k, o) in opts.iter().enumerate() {
                let t = c.anim(("files-opt", k), if k == *opt { 1.0 } else { 0.0 });
                let rect = c.r(mx + 28.0, my + 76.0 + k as f32 * 58.0, 384.0, 50.0);
                let style = if *o == files::FileOption::Delete {
                    ButtonStyle::Danger
                } else {
                    ButtonStyle::Secondary
                };
                widgets::button(c, rect, o.label(), style, t);
            }
        }
        Some(FilesMenu::ConfirmDelete(sel)) => {
            let name = f.focused().map(|e| e.name.as_str()).unwrap_or("");
            let folder = f.focused().is_some_and(|e| e.is_dir());
            let m = widgets::modal(c, 560.0, 220.0);
            let (mx, my) = c.to_design(m.min);
            c.text(
                mx + 280.0,
                my + 56.0,
                Align2::CENTER_CENTER,
                &c.fit(&format!("Delete {name}?"), 500.0, 22.0, Weight::Semibold),
                22.0,
                theme::text(),
                Weight::Semibold,
            );
            c.text(
                mx + 280.0,
                my + 92.0,
                Align2::CENTER_CENTER,
                if folder {
                    "The folder and everything in it will be gone for good."
                } else {
                    "It will be gone for good."
                },
                15.0,
                theme::text2(),
                Weight::Regular,
            );
            for (k, label) in ["Delete", "Keep"].iter().enumerate() {
                let t = c.anim(("files-del", k), if k == *sel { 1.0 } else { 0.0 });
                let rect = c.r(mx + 40.0 + k as f32 * 250.0, my + 134.0, 230.0, 56.0);
                let style = if k == 0 {
                    ButtonStyle::Danger
                } else {
                    ButtonStyle::Secondary
                };
                widgets::button(c, rect, label, style, t);
            }
        }
        Some(FilesMenu::Edit(edit)) => {
            let title = match &edit.kind {
                EditKind::Rename(_) => "Rename",
                EditKind::NewFolder => "New folder",
            };
            let panel = c.r(240.0, 70.0, 800.0, 590.0);
            c.scrim();
            c.glass(panel, 32.0);
            c.text(
                640.0,
                118.0,
                Align2::CENTER_CENTER,
                title,
                24.0,
                theme::text(),
                Weight::Semibold,
            );
            widgets::keyboard(
                c,
                &edit.osk,
                280.0,
                160.0,
                720.0,
                "Name",
                true,
                "files-osk",
                &|_, _| Target::Outside,
            );
        }
    }
}
