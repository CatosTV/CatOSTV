//! Logical remote input.
//!
//! CatosTV screens never react to raw keys directly. The GUI translates toolkit
//! key events into [`InputEvent`]s, and [`InputMapper`] turns them into
//! logical [`Action`]s that the navigation state machines understand.
//!
//! Remote contract:
//!
//! | Physical key                      | Action                         |
//! |-----------------------------------|--------------------------------|
//! | Arrow keys (with auto-repeat)     | `Up` / `Down` / `Left` / `Right` |
//! | Enter, Space (tap)                | `Ok` (fires on release)        |
//! | Enter, Space (held ≥ 0.6 s)       | `OkLong` (fires while held)    |
//! | Escape, Backspace, Browser Back   | `Back`                         |
//! | Home                              | `Home`                         |
//! | F10                               | `Quick` (Control Center)       |
//! | F2                                | `Options` (same as long-press) |
//! | Typed characters                  | `Char(c)` (outside text mode: only visible characters, never space, screens use them for type-to-search) |
//! | Backspace (text mode only)        | `Erase`                        |
//!
//! `Ok` fires on *release* so that a long press can be distinguished from a
//! tap. This is the same model Android TV uses.

/// How long OK must be held to count as a long press, in seconds.
pub const LONG_PRESS_SECS: f64 = 0.6;

/// Toolkit-independent key identity. The GUI maps its own key enum onto this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Space,
    Escape,
    Backspace,
    BrowserBack,
    Home,
    F2,
    F10,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Key {
        key: RawKey,
        pressed: bool,
        repeat: bool,
    },
    /// Text produced by a physical keyboard (already composed by the toolkit).
    Text(String),
}

impl InputEvent {
    pub fn press(key: RawKey) -> Self {
        Self::Key {
            key,
            pressed: true,
            repeat: false,
        }
    }
    pub fn release(key: RawKey) -> Self {
        Self::Key {
            key,
            pressed: false,
            repeat: false,
        }
    }
    pub fn repeat(key: RawKey) -> Self {
        Self::Key {
            key,
            pressed: true,
            repeat: true,
        }
    }
}

/// Logical remote action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    Ok,
    OkLong,
    Back,
    Home,
    Quick,
    Options,
    Char(char),
    Erase,
}

/// Converts raw key events into actions and detects long presses.
#[derive(Debug, Default, Clone)]
pub struct InputMapper {
    ok_down_at: Option<f64>,
    long_fired: bool,
}

impl InputMapper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process this frame's events. `now` is a monotonic time in seconds.
    /// `text_mode` is true while an on-screen keyboard has focus: typed
    /// characters are delivered and Space/Backspace edit text.
    pub fn map(&mut self, events: &[InputEvent], now: f64, text_mode: bool) -> Vec<Action> {
        let mut out = Vec::new();
        for ev in events {
            match ev {
                InputEvent::Text(s) => {
                    if text_mode {
                        out.extend(s.chars().filter(|c| !c.is_control()).map(Action::Char));
                    } else {
                        // Type-to-search: visible characters only. Space is
                        // OK outside text mode and must never type.
                        out.extend(
                            s.chars()
                                .filter(|c| !c.is_control() && !c.is_whitespace())
                                .map(Action::Char),
                        );
                    }
                }
                InputEvent::Key {
                    key,
                    pressed,
                    repeat,
                } => self.key(*key, *pressed, *repeat, now, text_mode, &mut out),
            }
        }
        if let Some(a) = self.tick(now) {
            out.push(a);
        }
        out
    }

    fn key(
        &mut self,
        key: RawKey,
        pressed: bool,
        repeat: bool,
        now: f64,
        text_mode: bool,
        out: &mut Vec<Action>,
    ) {
        let is_ok_key = key == RawKey::Enter || (key == RawKey::Space && !text_mode);
        if is_ok_key {
            if pressed && !repeat && self.ok_down_at.is_none() {
                self.ok_down_at = Some(now);
                self.long_fired = false;
            } else if !pressed {
                if self.ok_down_at.is_some() && !self.long_fired {
                    out.push(Action::Ok);
                }
                self.ok_down_at = None;
                self.long_fired = false;
            }
            return;
        }
        if !pressed {
            return;
        }
        let action = match key {
            RawKey::Up => Some(Action::Up),
            RawKey::Down => Some(Action::Down),
            RawKey::Left => Some(Action::Left),
            RawKey::Right => Some(Action::Right),
            // Non-directional keys do not auto-repeat.
            _ if repeat => None,
            RawKey::Escape | RawKey::BrowserBack => Some(Action::Back),
            RawKey::Backspace if text_mode => Some(Action::Erase),
            RawKey::Backspace => Some(Action::Back),
            RawKey::Home => Some(Action::Home),
            RawKey::F10 => Some(Action::Quick),
            RawKey::F2 => Some(Action::Options),
            // Space in text mode is delivered through InputEvent::Text.
            RawKey::Space | RawKey::Enter | RawKey::Other => None,
        };
        out.extend(action);
    }

    fn tick(&mut self, now: f64) -> Option<Action> {
        match self.ok_down_at {
            Some(t) if !self.long_fired && now - t >= LONG_PRESS_SECS => {
                self.long_fired = true;
                Some(Action::OkLong)
            }
            _ => None,
        }
    }

    /// True while OK is held and a long press could still fire; the GUI
    /// should keep repainting so [`InputMapper::map`] is called again.
    pub fn needs_tick(&self) -> bool {
        self.ok_down_at.is_some() && !self.long_fired
    }

    /// Forget any held key. Call when the window loses focus (for example
    /// when an app takes over) so a missing key-release cannot later be
    /// misread as a long press.
    pub fn reset(&mut self) {
        self.ok_down_at = None;
        self.long_fired = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RawKey::*;

    fn tap(m: &mut InputMapper, key: RawKey, t: f64) -> Vec<Action> {
        m.map(
            &[InputEvent::press(key), InputEvent::release(key)],
            t,
            false,
        )
    }

    #[test]
    fn tap_enter_is_ok_on_release() {
        let mut m = InputMapper::new();
        assert_eq!(m.map(&[InputEvent::press(Enter)], 0.0, false), vec![]);
        assert!(m.needs_tick());
        assert_eq!(
            m.map(&[InputEvent::release(Enter)], 0.1, false),
            vec![Action::Ok]
        );
        assert!(!m.needs_tick());
    }

    #[test]
    fn press_and_release_in_same_frame_is_ok() {
        let mut m = InputMapper::new();
        assert_eq!(tap(&mut m, Enter, 1.0), vec![Action::Ok]);
        assert_eq!(tap(&mut m, Space, 2.0), vec![Action::Ok]);
    }

    #[test]
    fn hold_enter_fires_one_long_press_and_no_ok() {
        let mut m = InputMapper::new();
        m.map(&[InputEvent::press(Enter)], 0.0, false);
        assert_eq!(m.map(&[], 0.3, false), vec![]);
        assert_eq!(
            m.map(&[InputEvent::repeat(Enter)], 0.7, false),
            vec![Action::OkLong]
        );
        assert_eq!(m.map(&[InputEvent::repeat(Enter)], 1.5, false), vec![]);
        assert_eq!(m.map(&[InputEvent::release(Enter)], 1.6, false), vec![]);
    }

    #[test]
    fn arrows_repeat_but_back_does_not() {
        let mut m = InputMapper::new();
        let ev = [
            InputEvent::press(Down),
            InputEvent::repeat(Down),
            InputEvent::press(Escape),
            InputEvent::repeat(Escape),
        ];
        assert_eq!(
            m.map(&ev, 0.0, false),
            vec![Action::Down, Action::Down, Action::Back]
        );
    }

    #[test]
    fn text_mode_delivers_chars_and_erase() {
        let mut m = InputMapper::new();
        let ev = [
            InputEvent::Text("a b".into()),
            InputEvent::press(Space), // ignored in text mode; Text carries the space
            InputEvent::press(Backspace),
        ];
        assert_eq!(
            m.map(&ev, 0.0, true),
            vec![
                Action::Char('a'),
                Action::Char(' '),
                Action::Char('b'),
                Action::Erase
            ]
        );
        // Outside text mode, visible characters still arrive (type-to-search)
        // but the space does not, and Backspace is Back.
        assert_eq!(
            m.map(&ev, 0.0, false),
            vec![Action::Char('a'), Action::Char('b'), Action::Back]
        );
    }

    #[test]
    fn reset_prevents_phantom_long_press() {
        let mut m = InputMapper::new();
        m.map(&[InputEvent::press(Enter)], 0.0, false);
        m.reset(); // window lost focus; release went to another app
        assert_eq!(m.map(&[], 10.0, false), vec![]);
    }

    #[test]
    fn global_keys() {
        let mut m = InputMapper::new();
        assert_eq!(tap(&mut m, Home, 0.0), vec![Action::Home]);
        assert_eq!(tap(&mut m, F10, 0.0), vec![Action::Quick]);
        assert_eq!(tap(&mut m, F2, 0.0), vec![Action::Options]);
        assert_eq!(tap(&mut m, BrowserBack, 0.0), vec![Action::Back]);
    }
}
