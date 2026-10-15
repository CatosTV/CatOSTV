//! egui key events → toolkit-independent [`InputEvent`]s for catostv-core.

use catostv_core::input::{InputEvent, RawKey};
use eframe::egui::{self, Event, Key};

fn raw(key: Key) -> RawKey {
    match key {
        Key::ArrowUp => RawKey::Up,
        Key::ArrowDown => RawKey::Down,
        Key::ArrowLeft => RawKey::Left,
        Key::ArrowRight => RawKey::Right,
        Key::Enter => RawKey::Enter,
        Key::Space => RawKey::Space,
        Key::Escape => RawKey::Escape,
        Key::Backspace => RawKey::Backspace,
        Key::BrowserBack => RawKey::BrowserBack,
        Key::Home => RawKey::Home,
        Key::F2 => RawKey::F2,
        Key::F10 => RawKey::F10,
        _ => RawKey::Other,
    }
}

/// This frame's key/text events, in order.
pub fn collect(ctx: &egui::Context) -> Vec<InputEvent> {
    ctx.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                Event::Key {
                    key,
                    pressed,
                    repeat,
                    modifiers,
                    ..
                } => {
                    // Ctrl/Alt/Super chords belong to the compositor or apps.
                    if modifiers.ctrl || modifiers.alt || modifiers.command || modifiers.mac_cmd {
                        return None;
                    }
                    Some(InputEvent::Key {
                        key: raw(*key),
                        pressed: *pressed,
                        repeat: *repeat,
                    })
                }
                Event::Text(t) => Some(InputEvent::Text(t.clone())),
                _ => None,
            })
            .collect()
    })
}

/// Whether the CatosTV window currently has keyboard focus.
pub fn window_focused(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.focused)
}

/// Monotonic time in seconds, as used by the input mapper.
pub fn now(ctx: &egui::Context) -> f64 {
    ctx.input(|i| i.time)
}
