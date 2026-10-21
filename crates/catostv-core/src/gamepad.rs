//! Game controllers as TV remotes.
//!
//! `catostv-inputd` reads USB and Bluetooth game controllers passively (like
//! keyboards, never grabbed: games still get every button) and turns their
//! buttons and D-pad into CatosTV navigation, sent to the shell over IPC.
//! This module is the pure part: which events count, and the mapping with
//! key repeat for a held direction.
//!
//! Mapping (Xbox names; "Nintendo layout" swaps A and B):
//!
//! | Button            | Action                     |
//! |-------------------|----------------------------|
//! | D-pad             | Up / Down / Left / Right   |
//! | A (south)         | OK                         |
//! | B (east)          | Back                       |
//! | Y (north), Start  | Menu (options)             |
//! | X (west), Select  | Control Center             |
//! | Guide (mode)      | Home (also from inside apps) |
//!
//! While an Android app is in front only Guide acts (Home); every other
//! button and direction is left to the app ([`over_android`]). Linux apps
//! keep the mapping.
//!
//! Any other controller (cheap USB pads, arcade sticks: "generic" buttons
//! `BTN_TRIGGER`…`BTN_BASE6`) works too: button 3 OK,
//! button 2 Back, button 1 / 10 Menu, button 4 / 9 Control Center. The left
//! stick moves like the D-pad (many cheap pads report their D-pad as a stick).
//! The stick's range comes from the device (`catostv-inputd` asks the kernel)
//! and is turned into -1 / 0 / 1 before it reaches this module.

/// CatosTV's own apps made for a remote, which do not read controllers
/// themselves: while one is in front, a controller presses its keys
/// (arrows, OK = Enter, Back = Esc, Menu = F2), like the remote. Games and
/// other apps get the controller as it is.
pub const REMOTE_APPS: [&str; 3] = ["cattube", "catfyn-player", "catostv-files-player"];

/// `$XDG_RUNTIME_DIR/catostv-pad-keys`: `on` while one of [`REMOTE_APPS`]
/// is in front (written by the shell, read by `catostv-inputd`).
pub fn keys_path() -> std::path::PathBuf {
    crate::env::runtime_dir().join("catostv-pad-keys")
}

pub fn keys_on() -> bool {
    std::fs::read_to_string(keys_path()).is_ok_and(|s| s.trim() == "on")
}

pub fn set_keys(on: bool) {
    let path = keys_path();
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, if on { "on\n" } else { "off\n" }).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// The key a controller presses in one of [`REMOTE_APPS`]; `None` for
/// what stays CatosTV's (Home, the Control Center).
pub fn key_for(out: PadOut) -> Option<u16> {
    use crate::uinput::{KEY_DOWN, KEY_ENTER, KEY_ESC, KEY_F2, KEY_LEFT, KEY_RIGHT, KEY_UP};
    Some(match out {
        PadOut::Up => KEY_UP,
        PadOut::Down => KEY_DOWN,
        PadOut::Left => KEY_LEFT,
        PadOut::Right => KEY_RIGHT,
        PadOut::Ok => KEY_ENTER,
        PadOut::Back => KEY_ESC,
        PadOut::Menu => KEY_F2,
        PadOut::Quick | PadOut::Home => return None,
    })
}

/// `EV_KEY` / `EV_ABS` event types.
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;

pub const BTN_SOUTH: u16 = 0x130;
pub const BTN_EAST: u16 = 0x131;
pub const BTN_NORTH: u16 = 0x133;
pub const BTN_WEST: u16 = 0x134;
pub const BTN_SELECT: u16 = 0x13a;
pub const BTN_START: u16 = 0x13b;
pub const BTN_MODE: u16 = 0x13c;
pub const BTN_DPAD_UP: u16 = 0x220;
pub const BTN_DPAD_DOWN: u16 = 0x221;
pub const BTN_DPAD_LEFT: u16 = 0x222;
pub const BTN_DPAD_RIGHT: u16 = 0x223;
/// Generic joystick buttons ("button 1" … "button 12").
pub const BTN_TL: u16 = 0x136;
pub const BTN_TR: u16 = 0x137;
pub const BTN_TL2: u16 = 0x138;
pub const BTN_TR2: u16 = 0x139;
pub const BTN_THUMBL: u16 = 0x13d;
pub const BTN_THUMBR: u16 = 0x13e;
pub const BTN_TRIGGER: u16 = 0x120;
pub const BTN_THUMB: u16 = 0x121;
pub const BTN_THUMB2: u16 = 0x122;
pub const BTN_TOP: u16 = 0x123;
pub const BTN_BASE3: u16 = 0x128;
pub const BTN_BASE4: u16 = 0x129;
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;

/// First repeat of a held direction, then the repeat interval (seconds).
pub const REPEAT_DELAY: f64 = 0.42;
pub const REPEAT_EVERY: f64 = 0.13;

/// A controller event that matters for navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadEvent {
    Button {
        code: u16,
        pressed: bool,
    },
    Hat {
        code: u16,
        value: i32,
    },
    /// Left stick pushed past half way: `dir` is -1, 0 or 1 on `code`
    /// (`ABS_X` / `ABS_Y`), see [`stick_dir`].
    Stick {
        code: u16,
        dir: i32,
    },
}

/// A stick position as -1 / 0 / 1: past 55 % of the way from the centre
/// of `min..=max` counts as pushed.
pub fn stick_dir(value: i32, min: i32, max: i32) -> i32 {
    if max <= min {
        return 0;
    }
    let half = (max as f64 - min as f64) / 2.0;
    let pos = (value as f64 - (min as f64 + half)) / half;
    if pos > 0.55 {
        1
    } else if pos < -0.55 {
        -1
    } else {
        0
    }
}

/// The navigation event for a raw evdev event from a controller, if any.
/// Everything else (sticks, triggers, gyro…) is dropped right away.
pub fn pad_event(ev_type: u16, code: u16, value: i32) -> Option<PadEvent> {
    match (ev_type, code) {
        (EV_KEY, _) if is_button(code) && (value == 0 || value == 1) => Some(PadEvent::Button {
            code,
            pressed: value == 1,
        }),
        (EV_ABS, ABS_HAT0X | ABS_HAT0Y) => Some(PadEvent::Hat { code, value }),
        _ => None,
    }
}

/// What a controller asks CatosTV to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PadOut {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
    Menu,
    Quick,
    Home,
}

/// What a controller does while an Android app is in front: only the
/// Guide button (`BTN_MODE`) acts, as Home. Every other
/// button, a custom Home binding included (a Home on BTN_WEST
/// was a racing game's accelerator), and every direction, held or
/// repeated, belong to the app, which gets the controller itself
/// (`android-pads`). Linux apps (Catfyn's player, the Files player,
/// Flatpaks) keep the full mapping: they do not read controllers, and the
/// Control Center is their only controller route to Back / Full screen /
/// Close.
pub fn over_android(ev: PadEvent) -> Option<PadOut> {
    matches!(
        ev,
        PadEvent::Button {
            code: BTN_MODE,
            pressed: true
        }
    )
    .then_some(PadOut::Home)
}

/// The actions a button can be bound to in Settings → Navigation &
/// controllers → Controller navigation. Directions come from the
/// d-pad and sticks and are not bindable.
pub const BINDABLE: [PadOut; 5] = [
    PadOut::Ok,
    PadOut::Back,
    PadOut::Menu,
    PadOut::Quick,
    PadOut::Home,
];

impl PadOut {
    /// Stable name in `ui.conf` (`pad_bind = ok 0x130`).
    pub fn key(self) -> &'static str {
        match self {
            PadOut::Up => "up",
            PadOut::Down => "down",
            PadOut::Left => "left",
            PadOut::Right => "right",
            PadOut::Ok => "ok",
            PadOut::Back => "back",
            PadOut::Menu => "options",
            PadOut::Quick => "quick",
            PadOut::Home => "home",
        }
    }

    pub fn from_key(k: &str) -> Option<Self> {
        BINDABLE.into_iter().find(|p| p.key() == k)
    }

    pub fn label(self) -> &'static str {
        match self {
            PadOut::Up => "Up",
            PadOut::Down => "Down",
            PadOut::Left => "Left",
            PadOut::Right => "Right",
            PadOut::Ok => "OK",
            PadOut::Back => "Back",
            PadOut::Menu => "Options",
            PadOut::Quick => "Control Center",
            PadOut::Home => "Home",
        }
    }
}

/// Human name of a controller button code ("A", "Start", "L1", "Button 7").
pub fn button_name(code: u16) -> String {
    match code {
        BTN_SOUTH => "A".into(),
        BTN_EAST => "B".into(),
        BTN_NORTH => "X".into(),
        BTN_WEST => "Y".into(),
        BTN_SELECT => "Select".into(),
        BTN_START => "Start".into(),
        BTN_MODE => "Guide".into(),
        BTN_TL => "L1".into(),
        BTN_TR => "R1".into(),
        BTN_TL2 => "L2".into(),
        BTN_TR2 => "R2".into(),
        BTN_THUMBL => "L3".into(),
        BTN_THUMBR => "R3".into(),
        BTN_DPAD_UP => "D-pad up".into(),
        BTN_DPAD_DOWN => "D-pad down".into(),
        BTN_DPAD_LEFT => "D-pad left".into(),
        BTN_DPAD_RIGHT => "D-pad right".into(),
        0x120..=0x12f => format!("Button {}", code - 0x120 + 1),
        _ => format!("Button 0x{code:x}"),
    }
}

/// Is `code` a controller button at all (anything in the BTN_ ranges)?
pub fn is_button(code: u16) -> bool {
    matches!(code, 0x100..=0x13f | 0x220..=0x223)
}

/// Settings asks `catostv-inputd` to capture the next button: while this
/// file exists, button presses are written to [`CAPTURED_FILE`] and
/// announced with `ShellCommand::PadCaptured` instead of acting.
pub fn capture_flag() -> std::path::PathBuf {
    crate::env::runtime_dir().join("catostv-pad-capture")
}

pub fn captured_file() -> std::path::PathBuf {
    crate::env::runtime_dir().join("catostv-pad-captured")
}

/// `(code)` written by inputd, read by the shell.
pub fn read_captured() -> Option<u16> {
    let s = std::fs::read_to_string(captured_file()).ok()?;
    s.trim().parse().ok()
}

/// Turns one controller's events into [`PadOut`]s, with key repeat while a
/// direction is held.
#[derive(Debug, Default, Clone)]
pub struct PadMapper {
    /// Nintendo layout: the right face button (east) is OK.
    pub nintendo: bool,
    /// Custom button → action (Settings → Controller navigation); checked
    /// before the layout defaults.
    pub bindings: Vec<(u16, PadOut)>,
    dpad: [bool; 4],
    hat: (i32, i32),
    stick: (i32, i32),
    held: Option<PadOut>,
    next_repeat: f64,
}

impl PadMapper {
    pub fn new(nintendo: bool) -> Self {
        Self {
            nintendo,
            ..Self::default()
        }
    }

    fn direction(&self) -> Option<PadOut> {
        let [up, down, left, right] = self.dpad;
        let (hx, hy) = (self.hat.0 + self.stick.0, self.hat.1 + self.stick.1);
        if up || hy < 0 {
            Some(PadOut::Up)
        } else if down || hy > 0 {
            Some(PadOut::Down)
        } else if left || hx < 0 {
            Some(PadOut::Left)
        } else if right || hx > 0 {
            Some(PadOut::Right)
        } else {
            None
        }
    }

    /// Handle one event at `now` (seconds, monotonic).
    pub fn on_event(&mut self, ev: PadEvent, now: f64) -> Vec<PadOut> {
        let before = self.direction();
        let mut out = Vec::new();
        match ev {
            PadEvent::Button { code, pressed }
                if pressed && self.bindings.iter().any(|(c, _)| *c == code) =>
            {
                if let Some((_, o)) = self.bindings.iter().find(|(c, _)| *c == code) {
                    out.push(*o);
                }
            }
            PadEvent::Button { code, pressed }
                if !pressed && self.bindings.iter().any(|(c, _)| *c == code) => {}
            PadEvent::Button { code, pressed } => match code {
                BTN_DPAD_UP => self.dpad[0] = pressed,
                BTN_DPAD_DOWN => self.dpad[1] = pressed,
                BTN_DPAD_LEFT => self.dpad[2] = pressed,
                BTN_DPAD_RIGHT => self.dpad[3] = pressed,
                _ if !pressed => {}
                BTN_SOUTH => out.push(if self.nintendo {
                    PadOut::Back
                } else {
                    PadOut::Ok
                }),
                BTN_EAST => out.push(if self.nintendo {
                    PadOut::Ok
                } else {
                    PadOut::Back
                }),
                BTN_NORTH | BTN_START => out.push(PadOut::Menu),
                BTN_WEST | BTN_SELECT => out.push(PadOut::Quick),
                BTN_MODE => out.push(PadOut::Home),
                // Generic pads: buttons 1-4 sit like △ ○ ✕ □.
                BTN_THUMB2 => out.push(if self.nintendo {
                    PadOut::Back
                } else {
                    PadOut::Ok
                }),
                BTN_THUMB => out.push(if self.nintendo {
                    PadOut::Ok
                } else {
                    PadOut::Back
                }),
                BTN_TRIGGER | BTN_BASE4 => out.push(PadOut::Menu),
                BTN_TOP | BTN_BASE3 => out.push(PadOut::Quick),
                _ => {}
            },
            PadEvent::Stick { code, dir } => {
                if code == ABS_X {
                    self.stick.0 = dir.signum();
                } else {
                    self.stick.1 = dir.signum();
                }
            }
            PadEvent::Hat { code, value } => {
                let v = value.signum();
                if code == ABS_HAT0X {
                    self.hat.0 = v;
                } else {
                    self.hat.1 = v;
                }
            }
        }
        let after = self.direction();
        if after != before {
            self.held = after;
            if let Some(d) = after {
                out.push(d);
                self.next_repeat = now + REPEAT_DELAY;
            }
        }
        out
    }

    /// When [`Self::tick`] next has something to do.
    pub fn next_deadline(&self) -> Option<f64> {
        self.held.map(|_| self.next_repeat)
    }

    /// Repeat a held direction.
    pub fn tick(&mut self, now: f64) -> Option<PadOut> {
        let d = self.held?;
        if now + 1e-9 < self.next_repeat {
            return None;
        }
        self.next_repeat = now + REPEAT_EVERY;
        Some(d)
    }

    /// Forget held buttons (device unplugged, events dropped).
    pub fn reset(&mut self) {
        *self = Self::new(self.nintendo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_controller_presses_remote_keys_in_catostvs_own_apps() {
        use crate::uinput::{KEY_ENTER, KEY_ESC, KEY_F2, KEY_UP};
        assert_eq!(key_for(PadOut::Up), Some(KEY_UP));
        assert_eq!(key_for(PadOut::Ok), Some(KEY_ENTER));
        assert_eq!(key_for(PadOut::Back), Some(KEY_ESC));
        assert_eq!(key_for(PadOut::Menu), Some(KEY_F2));
        // Home and the Control Center stay CatosTV's.
        assert_eq!(key_for(PadOut::Home), None);
        assert_eq!(key_for(PadOut::Quick), None);
        assert!(REMOTE_APPS.contains(&"cattube"));
    }

    #[test]
    fn only_guide_acts_over_android() {
        assert_eq!(over_android(btn(BTN_MODE, true)), Some(PadOut::Home));
        assert_eq!(over_android(btn(BTN_MODE, false)), None);
        // A custom Home binding (BTN_WEST on one pad) and every
        // other button or direction stay in the app.
        for code in [
            BTN_SOUTH, BTN_EAST, BTN_NORTH, BTN_WEST, BTN_SELECT, BTN_START,
        ] {
            assert_eq!(over_android(btn(code, true)), None, "{code:#x}");
        }
        assert_eq!(
            over_android(PadEvent::Hat {
                code: ABS_HAT0Y,
                value: -1
            }),
            None
        );
    }

    fn btn(code: u16, pressed: bool) -> PadEvent {
        PadEvent::Button { code, pressed }
    }

    #[test]
    fn only_navigation_events_pass() {
        assert_eq!(pad_event(EV_KEY, BTN_SOUTH, 1), Some(btn(BTN_SOUTH, true)));
        assert_eq!(pad_event(EV_KEY, BTN_SOUTH, 2), None, "autorepeat ignored");
        assert_eq!(pad_event(EV_ABS, 0, 1234), None, "sticks ignored");
        assert_eq!(pad_event(EV_KEY, 30, 1), None, "keyboard keys ignored");
        assert_eq!(
            pad_event(EV_ABS, ABS_HAT0Y, -1),
            Some(PadEvent::Hat {
                code: ABS_HAT0Y,
                value: -1
            })
        );
    }

    #[test]
    fn buttons_map_and_nintendo_swaps_a_b() {
        let mut m = PadMapper::new(false);
        assert_eq!(m.on_event(btn(BTN_SOUTH, true), 0.0), [PadOut::Ok]);
        assert_eq!(m.on_event(btn(BTN_SOUTH, false), 0.1), []);
        assert_eq!(m.on_event(btn(BTN_EAST, true), 0.2), [PadOut::Back]);
        assert_eq!(m.on_event(btn(BTN_START, true), 0.3), [PadOut::Menu]);
        assert_eq!(m.on_event(btn(BTN_SELECT, true), 0.3), [PadOut::Quick]);
        assert_eq!(m.on_event(btn(BTN_MODE, true), 0.4), [PadOut::Home]);
        let mut n = PadMapper::new(true);
        assert_eq!(n.on_event(btn(BTN_SOUTH, true), 0.0), [PadOut::Back]);
        // A custom binding wins over the layout: R1 = Home, and
        // a bound button's release does nothing.
        let mut b = PadMapper::new(false);
        b.bindings = vec![(BTN_TR, PadOut::Home), (BTN_SOUTH, PadOut::Quick)];
        assert_eq!(b.on_event(btn(BTN_TR, true), 0.0), [PadOut::Home]);
        assert_eq!(b.on_event(btn(BTN_TR, false), 0.0), []);
        assert_eq!(b.on_event(btn(BTN_SOUTH, true), 0.0), [PadOut::Quick]);
        assert_eq!(button_name(BTN_TR), "R1");
        assert_eq!(button_name(0x125), "Button 6");
        assert_eq!(PadOut::from_key("quick"), Some(PadOut::Quick));
        assert_eq!(n.on_event(btn(BTN_EAST, true), 0.0), [PadOut::Ok]);
    }

    #[test]
    fn held_direction_repeats_until_released() {
        let mut m = PadMapper::new(false);
        let hat = |v| PadEvent::Hat {
            code: ABS_HAT0X,
            value: v,
        };
        assert_eq!(m.on_event(hat(1), 1.0), [PadOut::Right]);
        assert_eq!(m.tick(1.2), None);
        assert_eq!(m.next_deadline(), Some(1.0 + REPEAT_DELAY));
        assert_eq!(m.tick(1.0 + REPEAT_DELAY), Some(PadOut::Right));
        assert_eq!(m.tick(1.0 + REPEAT_DELAY + 0.05), None);
        assert_eq!(
            m.tick(1.0 + REPEAT_DELAY + REPEAT_EVERY),
            Some(PadOut::Right)
        );
        assert_eq!(m.on_event(hat(0), 2.0), []);
        assert_eq!(m.tick(9.0), None);
        // D-pad buttons (some controllers) work the same way.
        assert_eq!(m.on_event(btn(BTN_DPAD_UP, true), 3.0), [PadOut::Up]);
        assert_eq!(m.on_event(btn(BTN_DPAD_UP, false), 3.1), []);
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn generic_pads_and_sticks() {
        let mut m = PadMapper::new(false);
        assert_eq!(m.on_event(btn(BTN_THUMB2, true), 0.0), [PadOut::Ok]);
        assert_eq!(m.on_event(btn(BTN_THUMB, true), 0.0), [PadOut::Back]);
        assert_eq!(m.on_event(btn(BTN_BASE4, true), 0.0), [PadOut::Menu]);
        assert_eq!(
            pad_event(EV_KEY, BTN_TRIGGER, 1),
            Some(btn(BTN_TRIGGER, true))
        );
        let stick = |code, dir| PadEvent::Stick { code, dir };
        assert_eq!(m.on_event(stick(ABS_Y, -1), 1.0), [PadOut::Up]);
        assert_eq!(m.on_event(stick(ABS_Y, -1), 1.1), []);
        assert_eq!(m.on_event(stick(ABS_Y, 0), 1.2), []);
        assert_eq!(m.next_deadline(), None);
        assert_eq!(stick_dir(255, 0, 255), 1);
        assert_eq!(stick_dir(128, 0, 255), 0);
        assert_eq!(stick_dir(0, 0, 255), -1);
        assert_eq!(stick_dir(-32768, -32768, 32767), -1);
        assert_eq!(stick_dir(5000, -32768, 32767), 0);
        assert_eq!(stick_dir(3, 3, 3), 0);
    }
}
