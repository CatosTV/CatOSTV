//! Mouse mode: the remote (or a game controller) moves a mouse pointer, for
//! desktop apps made for a mouse.
//!
//! Who does what:
//!
//! * The shell decides when mouse mode is on: for the Linux app in front,
//!   by the user's choice for that app (Control Center → Mouse), else by
//!   [`starts_on`]. It writes `on` / `off` to [`state_path`]; over CatosTV
//!   itself and over Android apps it is always off.
//! * `catostv-inputd` follows that file. While it says `on` it holds the
//!   keyboards and remotes it watches (`EVIOCGRAB`), turns the arrows, OK
//!   and Back into the pointer ([`role`]), and passes every other key on
//!   through its own keyboard, so Home, volume and typing still work. Back
//!   turns mouse mode off (it writes `off` itself) and lets go of them.
//!   Typing a letter does the same, so a real keyboard is never stuck.
//! * Controllers are read as always (never held): the left stick and the
//!   D-pad move the pointer, A clicks, X is the right button, the right
//!   stick scrolls, B turns mouse mode off.
//!
//! The pointer moves at a speed that grows while a direction is held
//! ([`Motion`]); a short tap moves it a few pixels, for small targets.
//! OK clicks when released; held for [`RIGHT_CLICK_SECS`] it is the right
//! button instead ([`Clicker`]).
//!
//! The pure parts here are tested; the device side is in `uinput`
//! (the virtual mouse) and `catostv-inputd`.

use std::path::PathBuf;

/// `$XDG_RUNTIME_DIR/catostv-pointer`: `on` or `off`.
pub fn state_path() -> PathBuf {
    crate::env::runtime_dir().join("catostv-pointer")
}

pub fn is_on() -> bool {
    std::fs::read_to_string(state_path()).is_ok_and(|s| s.trim() == "on")
}

/// Write the state (atomically: inputd may be reading it).
pub fn set_on(on: bool) {
    let path = state_path();
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, if on { "on\n" } else { "off\n" }).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Names of the devices mouse mode makes. `catostv-inputd` never reads
/// its own devices (it would read back what it sent).
pub const MOUSE_NAME: &str = "CatosTV mouse";
pub const KEYS_NAME: &str = "CatosTV keys";

pub fn is_own_device(name: &str) -> bool {
    name == MOUSE_NAME || name == KEYS_NAME
}

// ----------------------------------------------------------- which apps

/// Does mouse mode start on for this app when the user has not chosen?
/// On for Linux desktop apps from the Store, off for apps that work with a
/// remote or controller (the Store's list says which), for games, and for
/// everything that is not a Flatpak (CatosTV's own apps, Android).
pub fn starts_on(app: &crate::apps::AppEntry) -> bool {
    let crate::apps::AppSource::Flatpak { app_id } = &app.source else {
        return false;
    };
    if crate::flatpak::CURATED.iter().any(|c| c.id == app_id) {
        return crate::flatpak::NEEDS_MOUSE.contains(&app_id.as_str());
    }
    !desktop_categories(app_id).iter().any(|c| c == "Game")
}

/// The `Categories` of an installed Flatpak's desktop file (user, then
/// system installation).
fn desktop_categories(app_id: &str) -> Vec<String> {
    if app_id.contains('/') || app_id.starts_with('.') {
        return Vec::new();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    [
        home.join(".local/share/flatpak/exports/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ]
    .iter()
    .find_map(|dir| std::fs::read_to_string(dir.join(format!("{app_id}.desktop"))).ok())
    .map(|text| categories_of(&text))
    .unwrap_or_default()
}

/// `Categories=AudioVideo;Game;` in the main section → ["AudioVideo", "Game"].
pub fn categories_of(desktop_file: &str) -> Vec<String> {
    let mut main = false;
    for line in desktop_file.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            main = line == "[Desktop Entry]";
            continue;
        }
        if main && let Some(v) = line.strip_prefix("Categories=") {
            return v
                .split(';')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(str::to_string)
                .collect();
        }
    }
    Vec::new()
}

// ----------------------------------------------------------- keys

/// What a key does while mouse mode is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Move(Dir),
    /// OK: click on release, right button when held.
    Click,
    /// The options key (F2, Menu): right button.
    RightClick,
    /// Channel / page keys: scroll up (`1`) or down (`-1`).
    Scroll(i32),
    /// Back / Exit: mouse mode off.
    Leave,
    /// A letter or digit: mouse mode off, and the key goes on.
    Typing,
    /// Everything else goes on to the app unchanged.
    Pass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

pub const KEY_ESC: u16 = 1;
pub const KEY_ENTER: u16 = 28;
pub const KEY_F2: u16 = 60;
pub const KEY_KPENTER: u16 = 96;
pub const KEY_UP: u16 = 103;
pub const KEY_PAGEUP: u16 = 104;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_DOWN: u16 = 108;
pub const KEY_PAGEDOWN: u16 = 109;
pub const KEY_COMPOSE: u16 = 127;
pub const KEY_MENU: u16 = 139;
pub const KEY_BACK: u16 = 158;
pub const KEY_EXIT: u16 = 174;
pub const KEY_OK: u16 = 0x160;
pub const KEY_SELECT: u16 = 0x161;
pub const KEY_CHANNELUP: u16 = 0x192;
pub const KEY_CHANNELDOWN: u16 = 0x193;

pub fn role(code: u16) -> Role {
    match code {
        KEY_UP => Role::Move(Dir::Up),
        KEY_DOWN => Role::Move(Dir::Down),
        KEY_LEFT => Role::Move(Dir::Left),
        KEY_RIGHT => Role::Move(Dir::Right),
        KEY_ENTER | KEY_KPENTER | KEY_OK | KEY_SELECT => Role::Click,
        KEY_F2 | KEY_MENU | KEY_COMPOSE => Role::RightClick,
        KEY_PAGEUP | KEY_CHANNELUP => Role::Scroll(1),
        KEY_PAGEDOWN | KEY_CHANNELDOWN => Role::Scroll(-1),
        KEY_ESC | KEY_BACK | KEY_EXIT => Role::Leave,
        c if crate::evdev::classify(c) == crate::gesture::KeyClass::Text => Role::Typing,
        _ => Role::Pass,
    }
}

// ----------------------------------------------------------- motion

/// A tap moves this far (pixels), at once.
pub const NUDGE: f64 = 3.0;
/// Held longer than this, the pointer starts gliding.
pub const GLIDE_AFTER: f64 = 0.16;
/// Gliding speed (pixels per second): starts slow, reaches the top speed
/// after [`RAMP_SECS`] of holding.
pub const START_SPEED: f64 = 200.0;
pub const TOP_SPEED: f64 = 1100.0;
pub const RAMP_SECS: f64 = 1.2;

/// Taps shorter than this are a remote sending one tap per repeat (the TV
/// remote over HDMI, the phone): a run of them counts as holding the key.
pub const SYNTHETIC_TAP: f64 = 0.04;
/// Such taps further apart than this start a new run.
pub const STREAK_GAP: f64 = 0.6;

#[derive(Debug, Clone, Copy, Default)]
struct Held {
    /// The key is down (since when).
    down: Option<f64>,
    /// Gliding counts from here (the first press of a run of taps).
    origin: f64,
    last_press: f64,
    /// The last press was a remote's short tap.
    tap: bool,
    /// Time between the last two taps of a run (0: no run yet).
    interval: f64,
}

/// The pointer's movement from held directions (remote arrows or a
/// controller's D-pad), in pixels, with sub-pixel remainders kept.
#[derive(Debug, Clone, Default)]
pub struct Motion {
    dirs: [Held; 4],
    last: Option<f64>,
    rest: (f64, f64),
}

fn index(d: Dir) -> usize {
    match d {
        Dir::Up => 0,
        Dir::Down => 1,
        Dir::Left => 2,
        Dir::Right => 3,
    }
}

fn unit(d: Dir) -> (f64, f64) {
    match d {
        Dir::Up => (0.0, -1.0),
        Dir::Down => (0.0, 1.0),
        Dir::Left => (-1.0, 0.0),
        Dir::Right => (1.0, 0.0),
    }
}

const DIRS: [Dir; 4] = [Dir::Up, Dir::Down, Dir::Left, Dir::Right];

impl Held {
    /// Where gliding counts from, if this direction moves the pointer now:
    /// the key is down, or a run of remote taps is still coming.
    fn origin(&self, now: f64) -> Option<f64> {
        if self.down.is_some() {
            return Some(self.origin);
        }
        let wait = (self.interval * 1.6).clamp(0.12, STREAK_GAP);
        (self.interval > 0.0 && now < self.last_press + wait).then_some(self.origin)
    }
}

impl Motion {
    /// A direction went down: the nudge, in whole pixels.
    pub fn press(&mut self, d: Dir, now: f64) -> (i32, i32) {
        let h = &mut self.dirs[index(d)];
        if h.down.is_some() {
            return (0, 0); // auto-repeat of a held key
        }
        let run = h.tap && now - h.last_press <= STREAK_GAP;
        h.down = Some(now);
        if run {
            h.interval = now - h.last_press;
        } else {
            h.origin = now;
            h.interval = 0.0;
        }
        h.last_press = now;
        if self.last.is_none() {
            self.last = Some(now);
        }
        if run {
            return (0, 0);
        }
        let (x, y) = unit(d);
        ((x * NUDGE) as i32, (y * NUDGE) as i32)
    }

    pub fn release(&mut self, d: Dir, now: f64) {
        let h = &mut self.dirs[index(d)];
        if let Some(at) = h.down.take() {
            h.tap = now - at < SYNTHETIC_TAP;
            if !h.tap {
                h.interval = 0.0;
            }
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn moving(&self, now: f64) -> bool {
        self.dirs.iter().any(|h| h.origin(now).is_some())
    }

    /// Whole pixels to move since the last call.
    pub fn step(&mut self, now: f64) -> (i32, i32) {
        let Some(last) = self.last else {
            return (0, 0);
        };
        let dt = (now - last).clamp(0.0, 0.1);
        self.last = Some(now);
        let (mut vx, mut vy) = (0.0, 0.0);
        for d in DIRS {
            let Some(origin) = self.dirs[index(d)].origin(now) else {
                continue;
            };
            let held = now - origin - GLIDE_AFTER;
            if held <= 0.0 {
                continue;
            }
            let speed = START_SPEED + (TOP_SPEED - START_SPEED) * (held / RAMP_SECS).min(1.0);
            let (x, y) = unit(d);
            vx += x * speed;
            vy += y * speed;
        }
        if !self.moving(now) {
            self.last = None;
            self.rest = (0.0, 0.0);
            return (0, 0);
        }
        let x = self.rest.0 + vx * dt;
        let y = self.rest.1 + vy * dt;
        let (ix, iy) = (x.trunc(), y.trunc());
        self.rest = (x - ix, y - iy);
        (ix as i32, iy as i32)
    }
}

/// Stick dead zone and top speed (pixels per second, stick all the way).
pub const STICK_DEAD: f64 = 0.18;
pub const STICK_SPEED: f64 = 1300.0;

/// Pointer velocity for a stick at (`x`, `y`), each -1..=1: nothing inside
/// the dead zone, then growing with the square of the push, so small pushes
/// are precise and a full push crosses the screen in about a second.
pub fn stick_velocity(x: f64, y: f64) -> (f64, f64) {
    let r = (x * x + y * y).sqrt();
    if r <= STICK_DEAD {
        return (0.0, 0.0);
    }
    let k = ((r.min(1.0) - STICK_DEAD) / (1.0 - STICK_DEAD)).powi(2) * STICK_SPEED / r;
    (x * k, y * k)
}

/// A raw stick value as -1..=1 for the axis range `min..=max`.
pub fn normalize(value: i32, min: i32, max: i32) -> f64 {
    if max <= min {
        return 0.0;
    }
    let half = (max as f64 - min as f64) / 2.0;
    ((value as f64 - (min as f64 + half)) / half).clamp(-1.0, 1.0)
}

// ----------------------------------------------------------- OK

/// Held this long, OK is the right button.
pub const RIGHT_CLICK_SECS: f64 = 0.55;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    Left,
    Right,
}

/// OK as a mouse button: a press shorter than [`RIGHT_CLICK_SECS`] clicks
/// on release; a longer one is the right button, once, when the time is up.
#[derive(Debug, Clone, Default)]
pub struct Clicker {
    down_at: Option<f64>,
    fired: bool,
}

impl Clicker {
    pub fn press(&mut self, now: f64) {
        if self.down_at.is_none() {
            self.down_at = Some(now);
            self.fired = false;
        }
    }

    pub fn release(&mut self, now: f64) -> Option<Click> {
        let at = self.down_at.take()?;
        if self.fired || now - at >= RIGHT_CLICK_SECS {
            let late = !self.fired;
            self.fired = false;
            return late.then_some(Click::Right);
        }
        Some(Click::Left)
    }

    pub fn tick(&mut self, now: f64) -> Option<Click> {
        let at = self.down_at?;
        if !self.fired && now - at >= RIGHT_CLICK_SECS {
            self.fired = true;
            return Some(Click::Right);
        }
        None
    }

    /// When [`Clicker::tick`] has something to do.
    pub fn deadline(&self) -> Option<f64> {
        match self.down_at {
            Some(at) if !self.fired => Some(at + RIGHT_CLICK_SECS),
            _ => None,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::{AppEntry, AppSource};

    fn entry(source: AppSource) -> AppEntry {
        AppEntry {
            id: "x".into(),
            name: "X".into(),
            subtitle: String::new(),
            icon: "play".into(),
            accent: "violet".into(),
            command: "x".into(),
            singleton: true,
            icon_path: None,
            source,
        }
    }

    fn flatpak(id: &str) -> AppEntry {
        entry(AppSource::Flatpak {
            app_id: id.to_string(),
        })
    }

    #[test]
    fn which_apps_start_with_the_mouse() {
        // Desktop apps from the Store: on.
        assert!(starts_on(&flatpak("io.freetubeapp.FreeTube")));
        assert!(starts_on(&flatpak("com.spotify.Client")));
        // Made for a remote or a controller: off.
        assert!(!starts_on(&flatpak("tv.kodi.Kodi")));
        assert!(!starts_on(&flatpak("com.valvesoftware.Steam")));
        assert!(!starts_on(&flatpak("com.moonlight_stream.Moonlight")));
        // Not a Flatpak: off.
        assert!(!starts_on(&entry(AppSource::Configured)));
        assert!(!starts_on(&entry(AppSource::Android {
            package: "org.smarttube.stable".into(),
        })));
    }

    #[test]
    fn desktop_categories() {
        let f = "[Desktop Entry]\nName=X\nCategories=Game;ArcadeGame;\n[Desktop Action new]\nCategories=Office;\n";
        assert_eq!(categories_of(f), ["Game", "ArcadeGame"]);
        assert!(categories_of("[Desktop Action a]\nCategories=Game;\n").is_empty());
        assert!(super::desktop_categories("../etc/passwd").is_empty());
    }

    #[test]
    fn keys_in_mouse_mode() {
        assert_eq!(role(KEY_UP), Role::Move(Dir::Up));
        assert_eq!(role(KEY_RIGHT), Role::Move(Dir::Right));
        assert_eq!(role(KEY_ENTER), Role::Click);
        assert_eq!(role(KEY_SELECT), Role::Click);
        assert_eq!(role(KEY_F2), Role::RightClick);
        assert_eq!(role(KEY_ESC), Role::Leave);
        assert_eq!(role(KEY_BACK), Role::Leave);
        assert_eq!(role(KEY_CHANNELDOWN), Role::Scroll(-1));
        // Letters end mouse mode; Home, volume and play go on to the
        // compositor and the app.
        assert_eq!(role(30), Role::Typing);
        for k in [172, 115, 114, 113, 164, 57, 14] {
            assert_eq!(role(k), Role::Pass, "{k}");
        }
    }

    #[test]
    fn a_tap_nudges_and_holding_glides_faster_and_faster() {
        let mut m = Motion::default();
        assert_eq!(m.press(Dir::Right, 0.0), (3, 0));
        // Auto-repeat of the same key: nothing more.
        assert_eq!(m.press(Dir::Right, 0.05), (0, 0));
        // Before the glide starts: no movement.
        assert_eq!(m.step(0.1), (0, 0));
        let mut x = 0;
        let mut t = 0.1;
        while t < 0.5 {
            t += 0.008;
            x += m.step(t).0;
        }
        let slow = x;
        assert!(slow > 50 && slow < 200, "{slow}");
        let mut fast = 0;
        while t < 2.0 {
            t += 0.008;
            fast += m.step(t).0;
        }
        // The last half second is at top speed.
        let mut last = 0;
        while t < 2.5 {
            t += 0.008;
            last += m.step(t).0;
        }
        assert!((520..=580).contains(&last), "{last}");
        assert!(fast > slow);
        m.release(Dir::Right, 2.5);
        assert!(!m.moving(2.5));
        assert_eq!(m.step(3.0), (0, 0));
        // Up goes up.
        m.press(Dir::Up, 4.0);
        assert!(m.step(4.1).1 == 0);
        assert!(m.step(4.5).1 < 0);
    }

    #[test]
    fn a_remote_that_taps_on_every_repeat_glides_too() {
        // The TV remote over HDMI and the phone send 12 ms taps, one per
        // repeat, about every 0.15 s while the button is held.
        let mut m = Motion::default();
        let mut x = 0;
        let mut t = 0.0;
        let mut next_tap = 0.0;
        while t < 1.5 {
            if t >= next_tap {
                x += m.press(Dir::Left, t).0;
                m.release(Dir::Left, t + 0.012);
                next_tap += 0.15;
            }
            t += 0.008;
            x += m.step(t).0;
        }
        assert!(x < -400, "{x}");
        // Let go: it stops soon (within a quarter of a second).
        let mut after = 0;
        while t < 1.75 {
            t += 0.008;
            after += m.step(t).0;
        }
        assert!(!m.moving(t));
        assert!(after > -320, "{after}");
        // Two separate presses of a real remote (longer than a tap) only
        // nudge twice.
        let mut m = Motion::default();
        assert_eq!(m.press(Dir::Down, 10.0), (0, 3));
        m.release(Dir::Down, 10.1);
        assert_eq!(m.press(Dir::Down, 10.3), (0, 3));
        m.release(Dir::Down, 10.4);
        assert!(!m.moving(10.45));
        // One tap of the phone is one nudge, no glide.
        let mut m = Motion::default();
        assert_eq!(m.press(Dir::Up, 20.0), (0, -3));
        m.release(Dir::Up, 20.012);
        assert!(!m.moving(20.02));
    }

    #[test]
    fn stick() {
        assert_eq!(stick_velocity(0.1, -0.1), (0.0, 0.0));
        let (vx, vy) = stick_velocity(1.0, 0.0);
        assert!((vx - STICK_SPEED).abs() < 1e-6 && vy == 0.0);
        let (half, _) = stick_velocity(0.6, 0.0);
        assert!(half > 0.0 && half < STICK_SPEED / 3.0);
        assert!(normalize(0, -32768, 32767).abs() < 1e-4);
        assert_eq!(normalize(255, 0, 255), 1.0);
        assert_eq!(normalize(0, 0, 255), -1.0);
        assert_eq!(normalize(5, 5, 5), 0.0);
    }

    #[test]
    fn ok_clicks_on_release_and_holding_is_the_right_button() {
        let mut c = Clicker::default();
        c.press(1.0);
        assert_eq!(c.tick(1.2), None);
        assert_eq!(c.release(1.3), Some(Click::Left));
        c.press(2.0);
        assert_eq!(c.deadline(), Some(2.0 + RIGHT_CLICK_SECS));
        assert_eq!(c.tick(2.6), Some(Click::Right));
        assert_eq!(c.tick(2.8), None);
        assert_eq!(c.deadline(), None);
        // Letting go after the right button: nothing more.
        assert_eq!(c.release(3.0), None);
        // A release without a press is ignored.
        assert_eq!(c.release(4.0), None);
    }
}
