//! Display outputs via `wlr-randr` (wlr-output-management protocol).
//!
//! labwc implements `zwlr_output_manager_v1`, so `wlr-randr` (Arch `extra/
//! wlr-randr` 0.5.0) can list outputs and change mode, scale and adaptive
//! sync at runtime. CatosTV parses the **plain text** output; `wlr-randr 0.5`
//! also has `--json`, but core has no JSON parser and the text format has
//! been stable since 0.2.
//!
//! ## Text format (wlr-randr 0.5, `print_state` in upstream `main.c`)
//!
//! ```text
//! HDMI-A-1 "Samsung Electric Company SAMSUNG 0x01000E00 (HDMI-A-1)"   ← no indent
//!   Make: Samsung Electric Company                                     ← 2 spaces
//!   Physical size: 1600x900 mm        (only when known)
//!   Enabled: yes
//!   Modes:
//!     3840x2160 px, 60.000000 Hz (preferred, current)                  ← 4 spaces
//!   Position: 0,0                     (only when enabled)
//!   Transform: normal
//!   Scale: 1.000000
//!   Adaptive Sync: disabled
//! ```
//!
//! The refresh rate is printed as `%f` of `refresh_mHz / 1000.0` and the
//! `, … Hz` part is omitted when the compositor reports refresh 0.
//!
//! ## Setting a mode
//!
//! `wlr-randr --output NAME --mode WxH@R.RRRHz`. wlr-randr converts `R` to
//! millihertz with `round(strtod(R) * 1000)` and then requires an **exact**
//! integer match against an advertised mode (no tolerance): a real report on
//! bbs.archlinux.org (topic 305821) shows `--mode 1920x1080@165` failing
//! with `unknown mode: 1920x1080@165` because the mode is 164.996002 Hz.
//! [`Mode::wlr_arg`] therefore always prints the refresh with three decimals
//! derived from the stored millihertz, which round-trips exactly.
//!
//! ## HDR
//!
//! See [`hdr_supported`]: wlr-randr exposes no HDR capability, so CatosTV
//! cannot truthfully offer an HDR switch yet.
//!
//! Status: parser and argv construction unit-tested against captured text;
//! behaviour on real displays UNVERIFIED.

use crate::error::{Error, Result};
use crate::process::{Cmd, Runner};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// One display mode as advertised by the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    /// Refresh rate in millihertz (60 Hz = 60000), rounded. 0 = unknown.
    pub refresh_mhz: u32,
    pub preferred: bool,
    pub current: bool,
}

impl Mode {
    /// Same resolution and refresh rate (flags ignored).
    pub fn same_timing(&self, other: &Mode) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.refresh_mhz == other.refresh_mhz
    }

    /// `(width, height, refresh_mhz)`, the key used in [`DisplayPrefs`].
    pub fn key(&self) -> (u32, u32, u32) {
        (self.width, self.height, self.refresh_mhz)
    }

    /// "60 Hz", "59.94 Hz", "164.99 Hz"; empty when unknown. Decimals are
    /// shown only when the rate is not a whole number (to 0.01 Hz).
    pub fn refresh_label(&self) -> String {
        if self.refresh_mhz == 0 {
            return String::new();
        }
        let centi = (self.refresh_mhz + 5) / 10; // hundredths of a Hz, rounded
        let (whole, frac) = (centi / 100, centi % 100);
        if frac == 0 {
            format!("{whole} Hz")
        } else if frac % 10 == 0 {
            format!("{whole}.{} Hz", frac / 10)
        } else {
            format!("{whole}.{frac:02} Hz")
        }
    }

    /// TV menu label: "3840 × 2160 · 60 Hz" (or "3840 × 2160" without a
    /// known refresh rate).
    pub fn label(&self) -> String {
        let res = format!("{} × {}", self.width, self.height);
        match self.refresh_label() {
            r if r.is_empty() => res,
            r => format!("{res} · {r}"),
        }
    }

    /// Argument for `wlr-randr --mode`: `3840x2160@60.000Hz`, or `3840x2160`
    /// when the refresh rate is unknown.
    pub fn wlr_arg(&self) -> String {
        if self.refresh_mhz == 0 {
            format!("{}x{}", self.width, self.height)
        } else {
            format!(
                "{}x{}@{}.{:03}Hz",
                self.width,
                self.height,
                self.refresh_mhz / 1000,
                self.refresh_mhz % 1000
            )
        }
    }
}

/// One output (connector) as reported by `wlr-randr`.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    /// Connector name, e.g. `HDMI-A-1`. Used for `--output`.
    pub name: String,
    /// Compositor description, e.g. `Samsung Electric Company SAMSUNG … (HDMI-A-1)`.
    pub description: String,
    pub make: String,
    pub model: String,
    pub enabled: bool,
    /// In the order the compositor listed them (use [`menu_modes`] for a menu).
    pub modes: Vec<Mode>,
    /// 1.0 when not reported (disabled outputs print no scale).
    pub scale: f32,
    /// `None` when the compositor/wlr-randr does not report adaptive sync.
    pub adaptive_sync: Option<bool>,
    /// Physical size in millimetres, when the EDID provides it.
    pub physical_mm: Option<(u32, u32)>,
}

impl Output {
    fn new(name: &str, description: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            make: String::new(),
            model: String::new(),
            enabled: false,
            modes: Vec::new(),
            scale: 1.0,
            adaptive_sync: None,
            physical_mm: None,
        }
    }

    pub fn current_mode(&self) -> Option<&Mode> {
        self.modes.iter().find(|m| m.current)
    }

    pub fn preferred_mode(&self) -> Option<&Mode> {
        self.modes.iter().find(|m| m.preferred)
    }

    /// The advertised mode with exactly this timing, if any.
    pub fn find_mode(&self, width: u32, height: u32, refresh_mhz: u32) -> Option<&Mode> {
        self.modes
            .iter()
            .find(|m| m.width == width && m.height == height && m.refresh_mhz == refresh_mhz)
    }

    /// Friendly name for a TV menu: "Samsung" (brand repeated as model is
    /// dropped), "LG OLED65C1", falling back to the
    /// connector name.
    pub fn display_name(&self) -> String {
        let make = self.make.split_whitespace().next().unwrap_or("");
        // "Samsung" + "SAMSUNG" → "Samsung" (TVs often repeat the brand).
        let model = if self.model.eq_ignore_ascii_case(make) {
            ""
        } else {
            self.model.as_str()
        };
        let joined = format!("{make} {model}");
        let joined = joined.trim();
        if joined.is_empty() {
            self.name.clone()
        } else {
            joined.to_string()
        }
    }
}

/// Parse one mode line (already trimmed), e.g.
/// `3840x2160 px, 60.000000 Hz (preferred, current)` or `1024x768 px`.
fn parse_mode_line(line: &str) -> Option<Mode> {
    let (res, rest) = line.split_once(" px")?;
    let (w, h) = res.trim().split_once('x')?;
    let width = w.trim().parse().ok()?;
    let height = h.trim().parse().ok()?;
    let rest = rest.trim_start_matches(',').trim();
    let mut refresh_mhz = 0;
    if let Some(hz_at) = rest.find(" Hz") {
        let hz: f64 = rest[..hz_at].trim().parse().ok()?;
        if !hz.is_finite() || hz < 0.0 {
            return None;
        }
        refresh_mhz = (hz * 1000.0).round() as u32;
    }
    let flags = rest.find('(').map(|i| &rest[i..]).unwrap_or("").to_string();
    Some(Mode {
        width,
        height,
        refresh_mhz,
        preferred: flags.contains("preferred"),
        current: flags.contains("current"),
    })
}

/// Parse the plain-text output of `wlr-randr` (0.2–0.5).
///
/// Robust to unknown property lines (ignored), missing optional lines,
/// unindented junk (ignored, never attached to an output) and CRLF.
/// Outputs with no modes (e.g. virtual/headless) are still returned.
pub fn parse_wlr_randr(text: &str) -> Vec<Output> {
    let mut outputs: Vec<Output> = Vec::new();
    let mut in_modes = false;
    // False after an unindented line that is not a header, so stray text
    // never attaches properties to the previous output.
    let mut in_output = false;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            // Header: NAME "description"
            in_modes = false;
            let (name, desc) = line.split_once(' ').unwrap_or((line, ""));
            let desc = desc.trim();
            in_output = is_valid_output_name(name) && (desc.is_empty() || desc.starts_with('"'));
            if in_output {
                let desc = desc
                    .strip_prefix('"')
                    .and_then(|d| d.strip_suffix('"'))
                    .unwrap_or(desc);
                outputs.push(Output::new(name, desc));
            }
            continue;
        }
        let Some(out) = outputs.last_mut().filter(|_| in_output) else {
            continue;
        };
        let t = line.trim();
        if in_modes && let Some(mode) = parse_mode_line(t) {
            out.modes.push(mode);
            continue;
        }
        let Some((key, value)) = t.split_once(':') else {
            continue;
        };
        let value = value.trim();
        in_modes = false;
        match key.trim() {
            "Make" => out.make = value.to_string(),
            "Model" => out.model = value.to_string(),
            "Enabled" => out.enabled = value == "yes",
            "Modes" => in_modes = true,
            "Scale" => {
                if let Ok(s) = value.parse::<f32>()
                    && s.is_finite()
                    && s > 0.0
                {
                    out.scale = s;
                }
            }
            "Adaptive Sync" => {
                out.adaptive_sync = match value {
                    "enabled" | "yes" => Some(true),
                    "disabled" | "no" => Some(false),
                    _ => None,
                }
            }
            "Physical size" => {
                out.physical_mm = value
                    .trim_end_matches("mm")
                    .trim()
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
                    .filter(|&(w, h): &(u32, u32)| w > 0 && h > 0);
            }
            _ => {}
        }
    }
    outputs
}

/// A connector name we are willing to pass as an argv element: letters,
/// digits, `-`, `_`, `.`, `:`; not starting with `-`; at most 64 bytes.
pub fn is_valid_output_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c))
}

fn check_output(name: &str) -> Result<()> {
    if is_valid_output_name(name) {
        Ok(())
    } else {
        Err(Error::msg(format!("invalid display output name {name:?}")))
    }
}

/// Smallest and largest scale CatosTV will apply.
pub const MIN_SCALE: f32 = 0.5;
pub const MAX_SCALE: f32 = 4.0;

fn check_scale(scale: f32) -> Result<()> {
    if scale.is_finite() && (MIN_SCALE..=MAX_SCALE).contains(&scale) {
        Ok(())
    } else {
        Err(Error::msg(format!("display scale {scale} is out of range")))
    }
}

/// List outputs. Runs `wlr-randr` (~tens of ms); call off the UI thread.
pub fn outputs(r: &mut dyn Runner) -> Result<Vec<Output>> {
    let out = r.run_ok(&Cmd::new("wlr-randr"))?;
    Ok(parse_wlr_randr(&out.stdout))
}

/// Switch `output` to `mode`. The mode should come from that output's
/// advertised list (wlr-randr rejects anything else with `unknown mode`).
pub fn set_mode(r: &mut dyn Runner, output: &str, mode: &Mode) -> Result<()> {
    check_output(output)?;
    if mode.width == 0 || mode.height == 0 {
        return Err(Error::msg("invalid display mode"));
    }
    r.run_ok(&Cmd::new("wlr-randr").args(["--output", output, "--mode", &mode.wlr_arg()]))?;
    Ok(())
}

/// Switch `output` back to the mode the display marks as preferred
/// (`wlr-randr --preferred`, "Automatic" in a TV menu).
pub fn set_preferred(r: &mut dyn Runner, output: &str) -> Result<()> {
    check_output(output)?;
    r.run_ok(&Cmd::new("wlr-randr").args(["--output", output, "--preferred"]))?;
    Ok(())
}

/// Set the output scale ([`MIN_SCALE`]–[`MAX_SCALE`]).
pub fn set_scale(r: &mut dyn Runner, output: &str, scale: f32) -> Result<()> {
    check_output(output)?;
    check_scale(scale)?;
    r.run_ok(&Cmd::new("wlr-randr").args(["--output", output, "--scale", &format!("{scale}")]))?;
    Ok(())
}

/// Enable/disable adaptive sync (VRR): `--adaptive-sync enabled|disabled`.
///
/// labwc only honours runtime changes when `rc.xml` has
/// `<core><adaptiveSync>yes</adaptiveSync></core>` (labwc issue #1412: with
/// `no`/`fullscreen` the compositor overrides it). Check
/// [`Output::adaptive_sync`] afterwards to see what actually happened.
pub fn set_adaptive_sync(r: &mut dyn Runner, output: &str, on: bool) -> Result<()> {
    check_output(output)?;
    r.run_ok(&Cmd::new("wlr-randr").args([
        "--output",
        output,
        "--adaptive-sync",
        if on { "enabled" } else { "disabled" },
    ]))?;
    Ok(())
}

/// The mode to pass through when the current one has to be set again
/// from scratch: the same size at about half the refresh rate (60 → 30),
/// else any other mode. `None` without a current mode or an alternative.
pub fn bounce_mode(output: &Output) -> Option<Mode> {
    let cur = *output.current_mode()?;
    let half = cur.refresh_mhz / 2;
    let others = output.modes.iter().filter(|m| !m.same_timing(&cur));
    others
        .clone()
        .filter(|m| m.width == cur.width && m.height == cur.height)
        .min_by_key(|m| m.refresh_mhz.abs_diff(half))
        .or_else(|| others.clone().next())
        .copied()
}

/// Set every enabled output's mode again from scratch, by way of
/// [`bounce_mode`]. Some DP-to-HDMI chips (a Chrontel CH7218 on the test
/// TV) wake from sleep with the wrong colour conversion, a blue tint,
/// until the next full mode set. Returns how many outputs were reset.
pub fn reset_modes(r: &mut dyn Runner, pause: std::time::Duration) -> Result<usize> {
    let mut done = 0;
    for o in outputs(r)?.iter().filter(|o| o.enabled) {
        let (Some(cur), Some(via)) = (o.current_mode().copied(), bounce_mode(o)) else {
            continue;
        };
        set_mode(r, &o.name, &via)?;
        std::thread::sleep(pause);
        set_mode(r, &o.name, &cur)?;
        done += 1;
    }
    Ok(done)
}

/// Modes for a TV menu: every mode the display reports, de-duplicated by
/// timing (flags merged), sorted by resolution (pixel count, then width)
/// descending, then refresh rate descending.
pub fn menu_modes(output: &Output) -> Vec<Mode> {
    let mut modes: Vec<Mode> = Vec::new();
    for m in &output.modes {
        match modes.iter_mut().find(|x| x.same_timing(m)) {
            Some(x) => {
                x.preferred |= m.preferred;
                x.current |= m.current;
            }
            None => modes.push(*m),
        }
    }
    modes.sort_by(|a, b| {
        let pa = u64::from(a.width) * u64::from(a.height);
        let pb = u64::from(b.width) * u64::from(b.height);
        pb.cmp(&pa)
            .then(b.width.cmp(&a.width))
            .then(b.refresh_mhz.cmp(&a.refresh_mhz))
    });
    modes
}

/// Whether CatosTV can offer an HDR switch for this output. Always `false`.
///
/// Findings (checked 2026-10-14):
/// * Arch ships labwc **0.20.2** (wlroots 0.20), not 0.9.x. labwc 0.20 added
///   HDR10 output (PR #3424) and the `color-management-v1` /
///   `color-representation-manager-v1` protocols (PR #3469).
/// * It is enabled **globally in the compositor config**, not per output at
///   runtime: `rc.xml` `<core><hdr>yes</hdr></core>` ("Automatically enable
///   HDR support on outputs when configuring them", default `no`) and it
///   additionally requires the Vulkan renderer (`WLR_RENDERER=vulkan` in
///   `~/.config/labwc/environment`), labwc-config(5).
/// * `wlr-randr` 0.5 prints no HDR capability or state, and the
///   wlr-output-management protocol has no HDR field, so there is no way to
///   ask whether a given TV accepts HDR10.
///
/// A truthful HDR setting therefore needs a design decision (Vulkan renderer
/// on all supported GPUs? EDID parsing for HDR static metadata?) and real
/// hardware tests. Until then the GUI must show HDR as unavailable.
pub fn hdr_supported(_output: &Output) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Display preferences chosen in CatosTV, re-applied at session start.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DisplayPrefs {
    /// Connector name the preferences belong to (e.g. `HDMI-A-1`).
    pub output: String,
    /// `(width, height, refresh_mhz)`; `None` = the display's preferred mode.
    pub mode: Option<(u32, u32, u32)>,
    /// `None` = compositor default.
    pub scale: Option<f32>,
    /// Overscan margin the GUI leaves on every edge, 0–10 %.
    pub safe_area_pct: u8,
}

/// Largest safe-area margin CatosTV offers.
pub const MAX_SAFE_AREA_PCT: u8 = 10;

impl DisplayPrefs {
    /// Serialise as `key = value` lines.
    pub fn to_conf(&self) -> String {
        let mut s = String::from("# CatosTV display preferences (written by CatosTV)\n");
        if is_valid_output_name(&self.output) {
            s.push_str(&format!("output = {}\n", self.output));
        }
        if let Some((w, h, mhz)) = self.mode {
            s.push_str(&format!("mode = {w}x{h}@{mhz}mHz\n"));
        }
        if let Some(scale) = self.scale.filter(|s| check_scale(*s).is_ok()) {
            s.push_str(&format!("scale = {scale}\n"));
        }
        s.push_str(&format!(
            "safe_area_pct = {}\n",
            self.safe_area_pct.min(MAX_SAFE_AREA_PCT)
        ));
        s
    }

    /// Parse [`DisplayPrefs::to_conf`] output. Never fails: unknown keys,
    /// junk lines and invalid values are ignored (that field keeps its
    /// default), the safe area is clamped to 0–10.
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
            let v = v.trim();
            match k.trim() {
                "output" if is_valid_output_name(v) => p.output = v.to_string(),
                "mode" => p.mode = parse_conf_mode(v),
                "scale" => p.scale = v.parse::<f32>().ok().filter(|s| check_scale(*s).is_ok()),
                "safe_area_pct" => {
                    if let Ok(n) = v.parse::<u32>() {
                        p.safe_area_pct = n.min(u32::from(MAX_SAFE_AREA_PCT)) as u8;
                    }
                }
                _ => {}
            }
        }
        p
    }
}

/// `3840x2160@60000mHz` → `(3840, 2160, 60000)`.
fn parse_conf_mode(v: &str) -> Option<(u32, u32, u32)> {
    let (res, hz) = v.split_once('@')?;
    let (w, h) = res.split_once('x')?;
    let mhz = hz.trim().strip_suffix("mHz").unwrap_or(hz).trim();
    let (w, h, mhz) = (
        w.trim().parse().ok()?,
        h.trim().parse().ok()?,
        mhz.parse().ok()?,
    );
    (w > 0 && h > 0).then_some((w, h, mhz))
}

/// `$HOME/.config/catostv/display.conf` (same home resolution as
/// `env::apps_config_path`).
pub fn prefs_path() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/catostv"))
        .join(".config/catostv/display.conf")
}

/// Load preferences from [`prefs_path`]; defaults if missing or unreadable.
pub fn load_prefs() -> DisplayPrefs {
    std::fs::read_to_string(prefs_path())
        .map(|t| DisplayPrefs::from_conf(&t))
        .unwrap_or_default()
}

/// Save preferences to [`prefs_path`] atomically (temp file + rename).
pub fn save_prefs(prefs: &DisplayPrefs) -> Result<()> {
    let path = prefs_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("conf.tmp");
    std::fs::write(&tmp, prefs.to_conf())?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Interface size for an output nobody chose one for: 200 % from 4K
/// (3200 px wide and up), 150 % for 1440p-class modes, else 100 %.
pub fn default_scale(out: &Output) -> Option<f32> {
    let m = out.current_mode()?;
    Some(if m.width >= 3200 {
        2.0
    } else if m.width >= 2400 {
        1.5
    } else {
        1.0
    })
}

/// Give every enabled output without a saved interface size its
/// [`default_scale`]. Cheap and idempotent: the shell calls it on every
/// system refresh, so a TV plugged in (or a mode changed) later gets the
/// right size too.
pub fn apply_default_scale(
    r: &mut dyn Runner,
    prefs: &DisplayPrefs,
    outputs: &[Output],
) -> Result<()> {
    for out in outputs.iter().filter(|o| o.enabled) {
        let chosen = prefs.output == out.name && prefs.scale.is_some();
        if !chosen
            && let Some(scale) = default_scale(out)
            && (out.scale - scale).abs() > 0.001
        {
            set_scale(r, &out.name, scale)?;
        }
    }
    Ok(())
}

/// Re-apply saved preferences to the matching output.
///
/// Safe by construction:
/// * does nothing if the saved output is not connected now;
/// * applies a saved mode **only if the output currently advertises exactly
///   that timing** (a different TV on the same port, or a stale entry, is
///   silently skipped and the compositor's choice is kept);
/// * skips a mode/scale that is already active (no needless mode set,
///   which would blank the screen for a second).
///
/// Without a saved interface size, outputs get [`default_scale`] (200 % on
/// a 4K TV, apps were tiny at 4K).
pub fn apply_prefs(r: &mut dyn Runner, prefs: &DisplayPrefs, outputs: &[Output]) -> Result<()> {
    apply_default_scale(r, prefs, outputs)?;
    let saved = outputs.iter().find(|o| o.name == prefs.output && o.enabled);
    let Some(out) = saved else {
        return Ok(());
    };
    if let Some((w, h, mhz)) = prefs.mode
        && let Some(mode) = out.find_mode(w, h, mhz)
        && !mode.current
    {
        set_mode(r, &out.name, mode)?;
    }
    if let Some(scale) = prefs.scale
        && check_scale(scale).is_ok()
        && (out.scale - scale).abs() > 0.001
    {
        set_scale(r, &out.name, scale)?;
    }
    Ok(())
}

// ------------------------------------------------------------ screen power

/// The two tools turning the screen off needs (Arch `extra/wlopm` and
/// `extra/swayidle`). `wlopm` switches the outputs off and on
/// (zwlr-output-power-management-v1, which labwc implements); `swayidle`
/// runs `wlopm --on` on the first input after a pause, so the screen
/// comes back on a key press, a remote button or a click even if the shell
/// itself is stuck (`autostart` supervises it).
pub const POWER_TOOLS: [&str; 2] = ["wlopm", "swayidle"];

/// `wlopm --on '*'` / `wlopm --off '*'`: all outputs.
pub fn power_cmd(on: bool) -> Cmd {
    Cmd::new("wlopm")
        .args([if on { "--on" } else { "--off" }, "*"])
        .timeout(Duration::from_secs(5))
}

/// Switch every output's power on or off. Call off the UI thread.
pub fn set_power(r: &mut dyn Runner, on: bool) -> Result<()> {
    r.run_ok(&power_cmd(on))?;
    Ok(())
}

/// Both [`POWER_TOOLS`] are installed. The screen is never switched off
/// without them: `swayidle` is what makes sure it can come back.
pub fn power_tools_present() -> bool {
    POWER_TOOLS
        .iter()
        .all(|t| crate::process::find_executable(t).is_some())
}

/// Switches the screen off and on from a worker thread, in order, so the
/// UI never waits for `wlopm`. Without the tools (or on a developer
/// machine) it does nothing.
pub struct PowerSwitch {
    tx: Option<mpsc::Sender<bool>>,
    off: bool,
}

impl PowerSwitch {
    /// A switch that does nothing.
    pub fn disabled() -> Self {
        Self {
            tx: None,
            off: false,
        }
    }

    /// On an appliance with the tools installed: a worker running `wlopm`,
    /// and the screen switched on once, in case an earlier run left it off.
    pub fn start(appliance: bool) -> Self {
        if !appliance || !power_tools_present() {
            return Self::disabled();
        }
        let mut s = Self::with_worker(|on| {
            let _ = set_power(&mut crate::process::SystemRunner, on);
        });
        s.force_on();
        s
    }

    /// A switch handing every change to `f` on its own thread (tests).
    pub fn with_worker(mut f: impl FnMut(bool) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel::<bool>();
        let spawned = std::thread::Builder::new()
            .name("catostv-power".into())
            .spawn(move || {
                for on in rx {
                    f(on);
                }
            });
        match spawned {
            Ok(_) => Self {
                tx: Some(tx),
                off: false,
            },
            Err(_) => Self::disabled(),
        }
    }

    fn send(&self, on: bool) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(on);
        }
    }

    /// Switch the screen off (once).
    pub fn off(&mut self) {
        if self.tx.is_some() && !self.off {
            self.off = true;
            self.send(false);
        }
    }

    /// Switch the screen on, if this switch turned it off.
    pub fn on(&mut self) {
        if self.off {
            self.off = false;
            self.send(true);
        }
    }

    /// Switch the screen on whatever this switch believes.
    pub fn force_on(&mut self) {
        self.off = false;
        self.send(true);
    }

    pub fn is_off(&self) -> bool {
        self.off
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Output as CmdOutput, fake::FakeRunner};

    /// Format: wlr-randr 0.5 `print_state` (header unindented, properties 2
    /// spaces, modes 4 spaces). Content: the sample in the CatosTV task brief
    /// plus a disabled second output with no position/scale lines.
    const TV: &str = "\
HDMI-A-1 \"Samsung Electric Company SAMSUNG 0x01000E00 (HDMI-A-1)\"
  Make: Samsung Electric Company
  Model: SAMSUNG
  Serial: 0x01000E00
  Physical size: 1600x900 mm
  Enabled: yes
  Modes:
    720x400 px, 70.081001 Hz
    3840x2160 px, 60.000000 Hz (preferred, current)
    3840x2160 px, 59.940002 Hz
    3840x2160 px, 30.000000 Hz
    1920x1080 px, 60.000000 Hz
    1920x1080 px, 60.000000 Hz
    1920x1080 px, 59.940002 Hz
    1920x1080 px, 50.000000 Hz
    1280x720 px, 60.000000 Hz
    4096x2160 px, 24.000000 Hz
  Position: 0,0
  Transform: normal
  Scale: 1.000000
  Adaptive Sync: disabled
eDP-1 \"Unknown 0x0000 (eDP-1)\"
  Enabled: no
  Modes:
    1920x1080 px, 60.000000 Hz (preferred)
";

    /// Lines as posted on bbs.archlinux.org topic 305821 (wlr-randr on a
    /// 165 Hz monitor; indentation restored to wlr-randr's 2/4 spaces).
    const MONITOR_165: &str = "\
DP-1 \"SAC DP 0000000100000 (DP-1)\"
  Make: SAC
  Model: DP
  Serial: 0000000100000
  Physical size: 600x330 mm
  Enabled: yes
  Modes:
    1920x1080 px, 60.000000 Hz (preferred, current)
    1920x1080 px, 164.996002 Hz
    1920x1080 px, 144.000000 Hz
    1920x1080 px, 119.999001 Hz
    1920x1080 px, 99.998001 Hz
    1920x1080 px, 59.938999 Hz
    1680x1050 px, 59.953999 Hz
  Position: 0,0
  Transform: normal
  Scale: 1.000000
  Adaptive Sync: disabled
";

    #[test]
    fn parses_outputs_and_modes() {
        let outs = parse_wlr_randr(TV);
        assert_eq!(outs.len(), 2);
        let tv = &outs[0];
        assert_eq!(tv.name, "HDMI-A-1");
        assert_eq!(
            tv.description,
            "Samsung Electric Company SAMSUNG 0x01000E00 (HDMI-A-1)"
        );
        assert_eq!(tv.make, "Samsung Electric Company");
        assert_eq!(tv.model, "SAMSUNG");
        assert!(tv.enabled);
        assert_eq!(tv.modes.len(), 10);
        assert_eq!(tv.scale, 1.0);
        assert_eq!(tv.adaptive_sync, Some(false));
        assert_eq!(tv.physical_mm, Some((1600, 900)));
        assert_eq!(tv.display_name(), "Samsung");
        let cur = tv.current_mode().unwrap();
        assert_eq!(
            (cur.width, cur.height, cur.refresh_mhz),
            (3840, 2160, 60000)
        );
        assert!(cur.preferred);
        assert_eq!(tv.preferred_mode(), Some(cur));
        assert_eq!(tv.modes[0].refresh_mhz, 70081);
        assert_eq!(tv.modes[2].refresh_mhz, 59940);

        let laptop = &outs[1];
        assert_eq!(laptop.name, "eDP-1");
        assert!(!laptop.enabled);
        assert_eq!(laptop.scale, 1.0);
        assert_eq!(laptop.adaptive_sync, None);
        assert_eq!(laptop.current_mode(), None);
        assert!(laptop.preferred_mode().is_some());
        assert_eq!(laptop.display_name(), "eDP-1");
    }

    #[test]
    fn parses_real_high_refresh_sample() {
        let outs = parse_wlr_randr(MONITOR_165);
        let m = &outs[0];
        let rates: Vec<u32> = m.modes.iter().map(|x| x.refresh_mhz).collect();
        assert_eq!(rates, [60000, 164996, 144000, 119999, 99998, 59939, 59954]);
        // The exact millihertz round-trips into the argument wlr-randr needs.
        assert_eq!(m.modes[1].wlr_arg(), "1920x1080@164.996Hz");
        assert_eq!(m.modes[5].wlr_arg(), "1920x1080@59.939Hz");
        assert_eq!(m.modes[1].label(), "1920 × 1080 · 165 Hz");
        assert_eq!(m.modes[3].label(), "1920 × 1080 · 120 Hz");
        assert_eq!(m.modes[5].label(), "1920 × 1080 · 59.94 Hz");
    }

    #[test]
    fn tolerates_crlf_junk_and_missing_refresh() {
        let text = "garbage before any header\r\nVirtual-1 \"Virtual\"\r\n  Enabled: yes\r\n  Modes:\r\n    1024x768 px (current)\r\n    bogus px, x Hz\r\n  Scale: 2.000000\r\n  Something new: 42\r\n";
        let outs = parse_wlr_randr(text);
        assert_eq!(outs.len(), 1);
        assert_eq!(outs[0].modes.len(), 1);
        let m = outs[0].modes[0];
        assert_eq!(
            (m.width, m.height, m.refresh_mhz, m.current),
            (1024, 768, 0, true)
        );
        assert_eq!(m.label(), "1024 × 768");
        assert_eq!(m.wlr_arg(), "1024x768");
        assert_eq!(outs[0].scale, 2.0);
        assert!(parse_wlr_randr("").is_empty());
        // A stray unindented line ends the current output.
        let stray = "A-1 \"a\"\n  Scale: 1.000000\nwarning: something\n  Scale: 3.000000\n";
        let outs = parse_wlr_randr(stray);
        assert_eq!(outs.len(), 1);
        assert_eq!(outs[0].scale, 1.0);
    }

    #[test]
    fn labels() {
        let m = |mhz| Mode {
            width: 3840,
            height: 2160,
            refresh_mhz: mhz,
            preferred: false,
            current: false,
        };
        assert_eq!(m(60000).label(), "3840 × 2160 · 60 Hz");
        assert_eq!(m(59940).label(), "3840 × 2160 · 59.94 Hz");
        assert_eq!(m(23976).refresh_label(), "23.98 Hz");
        assert_eq!(m(29970).refresh_label(), "29.97 Hz");
        assert_eq!(m(59500).refresh_label(), "59.5 Hz");
        assert_eq!(m(59999).refresh_label(), "60 Hz");
        assert_eq!(m(60000).wlr_arg(), "3840x2160@60.000Hz");
        assert_eq!(m(23976).wlr_arg(), "3840x2160@23.976Hz");
    }

    #[test]
    fn menu_modes_dedupes_and_sorts() {
        let tv = &parse_wlr_randr(TV)[0];
        let labels: Vec<String> = menu_modes(tv).iter().map(Mode::label).collect();
        assert_eq!(
            labels,
            [
                "4096 × 2160 · 24 Hz",
                "3840 × 2160 · 60 Hz",
                "3840 × 2160 · 59.94 Hz",
                "3840 × 2160 · 30 Hz",
                "1920 × 1080 · 60 Hz",
                "1920 × 1080 · 59.94 Hz",
                "1920 × 1080 · 50 Hz",
                "1280 × 720 · 60 Hz",
                "720 × 400 · 70.08 Hz",
            ]
        );
        let menu = menu_modes(tv);
        assert!(menu[1].current && menu[1].preferred);
    }

    #[test]
    fn duplicate_modes_merge_flags() {
        let text = "X-1 \"x\"\n  Enabled: yes\n  Modes:\n    1920x1080 px, 60.000000 Hz (preferred)\n    1920x1080 px, 60.000000 Hz (current)\n";
        let menu = menu_modes(&parse_wlr_randr(text)[0]);
        assert_eq!(menu.len(), 1);
        assert!(menu[0].preferred && menu[0].current);
    }

    #[test]
    fn commands_are_exact_argv() {
        let tv = parse_wlr_randr(TV).remove(0);
        let mut r = FakeRunner::new();
        set_mode(&mut r, "HDMI-A-1", &tv.modes[2]).unwrap();
        set_preferred(&mut r, "HDMI-A-1").unwrap();
        set_scale(&mut r, "HDMI-A-1", 1.5).unwrap();
        set_adaptive_sync(&mut r, "HDMI-A-1", true).unwrap();
        set_adaptive_sync(&mut r, "HDMI-A-1", false).unwrap();
        let shown: Vec<String> = r.seen.iter().map(Cmd::display).collect();
        assert_eq!(
            shown,
            [
                "wlr-randr --output HDMI-A-1 --mode 3840x2160@59.940Hz",
                "wlr-randr --output HDMI-A-1 --preferred",
                "wlr-randr --output HDMI-A-1 --scale 1.5",
                "wlr-randr --output HDMI-A-1 --adaptive-sync enabled",
                "wlr-randr --output HDMI-A-1 --adaptive-sync disabled",
            ]
        );
    }

    #[test]
    fn reset_passes_through_half_the_refresh_rate() {
        let mut r = FakeRunner::new();
        r.push(CmdOutput::ok(TV));
        assert_eq!(reset_modes(&mut r, std::time::Duration::ZERO).unwrap(), 1);
        let shown: Vec<String> = r.seen.iter().map(Cmd::display).collect();
        assert_eq!(
            shown,
            [
                "wlr-randr",
                "wlr-randr --output HDMI-A-1 --mode 3840x2160@30.000Hz",
                "wlr-randr --output HDMI-A-1 --mode 3840x2160@60.000Hz",
            ]
        );
        // Only one mode at that size: any other mode will do.
        let mut one = parse_wlr_randr(TV).remove(0);
        one.modes.retain(|m| m.width != 3840 || m.current);
        assert_eq!(bounce_mode(&one).unwrap().width, 720);
        one.modes.retain(|m| m.current);
        assert_eq!(bounce_mode(&one), None);
    }

    #[test]
    fn rejects_bad_arguments_before_running_anything() {
        let mode = parse_wlr_randr(TV)[0].modes[1];
        let mut r = FakeRunner::new();
        for bad in ["", "--dryrun", "HDMI A", "a;b", "$(x)", &"x".repeat(65)] {
            assert!(set_mode(&mut r, bad, &mode).is_err(), "{bad:?}");
            assert!(set_scale(&mut r, bad, 1.0).is_err());
            assert!(set_adaptive_sync(&mut r, bad, true).is_err());
            assert!(set_preferred(&mut r, bad).is_err());
        }
        for bad_scale in [0.0, 0.1, 5.0, f32::NAN, f32::INFINITY, -1.0] {
            assert!(set_scale(&mut r, "HDMI-A-1", bad_scale).is_err());
        }
        let zero = Mode { width: 0, ..mode };
        assert!(set_mode(&mut r, "HDMI-A-1", &zero).is_err());
        assert!(r.seen.is_empty());
        assert!(is_valid_output_name("HDMI-A-1"));
        assert!(is_valid_output_name("DP-3"));
        assert!(is_valid_output_name("WL-1"));
    }

    #[test]
    fn outputs_runs_wlr_randr_and_reports_failure() {
        let mut r = FakeRunner::new();
        r.push(CmdOutput::ok(MONITOR_165));
        assert_eq!(outputs(&mut r).unwrap()[0].name, "DP-1");
        assert_eq!(r.seen[0].display(), "wlr-randr");
        r.push(CmdOutput::failed(1, "failed to connect to display"));
        let e = outputs(&mut r).unwrap_err();
        assert!(e.message().contains("failed to connect to display"));
    }

    #[test]
    fn prefs_round_trip_and_junk() {
        let p = DisplayPrefs {
            output: "HDMI-A-1".into(),
            mode: Some((3840, 2160, 59940)),
            scale: Some(1.5),
            safe_area_pct: 3,
        };
        let text = p.to_conf();
        assert!(text.contains("mode = 3840x2160@59940mHz\n"));
        assert_eq!(DisplayPrefs::from_conf(&text), p);

        let junk = "rubbish\n= =\noutput = ; rm -rf /\nmode = 12xq@1\nscale = 99\nsafe_area_pct = 50\nfuture_key = 1\n";
        assert_eq!(
            DisplayPrefs::from_conf(junk),
            DisplayPrefs {
                safe_area_pct: 10,
                ..DisplayPrefs::default()
            }
        );
        assert_eq!(DisplayPrefs::from_conf(""), DisplayPrefs::default());
        // Without a unit suffix the mode is still read.
        assert_eq!(
            DisplayPrefs::from_conf("mode = 1920x1080@60000").mode,
            Some((1920, 1080, 60000))
        );
    }

    #[test]
    fn apply_prefs_only_applies_advertised_modes() {
        let outs = parse_wlr_randr(TV);
        // Supported, not current → applied; scale changed too.
        let mut r = FakeRunner::new();
        let p = DisplayPrefs {
            output: "HDMI-A-1".into(),
            mode: Some((1920, 1080, 50000)),
            scale: Some(2.0),
            safe_area_pct: 0,
        };
        apply_prefs(&mut r, &p, &outs).unwrap();
        let shown: Vec<String> = r.seen.iter().map(Cmd::display).collect();
        assert_eq!(
            shown,
            [
                "wlr-randr --output HDMI-A-1 --mode 1920x1080@50.000Hz",
                "wlr-randr --output HDMI-A-1 --scale 2",
            ]
        );

        // Stale/unsupported mode, already-current scale → nothing runs.
        let mut r = FakeRunner::new();
        let stale = DisplayPrefs {
            mode: Some((1920, 1080, 120000)),
            scale: Some(1.0),
            ..p.clone()
        };
        apply_prefs(&mut r, &stale, &outs).unwrap();
        assert!(r.seen.is_empty());

        // Already current mode → no mode set; no saved size on a 4K TV →
        // the 200 % default.
        let current = DisplayPrefs {
            mode: Some((3840, 2160, 60000)),
            scale: None,
            ..p.clone()
        };
        apply_prefs(&mut r, &current, &outs).unwrap();
        let shown: Vec<String> = r.seen.iter().map(Cmd::display).collect();
        assert_eq!(shown, ["wlr-randr --output HDMI-A-1 --scale 2"]);

        // Saved output not connected → its prefs are ignored, but the TV
        // that is connected still gets its default size.
        for name in ["DP-9", "eDP-1"] {
            let mut r = FakeRunner::new();
            let gone = DisplayPrefs {
                output: name.into(),
                ..p.clone()
            };
            apply_prefs(&mut r, &gone, &outs).unwrap();
            let shown: Vec<String> = r.seen.iter().map(Cmd::display).collect();
            assert_eq!(shown, ["wlr-randr --output HDMI-A-1 --scale 2"]);
        }

        // An explicitly saved 100 % is respected.
        let mut r = FakeRunner::new();
        let small = DisplayPrefs {
            mode: None,
            scale: Some(1.0),
            ..p.clone()
        };
        apply_prefs(&mut r, &small, &outs).unwrap();
        assert!(r.seen.is_empty());
    }

    #[test]
    fn hdr_is_never_claimed() {
        assert!(!hdr_supported(&parse_wlr_randr(TV)[0]));
    }

    #[test]
    fn screen_power_commands() {
        assert_eq!(power_cmd(false).display(), "wlopm --off '*'");
        assert_eq!(power_cmd(true).display(), "wlopm --on '*'");
        let mut r = FakeRunner::new();
        set_power(&mut r, false).unwrap();
        assert_eq!(r.seen[0].display(), "wlopm --off '*'");
    }

    #[test]
    fn power_switch_keeps_order_and_skips_repeats() {
        let (tx, rx) = mpsc::channel();
        let mut s = PowerSwitch::with_worker(move |on| {
            let _ = tx.send(on);
        });
        let got = |rx: &mpsc::Receiver<bool>| rx.recv_timeout(Duration::from_secs(2)).unwrap();
        s.on(); // nothing to undo
        s.off();
        s.off(); // already off
        assert!(s.is_off());
        s.on();
        s.on(); // already on
        s.force_on(); // always sent
        s.off();
        assert_eq!(
            [got(&rx), got(&rx), got(&rx), got(&rx)],
            [false, true, true, false]
        );
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn disabled_power_switch_never_turns_the_screen_off() {
        let mut s = PowerSwitch::disabled();
        s.off();
        assert!(!s.is_off());
        s.on();
        s.force_on();
        // Not an appliance (a developer's PC): never touched.
        assert!(!PowerSwitch::start(false).is_off());
    }
}
