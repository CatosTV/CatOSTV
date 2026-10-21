//! Resolution-independent drawing surface.
//!
//! Every CatosTV screen is laid out on a fixed **1280 × 720 design canvas**
//! and scaled uniformly (letterboxed if the aspect ratio differs) to the real
//! output. A 1080p TV renders at 1.5×, a 4K TV at 3×, so spacing and text
//! stay identical at viewing distance and nothing needs per-resolution code.
//! Any resolution or aspect ratio the display reports works.
//!
//! **Picture fit** (Settings → Display) shrinks the canvas by a percentage
//! on every side for TVs that overscan.
//!
//! Only egui's `Painter` is used, no egui widgets, so egui never takes
//! keyboard focus and remote input stays fully under CatosTV's control. For
//! the mouse, every clickable rectangle is registered with [`Canvas::hit`];
//! the app hit-tests the pointer against last frame's list.

use crate::theme::{self, Rgb, Weight};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontFamily, FontId, Mesh, Painter, Pos2, Rect, Shadow,
    Shape, Stroke, StrokeKind, Vec2, pos2, vec2,
};
use std::cell::RefCell;
use std::rc::Rc;

pub const DESIGN_W: f32 = 1280.0;
pub const DESIGN_H: f32 = 720.0;

#[derive(Clone)]
pub struct Canvas<H: Copy = ()> {
    pub painter: Painter,
    pub ctx: egui::Context,
    /// Top-left of the design canvas on screen (points).
    pub origin: Pos2,
    /// Screen points per design unit.
    pub s: f32,
    pub screen: Rect,
    hits: Rc<RefCell<Vec<(Rect, H)>>>,
}

impl<H: Copy> Canvas<H> {
    /// `safe_area_pct`: picture-fit margin on each side (0–10).
    pub fn new(ui: &egui::Ui, safe_area_pct: u8) -> Self {
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        let inset = screen.size() * (safe_area_pct.min(20) as f32 / 100.0);
        let usable = Rect::from_min_max(screen.min + inset, screen.max - inset);
        let s = (usable.width() / DESIGN_W)
            .min(usable.height() / DESIGN_H)
            .max(0.1);
        let size = vec2(DESIGN_W * s, DESIGN_H * s);
        let origin = usable.center() - size / 2.0;
        Self {
            painter: ui.painter().clone(),
            ctx,
            origin,
            s,
            screen,
            hits: Rc::new(RefCell::new(Vec::new())),
        }
    }

    // ---------------- geometry ----------------

    pub fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + vec2(x * self.s, y * self.s)
    }

    pub fn r(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(self.p(x, y), vec2(w * self.s, h * self.s))
    }

    pub fn len(&self, v: f32) -> f32 {
        v * self.s
    }

    /// Screen position → design coordinates.
    pub fn to_design(&self, p: Pos2) -> (f32, f32) {
        (
            (p.x - self.origin.x) / self.s,
            (p.y - self.origin.y) / self.s,
        )
    }

    /// Design-space size of a screen rect.
    pub fn dsize(&self, r: Rect) -> (f32, f32) {
        (r.width() / self.s, r.height() / self.s)
    }

    fn cr(&self, radius: f32) -> CornerRadius {
        CornerRadius::same((radius * self.s).round().clamp(0.0, 255.0) as u8)
    }

    /// The same canvas, drawing only inside the design rect (x, y, w, h):
    /// for lists that scroll smoothly under a fixed window.
    pub fn clipped(&self, x: f32, y: f32, w: f32, h: f32) -> Self {
        let mut c = self.clone();
        c.painter = self.painter.with_clip_rect(self.r(x, y, w, h));
        c
    }

    // ---------------- pointer ----------------

    /// Register a clickable rect (later registrations are on top).
    pub fn hit(&self, rect: Rect, target: H) {
        self.hits.borrow_mut().push((rect, target));
    }

    /// The hit list of this frame.
    pub fn take_hits(&self) -> Vec<(Rect, H)> {
        std::mem::take(&mut self.hits.borrow_mut())
    }

    // ---------------- shapes (all coordinates in design units) ----------------

    pub fn fill(&self, rect: Rect, radius: f32, color: Color32) {
        self.painter.rect_filled(rect, self.cr(radius), color);
    }

    pub fn outline(&self, rect: Rect, radius: f32, width: f32, color: Color32) {
        self.painter.rect_stroke(
            rect,
            self.cr(radius),
            Stroke::new(self.len(width), color),
            StrokeKind::Inside,
        );
    }

    /// Soft drop shadow / glow under a rounded rect.
    pub fn shadow(&self, rect: Rect, radius: f32, offset_y: f32, blur: f32, color: Color32) {
        // egui feathers a shadow by offsetting the outline inwards by half
        // the blur: when that is more than the element's half-height (or
        // more than the corner radius) the inner path folds over and
        // draws thin diagonal lines out of the corners (seen on the TV at
        // 300 %). Keep the blur within the element and round the
        // corners at least as much as the feather.
        let (w, h) = self.dsize(rect);
        let blur = blur.min(w.min(h) * 0.9);
        let radius = radius.max(blur * 0.5).min(w.min(h) * 0.5);
        let shadow = Shadow {
            offset: [0, (offset_y * self.s).round().clamp(-128.0, 127.0) as i8],
            blur: (blur * self.s).round().clamp(0.0, 255.0) as u8,
            spread: 0,
            color,
        };
        self.painter.add(shadow.as_shape(rect, self.cr(radius)));
    }

    /// Frosted glass panel: soft shadow, a smoky tint that calms the light
    /// behind it, a milky layer, a brighter top half and a hairline edge
    /// that catches the light.
    pub fn glass(&self, rect: Rect, radius: f32) {
        self.shadow(rect, radius, 18.0, 52.0, theme::rgba(theme::BLACK, 80));
        self.fill(rect, radius, theme::rgba(theme::BG_TOP, 96));
        self.fill(rect, radius, theme::glass());
        self.frost_edge(rect, radius, 1.0);
    }

    /// The light-catching edge of frosted glass: a soft lighter band in the
    /// upper half and a hairline all round.
    pub fn frost_edge(&self, rect: Rect, radius: f32, k: f32) {
        let a = |v: f32| (v * k).clamp(0.0, 255.0) as u8;
        self.rounded_gradient(rect, radius, theme::WHITE, a(16.0), 0);
        self.outline(rect, radius, 1.0, theme::rgba(theme::WHITE, a(26.0)));
    }

    /// A darker, more opaque glass for overlays that must read over content.
    pub fn sheet(&self, rect: Rect, radius: f32) {
        self.shadow(rect, radius, 24.0, 64.0, theme::rgba(theme::BLACK, 150));
        self.fill(rect, radius, theme::rgba(theme::PANEL, 248));
        self.outline(rect, radius, 1.0, theme::border_hi());
    }

    pub fn circle(&self, x: f32, y: f32, radius: f32, color: Color32) {
        self.painter
            .circle_filled(self.p(x, y), self.len(radius), color);
    }

    pub fn ring(&self, x: f32, y: f32, radius: f32, width: f32, color: Color32) {
        self.painter.circle_stroke(
            self.p(x, y),
            self.len(radius),
            Stroke::new(self.len(width), color),
        );
    }

    /// Open polyline through design-space points.
    pub fn polyline(&self, pts: &[(f32, f32)], width: f32, color: Color32) {
        let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.painter
            .add(Shape::line(pts, Stroke::new(self.len(width), color)));
    }

    pub fn closed(&self, pts: &[(f32, f32)], width: f32, color: Color32) {
        let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.painter
            .add(Shape::closed_line(pts, Stroke::new(self.len(width), color)));
    }

    /// Filled convex polygon.
    pub fn polygon(&self, pts: &[(f32, f32)], color: Color32) {
        let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| self.p(x, y)).collect();
        self.painter
            .add(Shape::convex_polygon(pts, color, Stroke::NONE));
    }

    pub fn segment(&self, a: (f32, f32), b: (f32, f32), width: f32, color: Color32) {
        self.painter.line_segment(
            [self.p(a.0, a.1), self.p(b.0, b.1)],
            Stroke::new(self.len(width), color),
        );
    }

    /// Soft radial glow (vertex-coloured triangle fan; no textures).
    /// `radius` in design units, `alpha` at the centre. A Gaussian falloff
    /// in many fine rings, so the light has no visible edge or banding
    /// (the old five-ring glow looked "toy-like").
    pub fn glow(&self, x: f32, y: f32, radius: f32, color: Rgb, alpha: u8) {
        self.glow_ellipse(x, y, radius, radius, 0.0, color, alpha);
    }

    /// [`Self::glow`] stretched to an ellipse (`rx` × `ry`) and turned by
    /// `angle` radians: long soft bands of light for the backdrop.
    #[allow(clippy::too_many_arguments)]
    pub fn glow_ellipse(
        &self,
        x: f32,
        y: f32,
        rx: f32,
        ry: f32,
        angle: f32,
        color: Rgb,
        alpha: u8,
    ) {
        let c = self.p(x, y);
        let (rx, ry) = (self.len(rx), self.len(ry));
        let (sa, ca) = angle.sin_cos();
        let mut mesh = Mesh::default();
        const RINGS: u32 = 16;
        const SEG: u32 = 72;
        // exp(-k f²), shifted so it reaches exactly 0 at the edge.
        let k = 3.2_f32;
        let tail = (-k).exp();
        let falloff = |f: f32| (((-k * f * f).exp() - tail) / (1.0 - tail)).max(0.0);
        mesh.colored_vertex(c, theme::rgba(color, alpha));
        for ring in 1..=RINGS {
            let f = ring as f32 / RINGS as f32;
            let col = theme::rgba(color, (alpha as f32 * falloff(f)).round() as u8);
            for i in 0..SEG {
                let t = i as f32 / SEG as f32 * std::f32::consts::TAU;
                let (ex, ey) = (t.cos() * rx * f, t.sin() * ry * f);
                mesh.colored_vertex(c + vec2(ex * ca - ey * sa, ex * sa + ey * ca), col);
            }
        }
        for i in 0..SEG {
            mesh.add_triangle(0, 1 + i, 1 + (i + 1) % SEG);
        }
        for ring in 0..RINGS - 1 {
            let a0 = 1 + ring * SEG;
            let b0 = a0 + SEG;
            for i in 0..SEG {
                let j = (i + 1) % SEG;
                mesh.add_triangle(a0 + i, b0 + i, b0 + j);
                mesh.add_triangle(a0 + i, b0 + j, a0 + j);
            }
        }
        self.painter.add(Shape::mesh(mesh));
    }

    /// Vertical gradient over a (square-cornered) screen rect.
    pub fn vgradient(&self, rect: Rect, top: Color32, bottom: Color32) {
        let mut mesh = Mesh::default();
        mesh.colored_vertex(rect.min, top);
        mesh.colored_vertex(pos2(rect.max.x, rect.min.y), top);
        mesh.colored_vertex(pos2(rect.min.x, rect.max.y), bottom);
        mesh.colored_vertex(rect.max, bottom);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(2, 1, 3);
        self.painter.add(Shape::mesh(mesh));
    }

    /// Horizontal gradient over a (square-cornered) screen rect.
    pub fn hgradient(&self, rect: Rect, left: Color32, right: Color32) {
        let mut mesh = Mesh::default();
        mesh.colored_vertex(rect.min, left);
        mesh.colored_vertex(pos2(rect.max.x, rect.min.y), right);
        mesh.colored_vertex(pos2(rect.min.x, rect.max.y), left);
        mesh.colored_vertex(rect.max, right);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(2, 1, 3);
        self.painter.add(Shape::mesh(mesh));
    }

    /// A value remembered between frames (small state for presentation
    /// only: the previous backdrop, the tab the pill came from).
    pub fn remember<T: Clone + Send + Sync + 'static>(
        &self,
        id: impl std::hash::Hash + std::fmt::Debug,
        update: impl FnOnce(Option<T>) -> T,
    ) -> T {
        let id = egui::Id::new(("remember", id));
        self.ctx.data_mut(|d| {
            let v = update(d.get_temp::<T>(id));
            d.insert_temp(id, v.clone());
            v
        })
    }

    // ---------------- text ----------------

    pub fn font(&self, size: f32, weight: Weight) -> FontId {
        let family = match weight {
            Weight::Regular => FontFamily::Proportional,
            Weight::Medium => FontFamily::Name(theme::MEDIUM.into()),
            Weight::Semibold => FontFamily::Name(theme::SEMIBOLD.into()),
        };
        FontId::new(size * self.s, family)
    }

    /// Single-line text anchored at a design point. Returns its screen rect.
    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &self,
        x: f32,
        y: f32,
        anchor: Align2,
        text: &str,
        size: f32,
        color: Color32,
        weight: Weight,
    ) -> Rect {
        self.painter
            .text(self.p(x, y), anchor, text, self.font(size, weight), color)
    }

    /// Width of a single line of text, in design units.
    pub fn text_width(&self, text: &str, size: f32, weight: Weight) -> f32 {
        let g =
            self.painter
                .layout_no_wrap(text.to_owned(), self.font(size, weight), Color32::WHITE);
        g.size().x / self.s
    }

    /// Shorten `text` with an ellipsis so it fits in `max_w` design units.
    pub fn fit(&self, text: &str, max_w: f32, size: f32, weight: Weight) -> String {
        if self.text_width(text, size, weight) <= max_w {
            return text.to_owned();
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let candidate: String = chars.iter().collect::<String>().trim_end().to_string() + "…";
            if self.text_width(&candidate, size, weight) <= max_w {
                return candidate;
            }
        }
        "…".to_owned()
    }

    /// Wrapped paragraph starting at (x, y) with width `w`. Returns the height
    /// used, in design units.
    #[allow(clippy::too_many_arguments)]
    pub fn paragraph(
        &self,
        x: f32,
        y: f32,
        w: f32,
        text: &str,
        size: f32,
        color: Color32,
        weight: Weight,
    ) -> f32 {
        let g = self
            .painter
            .layout(text.to_owned(), self.font(size, weight), color, w * self.s);
        let h = g.size().y;
        self.painter.galley(self.p(x, y), g, color);
        h / self.s
    }

    // ---------------- animation ----------------

    /// 0 → 1 over `secs` from the first frame `id` is drawn (again): for
    /// things that should fade or slide in when they appear (a newly
    /// focused app's name, a page's tiles). Unlike [`Self::anim`], whose
    /// first value is already the target.
    pub fn appear(&self, id: impl std::hash::Hash + std::fmt::Debug, secs: f32) -> f32 {
        let id = egui::Id::new(("appear", id));
        let now = self.ctx.input(|i| i.time);
        let frame = self.ctx.cumulative_frame_nr();
        let start = self.ctx.data_mut(|d| {
            let e = d.get_temp_mut_or_insert_with(id, || (now, frame));
            // Not drawn last frame: it is appearing again.
            if e.1 + 1 < frame {
                e.0 = now;
            }
            e.1 = frame;
            e.0
        });
        let t = ((now - start) as f32 / secs.max(0.001)).clamp(0.0, 1.0);
        if t < 1.0 {
            self.ctx.request_repaint();
        }
        t
    }

    /// Animate towards `target` with a short ease-out (0.18 s), keyed by
    /// `id` (focus, the gliding highlight and scrolling all move
    /// this way, quick start, soft landing, no overshoot).
    pub fn anim(&self, id: impl std::hash::Hash + std::fmt::Debug, target: f32) -> f32 {
        self.anim_ease(id, target, 0.18)
    }

    /// [`Self::anim`] over `secs`: cubic ease-out from wherever the value
    /// is when the target changes.
    pub fn anim_ease(
        &self,
        id: impl std::hash::Hash + std::fmt::Debug,
        target: f32,
        secs: f32,
    ) -> f32 {
        let id = egui::Id::new(("ease", id));
        let now = self.ctx.input(|i| i.time);
        let secs = secs.max(0.001) as f64;
        let value = self.ctx.data_mut(|d| {
            let e: &mut (f32, f32, f64) =
                d.get_temp_mut_or_insert_with(id, || (target, target, now - secs));
            let at = |e: &(f32, f32, f64)| {
                let t = ((now - e.2) / secs).clamp(0.0, 1.0) as f32;
                e.0 + (e.1 - e.0) * ease_out(t)
            };
            if (target - e.1).abs() > 1e-4 {
                *e = (at(e), target, now);
            }
            (at(e), now - e.2 < secs)
        });
        if value.1 {
            self.ctx.request_repaint();
        }
        value.0
    }

    /// A damped spring towards `target`, keyed by `id`: `(value, velocity)`.
    /// `response` is roughly how long it takes to get there (seconds);
    /// `damping` 1.0 lands without overshoot, lower values overshoot a
    /// little and settle, which reads as fluid. The first value is the
    /// target itself.
    pub fn spring(
        &self,
        id: impl std::hash::Hash + std::fmt::Debug,
        target: f32,
        response: f32,
        damping: f32,
    ) -> (f32, f32) {
        let id = egui::Id::new(("spring", id));
        let now = self.ctx.input(|i| i.time);
        let k = (std::f32::consts::TAU / response.max(0.05)).powi(2);
        let c = 2.0 * damping * k.sqrt();
        let (x, v, moving) = self.ctx.data_mut(|d| {
            let e: &mut (f32, f32, f64) = d.get_temp_mut_or_insert_with(id, || (target, 0.0, now));
            // After a pause (it was resting, nothing was drawn) it moves
            // on from where it rested, one frame at a time.
            let gap = now - e.2;
            let mut dt = if gap > 0.1 { 1.0 / 60.0 } else { gap as f32 };
            e.2 = now;
            while dt > 0.0 {
                let h = dt.min(1.0 / 240.0);
                e.1 += (-k * (e.0 - target) - c * e.1) * h;
                e.0 += e.1 * h;
                dt -= h;
            }
            let scale = target.abs().max(1.0);
            let moving = (e.0 - target).abs() > 1e-3 * scale || e.1.abs() > 1e-2 * scale;
            if !moving {
                (e.0, e.1) = (target, 0.0);
            }
            (e.0, e.1, moving)
        });
        if moving {
            self.ctx.request_repaint();
        }
        (x, v)
    }

    /// The shared backdrop: a deep, nearly black base lit by a slow
    /// "nebula" of soft colour bands, violet, a warm amber swirl, deep blue
    /// and a hint of teal, like frosted glass held in front of coloured
    /// light (reference: a glass-morphism wallpaper).
    /// The accent colour leads the palette; `tint` (the focused app) adds a
    /// glow of its own. `glow = false` (Settings → Customization) keeps just
    /// the gradient.
    pub fn background(&self, tint: Option<Rgb>, glow: bool) {
        self.background_at(tint, glow, 0.0);
    }

    /// [`Self::background`] at time `t` (seconds): the light bands drift
    /// and turn slowly so Home is never a still picture.
    /// Periods are long and unrelated, so it never visibly loops.
    pub fn background_at(&self, tint: Option<Rgb>, glow: bool, t: f64) {
        self.vgradient(
            self.screen,
            theme::rgb(theme::BG_TOP),
            theme::rgb(theme::BG_BOTTOM),
        );
        if !glow {
            return;
        }
        let accent = theme::accent_rgb();
        let mix = |a: Rgb, b: Rgb, k: f32| {
            let m = |x: u8, y: u8| (x as f32 * k + y as f32 * (1.0 - k)) as u8;
            (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
        };
        let violet = mix(accent, (118, 82, 230), 0.55);
        let amber = (232, 116, 52);
        let blue = (38, 86, 196);
        let teal = (32, 150, 160);
        use std::f32::consts::PI;
        // Slow drift: each band wanders on its own sine (seconds → offset).
        let w = |period: f64, amp: f32, phase: f64| {
            ((t / period * std::f64::consts::TAU + phase).sin() as f32) * amp
        };
        // Broad light first, bands on top: a swirl from top-left (violet)
        // through the centre (amber) to bottom-right (blue).
        let bands: [(f32, f32, f32, f32, f32, Rgb, u8); 5] = [
            (
                150.0 + w(41.0, 60.0, 0.0),
                40.0 + w(53.0, 40.0, 1.0),
                860.0,
                540.0,
                -0.35 + w(67.0, 0.12, 2.0),
                violet,
                120,
            ),
            (
                1180.0 + w(47.0, 70.0, 3.0),
                690.0 + w(37.0, 40.0, 0.5),
                780.0,
                500.0,
                0.4,
                blue,
                105,
            ),
            (
                700.0 + w(29.0, 90.0, 1.5),
                330.0 + w(43.0, 50.0, 2.5),
                600.0,
                170.0,
                -PI / 5.0 + w(31.0, 0.25, 0.7),
                amber,
                82,
            ),
            (
                520.0 + w(37.0, 70.0, 4.0),
                480.0 + w(59.0, 40.0, 0.2),
                460.0,
                120.0,
                PI / 7.0 + w(23.0, 0.3, 1.1),
                mix(amber, violet, 0.5),
                58,
            ),
            (
                1130.0 + w(61.0, 50.0, 2.2),
                120.0 + w(33.0, 40.0, 3.3),
                420.0,
                280.0,
                0.6,
                teal,
                46,
            ),
        ];
        for (x, y, rx, ry, angle, col, alpha) in bands {
            self.glow_ellipse(x, y, rx, ry, angle, col, alpha);
        }
        if let Some(t) = tint {
            self.glow(1000.0, 220.0, 640.0, mix(t, theme::BG_TOP, 0.85), 40);
        }
        // Frost: a veil that softens the colour into a glassy haze, and a
        // faint sheen along the top edge.
        self.fill(self.screen, 0.0, theme::rgba(theme::BG_TOP, 50));
        let sheen = self.r(0.0, 0.0, 1280.0, 240.0);
        self.vgradient(
            sheen,
            theme::rgba(theme::WHITE, 8),
            theme::rgba(theme::WHITE, 0),
        );
    }

    /// Dim everything behind a modal overlay.
    pub fn scrim(&self) {
        self.painter.rect_filled(self.screen, 0.0, theme::scrim());
    }

    /// A rounded rect filled with a top-to-bottom gradient (sheens on glass).
    pub fn rounded_gradient(&self, rect: Rect, radius: f32, color: Rgb, top: u8, bottom: u8) {
        use std::f32::consts::FRAC_PI_2;
        let r = (radius * self.s)
            .min(rect.width() / 2.0)
            .min(rect.height() / 2.0);
        let corners = [
            (pos2(rect.max.x - r, rect.min.y + r), -FRAC_PI_2),
            (pos2(rect.max.x - r, rect.max.y - r), 0.0),
            (pos2(rect.min.x + r, rect.max.y - r), FRAC_PI_2),
            (pos2(rect.min.x + r, rect.min.y + r), 2.0 * FRAC_PI_2),
        ];
        let col = |y: f32| {
            let t = ((y - rect.min.y) / rect.height().max(1.0)).clamp(0.0, 1.0);
            theme::rgba(
                color,
                (top as f32 + (bottom as f32 - top as f32) * t).round() as u8,
            )
        };
        let mut mesh = Mesh::default();
        let c = rect.center();
        mesh.colored_vertex(c, col(c.y));
        let steps = 8;
        for (o, a0) in corners {
            for k in 0..=steps {
                let a = a0 + FRAC_PI_2 * k as f32 / steps as f32;
                let p = pos2(o.x + r * a.cos(), o.y + r * a.sin());
                mesh.colored_vertex(p, col(p.y));
            }
        }
        let n = mesh.vertices.len() as u32 - 1;
        for i in 1..=n {
            mesh.add_triangle(0, i, if i == n { 1 } else { i + 1 });
        }
        self.painter.add(Shape::mesh(mesh));
    }

    /// Draw an image with rounded corners (thumbnails, artwork): a textured
    /// triangle fan over the rounded outline, UVs from position.
    pub fn image_rounded(&self, rect: Rect, radius: f32, tex: egui::TextureId, tint: Color32) {
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        self.image_rounded_uv(rect, radius, tex, full, tint);
    }

    /// [`Canvas::image_rounded`] showing only the `crop` part of the
    /// picture (0..1 on both axes).
    pub fn image_rounded_uv(
        &self,
        rect: Rect,
        radius: f32,
        tex: egui::TextureId,
        crop: Rect,
        tint: Color32,
    ) {
        use std::f32::consts::FRAC_PI_2;
        let r = (radius * self.s)
            .min(rect.width() / 2.0)
            .min(rect.height() / 2.0);
        let corners = [
            (pos2(rect.max.x - r, rect.min.y + r), -FRAC_PI_2),
            (pos2(rect.max.x - r, rect.max.y - r), 0.0),
            (pos2(rect.min.x + r, rect.max.y - r), FRAC_PI_2),
            (pos2(rect.min.x + r, rect.min.y + r), 2.0 * FRAC_PI_2),
        ];
        let uv = |p: Pos2| {
            pos2(
                crop.min.x + crop.width() * (p.x - rect.min.x) / rect.width().max(1.0),
                crop.min.y + crop.height() * (p.y - rect.min.y) / rect.height().max(1.0),
            )
        };
        let mut mesh = Mesh::with_texture(tex);
        let c = rect.center();
        mesh.vertices.push(egui::epaint::Vertex {
            pos: c,
            uv: uv(c),
            color: tint,
        });
        let steps = 8;
        for (o, a0) in corners {
            for k in 0..=steps {
                let a = a0 + FRAC_PI_2 * k as f32 / steps as f32;
                let p = pos2(o.x + r * a.cos(), o.y + r * a.sin());
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: p,
                    uv: uv(p),
                    color: tint,
                });
            }
        }
        let n = mesh.vertices.len() as u32 - 1;
        for i in 1..=n {
            mesh.add_triangle(0, i, if i == n { 1 } else { i + 1 });
        }
        self.painter.add(mesh);
    }

    /// A soft drop shadow with a real Gaussian falloff: a rounded
    /// rect blurred once per (corner radius, blur) on the CPU, kept as a
    /// texture, and drawn as a nine-slice, corners as they are, edges and
    /// middle stretched. No stacked rings, no banding (the alpha is
    /// dithered when it is quantised). `sigma` and `offset_y` in design
    /// units; `color` carries the shadow's colour and strength.
    pub fn soft_shadow(&self, rect: Rect, radius: f32, sigma: f32, offset_y: f32, color: Color32) {
        if color.a() == 0 {
            return;
        }
        let r_px = (radius * self.s).round().clamp(0.0, 160.0) as u32;
        let sigma_px = (sigma * self.s).round().clamp(1.0, 72.0) as u32;
        let (tex, corner, size) = shadow_texture(&self.ctx, r_px, sigma_px);
        let pad = (3 * sigma_px) as f32;
        let outer = rect.translate(vec2(0.0, offset_y * self.s)).expand(pad);
        let c = corner as f32;
        let t = size as f32;
        // Corners keep their size; a rect too small for them shares it.
        let cx = c.min(outer.width() / 2.0);
        let cy = c.min(outer.height() / 2.0);
        let xs = [outer.min.x, outer.min.x + cx, outer.max.x - cx, outer.max.x];
        let ys = [outer.min.y, outer.min.y + cy, outer.max.y - cy, outer.max.y];
        let us = [0.0, cx / t, 1.0 - cx / t, 1.0];
        let vs = [0.0, cy / t, 1.0 - cy / t, 1.0];
        let mut mesh = Mesh::with_texture(tex);
        for j in 0..4 {
            for i in 0..4 {
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: pos2(xs[i], ys[j]),
                    uv: pos2(us[i], vs[j]),
                    color,
                });
            }
        }
        for j in 0..3u32 {
            for i in 0..3u32 {
                let a = j * 4 + i;
                mesh.add_triangle(a, a + 1, a + 5);
                mesh.add_triangle(a, a + 5, a + 4);
            }
        }
        self.painter.add(Shape::mesh(mesh));
    }

    /// A hairline of light along the top edge of a rounded rect, fading out
    /// down the corners (the 1-px inner highlight of focused glass).
    pub fn top_highlight(&self, rect: Rect, radius: f32, color: Color32) {
        use std::f32::consts::FRAC_PI_2;
        if color.a() == 0 {
            return;
        }
        let w = (self.s * 0.45).max(1.0);
        let r = (radius * self.s)
            .min(rect.width() / 2.0)
            .min(rect.height() / 2.0)
            .max(w * 2.0);
        // Along the inside of the outline: left arc, top edge, right arc.
        let inset = |p: Pos2, n: Vec2, d: f32| p - n * d;
        let mut pts: Vec<(Pos2, Vec2)> = Vec::new();
        let steps = 16;
        let left = pos2(rect.min.x + r, rect.min.y + r);
        for k in 0..=steps {
            let a = 2.0 * FRAC_PI_2 + FRAC_PI_2 * k as f32 / steps as f32;
            let n = vec2(a.cos(), a.sin());
            pts.push((left + n * r, n));
        }
        let right = pos2(rect.max.x - r, rect.min.y + r);
        for k in 0..=steps {
            let a = 3.0 * FRAC_PI_2 + FRAC_PI_2 * k as f32 / steps as f32;
            let n = vec2(a.cos(), a.sin());
            pts.push((right + n * r, n));
        }
        let fade = |p: Pos2| {
            let k = (1.0 - (p.y - rect.min.y) / r).clamp(0.0, 1.0);
            Color32::from_rgba_unmultiplied(
                color.r(),
                color.g(),
                color.b(),
                (color.a() as f32 * k * k) as u8,
            )
        };
        let mut mesh = Mesh::default();
        for (p, n) in &pts {
            let outer = inset(*p, *n, 0.0);
            let inner = inset(*p, *n, w);
            mesh.colored_vertex(outer, fade(outer));
            mesh.colored_vertex(inner, fade(outer));
        }
        for i in 0..(pts.len() as u32 - 1) {
            let a = i * 2;
            mesh.add_triangle(a, a + 1, a + 3);
            mesh.add_triangle(a, a + 3, a + 2);
        }
        self.painter.add(Shape::mesh(mesh));
    }

    /// Draw an image (texture) into a design-space rect.
    pub fn image(&self, rect: Rect, tex: egui::TextureId, tint: Color32) {
        self.painter.image(
            tex,
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            tint,
        );
    }
}

/// Cubic ease-out: fast start, soft landing, never past 1.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// The blurred rounded-rect texture behind [`Canvas::soft_shadow`]: white
/// with a Gaussian alpha (three box blurs ≈ a Gaussian of `sigma_px`),
/// kept in egui's memory per (radius, sigma). Returns the texture, the
/// corner size of its nine-slice and its side length, in pixels.
fn shadow_texture(ctx: &egui::Context, r_px: u32, sigma_px: u32) -> (egui::TextureId, u32, u32) {
    let key = egui::Id::new(("catostv-soft-shadow", r_px, sigma_px));
    let pad = 3 * sigma_px;
    let corner = pad + r_px + 2;
    let size = 2 * corner + 1;
    if let Some(h) = ctx.data_mut(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return (h.id(), corner, size);
    }
    let n = size as usize;
    let mut a = vec![0f32; n * n];
    // The shape, anti-aliased: a rounded rect inset by the blur margin.
    let (lo, hi) = (pad as f32, (size - pad) as f32);
    let r = r_px as f32;
    for y in 0..n {
        for x in 0..n {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = px.clamp(lo + r, hi - r);
            let cy = py.clamp(lo + r, hi - r);
            let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - r;
            let inside_box = px >= lo && px <= hi && py >= lo && py <= hi;
            a[y * n + x] = if inside_box {
                (0.5 - d).clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
    }
    // Three box blurs, each way, approximate a Gaussian of sigma.
    let box_r =
        ((((12.0 * (sigma_px as f32).powi(2) / 3.0) + 1.0).sqrt() - 1.0) / 2.0).round() as usize;
    let mut tmp = vec![0f32; n * n];
    for _ in 0..3 {
        box_blur(&a, &mut tmp, n, box_r, true);
        box_blur(&tmp, &mut a, n, box_r, false);
    }
    // Quantise with a little dither so the 8-bit falloff shows no steps.
    let mut rgba = Vec::with_capacity(n * n * 4);
    for (i, v) in a.iter().enumerate() {
        let noise = ((i as u32).wrapping_mul(2_654_435_761) >> 24) as f32 / 255.0 - 0.5;
        let alpha = (v * 255.0 + noise).round().clamp(0.0, 255.0) as u8;
        rgba.extend_from_slice(&[255, 255, 255, alpha]);
    }
    let image = egui::ColorImage::from_rgba_unmultiplied([n, n], &rgba);
    let handle = ctx.load_texture(
        format!("soft-shadow-{r_px}-{sigma_px}"),
        image,
        egui::TextureOptions::LINEAR,
    );
    let id = handle.id();
    ctx.data_mut(|d| d.insert_temp(key, handle));
    (id, corner, size)
}

/// One box-blur pass of radius `r` over an `n`×`n` buffer, along rows
/// (`horizontal`) or columns, edges clamped to zero.
fn box_blur(src: &[f32], dst: &mut [f32], n: usize, r: usize, horizontal: bool) {
    let at = |line: usize, k: usize| {
        if horizontal {
            line * n + k
        } else {
            k * n + line
        }
    };
    let w = (2 * r + 1) as f32;
    for line in 0..n {
        let mut acc = 0.0;
        for k in 0..=r.min(n - 1) {
            acc += src[at(line, k)];
        }
        for k in 0..n {
            dst[at(line, k)] = acc / w;
            if k + r + 1 < n {
                acc += src[at(line, k + r + 1)];
            }
            if k >= r {
                acc -= src[at(line, k - r)];
            }
        }
    }
}

/// Convenience for `Vec2` in design units (used by layout code).
pub fn dv(x: f32, y: f32) -> Vec2 {
    vec2(x, y)
}
