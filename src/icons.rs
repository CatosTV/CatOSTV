//! Vector icons drawn with the painter.
//!
//! Text glyphs such as ⌁ ᛒ ⏻ are not in egui's default fonts and render as
//! empty boxes, so CatosTV draws its icons as shapes. Every icon fits a square
//! of `size` design units centred on (cx, cy).
//!
//! Icon names used in config/settings: wifi, bluetooth, speaker, muted,
//! volume-down, volume-up, gear, power, moon, search, home, grid, display,
//! play, media, game, music, stream, browser, tv, cat, catfyn, lock, chevron, check,
//! refresh, disk, globe, spark, store, android, cast, keyboard, palette,
//! phone, remote, download, trash, usb, plus, close, back, headphones,
//! sun, cloud, rain, snow, storm, fog, clock, timer, info, chip, hdr,
//! sync, image, key, terminal, pin, restart.
//! Anything else is drawn as a monogram.

use crate::canvas::Canvas;
use crate::theme::{self, Weight};
use eframe::egui::{Align2, Color32};
use std::f32::consts::PI;

fn arc(cx: f32, cy: f32, r: f32, from: f32, to: f32, steps: usize) -> Vec<(f32, f32)> {
    (0..=steps)
        .map(|i| {
            let t = from + (to - from) * i as f32 / steps as f32;
            (cx + r * t.cos(), cy + r * t.sin())
        })
        .collect()
}

pub fn draw<H: Copy>(c: &Canvas<H>, name: &str, cx: f32, cy: f32, size: f32, color: Color32) {
    let u = size / 2.0; // half extent
    let w = (size * 0.09).max(1.5); // stroke width
    let at = |x: f32, y: f32| (cx + x * u, cy + y * u);
    match name {
        "wifi" => {
            for r in [0.35_f32, 0.65, 0.95] {
                c.polyline(
                    &arc(cx, cy + 0.55 * u, r * u, -PI * 0.75, -PI * 0.25, 12),
                    w,
                    color,
                );
            }
            c.circle(cx, cy + 0.55 * u, w * 0.9, color);
        }
        "bluetooth" => c.polyline(
            &[
                at(-0.45, -0.4),
                at(0.4, 0.4),
                at(0.0, 0.85),
                at(0.0, -0.85),
                at(0.4, -0.4),
                at(-0.45, 0.4),
            ],
            w,
            color,
        ),
        "speaker" | "muted" | "volume-down" | "volume-up" => {
            c.polygon(
                &[
                    at(-0.8, -0.3),
                    at(-0.4, -0.3),
                    at(0.05, -0.75),
                    at(0.05, 0.75),
                    at(-0.4, 0.3),
                    at(-0.8, 0.3),
                ],
                color,
            );
            match name {
                "muted" => {
                    c.segment(at(0.35, -0.3), at(0.9, 0.3), w, color);
                    c.segment(at(0.35, 0.3), at(0.9, -0.3), w, color);
                }
                "volume-down" => c.segment(at(0.35, 0.0), at(0.9, 0.0), w, color),
                "volume-up" => {
                    c.segment(at(0.35, 0.0), at(0.95, 0.0), w, color);
                    c.segment(at(0.65, -0.3), at(0.65, 0.3), w, color);
                }
                _ => {
                    c.polyline(
                        &arc(cx + 0.05 * u, cy, 0.45 * u, -PI / 4.0, PI / 4.0, 8),
                        w,
                        color,
                    );
                    c.polyline(
                        &arc(cx + 0.05 * u, cy, 0.8 * u, -PI / 4.0, PI / 4.0, 10),
                        w,
                        color,
                    );
                }
            }
        }
        "headphones" => {
            c.polyline(&arc(cx, cy + 0.1 * u, 0.75 * u, PI, 2.0 * PI, 16), w, color);
            for side in [-1.0_f32, 1.0] {
                let (x0, y0) = at(if side < 0.0 { -0.9 } else { 0.55 }, 0.1);
                c.fill(c.r(x0, y0, 0.35 * u, 0.7 * u), 0.12 * u, color);
            }
        }
        "gear" => {
            c.ring(cx, cy, 0.55 * u, w, color);
            c.ring(cx, cy, 0.2 * u, w, color);
            for i in 0..8 {
                let a = i as f32 * PI / 4.0;
                c.segment(
                    (cx + 0.55 * u * a.cos(), cy + 0.55 * u * a.sin()),
                    (cx + 0.9 * u * a.cos(), cy + 0.9 * u * a.sin()),
                    w * 1.4,
                    color,
                );
            }
        }
        "power" => {
            c.polyline(
                &arc(cx, cy + 0.1 * u, 0.7 * u, -PI * 0.3, PI * 1.3, 24),
                w,
                color,
            );
            c.segment(at(0.0, -0.9), at(0.0, 0.0), w, color);
        }
        "moon" => {
            // A crescent: the outer circle's arc from tip to tip round the
            // left, then back along the inner circle; both meet at the tips.
            // Two strokes, not one closed line: a join at the sharp tips
            // would stick out.
            c.polyline(
                &arc(cx, cy, 0.82 * u, PI * 0.0765, PI * 1.4813, 28),
                w,
                color,
            );
            c.polyline(
                &arc(
                    cx + 0.36 * u,
                    cy - 0.30 * u,
                    0.66 * u,
                    PI * 1.2877,
                    PI * 0.27,
                    22,
                ),
                w,
                color,
            );
        }
        "sun" => {
            c.ring(cx, cy, 0.38 * u, w, color);
            for i in 0..8 {
                let a = i as f32 * PI / 4.0;
                c.segment(
                    (cx + 0.6 * u * a.cos(), cy + 0.6 * u * a.sin()),
                    (cx + 0.88 * u * a.cos(), cy + 0.88 * u * a.sin()),
                    w,
                    color,
                );
            }
        }
        "cloud" | "rain" | "snow" | "storm" | "fog" => {
            // A cloud, raised a little when something falls from it.
            let dy = if name == "cloud" { 0.0 } else { -0.3 };
            let mut pts = arc(
                cx - 0.35 * u,
                cy + (0.1 + dy) * u,
                0.38 * u,
                PI * 0.5,
                PI * 1.5,
                10,
            );
            pts.extend(arc(
                cx + 0.05 * u,
                cy + (-0.1 + dy) * u,
                0.45 * u,
                PI * 1.1,
                PI * 1.95,
                10,
            ));
            pts.extend(arc(
                cx + 0.5 * u,
                cy + (0.15 + dy) * u,
                0.33 * u,
                PI * 1.5,
                PI * 2.5,
                10,
            ));
            c.closed(&pts, w, color);
            let below = cy + (0.65 + dy) * u;
            match name {
                "rain" => {
                    for x in [-0.4_f32, 0.0, 0.4] {
                        c.segment(at(x + 0.08, 0.5), at(x - 0.08, 0.9), w, color);
                    }
                }
                "snow" => {
                    for x in [-0.4_f32, 0.0, 0.4] {
                        c.circle(cx + x * u, below + 0.15 * u, w * 0.9, color);
                    }
                }
                "storm" => c.polyline(
                    &[at(0.1, 0.4), at(-0.15, 0.7), at(0.1, 0.7), at(-0.1, 1.0)],
                    w,
                    color,
                ),
                "fog" => {
                    c.segment(at(-0.7, 0.55), at(0.7, 0.55), w, color);
                    c.segment(at(-0.5, 0.85), at(0.5, 0.85), w, color);
                }
                _ => {}
            }
        }
        "search" => {
            c.ring(cx - 0.15 * u, cy - 0.15 * u, 0.55 * u, w, color);
            c.segment(at(0.25, 0.25), at(0.85, 0.85), w * 1.3, color);
        }
        "home" => c.closed(
            &[
                at(-0.8, -0.05),
                at(0.0, -0.8),
                at(0.8, -0.05),
                at(0.6, -0.05),
                at(0.6, 0.8),
                at(-0.6, 0.8),
                at(-0.6, -0.05),
            ],
            w,
            color,
        ),
        "grid" => {
            for (x, y) in [(-0.45, -0.45), (0.45, -0.45), (-0.45, 0.45), (0.45, 0.45)] {
                let (px, py) = at(x, y);
                c.fill(
                    c.r(px - 0.3 * u, py - 0.3 * u, 0.6 * u, 0.6 * u),
                    0.15 * u,
                    color,
                );
            }
        }
        "display" | "stream" | "tv" => {
            let (x0, y0) = at(-0.9, -0.7);
            c.outline(c.r(x0, y0, 1.8 * u, 1.2 * u), 0.15 * u, w, color);
            match name {
                "display" => c.segment(at(-0.35, 0.85), at(0.35, 0.85), w, color),
                "stream" => c.polygon(&[at(-0.2, -0.35), at(0.35, -0.1), at(-0.2, 0.15)], color),
                _ => {
                    c.segment(at(-0.35, -0.95), at(0.0, -0.7), w, color);
                    c.segment(at(0.35, -0.95), at(0.0, -0.7), w, color);
                    c.segment(at(-0.5, 0.75), at(0.5, 0.75), w, color);
                }
            }
            if name == "stream" {
                c.segment(at(-0.35, 0.85), at(0.35, 0.85), w, color);
            }
        }
        "play" => c.polygon(&[at(-0.45, -0.75), at(0.75, 0.0), at(-0.45, 0.75)], color),
        // The brand mark: a cat's head that is also a TV, the screen is the
        // face, two ears on top. "catfyn" puts a play triangle on
        // the screen instead of the face (the media app).
        "cat" | "catfyn" => {
            let (x0, y0) = at(-0.86, -0.42);
            c.outline(c.r(x0, y0, 1.72 * u, 1.2 * u), 0.32 * u, w, color);
            c.polygon(
                &[at(-0.86, -0.18), at(-0.66, -0.98), at(-0.22, -0.42)],
                color,
            );
            c.polygon(&[at(0.86, -0.18), at(0.66, -0.98), at(0.22, -0.42)], color);
            if name == "cat" {
                c.circle(cx - 0.36 * u, cy + 0.06 * u, 0.12 * u, color);
                c.circle(cx + 0.36 * u, cy + 0.06 * u, 0.12 * u, color);
                c.polygon(&[at(-0.1, 0.3), at(0.1, 0.3), at(0.0, 0.42)], color);
            } else {
                c.polygon(&[at(-0.2, -0.08), at(0.34, 0.17), at(-0.2, 0.42)], color);
            }
        }
        "media" => {
            c.ring(cx, cy, 0.85 * u, w, color);
            c.polygon(&[at(-0.25, -0.4), at(0.45, 0.0), at(-0.25, 0.4)], color);
        }
        "game" => {
            let (x0, y0) = at(-0.95, -0.5);
            c.outline(c.r(x0, y0, 1.9 * u, 1.0 * u), 0.45 * u, w, color);
            c.segment(at(-0.6, 0.0), at(-0.2, 0.0), w, color);
            c.segment(at(-0.4, -0.2), at(-0.4, 0.2), w, color);
            c.circle(cx + 0.35 * u, cy - 0.1 * u, w * 0.9, color);
            c.circle(cx + 0.6 * u, cy + 0.12 * u, w * 0.9, color);
        }
        "music" => {
            c.circle(cx - 0.45 * u, cy + 0.55 * u, 0.25 * u, color);
            c.circle(cx + 0.5 * u, cy + 0.4 * u, 0.25 * u, color);
            c.segment(at(-0.22, 0.55), at(-0.22, -0.6), w, color);
            c.segment(at(0.73, 0.4), at(0.73, -0.8), w, color);
            c.segment(at(-0.22, -0.6), at(0.73, -0.8), w * 1.6, color);
        }
        "browser" | "globe" => {
            c.ring(cx, cy, 0.85 * u, w, color);
            c.segment(at(-0.85, 0.0), at(0.85, 0.0), w, color);
            let mut meridian = arc(cx, cy, 0.85 * u, -PI / 2.0, PI / 2.0, 16);
            for p in &mut meridian {
                p.0 = cx + (p.0 - cx) * 0.45;
            }
            c.polyline(&meridian, w, color);
            let mut back = arc(cx, cy, 0.85 * u, PI / 2.0, PI * 1.5, 16);
            for p in &mut back {
                p.0 = cx + (p.0 - cx) * 0.45;
            }
            c.polyline(&back, w, color);
        }
        "lock" => {
            let (x0, y0) = at(-0.55, -0.1);
            c.fill(c.r(x0, y0, 1.1 * u, 0.9 * u), 0.15 * u, color);
            c.polyline(&arc(cx, cy - 0.1 * u, 0.35 * u, PI, PI * 2.0, 10), w, color);
        }
        "clock" | "timer" => {
            c.ring(
                cx,
                cy + if name == "timer" { 0.1 * u } else { 0.0 },
                0.85 * u,
                w,
                color,
            );
            let oy = if name == "timer" { 0.1 } else { 0.0 };
            c.segment(at(0.0, oy), at(0.0, oy - 0.5), w, color);
            c.segment(at(0.0, oy), at(0.38, oy + 0.22), w, color);
            if name == "timer" {
                c.segment(at(-0.3, -0.98), at(0.3, -0.98), w, color);
            }
        }
        "info" => {
            c.ring(cx, cy, 0.85 * u, w, color);
            c.circle(cx, cy - 0.42 * u, w * 0.9, color);
            c.segment(at(0.0, -0.12), at(0.0, 0.5), w * 1.1, color);
        }
        "chip" => {
            let (x0, y0) = at(-0.55, -0.55);
            c.outline(c.r(x0, y0, 1.1 * u, 1.1 * u), 0.15 * u, w, color);
            let (x1, y1) = at(-0.22, -0.22);
            c.fill(c.r(x1, y1, 0.44 * u, 0.44 * u), 0.06 * u, color);
            for k in [-0.3_f32, 0.0, 0.3] {
                c.segment(at(k, -0.55), at(k, -0.9), w * 0.8, color);
                c.segment(at(k, 0.55), at(k, 0.9), w * 0.8, color);
                c.segment(at(-0.55, k), at(-0.9, k), w * 0.8, color);
                c.segment(at(0.55, k), at(0.9, k), w * 0.8, color);
            }
        }
        "hdr" => {
            let (x0, y0) = at(-0.95, -0.6);
            c.outline(c.r(x0, y0, 1.9 * u, 1.2 * u), 0.2 * u, w, color);
            c.text(
                cx,
                cy,
                Align2::CENTER_CENTER,
                "HDR",
                0.62 * u,
                color,
                Weight::Semibold,
            );
        }
        "sync" => {
            c.polyline(&arc(cx, cy, 0.75 * u, PI * 1.1, PI * 1.9, 14), w, color);
            c.polyline(&arc(cx, cy, 0.75 * u, PI * 0.1, PI * 0.9, 14), w, color);
            let (ax, ay) = (
                cx + 0.75 * u * (PI * 1.9).cos(),
                cy + 0.75 * u * (PI * 1.9).sin(),
            );
            c.polyline(
                &[
                    (ax - 0.3 * u, ay - 0.05 * u),
                    (ax, ay),
                    (ax + 0.05 * u, ay - 0.32 * u),
                ],
                w,
                color,
            );
            let (bx, by) = (
                cx + 0.75 * u * (PI * 0.9).cos(),
                cy + 0.75 * u * (PI * 0.9).sin(),
            );
            c.polyline(
                &[
                    (bx + 0.3 * u, by + 0.05 * u),
                    (bx, by),
                    (bx - 0.05 * u, by + 0.32 * u),
                ],
                w,
                color,
            );
        }
        "image" => {
            let (x0, y0) = at(-0.9, -0.7);
            c.outline(c.r(x0, y0, 1.8 * u, 1.4 * u), 0.15 * u, w, color);
            c.circle(cx + 0.4 * u, cy - 0.3 * u, 0.15 * u, color);
            c.polyline(
                &[
                    at(-0.75, 0.55),
                    at(-0.25, -0.05),
                    at(0.15, 0.35),
                    at(0.4, 0.15),
                    at(0.75, 0.55),
                ],
                w,
                color,
            );
        }
        "key" => {
            c.ring(cx - 0.45 * u, cy, 0.35 * u, w, color);
            c.segment(at(-0.1, 0.0), at(0.9, 0.0), w, color);
            c.segment(at(0.55, 0.0), at(0.55, 0.35), w, color);
            c.segment(at(0.8, 0.0), at(0.8, 0.3), w, color);
        }
        "terminal" => {
            let (x0, y0) = at(-0.9, -0.75);
            c.outline(c.r(x0, y0, 1.8 * u, 1.5 * u), 0.15 * u, w, color);
            c.polyline(&[at(-0.5, -0.3), at(-0.15, 0.0), at(-0.5, 0.3)], w, color);
            c.segment(at(0.05, 0.35), at(0.5, 0.35), w, color);
        }
        "pin" => {
            let mut pts = arc(cx, cy - 0.25 * u, 0.55 * u, PI * 0.8, PI * 2.2, 20);
            pts.push(at(0.0, 0.9));
            c.closed(&pts, w, color);
            c.circle(cx, cy - 0.25 * u, 0.17 * u, color);
        }
        "restart" => {
            c.polyline(&arc(cx, cy, 0.75 * u, -PI * 0.3, PI * 1.35, 24), w, color);
            let (ax, ay) = (
                cx + 0.75 * u * (-PI * 0.3).cos(),
                cy + 0.75 * u * (-PI * 0.3).sin(),
            );
            c.polygon(
                &[
                    (ax - 0.35 * u, ay - 0.05 * u),
                    (ax + 0.1 * u, ay - 0.35 * u),
                    (ax + 0.1 * u, ay + 0.15 * u),
                ],
                color,
            );
            c.segment(at(0.0, -0.35), at(0.0, 0.2), w, color);
        }
        "chevron" => c.polyline(
            &[at(-0.25, -0.55), at(0.3, 0.0), at(-0.25, 0.55)],
            w * 1.2,
            color,
        ),
        "check" => c.polyline(
            &[at(-0.6, 0.0), at(-0.15, 0.45), at(0.65, -0.45)],
            w * 1.4,
            color,
        ),
        "refresh" => {
            c.polyline(&arc(cx, cy, 0.7 * u, -PI * 0.1, PI * 1.5, 20), w, color);
            let (ex, ey) = (
                cx + 0.7 * u * (PI * 1.5).cos(),
                cy + 0.7 * u * (PI * 1.5).sin(),
            );
            c.polygon(
                &[
                    (ex - 0.05 * u, ey - 0.3 * u),
                    (ex + 0.35 * u, ey),
                    (ex - 0.05 * u, ey + 0.3 * u),
                ],
                color,
            );
        }
        "disk" => {
            let (x0, y0) = at(-0.85, -0.5);
            c.outline(c.r(x0, y0, 1.7 * u, 1.0 * u), 0.2 * u, w, color);
            c.circle(cx + 0.5 * u, cy, w, color);
            c.segment(at(-0.55, 0.0), at(0.15, 0.0), w, color);
        }
        "spark" => {
            c.polygon(
                &[
                    at(0.0, -0.95),
                    at(0.22, -0.22),
                    at(0.95, 0.0),
                    at(0.22, 0.22),
                    at(0.0, 0.95),
                    at(-0.22, 0.22),
                    at(-0.95, 0.0),
                    at(-0.22, -0.22),
                ],
                color,
            );
        }
        "store" => {
            let (x0, y0) = at(-0.75, -0.35);
            c.outline(c.r(x0, y0, 1.5 * u, 1.25 * u), 0.2 * u, w, color);
            c.polyline(
                &arc(cx, cy - 0.35 * u, 0.38 * u, PI, PI * 2.0, 12),
                w,
                color,
            );
        }
        "android" => {
            c.polyline(&arc(cx, cy + 0.25 * u, 0.8 * u, PI, PI * 2.0, 20), w, color);
            c.segment(at(-0.8, 0.25), at(0.8, 0.25), w, color);
            c.segment(at(-0.45, -0.45), at(-0.7, -0.85), w, color);
            c.segment(at(0.45, -0.45), at(0.7, -0.85), w, color);
            c.circle(cx - 0.32 * u, cy - 0.05 * u, w * 0.9, color);
            c.circle(cx + 0.32 * u, cy - 0.05 * u, w * 0.9, color);
        }
        "cast" => {
            c.polyline(
                &[
                    at(-0.85, -0.25),
                    at(-0.85, -0.75),
                    at(0.85, -0.75),
                    at(0.85, 0.75),
                    at(0.15, 0.75),
                ],
                w,
                color,
            );
            for r in [0.3_f32, 0.62, 0.95] {
                c.polyline(
                    &arc(cx - 0.85 * u, cy + 0.75 * u, r * u, -PI / 2.0, 0.0, 8),
                    w,
                    color,
                );
            }
        }
        "keyboard" => {
            let (x0, y0) = at(-0.95, -0.6);
            c.outline(c.r(x0, y0, 1.9 * u, 1.2 * u), 0.2 * u, w, color);
            for row in 0..2 {
                for k in 0..5 {
                    let (kx, ky) = at(-0.62 + k as f32 * 0.31, -0.28 + row as f32 * 0.3);
                    c.circle(kx, ky, w * 0.75, color);
                }
            }
            c.segment(at(-0.45, 0.33), at(0.45, 0.33), w, color);
        }
        "palette" => {
            c.ring(cx, cy, 0.85 * u, w, color);
            for (x, y) in [(-0.35, -0.3), (0.12, -0.45), (0.45, -0.05)] {
                let (px, py) = at(x, y);
                c.circle(px, py, 0.13 * u, color);
            }
            c.circle(cx - 0.25 * u, cy + 0.35 * u, 0.2 * u, color);
        }
        "phone" | "remote" => {
            let (x0, y0) = at(-0.5, -0.95);
            c.outline(c.r(x0, y0, 1.0 * u, 1.9 * u), 0.22 * u, w, color);
            if name == "phone" {
                c.segment(at(-0.15, 0.7), at(0.15, 0.7), w, color);
            } else {
                c.ring(cx, cy - 0.35 * u, 0.25 * u, w, color);
                c.circle(cx, cy + 0.35 * u, w * 0.9, color);
            }
        }
        "download" => {
            c.segment(at(0.0, -0.85), at(0.0, 0.35), w, color);
            c.polyline(&[at(-0.4, -0.05), at(0.0, 0.35), at(0.4, -0.05)], w, color);
            c.segment(at(-0.75, 0.8), at(0.75, 0.8), w, color);
        }
        "trash" => {
            c.segment(at(-0.8, -0.55), at(0.8, -0.55), w, color);
            c.segment(at(-0.25, -0.85), at(0.25, -0.85), w, color);
            c.closed(
                &[
                    at(-0.6, -0.55),
                    at(-0.45, 0.85),
                    at(0.45, 0.85),
                    at(0.6, -0.55),
                ],
                w,
                color,
            );
        }
        "usb" => {
            let (x0, y0) = at(-0.4, -0.95);
            c.outline(c.r(x0, y0, 0.8 * u, 0.7 * u), 0.08 * u, w, color);
            let (x1, y1) = at(-0.6, -0.25);
            c.outline(c.r(x1, y1, 1.2 * u, 1.2 * u), 0.25 * u, w, color);
        }
        // RJ-45 plug: a rounded body with a latch tab and three contacts.
        "ethernet" => {
            let (x0, y0) = at(-0.75, -0.55);
            c.outline(c.r(x0, y0, 1.5 * u, 1.15 * u), 0.15 * u, w, color);
            let (x1, y1) = at(-0.3, 0.6);
            c.outline(c.r(x1, y1, 0.6 * u, 0.3 * u), 0.05 * u, w, color);
            for x in [-0.35_f32, 0.0, 0.35] {
                c.segment(at(x, -0.3), at(x, 0.05), w, color);
            }
        }
        // Folder: a tab on the back panel, then the body.
        "folder" => {
            c.closed(
                &[
                    at(-0.8, -0.55),
                    at(-0.3, -0.55),
                    at(-0.15, -0.38),
                    at(0.8, -0.38),
                    at(0.8, 0.6),
                    at(-0.8, 0.6),
                ],
                w,
                color,
            );
            c.segment(at(-0.8, -0.18), at(0.8, -0.18), w, color);
        }
        "plus" => {
            c.segment(at(-0.7, 0.0), at(0.7, 0.0), w * 1.2, color);
            c.segment(at(0.0, -0.7), at(0.0, 0.7), w * 1.2, color);
        }
        "close" => {
            c.segment(at(-0.6, -0.6), at(0.6, 0.6), w * 1.2, color);
            c.segment(at(-0.6, 0.6), at(0.6, -0.6), w * 1.2, color);
        }
        "back" => c.polyline(
            &[at(0.3, -0.55), at(-0.25, 0.0), at(0.3, 0.55)],
            w * 1.2,
            color,
        ),
        // Mouse pointer: the arrow with its tail.
        "pointer" => {
            c.closed(
                &[
                    at(-0.45, -0.8),
                    at(0.5, 0.15),
                    at(0.02, 0.18),
                    at(0.3, 0.75),
                    at(0.08, 0.85),
                    at(-0.2, 0.28),
                    at(-0.45, 0.55),
                ],
                w,
                color,
            );
        }
        // Full screen: two corner brackets pointing out / in.
        "expand" => {
            c.polyline(
                &[at(-0.75, -0.25), at(-0.75, -0.75), at(-0.25, -0.75)],
                w,
                color,
            );
            c.polyline(&[at(0.25, 0.75), at(0.75, 0.75), at(0.75, 0.25)], w, color);
            c.segment(at(-0.65, -0.65), at(-0.15, -0.15), w, color);
            c.segment(at(0.65, 0.65), at(0.15, 0.15), w, color);
        }
        "shrink" => {
            c.polyline(
                &[at(-0.25, -0.75), at(-0.25, -0.25), at(-0.75, -0.25)],
                w,
                color,
            );
            c.polyline(&[at(0.25, 0.75), at(0.25, 0.25), at(0.75, 0.25)], w, color);
            c.segment(at(-0.3, -0.3), at(-0.8, -0.8), w, color);
            c.segment(at(0.3, 0.3), at(0.8, 0.8), w, color);
        }
        other => {
            let label: String = other.chars().take(2).collect::<String>().to_uppercase();
            c.text(
                cx,
                cy,
                Align2::CENTER_CENTER,
                &label,
                size * 0.72,
                color,
                Weight::Semibold,
            );
        }
    }
}

/// Wi-Fi signal strength as 1–4 bars.
pub fn signal_bars<H: Copy>(
    c: &Canvas<H>,
    x: f32,
    y: f32,
    signal: u8,
    color: Color32,
    dim: Color32,
) {
    let bars = match signal {
        0..=24 => 1,
        25..=49 => 2,
        50..=74 => 3,
        _ => 4,
    };
    for i in 0..4 {
        let h = 5.0 + i as f32 * 4.0;
        let col = if i < bars { color } else { dim };
        c.fill(c.r(x + i as f32 * 6.0, y + 18.0 - h, 4.0, h), 1.0, col);
    }
}

/// The brand mark: accent rounded square with the cat-TV glyph.
pub fn logo<H: Copy>(c: &Canvas<H>, x: f32, y: f32, size: f32) {
    let r = c.r(x, y, size, size);
    c.shadow(r, size * 0.3, 4.0, 18.0, theme::accent_a(90));
    c.fill(r, size * 0.3, theme::accent());
    draw(
        c,
        "cat",
        x + size / 2.0,
        y + size / 2.0 + size * 0.06,
        size * 0.62,
        theme::on_accent(),
    );
}
