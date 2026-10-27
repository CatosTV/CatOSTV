//! Interface and session preferences you can change from Settings.
//!
//! Stored as a trivial `key = value` file at `~/.config/catostv/ui.conf`
//! (unknown keys and junk lines are ignored, so a hand edit can never stop
//! the shell from starting). Display preferences (mode, scale, picture fit)
//! live separately in [`crate::display::DisplayPrefs`].

use std::path::PathBuf;

/// Selectable interface accent colours: `(name, label)`. The GUI maps the
/// name to an actual colour (`src/theme.rs`). The first entry is the default.
pub const ACCENTS: &[(&str, &str)] = &[
    ("violet", "Violet"),
    ("periwinkle", "Periwinkle"),
    ("blue", "Blue"),
    ("teal", "Teal"),
    ("green", "Green"),
    ("amber", "Amber"),
    ("rose", "Rose"),
    ("red", "Red"),
    ("silver", "Silver"),
];

pub const DEFAULT_ACCENT: &str = "violet";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    #[default]
    Glassy,
    Flat,
}

impl Theme {
    pub fn from_key(key: &str) -> Self {
        if key == "flat" {
            Self::Flat
        } else {
            Self::Glassy
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Glassy => "glassy",
            Self::Flat => "flat",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Glassy => "Glassy",
            Self::Flat => "Flat",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Glassy => Self::Flat,
            Self::Flat => Self::Glassy,
        }
    }
}

fn parse_code(s: &str) -> Option<u16> {
    let code = match s.strip_prefix("0x") {
        Some(h) => u16::from_str_radix(h, 16).ok()?,
        None => s.parse().ok()?,
    };
    crate::gamepad::is_button(code).then_some(code)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiPrefs {
    pub theme: Theme,
    /// One of [`ACCENTS`] (names). Unknown values fall back to the default.
    pub accent: String,
    /// The one-time key guide has been shown on this TV.
    pub key_guide_seen: bool,
    /// Clock shows 24-hour time (`false` → 12-hour with AM/PM).
    pub clock_24h: bool,
    /// Offer this TV as a DLNA / YouTube DIAL receiver on the LAN.
    pub cast_enabled: bool,
    /// HDR output (experimental). Not persisted across reboots: the shell
    /// derives it from [`hdr_flag_path`] at start, and `catostv-session`
    /// starts labwc with the Vulkan renderer and `<hdr>yes</hdr>` only
    /// while that runtime flag exists.
    pub hdr: bool,
    /// Game controllers navigate CatosTV (read by `catostv-inputd`).
    pub pad_nav: bool,
    /// Controller layout: the right face button is OK (Nintendo style).
    /// Default for controllers without their own choice.
    pub pad_nintendo: bool,
    /// Controllers (by device name) set to the Nintendo layout in
    /// Settings → Navigation & controllers (per controller).
    pub pad_nintendo_devices: Vec<String>,
    /// Controller navigation bindings (`PadOut::key()`, button code).
    pub pad_bindings: Vec<(String, u16)>,
    /// Android image flavour for the setup: "gapps" (Google Play) or
    /// "vanilla" (Aurora Store). Read by the root setup script.
    pub android_variant: String,
    /// Catfyn plays the next episode when the end credits start.
    pub fin_autoplay: bool,
    /// Seconds the "Next episode" card counts down: 5, 10 or 15.
    pub fin_countdown: u32,
    /// The screensaver starts after a while without input, and Sleep
    /// shows it instead of putting the TV to sleep.
    pub screensaver: bool,
    /// "cats" or "photos" (`screensaver::Pictures`).
    pub saver_pictures: String,
    /// Minutes without input, one of `screensaver::MINUTES`.
    pub saver_minutes: u32,
    /// Home's apps in the order the user put them (app ids); apps not
    /// listed follow in their usual order.
    pub app_order: Vec<String>,
    /// Behind the interface: one of [`HOME_BACKGROUNDS`] (keys).
    pub home_background: String,
    /// Mouse mode chosen per app (app id, on) in the Control Center; apps
    /// not listed follow `cursor::starts_on`.
    pub mouse_apps: Vec<(String, bool)>,
}

/// Settings → Customization → Home background: key and label. "glow" is
/// the drifting light; the others are softly blurred pictures from the
/// screensaver's collections. Cats by default.
pub const HOME_BACKGROUNDS: [(&str, &str); 3] = [
    ("glow", "Glow"),
    ("cats", "NOICE CATS"),
    ("photos", "Random photos"),
];
pub const DEFAULT_HOME_BACKGROUND: &str = "cats";

/// The label of a [`HOME_BACKGROUNDS`] key (unknown → Glow).
pub fn home_background_label(key: &str) -> &'static str {
    HOME_BACKGROUNDS
        .iter()
        .find(|(k, _)| *k == key)
        .map_or(HOME_BACKGROUNDS[0].1, |(_, l)| l)
}

/// The next Home background in `delta`'s direction (wraps).
pub fn cycle_home_background(key: &str, delta: i32) -> &'static str {
    let n = HOME_BACKGROUNDS.len() as i32;
    let i = HOME_BACKGROUNDS
        .iter()
        .position(|(k, _)| *k == key)
        .unwrap_or(0) as i32;
    HOME_BACKGROUNDS[(i + delta).rem_euclid(n) as usize].0
}

/// The countdown lengths Settings → Catfyn offers.
pub const FIN_COUNTDOWNS: [u32; 3] = [5, 10, 15];

/// The next countdown length in `delta`'s direction (no wrap).
pub fn cycle_countdown(current: u32, delta: i32) -> u32 {
    let i = FIN_COUNTDOWNS
        .iter()
        .position(|&c| c == current)
        .unwrap_or(1) as i32;
    FIN_COUNTDOWNS[(i + delta).clamp(0, FIN_COUNTDOWNS.len() as i32 - 1) as usize]
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            accent: DEFAULT_ACCENT.into(),
            key_guide_seen: false,
            clock_24h: true,
            cast_enabled: true,
            hdr: false,
            pad_nav: true,
            pad_nintendo: false,
            pad_nintendo_devices: Vec::new(),
            pad_bindings: Vec::new(),
            android_variant: "gapps".into(),
            fin_autoplay: true,
            fin_countdown: 10,
            screensaver: true,
            saver_pictures: "cats".into(),
            saver_minutes: crate::screensaver::DEFAULT_MINUTES,
            app_order: Vec::new(),
            home_background: DEFAULT_HOME_BACKGROUND.into(),
            mouse_apps: Vec::new(),
        }
    }
}

/// Index of `name` in [`ACCENTS`] (0 when unknown).
pub fn accent_index(name: &str) -> usize {
    ACCENTS.iter().position(|(n, _)| *n == name).unwrap_or(0)
}

/// The accent `delta` steps away from `name`, wrapping around.
pub fn cycle_accent(name: &str, delta: i32) -> &'static str {
    let n = ACCENTS.len() as i32;
    let i = (accent_index(name) as i32 + delta).rem_euclid(n);
    ACCENTS[i as usize].0
}

pub fn accent_label(name: &str) -> &'static str {
    ACCENTS[accent_index(name)].1
}

impl UiPrefs {
    /// Does controller `name` use the Nintendo layout?
    pub fn nintendo_for(&self, name: &str) -> bool {
        self.pad_nintendo || self.pad_nintendo_devices.iter().any(|d| d == name)
    }

    /// Set controller `name`'s layout.
    /// The button bound to `action` ("ok", "back", …), if any.
    pub fn pad_binding(&self, action: &str) -> Option<u16> {
        self.pad_bindings
            .iter()
            .find(|(a, _)| a == action)
            .map(|(_, c)| *c)
    }

    /// Bind `code` to `action`; a button bound elsewhere moves.
    pub fn set_pad_binding(&mut self, action: &str, code: u16) {
        self.pad_bindings.retain(|(a, c)| a != action && *c != code);
        self.pad_bindings.push((action.to_string(), code));
    }

    pub fn clear_pad_bindings(&mut self) {
        self.pad_bindings.clear();
    }

    /// Bindings as the mapper wants them.
    pub fn pad_mapper_bindings(&self) -> Vec<(u16, crate::gamepad::PadOut)> {
        self.pad_bindings
            .iter()
            .filter_map(|(a, c)| crate::gamepad::PadOut::from_key(a).map(|o| (*c, o)))
            .collect()
    }

    /// Mouse mode as chosen for app `id`, if it was.
    pub fn mouse_choice(&self, id: &str) -> Option<bool> {
        self.mouse_apps
            .iter()
            .find(|(a, _)| a == id)
            .map(|(_, on)| *on)
    }

    pub fn set_mouse_choice(&mut self, id: &str, on: bool) {
        self.mouse_apps.retain(|(a, _)| a != id);
        self.mouse_apps.push((id.to_string(), on));
    }

    pub fn set_nintendo_for(&mut self, name: &str, on: bool) {
        self.pad_nintendo_devices.retain(|d| d != name);
        if on {
            self.pad_nintendo_devices.push(name.to_string());
        }
    }

    pub fn from_conf(text: &str) -> Self {
        let mut p = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim().trim_matches('"');
            match k.trim() {
                "theme" => p.theme = Theme::from_key(v),
                "accent" if ACCENTS.iter().any(|(n, _)| *n == v) => p.accent = v.into(),
                "key_guide_seen" => p.key_guide_seen = v == "true",
                "clock_24h" => p.clock_24h = v != "false",
                "cast_enabled" => p.cast_enabled = v != "false",
                "hdr" => p.hdr = v == "true",
                "pad_nav" => p.pad_nav = v != "false",
                "pad_nintendo" => p.pad_nintendo = v == "true",
                "pad_nintendo_device" if !v.is_empty() && v.len() <= 128 => {
                    p.pad_nintendo_devices.push(v.to_string())
                }
                "app" if !v.is_empty() && v.len() <= 256 => p.app_order.push(v.to_string()),
                "mouse_app" => {
                    if let Some((id, on)) = v.rsplit_once(' ')
                        && !id.trim().is_empty()
                        && id.len() <= 256
                        && (on == "on" || on == "off")
                    {
                        p.set_mouse_choice(id.trim(), on == "on");
                    }
                }
                "home_background" if HOME_BACKGROUNDS.iter().any(|(k, _)| *k == v) => {
                    p.home_background = v.to_string()
                }
                "fin_autoplay" => p.fin_autoplay = v != "false",
                "fin_countdown" => {
                    if let Ok(n) = v.parse::<u32>()
                        && FIN_COUNTDOWNS.contains(&n)
                    {
                        p.fin_countdown = n;
                    }
                }
                "screensaver" => p.screensaver = v != "false",
                "screensaver_pictures" if v == "cats" || v == "photos" => {
                    p.saver_pictures = v.to_string()
                }
                "screensaver_minutes" => {
                    if let Ok(n) = v.parse::<u32>()
                        && crate::screensaver::MINUTES.contains(&n)
                    {
                        p.saver_minutes = n;
                    }
                }
                "android_variant" if v == "gapps" || v == "vanilla" => {
                    p.android_variant = v.to_string()
                }
                "pad_bind" => {
                    let mut it = v.split_whitespace();
                    if let (Some(action), Some(code)) = (it.next(), it.next())
                        && crate::gamepad::PadOut::from_key(action).is_some()
                        && let Some(code) = parse_code(code)
                    {
                        p.set_pad_binding(action, code);
                    }
                }
                _ => {}
            }
        }
        p
    }

    pub fn to_conf(&self) -> String {
        let mut s = format!(
            "# CatosTV interface preferences (written by the shell)\naccent = {}\nkey_guide_seen = {}\nclock_24h = {}\nhdr = {}\npad_nav = {}\npad_nintendo = {}\n",
            self.accent,
            self.key_guide_seen,
            self.clock_24h,
            self.hdr,
            self.pad_nav,
            self.pad_nintendo
        );
        s.push_str(&format!("android_variant = {}\n", self.android_variant));
        s.push_str(&format!("theme = {}\n", self.theme.key()));
        s.push_str(&format!("cast_enabled = {}\n", self.cast_enabled));
        s.push_str(&format!("home_background = {}\n", self.home_background));
        s.push_str(&format!(
            "fin_autoplay = {}\nfin_countdown = {}\n",
            self.fin_autoplay, self.fin_countdown
        ));
        s.push_str(&format!(
            "screensaver = {}\nscreensaver_pictures = {}\nscreensaver_minutes = {}\n",
            self.screensaver, self.saver_pictures, self.saver_minutes
        ));
        for (a, c) in &self.pad_bindings {
            s.push_str(&format!("pad_bind = {a} 0x{c:x}\n"));
        }
        for d in &self.pad_nintendo_devices {
            // One line per device; names never contain newlines (sysfs).
            let d: String = d.chars().filter(|c| !c.is_control()).collect();
            s.push_str(&format!("pad_nintendo_device = {d}\n"));
        }
        for id in &self.app_order {
            let id: String = id.chars().filter(|c| !c.is_control()).collect();
            s.push_str(&format!("app = {id}\n"));
        }
        for (id, on) in &self.mouse_apps {
            let id: String = id.chars().filter(|c| !c.is_control()).collect();
            s.push_str(&format!(
                "mouse_app = {id} {}\n",
                if *on { "on" } else { "off" }
            ));
        }
        s
    }
}

/// `$XDG_RUNTIME_DIR/catostv-hdr`: present = start the compositor in HDR.
/// It lives in the runtime directory on purpose, so HDR never survives a
/// reboot: a TV that shows black in HDR always comes back after a restart
/// `catostv-session` reads and removes it.
pub fn hdr_flag_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("catostv-hdr")
}

/// `~/.config/catostv/ui.conf`
pub fn path() -> PathBuf {
    crate::env::apps_config_path()
        .parent()
        .map(|p| p.join("ui.conf"))
        .unwrap_or_else(|| PathBuf::from("/home/catostv/.config/catostv/ui.conf"))
}

pub fn load() -> UiPrefs {
    std::fs::read_to_string(path())
        .map(|t| UiPrefs::from_conf(&t))
        .unwrap_or_default()
}

/// Atomic save (write a temp file, then rename).
pub fn save(p: &UiPrefs) -> std::io::Result<()> {
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("conf.tmp");
    std::fs::write(&tmp, p.to_conf())?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_choice_per_app() {
        let mut p = UiPrefs::default();
        assert_eq!(p.mouse_choice("org.videolan.VLC"), None);
        p.set_mouse_choice("org.videolan.VLC", false);
        p.set_mouse_choice("tv.kodi.Kodi", true);
        p.set_mouse_choice("org.videolan.VLC", true);
        let back = UiPrefs::from_conf(&p.to_conf());
        assert_eq!(back.mouse_choice("org.videolan.VLC"), Some(true));
        assert_eq!(back.mouse_choice("tv.kodi.Kodi"), Some(true));
        assert_eq!(back.mouse_apps.len(), 2);
        let junk = UiPrefs::from_conf(
            "mouse_app = x maybe\nmouse_app = \nmouse_app = on\nmouse_app = a.b off\n",
        );
        assert_eq!(junk.mouse_apps, [("a.b".to_string(), false)]);
    }

    #[test]
    fn round_trip_and_junk() {
        assert_eq!(UiPrefs::from_conf("accent = teal\n").theme, Theme::Glassy);
        assert_eq!(UiPrefs::from_conf("theme = unknown\n").theme, Theme::Glassy);
        assert!(UiPrefs::from_conf("").cast_enabled);
        assert!(UiPrefs::from_conf("cast_enabled = junk\n").cast_enabled);
        let p = UiPrefs {
            theme: Theme::Flat,
            accent: "teal".into(),
            key_guide_seen: true,
            clock_24h: false,
            cast_enabled: false,
            hdr: true,
            pad_nav: false,
            pad_nintendo: true,
            pad_nintendo_devices: vec!["8BitDo Pro 2".into()],
            pad_bindings: vec![("ok".into(), 0x131), ("home".into(), 0x13c)],
            android_variant: "vanilla".into(),
            fin_autoplay: false,
            fin_countdown: 15,
            screensaver: false,
            saver_pictures: "photos".into(),
            saver_minutes: 10,
            app_order: vec!["catfyn".into(), "com.example.Tv".into()],
            home_background: "photos".into(),
            mouse_apps: vec![("io.freetubeapp.FreeTube".into(), true)],
        };
        assert_eq!(UiPrefs::from_conf(&p.to_conf()), p);
        // Home background: cats by default, unknown values ignored, wraps.
        assert_eq!(UiPrefs::default().home_background, "cats");
        assert_eq!(
            UiPrefs::from_conf("home_background = dogs\n").home_background,
            "cats"
        );
        assert_eq!(cycle_home_background("glow", -1), "photos");
        assert_eq!(cycle_home_background("photos", 1), "glow");
        assert_eq!(home_background_label("cats"), "NOICE CATS");
        // Catfyn autoplay: on, 10 s by default; only 5/10/15.
        let d = UiPrefs::default();
        assert!(d.fin_autoplay);
        assert_eq!(d.fin_countdown, 10);
        assert_eq!(UiPrefs::from_conf("fin_countdown = 7\n").fin_countdown, 10);
        // Screensaver: on, cats, 3 minutes by default.
        assert!(d.screensaver);
        assert_eq!((d.saver_pictures.as_str(), d.saver_minutes), ("cats", 3));
        let junk = UiPrefs::from_conf("screensaver_minutes = 2\nscreensaver_pictures = dogs\n");
        assert_eq!(
            (junk.saver_pictures.as_str(), junk.saver_minutes),
            ("cats", 3)
        );
        assert_eq!(cycle_countdown(10, 1), 15);
        assert_eq!(cycle_countdown(15, 1), 15);
        assert_eq!(cycle_countdown(10, -1), 5);
        assert_eq!(cycle_countdown(5, -1), 5);
        // A button bound to a second action moves there.
        let mut b = UiPrefs::default();
        b.set_pad_binding("ok", 0x131);
        b.set_pad_binding("back", 0x131);
        assert_eq!(b.pad_binding("ok"), None);
        assert_eq!(b.pad_binding("back"), Some(0x131));
        assert_eq!(
            b.pad_mapper_bindings(),
            vec![(0x131, crate::gamepad::PadOut::Back)]
        );
        // Junk bindings are dropped.
        let j =
            UiPrefs::from_conf("pad_bind = fly 0x131\npad_bind = ok zzz\npad_bind = ok 0x9999\n");
        assert!(j.pad_bindings.is_empty());
        let mut q = UiPrefs::default();
        q.set_nintendo_for("Switch Pro", true);
        assert!(q.nintendo_for("Switch Pro") && !q.nintendo_for("Xbox"));
        q.set_nintendo_for("Switch Pro", false);
        assert!(!q.nintendo_for("Switch Pro"));
        let junk = UiPrefs::from_conf("accent = neon\n???\nkey_guide_seen=maybe\n");
        assert_eq!(junk, UiPrefs::default());
    }

    #[test]
    fn cycling_wraps() {
        assert_eq!(cycle_accent("violet", 1), "periwinkle");
        assert_eq!(cycle_accent("violet", -1), "silver");
        assert_eq!(cycle_accent("unknown", 1), "periwinkle");
        assert_eq!(accent_label("rose"), "Rose");
    }
}
