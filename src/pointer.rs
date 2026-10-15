//! Mouse / touchpad input ("allow cursor use").
//!
//! The TV is remote-first, so the pointer is an *extra*: hovering focuses
//! what is under it, a click is OK, a right click is Back and the wheel
//! moves up/down. The cursor hides itself after a few seconds without
//! movement or as soon as a key is pressed.

use eframe::egui::{self, Event, MouseWheelUnit, PointerButton, Pos2};
use std::time::{Duration, Instant};

/// Hide the cursor after this long without pointer movement.
const HIDE_AFTER: Duration = Duration::from_secs(3);
/// Wheel distance (points) that counts as one step.
const POINTS_PER_STEP: f32 = 60.0;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Frame {
    /// Pointer position if it moved this frame.
    pub moved_to: Option<Pos2>,
    /// Primary button released (a click) at this position.
    pub click: Option<Pos2>,
    /// Secondary (right) button released.
    pub back: bool,
    /// Wheel steps: positive = content moves down = user scrolls up.
    pub wheel: i32,
    /// A key was pressed (hides the cursor).
    pub key: bool,
}

pub struct Pointer {
    last_move: Option<Instant>,
    wheel_acc: f32,
}

impl Default for Pointer {
    fn default() -> Self {
        Self::new()
    }
}

impl Pointer {
    pub fn new() -> Self {
        Self {
            last_move: None,
            wheel_acc: 0.0,
        }
    }

    /// Read this frame's pointer events and update cursor visibility.
    pub fn collect(&mut self, ctx: &egui::Context) -> Frame {
        let mut f = Frame::default();
        ctx.input(|i| {
            for e in &i.events {
                match e {
                    Event::PointerMoved(p) => f.moved_to = Some(*p),
                    Event::PointerButton {
                        pos,
                        button,
                        pressed: false,
                        ..
                    } => match button {
                        PointerButton::Primary => f.click = Some(*pos),
                        PointerButton::Secondary => f.back = true,
                        _ => {}
                    },
                    Event::MouseWheel { unit, delta, .. } => {
                        self.wheel_acc += match unit {
                            MouseWheelUnit::Point => delta.y / POINTS_PER_STEP,
                            MouseWheelUnit::Line => delta.y,
                            MouseWheelUnit::Page => delta.y * 3.0,
                        };
                    }
                    Event::Key { pressed: true, .. } => f.key = true,
                    _ => {}
                }
            }
        });
        let steps = self.wheel_acc.trunc();
        self.wheel_acc -= steps;
        f.wheel = steps as i32;
        if f.moved_to.is_some() || f.click.is_some() {
            self.last_move = Some(Instant::now());
        }
        if f.key {
            self.last_move = None;
        }
        let visible = self.last_move.is_some_and(|t| t.elapsed() < HIDE_AFTER);
        ctx.set_cursor_icon(if visible {
            egui::CursorIcon::Default
        } else {
            egui::CursorIcon::None
        });
        if visible {
            ctx.request_repaint_after(HIDE_AFTER);
        }
        f
    }
}
