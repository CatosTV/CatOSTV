//! Passive reading of Linux input devices (`/dev/input/event*`), std only.
//!
//! Used by `catostv-inputd` to watch keyboards and remotes for the escape
//! gesture ([`crate::gesture`]). Design rules:
//!
//! * **Passive.** Devices are opened read-only and not grabbed, so the
//!   compositor and apps still receive every key. Several readers of one
//!   evdev node each get their own copy of events. The one exception is
//!   mouse mode ([`crate::cursor`]): while it is on, keyboards and remotes
//!   are held ([`Watcher::grab`]) and their keys come as [`DeviceMsg::Raw`]
//!   so `catostv-inputd` can pass them on; game controllers are never held.
//! * **No ioctl.** Which devices are interesting is decided from sysfs
//!   (`/sys/class/input/eventN/device/capabilities/{ev,key}`), parsed by
//!   pure, tested functions.
//! * **Nothing leaves the reader thread but key classes.** Each device
//!   thread turns `EV_KEY` events into [`KeyClass`]es right away; key codes
//!   other than the Back-class keys never reach the rest of the program, and
//!   every other event type (including `EV_MSC` scan codes) is dropped.
//!   Game controllers are the exception: their face buttons and D-pad pass
//!   as [`crate::gamepad::PadEvent`]s (they carry no typing), and in mouse
//!   mode their sticks as [`DeviceMsg::Axis`].
//! * **One thread per device**, doing blocking `read()`s; a read error
//!   (`ENODEV` when the device is unplugged) or end-of-file ends the thread
//!   and is reported as [`DeviceMsg::Gone`]. Hot-plug is handled by calling
//!   [`Watcher::rescan`] periodically ([`RESCAN_INTERVAL`]); a device that is
//!   already being read is never opened twice.
//!
//! Layout assumptions (x86-64, CatosTV's only platform; also true on other
//! 64-bit Linux targets): sysfs bitmap words are 64-bit `unsigned long`s, and
//! `struct input_event` is 24 bytes little-endian
//! (`tv_sec: i64, tv_usec: i64, type: u16, code: u16, value: i32`).
//!
//! Event times: the kernel stamps events with `CLOCK_REALTIME` unless a
//! client switches the clock with the `EVIOCSCLOCKID` ioctl, and the wall
//! clock can jump (NTP sync right after boot). The reader therefore stamps
//! events with [`Instant`] when `read()` returns instead; the delay is far
//! below the gesture thresholds.

use crate::gesture::{BackKey, KeyAction, KeyClass, KeyEvent};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// How often `catostv-inputd` looks for newly plugged-in devices.
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(3);

/// Size of one `struct input_event` on 64-bit Linux.
pub const EVENT_SIZE: usize = 24;

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const SYN_DROPPED: u16 = 3;

pub const KEY_ESC: u16 = 1;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_BACK: u16 = 158;
/// Not watched: labwc binds `XF86HomePage` globally (rc.xml), so Home
/// already works inside every app without this daemon.
pub const KEY_HOMEPAGE: u16 = 172;

/// The Back-class key codes, in the order they are reported.
pub const BACK_KEYS: [u16; 3] = [KEY_ESC, KEY_BACKSPACE, KEY_BACK];

/// One `struct input_event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEvent {
    pub sec: i64,
    pub usec: i64,
    pub ev_type: u16,
    pub code: u16,
    pub value: i32,
}

/// Parse raw `read()` bytes into events. Trailing bytes that do not form a
/// whole event are ignored (evdev never returns partial events).
pub fn parse_events(bytes: &[u8]) -> Vec<RawEvent> {
    bytes
        .as_chunks::<EVENT_SIZE>()
        .0
        .iter()
        .map(|c| {
            let i64_at = |o: usize| i64::from_le_bytes(c[o..o + 8].try_into().expect("8 bytes"));
            let u16_at = |o: usize| u16::from_le_bytes(c[o..o + 2].try_into().expect("2 bytes"));
            RawEvent {
                sec: i64_at(0),
                usec: i64_at(8),
                ev_type: u16_at(16),
                code: u16_at(18),
                value: i32::from_le_bytes(c[20..24].try_into().expect("4 bytes")),
            }
        })
        .collect()
}

/// Encode an event exactly as the kernel would (for tests and tools).
pub fn encode_event(ev: &RawEvent) -> [u8; EVENT_SIZE] {
    let mut b = [0u8; EVENT_SIZE];
    b[0..8].copy_from_slice(&ev.sec.to_le_bytes());
    b[8..16].copy_from_slice(&ev.usec.to_le_bytes());
    b[16..18].copy_from_slice(&ev.ev_type.to_le_bytes());
    b[18..20].copy_from_slice(&ev.code.to_le_bytes());
    b[20..24].copy_from_slice(&ev.value.to_le_bytes());
    b
}

/// Parse a sysfs capability bitmap such as
/// `"402000000 3803078f800d001 feffffdfffefffff fffffffffffffffe"`.
///
/// sysfs prints 64-bit hex words **most-significant first** (leading zero
/// words omitted). The result is in the opposite order: index 0 holds bits
/// 0–63. Malformed input yields an empty bitmap (= no capabilities), so a
/// device we cannot understand is simply not watched.
pub fn parse_key_bitmap(s: &str) -> Vec<u64> {
    let mut words = Vec::new();
    for w in s.split_whitespace() {
        match u64::from_str_radix(w, 16) {
            Ok(v) => words.push(v),
            Err(_) => return Vec::new(),
        }
    }
    words.reverse();
    words
}

/// True if bit `code` is set in a bitmap from [`parse_key_bitmap`].
pub fn has_key(bitmap: &[u64], code: u16) -> bool {
    let (word, bit) = (usize::from(code) / 64, u32::from(code) % 64);
    bitmap.get(word).is_some_and(|w| w & (1u64 << bit) != 0)
}

/// Back-class key for an evdev key code.
pub fn back_key(code: u16) -> Option<BackKey> {
    match code {
        KEY_ESC => Some(BackKey::Escape),
        KEY_BACKSPACE => Some(BackKey::Backspace),
        KEY_BACK => Some(BackKey::Back),
        _ => None,
    }
}

/// Coarse class of an evdev key code (see [`KeyClass`]). This is the only
/// place key codes are looked at; it runs inside the reader thread.
pub fn classify(code: u16) -> KeyClass {
    if let Some(k) = back_key(code) {
        return KeyClass::Back(k);
    }
    let text = matches!(code,
        2..=13          // 1..0 - =
        | 16..=27       // q..p [ ]
        | 29            // left ctrl
        | 30..=43       // a..l ; ' ` left shift backslash
        | 44..=56       // z..m , . / right shift, keypad *, left alt
        | 58            // caps lock
        | 71..=83       // keypad digits, - + .
        | 86            // 102nd key (< > on ISO keyboards)
        | 97 | 98       // right ctrl, keypad /
        | 100           // right alt (AltGr)
        | 117 | 121     // keypad = ,
        | 125 | 126 // left/right super
    );
    if text {
        KeyClass::Text
    } else {
        KeyClass::Other
    }
}

/// Map an evdev event to a gesture key event, or `None` if it is not a key
/// event (or has an unknown value).
pub fn key_event(ev: &RawEvent, time: f64) -> Option<KeyEvent> {
    if ev.ev_type != EV_KEY {
        return None;
    }
    let action = match ev.value {
        0 => KeyAction::Release,
        1 => KeyAction::Press,
        2 => KeyAction::Repeat,
        _ => return None,
    };
    Some(KeyEvent::new(classify(ev.code), action, time))
}

/// An input device as seen in sysfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// `eventN`, the identity used to avoid opening a device twice.
    pub id: String,
    /// `/dev/input/eventN`.
    pub path: PathBuf,
    /// Kernel device name, e.g. "AT Translated Set 2 keyboard".
    pub name: String,
    pub has_ev_key: bool,
    /// Which of [`BACK_KEYS`] the device can send.
    pub back_keys: Vec<u16>,
    /// A game controller: the south face button (`BTN_SOUTH`) or generic
    /// joystick buttons (`BTN_TRIGGER`, cheap USB pads).
    pub gamepad: bool,
    /// One of mouse mode's own devices ([`crate::cursor::is_own_device`]):
    /// never read, or inputd would read back what it sends.
    pub own: bool,
}

impl DeviceInfo {
    /// Watched devices: they send key events and have a Back key, or are
    /// game controllers.
    pub fn is_interesting(&self) -> bool {
        self.has_ev_key && !self.own && (!self.back_keys.is_empty() || self.gamepad)
    }

    /// e.g. `"Escape, Backspace"`.
    pub fn back_key_names(&self) -> String {
        let names: Vec<&str> = self
            .back_keys
            .iter()
            .filter_map(|&c| back_key(c))
            .map(BackKey::name)
            .collect();
        if names.is_empty() {
            "none".into()
        } else {
            names.join(", ")
        }
    }
}

/// Build a [`DeviceInfo`] from the contents of the sysfs files (pure).
pub fn device_info(
    id: &str,
    path: PathBuf,
    name: &str,
    ev_caps: &str,
    key_caps: &str,
) -> DeviceInfo {
    let ev = parse_key_bitmap(ev_caps);
    let keys = parse_key_bitmap(key_caps);
    DeviceInfo {
        id: id.to_string(),
        path,
        name: name.trim().to_string(),
        has_ev_key: has_key(&ev, EV_KEY),
        back_keys: BACK_KEYS
            .into_iter()
            .filter(|&c| has_key(&keys, c))
            .collect(),
        gamepad: has_key(&keys, crate::gamepad::BTN_SOUTH)
            || has_key(&keys, crate::gamepad::BTN_TRIGGER),
        own: crate::cursor::is_own_device(name.trim()),
    }
}

/// The number in `eventN`, if `name` is an evdev node name.
fn event_number(name: &str) -> Option<u32> {
    name.strip_prefix("event")?.parse().ok()
}

/// List every `eventN` node in `dev_dir` (normally `/dev/input`) with its
/// sysfs description from `sys_class_input` (normally `/sys/class/input`),
/// sorted by N. Nodes without readable sysfs data get empty capabilities.
pub fn scan_devices(dev_dir: &Path, sys_class_input: &Path) -> Vec<DeviceInfo> {
    let Ok(entries) = std::fs::read_dir(dev_dir) else {
        return Vec::new();
    };
    let mut found: Vec<(u32, DeviceInfo)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let id = e.file_name().to_str()?.to_string();
            let n = event_number(&id)?;
            let dev = sys_class_input.join(&id).join("device");
            let read = |rel: &str| std::fs::read_to_string(dev.join(rel)).unwrap_or_default();
            let info = device_info(
                &id,
                e.path(),
                &read("name"),
                &read("capabilities/ev"),
                &read("capabilities/key"),
            );
            Some((n, info))
        })
        .collect();
    found.sort_by_key(|(n, _)| *n);
    found.into_iter().map(|(_, d)| d).collect()
}

/// Which of the `found` devices should be started: interesting and not
/// already being read (pure).
pub fn devices_to_start<'a>(
    found: &'a [DeviceInfo],
    active: &HashSet<String>,
) -> Vec<&'a DeviceInfo> {
    found
        .iter()
        .filter(|d| d.is_interesting() && !active.contains(&d.id))
        .collect()
}

/// Messages from the device reader threads.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceMsg {
    Key {
        device: String,
        event: KeyEvent,
    },
    /// A game controller's navigation button or D-pad.
    Pad {
        device: String,
        event: crate::gamepad::PadEvent,
        time: f64,
    },
    /// Mouse mode: a key of a held keyboard or remote, as it came
    /// (`value` 1 down, 0 up, 2 repeat). Only sent while mouse mode is on.
    Raw {
        device: String,
        code: u16,
        value: i32,
    },
    /// Mouse mode: a controller stick (`ABS_X`, `ABS_Y`, `ABS_RX`,
    /// `ABS_RY`) at -1..=1.
    Axis {
        device: String,
        code: u16,
        value: f64,
    },
    /// The kernel dropped events (`SYN_DROPPED`): key state is unknown.
    Dropped {
        device: String,
    },
    /// The device was unplugged or failed; its thread has ended.
    Gone {
        device: String,
        reason: String,
    },
}

/// What [`Watcher::rescan`] did, for logging.
#[derive(Debug, Clone, PartialEq)]
pub enum ScanNotice {
    Started(DeviceInfo),
    /// Opening failed (e.g. permission denied). Reported once per device;
    /// the open is retried on every rescan (udev may set the group a moment
    /// after the node appears).
    OpenFailed {
        device: DeviceInfo,
        error: String,
    },
}

/// Starts and tracks one reader thread per interesting device.
pub struct Watcher {
    dev_dir: PathBuf,
    sys_dir: PathBuf,
    epoch: Instant,
    tx: Sender<DeviceMsg>,
    rx: Receiver<DeviceMsg>,
    active: HashSet<String>,
    open_failed: HashSet<String>,
    /// Mouse mode: readers send raw keys and stick positions.
    raw: Arc<AtomicBool>,
    /// The open devices (shared with their reader threads), for holding
    /// them in mouse mode, and whether each is a controller.
    files: HashMap<String, (Arc<File>, bool)>,
}

/// `EVIOCGRAB`: _IOW('E', 0x90, int).
const EVIOCGRAB: std::ffi::c_ulong = 0x4004_4590;
/// `EVIOCGKEY(96)`: _IOC(_IOC_READ, 'E', 0x18, 96), the keys held now
/// (KEY_MAX 0x2ff → 96 bytes).
const EVIOCGKEY_96: std::ffi::c_ulong = 0x8060_4518;

unsafe extern "C" {
    fn ioctl(fd: i32, request: std::ffi::c_ulong, ...) -> i32;
}

impl Watcher {
    pub fn new(dev_dir: impl Into<PathBuf>, sys_dir: impl Into<PathBuf>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            dev_dir: dev_dir.into(),
            sys_dir: sys_dir.into(),
            epoch: Instant::now(),
            tx,
            rx,
            active: HashSet::new(),
            open_failed: HashSet::new(),
            raw: Arc::new(AtomicBool::new(false)),
            files: HashMap::new(),
        }
    }

    /// Mouse mode on or off for the readers (see [`DeviceMsg::Raw`]).
    pub fn set_raw(&self, on: bool) {
        self.raw.store(on, Ordering::SeqCst);
    }

    /// The keyboards and remotes being read (not controllers).
    pub fn keyboards(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .files
            .iter()
            .filter(|(_, (_, pad))| !pad)
            .map(|(id, _)| id.clone())
            .collect();
        v.sort();
        v
    }

    /// Is any key of this device down right now? Holding a device with a
    /// key down would leave that key stuck for everyone else.
    pub fn keys_held(&self, id: &str) -> bool {
        use std::os::fd::AsRawFd;
        let Some((f, _)) = self.files.get(id) else {
            return false;
        };
        let mut bits = [0u8; 96];
        // SAFETY: EVIOCGKEY(96) writes at most 96 bytes into `bits`.
        let r = unsafe { ioctl(f.as_raw_fd(), EVIOCGKEY_96, bits.as_mut_ptr()) };
        r > 0 && bits.iter().any(|b| *b != 0)
    }

    /// Hold (`on`) or let go of a keyboard or remote: while held, only
    /// this program gets its keys. Controllers are never held.
    pub fn grab(&self, id: &str, on: bool) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let Some((f, pad)) = self.files.get(id) else {
            return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        };
        if *pad {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }
        // SAFETY: EVIOCGRAB takes an int argument by value.
        let r = unsafe { ioctl(f.as_raw_fd(), EVIOCGRAB, on as std::ffi::c_int) };
        if r < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// `/dev/input` + `/sys/class/input`.
    pub fn system() -> Self {
        Self::new("/dev/input", "/sys/class/input")
    }

    /// All event devices (watched or not), for `--list`.
    pub fn devices(&self) -> Vec<DeviceInfo> {
        scan_devices(&self.dev_dir, &self.sys_dir)
    }

    /// Seconds since the watcher was created, on the clock used for
    /// [`KeyEvent::time`].
    pub fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    /// Number of devices currently being read.
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    pub fn is_active(&self, id: &str) -> bool {
        self.active.contains(id)
    }

    /// Start reader threads for interesting devices not yet being read.
    pub fn rescan(&mut self) -> Vec<ScanNotice> {
        let found = self.devices();
        let present: HashSet<&str> = found.iter().map(|d| d.id.as_str()).collect();
        self.open_failed.retain(|id| present.contains(id.as_str()));
        let to_start: Vec<DeviceInfo> = devices_to_start(&found, &self.active)
            .into_iter()
            .cloned()
            .collect();
        let mut notices = Vec::new();
        for dev in &to_start {
            match self.start(dev) {
                Ok(()) => {
                    self.open_failed.remove(&dev.id);
                    self.active.insert(dev.id.clone());
                    notices.push(ScanNotice::Started(dev.clone()));
                }
                Err(e) => {
                    if self.open_failed.insert(dev.id.clone()) {
                        notices.push(ScanNotice::OpenFailed {
                            device: dev.clone(),
                            error: e.to_string(),
                        });
                    }
                }
            }
        }
        notices
    }

    fn start(&mut self, dev: &DeviceInfo) -> std::io::Result<()> {
        // Read-only, not grabbed: apps keep receiving every key.
        let file = Arc::new(File::open(&dev.path)?);
        let tx = self.tx.clone();
        let id = dev.id.clone();
        let epoch = self.epoch;
        let gamepad = dev.gamepad;
        let raw = self.raw.clone();
        let reader = file.clone();
        thread::Builder::new()
            .name(format!("inputd-{id}"))
            .spawn(move || read_device(reader, id, gamepad, epoch, raw, tx))?;
        self.files.insert(dev.id.clone(), (file, gamepad));
        Ok(())
    }

    /// Wait up to `timeout` for the next message. `None` on timeout.
    /// Keeps the set of active devices up to date.
    pub fn recv_timeout(&mut self, timeout: Duration) -> Option<DeviceMsg> {
        match self.rx.recv_timeout(timeout) {
            Ok(msg) => {
                if let DeviceMsg::Gone { device, .. } = &msg {
                    self.active.remove(device);
                    self.files.remove(device);
                }
                Some(msg)
            }
            Err(RecvTimeoutError::Timeout) => None,
            // Unreachable: the watcher itself holds a Sender. Sleep anyway so
            // a future change can never turn this into a busy loop.
            Err(RecvTimeoutError::Disconnected) => {
                thread::sleep(timeout);
                None
            }
        }
    }
}

/// `(min, max)` of absolute axis `abs` (EVIOCGABS), if the device has it.
fn abs_range(file: &File, abs: u16) -> Option<(i32, i32)> {
    use std::os::fd::AsRawFd;
    unsafe extern "C" {
        fn ioctl(fd: i32, request: std::ffi::c_ulong, ...) -> i32;
    }
    // _IOR('E', 0x40 + abs, struct input_absinfo), 6 × i32.
    let req: std::ffi::c_ulong = 0x8018_4540 + abs as std::ffi::c_ulong;
    let mut info = [0i32; 6];
    // SAFETY: EVIOCGABS writes exactly one struct input_absinfo (24 bytes).
    let r = unsafe { ioctl(file.as_raw_fd(), req, info.as_mut_ptr()) };
    (r >= 0 && info[2] > info[1]).then_some((info[1], info[2]))
}

/// Controller sticks reported in mouse mode: left X / Y, right X / Y.
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;

/// Reader thread body: blocking reads until error/EOF.
fn read_device(
    file: Arc<File>,
    id: String,
    gamepad: bool,
    epoch: Instant,
    raw: Arc<AtomicBool>,
    tx: Sender<DeviceMsg>,
) {
    use crate::gamepad::{ABS_X, ABS_Y, EV_ABS, PadEvent, stick_dir};
    let mut buf = [0u8; EVENT_SIZE * 64];
    // Left stick (or a D-pad reported as one): its range, last direction.
    let ranges = if gamepad {
        [abs_range(&file, ABS_X), abs_range(&file, ABS_Y)]
    } else {
        [None, None]
    };
    let right_ranges = if gamepad {
        [abs_range(&file, ABS_RX), abs_range(&file, ABS_RY)]
    } else {
        [None, None]
    };
    let mut stick = [0i32; 2];
    let reason = loop {
        match (&*file).read(&mut buf) {
            Ok(0) => break "end of file".to_string(),
            Ok(n) => {
                let time = epoch.elapsed().as_secs_f64();
                let mouse = raw.load(Ordering::Relaxed);
                for ev in parse_events(&buf[..n]) {
                    // Mouse mode: stick positions as they are.
                    if mouse && gamepad && ev.ev_type == EV_ABS {
                        let range = match ev.code {
                            ABS_X | ABS_Y => ranges[ev.code as usize],
                            ABS_RX => right_ranges[0],
                            ABS_RY => right_ranges[1],
                            _ => None,
                        };
                        if let Some((min, max)) = range {
                            let msg = DeviceMsg::Axis {
                                device: id.clone(),
                                code: ev.code,
                                value: crate::cursor::normalize(ev.value, min, max),
                            };
                            if tx.send(msg).is_err() {
                                return;
                            }
                        }
                    }
                    let msg = if ev.ev_type == EV_SYN && ev.code == SYN_DROPPED {
                        DeviceMsg::Dropped { device: id.clone() }
                    } else if mouse && !gamepad {
                        // Mouse mode: keys as they came, to be passed on.
                        if ev.ev_type != EV_KEY || !(0..=2).contains(&ev.value) {
                            continue;
                        }
                        DeviceMsg::Raw {
                            device: id.clone(),
                            code: ev.code,
                            value: ev.value,
                        }
                    } else if gamepad
                        && ev.ev_type == EV_ABS
                        && (ev.code == ABS_X || ev.code == ABS_Y)
                    {
                        let axis = ev.code as usize;
                        let Some((min, max)) = ranges[axis] else {
                            continue;
                        };
                        let dir = stick_dir(ev.value, min, max);
                        if dir == stick[axis] {
                            continue;
                        }
                        stick[axis] = dir;
                        DeviceMsg::Pad {
                            device: id.clone(),
                            event: PadEvent::Stick { code: ev.code, dir },
                            time,
                        }
                    } else if gamepad {
                        match crate::gamepad::pad_event(ev.ev_type, ev.code, ev.value) {
                            Some(event) => DeviceMsg::Pad {
                                device: id.clone(),
                                event,
                                time,
                            },
                            None => continue,
                        }
                    } else if let Some(event) = key_event(&ev, time) {
                        DeviceMsg::Key {
                            device: id.clone(),
                            event,
                        }
                    } else {
                        continue;
                    };
                    if tx.send(msg).is_err() {
                        return; // watcher gone: the program is exiting
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => break e.to_string(),
        }
    };
    let _ = tx.send(DeviceMsg::Gone { device: id, reason });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `AT Translated Set 2 keyboard` (laptop / QEMU PS/2 keyboard).
    const AT_KBD_KEYS: &str = "402000000 3803078f800d001 feffffdfffefffff fffffffffffffffe\n";
    const AT_KBD_EV: &str = "120013\n";
    /// ACPI "Power Button": only KEY_POWER (116).
    const POWER_KEYS: &str = "10000000000000 0\n";
    const POWER_EV: &str = "3\n";
    /// A remote's "Consumer Control" device: KEY_BACK + KEY_HOMEPAGE only.
    const REMOTE_KEYS: &str = "100040000000 0 0\n";

    fn ev(ev_type: u16, code: u16, value: i32) -> RawEvent {
        RawEvent {
            sec: 1_700_000_000,
            usec: 123_456,
            ev_type,
            code,
            value,
        }
    }

    #[test]
    fn bitmap_words_are_reversed_to_least_significant_first() {
        assert_eq!(parse_key_bitmap("1 0 ff"), vec![0xff, 0, 1]);
        assert_eq!(parse_key_bitmap("0\n"), vec![0]);
        assert_eq!(parse_key_bitmap(""), Vec::<u64>::new());
        assert_eq!(parse_key_bitmap("xyz 12"), Vec::<u64>::new());
        assert_eq!(parse_key_bitmap("1ffffffffffffffff"), Vec::<u64>::new());
    }

    #[test]
    fn has_key_reads_the_right_bits() {
        let kbd = parse_key_bitmap(AT_KBD_KEYS);
        assert!(has_key(&kbd, KEY_ESC));
        assert!(has_key(&kbd, KEY_BACKSPACE));
        assert!(!has_key(&kbd, 0)); // KEY_RESERVED is never set
        assert!(!has_key(&kbd, 0x2ff)); // beyond the bitmap
        let power = parse_key_bitmap(POWER_KEYS);
        assert!(has_key(&power, 116));
        assert!(!has_key(&power, KEY_ESC));
        let remote = parse_key_bitmap(REMOTE_KEYS);
        assert!(has_key(&remote, KEY_BACK));
        assert!(has_key(&remote, KEY_HOMEPAGE));
        assert!(!has_key(&remote, KEY_ESC));
        let ev = parse_key_bitmap(AT_KBD_EV);
        assert!(has_key(&ev, EV_KEY));
        assert!(!has_key(&parse_key_bitmap("4"), EV_KEY)); // EV_REL only
    }

    #[test]
    fn device_classification() {
        let p = PathBuf::from("/dev/input/event0");
        let kbd = device_info("event0", p.clone(), "AT kbd\n", AT_KBD_EV, AT_KBD_KEYS);
        assert!(kbd.is_interesting());
        assert_eq!(kbd.name, "AT kbd");
        assert!(kbd.back_keys.starts_with(&[KEY_ESC, KEY_BACKSPACE]));
        let power = device_info("event1", p.clone(), "Power Button", POWER_EV, POWER_KEYS);
        assert!(!power.is_interesting());
        assert_eq!(power.back_key_names(), "none");
        let remote = device_info("event2", p.clone(), "Remote", "3", REMOTE_KEYS);
        assert!(remote.is_interesting());
        assert_eq!(remote.back_key_names(), "Back");
        // Back key in the key bitmap but no EV_KEY: not interesting.
        let odd = device_info("event3", p.clone(), "x", "4", REMOTE_KEYS);
        assert!(!odd.is_interesting());
        // Missing sysfs data.
        assert!(!device_info("event4", p.clone(), "", "", "").is_interesting());
        // An Xbox-style controller: BTN_SOUTH (0x130) in the key bitmap.
        let mut words = vec!["0".to_string(); 5];
        // sysfs prints the highest word first: word 4 (bits 256..319).
        words[0] = format!("{:x}", 1u64 << (0x130 - 4 * 64));
        let pad = device_info(
            "event5",
            p,
            "Xbox Wireless Controller",
            "20000b",
            &words.join(" "),
        );
        assert!(pad.gamepad && pad.is_interesting());
    }

    #[test]
    fn parse_events_decodes_input_event_layout() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&encode_event(&ev(EV_KEY, KEY_BACK, 1)));
        bytes.extend_from_slice(&encode_event(&RawEvent {
            sec: -1,
            usec: 999_999,
            ev_type: EV_SYN,
            code: 0,
            value: -7,
        }));
        bytes.extend_from_slice(&[1, 2, 3]); // trailing partial event
        let evs = parse_events(&bytes);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0], ev(EV_KEY, KEY_BACK, 1));
        assert_eq!(evs[1].sec, -1);
        assert_eq!(evs[1].value, -7);
        // Hand-written bytes, independent of encode_event.
        let mut raw = [0u8; 24];
        raw[0] = 5; // tv_sec = 5
        raw[16] = 1; // EV_KEY
        raw[18] = 14; // KEY_BACKSPACE
        raw[20] = 2; // autorepeat
        assert_eq!(
            parse_events(&raw),
            vec![RawEvent {
                sec: 5,
                usec: 0,
                ev_type: 1,
                code: 14,
                value: 2
            }]
        );
        assert!(parse_events(&[]).is_empty());
    }

    #[test]
    fn classify_keys() {
        assert_eq!(classify(KEY_ESC), KeyClass::Back(BackKey::Escape));
        assert_eq!(classify(KEY_BACKSPACE), KeyClass::Back(BackKey::Backspace));
        assert_eq!(classify(KEY_BACK), KeyClass::Back(BackKey::Back));
        // Letters, digits, punctuation, modifiers, keypad digits: typing.
        for code in [
            30, 16, 44, 2, 11, 12, 39, 51, 53, 42, 54, 29, 56, 100, 125, 82, 86,
        ] {
            assert_eq!(classify(code), KeyClass::Text, "code {code}");
        }
        // Navigation and TV keys: not typing.
        for code in [
            28,  // Enter
            57,  // Space (play/pause in players)
            15,  // Tab
            103, // Up
            108, // Down
            105, // Left
            106, // Right
            96,  // keypad Enter
            KEY_HOMEPAGE,
            164,   // play/pause
            115,   // volume up
            59,    // F1
            0x110, // BTN_LEFT
        ] {
            assert_eq!(classify(code), KeyClass::Other, "code {code}");
        }
    }

    #[test]
    fn key_event_mapping() {
        let k = key_event(&ev(EV_KEY, KEY_ESC, 1), 2.5).unwrap();
        assert_eq!(
            k,
            KeyEvent::new(KeyClass::Back(BackKey::Escape), KeyAction::Press, 2.5)
        );
        assert_eq!(
            key_event(&ev(EV_KEY, 30, 2), 0.0).unwrap().action,
            KeyAction::Repeat
        );
        assert_eq!(
            key_event(&ev(EV_KEY, 30, 0), 0.0).unwrap().action,
            KeyAction::Release
        );
        assert_eq!(key_event(&ev(EV_KEY, 30, 5), 0.0), None);
        assert_eq!(key_event(&ev(4, 4, 30), 0.0), None); // EV_MSC scan code
        assert_eq!(key_event(&ev(EV_SYN, 0, 0), 0.0), None);
    }

    fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("catostv-evdev-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dev = root.join("dev");
        let sys = root.join("sys");
        std::fs::create_dir_all(&dev).unwrap();
        (root, dev, sys)
    }

    fn add_device(
        dev: &Path,
        sys: &Path,
        id: &str,
        name: &str,
        ev_caps: &str,
        keys: &str,
        data: &[u8],
    ) {
        std::fs::write(dev.join(id), data).unwrap();
        let d = sys.join(id).join("device");
        std::fs::create_dir_all(d.join("capabilities")).unwrap();
        std::fs::write(d.join("name"), format!("{name}\n")).unwrap();
        std::fs::write(d.join("capabilities/ev"), ev_caps).unwrap();
        std::fs::write(d.join("capabilities/key"), keys).unwrap();
    }

    #[test]
    fn scan_finds_event_nodes_in_numeric_order() {
        let (root, dev, sys) = fixture("scan");
        add_device(&dev, &sys, "event10", "Remote", "3", REMOTE_KEYS, b"");
        add_device(
            &dev,
            &sys,
            "event2",
            "Keyboard",
            AT_KBD_EV,
            AT_KBD_KEYS,
            b"",
        );
        add_device(
            &dev,
            &sys,
            "event1",
            "Power Button",
            POWER_EV,
            POWER_KEYS,
            b"",
        );
        std::fs::write(dev.join("event7"), b"").unwrap(); // no sysfs entry
        std::fs::write(dev.join("mice"), b"").unwrap();
        std::fs::write(dev.join("eventX"), b"").unwrap();
        let found = scan_devices(&dev, &sys);
        let ids: Vec<&str> = found.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["event1", "event2", "event7", "event10"]);
        assert_eq!(found[1].name, "Keyboard");
        assert_eq!(found[1].path, dev.join("event2"));

        let mut active = HashSet::new();
        let start: Vec<&str> = devices_to_start(&found, &active)
            .iter()
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(start, ["event2", "event10"]);
        active.insert("event2".to_string());
        let start: Vec<&str> = devices_to_start(&found, &active)
            .iter()
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(start, ["event10"], "an active device is never opened twice");

        assert!(scan_devices(&root.join("missing"), &sys).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn watcher_reads_classified_events_until_eof() {
        let (root, dev, sys) = fixture("watch");
        let mut data = Vec::new();
        for e in [
            ev(EV_KEY, KEY_ESC, 1),
            ev(4, 4, 0x70029), // EV_MSC scan code: dropped
            ev(EV_SYN, 0, 0),
            ev(EV_KEY, 30, 1), // 'a': arrives only as Text
            ev(EV_SYN, SYN_DROPPED, 0),
            ev(EV_KEY, KEY_ESC, 0),
        ] {
            data.extend_from_slice(&encode_event(&e));
        }
        add_device(
            &dev,
            &sys,
            "event3",
            "Keyboard",
            AT_KBD_EV,
            AT_KBD_KEYS,
            &data,
        );
        add_device(
            &dev,
            &sys,
            "event4",
            "Power Button",
            POWER_EV,
            POWER_KEYS,
            b"",
        );

        let mut w = Watcher::new(&dev, &sys);
        let notices = w.rescan();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(matches!(&notices[0], ScanNotice::Started(d) if d.id == "event3"));
        assert!(w.is_active("event3"));

        let mut msgs = Vec::new();
        while let Some(m) = w.recv_timeout(Duration::from_secs(5)) {
            let gone = matches!(m, DeviceMsg::Gone { .. });
            msgs.push(m);
            if gone {
                break;
            }
        }
        let summary: Vec<String> = msgs
            .iter()
            .map(|m| match m {
                DeviceMsg::Key { device, event } => {
                    format!("{device} {:?} {:?}", event.key, event.action)
                }
                DeviceMsg::Dropped { device } => format!("{device} dropped"),
                DeviceMsg::Pad { device, event, .. } => format!("{device} {event:?}"),
                DeviceMsg::Gone { device, reason } => format!("{device} gone: {reason}"),
                DeviceMsg::Raw {
                    device,
                    code,
                    value,
                } => format!("{device} raw {code} {value}"),
                DeviceMsg::Axis {
                    device,
                    code,
                    value,
                } => format!("{device} axis {code} {value}"),
            })
            .collect();
        assert_eq!(
            summary,
            [
                "event3 Back(Escape) Press",
                "event3 Text Press",
                "event3 dropped",
                "event3 Back(Escape) Release",
                "event3 gone: end of file",
            ]
        );
        assert!(!w.is_active("event3"));
        assert_eq!(w.active_count(), 0);
        assert!(w.now() >= 0.0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn mouse_mode_sends_keys_as_they_came_and_skips_its_own_devices() {
        let (root, dev, sys) = fixture("raw");
        let mut data = Vec::new();
        for e in [
            ev(EV_KEY, 103, 1), // Up
            ev(4, 4, 0x70052),  // EV_MSC: dropped
            ev(EV_KEY, 103, 2),
            ev(EV_KEY, 30, 1), // 'a', as it is
            ev(EV_KEY, 103, 0),
        ] {
            data.extend_from_slice(&encode_event(&e));
        }
        add_device(
            &dev,
            &sys,
            "event3",
            "Remote",
            AT_KBD_EV,
            AT_KBD_KEYS,
            &data,
        );
        // Mouse mode's own keyboard is never read.
        add_device(
            &dev,
            &sys,
            "event4",
            crate::cursor::KEYS_NAME,
            AT_KBD_EV,
            AT_KBD_KEYS,
            b"",
        );
        let mut w = Watcher::new(&dev, &sys);
        w.set_raw(true);
        let started: Vec<String> = w
            .rescan()
            .into_iter()
            .filter_map(|n| match n {
                ScanNotice::Started(d) => Some(d.id),
                _ => None,
            })
            .collect();
        assert_eq!(started, ["event3"]);
        let mut got = Vec::new();
        while let Some(m) = w.recv_timeout(Duration::from_secs(5)) {
            match m {
                DeviceMsg::Raw { code, value, .. } => got.push((code, value)),
                DeviceMsg::Gone { .. } => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(got, [(103, 1), (103, 2), (30, 1), (103, 0)]);
        assert!(w.keyboards().is_empty(), "a device that is gone is let go");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn open_failure_is_reported_once() {
        let (root, dev, sys) = fixture("openfail");
        // sysfs says "keyboard" but the node is missing: open fails.
        add_device(&dev, &sys, "event5", "Ghost", AT_KBD_EV, AT_KBD_KEYS, b"");
        std::fs::remove_file(dev.join("event5")).unwrap();
        std::fs::create_dir(dev.join("event5")).unwrap();
        std::fs::set_permissions(
            dev.join("event5"),
            std::os::unix::fs::PermissionsExt::from_mode(0o000),
        )
        .unwrap();
        let mut w = Watcher::new(&dev, &sys);
        let first = w.rescan();
        // Opening a directory read-only succeeds as root, then read() fails
        // with EISDIR; as a normal user open() fails with EACCES. Either way
        // the device must not be reported twice as failed.
        match first.as_slice() {
            [ScanNotice::OpenFailed { device, .. }] => {
                assert_eq!(device.id, "event5");
                assert!(w.rescan().is_empty(), "failure reported only once");
            }
            [ScanNotice::Started(_)] => {
                let m = w.recv_timeout(Duration::from_secs(5));
                assert!(matches!(m, Some(DeviceMsg::Gone { .. })), "{m:?}");
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
