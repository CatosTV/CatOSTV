//! HDMI-CEC: TV remote → keyboard keys (pure logic for `catostv-cecd`).
//!
//! The daemon (`src/bin/catostv-cecd.rs`) runs `cec-client` from Arch
//! `extra/libcec` as a long-running child, reads its log on stdout, and
//! turns the TV remote's key frames into key presses injected with `wtype`.
//! This module holds everything that decides something, so it can be tested
//! without hardware:
//!
//! * [`parse_adapters`], `cec-client -l` output;
//! * [`client_cmd`], the long-running `cec-client` argv;
//! * [`parse_key_line`], one log line → [`CecKey`];
//! * [`map_key`], CEC user-control code → keysym + repeat policy;
//! * [`KeyRepeater`], auto-repeat / long-press handling;
//! * [`Backoff`], restart delays when `cec-client` dies.
//!
//! ## Log format (libcec `cec-client.cpp`, checked against upstream master)
//!
//! Every log line is `"%s[%16lld]\t%s"`: a level label (`ERROR:   `,
//! `WARNING: `, `NOTICE:  `, `TRAFFIC: `, `DEBUG:   `), a right-aligned
//! timestamp in ms, a tab, the message. `cec-client` writes with
//! `std::cout << … << std::endl`, so lines are flushed even into a pipe.
//! With `-d 13` (ERROR|NOTICE|TRAFFIC; `-d` is a bitmask) a remote key
//! press looks like (libcec issue #467):
//!
//! ```text
//! TRAFFIC: [         3516912] <TAB> >> 01:44:00      ← received: TV(0)→1, <User Control Pressed>, Select
//! ```
//!
//! CatosTV parses the **raw frames**, not the DEBUG `key pressed: select (0)
//! current(ff) duration(0)` lines: libcec prints those twice per press in
//! some versions (both `select (0)` and `select (0, 0)` forms appear in
//! issue #467) and applies its own repeat logic. Frames are:
//!
//! * `>> I D:44:KK`, User Control Pressed, key code `KK` (CEC 1.4 table
//!   "UI Command Codes"); TVs repeat it every 200–450 ms while held;
//! * `>> I D:45`, User Control Released;
//! * `>> I D:8b:…`, Vendor Remote Button Up, which Samsung (Anynet+) TVs
//!   send instead of 0x45 (seen in plex-media-player issue #138).
//!
//! Status: parsers/state machine unit-tested; on real CEC hardware
//! UNVERIFIED. Most PC GPUs have **no** CEC line; this needs a Pulse-Eight
//! USB-CEC adapter or a board/kernel CEC device.

use crate::error::{Error, Result};
use crate::process::{Cmd, Runner};
use std::time::Duration;

/// `cec-client -d` value: ERROR (1) | NOTICE (4) | TRAFFIC (8).
pub const LOG_LEVEL: &str = "13";
/// OSD name the TV shows for this device (CEC limit: 14 characters).
pub const DEFAULT_OSD_NAME: &str = "CatosTV";
/// A held key is considered released when no repeat arrives within this
/// time (CEC follower safety timeout is 550 ms; initiators repeat every
/// 200–450 ms).
pub const RELEASE_TIMEOUT: Duration = Duration::from_millis(550);
/// Holding OK at least this long is a long press (same as
/// `input::LONG_PRESS_SECS`).
pub const LONG_PRESS: Duration = Duration::from_millis(600);
/// A TV that sends release frames but no repeats: a held key is given up
/// (treated as released) after this long without a release frame.
pub const HOLD_SAFETY: Duration = Duration::from_secs(5);

/// One CEC adapter from `cec-client -l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    /// `com port:` value, e.g. `/dev/ttyACM0`; passed as cec-client's port.
    pub com_port: String,
    /// `type:` value if printed, e.g. `Pulse-Eight USB-CEC Adapter`.
    pub kind: String,
}

/// Parse `cec-client -l`:
///
/// ```text
/// libCEC version: 3.0.1, git revision: defcd67
/// Found devices: 2
/// device:              1
/// com port:            /dev/ttyACM0
/// vendor id:           2548
/// ...
/// type:                unknown
/// ```
///
/// (`Found devices: NONE` when there is no adapter.)
pub fn parse_adapters(text: &str) -> Vec<Adapter> {
    let mut adapters: Vec<Adapter> = Vec::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "com port" if !v.is_empty() => adapters.push(Adapter {
                com_port: v.to_string(),
                kind: String::new(),
            }),
            "type" => {
                if let Some(a) = adapters.last_mut() {
                    a.kind = v.to_string();
                }
            }
            _ => {}
        }
    }
    adapters
}

/// A com-port string safe to pass as cec-client's positional argument
/// (cec-client treats any unrecognised argument as the port).
pub fn is_valid_com_port(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/_.-:".contains(c))
}

/// OSD name: 1–14 ASCII letters, digits, spaces, `.`, `_`, `-`.
pub fn is_valid_osd_name(s: &str) -> bool {
    (1..=14).contains(&s.len())
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || " ._-".contains(c))
}

/// Detect adapters (`cec-client -l`; may take a few seconds).
pub fn list_adapters(r: &mut dyn Runner) -> Result<Vec<Adapter>> {
    let out = r.run_ok(
        &Cmd::new("cec-client")
            .arg("-l")
            .timeout(Duration::from_secs(15)),
    )?;
    Ok(parse_adapters(&out.stdout))
}

/// The long-running client: `cec-client -t p -o NAME -d 13 PORT`
/// (`-t p` = playback device, so the TV routes remote keys to it). The
/// daemon spawns this itself (it must stream stdout); `timeout` is unused.
pub fn client_cmd(adapter: &Adapter, osd_name: &str) -> Result<Cmd> {
    if !is_valid_com_port(&adapter.com_port) {
        return Err(Error::msg(format!(
            "unusable CEC adapter port {:?}",
            adapter.com_port
        )));
    }
    if !is_valid_osd_name(osd_name) {
        return Err(Error::msg(format!("invalid CEC OSD name {osd_name:?}")));
    }
    Ok(Cmd::new("cec-client").args([
        "-t",
        "p",
        "-o",
        osd_name,
        "-d",
        LOG_LEVEL,
        &adapter.com_port,
    ]))
}

/// A remote-control event from one log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CecKey {
    /// User Control Pressed with this UI command code (also sent as repeat).
    Pressed(u8),
    /// User Control Released / Vendor Remote Button Up.
    Released,
}

/// Parse one `cec-client` log line. Only received (`>>`) TRAFFIC frames
/// with opcode 0x44, 0x45 or 0x8B count; broadcast frames are ignored.
pub fn parse_key_line(line: &str) -> Option<CecKey> {
    let rest = line.trim_start().strip_prefix("TRAFFIC:")?;
    let after_ts = &rest[rest.find(']')? + 1..];
    let frame = after_ts.trim_start().strip_prefix(">>")?.trim();
    let mut bytes = Vec::new();
    for part in frame.split(':') {
        if part.len() != 2 {
            return None;
        }
        bytes.push(u8::from_str_radix(part, 16).ok()?);
    }
    let header = *bytes.first()?;
    if header & 0x0F == 0x0F {
        return None; // broadcast: never a key press meant for us
    }
    match *bytes.get(1)? {
        0x44 => Some(CecKey::Pressed(*bytes.get(2)?)),
        0x45 | 0x8B => Some(CecKey::Released),
        _ => None,
    }
}

/// Log lines worth copying into CatosTV's log (errors and notices).
pub fn is_notable_log_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("ERROR:") || t.starts_with("NOTICE:")
}

/// What to do while a key is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// One tap per press frame, so the TV's repeat rate drives navigation.
    Repeat,
    /// One tap per press; repeats are ignored until release.
    Once,
    /// Tap on release; held ≥ [`LONG_PRESS`] taps `long` instead.
    Hold { long: &'static str },
}

/// A CEC key's injected keysym and repeat policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyMapping {
    /// XKB keysym name for `wtype -k`.
    pub keysym: &'static str,
    pub policy: Policy,
}

/// CEC UI command code → keysym. The one mapping table (HDMI-CEC 1.4
/// "UI Command Codes"). Keysyms are what CatosTV and labwc already react to:
///
/// | CEC                         | Key                    | Effect in CatosTV          |
/// |-----------------------------|------------------------|--------------------------|
/// | Up/Down/Left/Right          | Up/Down/Left/Right     | navigate (repeats)       |
/// | Select (hold ≥ 0.6 s)       | Return (F2)            | OK (Options)             |
/// | Exit                        | Escape                 | Back                     |
/// | Root Menu                   | XF86HomePage           | Home (labwc binding)     |
/// | Setup Menu                  | XF86Tools              | Control Center (labwc)   |
/// | Contents / Context Menu     | F2                     | Options                  |
/// | Numbers, Dot, Enter, Clear  | 0–9, period, Return, BackSpace | text entry       |
/// | Channel/Page Up/Down        | Page_Up / Page_Down    | apps                     |
/// | Volume Up/Down, Mute        | XF86Audio…Volume/Mute  | wpctl (labwc bindings)   |
/// | Play, Pause, Stop, …        | XF86AudioPlay, …       | media apps (MPRIS)       |
///
/// Deliberately unmapped: Power/Power Toggle/Off/On (the TV remote must
/// never shut down the PC), colour keys F1–F4 (no agreed keysym), EPG,
/// Help, diagonal directions, Record.
pub fn map_key(code: u8) -> Option<KeyMapping> {
    use Policy::*;
    let (keysym, policy) = match code {
        0x00 => ("Return", Hold { long: "F2" }),
        0x01 => ("Up", Repeat),
        0x02 => ("Down", Repeat),
        0x03 => ("Left", Repeat),
        0x04 => ("Right", Repeat),
        0x09 => ("XF86HomePage", Once), // Root Menu
        0x0A => ("XF86Tools", Once),    // Setup Menu
        0x0B => ("F2", Once),           // Contents Menu
        0x0D => ("Escape", Once),       // Exit (the "Back"/"Return" key)
        0x11 => ("F2", Once),           // Media Context-sensitive Menu
        0x20 => ("0", Once),
        0x21 => ("1", Once),
        0x22 => ("2", Once),
        0x23 => ("3", Once),
        0x24 => ("4", Once),
        0x25 => ("5", Once),
        0x26 => ("6", Once),
        0x27 => ("7", Once),
        0x28 => ("8", Once),
        0x29 => ("9", Once),
        0x2A => ("period", Once),
        0x2B => ("Return", Once),             // Enter
        0x2C => ("BackSpace", Repeat),        // Clear
        0x30 | 0x37 => ("Page_Up", Repeat),   // Channel Up / Page Up
        0x31 | 0x38 => ("Page_Down", Repeat), // Channel Down / Page Down
        0x41 => ("XF86AudioRaiseVolume", Repeat),
        0x42 => ("XF86AudioLowerVolume", Repeat),
        0x43 | 0x65 => ("XF86AudioMute", Once), // Mute / Mute Function
        0x44 | 0x60 => ("XF86AudioPlay", Once), // Play / Play Function
        0x45 | 0x64 => ("XF86AudioStop", Once), // Stop / Stop Function
        0x46 => ("XF86AudioPause", Once),
        0x61 => ("XF86AudioPlay", Once), // Pause-Play Function (toggle)
        0x48 => ("XF86AudioRewind", Repeat),
        0x49 => ("XF86AudioForward", Repeat),
        0x4B => ("XF86AudioNext", Once), // Forward (skip)
        0x4C => ("XF86AudioPrev", Once), // Backward (skip)
        _ => return None,
    };
    Some(KeyMapping { keysym, policy })
}

/// `wtype -k KEYSYM` (a press and release on a virtual keyboard).
pub fn wtype_cmd(keysym: &str) -> Cmd {
    Cmd::new("wtype").args(["-k", keysym])
}

/// The first kernel CEC device (`/dev/cec0` on Intel DisplayPort), if any.
pub fn kernel_device() -> Option<String> {
    (0..4)
        .map(|n| format!("/dev/cec{n}"))
        .find(|p| std::path::Path::new(p).exists())
}

/// One Volume Up/Down press and release sent to the TV with `cec-ctl`
/// (v4l-utils). The kernel lets it share the adapter with cec-client.
pub fn tv_volume_cmd(device: &str, up: bool) -> Cmd {
    let key = if up { "volume-up" } else { "volume-down" };
    Cmd::new("cec-ctl").args([
        "-d",
        device,
        "--to",
        "0",
        "--user-control-pressed",
        &format!("ui-cmd={key}"),
        "--user-control-released",
    ])
}

#[derive(Debug, Clone, Copy)]
struct Held {
    code: u8,
    mapping: KeyMapping,
    since: Duration,
    last: Duration,
    long_sent: bool,
}

/// Turns press/release frames into key taps.
///
/// Time is passed in as a monotonic `Duration` (e.g. `Instant::elapsed` of
/// a fixed epoch) so the logic is testable. The caller must call
/// [`KeyRepeater::on_tick`] at [`KeyRepeater::deadline`] to finish keys
/// whose release frame never arrives (many TVs only send presses).
/// TVs differ: some repeat the press frame while a key is held
/// and never send a release; others send one press and a release when the
/// finger lifts. The repeater learns which kind it talks to from the first
/// release frame it sees: with release frames, a Hold key (OK) waits for
/// the release instead of giving up after [`RELEASE_TIMEOUT`], which on
/// an TV turned every long press into a tap.
#[derive(Debug, Clone, Default)]
pub struct KeyRepeater {
    held: Option<Held>,
    saw_release: bool,
}

impl KeyRepeater {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one event; returns keysyms to tap, in order.
    pub fn on_key(&mut self, key: CecKey, now: Duration) -> Vec<&'static str> {
        let mut out = Vec::new();
        match key {
            CecKey::Pressed(code) => {
                if let Some(h) = self.held.as_mut()
                    && h.code == code
                    && now.saturating_sub(h.last) <= RELEASE_TIMEOUT
                {
                    // Auto-repeat of the held key.
                    h.last = now;
                    match h.mapping.policy {
                        Policy::Repeat => out.push(h.mapping.keysym),
                        Policy::Once => {}
                        Policy::Hold { long } => {
                            if !h.long_sent && now.saturating_sub(h.since) >= LONG_PRESS {
                                h.long_sent = true;
                                out.push(long);
                            }
                        }
                    }
                    return out;
                }
                // A new key: finish the previous one where we last saw it.
                if let Some(prev) = self.held {
                    out.extend(self.finish(prev.last));
                }
                let Some(mapping) = map_key(code) else {
                    return out;
                };
                if !matches!(mapping.policy, Policy::Hold { .. }) {
                    out.push(mapping.keysym);
                }
                self.held = Some(Held {
                    code,
                    mapping,
                    since: now,
                    last: now,
                    long_sent: false,
                });
            }
            CecKey::Released => {
                self.saw_release = true;
                out.extend(self.finish(now));
            }
        }
        out
    }

    /// Whether this key is waiting for the TV's release frame rather than
    /// for its repeats to stop.
    fn waits_for_release(&self, h: &Held) -> bool {
        self.saw_release && matches!(h.mapping.policy, Policy::Hold { .. })
    }

    /// Finish a held key whose repeats stopped without a release frame;
    /// with a TV that sends release frames, fire the long press of a Hold
    /// key once it has been held long enough (the finger is still down).
    pub fn on_tick(&mut self, now: Duration) -> Vec<&'static str> {
        let Some(h) = self.held else {
            return Vec::new();
        };
        if self.waits_for_release(&h) {
            if now.saturating_sub(h.since) >= HOLD_SAFETY {
                return self.finish(now);
            }
            if let Policy::Hold { long } = h.mapping.policy
                && !h.long_sent
                && now.saturating_sub(h.since) >= LONG_PRESS
            {
                if let Some(h) = self.held.as_mut() {
                    h.long_sent = true;
                }
                return vec![long];
            }
            return Vec::new();
        }
        if now.saturating_sub(h.last) > RELEASE_TIMEOUT {
            self.finish(h.last)
        } else {
            Vec::new()
        }
    }

    /// When [`KeyRepeater::on_tick`] should next be called (`None` = no key held).
    pub fn deadline(&self) -> Option<Duration> {
        let h = self.held?;
        if self.waits_for_release(&h) {
            Some(if h.long_sent {
                h.since + HOLD_SAFETY
            } else {
                h.since + LONG_PRESS
            })
        } else {
            Some(h.last + RELEASE_TIMEOUT + Duration::from_millis(1))
        }
    }

    fn finish(&mut self, released_at: Duration) -> Vec<&'static str> {
        let Some(h) = self.held.take() else {
            return Vec::new();
        };
        match h.mapping.policy {
            Policy::Hold { long } if !h.long_sent => {
                if released_at.saturating_sub(h.since) >= LONG_PRESS {
                    vec![long]
                } else {
                    vec![h.mapping.keysym]
                }
            }
            _ => Vec::new(),
        }
    }
}

/// Restart delays for a dying `cec-client`: 2, 4, 8 … 60 s; reset after
/// a run that lasted at least a minute.
#[derive(Debug, Clone, Default)]
pub struct Backoff {
    failures: u32,
}

impl Backoff {
    pub const MIN: Duration = Duration::from_secs(2);
    pub const MAX: Duration = Duration::from_secs(60);
    pub const HEALTHY_RUN: Duration = Duration::from_secs(60);

    pub fn new() -> Self {
        Self::default()
    }

    /// Delay before the next start, given how long the last run lasted.
    pub fn next_delay(&mut self, ran_for: Duration) -> Duration {
        if ran_for >= Self::HEALTHY_RUN {
            self.failures = 0;
        }
        let delay = Self::MIN
            .saturating_mul(1u32 << self.failures.min(6))
            .min(Self::MAX);
        self.failures = self.failures.saturating_add(1);
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Output, fake::FakeRunner};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// `cec-client -l` lines as posted in LibreELEC forum thread 4084
    /// (Pulse-Eight USB adapter + an Amlogic "AOCEC" device).
    const LIST: &str = "libCEC version: 3.0.1, git revision: defcd67
Found devices: 2

device:              1
com port:            /dev/ttyACM0
vendor id:           2548
product id:          1002
firmware version:    65535

device:              2
com port:            AOCEC
vendor id:           0000
product id:          0000
firmware version:    5
type:                unknown
";

    #[test]
    fn adapters() {
        let a = parse_adapters(LIST);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].com_port, "/dev/ttyACM0");
        assert_eq!(a[0].kind, "");
        assert_eq!(a[1].com_port, "AOCEC");
        assert_eq!(a[1].kind, "unknown");
        // cec-client.cpp prints this when nothing is found.
        assert!(parse_adapters("libCEC version: 8.1.7\nFound devices: NONE\n").is_empty());
    }

    #[test]
    fn list_adapters_runs_cec_client() {
        let mut r = FakeRunner::new();
        r.push(Output::ok(LIST));
        assert_eq!(list_adapters(&mut r).unwrap().len(), 2);
        assert_eq!(r.seen[0].display(), "cec-client -l");
        assert_eq!(r.seen[0].timeout, Duration::from_secs(15));
    }

    #[test]
    fn client_command_and_validation() {
        let a = &parse_adapters(LIST)[0];
        assert_eq!(
            client_cmd(a, DEFAULT_OSD_NAME).unwrap().display(),
            "cec-client -t p -o CatosTV -d 13 /dev/ttyACM0"
        );
        assert_eq!(client_cmd(a, "Living Room").unwrap().args[3], "Living Room");
        for bad_port in ["", "-s", "/dev/x;rm", "a b"] {
            let bad = Adapter {
                com_port: bad_port.into(),
                kind: String::new(),
            };
            assert!(client_cmd(&bad, "CatosTV").is_err(), "{bad_port:?}");
        }
        for bad_name in ["", "-d", "Fifteen chars!!", "a\tb", "é"] {
            assert!(client_cmd(a, bad_name).is_err(), "{bad_name:?}");
        }
    }

    #[test]
    fn key_lines() {
        // Exact lines from libcec issue #467 (tab after the timestamp).
        assert_eq!(
            parse_key_line("TRAFFIC: [         3516912]\t>> 01:44:00"),
            Some(CecKey::Pressed(0x00))
        );
        assert_eq!(
            parse_key_line("TRAFFIC: [         2012876]\t<< 28:44:00"),
            None,
            "frames we send are ignored"
        );
        assert_eq!(
            parse_key_line(
                "DEBUG:   [         3516913]\tkey pressed: select (0) current(ff) duration(0)"
            ),
            None,
            "debug key lines are deliberately not used"
        );
        assert_eq!(
            parse_key_line("DEBUG:   [         3517096]\tkey released: select (0) D:183ms"),
            None
        );
        // Release (0x45), Samsung vendor button up (0x8B), other opcodes.
        assert_eq!(
            parse_key_line("TRAFFIC: [            8807]\t>> 04:45"),
            Some(CecKey::Released)
        );
        assert_eq!(
            parse_key_line("TRAFFIC: [            8807]\t>> 04:8b:01"),
            Some(CecKey::Released)
        );
        assert_eq!(
            parse_key_line("TRAFFIC: [            9000]\t>> 04:44:01"),
            Some(CecKey::Pressed(0x01))
        );
        assert_eq!(parse_key_line("TRAFFIC: [  1]\t>> 0f:87:00:00:f0"), None);
        assert_eq!(
            parse_key_line("TRAFFIC: [  1]\t>> 0f:44:01"),
            None,
            "broadcast"
        );
        assert_eq!(
            parse_key_line("TRAFFIC: [  1]\t>> 04:44"),
            None,
            "no operand"
        );
        assert_eq!(parse_key_line("TRAFFIC: [  1]\t>> 04:4x:01"), None);
        assert_eq!(parse_key_line("TRAFFIC: [             255] << f0"), None);
        assert_eq!(
            parse_key_line(">> 04:44:01"),
            None,
            "needs the TRAFFIC label"
        );
        assert_eq!(parse_key_line(""), None);
    }

    #[test]
    fn notable_lines() {
        assert!(is_notable_log_line(
            "ERROR:   [  12]\tcould not open a connection"
        ));
        assert!(is_notable_log_line(
            "NOTICE:  [  12]\tCEC client registered"
        ));
        assert!(!is_notable_log_line("TRAFFIC: [  12]\t>> 01:44:00"));
    }

    #[test]
    fn tv_volume_command() {
        let c = tv_volume_cmd("/dev/cec0", true);
        assert_eq!(c.program, "cec-ctl");
        assert_eq!(
            c.args,
            [
                "-d",
                "/dev/cec0",
                "--to",
                "0",
                "--user-control-pressed",
                "ui-cmd=volume-up",
                "--user-control-released"
            ]
        );
    }

    #[test]
    fn mapping_table() {
        let k = |c| map_key(c).map(|m| m.keysym);
        assert_eq!(k(0x01), Some("Up"));
        assert_eq!(k(0x04), Some("Right"));
        assert_eq!(k(0x0D), Some("Escape"));
        assert_eq!(k(0x09), Some("XF86HomePage"));
        assert_eq!(k(0x0A), Some("XF86Tools"));
        assert_eq!(k(0x20), Some("0"));
        assert_eq!(k(0x29), Some("9"));
        assert_eq!(k(0x41), Some("XF86AudioRaiseVolume"));
        assert_eq!(k(0x44), Some("XF86AudioPlay"));
        assert_eq!(k(0x46), Some("XF86AudioPause"));
        for never in [0x40, 0x6B, 0x6C, 0x6D, 0x71, 0x72, 0x73, 0x74, 0x53, 0xFF] {
            assert_eq!(k(never), None, "{never:#x}");
        }
        assert_eq!(map_key(0x00).unwrap().policy, Policy::Hold { long: "F2" });
        // Every mapped key is a plain keysym name wtype can take as argv.
        for code in 0..=255u8 {
            if let Some(m) = map_key(code) {
                assert!(
                    m.keysym
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_'),
                    "{code:#x}"
                );
            }
        }
        assert_eq!(wtype_cmd("Up").display(), "wtype -k Up");
    }

    #[test]
    fn arrows_repeat_with_the_tv() {
        let mut k = KeyRepeater::new();
        assert_eq!(k.on_key(CecKey::Pressed(0x02), ms(0)), ["Down"]);
        assert_eq!(k.on_key(CecKey::Pressed(0x02), ms(450)), ["Down"]);
        assert_eq!(k.on_key(CecKey::Pressed(0x02), ms(900)), ["Down"]);
        assert!(k.on_key(CecKey::Released, ms(1000)).is_empty());
        assert_eq!(k.deadline(), None);
    }

    #[test]
    fn once_keys_ignore_repeats_until_release_or_gap() {
        let mut k = KeyRepeater::new();
        assert_eq!(k.on_key(CecKey::Pressed(0x0D), ms(0)), ["Escape"]);
        assert!(k.on_key(CecKey::Pressed(0x0D), ms(400)).is_empty());
        assert!(k.on_key(CecKey::Released, ms(500)).is_empty());
        assert_eq!(k.on_key(CecKey::Pressed(0x0D), ms(600)), ["Escape"]);
        // TV without release frames: a press after the gap is a new press.
        assert!(k.on_tick(ms(1000)).is_empty());
        assert_eq!(k.deadline(), Some(ms(600 + 551)));
        assert!(k.on_tick(ms(1200)).is_empty());
        assert_eq!(k.deadline(), None);
        assert_eq!(k.on_key(CecKey::Pressed(0x0D), ms(1300)), ["Escape"]);
    }

    #[test]
    fn select_tap_and_long_press() {
        // Tap: nothing on press, Return on release.
        let mut k = KeyRepeater::new();
        assert!(k.on_key(CecKey::Pressed(0x00), ms(0)).is_empty());
        assert_eq!(k.on_key(CecKey::Released, ms(180)), ["Return"]);

        // Held with repeats: F2 once, as soon as 0.6 s is reached.
        assert!(k.on_key(CecKey::Pressed(0x00), ms(1000)).is_empty());
        assert!(k.on_key(CecKey::Pressed(0x00), ms(1450)).is_empty());
        assert_eq!(k.on_key(CecKey::Pressed(0x00), ms(1900)), ["F2"]);
        assert!(k.on_key(CecKey::Pressed(0x00), ms(2350)).is_empty());
        assert!(k.on_key(CecKey::Released, ms(2400)).is_empty());

        // Held without repeats (press … release 800 ms later): F2 on release.
        assert!(k.on_key(CecKey::Pressed(0x00), ms(3000)).is_empty());
        assert_eq!(k.on_key(CecKey::Released, ms(3800)), ["F2"]);

        // This TV has sent release frames, so a press with none yet is
        // still held: the long press fires at 0.6 s.
        assert!(k.on_key(CecKey::Pressed(0x00), ms(5000)).is_empty());
        assert!(k.on_tick(ms(5500)).is_empty());
        assert_eq!(k.on_tick(ms(5600)), ["F2"]);

        // A TV that never sends release frames: Return once the repeats stop.
        let mut k = KeyRepeater::new();
        assert!(k.on_key(CecKey::Pressed(0x00), ms(5000)).is_empty());
        assert!(k.on_tick(ms(5500)).is_empty());
        assert_eq!(k.on_tick(ms(5600)), ["Return"]);
    }

    #[test]
    fn tv_with_release_frames_waits_for_the_release() {
        // One TV: one press frame, no repeats, a release frame
        // when the finger lifts. Before the first release frame the
        // repeater cannot know that, so the first OK behaves as before.
        let mut k = KeyRepeater::new();
        assert!(k.on_key(CecKey::Pressed(0x00), ms(0)).is_empty());
        assert_eq!(k.on_key(CecKey::Released, ms(120)), ["Return"]);
        // From now on a held OK is not given up at 550 ms …
        assert!(k.on_key(CecKey::Pressed(0x00), ms(1000)).is_empty());
        assert_eq!(k.deadline(), Some(ms(1600)));
        assert!(k.on_tick(ms(1551)).is_empty());
        // … it fires the long press while still held, once at 0.6 s …
        assert_eq!(k.on_tick(ms(1600)), ["F2"]);
        assert_eq!(k.deadline(), Some(ms(6000)));
        assert!(k.on_tick(ms(2000)).is_empty());
        // … and the release then injects nothing more.
        assert!(k.on_key(CecKey::Released, ms(2400)).is_empty());
        assert_eq!(k.deadline(), None);
        // A short tap is still a tap.
        assert!(k.on_key(CecKey::Pressed(0x00), ms(3000)).is_empty());
        assert_eq!(k.on_key(CecKey::Released, ms(3200)), ["Return"]);
        // Arrows keep the repeat-or-timeout logic.
        assert_eq!(k.on_key(CecKey::Pressed(0x02), ms(4000)), ["Down"]);
        assert_eq!(k.deadline(), Some(ms(4551)));
        // A lost release frame: the safety timeout ends the hold.
        assert!(k.on_key(CecKey::Pressed(0x00), ms(5000)).is_empty());
        assert_eq!(k.on_tick(ms(5600)), ["F2"]);
        assert!(k.on_tick(ms(10_000)).is_empty());
        assert_eq!(k.deadline(), None);
    }

    #[test]
    fn a_new_key_finishes_the_previous_one() {
        let mut k = KeyRepeater::new();
        assert!(k.on_key(CecKey::Pressed(0x00), ms(0)).is_empty());
        assert_eq!(k.on_key(CecKey::Pressed(0x01), ms(100)), ["Return", "Up"]);
        // Unmapped key (Power) clears state but injects nothing.
        assert!(k.on_key(CecKey::Pressed(0x40), ms(200)).is_empty());
        assert_eq!(k.deadline(), None);
        assert!(k.on_key(CecKey::Released, ms(250)).is_empty());
    }

    #[test]
    fn backoff_grows_and_resets() {
        let mut b = Backoff::new();
        let short = Duration::from_secs(1);
        let delays: Vec<u64> = (0..8).map(|_| b.next_delay(short).as_secs()).collect();
        assert_eq!(delays, [2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(b.next_delay(Duration::from_secs(600)).as_secs(), 2);
    }
}
