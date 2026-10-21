//! A virtual keyboard (`/dev/uinput`) for the TV remote (CEC) and the phone
//! remote, and a virtual mouse for mouse mode ([`crate::cursor`]) and the
//! phone's touchpad.
//!
//! Why not `wtype`: wtype uploads its own keymap with made-up key codes.
//! GTK/Qt apps translate those through the keymap and are fine, but
//! Android (Waydroid) reads the raw key code, so remote keys did nothing
//! inside Android apps, and Back was "hit or miss".
//! A uinput device is a real keyboard to the compositor: standard evdev
//! codes, the compositor's normal keymap, and labwc's key bindings apply.
//!
//! While an Android app is in front ([`front`] = `android`), Back is sent as
//! `KEY_BACK`, what an Android TV remote sends, instead of Escape. OK
//! stays Enter: the Android TV image maps key 28 to DPAD_CENTER itself,
//! and Waydroid's keyboard pipe into Android drops high key codes such as
//! KEY_SELECT (353), which was the OK key before ("arrows work in
//! SmartTube, OK does not").
//!
//! Access: `/dev/uinput` is group `input` (udev rule
//! `/etc/udev/rules.d/70-catostv-uinput.rules`); the TV account is in that
//! group already (catostv-inputd reads remotes).

use std::fs::File;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::Duration;

pub const KEY_ESC: u16 = 1;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_ENTER: u16 = 28;
pub const KEY_DOT: u16 = 52;
pub const KEY_F2: u16 = 60;
pub const KEY_F10: u16 = 68;
pub const KEY_UP: u16 = 103;
pub const KEY_PAGEUP: u16 = 104;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_DOWN: u16 = 108;
pub const KEY_PAGEDOWN: u16 = 109;
pub const KEY_MUTE: u16 = 113;
pub const KEY_VOLUMEDOWN: u16 = 114;
pub const KEY_VOLUMEUP: u16 = 115;
pub const KEY_BACK: u16 = 158;
pub const KEY_NEXTSONG: u16 = 163;
pub const KEY_PLAYPAUSE: u16 = 164;
pub const KEY_PREVIOUSSONG: u16 = 165;
pub const KEY_STOPCD: u16 = 166;
pub const KEY_REWIND: u16 = 168;
pub const KEY_CONFIG: u16 = 171;
pub const KEY_HOMEPAGE: u16 = 172;
pub const KEY_PAUSECD: u16 = 201;
pub const KEY_FASTFORWARD: u16 = 208;
pub const KEY_SELECT: u16 = 353;

/// The evdev key that produces this XKB keysym with the standard keymap
/// (the keysyms CEC and the phone remote use, see `cec::map_key` and
/// `remote::KEYS`).
pub fn code_for_keysym(keysym: &str) -> Option<u16> {
    Some(match keysym {
        "Up" => KEY_UP,
        "Down" => KEY_DOWN,
        "Left" => KEY_LEFT,
        "Right" => KEY_RIGHT,
        "Return" => KEY_ENTER,
        "Escape" => KEY_ESC,
        "BackSpace" => KEY_BACKSPACE,
        "F2" => KEY_F2,
        "F10" => KEY_F10,
        "period" => KEY_DOT,
        "1" => 2,
        "2" => 3,
        "3" => 4,
        "4" => 5,
        "5" => 6,
        "6" => 7,
        "7" => 8,
        "8" => 9,
        "9" => 10,
        "0" => 11,
        "Page_Up" => KEY_PAGEUP,
        "Page_Down" => KEY_PAGEDOWN,
        "XF86HomePage" => KEY_HOMEPAGE,
        "XF86Tools" => KEY_CONFIG,
        "XF86AudioRaiseVolume" => KEY_VOLUMEUP,
        "XF86AudioLowerVolume" => KEY_VOLUMEDOWN,
        "XF86AudioMute" => KEY_MUTE,
        "XF86AudioPlay" => KEY_PLAYPAUSE,
        "XF86AudioPause" => KEY_PAUSECD,
        "XF86AudioStop" => KEY_STOPCD,
        "XF86AudioRewind" => KEY_REWIND,
        "XF86AudioForward" => KEY_FASTFORWARD,
        "XF86AudioNext" => KEY_NEXTSONG,
        "XF86AudioPrev" => KEY_PREVIOUSSONG,
        _ => return None,
    })
}

/// Every key the virtual keyboard can send.
pub const ALL_KEYS: &[u16] = &[
    KEY_ESC,
    2,
    3,
    4,
    5,
    6,
    7,
    8,
    9,
    10,
    11,
    KEY_BACKSPACE,
    KEY_ENTER,
    KEY_DOT,
    KEY_F2,
    KEY_F10,
    KEY_UP,
    KEY_PAGEUP,
    KEY_LEFT,
    KEY_RIGHT,
    KEY_DOWN,
    KEY_PAGEDOWN,
    KEY_MUTE,
    KEY_VOLUMEDOWN,
    KEY_VOLUMEUP,
    KEY_BACK,
    KEY_NEXTSONG,
    KEY_PLAYPAUSE,
    KEY_PREVIOUSSONG,
    KEY_STOPCD,
    KEY_REWIND,
    KEY_CONFIG,
    KEY_HOMEPAGE,
    KEY_PAUSECD,
    KEY_FASTFORWARD,
    KEY_SELECT,
];

/// What is on screen, as written by catostv-shell to
/// `$XDG_RUNTIME_DIR/catostv-front`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Front {
    Shell,
    App,
    Android,
}

impl Front {
    pub fn as_str(self) -> &'static str {
        match self {
            Front::Shell => "shell",
            Front::App => "app",
            Front::Android => "android",
        }
    }

    pub fn parse(s: &str) -> Front {
        match s.trim() {
            "android" => Front::Android,
            "app" => Front::App,
            _ => Front::Shell,
        }
    }
}

pub fn front_path() -> PathBuf {
    crate::env::runtime_dir().join("catostv-front")
}

/// What is on screen now (Shell when unknown).
pub fn front() -> Front {
    std::fs::read_to_string(front_path())
        .map(|s| Front::parse(&s))
        .unwrap_or(Front::Shell)
}

/// Write the front state (only when it changed; the caller tracks that).
pub fn set_front(f: Front) {
    let path = front_path();
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, f.as_str()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// The key to send for `code` when `front` is on screen: Android gets the
/// keys an Android TV remote sends.
pub fn translate(code: u16, front: Front) -> u16 {
    match (front, code) {
        (Front::Android, KEY_ESC) => KEY_BACK,
        _ => code,
    }
}

// ---- /dev/uinput ---------------------------------------------------------

mod ffi {
    unsafe extern "C" {
        pub fn ioctl(fd: i32, request: std::ffi::c_ulong, ...) -> i32;
    }
}

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
pub const REL_X: u16 = 0;
pub const REL_Y: u16 = 1;
pub const REL_HWHEEL: u16 = 6;
pub const REL_WHEEL: u16 = 8;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
const INPUT_PROP_POINTER: u16 = 0;
const BUS_VIRTUAL: u16 = 0x06;
const UINPUT_MAX_NAME_SIZE: usize = 80;

// _IOW('U', n, T) = (1 << 30) | (size_of::<T>() << 16) | ('U' << 8) | n
const fn iow(nr: u64, size: u64) -> std::ffi::c_ulong {
    ((1u64 << 30) | (size << 16) | ((b'U' as u64) << 8) | nr) as std::ffi::c_ulong
}
const UI_DEV_CREATE: std::ffi::c_ulong = 0x5501;
const UI_DEV_DESTROY: std::ffi::c_ulong = 0x5502;
const UI_DEV_SETUP: std::ffi::c_ulong = iow(3, 92);
const UI_SET_EVBIT: std::ffi::c_ulong = iow(100, 4);
const UI_SET_KEYBIT: std::ffi::c_ulong = iow(101, 4);
const UI_SET_RELBIT: std::ffi::c_ulong = iow(102, 4);
const UI_SET_PROPBIT: std::ffi::c_ulong = iow(110, 4);

/// `struct uinput_setup` (linux/uinput.h): input_id + name + ff_effects_max.
#[repr(C)]
struct UinputSetup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; UINPUT_MAX_NAME_SIZE],
    ff_effects_max: u32,
}

/// One `struct input_event` (64-bit time layout).
pub fn event_bytes(kind: u16, code: u16, value: i32) -> [u8; 24] {
    let mut b = [0u8; 24];
    // Timestamps are filled in by the kernel; zero is fine.
    b[16..18].copy_from_slice(&kind.to_ne_bytes());
    b[18..20].copy_from_slice(&code.to_ne_bytes());
    b[20..24].copy_from_slice(&value.to_ne_bytes());
    b
}

/// Create a uinput device: `bits` are (ioctl, value) pairs set before
/// the setup, e.g. (UI_SET_EVBIT, EV_KEY), (UI_SET_KEYBIT, KEY_UP).
fn create_device(name: &str, product: u16, bits: &[(std::ffi::c_ulong, u16)]) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")?;
    let fd = file.as_raw_fd();
    let check = |r: i32, what: &str| -> io::Result<()> {
        if r < 0 {
            let e = io::Error::last_os_error();
            Err(io::Error::new(e.kind(), format!("uinput {what}: {e}")))
        } else {
            Ok(())
        }
    };
    // SAFETY: plain ioctls on a file we own, with integer arguments or a
    // pointer to a correctly sized #[repr(C)] struct that outlives the call.
    unsafe {
        for &(req, v) in bits {
            check(ffi::ioctl(fd, req, v as std::ffi::c_int), "bits")?;
        }
        let mut setup = UinputSetup {
            bustype: BUS_VIRTUAL,
            vendor: 0x7478, // "tx"
            product,
            version: 1,
            name: [0; UINPUT_MAX_NAME_SIZE],
            ff_effects_max: 0,
        };
        let n = name.len().min(UINPUT_MAX_NAME_SIZE - 1);
        setup.name[..n].copy_from_slice(&name.as_bytes()[..n]);
        check(
            ffi::ioctl(fd, UI_DEV_SETUP, &setup as *const UinputSetup),
            "setup",
        )?;
        check(ffi::ioctl(fd, UI_DEV_CREATE), "create")?;
    }
    // The compositor needs a moment to open the new device before the
    // first event, or that event is lost.
    std::thread::sleep(Duration::from_millis(300));
    Ok(file)
}

fn destroy_device(file: &File) {
    // SAFETY: ioctl without arguments on our own fd.
    unsafe {
        ffi::ioctl(file.as_raw_fd(), UI_DEV_DESTROY);
    }
}

/// Every key a whole keyboard can send, for [`VirtualKeyboard::create_full`]:
/// 1..=255 and the TV keys above the button range (0x160..=0x2bf). Never
/// a mouse or gamepad button (0x100..=0x15f, 0x2c0..).
pub fn full_keyboard_keys() -> impl Iterator<Item = u16> {
    (1u16..=0xff).chain(0x160..=0x2bf)
}

/// A virtual keyboard. Dropping it removes the device.
pub struct VirtualKeyboard {
    file: File,
}

impl VirtualKeyboard {
    /// Create "CatosTV <who>" (e.g. "CatosTV remote").
    pub fn create(name: &str) -> io::Result<Self> {
        Self::with_keys(name, 0x0001, ALL_KEYS.iter().copied())
    }

    /// A keyboard that can send any key: mouse mode passes the keys of the
    /// remotes it holds on through it.
    pub fn create_full(name: &str) -> io::Result<Self> {
        Self::with_keys(name, 0x0003, full_keyboard_keys())
    }

    fn with_keys(name: &str, product: u16, keys: impl Iterator<Item = u16>) -> io::Result<Self> {
        let mut bits = vec![(UI_SET_EVBIT, EV_KEY)];
        bits.extend(keys.map(|k| (UI_SET_KEYBIT, k)));
        Ok(Self {
            file: create_device(name, product, &bits)?,
        })
    }

    fn emit(&mut self, kind: u16, code: u16, value: i32) -> io::Result<()> {
        self.file.write_all(&event_bytes(kind, code, value))
    }

    pub fn key(&mut self, code: u16, down: bool) -> io::Result<()> {
        self.emit(EV_KEY, code, down as i32)?;
        self.emit(EV_SYN, 0, 0)
    }

    /// A raw key event: 1 down, 0 up, 2 auto-repeat.
    pub fn key_value(&mut self, code: u16, value: i32) -> io::Result<()> {
        self.emit(EV_KEY, code, value)?;
        self.emit(EV_SYN, 0, 0)
    }

    /// Press and release.
    pub fn tap(&mut self, code: u16) -> io::Result<()> {
        self.key(code, true)?;
        std::thread::sleep(Duration::from_millis(12));
        self.key(code, false)
    }
}

/// A virtual mouse: relative motion, wheel, left / right / middle button.
/// Dropping it removes the device.
pub struct VirtualMouse {
    file: File,
}

impl VirtualMouse {
    pub fn create(name: &str) -> io::Result<Self> {
        let bits = [
            (UI_SET_EVBIT, EV_KEY),
            (UI_SET_EVBIT, EV_REL),
            (UI_SET_KEYBIT, BTN_LEFT),
            (UI_SET_KEYBIT, BTN_RIGHT),
            (UI_SET_KEYBIT, BTN_MIDDLE),
            (UI_SET_RELBIT, REL_X),
            (UI_SET_RELBIT, REL_Y),
            (UI_SET_RELBIT, REL_WHEEL),
            (UI_SET_RELBIT, REL_HWHEEL),
            (UI_SET_PROPBIT, INPUT_PROP_POINTER),
        ];
        Ok(Self {
            file: create_device(name, 0x0002, &bits)?,
        })
    }

    fn write(&mut self, events: &[(u16, u16, i32)]) -> io::Result<()> {
        let mut buf = Vec::with_capacity((events.len() + 1) * 24);
        for &(k, c, v) in events {
            buf.extend_from_slice(&event_bytes(k, c, v));
        }
        buf.extend_from_slice(&event_bytes(EV_SYN, 0, 0));
        self.file.write_all(&buf)
    }

    /// Move by whole pixels (nothing is sent for 0, 0).
    pub fn move_by(&mut self, dx: i32, dy: i32) -> io::Result<()> {
        let mut ev = Vec::with_capacity(2);
        if dx != 0 {
            ev.push((EV_REL, REL_X, dx));
        }
        if dy != 0 {
            ev.push((EV_REL, REL_Y, dy));
        }
        if ev.is_empty() {
            return Ok(());
        }
        self.write(&ev)
    }

    /// Wheel notches: positive `v` scrolls up, positive `h` right.
    pub fn wheel(&mut self, v: i32, h: i32) -> io::Result<()> {
        let mut ev = Vec::with_capacity(2);
        if v != 0 {
            ev.push((EV_REL, REL_WHEEL, v));
        }
        if h != 0 {
            ev.push((EV_REL, REL_HWHEEL, h));
        }
        if ev.is_empty() {
            return Ok(());
        }
        self.write(&ev)
    }

    pub fn button(&mut self, code: u16, down: bool) -> io::Result<()> {
        self.write(&[(EV_KEY, code, down as i32)])
    }

    /// Press and release.
    pub fn click(&mut self, code: u16) -> io::Result<()> {
        self.button(code, true)?;
        std::thread::sleep(Duration::from_millis(15));
        self.button(code, false)
    }
}

impl Drop for VirtualMouse {
    fn drop(&mut self) {
        destroy_device(&self.file);
    }
}

impl Drop for VirtualKeyboard {
    fn drop(&mut self) {
        destroy_device(&self.file);
    }
}

/// Sends keysyms: through a uinput keyboard when `/dev/uinput` is usable,
/// otherwise `wtype` (older installs without the udev rule).
pub struct KeySender {
    kbd: Option<VirtualKeyboard>,
    name: String,
    /// Last failed uinput attempt (retried every minute).
    failed_at: Option<std::time::Instant>,
}

impl KeySender {
    pub fn new(name: &str) -> Self {
        Self {
            kbd: None,
            name: name.to_string(),
            failed_at: None,
        }
    }

    fn keyboard(&mut self) -> Option<&mut VirtualKeyboard> {
        if self.kbd.is_none()
            && self
                .failed_at
                .is_none_or(|t| t.elapsed() > Duration::from_secs(60))
        {
            match VirtualKeyboard::create(&self.name) {
                Ok(k) => self.kbd = Some(k),
                Err(e) => {
                    eprintln!("{}: {e}; falling back to wtype", self.name);
                    self.failed_at = Some(std::time::Instant::now());
                }
            }
        }
        self.kbd.as_mut()
    }

    /// Tap `keysym`. Returns an error only when neither uinput nor wtype
    /// could send it.
    pub fn tap(&mut self, keysym: &str) -> crate::Result<()> {
        if let Some(code) = code_for_keysym(keysym) {
            let code = translate(code, front());
            if let Some(k) = self.keyboard() {
                match k.tap(code) {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        eprintln!("{}: uinput write failed ({e}); recreating", self.name);
                        self.kbd = None;
                    }
                }
            }
        }
        use crate::process::Runner;
        crate::process::SystemRunner
            .run_ok(&crate::process::Cmd::new("wtype").args(["-k", keysym]))
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_keyboards_never_look_like_a_gamepad() {
        // "CatosTV TV remote" (catostv-cecd) and "CatosTV phone remote"
        // (catostv-remoted) declare only keyboard keys: no gamepad/joystick
        // buttons (BTN_JOYSTICK/BTN_GAMEPAD 0x120..=0x13f, BTN_TRIGGER_HAPPY
        // 0x2c0..=0x2ff) and no axes, so udev never tags them as a joystick
        // and android-pads never hands them to Android.
        assert!(
            !ALL_KEYS
                .iter()
                .any(|&k| (0x120..=0x13f).contains(&k) || (0x2c0..=0x2ff).contains(&k))
        );
    }

    #[test]
    fn the_pass_through_keyboard_is_not_a_mouse_or_gamepad() {
        // Mouse mode's keyboard must never look like a pointer or a pad
        // (udev would tag it, and libinput would treat it so).
        assert!(full_keyboard_keys().all(|k| !(0x100..=0x15f).contains(&k) && k < 0x2c0));
        assert!(full_keyboard_keys().any(|k| k == KEY_SELECT));
        assert!(full_keyboard_keys().any(|k| k == 0xff));
    }

    #[test]
    fn ioctl_numbers_match_linux_headers() {
        assert_eq!(UI_SET_RELBIT, 0x4004_5566);
        assert_eq!(UI_SET_PROPBIT, 0x4004_556e);
        assert_eq!(UI_SET_EVBIT, 0x4004_5564);
        assert_eq!(UI_SET_KEYBIT, 0x4004_5565);
        assert_eq!(UI_DEV_SETUP, 0x405c_5503);
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
    }

    #[test]
    fn every_remote_keysym_has_a_code() {
        for (_, sym) in crate::remote::KEYS {
            assert!(code_for_keysym(sym).is_some(), "{sym}");
        }
        for code in 0u8..=0xFF {
            if let Some(m) = crate::cec::map_key(code) {
                assert!(code_for_keysym(m.keysym).is_some(), "{}", m.keysym);
                if let crate::cec::Policy::Hold { long } = m.policy {
                    assert!(code_for_keysym(long).is_some(), "{long}");
                }
            }
        }
    }

    #[test]
    fn every_code_is_registered() {
        for (_, sym) in crate::remote::KEYS {
            let c = code_for_keysym(sym).unwrap();
            assert!(ALL_KEYS.contains(&c));
            assert!(ALL_KEYS.contains(&translate(c, Front::Android)));
        }
    }

    #[test]
    fn android_gets_tv_remote_keys() {
        assert_eq!(translate(KEY_ESC, Front::Android), KEY_BACK);
        // OK stays Enter: the TV image maps it to DPAD_CENTER.
        assert_eq!(translate(KEY_ENTER, Front::Android), KEY_ENTER);
        assert_eq!(translate(KEY_ESC, Front::App), KEY_ESC);
        assert_eq!(translate(KEY_UP, Front::Android), KEY_UP);
        assert_eq!(Front::parse("android\n"), Front::Android);
        assert_eq!(Front::parse("bogus"), Front::Shell);
    }

    #[test]
    fn event_layout() {
        let b = event_bytes(EV_KEY, KEY_BACK, 1);
        assert_eq!(&b[16..18], &1u16.to_ne_bytes());
        assert_eq!(&b[18..20], &158u16.to_ne_bytes());
        assert_eq!(&b[20..24], &1i32.to_ne_bytes());
    }
}
