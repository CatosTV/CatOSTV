//! NetworkManager adapter (`nmcli`).
//!
//! All parsing uses nmcli's terse mode (`-t`), where fields are separated by
//! `:` and literal colons/backslashes inside values are escaped as `\:` and
//! `\\`.

use crate::error::{Error, Result};
use crate::process::{Cmd, Runner, SLOW};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    /// 0–100
    pub signal: u8,
    /// False for open networks (no password needed).
    pub secure: bool,
    /// Currently connected.
    pub in_use: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActiveConnection {
    pub name: String,
    /// `wifi`, `ethernet` or the raw nmcli type.
    pub kind: String,
    pub device: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetStatus {
    pub online: bool,
    pub connections: Vec<ActiveConnection>,
}

impl NetStatus {
    /// True when a wired (Ethernet) connection is active. The status icon
    /// then shows Ethernet instead of Wi-Fi.
    pub fn wired(&self) -> bool {
        self.connections.iter().any(|c| c.kind == "ethernet")
    }

    /// "Home Wi-Fi", "Wired", or "Not connected". A VPN is not the
    /// connection itself (it runs over it): left out here.
    pub fn summary(&self) -> String {
        match self
            .connections
            .iter()
            .find(|c| !crate::vpn::is_vpn_type(&c.kind))
        {
            Some(c) if c.kind == "ethernet" => format!("Wired ({})", c.name),
            Some(c) => c.name.clone(),
            None => "Not connected".into(),
        }
    }
}

/// Split one terse nmcli line into fields, honouring `\:` and `\\` escapes.
pub fn split_terse(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    fields.last_mut().unwrap().push(next);
                }
            }
            ':' => fields.push(String::new()),
            c => fields.last_mut().unwrap().push(c),
        }
    }
    fields
}

/// Parse `nmcli -t -f IN-USE,SSID,SIGNAL,SECURITY device wifi list`.
///
/// Hidden networks (empty SSID) are skipped. The same SSID broadcast by
/// several access points/bands is merged, keeping the strongest signal.
/// Result order: connected network first, then strongest signal.
pub fn parse_wifi_list(text: &str) -> Vec<WifiNetwork> {
    let mut nets: Vec<WifiNetwork> = Vec::new();
    for line in text.lines() {
        let f = split_terse(line);
        if f.len() < 4 {
            continue;
        }
        let ssid = f[1].trim().to_string();
        if ssid.is_empty() {
            continue;
        }
        let in_use = f[0].trim() == "*";
        let signal = f[2].trim().parse::<u8>().unwrap_or(0).min(100);
        let security = f[3].trim();
        let secure = !(security.is_empty() || security == "--");
        match nets.iter_mut().find(|n| n.ssid == ssid) {
            Some(existing) => {
                existing.signal = existing.signal.max(signal);
                existing.in_use |= in_use;
                existing.secure |= secure;
            }
            None => nets.push(WifiNetwork {
                ssid,
                signal,
                secure,
                in_use,
            }),
        }
    }
    nets.sort_by(|a, b| b.in_use.cmp(&a.in_use).then(b.signal.cmp(&a.signal)));
    nets
}

/// Parse `nmcli -t -f NAME,TYPE,DEVICE connection show --active`.
pub fn parse_active_connections(text: &str) -> Vec<ActiveConnection> {
    text.lines()
        .filter_map(|line| {
            let f = split_terse(line);
            if f.len() < 3 {
                return None;
            }
            let kind = match f[1].as_str() {
                "802-11-wireless" | "wifi" => "wifi".to_string(),
                "802-3-ethernet" | "ethernet" => "ethernet".to_string(),
                "loopback" => return None,
                other => other.to_string(),
            };
            if f[2] == "lo" {
                return None;
            }
            Some(ActiveConnection {
                name: f[0].clone(),
                kind,
                device: f[2].clone(),
            })
        })
        .collect()
}

/// Parse `nmcli -t -f STATE,CONNECTIVITY general` ("connected:full").
pub fn parse_online(text: &str) -> bool {
    let line = text.lines().next().unwrap_or("");
    let f = split_terse(line);
    let state = f.first().map(String::as_str).unwrap_or("");
    let connectivity = f.get(1).map(String::as_str).unwrap_or("");
    connectivity == "full"
        || (state == "connected" && connectivity != "none" && connectivity != "limited")
}

pub fn wifi_enabled(r: &mut dyn Runner) -> Result<bool> {
    let out = r.run_ok(&Cmd::new("nmcli").args(["radio", "wifi"]))?;
    Ok(out.stdout.trim() == "enabled")
}

/// The Wi-Fi switch, or `None` when the TV has no Wi-Fi adapter
/// (NetworkManager's switch still reads "enabled" then).
pub fn wifi_state(r: &mut dyn Runner) -> Result<Option<bool>> {
    let dev = r.run_ok(&Cmd::new("nmcli").args(["-t", "-f", "TYPE", "device"]))?;
    if !dev.stdout.lines().any(|l| l.trim() == "wifi") {
        return Ok(None);
    }
    wifi_enabled(r).map(Some)
}

pub fn set_wifi(r: &mut dyn Runner, on: bool) -> Result<()> {
    r.run_ok(&Cmd::new("nmcli").args(["radio", "wifi", if on { "on" } else { "off" }]))?;
    Ok(())
}

pub fn status(r: &mut dyn Runner) -> Result<NetStatus> {
    let general =
        r.run_ok(&Cmd::new("nmcli").args(["-t", "-f", "STATE,CONNECTIVITY", "general"]))?;
    let active = r.run_ok(&Cmd::new("nmcli").args([
        "-t",
        "-f",
        "NAME,TYPE,DEVICE",
        "connection",
        "show",
        "--active",
    ]))?;
    Ok(NetStatus {
        online: parse_online(&general.stdout),
        connections: parse_active_connections(&active.stdout),
    })
}

/// Rescan and list nearby Wi-Fi networks. Slow (several seconds): call from
/// a worker thread, never from the UI thread.
pub fn scan(r: &mut dyn Runner) -> Result<Vec<WifiNetwork>> {
    let out = r.run_ok(
        &Cmd::new("nmcli")
            .args([
                "-t",
                "-f",
                "IN-USE,SSID,SIGNAL,SECURITY",
                "device",
                "wifi",
                "list",
                "--rescan",
                "yes",
            ])
            .timeout(SLOW),
    )?;
    Ok(parse_wifi_list(&out.stdout))
}

/// Connect to a network. `password` is `None` for open networks.
///
/// Security note: nmcli receives the password as an argument, so it is
/// briefly visible in the process list to processes of the same user/root.
/// On the single-user appliance this is acceptable; see SECURITY_MODEL.md.
pub fn connect(r: &mut dyn Runner, ssid: &str, password: Option<&str>) -> Result<()> {
    let mut cmd = Cmd::new("nmcli")
        .args(["device", "wifi", "connect", ssid])
        .timeout(SLOW);
    if let Some(pw) = password.filter(|p| !p.is_empty()) {
        cmd = cmd.args(["password", pw]);
    }
    let out = r.run(&cmd)?;
    if out.success() {
        return Ok(());
    }
    let text = format!("{} {}", out.stderr, out.stdout);
    let reason =
        if text.contains("Secrets were required") || text.contains("802-11-wireless-security") {
            "the password was not accepted".to_string()
        } else if text.contains("No network with SSID") {
            "the network is out of range".to_string()
        } else {
            out.failure_reason()
        };
    Err(Error::msg(format!("Could not join {ssid}: {reason}")))
}

// ------------------------------------------------------------------ ethernet

/// The wired connection: what Quick Settings and Settings → Network
/// show, and its IP settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ethernet {
    pub device: String,
    /// NetworkManager's profile on it; `None` = no cable / nothing set up.
    pub connection: Option<String>,
    pub connected: bool,
    pub mac: String,
    /// Current IPv4 address with prefix, "192.168.1.20/24".
    pub address: Option<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    /// The profile's IPv4 method is manual (otherwise automatic/DHCP).
    pub manual: bool,
}

/// IPv4 settings to apply to the wired connection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IpConfig {
    pub manual: bool,
    /// "192.168.1.50/24" (a bare address gets /24).
    pub address: String,
    pub gateway: String,
    /// One or more addresses, separated by spaces or commas.
    pub dns: String,
}

impl IpConfig {
    /// The settings an Ethernet connection has now.
    pub fn from_ethernet(e: &Ethernet) -> Self {
        IpConfig {
            manual: e.manual,
            address: e.address.clone().unwrap_or_default(),
            gateway: e.gateway.clone().unwrap_or_default(),
            dns: e.dns.join(" "),
        }
    }

    /// Check a manual configuration; automatic needs nothing.
    pub fn validate(&self) -> std::result::Result<(), &'static str> {
        if !self.manual {
            return Ok(());
        }
        if cidr(&self.address).is_none() {
            return Err("Enter an IP address like 192.168.1.50/24");
        }
        if !self.gateway.trim().is_empty() && !is_ipv4(self.gateway.trim()) {
            return Err("The gateway must be an address like 192.168.1.1");
        }
        if dns_list(&self.dns).is_none() {
            return Err("DNS servers must be addresses like 1.1.1.1");
        }
        Ok(())
    }
}

pub fn is_ipv4(s: &str) -> bool {
    s.parse::<std::net::Ipv4Addr>().is_ok()
}

/// "192.168.1.50/24" (or a bare address → /24), normalised; `None` if invalid.
pub fn cidr(s: &str) -> Option<String> {
    let s = s.trim();
    let (ip, prefix) = match s.split_once('/') {
        Some((ip, p)) => (ip, p.parse::<u8>().ok()?),
        None => (s, 24),
    };
    (is_ipv4(ip) && (1..=32).contains(&prefix)).then(|| format!("{ip}/{prefix}"))
}

/// DNS servers from "1.1.1.1, 8.8.8.8"; `None` if any is invalid. Empty is
/// allowed (no DNS servers of its own).
pub fn dns_list(s: &str) -> Option<Vec<String>> {
    s.split([',', ' '])
        .filter(|x| !x.is_empty())
        .map(|x| is_ipv4(x).then(|| x.to_string()))
        .collect()
}

/// The first Ethernet device from `nmcli -t -f DEVICE,TYPE,STATE,CONNECTION device`:
/// (device, state, connection).
pub fn parse_ethernet_device(text: &str) -> Option<(String, String, Option<String>)> {
    // Unmanaged ones are virtual (Android's `veth…`), not a port.
    text.lines().find_map(|l| {
        let f = split_terse(l);
        (f.len() >= 4 && f[1] == "ethernet" && f[2] != "unmanaged").then(|| {
            let conn = (!f[3].is_empty() && f[3] != "--").then(|| f[3].clone());
            (f[0].clone(), f[2].clone(), conn)
        })
    })
}

/// Fill the current address, gateway, DNS and MAC from
/// `nmcli -t -f GENERAL.HWADDR,IP4.ADDRESS,IP4.GATEWAY,IP4.DNS device show <dev>`.
pub fn parse_device_show(text: &str, e: &mut Ethernet) {
    for l in text.lines() {
        // Key, then everything after the first colon: `device show` does
        // not escape the colons of a MAC address.
        let Some((key, value)) = l.split_once(':') else {
            continue;
        };
        let value = value.replace("\\:", ":");
        if value.is_empty() || value == "--" {
            continue;
        }
        let key = key.split('[').next().unwrap_or(key);
        match key {
            "GENERAL.HWADDR" => e.mac = value,
            "IP4.ADDRESS" if e.address.is_none() => e.address = Some(value),
            "IP4.GATEWAY" => e.gateway = Some(value),
            "IP4.DNS" => e.dns.push(value),
            _ => {}
        }
    }
}

/// The wired connection, if the TV has an Ethernet port.
pub fn ethernet(r: &mut dyn Runner) -> Option<Ethernet> {
    let dev = r
        .run_ok(&Cmd::new("nmcli").args(["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "device"]))
        .ok()?;
    let (device, state, connection) = parse_ethernet_device(&dev.stdout)?;
    let mut e = Ethernet {
        connected: state == "connected",
        device: device.clone(),
        connection: connection.clone(),
        ..Default::default()
    };
    if let Ok(o) = r.run_ok(&Cmd::new("nmcli").args([
        "-t",
        "-f",
        "GENERAL.HWADDR,IP4.ADDRESS,IP4.GATEWAY,IP4.DNS",
        "device",
        "show",
        &device,
    ])) {
        parse_device_show(&o.stdout, &mut e);
    }
    if let Some(c) = &connection
        && let Ok(o) =
            r.run_ok(&Cmd::new("nmcli").args(["-t", "-f", "ipv4.method", "connection", "show", c]))
    {
        e.manual = o.stdout.trim().ends_with(":manual");
    }
    Some(e)
}

/// Apply `cfg` to the wired connection and reconnect it.
pub fn set_ethernet_ip(r: &mut dyn Runner, connection: &str, cfg: &IpConfig) -> Result<()> {
    cfg.validate().map_err(Error::msg)?;
    let mut args: Vec<String> = vec!["connection".into(), "modify".into(), connection.into()];
    if cfg.manual {
        let dns = dns_list(&cfg.dns).unwrap_or_default().join(" ");
        args.extend([
            "ipv4.method".into(),
            "manual".into(),
            "ipv4.addresses".into(),
            cidr(&cfg.address).unwrap_or_default(),
            "ipv4.gateway".into(),
            cfg.gateway.trim().to_string(),
            "ipv4.dns".into(),
            dns,
        ]);
    } else {
        args.extend(
            [
                "ipv4.method",
                "auto",
                "ipv4.addresses",
                "",
                "ipv4.gateway",
                "",
                "ipv4.dns",
                "",
            ]
            .map(String::from),
        );
    }
    let out = r.run(&Cmd::new("nmcli").args(args))?;
    if !out.success() {
        return Err(Error::msg(format!(
            "Could not change the Ethernet settings: {}",
            out.failure_reason()
        )));
    }
    let out = r.run(
        &Cmd::new("nmcli")
            .args(["connection", "up", connection])
            .timeout(SLOW),
    )?;
    if !out.success() {
        return Err(Error::msg(format!(
            "Saved, but Ethernet did not reconnect: {}",
            out.failure_reason()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Output, fake::FakeRunner};

    #[test]
    fn ethernet_device_and_details() {
        // Real output from the TV (2026-10-16), Android's veth first.
        let dev = "vethK5nvNg:ethernet:unmanaged:\nwlan0:wifi:disconnected:\neno1:ethernet:connected:Wired connection 1\nlo:loopback:connected (externally):lo\n";
        assert_eq!(
            parse_ethernet_device(dev),
            Some((
                "eno1".into(),
                "connected".into(),
                Some("Wired connection 1".into())
            ))
        );
        assert_eq!(
            parse_ethernet_device("eno1:ethernet:unavailable:\n")
                .unwrap()
                .2,
            None
        );
        let mut e = Ethernet::default();
        parse_device_show(
            "GENERAL.HWADDR:AA:BB:CC:DD:EE:FF\nIP4.ADDRESS[1]:192.168.1.88/24\nIP4.GATEWAY:192.168.1.1\nIP4.DNS[1]:192.168.1.1\nIP4.DNS[2]:1.1.1.1\n",
            &mut e,
        );
        assert_eq!(e.mac, "AA:BB:CC:DD:EE:FF");
        assert_eq!(e.address.as_deref(), Some("192.168.1.88/24"));
        assert_eq!(e.gateway.as_deref(), Some("192.168.1.1"));
        assert_eq!(e.dns, vec!["192.168.1.1", "1.1.1.1"]);
    }

    #[test]
    fn ip_settings_are_checked() {
        assert_eq!(cidr("192.168.1.50").as_deref(), Some("192.168.1.50/24"));
        assert_eq!(cidr(" 10.0.0.2/8 ").as_deref(), Some("10.0.0.2/8"));
        assert!(cidr("192.168.1.300/24").is_none());
        assert!(cidr("10.0.0.2/33").is_none());
        assert_eq!(
            dns_list("1.1.1.1, 8.8.8.8"),
            Some(vec!["1.1.1.1".into(), "8.8.8.8".into()])
        );
        assert_eq!(dns_list(""), Some(vec![]));
        assert!(dns_list("1.1.1.1 nope").is_none());
        let mut c = IpConfig {
            manual: true,
            address: "192.168.1.50/24".into(),
            gateway: "192.168.1.1".into(),
            dns: "1.1.1.1".into(),
        };
        assert!(c.validate().is_ok());
        c.gateway = "x".into();
        assert!(c.validate().is_err());
        assert!(
            IpConfig::default().validate().is_ok(),
            "automatic needs nothing"
        );
    }

    #[test]
    fn set_ethernet_ip_commands() {
        let mut r = FakeRunner::new();
        let cfg = IpConfig {
            manual: true,
            address: "192.168.1.50".into(),
            gateway: "192.168.1.1".into(),
            dns: "1.1.1.1,8.8.8.8".into(),
        };
        set_ethernet_ip(&mut r, "Wired connection 1", &cfg).unwrap();
        assert_eq!(
            r.seen[0].args,
            [
                "connection",
                "modify",
                "Wired connection 1",
                "ipv4.method",
                "manual",
                "ipv4.addresses",
                "192.168.1.50/24",
                "ipv4.gateway",
                "192.168.1.1",
                "ipv4.dns",
                "1.1.1.1 8.8.8.8"
            ]
        );
        assert_eq!(r.seen[1].args, ["connection", "up", "Wired connection 1"]);
        let mut r = FakeRunner::new();
        set_ethernet_ip(&mut r, "W", &IpConfig::default()).unwrap();
        assert_eq!(r.seen[0].args[3..5], ["ipv4.method", "auto"]);
        assert_eq!(r.seen[0].args[6], "");
        let mut r = FakeRunner::new();
        let bad = IpConfig {
            manual: true,
            address: "nope".into(),
            ..Default::default()
        };
        assert!(set_ethernet_ip(&mut r, "W", &bad).is_err());
        assert!(r.seen.is_empty(), "nothing runs for invalid settings");
    }

    #[test]
    fn terse_escapes() {
        assert_eq!(split_terse(r"a\:b:c\\d:"), vec!["a:b", r"c\d", ""]);
    }

    #[test]
    fn wifi_list_merges_bands_and_sorts() {
        // The same SSID appears on 2.4 and 5 GHz with different signals and
        // they are NOT adjacent; the old dedup_by() missed exactly this case.
        let text = "\
 :Home:40:WPA2
 :Cafe Free:80:
 :Office\\:5G:70:WPA2 WPA3
 ::90:WPA2
*:Home:75:WPA2
 :Home:55:WPA2
";
        let nets = parse_wifi_list(text);
        let names: Vec<_> = nets.iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(names, vec!["Home", "Cafe Free", "Office:5G"]);
        assert_eq!(nets[0].signal, 75);
        assert!(nets[0].in_use);
        assert!(!nets[1].secure);
        assert!(nets[2].secure);
    }

    #[test]
    fn active_connections_skip_loopback() {
        let text = "lo:loopback:lo\nHome:802-11-wireless:wlan0\nWired connection 1:802-3-ethernet:enp3s0\n";
        let c = parse_active_connections(text);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].kind, "wifi");
        assert_eq!(c[1].kind, "ethernet");
        let st = NetStatus {
            online: true,
            connections: c,
        };
        assert_eq!(st.summary(), "Home");
    }

    #[test]
    fn online_detection() {
        assert!(parse_online("connected:full\n"));
        assert!(parse_online("connected:unknown\n"));
        assert!(!parse_online("connected (local only):limited\n"));
        assert!(!parse_online("disconnected:none\n"));
        assert!(!parse_online(""));
    }

    #[test]
    fn connect_open_network_sends_no_password() {
        let mut r = FakeRunner::new();
        connect(&mut r, "Cafe Free", None).unwrap();
        assert_eq!(
            r.seen[0].args,
            vec!["device", "wifi", "connect", "Cafe Free"]
        );
    }

    #[test]
    fn connect_failure_is_friendly() {
        let mut r = FakeRunner::new();
        r.push(Output::failed(
            4,
            "Error: Connection activation failed: Secrets were required, but not provided.",
        ));
        let err = connect(&mut r, "Home", Some("wrong")).unwrap_err();
        assert_eq!(
            err.message(),
            "Could not join Home: the password was not accepted"
        );
        assert_eq!(r.seen[0].args[4..], ["password", "wrong"]);
    }

    #[test]
    fn radio_state() {
        let mut r = FakeRunner::new();
        r.push(Output::ok("enabled\n"));
        assert!(wifi_enabled(&mut r).unwrap());
        r.push(Output::ok("disabled\n"));
        assert!(!wifi_enabled(&mut r).unwrap());
    }

    #[test]
    fn no_wifi_adapter_means_no_switch() {
        let mut r = FakeRunner::new();
        r.push(Output::ok("ethernet\nbridge\nloopback\n"));
        assert_eq!(wifi_state(&mut r).unwrap(), None);
        assert_eq!(r.seen.len(), 1);
        let mut r = FakeRunner::new();
        r.push(Output::ok("ethernet\nwifi\n"));
        r.push(Output::ok("enabled\n"));
        assert_eq!(wifi_state(&mut r).unwrap(), Some(true));
    }
}
