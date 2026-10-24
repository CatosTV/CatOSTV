//! The screensaver: a full-screen slideshow with the time, the date and
//! the weather in the bottom corners.

use crate::canvas::Canvas;
use crate::icons;
use crate::saver::{Slide, Slideshow};
use crate::theme::{self, Weight};
use catostv_core::screensaver::{self as ss, Weather};
use eframe::egui::{Align2, Color32, Rect, pos2, vec2};

pub struct SaverView<'a> {
    pub show: &'a Slideshow,
    pub weather: Option<&'a Weather>,
    pub time: &'a str,
    pub date: &'a str,
    pub night: bool,
    pub now: f64,
    /// A completely black screen: no pictures, no clock (the display
    /// switches off a moment later).
    pub blank: bool,
}

pub fn draw<H: Copy>(c: &Canvas<H>, v: &SaverView) {
    let fade_in = (v.show.started.elapsed().as_secs_f32() / 0.8).min(1.0);
    c.fill(
        c.screen,
        0.0,
        theme::rgba(theme::BLACK, (255.0 * fade_in) as u8),
    );
    if v.blank {
        return;
    }
    if v.show.cur.is_none() {
        c.background_at(None, true, v.now);
    }
    if let Some(p) = &v.show.prev {
        slide(c, p, 1.0);
    }
    if let Some(s) = &v.show.cur {
        slide(
            c,
            s,
            if v.show.prev.is_some() {
                v.show.fade()
            } else {
                fade_in.min(v.show.fade())
            },
        );
    }

    // Shade the bottom so the text reads on any picture.
    let top = c.p(0.0, 430.0).y;
    c.vgradient(
        Rect::from_min_max(pos2(c.screen.min.x, top), c.screen.max),
        theme::rgba(theme::BLACK, 0),
        theme::rgba(theme::BLACK, 170),
    );

    let white = theme::rgba(theme::WHITE, (255.0 * fade_in) as u8);
    let soft = theme::rgba(theme::WHITE, (200.0 * fade_in) as u8);
    c.text(
        64.0,
        640.0,
        Align2::LEFT_BOTTOM,
        v.time,
        104.0,
        white,
        Weight::Semibold,
    );
    c.text(
        70.0,
        666.0,
        Align2::LEFT_CENTER,
        v.date,
        21.0,
        soft,
        Weight::Medium,
    );

    if let Some(w) = v.weather {
        let temp = w.temp_label();
        let tw = c.text_width(&temp, 56.0, Weight::Semibold);
        let right = 1216.0;
        c.text(
            right,
            622.0,
            Align2::RIGHT_BOTTOM,
            &temp,
            56.0,
            white,
            Weight::Semibold,
        );
        icons::draw(
            c,
            ss::weather_icon(w.code, v.night),
            right - tw - 34.0,
            594.0,
            38.0,
            white,
        );
        let line = match (w.desc.is_empty(), w.place.is_empty()) {
            (false, false) => format!("{} · {}", w.desc, w.place),
            (false, true) => w.desc.clone(),
            _ => w.place.clone(),
        };
        c.text(
            right,
            666.0,
            Align2::RIGHT_CENTER,
            &c.fit(&line, 520.0, 19.0, Weight::Medium),
            19.0,
            soft,
            Weight::Medium,
        );
    }
}

/// One picture, slowly zooming in (and drifting the other way every
/// other picture), at opacity `alpha`.
fn slide<H: Copy>(c: &Canvas<H>, s: &Slide, alpha: f32) {
    slide_for(c, s, alpha, ss::PICTURE_SECS);
}

/// One picture of a slideshow showing `secs` each: a slow zoom and drift,
/// the other way for every other picture.
pub fn slide_for<H: Copy>(c: &Canvas<H>, s: &Slide, alpha: f32, secs: f32) {
    let t = (s.since.elapsed().as_secs_f32() / (secs + ss::FADE_SECS)).min(1.0);
    let zoom = 1.0 + 0.07 * t;
    let drift = if s.n.is_multiple_of(2) { 1.0 } else { -1.0 };
    let size = c.screen.size() * zoom;
    let extra = size - c.screen.size();
    let centre = c.screen.center() + vec2(extra.x * 0.3 * drift * (t - 0.5), 0.0);
    let rect = Rect::from_center_size(centre, size);
    let a = (alpha.clamp(0.0, 1.0) * 255.0) as u8;
    c.image(rect, s.tex.id(), Color32::from_white_alpha(a));
}
