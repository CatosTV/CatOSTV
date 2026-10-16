//! BlueZ adapter (`bluetoothctl`).
//!
//! `bluetoothctl` blocks forever ("Waiting to connect to bluetoothd...") when
//! the daemon is not running. Every call therefore goes through a
//! [`Runner`] with a short timeout and must never run on the UI thread.
//!
//! Pairing (`scan`, `pair`, `trust`, `connect`, …) is documented in the
//! "Discovery and pairing" section below.

use crate::error::{Error, Result};
use crate::process::{Cmd, Output, Runner};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BluetoothStatus {
    /// A controller exists and bluetoothd answered.
    pub available: bool,
    pub powered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub address: String,
    pub name: String,
}

/// Parse `bluetoothctl show`.
pub fn parse_show(text: &str) -> BluetoothStatus {
    if text.contains("No default controller available") || !text.contains("Controller ") {
        return BluetoothStatus::default();
    }
    BluetoothStatus {
        available: true,
        powered: text.lines().any(|l| l.trim() == "Powered: yes"),
    }
}

/// Parse `bluetoothctl devices Paired` / `paired-devices`.
pub fn parse_devices(text: &str) -> Vec<Device> {
    text.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("Device ")?;
            let (address, name) = rest.split_once(' ').unwrap_or((rest, ""));
            Some(Device {
                address: address.to_string(),
                name: if name.is_empty() {
                    address.to_string()
                } else {
                    name.to_string()
                },
            })
        })
        .collect()
}

pub fn status(r: &mut dyn Runner) -> BluetoothStatus {
    match r.run(&Cmd::new("bluetoothctl").arg("show")) {
        Ok(out) if !out.timed_out => parse_show(&out.stdout),
        _ => BluetoothStatus::default(),
    }
}

pub fn set_power(r: &mut dyn Runner, on: bool) -> Result<()> {
    r.run_ok(&Cmd::new("bluetoothctl").args(["power", if on { "on" } else { "off" }]))?;
    Ok(())
}

pub fn paired_devices(r: &mut dyn Runner) -> Vec<Device> {
    // BlueZ ≥ 5.65 uses `devices Paired`; older versions `paired-devices`.
    for args in [&["devices", "Paired"][..], &["paired-devices"][..]] {
        if let Ok(out) = r.run(&Cmd::new("bluetoothctl").args(args.iter().copied()))
            && out.success()
        {
            return parse_devices(&out.stdout);
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// Discovery and pairing
// ---------------------------------------------------------------------------
//
// Everything runs `bluetoothctl` in non-interactive mode (command as argv,
// stdin closed). Facts this relies on, from BlueZ `client/main.c` and
// `src/shared/shell.c` (BlueZ 5.7x/5.8x; Arch bluez-utils 5.87):
//
// * In non-interactive mode bluetoothctl does **not** register an agent
//   (`auto_register_agent && !bt_shell_get_env("NON_INTERACTIVE")`), so
//   `--agent NoInputNoOutput` has no effect there and is not passed. With no
//   agent for the calling client, bluetoothd pairs with IO capability
//   NoInputNoOutput ("Just Works") unless another process registered a
//   default agent (CatosTV does not run one).
// * `scan on` replies "Discovery started" and then keeps running
//   (-EINPROGRESS); `--timeout N` makes it exit after N seconds.
// * `pair` prints "Pairing successful" / "Failed to pair: <error>",
//   `trust` "Changing <mac> trust succeeded", `connect` "Connection
//   successful" / "Failed to connect: <error>", `disconnect` "Disconnection
//   successful" (older: "Successful disconnected"), `remove` "Device has
//   been removed", unknown addresses "Device <mac> not available"; failures
//   exit with status 1.
// * Discovered (unpaired) devices are forgotten by bluetoothd about 30 s
//   after discovery stops (`TemporaryTimeout`), so pair soon after `scan`.
//
// LIMITATION: devices that require entering or confirming a PIN/passkey
// (some keyboards, legacy-pairing devices) cannot be paired this way yet;
// that needs a D-Bus agent (`org.bluez.Agent1`) that shows the code on the
// TV. Most gamepads, remotes, headphones and speakers use Just Works.
//
// Status: parsers and argv tested with FakeRunner; pairing on real hardware
// UNVERIFIED.

/// Shortest/longest scan CatosTV allows, in seconds.
pub const MIN_SCAN_SECS: u32 = 3;
pub const MAX_SCAN_SECS: u32 = 60;
/// Generous timeouts: pairing waits for the remote device.
pub const PAIR_TIMEOUT: Duration = Duration::from_secs(30);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const SMALL_TIMEOUT: Duration = Duration::from_secs(10);

/// `AA:BB:CC:DD:EE:FF` (hex digits, either case).
pub fn is_valid_mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Validate and normalise (upper case, as BlueZ prints addresses).
fn check_mac(mac: &str) -> Result<String> {
    if is_valid_mac(mac) {
        Ok(mac.to_ascii_uppercase())
    } else {
        Err(Error::msg(format!("invalid Bluetooth address {mac:?}")))
    }
}

/// Remove ANSI escape sequences (bluetoothctl colours `[NEW]`/`[CHG]`/
/// `[DEL]` and clears prompt lines) and control characters other than
/// newline and tab.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    // CSI: parameters/intermediates until a final byte @..~
                    for f in chars.by_ref() {
                        if ('@'..='~').contains(&f) {
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
            }
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// True if bluetoothd knows a real name for the device. Unnamed devices
/// show their address with dashes (`4C-87-5D-12-34-56`) as name.
pub fn has_real_name(d: &Device) -> bool {
    let name = d.name.trim();
    !name.is_empty()
        && !name.eq_ignore_ascii_case(&d.address)
        && !name.replace('-', ":").eq_ignore_ascii_case(&d.address)
}

/// Parse `bluetoothctl --timeout N scan on` output (after [`strip_ansi`]):
/// `[NEW] Device <mac> <name>` adds a device, `[CHG] Device <mac> Name:
/// <name>` / `Alias: <name>` renames it, `[DEL] Device <mac>` removes it.
pub fn parse_scan_output(text: &str) -> Vec<Device> {
    let mut devices: Vec<Device> = Vec::new();
    for line in text.lines() {
        for (tag, kind) in [
            ("[NEW] Device ", 0),
            ("[CHG] Device ", 1),
            ("[DEL] Device ", 2),
        ] {
            let Some(i) = line.find(tag) else { continue };
            let rest = line[i + tag.len()..].trim();
            let (mac, tail) = rest.split_once(' ').unwrap_or((rest, ""));
            if !is_valid_mac(mac) {
                continue;
            }
            let mac = mac.to_ascii_uppercase();
            let pos = devices.iter().position(|d| d.address == mac);
            match kind {
                0 => {
                    let name = if tail.trim().is_empty() {
                        mac.clone()
                    } else {
                        tail.trim().to_string()
                    };
                    match pos {
                        Some(p) => devices[p].name = name,
                        None => devices.push(Device { address: mac, name }),
                    }
                }
                1 => {
                    let new_name = tail
                        .strip_prefix("Name: ")
                        .or_else(|| tail.strip_prefix("Alias: "))
                        .map(str::trim)
                        .filter(|n| !n.is_empty());
                    if let (Some(p), Some(n)) = (pos, new_name) {
                        devices[p].name = n.to_string();
                    }
                }
                _ => {
                    if let Some(p) = pos {
                        devices.remove(p);
                    }
                }
            }
        }
    }
    devices
}

/// Map a bluetoothctl failure to a short sentence for the TV.
fn friendly_failure(action: &str, text: &str, out: &Output) -> Error {
    if out.timed_out {
        return Error::msg(format!(
            "{action} timed out. Make sure the device is on and close by."
        ));
    }
    if text.contains("Waiting to connect to bluetoothd") {
        return Error::msg("The Bluetooth service is not running");
    }
    if text.contains("No default controller available") {
        return Error::msg("No Bluetooth adapter found");
    }
    if text.contains("not available") {
        return Error::msg("The device is no longer visible. Scan again and retry.");
    }
    if text.contains("AuthenticationFailed")
        || text.contains("AuthenticationRejected")
        || text.contains("AuthenticationCanceled")
    {
        return Error::msg(
            "Pairing was rejected. This device may need a PIN, which CatosTV cannot enter yet.",
        );
    }
    if text.contains("ConnectionAttemptFailed") || text.contains("Page Timeout") {
        return Error::msg("The device did not respond. Put it in pairing mode and retry.");
    }
    if text.contains("NotReady") {
        return Error::msg("Bluetooth is off");
    }
    let detail = text
        .lines()
        .map(str::trim)
        .rfind(|l| l.starts_with("Failed"))
        .map(str::to_string)
        .unwrap_or_else(|| out.failure_reason());
    Error::msg(format!("{action} failed ({detail})"))
}

/// Run one device command; succeed if any `ok_markers` appears in the
/// output, or the exit code is 0 and nothing says "Failed".
fn device_cmd(
    r: &mut dyn Runner,
    action: &str,
    verb: &str,
    mac: &str,
    timeout: Duration,
    ok_markers: &[&str],
) -> Result<()> {
    let mac = check_mac(mac)?;
    let out = r.run(&Cmd::new("bluetoothctl").args([verb, &mac]).timeout(timeout))?;
    let text = strip_ansi(&format!("{}\n{}", out.stdout, out.stderr));
    if ok_markers.iter().any(|m| text.contains(m))
        || (out.success() && !text.contains("Failed") && !text.contains("not available"))
    {
        Ok(())
    } else {
        Err(friendly_failure(action, &text, &out))
    }
}

/// Discover nearby devices for `secs` seconds (clamped to 3–60) and return
/// the unpaired ones that have a real name, sorted by name.
///
/// Runs `bluetoothctl --timeout SECS scan on`, then `bluetoothctl devices`
/// and the paired list. Bluetooth must be powered on ([`set_power`]).
pub fn scan(r: &mut dyn Runner, secs: u32) -> Result<Vec<Device>> {
    let secs = secs.clamp(MIN_SCAN_SECS, MAX_SCAN_SECS);
    let out = r.run(
        &Cmd::new("bluetoothctl")
            .args(["--timeout", &secs.to_string(), "scan", "on"])
            .timeout(Duration::from_secs(u64::from(secs) + 10)),
    )?;
    let text = strip_ansi(&format!("{}\n{}", out.stdout, out.stderr));
    if text.contains("Failed to start discovery")
        || text.contains("No default controller available")
        || (out.timed_out && !text.contains("Discovery started"))
    {
        return Err(friendly_failure("Bluetooth scan", &text, &out));
    }
    let mut found = parse_scan_output(&text);
    if let Ok(all) = r.run(&Cmd::new("bluetoothctl").arg("devices"))
        && all.success()
    {
        for d in parse_devices(&strip_ansi(&all.stdout)) {
            let d = Device {
                address: d.address.to_ascii_uppercase(),
                name: d.name,
            };
            match found.iter_mut().find(|f| f.address == d.address) {
                Some(f) if !has_real_name(f) => f.name = d.name,
                Some(_) => {}
                None => found.push(d),
            }
        }
    }
    let paired: Vec<String> = paired_devices(r)
        .into_iter()
        .map(|d| d.address.to_ascii_uppercase())
        .collect();
    found.retain(|d| is_valid_mac(&d.address) && has_real_name(d) && !paired.contains(&d.address));
    found.sort_by_key(|d| d.name.to_lowercase());
    Ok(found)
}

/// `bluetoothctl pair MAC` (Just Works; see the module notes).
pub fn pair(r: &mut dyn Runner, mac: &str) -> Result<()> {
    device_cmd(
        r,
        "Pairing",
        "pair",
        mac,
        PAIR_TIMEOUT,
        &["Pairing successful", "AlreadyExists"],
    )
}

/// `bluetoothctl trust MAC`, lets the device reconnect by itself.
pub fn trust(r: &mut dyn Runner, mac: &str) -> Result<()> {
    device_cmd(
        r,
        "Trust",
        "trust",
        mac,
        SMALL_TIMEOUT,
        &["trust succeeded"],
    )
}

/// `bluetoothctl connect MAC`.
pub fn connect(r: &mut dyn Runner, mac: &str) -> Result<()> {
    device_cmd(
        r,
        "Connecting",
        "connect",
        mac,
        CONNECT_TIMEOUT,
        &["Connection successful", "AlreadyConnected"],
    )
}

/// `bluetoothctl disconnect MAC`.
pub fn disconnect(r: &mut dyn Runner, mac: &str) -> Result<()> {
    device_cmd(
        r,
        "Disconnecting",
        "disconnect",
        mac,
        SMALL_TIMEOUT,
        &["Disconnection successful", "Successful disconnected"],
    )
}

/// `bluetoothctl remove MAC`, unpair and forget the device.
pub fn remove(r: &mut dyn Runner, mac: &str) -> Result<()> {
    device_cmd(
        r,
        "Removing the device",
        "remove",
        mac,
        SMALL_TIMEOUT,
        &["Device has been removed"],
    )
}

/// Pair, trust, then connect: the whole "add a device" flow. Stops at the
/// first failure (a paired-but-unconnected device can be connected later).
pub fn pair_trust_connect(r: &mut dyn Runner, mac: &str) -> Result<()> {
    check_mac(mac)?;
    pair(r, mac)?;
    trust(r, mac)?;
    connect(r, mac)
}

/// Details of one device (`bluetoothctl info MAC`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceInfo {
    pub address: String,
    /// Alias if set (what the user/BlueZ calls it), else Name, else address.
    pub name: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    /// freedesktop icon name from the device class, e.g. `input-keyboard`,
    /// `input-gaming`, `audio-headset`, `audio-headphones`, `audio-card`.
    pub icon: Option<String>,
    /// Battery level in percent, when the device reports it
    /// ("Battery Percentage: 0x64 (100)").
    pub battery: Option<u8>,
}

/// "Battery Percentage: 0x64 (100)" -> 100.
fn parse_battery(v: &str) -> Option<u8> {
    let n = match v.split_once('(') {
        Some((_, rest)) => rest.trim_end_matches(')').trim(),
        None => v.trim(),
    };
    let n = match n.strip_prefix("0x") {
        Some(hex) => u8::from_str_radix(hex, 16).ok()?,
        None => n.parse().ok()?,
    };
    (n <= 100).then_some(n)
}

/// Parse `bluetoothctl info MAC`:
///
/// ```text
/// Device E4:17:D8:AA:BB:CC (public)
///     Name: 8BitDo SN30 Pro
///     Icon: input-gaming
///     Paired: yes
/// ```
pub fn parse_info(text: &str) -> Option<DeviceInfo> {
    let text = strip_ansi(text);
    let mut info: Option<DeviceInfo> = None;
    let (mut name, mut alias) = (None, None);
    for line in text.lines() {
        let t = line.trim();
        if info.is_none() {
            if let Some(rest) = t.strip_prefix("Device ") {
                let mac = rest.split_whitespace().next().unwrap_or("");
                if is_valid_mac(mac) && !t.contains("not available") {
                    info = Some(DeviceInfo {
                        address: mac.to_ascii_uppercase(),
                        ..DeviceInfo::default()
                    });
                }
            }
            continue;
        }
        let Some(i) = info.as_mut() else { continue };
        let Some((k, v)) = t.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k {
            "Name" => name = Some(v.to_string()),
            "Alias" => alias = Some(v.to_string()),
            "Icon" if !v.is_empty() => i.icon = Some(v.to_string()),
            "Paired" => i.paired = v == "yes",
            "Trusted" => i.trusted = v == "yes",
            "Connected" => i.connected = v == "yes",
            "Battery Percentage" => i.battery = parse_battery(v),
            _ => {}
        }
    }
    let mut info = info?;
    info.name = alias
        .or(name)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| info.address.clone());
    Some(info)
}

/// `bluetoothctl info MAC`.
pub fn info(r: &mut dyn Runner, mac: &str) -> Result<DeviceInfo> {
    let mac = check_mac(mac)?;
    let out = r.run(&Cmd::new("bluetoothctl").args(["info", &mac]))?;
    let text = strip_ansi(&format!("{}\n{}", out.stdout, out.stderr));
    match parse_info(&text) {
        Some(i) if !out.timed_out => Ok(i),
        _ => Err(friendly_failure("Reading the device", &text, &out)),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn battery_level() {
        let text = "Device 2C:BE:EE:3A:D7:46 (public)\n\tName: CMF Buds 2a\n\tIcon: audio-headset\n\tPaired: yes\n\tConnected: yes\n\tBattery Percentage: 0x64 (100)\n";
        let i = super::parse_info(text).unwrap();
        assert!(i.connected);
        assert_eq!(i.battery, Some(100));
        assert_eq!(super::parse_battery("0x2a (42)"), Some(42));
        assert_eq!(super::parse_battery("rubbish"), None);
        let none =
            super::parse_info("Device 2C:BE:EE:3A:D7:46 (public)\n\tConnected: yes\n").unwrap();
        assert_eq!(none.battery, None);
    }

    use super::*;
    use crate::process::{Output, fake::FakeRunner};

    const SHOW: &str = "Controller 00:1A:7D:DA:71:13 (public)\n\tName: catostv\n\tPowered: yes\n\tDiscoverable: no\n";

    #[test]
    fn show_parsing() {
        assert_eq!(
            parse_show(SHOW),
            BluetoothStatus {
                available: true,
                powered: true
            }
        );
        assert!(!parse_show(&SHOW.replace("Powered: yes", "Powered: no")).powered);
        assert_eq!(
            parse_show("No default controller available\n"),
            BluetoothStatus::default()
        );
        assert_eq!(
            parse_show("Waiting to connect to bluetoothd..."),
            BluetoothStatus::default()
        );
    }

    #[test]
    fn hung_daemon_reports_unavailable() {
        let mut r = FakeRunner::new();
        r.push(Output {
            timed_out: true,
            stdout: "Waiting to connect to bluetoothd...".into(),
            ..Output::default()
        });
        assert!(!status(&mut r).available);
    }

    #[test]
    fn devices_parsing_and_fallback() {
        let mut r = FakeRunner::new();
        r.push(Output::failed(1, "Invalid command"));
        r.push(Output::ok(
            "Device AA:BB:CC:DD:EE:FF CatosTV Remote\nDevice 11:22:33:44:55:66\n",
        ));
        let d = paired_devices(&mut r);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].name, "CatosTV Remote");
        assert_eq!(d[1].name, "11:22:33:44:55:66");
        assert_eq!(r.seen[1].args, vec!["paired-devices"]);
    }

    const NEW: &str = "[\u{1b}[0;92mNEW\u{1b}[0m]";
    const CHG: &str = "[\u{1b}[0;93mCHG\u{1b}[0m]";
    const DEL: &str = "[\u{1b}[0;91mDEL\u{1b}[0m]";

    /// `bluetoothctl --timeout 8 scan on`: message texts from BlueZ
    /// client/main.c ("Discovery started", `[NEW] Device <mac> <name>`) and
    /// the shell's colour codes (COLOR_GREEN/YELLOW/RED); CONSTRUCTED,
    /// not captured from a real adapter.
    fn scan_text() -> String {
        format!(
            "Discovery started\n\
             {CHG} Controller 00:1A:7D:DA:71:13 Discovering: yes\n\
             {NEW} Device 4C:87:5D:12:34:56 4C-87-5D-12-34-56\n\
             {NEW} Device e4:17:d8:aa:bb:cc 8BitDo SN30 Pro\n\
             {NEW} Device 11:22:33:44:55:66 Old Remote\n\
             {CHG} Device 4C:87:5D:12:34:56 Name: Xbox Wireless Controller\n\
             {CHG} Device 4C:87:5D:12:34:56 RSSI: -60\n\
             {NEW} Device 70:70:70:70:70:70 70-70-70-70-70-70\n\
             {NEW} Device 99:99:99:99:99:99 Gone Soon\n\
             {DEL} Device 99:99:99:99:99:99 Gone Soon\n\
             \r\u{1b}[K{CHG} Controller 00:1A:7D:DA:71:13 Discovering: no\n"
        )
    }

    #[test]
    fn mac_validation() {
        assert!(is_valid_mac("AA:BB:CC:DD:EE:FF"));
        assert!(is_valid_mac("aa:bb:cc:00:11:22"));
        for bad in [
            "",
            "AA:BB:CC:DD:EE",
            "AA:BB:CC:DD:EE:FF:00",
            "AA-BB-CC-DD-EE-FF",
            "GG:BB:CC:DD:EE:FF",
            "AAA:B:CC:DD:EE:FF",
            "AA:BB:CC:DD:EE:F ",
            "--help",
            "AA:BB:CC:DD:EE:FF; rm",
        ] {
            assert!(!is_valid_mac(bad), "{bad:?}");
        }
    }

    #[test]
    fn invalid_macs_never_run_anything() {
        let mut r = FakeRunner::new();
        for bad in ["", "--agent", "AA:BB:CC:DD:EE", "x;y"] {
            assert!(pair(&mut r, bad).is_err());
            assert!(trust(&mut r, bad).is_err());
            assert!(connect(&mut r, bad).is_err());
            assert!(disconnect(&mut r, bad).is_err());
            assert!(remove(&mut r, bad).is_err());
            assert!(pair_trust_connect(&mut r, bad).is_err());
            assert!(info(&mut r, bad).is_err());
        }
        assert!(r.seen.is_empty());
    }

    #[test]
    fn strip_ansi_removes_colours_and_controls() {
        assert_eq!(
            strip_ansi("\r\u{1b}[K[\u{1b}[0;92mNEW\u{1b}[0m] Device\tx\n"),
            "[NEW] Device\tx\n"
        );
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn scan_output_parsing() {
        let d = parse_scan_output(&strip_ansi(&scan_text()));
        let names: Vec<&str> = d.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Xbox Wireless Controller",
                "8BitDo SN30 Pro",
                "Old Remote",
                "70-70-70-70-70-70"
            ]
        );
        assert_eq!(d[1].address, "E4:17:D8:AA:BB:CC");
        assert!(!has_real_name(&d[3]));
        assert!(has_real_name(&d[0]));
    }

    #[test]
    fn scan_runs_three_commands_and_filters() {
        let mut r = FakeRunner::new();
        r.push(Output::ok(scan_text()));
        // `bluetoothctl devices` adds one cached device and names another.
        r.push(Output::ok(
            "Device 4C:87:5D:12:34:56 Xbox Wireless Controller\n\
             Device 70:70:70:70:70:70 Soundbar\n\
             Device 11:22:33:44:55:66 Old Remote\n\
             Device AB:CD:EF:01:23:45 Jabra Elite\n",
        ));
        r.push(Output::ok("Device 11:22:33:44:55:66 Old Remote\n")); // devices Paired
        let found = scan(&mut r, 100).unwrap();
        let names: Vec<&str> = found.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "8BitDo SN30 Pro",
                "Jabra Elite",
                "Soundbar",
                "Xbox Wireless Controller"
            ]
        );
        assert_eq!(
            r.seen[0].display(),
            "bluetoothctl --timeout 60 scan on",
            "secs clamped to 60"
        );
        assert_eq!(r.seen[0].timeout, Duration::from_secs(70));
        assert_eq!(r.seen[1].args, vec!["devices"]);
        assert_eq!(r.seen[2].args, vec!["devices", "Paired"]);
    }

    #[test]
    fn scan_failures_are_readable() {
        let mut r = FakeRunner::new();
        r.push(Output {
            code: Some(1),
            stdout: "Failed to start discovery: org.bluez.Error.NotReady\n".into(),
            ..Output::default()
        });
        assert_eq!(scan(&mut r, 1).unwrap_err().message(), "Bluetooth is off");
        assert_eq!(r.seen[0].args[1], "3", "secs clamped to 3");

        let mut r = FakeRunner::new();
        r.push(Output {
            timed_out: true,
            stdout: "Waiting to connect to bluetoothd...".into(),
            ..Output::default()
        });
        assert!(scan(&mut r, 5).unwrap_err().message().contains("timed out"));
        assert_eq!(r.seen.len(), 1);
    }

    #[test]
    fn pair_trust_connect_flow() {
        let mut r = FakeRunner::new();
        r.push(Output::ok(
            "Attempting to pair with E4:17:D8:AA:BB:CC\n[CHG] Device E4:17:D8:AA:BB:CC Paired: yes\nPairing successful\n",
        ));
        r.push(Output::ok(
            "[CHG] Device E4:17:D8:AA:BB:CC Trusted: yes\nChanging E4:17:D8:AA:BB:CC trust succeeded\n",
        ));
        r.push(Output::ok(
            "Attempting to connect to E4:17:D8:AA:BB:CC\n[CHG] Device E4:17:D8:AA:BB:CC Connected: yes\nConnection successful\n",
        ));
        pair_trust_connect(&mut r, "e4:17:d8:aa:bb:cc").unwrap();
        let shown: Vec<String> = r.seen.iter().map(Cmd::display).collect();
        assert_eq!(
            shown,
            [
                "bluetoothctl pair E4:17:D8:AA:BB:CC",
                "bluetoothctl trust E4:17:D8:AA:BB:CC",
                "bluetoothctl connect E4:17:D8:AA:BB:CC",
            ]
        );
        assert_eq!(r.seen[0].timeout, PAIR_TIMEOUT);
        assert_eq!(r.seen[2].timeout, CONNECT_TIMEOUT);
    }

    #[test]
    fn already_paired_or_connected_counts_as_success() {
        let mut r = FakeRunner::new();
        r.push(Output::failed(
            1,
            "Failed to pair: org.bluez.Error.AlreadyExists",
        ));
        r.push(Output::ok("Changing AA:BB:CC:DD:EE:FF trust succeeded"));
        r.push(Output::failed(
            1,
            "Failed to connect: org.bluez.Error.AlreadyConnected",
        ));
        pair_trust_connect(&mut r, "AA:BB:CC:DD:EE:FF").unwrap();
    }

    #[test]
    fn pair_failures_stop_the_flow_with_a_reason() {
        let mut r = FakeRunner::new();
        r.push(Output {
            code: Some(1),
            stdout: "Attempting to pair with AA:BB:CC:DD:EE:FF\nFailed to pair: org.bluez.Error.AuthenticationFailed\n".into(),
            ..Output::default()
        });
        let e = pair_trust_connect(&mut r, "AA:BB:CC:DD:EE:FF").unwrap_err();
        assert!(e.message().contains("PIN"), "{e}");
        assert_eq!(r.seen.len(), 1);

        let mut r = FakeRunner::new();
        r.push(Output::failed(1, "Device AA:BB:CC:DD:EE:FF not available"));
        let e = pair(&mut r, "AA:BB:CC:DD:EE:FF").unwrap_err();
        assert!(e.message().contains("Scan again"));

        let mut r = FakeRunner::new();
        r.push(Output {
            timed_out: true,
            ..Output::default()
        });
        assert!(
            connect(&mut r, "AA:BB:CC:DD:EE:FF")
                .unwrap_err()
                .message()
                .contains("timed out")
        );

        let mut r = FakeRunner::new();
        r.push(Output::failed(
            1,
            "Failed to connect: org.bluez.Error.Failed br-connection-unknown",
        ));
        assert_eq!(
            connect(&mut r, "AA:BB:CC:DD:EE:FF").unwrap_err().message(),
            "Connecting failed (Failed to connect: org.bluez.Error.Failed br-connection-unknown)"
        );
    }

    #[test]
    fn disconnect_and_remove() {
        let mut r = FakeRunner::new();
        r.push(Output::ok(
            "Attempting to disconnect from AA:BB:CC:DD:EE:FF\nSuccessful disconnected\n",
        ));
        r.push(Output::ok(
            "[DEL] Device AA:BB:CC:DD:EE:FF Pad\nDevice has been removed\n",
        ));
        disconnect(&mut r, "AA:BB:CC:DD:EE:FF").unwrap();
        remove(&mut r, "AA:BB:CC:DD:EE:FF").unwrap();
        assert_eq!(r.seen[0].args, vec!["disconnect", "AA:BB:CC:DD:EE:FF"]);
        assert_eq!(r.seen[1].args, vec!["remove", "AA:BB:CC:DD:EE:FF"]);
    }

    /// `bluetoothctl info` layout: first line "Device <mac> (<type>)" (BlueZ
    /// client/main.c cmd_info), then tab-indented "Key: value" properties.
    /// Values CONSTRUCTED for an Xbox controller.
    const INFO: &str = "Device 4C:87:5D:12:34:56 (public)\n\
\tName: Xbox Wireless Controller\n\
\tAlias: Living room pad\n\
\tClass: 0x00000508\n\
\tIcon: input-gaming\n\
\tPaired: yes\n\
\tBonded: yes\n\
\tTrusted: no\n\
\tBlocked: no\n\
\tConnected: yes\n\
\tLegacyPairing: no\n\
\tUUID: Human Interface Device... (00001124-0000-1000-8000-00805f9b34fb)\n\
\tModalias: usb:v045Ep0B13d0503\n\
\tRSSI: -55\n";

    #[test]
    fn info_parsing() {
        let i = parse_info(INFO).unwrap();
        assert_eq!(
            i,
            DeviceInfo {
                address: "4C:87:5D:12:34:56".into(),
                name: "Living room pad".into(),
                paired: true,
                trusted: false,
                connected: true,
                icon: Some("input-gaming".into()),
                battery: None,
            }
        );
        let no_alias = parse_info("Device 11:22:33:44:55:66\n\tPaired: no\n").unwrap();
        assert_eq!(no_alias.name, "11:22:33:44:55:66");
        assert_eq!(no_alias.icon, None);
        assert_eq!(parse_info("Device 11:22:33:44:55:66 not available\n"), None);
        assert_eq!(parse_info(""), None);
    }

    #[test]
    fn info_command() {
        let mut r = FakeRunner::new();
        r.push(Output::ok(INFO));
        r.push(Output::failed(1, "Device 11:22:33:44:55:66 not available"));
        assert!(info(&mut r, "4c:87:5d:12:34:56").unwrap().connected);
        assert_eq!(r.seen[0].args, vec!["info", "4C:87:5D:12:34:56"]);
        assert!(info(&mut r, "11:22:33:44:55:66").is_err());
    }
}
