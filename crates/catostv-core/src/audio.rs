//! PipeWire / WirePlumber adapter (`wpctl`, plus `pactl` for card profiles).
//!
//! * Volume/mute of the default sink: `wpctl get-volume` / `set-volume` /
//!   `set-mute` on `@DEFAULT_AUDIO_SINK@`.
//! * Output devices: the `Audio → Sinks:` section of `wpctl status`
//!   (WirePlumber 0.5), see [`parse_wpctl_sinks`].
//! * HDMI card profiles: `pactl list cards` / `pactl set-card-profile`
//!   (pipewire-pulse; `pactl` is in Arch `libpulse`, a hard dependency of
//!   `pipewire-pulse`), see [`hdmi_profile_hint`].
//!
//! Text parsers assume the English UI strings of `wpctl`/`pactl`; the
//! installed system's locale is `en_US.UTF-8` (`install::LOCALE`).

use crate::error::{Error, Result};
use crate::process::{Cmd, Runner};

const SINK: &str = "@DEFAULT_AUDIO_SINK@";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VolumeState {
    /// 0–100 (CatosTV never boosts above 100 %).
    pub percent: u8,
    pub muted: bool,
}

/// Parse `wpctl get-volume` output: `Volume: 0.45` or `Volume: 0.45 [MUTED]`.
pub fn parse_volume(text: &str) -> Option<VolumeState> {
    let rest = text.trim().strip_prefix("Volume:")?.trim();
    let number = rest.split_whitespace().next()?;
    let value: f32 = number.parse().ok()?;
    Some(VolumeState {
        percent: (value * 100.0).round().clamp(0.0, 100.0) as u8,
        muted: rest.contains("[MUTED]"),
    })
}

pub fn get(r: &mut dyn Runner) -> Result<VolumeState> {
    let out = r.run_ok(&Cmd::new("wpctl").args(["get-volume", SINK]))?;
    parse_volume(&out.stdout).ok_or_else(|| Error::msg("could not read the volume"))
}

/// Change volume by `delta` percentage points (clamped to 0–100) and return
/// the new state as reported by PipeWire. Turning it up also unmutes.
pub fn step(r: &mut dyn Runner, delta: i8) -> Result<VolumeState> {
    let arg = if delta >= 0 {
        format!("{delta}%+")
    } else {
        format!("{}%-", -(delta as i16))
    };
    r.run_ok(&Cmd::new("wpctl").args(["set-volume", "-l", "1.0", SINK, &arg]))?;
    if delta > 0 {
        r.run_ok(&Cmd::new("wpctl").args(["set-mute", SINK, "0"]))?;
    }
    get(r)
}

/// Set the volume to `percent` (0–100), unmuting too when `unmute`.
pub fn set(r: &mut dyn Runner, percent: u8, unmute: bool) -> Result<VolumeState> {
    let level = format!("{:.2}", f32::from(percent.min(100)) / 100.0);
    r.run_ok(&Cmd::new("wpctl").args(["set-volume", SINK, &level]))?;
    if unmute {
        r.run_ok(&Cmd::new("wpctl").args(["set-mute", SINK, "0"]))?;
    }
    get(r)
}

pub fn toggle_mute(r: &mut dyn Runner) -> Result<VolumeState> {
    r.run_ok(&Cmd::new("wpctl").args(["set-mute", SINK, "toggle"]))?;
    get(r)
}

// ---------------------------------------------------------------------------
// Output devices (sinks)
// ---------------------------------------------------------------------------

/// What kind of output a sink is, for icons and the TV-sink heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkKind {
    /// HDMI or DisplayPort audio (the TV or an AV receiver).
    Hdmi,
    /// Built-in speakers, line out, headphone jack.
    Analog,
    Bluetooth,
    Usb,
    Other,
}

impl SinkKind {
    pub fn label(self) -> &'static str {
        match self {
            SinkKind::Hdmi => "TV (HDMI)",
            SinkKind::Analog => "Speakers / headphones",
            SinkKind::Bluetooth => "Bluetooth",
            SinkKind::Usb => "USB audio",
            SinkKind::Other => "Audio output",
        }
    }
}

/// One audio output device as listed by `wpctl status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sink {
    /// PipeWire object id (changes across restarts; use for `set-default`).
    pub id: u32,
    /// Description shown by `wpctl status`, e.g. "Built-in Audio Digital Stereo (HDMI)".
    pub name: String,
    /// PipeWire `node.name` from `wpctl status --name`
    /// (e.g. `alsa_output.pci-0000_01_00.1.hdmi-stereo`), stable across
    /// reboots. `None` if that query failed.
    pub node_name: Option<String>,
    pub is_default: bool,
    pub kind: SinkKind,
}

/// One entry of the `Audio → Sinks:` section of `wpctl status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub id: u32,
    /// Description (or node name with `--name`), padding and `[vol: …]` removed.
    pub text: String,
    pub is_default: bool,
}

fn is_tree_char(c: char) -> bool {
    c.is_whitespace() || matches!(c, '│' | '├' | '└' | '─')
}

/// Parse the `Audio → Sinks:` section of `wpctl status` (WirePlumber 0.5).
///
/// Format (wpctl.c: tree prefix ` │  `, then `%c %4u. %-35s`, then an
/// optional ` [vol: %.2f]` / ` [vol: %.2f MUTED]`):
///
/// ```text
/// Audio
///  ├─ Sinks:
///  │      43. USB Audio Front Headphones          [vol: 1.00]
///  │  *  111. HDA NVidia Digital Stereo (HDMI 2)  [vol: 1.00]
///  │
///  ├─ Sources:
/// ```
///
/// The `Video` section also has a `Sinks:` subsection; it is ignored.
pub fn parse_wpctl_sinks(text: &str) -> Vec<StatusEntry> {
    let mut top = String::new();
    let mut section = String::new();
    let mut entries = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            top = line.trim().to_string();
            section.clear();
            continue;
        }
        let t = line.trim_start_matches(is_tree_char).trim_end();
        let prefix = &line[..line.len() - line.trim_start_matches(is_tree_char).len()];
        if prefix.contains("├─") || prefix.contains("└─") {
            section = t.trim_end_matches(':').trim().to_string();
            continue;
        }
        if top != "Audio" || section != "Sinks" {
            continue;
        }
        let (is_default, rest) = match t.strip_prefix('*') {
            Some(r) => (true, r.trim_start()),
            None => (false, t),
        };
        let Some((id, rest)) = rest.split_once(". ") else {
            continue;
        };
        let Ok(id) = id.trim().parse::<u32>() else {
            continue;
        };
        let name = match rest.rfind("[vol:") {
            Some(i) => &rest[..i],
            None => rest,
        }
        .trim();
        if name.is_empty() {
            continue;
        }
        entries.push(StatusEntry {
            id,
            text: name.to_string(),
            is_default,
        });
    }
    entries
}

/// Classify a sink from its description and (if known) its `node.name`.
/// The node name is reliable (`bluez_output.…`, `….hdmi-stereo`,
/// `alsa_output.usb-…`); descriptions of Bluetooth sinks are just the
/// device name, so without a node name they end up as `Other`.
pub fn classify_sink(description: &str, node_name: Option<&str>) -> SinkKind {
    let d = description.to_ascii_lowercase();
    let n = node_name.unwrap_or("").to_ascii_lowercase();
    if n.starts_with("bluez_output.") || n.starts_with("bluez_sink.") || d.contains("bluetooth") {
        SinkKind::Bluetooth
    } else if n.contains("hdmi")
        || n.contains("displayport")
        || d.contains("hdmi")
        || d.contains("displayport")
    {
        SinkKind::Hdmi
    } else if n.starts_with("alsa_output.usb-") || d.contains("usb") {
        SinkKind::Usb
    } else if n.contains("analog")
        || d.contains("analog")
        || d.contains("speaker")
        || d.contains("headphone")
    {
        SinkKind::Analog
    } else {
        SinkKind::Other
    }
}

/// Merge `wpctl status` (descriptions) with `wpctl status --name` (node
/// names) by id.
pub fn merge_sinks(described: &[StatusEntry], named: &[StatusEntry]) -> Vec<Sink> {
    described
        .iter()
        .map(|e| {
            let node_name = named.iter().find(|n| n.id == e.id).map(|n| n.text.clone());
            Sink {
                id: e.id,
                kind: classify_sink(&e.text, node_name.as_deref()),
                name: e.text.clone(),
                node_name,
                is_default: e.is_default,
            }
        })
        .collect()
}

/// List audio outputs. Runs `wpctl status` and `wpctl status --name`
/// (the second is optional; without it Bluetooth sinks classify as Other).
pub fn sinks(r: &mut dyn Runner) -> Result<Vec<Sink>> {
    let out = r.run_ok(&Cmd::new("wpctl").arg("status"))?;
    let described = parse_wpctl_sinks(&out.stdout);
    let named = match r.run(&Cmd::new("wpctl").args(["status", "--name"])) {
        Ok(o) if o.success() => parse_wpctl_sinks(&o.stdout),
        _ => Vec::new(),
    };
    Ok(merge_sinks(&described, &named))
}

/// Make sink `id` the default output (`wpctl set-default ID`). WirePlumber
/// remembers the choice across restarts (`default-nodes` state file).
pub fn set_default_sink(r: &mut dyn Runner, id: u32) -> Result<()> {
    r.run_ok(&Cmd::new("wpctl").args(["set-default", &id.to_string()]))?;
    Ok(())
}

/// The sink CatosTV should switch to so sound comes out of the TV: the first
/// HDMI sink, if one exists and the current default is not already HDMI.
///
/// Deliberate refinement: also `None` while the default is a Bluetooth
/// sink, so a user who chose headphones is never switched back to the TV.
pub fn preferred_tv_sink(sinks: &[Sink]) -> Option<u32> {
    let default_kind = sinks.iter().find(|s| s.is_default).map(|s| s.kind);
    if matches!(default_kind, Some(SinkKind::Hdmi | SinkKind::Bluetooth)) {
        return None;
    }
    sinks
        .iter()
        .find(|s| s.kind == SinkKind::Hdmi)
        .map(|s| s.id)
}

/// Where sound should go after the outputs changed from `before` to `now`:
/// a Bluetooth output that just appeared (headphones, a speaker) takes the
/// sound; when the Bluetooth output that had it goes away, sound goes back to
/// the TV (HDMI). `None` when nothing needs to change.
pub fn route_on_change(before: &[Sink], now: &[Sink]) -> Option<u32> {
    let key = |s: &Sink| s.node_name.clone().unwrap_or_else(|| s.name.clone());
    let was = |s: &Sink| before.iter().any(|b| key(b) == key(s));
    if let Some(new) = now
        .iter()
        .find(|s| s.kind == SinkKind::Bluetooth && !was(s))
    {
        return (!new.is_default).then_some(new.id);
    }
    let lost_bt = before
        .iter()
        .any(|b| b.kind == SinkKind::Bluetooth && !now.iter().any(|s| key(s) == key(b)));
    let default_kind = now.iter().find(|s| s.is_default).map(|s| s.kind);
    if lost_bt && !matches!(default_kind, Some(SinkKind::Hdmi | SinkKind::Bluetooth)) {
        return now.iter().find(|s| s.kind == SinkKind::Hdmi).map(|s| s.id);
    }
    None
}

// ---------------------------------------------------------------------------
// Card profiles (pactl, via pipewire-pulse)
// ---------------------------------------------------------------------------
//
// On many PCs the HDMI sink does not exist until the sound card's *profile*
// is switched to an HDMI output profile (e.g. the GPU's HDMI audio card
// starts on `off` or `pro-audio`, or an Intel card on `analog-stereo`).
// `wpctl set-profile DEVICE_ID INDEX` exists but needs the numeric profile
// index, which only `wpctl inspect`/`pw-dump` (JSON) expose; `pactl list
// cards` prints names, availability and priority as text, so CatosTV uses
// `pactl` for this:
//
//   pactl list cards                               → parse_pactl_cards
//   pactl set-card-profile CARD_NAME PROFILE_NAME  → set_card_profile
//
// After switching, re-run `sinks()`; the new HDMI sink appears within a
// moment and can be made default with `set_default_sink`.

/// One profile line of `pactl list cards`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardProfile {
    /// e.g. `output:hdmi-stereo-extra1`
    pub name: String,
    /// e.g. `Digital Stereo (HDMI 2) Output`
    pub description: String,
    pub sinks: u32,
    pub sources: u32,
    pub priority: u32,
    /// `available: no` means nothing is plugged into that port (no ELD).
    pub available: bool,
}

impl CardProfile {
    /// An output profile that creates an HDMI/DisplayPort sink.
    pub fn is_hdmi_output(&self) -> bool {
        self.sinks > 0 && self.name.to_ascii_lowercase().contains("hdmi")
    }
}

/// One card of `pactl list cards`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Card {
    pub index: u32,
    /// e.g. `alsa_card.pci-0000_01_00.1` (argument for `set-card-profile`).
    pub name: String,
    /// `device.description` property, e.g. "HDA NVidia".
    pub description: String,
    pub profiles: Vec<CardProfile>,
    pub active_profile: String,
}

/// Parse a profile line, e.g.
/// `output:hdmi-stereo: Digital Stereo (HDMI) Output (sinks: 1, sources: 0, priority: 5900, available: no)`.
fn parse_profile_line(t: &str) -> Option<CardProfile> {
    let (name, rest) = t.split_once(": ")?;
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    let open = rest.rfind(" (sinks:")?;
    let description = rest[..open].trim().to_string();
    let attrs = rest[open + 2..].trim_end().trim_end_matches(')');
    let mut p = CardProfile {
        name: name.to_string(),
        description,
        sinks: 0,
        sources: 0,
        priority: 0,
        available: true, // older servers print no `available` field
    };
    for kv in attrs.split(',') {
        let Some((k, v)) = kv.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "sinks" => p.sinks = v.parse().unwrap_or(0),
            "sources" => p.sources = v.parse().unwrap_or(0),
            "priority" => p.priority = v.parse().unwrap_or(0),
            "available" => p.available = v != "no",
            _ => {}
        }
    }
    Some(p)
}

/// Parse `pactl list cards` (English output, PulseAudio 15+/pipewire-pulse).
pub fn parse_pactl_cards(text: &str) -> Vec<Card> {
    let mut cards: Vec<Card> = Vec::new();
    let mut in_profiles = false;
    for raw in text.lines() {
        let t = raw.trim();
        if let Some(n) = t.strip_prefix("Card #") {
            cards.push(Card {
                index: n.trim().parse().unwrap_or(0),
                ..Card::default()
            });
            in_profiles = false;
            continue;
        }
        let Some(card) = cards.last_mut() else {
            continue;
        };
        if t == "Profiles:" {
            in_profiles = true;
            continue;
        }
        if let Some(v) = t.strip_prefix("Active Profile:") {
            card.active_profile = v.trim().to_string();
            in_profiles = false;
            continue;
        }
        if t.ends_with(':') && !t.contains(' ') {
            // Another block header (Properties:, Ports:, …).
            in_profiles = false;
            continue;
        }
        if in_profiles {
            if let Some(p) = parse_profile_line(t) {
                card.profiles.push(p);
            }
            continue;
        }
        if let Some(v) = t.strip_prefix("Name:")
            && card.name.is_empty()
        {
            card.name = v.trim().to_string();
        } else if let Some(v) = t.strip_prefix("device.description = ")
            && card.description.is_empty()
        {
            card.description = v.trim().trim_matches('"').to_string();
        }
    }
    cards
}

/// A card profile switch CatosTV could offer ("Use TV speakers").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSwitch {
    pub card: String,
    pub card_description: String,
    pub profile: String,
    pub profile_description: String,
}

fn hdmi_candidates(cards: &[Card]) -> Vec<ProfileSwitch> {
    let mut out = Vec::new();
    for c in cards {
        let mut profiles: Vec<&CardProfile> = c
            .profiles
            .iter()
            .filter(|p| p.is_hdmi_output() && p.available)
            .collect();
        // Plain PCM stereo first (works with every TV; AC3/DTS passthrough
        // profiles need encoder plugins), then PipeWire's own priority.
        let stereo = |p: &CardProfile| p.name.contains("hdmi-stereo");
        profiles.sort_by(|a, b| stereo(b).cmp(&stereo(a)).then(b.priority.cmp(&a.priority)));
        out.extend(profiles.into_iter().map(|p| ProfileSwitch {
            card: c.name.clone(),
            card_description: c.description.clone(),
            profile: p.name.clone(),
            profile_description: p.description.clone(),
        }));
    }
    out
}

/// The best profile switch that would create an HDMI sink, or `None` if a
/// card already uses an HDMI profile or no HDMI port has a display attached
/// (`available: no`). Picks, on the first card that has one, an available
/// HDMI output profile: plain stereo before surround/passthrough, then
/// highest priority.
pub fn hdmi_profile_hint(cards: &[Card]) -> Option<ProfileSwitch> {
    let already = cards.iter().any(|c| {
        c.profiles
            .iter()
            .any(|p| p.name == c.active_profile && p.is_hdmi_output())
    });
    if already {
        return None;
    }
    hdmi_candidates(cards).into_iter().next()
}

/// List cards (`pactl list cards`).
pub fn cards(r: &mut dyn Runner) -> Result<Vec<Card>> {
    let out = r.run_ok(&Cmd::new("pactl").args(["list", "cards"]))?;
    Ok(parse_pactl_cards(&out.stdout))
}

/// Every available HDMI output profile on every card, best first per card.
pub fn hdmi_profiles(r: &mut dyn Runner) -> Result<Vec<ProfileSwitch>> {
    Ok(hdmi_candidates(&cards(r)?))
}

/// A card or profile name safe to pass as an argv element.
pub fn is_valid_pa_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:+@".contains(c))
}

/// `pactl set-card-profile CARD PROFILE`.
pub fn set_card_profile(r: &mut dyn Runner, card: &str, profile: &str) -> Result<()> {
    if !is_valid_pa_name(card) || !is_valid_pa_name(profile) {
        return Err(Error::msg("invalid sound card or profile name"));
    }
    r.run_ok(&Cmd::new("pactl").args(["set-card-profile", card, profile]))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Output, fake::FakeRunner};

    #[test]
    fn parse() {
        assert_eq!(
            parse_volume("Volume: 0.45\n"),
            Some(VolumeState {
                percent: 45,
                muted: false
            })
        );
        assert_eq!(
            parse_volume("Volume: 1.30 [MUTED]"),
            Some(VolumeState {
                percent: 100,
                muted: true
            })
        );
        assert_eq!(parse_volume("Volume: 0.004").unwrap().percent, 0);
        assert_eq!(parse_volume("garbage"), None);
    }

    #[test]
    fn step_uses_relative_change_with_limit() {
        let mut r = FakeRunner::new();
        r.push(Output::ok(""));
        r.push(Output::ok("Volume: 0.50"));
        let v = step(&mut r, -5).unwrap();
        assert_eq!(v.percent, 50);
        assert_eq!(
            r.seen[0].args,
            vec!["set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", "5%-"]
        );
        r.push(Output::ok(""));
        r.push(Output::ok(""));
        r.push(Output::ok("Volume: 0.55"));
        assert_eq!(step(&mut r, 5).unwrap().percent, 55);
        assert_eq!(r.seen[2].args[4], "5%+");
        // Up unmutes.
        assert_eq!(
            r.seen[3].args,
            vec!["set-mute", "@DEFAULT_AUDIO_SINK@", "0"]
        );
        let mut r = FakeRunner::new();
        r.push(Output::ok(""));
        r.push(Output::ok("Volume: 0.95"));
        assert_eq!(set(&mut r, 95, false).unwrap().percent, 95);
        assert_eq!(
            r.seen[0].args,
            vec!["set-volume", "@DEFAULT_AUDIO_SINK@", "0.95"]
        );
        assert_eq!(r.seen.len(), 2);
    }

    /// `wpctl status` as posted on bbs.archlinux.org topic 305641
    /// (WirePlumber 0.5, PipeWire 1.4): Audio, Video and Settings sections
    /// as posted (a few device/source lines trimmed); the PipeWire header
    /// and client list are a placeholder in the same format.
    const STATUS: &str = "\
PipeWire 'pipewire-0' [1.4.2, user@host, cookie:1234]
 └─ Clients:
        33. WirePlumber                         [1.4.2, user@host, pid:1201]

Audio
 ├─ Devices:
 │      53. HDA NVidia                          [alsa]
 │      56. USB Audio                           [alsa]
 │      57. Built-in Audio                      [alsa]
 │
 ├─ Sinks:
 │      38. Yeti Stereo Microphone Analog Stereo [vol: 0.00]
 │      43. USB Audio Front Headphones          [vol: 1.00]
 │      45. USB Audio S/PDIF Output             [vol: 1.00]
 │      67. USB Audio Speakers                  [vol: 1.00]
 │  *  111. HDA NVidia Digital Stereo (HDMI 2)  [vol: 1.00]
 │
 ├─ Sources:
 │      44. USB Camera Analog Stereo            [vol: 1.00]
 │  *   46. Yeti Stereo Microphone Analog Stereo [vol: 1.00]
 │
 ├─ Filters:
 │
 └─ Streams:

Video
 ├─ Devices:
 │      60. USB Camera                          [v4l2]
 ├─ Sinks:
 │      99. Not An Audio Sink
 ├─ Sources:
 ├─ Filters:
 └─ Streams:

Settings
 └─ Default Configured Devices:
         0. Audio/Sink    alsa_output.pci-0000_01_00.1.hdmi-stereo-extra1
";

    /// Same layout with node names (`wpctl status --name`); CONSTRUCTED
    /// from STATUS using the node-name patterns PipeWire's ALSA monitor
    /// assigns. The ` MUTED` tag follows wpctl.c (` [vol: %.2f MUTED]`).
    const STATUS_NAMES: &str = "\
Audio
 ├─ Sinks:
 │      38. alsa_output.usb-Blue_Microphones_Yeti_Stereo_Microphone_REV8-00.analog-stereo [vol: 0.00]
 │      43. alsa_output.usb-Generic_USB_Audio-00.HiFi__Headphones__sink [vol: 1.00]
 │      45. alsa_output.usb-Generic_USB_Audio-00.HiFi__SPDIF__sink [vol: 1.00]
 │      67. alsa_output.usb-Generic_USB_Audio-00.HiFi__Speaker__sink [vol: 1.00 MUTED]
 │  *  111. alsa_output.pci-0000_01_00.1.hdmi-stereo-extra1 [vol: 1.00]
 ├─ Sources:
";

    /// Laptop with Bluetooth headphones as default; CONSTRUCTED in the
    /// format above.
    const STATUS_BT: &str = "\
Audio
 ├─ Sinks:
 │      48. Built-in Audio Analog Stereo        [vol: 0.40 MUTED]
 │      62. Built-in Audio Digital Stereo (HDMI) [vol: 1.00]
 │  *   70. WH-1000XM4                          [vol: 0.55]
 ├─ Sources:
";
    const STATUS_BT_NAMES: &str = "\
Audio
 ├─ Sinks:
 │      48. alsa_output.pci-0000_00_1f.3.analog-stereo [vol: 0.40 MUTED]
 │      62. alsa_output.pci-0000_00_1f.3.hdmi-stereo [vol: 1.00]
 │  *   70. bluez_output.AC_80_0A_12_34_56.1    [vol: 0.55]
 ├─ Sources:
";

    #[test]
    fn parses_only_audio_sinks() {
        let e = parse_wpctl_sinks(STATUS);
        let ids: Vec<u32> = e.iter().map(|s| s.id).collect();
        assert_eq!(ids, [38, 43, 45, 67, 111]);
        assert_eq!(e[0].text, "Yeti Stereo Microphone Analog Stereo");
        assert_eq!(e[4].text, "HDA NVidia Digital Stereo (HDMI 2)");
        assert!(e[4].is_default);
        assert!(!e[0].is_default);
        assert!(parse_wpctl_sinks("").is_empty());
        assert!(parse_wpctl_sinks("Audio\n ├─ Sinks:\n │  junk line\n").is_empty());
    }

    #[test]
    fn sinks_merge_names_and_classify() {
        let mut r = FakeRunner::new();
        r.push(Output::ok(STATUS));
        r.push(Output::ok(STATUS_NAMES));
        let s = sinks(&mut r).unwrap();
        assert_eq!(r.seen[0].args, vec!["status"]);
        assert_eq!(r.seen[1].args, vec!["status", "--name"]);
        let kinds: Vec<SinkKind> = s.iter().map(|x| x.kind).collect();
        assert_eq!(
            kinds,
            [
                SinkKind::Usb,
                SinkKind::Usb,
                SinkKind::Usb,
                SinkKind::Usb,
                SinkKind::Hdmi
            ]
        );
        assert_eq!(
            s[4].node_name.as_deref(),
            Some("alsa_output.pci-0000_01_00.1.hdmi-stereo-extra1")
        );
        assert_eq!(preferred_tv_sink(&s), None, "default is already HDMI");
    }

    #[test]
    fn names_query_is_optional() {
        let mut r = FakeRunner::new();
        r.push(Output::ok(STATUS_BT));
        r.push(Output::failed(1, "Unknown option --name"));
        let s = sinks(&mut r).unwrap();
        assert!(s.iter().all(|x| x.node_name.is_none()));
        // Without node names the headphones are only "Other".
        assert_eq!(s[2].kind, SinkKind::Other);
        assert_eq!(s[0].kind, SinkKind::Analog);
        assert_eq!(s[1].kind, SinkKind::Hdmi);
        // Default "Other" is not protected → suggest the HDMI sink.
        assert_eq!(preferred_tv_sink(&s), Some(62));
    }

    #[test]
    fn bluetooth_takes_the_sound_and_gives_it_back() {
        let sink = |id, kind, default| Sink {
            id,
            name: format!("sink {id}"),
            node_name: Some(format!("node{id}")),
            is_default: default,
            kind,
        };
        let tv = vec![sink(53, SinkKind::Hdmi, true)];
        let buds = vec![
            sink(53, SinkKind::Hdmi, true),
            sink(65, SinkKind::Bluetooth, false),
        ];
        assert_eq!(route_on_change(&tv, &buds), Some(65), "buds connect");
        assert_eq!(route_on_change(&buds, &buds), None, "nothing changed");
        // Disconnected: WirePlumber may fall back to another output.
        let after = vec![
            sink(53, SinkKind::Hdmi, false),
            sink(54, SinkKind::Analog, true),
        ];
        let had = vec![
            sink(53, SinkKind::Hdmi, false),
            sink(54, SinkKind::Analog, false),
            sink(65, SinkKind::Bluetooth, true),
        ];
        assert_eq!(route_on_change(&had, &after), Some(53), "back to the TV");
        let hdmi_already = vec![sink(53, SinkKind::Hdmi, true)];
        assert_eq!(route_on_change(&had, &hdmi_already), None);
        // At start-up (nothing known yet) connected buds get the sound.
        assert_eq!(route_on_change(&[], &buds), Some(65));
    }

    #[test]
    fn tv_sink_heuristic() {
        let s = merge_sinks(
            &parse_wpctl_sinks(STATUS_BT),
            &parse_wpctl_sinks(STATUS_BT_NAMES),
        );
        assert_eq!(s[2].kind, SinkKind::Bluetooth);
        assert_eq!(preferred_tv_sink(&s), None, "never steal from headphones");

        let mut analog_default = s.clone();
        analog_default[2].is_default = false;
        analog_default[0].is_default = true;
        assert_eq!(preferred_tv_sink(&analog_default), Some(62));

        let no_hdmi: Vec<Sink> = analog_default
            .into_iter()
            .filter(|x| x.kind != SinkKind::Hdmi)
            .collect();
        assert_eq!(preferred_tv_sink(&no_hdmi), None);
        assert_eq!(preferred_tv_sink(&[]), None);
    }

    #[test]
    fn classify_examples() {
        assert_eq!(
            classify_sink(
                "GA102 High Definition Audio Controller Digital Stereo (HDMI)",
                None
            ),
            SinkKind::Hdmi
        );
        assert_eq!(
            classify_sink(
                "Navi 21 HDMI/DP Audio",
                Some("alsa_output.pci-0000_03_00.1.hdmi-stereo")
            ),
            SinkKind::Hdmi
        );
        assert_eq!(
            classify_sink("Built-in Audio Analog Stereo", None),
            SinkKind::Analog
        );
        assert_eq!(
            classify_sink("Digital Stereo (IEC958)", None),
            SinkKind::Other
        );
    }

    #[test]
    fn set_default_argv() {
        let mut r = FakeRunner::new();
        set_default_sink(&mut r, 62).unwrap();
        assert_eq!(r.seen[0].display(), "wpctl set-default 62");
    }

    /// `pactl list cards` (pipewire-pulse): card #49 (NVIDIA) is from
    /// bbs.archlinux.org topic 297827 (Ports trimmed, Properties shortened);
    /// card #50 (Intel analog + HDMI) is CONSTRUCTED in the same format.
    const CARDS: &str = "\
Card #49
\tName: alsa_card.pci-0000_01_00.1
\tDriver: alsa
\tOwner Module: n/a
\tProperties:
\t\tdevice.description = \"HDA NVidia\"
\t\tdevice.name = \"alsa_card.pci-0000_01_00.1\"
\tProfiles:
\t\toff: Off (sinks: 0, sources: 0, priority: 0, available: yes)
\t\toutput:hdmi-stereo: Digital Stereo (HDMI) Output (sinks: 1, sources: 0, priority: 5900, available: no)
\t\toutput:hdmi-surround: Digital Surround 5.1 (HDMI) Output (sinks: 1, sources: 0, priority: 800, available: no)
\t\toutput:hdmi-surround71: Digital Surround 7.1 (HDMI) Output (sinks: 1, sources: 0, priority: 800, available: no)
\t\toutput:hdmi-ac3-surround: Digital Surround 5.1 (HDMI/AC3) Output (sinks: 1, sources: 0, priority: 100, available: yes)
\t\tpro-audio: Pro Audio (sinks: 1, sources: 0, priority: 1, available: yes)
\tActive Profile: pro-audio
\tPorts:
\t\thdmi-output-0: HDMI / DisplayPort (type: HDMI, priority: 5900, latency offset: 0 usec, availability group: Legacy 1, not available)
\t\t\tProperties:
\t\t\t\tport.type = \"hdmi\"
\t\t\tPart of profile(s): output:hdmi-stereo, output:hdmi-surround, output:hdmi-surround71

Card #50
\tName: alsa_card.pci-0000_00_1f.3
\tDriver: alsa
\tProperties:
\t\tdevice.description = \"Built-in Audio\"
\tProfiles:
\t\toff: Off (sinks: 0, sources: 0, priority: 0, available: yes)
\t\toutput:analog-stereo+input:analog-stereo: Analog Stereo Duplex (sinks: 1, sources: 1, priority: 6565, available: yes)
\t\toutput:hdmi-stereo: Digital Stereo (HDMI) Output (sinks: 1, sources: 0, priority: 5900, available: yes)
\t\toutput:hdmi-stereo+input:analog-stereo: Digital Stereo (HDMI) Output + Analog Stereo Input (sinks: 1, sources: 1, priority: 5965, available: yes)
\t\tinput:analog-stereo: Analog Stereo Input (sinks: 0, sources: 1, priority: 65, available: yes)
\tActive Profile: output:analog-stereo+input:analog-stereo
\tPorts:
\t\tanalog-output-speaker: Speakers (type: Speaker, priority: 10000, latency offset: 0 usec, availability unknown)
";

    #[test]
    fn parses_cards() {
        let c = parse_pactl_cards(CARDS);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].index, 49);
        assert_eq!(c[0].name, "alsa_card.pci-0000_01_00.1");
        assert_eq!(c[0].description, "HDA NVidia");
        assert_eq!(c[0].active_profile, "pro-audio");
        assert_eq!(c[0].profiles.len(), 6);
        let p = &c[0].profiles[1];
        assert_eq!(p.name, "output:hdmi-stereo");
        assert_eq!(p.description, "Digital Stereo (HDMI) Output");
        assert_eq!(
            (p.sinks, p.sources, p.priority, p.available),
            (1, 0, 5900, false)
        );
        assert!(p.is_hdmi_output());
        assert!(!c[0].profiles[0].is_hdmi_output());
        assert_eq!(
            c[1].profiles[1].name,
            "output:analog-stereo+input:analog-stereo"
        );
        assert_eq!(c[1].profiles[1].sources, 1);
        assert!(parse_pactl_cards("garbage\n\tProfiles:\n").is_empty());
    }

    #[test]
    fn hdmi_hint_picks_available_highest_priority() {
        let c = parse_pactl_cards(CARDS);
        let hint = hdmi_profile_hint(&c).unwrap();
        // NVIDIA: only the AC3 profile is available (TV attached there).
        assert_eq!(hint.card, "alsa_card.pci-0000_01_00.1");
        assert_eq!(hint.profile, "output:hdmi-ac3-surround");
        assert_eq!(hint.card_description, "HDA NVidia");

        let all = hdmi_candidates(&c);
        let names: Vec<&str> = all.iter().map(|p| p.profile.as_str()).collect();
        assert_eq!(
            names,
            [
                "output:hdmi-ac3-surround",
                "output:hdmi-stereo+input:analog-stereo",
                "output:hdmi-stereo"
            ]
        );

        // Stereo is preferred over a higher-priority surround profile.
        let mut surround_first = c[0].clone();
        for p in &mut surround_first.profiles {
            p.available = true;
        }
        surround_first.profiles[2].priority = 9000;
        assert_eq!(
            hdmi_profile_hint(&[surround_first]).unwrap().profile,
            "output:hdmi-stereo"
        );

        // A card already on an HDMI profile → no hint.
        let mut on_hdmi = c.clone();
        on_hdmi[1].active_profile = "output:hdmi-stereo".into();
        assert_eq!(hdmi_profile_hint(&on_hdmi), None);
    }

    #[test]
    fn profile_commands() {
        let mut r = FakeRunner::new();
        r.push(Output::ok(CARDS));
        assert_eq!(hdmi_profiles(&mut r).unwrap().len(), 3);
        assert_eq!(r.seen[0].display(), "pactl list cards");
        set_card_profile(&mut r, "alsa_card.pci-0000_00_1f.3", "output:hdmi-stereo").unwrap();
        assert_eq!(
            r.seen[1].display(),
            "pactl set-card-profile alsa_card.pci-0000_00_1f.3 output:hdmi-stereo"
        );
        for (card, profile) in [
            ("", "x"),
            ("-h", "x"),
            ("card", "--help"),
            ("a b", "x"),
            ("card", "x;y"),
        ] {
            assert!(set_card_profile(&mut r, card, profile).is_err());
        }
        assert_eq!(r.seen.len(), 2);
    }
}
