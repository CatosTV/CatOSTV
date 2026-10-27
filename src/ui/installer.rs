//! Drawing for `catostv-installer` (wizard logic: `catostv_core::installer`).
//!
//! Copy rule (2026-10-14): short labels,
//! no taglines, no marketing sentences. One line of help at most per page.

use crate::canvas::Canvas;
use crate::icons;
use crate::theme::{self, Weight};
use crate::widgets::{self, ButtonStyle, ListRow};
use catostv_core::installer::{Installer, STAGES, Scan, Step, Target};
use catostv_core::settings::RowKind;
use catostv_core::{install, keyboard, timezone};
use eframe::egui::{Align2, Rect};

type C = Canvas<Target>;

const X0: f32 = 372.0;
const MAIN_W: f32 = 836.0;

fn t(c: &C, id: impl std::hash::Hash + std::fmt::Debug, on: bool) -> f32 {
    c.anim(id, if on { 1.0 } else { 0.0 })
}

/// Draw one installer frame. `now`: monotonic seconds (spinner).
pub fn draw(c: &C, f: &Installer, now: f64) {
    c.background(None, true);
    draw_sidebar(c, f);
    let main = c.r(320.0, 32.0, 928.0, 656.0);
    c.glass(main, 28.0);
    let enter = c.entrance("installer-step", f.step, crate::motion::PAGE);
    let content = c.translated(0.0, 14.0 * (1.0 - enter)).opacity(enter);
    let c = &content;
    match f.step {
        Step::Welcome => draw_welcome(c, f, now),
        Step::Keyboard => draw_keyboard(c, f),
        Step::Network => draw_network(c, f, now),
        Step::WifiPassword => draw_password(c, f),
        Step::Disk => draw_disks(c, f, now),
        Step::Timezone => draw_timezone(c, f),
        Step::Confirm => draw_confirm(c, f),
        Step::Installing => draw_installing(c, f, now),
        Step::Done => draw_done(c, f),
        Step::Failed => draw_failed(c, f),
    }
    if let Some(n) = &f.notice {
        let ok = n.starts_with("Connected");
        c.text(
            X0,
            652.0,
            Align2::LEFT_CENTER,
            &c.fit(n, MAIN_W, 15.0, Weight::Medium),
            15.0,
            if ok {
                theme::rgb(theme::SUCCESS)
            } else {
                theme::text2()
            },
            Weight::Medium,
        );
    }
}

fn title(c: &C, heading: &str, body: &str) {
    c.text(
        X0,
        74.0,
        Align2::LEFT_TOP,
        heading,
        34.0,
        theme::text(),
        Weight::Semibold,
    );
    if !body.is_empty() {
        c.text(
            X0,
            124.0,
            Align2::LEFT_TOP,
            body,
            17.0,
            theme::text2(),
            Weight::Regular,
        );
    }
}

fn draw_sidebar(c: &C, f: &Installer) {
    let r = c.r(32.0, 32.0, 268.0, 656.0);
    c.glass(r, 28.0);
    icons::logo(c, 60.0, 62.0, 40.0);
    c.text(
        114.0,
        82.0,
        Align2::LEFT_CENTER,
        "CatosTV",
        22.0,
        theme::text(),
        Weight::Semibold,
    );
    let stage = f.step.stage();
    for (i, name) in STAGES.iter().enumerate() {
        let y = 160.0 + i as f32 * 54.0;
        let (ink, w) = if i == stage {
            (theme::text(), Weight::Semibold)
        } else if i < stage {
            (theme::text2(), Weight::Medium)
        } else {
            (theme::text3(), Weight::Medium)
        };
        if i < stage {
            c.circle(76.0, y, 11.0, theme::accent_a(60));
            icons::draw(c, "check", 76.0, y, 12.0, theme::accent());
        } else if i == stage {
            c.circle(76.0, y, 11.0, theme::accent());
            c.circle(76.0, y, 4.0, theme::on_accent());
        } else {
            c.ring(76.0, y, 10.0, 1.5, theme::border_hi());
        }
        if i + 1 < STAGES.len() {
            c.segment(
                (76.0, y + 15.0),
                (76.0, y + 39.0),
                1.5,
                if i < stage {
                    theme::accent_a(120)
                } else {
                    theme::border()
                },
            );
        }
        c.text(100.0, y, Align2::LEFT_CENTER, name, 16.0, ink, w);
    }
    if f.dry_run {
        let b = c.r(56.0, 628.0, 220.0, 36.0);
        c.fill(b, 18.0, theme::rgba(theme::WARNING, 36));
        c.text(
            166.0,
            646.0,
            Align2::CENTER_CENTER,
            "Dry run · no disk is erased",
            12.5,
            theme::rgb(theme::WARNING),
            Weight::Semibold,
        );
    }
}

fn two_buttons(
    c: &C,
    f: &Installer,
    a: (&str, ButtonStyle),
    b: (&str, ButtonStyle),
    y: f32,
    focus: usize,
) {
    for (k, (label, style)) in [a, b].into_iter().enumerate() {
        let x = X0 + k as f32 * 236.0;
        let rect = c.r(x, y, 220.0, 54.0);
        widgets::button(
            c,
            rect,
            label,
            style,
            t(c, ("btn", f.step as u8, k), focus == k),
        );
        c.hit(rect, Target::Item(k));
    }
}

fn draw_welcome(c: &C, f: &Installer, now: f64) {
    // A slow "breathing" glow behind the logo.
    let breathe = (0.5 + 0.5 * (now * 1.3).sin()) as f32;
    c.glow(
        X0 + 42.0,
        138.0,
        120.0 + 24.0 * breathe,
        theme::accent_rgb(),
        (40.0 + 40.0 * breathe) as u8,
    );
    icons::logo(c, X0, 96.0, 84.0);
    // welcome with the idea, not a time estimate.
    c.text(
        X0,
        214.0,
        Align2::LEFT_TOP,
        "Free your TV",
        50.0,
        theme::text(),
        Weight::Semibold,
    );
    c.text(
        X0 + 2.0,
        282.0,
        Align2::LEFT_TOP,
        "Welcome to the libre TV experience. Open, private and free of distractions.",
        19.0,
        theme::text2(),
        Weight::Regular,
    );
    c.text(
        X0 + 2.0,
        314.0,
        Align2::LEFT_TOP,
        "Your apps, your shows, nothing else. Setup needs an internet connection.",
        16.0,
        theme::text3(),
        Weight::Regular,
    );
    two_buttons(
        c,
        f,
        ("Let's go", ButtonStyle::Primary),
        ("Power off", ButtonStyle::Secondary),
        372.0,
        f.choice,
    );
}

#[allow(clippy::too_many_arguments)]
fn list_rows(
    c: &C,
    id: &str,
    x: f32,
    y: f32,
    w: f32,
    rows: &[ListRow],
    index: usize,
    visible: usize,
    row_h: f32,
) {
    let first = index
        .saturating_sub(visible / 2)
        .min(rows.len().saturating_sub(visible));
    for (k, row) in rows.iter().enumerate().skip(first).take(visible) {
        let rect = c.r(x, y + (k - first) as f32 * (row_h + 6.0), w, row_h);
        widgets::list_row(c, rect, row, t(c, (id, k), index == k));
        c.hit(rect, Target::Item(k));
    }
}

fn draw_keyboard(c: &C, f: &Installer) {
    title(c, "Pick your keyboard", "");
    let rows: Vec<ListRow> = keyboard::LAYOUTS
        .iter()
        .map(|(_, name, _)| ListRow::simple(name))
        .collect();
    list_rows(c, "kb", X0, 140.0, 440.0, &rows, f.kb_index, 9, 50.0);
    // Try-it field.
    let bx = X0 + 470.0;
    c.text(
        bx,
        150.0,
        Align2::LEFT_CENTER,
        "Test it here",
        15.0,
        theme::text3(),
        Weight::Medium,
    );
    let field = c.r(bx, 170.0, 366.0, 56.0);
    c.fill(field, 14.0, theme::rgba(theme::BLACK, 90));
    c.outline(field, 14.0, 1.5, theme::accent_a(180));
    let shown = c.fit(&f.kb_test, 320.0, 20.0, Weight::Medium);
    let r = c.text(
        bx + 18.0,
        198.0,
        Align2::LEFT_CENTER,
        &shown,
        20.0,
        theme::text(),
        Weight::Medium,
    );
    let cx = if f.kb_test.is_empty() {
        bx + 18.0
    } else {
        (r.max.x - c.origin.x) / c.s + 3.0
    };
    c.segment((cx, 184.0), (cx, 212.0), 2.0, theme::accent());
}

fn draw_network(c: &C, f: &Installer, now: f64) {
    title(c, "Get online", "");
    let (label, col) = if f.online {
        ("Connected", theme::SUCCESS)
    } else {
        ("Not connected", theme::WARNING)
    };
    let tw = c.text_width(label, 14.0, Weight::Semibold);
    let pill = c.r(X0, 128.0, tw + 40.0, 30.0);
    c.fill(pill, 15.0, theme::rgba(col, 36));
    c.circle(X0 + 16.0, 143.0, 4.5, theme::rgb(col));
    c.text(
        X0 + 28.0,
        143.0,
        Align2::LEFT_CENTER,
        label,
        14.0,
        theme::rgb(col),
        Weight::Semibold,
    );
    let status = match &f.wifi {
        Scan::Busy => "Searching for Wi-Fi…".to_string(),
        Scan::Failed(e) => e.clone(),
        // Online already (a cable): nothing to ask for.
        Scan::Done(v) if v.is_empty() && f.online => String::new(),
        Scan::Done(v) if v.is_empty() => "No Wi-Fi networks. Plug in a cable".into(),
        _ => String::new(),
    };
    let busy = matches!(f.wifi, Scan::Busy);
    let sx = X0 + tw + 56.0;
    if busy {
        widgets::spinner(c, sx + 8.0, 143.0, 8.0, now);
    }
    c.text(
        if busy { sx + 26.0 } else { sx },
        143.0,
        Align2::LEFT_CENTER,
        &status,
        14.0,
        theme::text3(),
        Weight::Regular,
    );
    let nets = f.wifi.items();
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
        .map(|(n, s)| ListRow {
            subtitle: s,
            icon: Some("wifi"),
            ..ListRow::simple(&n.ssid)
        })
        .collect();
    rows.push(ListRow {
        icon: Some("refresh"),
        kind: Some(RowKind::Action),
        ..ListRow::simple("Search again")
    });
    rows.push(ListRow {
        icon: Some("chevron"),
        kind: Some(RowKind::Action),
        disabled: !f.online && !f.dry_run,
        ..ListRow::simple("Continue")
    });
    list_rows(c, "net", X0, 180.0, MAIN_W, &rows, f.net_index, 7, 56.0);
}

fn draw_password(c: &C, f: &Installer) {
    title(
        c,
        &c.fit(&format!("Join {}", f.ssid), MAIN_W, 34.0, Weight::Semibold),
        "",
    );
    widgets::keyboard(
        c,
        &f.osk,
        X0,
        140.0,
        MAIN_W,
        "Wi-Fi password",
        f.connecting.is_none(),
        "inst-osk",
        &|r, col| Target::Key(r, col),
    );
}

fn draw_disks(c: &C, f: &Installer, now: f64) {
    title(
        c,
        "Where should CatosTV live?",
        "This disk will be wiped clean.",
    );
    let disks = f.disks.items();
    let status = match &f.disks {
        Scan::Busy => Some("Looking for disks…".to_string()),
        Scan::Failed(e) => Some(format!("Could not list disks: {e}")),
        Scan::Done(v) if v.is_empty() => Some("No disks found".to_string()),
        _ => None,
    };
    if let Some(s) = status {
        let busy = matches!(f.disks, Scan::Busy);
        if busy {
            widgets::spinner(c, X0 + 11.0, 186.0, 10.0, now);
        }
        c.text(
            if busy { X0 + 32.0 } else { X0 },
            176.0,
            Align2::LEFT_TOP,
            &s,
            16.0,
            theme::text3(),
            Weight::Regular,
        );
    }
    let total = disks.len() + 1;
    let visible = 5usize;
    let first = f.disk_index.saturating_sub(visible - 1);
    let mut y = 172.0;
    for k in first..total.min(first + visible + 1) {
        if y > 600.0 {
            break;
        }
        let tt = t(c, ("disk", k), f.disk_index == k);
        if k == disks.len() {
            let rect = c.r(X0, y, MAIN_W, 52.0);
            widgets::list_row(
                c,
                rect,
                &ListRow {
                    icon: Some("refresh"),
                    kind: Some(RowKind::Action),
                    ..ListRow::simple("Search again")
                },
                tt,
            );
            c.hit(rect, Target::Item(k));
            break;
        }
        let d = &disks[k];
        let base = c.r(X0, y, MAIN_W, 76.0);
        c.hit(base, Target::Item(k));
        let rect = base;
        c.fill(rect, 16.0, theme::rgba(theme::WHITE, 12));
        widgets::focus_pill(c, rect, 16.0, tt);
        let ok = d.eligible();
        icons::draw(
            c,
            "disk",
            X0 + 34.0,
            y + 38.0,
            24.0,
            if ok { theme::accent() } else { theme::text3() },
        );
        c.text(
            X0 + 64.0,
            y + 26.0,
            Align2::LEFT_CENTER,
            &c.fit(&d.title(), 740.0, 17.0, Weight::Semibold),
            17.0,
            if ok { theme::text() } else { theme::text3() },
            Weight::Semibold,
        );
        let (line, color) = match &d.blocked {
            Some(reason) => (
                format!("{} · {reason}", d.detail()),
                theme::rgb(theme::DANGER),
            ),
            None if !d.warnings.is_empty() => (
                format!("{} · {}", d.detail(), d.warnings.join(" · ")),
                theme::rgb(theme::WARNING),
            ),
            None => (d.detail(), theme::text3()),
        };
        c.text(
            X0 + 64.0,
            y + 52.0,
            Align2::LEFT_CENTER,
            &c.fit(&line, 750.0, 13.0, Weight::Regular),
            13.0,
            color,
            Weight::Regular,
        );
        y += 84.0;
    }
}

fn draw_timezone(c: &C, f: &Installer) {
    title(c, "Your time zone", "");
    let labels: Vec<String> = timezone::COMMON
        .iter()
        .map(|z| z.replace('_', " "))
        .collect();
    let rows: Vec<ListRow> = labels
        .iter()
        .map(|l| ListRow {
            icon: Some("globe"),
            ..ListRow::simple(l)
        })
        .collect();
    list_rows(c, "tz", X0, 140.0, MAIN_W, &rows, f.tz_index, 9, 50.0);
}

fn draw_confirm(c: &C, f: &Installer) {
    title(c, "Ready to install?", "");
    let card = c.r(X0, 140.0, MAIN_W, 280.0);
    c.fill(card, 22.0, theme::rgba(theme::BLACK, 90));
    c.outline(card, 22.0, 1.0, theme::rgba(theme::DANGER, 90));
    if let Some(d) = f.selected_disk() {
        icons::draw(c, "disk", X0 + 48.0, 188.0, 30.0, theme::rgb(theme::DANGER));
        c.text(
            X0 + 84.0,
            178.0,
            Align2::LEFT_CENTER,
            &c.fit(&d.title(), 720.0, 21.0, Weight::Semibold),
            21.0,
            theme::text(),
            Weight::Semibold,
        );
        c.text(
            X0 + 84.0,
            204.0,
            Align2::LEFT_CENTER,
            &d.detail(),
            14.0,
            theme::text2(),
            Weight::Regular,
        );
        let mut y = 250.0;
        for w in &d.warnings {
            c.text(
                X0 + 32.0,
                y,
                Align2::LEFT_CENTER,
                &c.fit(&format!("•  {w}"), 780.0, 14.0, Weight::Regular),
                14.0,
                theme::rgb(theme::WARNING),
                Weight::Regular,
            );
            y += 26.0;
        }
        c.text(
            X0 + 32.0,
            y + 4.0,
            Align2::LEFT_CENTER,
            "Everything on this disk will be erased.",
            15.0,
            theme::rgb(theme::DANGER),
            Weight::Semibold,
        );
        let facts = format!(
            "{}  ·  {}  ·  {}",
            f.timezone().replace('_', " "),
            keyboard::label(f.keyboard()),
            f.drivers().summary
        );
        c.text(
            X0 + 32.0,
            388.0,
            Align2::LEFT_CENTER,
            &facts,
            14.0,
            theme::text3(),
            Weight::Regular,
        );
    }
    let buttons = [
        ("Cancel", ButtonStyle::Secondary, 220.0),
        ("Erase and install", ButtonStyle::Danger, 260.0),
    ];
    let mut x = X0;
    for (k, (label, style, w)) in buttons.into_iter().enumerate() {
        let rect = c.r(x, 452.0, w, 54.0);
        widgets::button(
            c,
            rect,
            label,
            style,
            t(c, ("confirm", k), f.confirm_index == k),
        );
        c.hit(rect, Target::Item(k));
        x += w + 16.0;
    }
}

fn draw_installing(c: &C, f: &Installer, now: f64) {
    // plain, calm heading.
    title(
        c,
        "Setting your TV free",
        "Sit back. Your distraction-free TV is on its way.",
    );
    let p = &f.progress;
    widgets::spinner(c, X0 + 30.0, 246.0, 26.0, now);
    let label = if p.label.is_empty() {
        "Getting ready"
    } else {
        p.label.as_str()
    };
    c.text(
        X0 + 80.0,
        232.0,
        Align2::LEFT_CENTER,
        &c.fit(label, MAIN_W - 200.0, 22.0, Weight::Semibold),
        22.0,
        theme::text(),
        Weight::Semibold,
    );
    let detail = if p.detail.is_empty() {
        format!(
            "Step {} of {}",
            (p.index + 1).min(p.total.max(1)),
            p.total.max(1)
        )
    } else {
        p.detail.clone()
    };
    c.text(
        X0 + 80.0,
        262.0,
        Align2::LEFT_CENTER,
        &c.fit(&detail, MAIN_W - 200.0, 15.0, Weight::Regular),
        15.0,
        theme::text2(),
        Weight::Regular,
    );
    c.text(
        X0 + MAIN_W,
        246.0,
        Align2::RIGHT_CENTER,
        &format!("{}%", (p.fraction() * 100.0).round() as u32),
        28.0,
        theme::text(),
        Weight::Semibold,
    );
    widgets::progress_bar(c, X0, 312.0, MAIN_W, p.fraction(), now);
    // What is done and what is left (1.0): the last finished steps ticked,
    // then how many remain.
    let mut y = 372.0;
    let shown = p.done.len().saturating_sub(5);
    for label in &p.done[shown..] {
        icons::draw(c, "check", X0 + 12.0, y, 16.0, theme::rgb(theme::SUCCESS));
        c.text(
            X0 + 40.0,
            y,
            Align2::LEFT_CENTER,
            &c.fit(label, MAIN_W - 60.0, 16.0, Weight::Regular),
            16.0,
            theme::text2(),
            Weight::Regular,
        );
        y += 36.0;
    }
    let left = p.total.saturating_sub(p.index + 1);
    if left > 0 {
        c.text(
            X0 + 40.0,
            y,
            Align2::LEFT_CENTER,
            &format!(
                "{left} more {} after this one",
                if left == 1 { "step" } else { "steps" }
            ),
            15.0,
            theme::text3(),
            Weight::Regular,
        );
    }
    c.text(
        X0,
        596.0,
        Align2::LEFT_CENTER,
        if f.dry_run {
            "Dry run: commands are only simulated."
        } else {
            "Keep the TV on and the USB drive in until this finishes."
        },
        15.0,
        if f.dry_run {
            theme::rgb(theme::WARNING)
        } else {
            theme::text3()
        },
        Weight::Medium,
    );
}

fn draw_done(c: &C, f: &Installer) {
    title(
        c,
        "You're all set",
        "Remove the USB drive and restart to start watching.",
    );
    let card = c.r(X0, 168.0, MAIN_W, 372.0);
    c.fill(card, 22.0, theme::rgba(theme::BLACK, 70));
    c.outline(card, 22.0, 1.0, theme::border());
    widgets::key_guide(c, X0 + 32.0, 186.0, MAIN_W - 64.0, 48.0);
    two_buttons(
        c,
        f,
        ("Restart now", ButtonStyle::Primary),
        ("Power off", ButtonStyle::Secondary),
        568.0,
        f.choice,
    );
}

fn draw_failed(c: &C, f: &Installer) {
    title(c, "Something went wrong", "");
    let (msg, modified) = f.failure.clone().unwrap_or_default();
    let box_r = c.r(X0, 140.0, MAIN_W, 150.0);
    c.fill(box_r, 18.0, theme::rgba(theme::DANGER, 28));
    c.outline(box_r, 18.0, 1.0, theme::rgba(theme::DANGER, 90));
    c.paragraph(
        X0 + 24.0,
        160.0,
        MAIN_W - 48.0,
        &msg,
        16.0,
        theme::rgb(theme::DANGER),
        Weight::Medium,
    );
    let state = if modified {
        "The disk was partly written. Try again on the same or another disk."
    } else {
        "Nothing on your disks was changed."
    };
    c.text(
        X0,
        316.0,
        Align2::LEFT_TOP,
        state,
        16.0,
        theme::text(),
        Weight::Regular,
    );
    c.text(
        X0,
        346.0,
        Align2::LEFT_TOP,
        &format!("Log: {}", install::LOG_PATH),
        14.0,
        theme::text3(),
        Weight::Regular,
    );
    two_buttons(
        c,
        f,
        ("Try again", ButtonStyle::Primary),
        ("Power off", ButtonStyle::Secondary),
        400.0,
        f.choice,
    );
}

/// The topmost target under `pos`.
pub fn hit_test(hits: &[(Rect, Target)], pos: eframe::egui::Pos2) -> Option<Target> {
    hits.iter()
        .rev()
        .find(|(r, _)| r.contains(pos))
        .map(|(_, t)| *t)
}
