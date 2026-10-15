//! SSDP (UPnP discovery) on UDP 239.255.255.250:1900.
//!
//! Used by `catostv-castd` to announce the DLNA MediaRenderer and the DIAL
//! (YouTube) device. Pure builders/parsers are unit-tested; [`run`] is a
//! thin, timeout-driven loop around them.
//!
//! Behaviour:
//!
//! * binds UDP port 1900 with `SO_REUSEADDR` (so it can coexist with other
//!   SSDP listeners) and joins the multicast group on **every IPv4
//!   interface** reported by `ip -4 -o addr show` (re-checked every 30 s, so
//!   Wi-Fi reconnects and DHCP changes are picked up). If no interface is
//!   found it joins on `INADDR_ANY`, which lets the kernel pick the
//!   default-route interface only;
//! * answers `M-SEARCH` for `ssdp:all`, `upnp:rootdevice`, `uuid:…`, the
//!   device types and the service types (including the DIAL service
//!   `urn:dial-multiscreen-org:service:dial:1`). `LOCATION` uses the local
//!   address that routes to the requester (UDP connect trick, nothing is
//!   sent). Only requests from private / link-local / loopback addresses are
//!   answered and responses are rate limited, so the daemon cannot be used
//!   as a traffic amplifier;
//! * multicasts `ssdp:alive` every 30 s (`max-age=1800`) out of each
//!   interface (a socket bound to the interface address selects the
//!   outgoing interface on Linux) and `ssdp:byebye` on shutdown.
//!
//! Limits: IPv4 only; responses are sent immediately instead of being spread
//! over the `MX` window (fine for a home LAN).

use crate::process::{Cmd, Runner};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
pub const SSDP_PORT: u16 = 1900;
/// `CACHE-CONTROL: max-age` of every announcement, seconds.
pub const MAX_AGE: u32 = 1800;
/// How often `ssdp:alive` is repeated.
pub const NOTIFY_INTERVAL: Duration = Duration::from_secs(30);
/// How often the interface list is re-read.
pub const INTERFACE_REFRESH: Duration = Duration::from_secs(30);
/// Largest datagram we look at.
pub const MAX_DATAGRAM: usize = 2048;
/// Search responses allowed per second (all requesters together).
pub const MAX_RESPONSES_PER_SEC: u32 = 200;

pub const ROOT_DEVICE: &str = "upnp:rootdevice";
pub const DIAL_SERVICE: &str = "urn:dial-multiscreen-org:service:dial:1";

/// One root device to announce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertisement {
    /// Bare UUID (without the `uuid:` prefix).
    pub uuid: String,
    /// Path of the device description on the HTTP server, e.g.
    /// `/dlna/description.xml`.
    pub location_path: String,
    pub device_type: String,
    pub service_types: Vec<String>,
}

impl Advertisement {
    /// All (NT, USN) pairs this device announces, in the order the UPnP
    /// spec lists them: rootdevice, uuid, device type, services.
    pub fn notification_types(&self) -> Vec<(String, String)> {
        let udn = format!("uuid:{}", self.uuid);
        let mut out = vec![
            (ROOT_DEVICE.to_string(), format!("{udn}::{ROOT_DEVICE}")),
            (udn.clone(), udn.clone()),
            (
                self.device_type.clone(),
                format!("{udn}::{}", self.device_type),
            ),
        ];
        for s in &self.service_types {
            out.push((s.clone(), format!("{udn}::{s}")));
        }
        out
    }

    /// (ST, USN) pairs to answer for a search target.
    pub fn search_matches(&self, st: &str) -> Vec<(String, String)> {
        let st = st.trim();
        if st == "ssdp:all" {
            return self.notification_types();
        }
        let udn = format!("uuid:{}", self.uuid);
        if st == ROOT_DEVICE {
            return vec![(st.to_string(), format!("{udn}::{ROOT_DEVICE}"))];
        }
        if st.eq_ignore_ascii_case(&udn) {
            return vec![(udn.clone(), udn)];
        }
        std::iter::once(&self.device_type)
            .chain(self.service_types.iter())
            .filter(|ours| type_matches(ours, st))
            .map(|_| (st.to_string(), format!("{udn}::{st}")))
            .take(1)
            .collect()
    }
}

/// `urn:…:Type:2` matches a search for `urn:…:Type:1` and `:2` (a device
/// must answer searches for lower versions it is compatible with).
fn type_matches(ours: &str, wanted: &str) -> bool {
    let (Some((our_base, our_ver)), Some((want_base, want_ver))) =
        (ours.rsplit_once(':'), wanted.rsplit_once(':'))
    else {
        return false;
    };
    match (our_ver.parse::<u32>(), want_ver.parse::<u32>()) {
        (Ok(o), Ok(w)) => our_base == want_base && w >= 1 && w <= o,
        _ => false,
    }
}

/// A parsed `M-SEARCH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MSearch {
    pub st: String,
    /// Seconds (0–5 after clamping); absent for unicast searches.
    pub mx: u32,
}

/// Parse an M-SEARCH datagram. Anything else (NOTIFY, responses, garbage,
/// missing `MAN: "ssdp:discover"`) yields `None`.
pub fn parse_msearch(data: &[u8]) -> Option<MSearch> {
    if data.len() > MAX_DATAGRAM {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    let mut lines = text.split('\n').map(|l| l.trim_end_matches('\r'));
    let first = lines.next()?;
    let mut parts = first.split_whitespace();
    if parts.next()? != "M-SEARCH" || parts.next()? != "*" || !parts.next()?.starts_with("HTTP/1.")
    {
        return None;
    }
    let mut st = None;
    let mut man_ok = false;
    let mut mx = 1;
    for line in lines.take(40) {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k.trim().to_ascii_uppercase().as_str() {
            "ST" => st = Some(v.to_string()),
            "MAN" => man_ok = v.trim_matches('"') == "ssdp:discover",
            "MX" => mx = v.parse::<u32>().unwrap_or(1).min(5),
            _ => {}
        }
    }
    let st = st.filter(|s| !s.is_empty() && s.len() <= 256)?;
    if !man_ok || st.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return None;
    }
    Some(MSearch { st, mx })
}

/// `SERVER` / `USER-AGENT` value: `Linux UPnP/1.0 CatosTV/<version>`.
pub fn server_string() -> String {
    format!("Linux UPnP/1.0 CatosTV/{}", crate::env::version())
}

/// Unicast answer to an M-SEARCH.
pub fn search_response(st: &str, usn: &str, location: &str, boot_id: u32) -> String {
    format!(
        "HTTP/1.1 200 OK\r\n\
         CACHE-CONTROL: max-age={MAX_AGE}\r\n\
         EXT:\r\n\
         LOCATION: {location}\r\n\
         SERVER: {}\r\n\
         ST: {st}\r\n\
         USN: {usn}\r\n\
         BOOTID.UPNP.ORG: {boot_id}\r\n\
         CONFIGID.UPNP.ORG: 1\r\n\
         \r\n",
        server_string()
    )
}

/// Multicast `ssdp:alive`.
pub fn notify_alive(nt: &str, usn: &str, location: &str, boot_id: u32) -> String {
    format!(
        "NOTIFY * HTTP/1.1\r\n\
         HOST: {MULTICAST_ADDR}:{SSDP_PORT}\r\n\
         CACHE-CONTROL: max-age={MAX_AGE}\r\n\
         LOCATION: {location}\r\n\
         NT: {nt}\r\n\
         NTS: ssdp:alive\r\n\
         SERVER: {}\r\n\
         USN: {usn}\r\n\
         BOOTID.UPNP.ORG: {boot_id}\r\n\
         CONFIGID.UPNP.ORG: 1\r\n\
         \r\n",
        server_string()
    )
}

/// Multicast `ssdp:byebye`.
pub fn notify_byebye(nt: &str, usn: &str, boot_id: u32) -> String {
    format!(
        "NOTIFY * HTTP/1.1\r\n\
         HOST: {MULTICAST_ADDR}:{SSDP_PORT}\r\n\
         NT: {nt}\r\n\
         NTS: ssdp:byebye\r\n\
         USN: {usn}\r\n\
         BOOTID.UPNP.ORG: {boot_id}\r\n\
         CONFIGID.UPNP.ORG: 1\r\n\
         \r\n"
    )
}

pub fn location_url(ip: Ipv4Addr, http_port: u16, path: &str) -> String {
    format!("http://{ip}:{http_port}{path}")
}

/// The local IPv4 address the kernel would use to reach `peer`. Uses a
/// connected UDP socket; no packet is sent.
pub fn local_ip_for(peer: Ipv4Addr) -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    sock.connect((peer, SSDP_PORT)).ok()?;
    match sock.local_addr().ok()? {
        SocketAddr::V4(a) if !a.ip().is_unspecified() => Some(*a.ip()),
        _ => None,
    }
}

/// Addresses we are willing to answer: RFC 1918, link-local, loopback and
/// carrier-grade NAT (100.64/10, used by some mesh/VPN LANs).
pub fn is_lan_source(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private()
        || ip.is_link_local()
        || ip.is_loopback()
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
}

/// Parse `ip -4 -o addr show` into non-loopback interface addresses.
pub fn parse_ip_addr_output(text: &str) -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        while let Some(w) = words.next() {
            if w == "inet" {
                if let Some(ip) = words
                    .next()
                    .and_then(|cidr| cidr.split('/').next())
                    .and_then(|s| s.parse::<Ipv4Addr>().ok())
                    && !ip.is_loopback()
                    && !ip.is_unspecified()
                    && !out.contains(&ip)
                {
                    out.push(ip);
                }
                break;
            }
        }
    }
    out
}

/// Current IPv4 interface addresses (empty if `ip` fails).
pub fn interface_addrs(r: &mut dyn Runner) -> Vec<Ipv4Addr> {
    match r.run(&Cmd::new("ip").args(["-4", "-o", "addr", "show", "up"])) {
        Ok(out) if out.success() => parse_ip_addr_output(&out.stdout),
        _ => Vec::new(),
    }
}

/// Compute the datagrams to send back for one received packet.
/// `local_ip` is the address to advertise to this requester.
pub fn responses_for(
    data: &[u8],
    adverts: &[Advertisement],
    local_ip: Ipv4Addr,
    http_port: u16,
    boot_id: u32,
) -> Vec<String> {
    let Some(search) = parse_msearch(data) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ad in adverts {
        let location = location_url(local_ip, http_port, &ad.location_path);
        for (st, usn) in ad.search_matches(&search.st) {
            out.push(search_response(&st, &usn, &location, boot_id));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Socket setup
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod ffi {
    #[repr(C)]
    pub struct SockAddrIn {
        pub sin_family: u16,
        pub sin_port: u16,
        pub sin_addr: u32,
        pub sin_zero: [u8; 8],
    }
    unsafe extern "C" {
        pub fn socket(domain: i32, ty: i32, protocol: i32) -> i32;
        pub fn setsockopt(
            fd: i32,
            level: i32,
            name: i32,
            value: *const core::ffi::c_void,
            len: u32,
        ) -> i32;
        pub fn bind(fd: i32, addr: *const SockAddrIn, len: u32) -> i32;
        pub fn close(fd: i32) -> i32;
    }
    pub const AF_INET: i32 = 2;
    pub const SOCK_DGRAM: i32 = 2;
    pub const SOCK_CLOEXEC: i32 = 0o2000000;
    pub const SOL_SOCKET: i32 = 1;
    pub const SO_REUSEADDR: i32 = 2;
}

/// Bind a UDP socket on `0.0.0.0:port` with `SO_REUSEADDR`, so several
/// SSDP listeners can share port 1900. Falls back to a plain bind.
pub fn bind_reuse(port: u16) -> io::Result<UdpSocket> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::FromRawFd;
        // SAFETY: plain socket syscalls on a fresh descriptor we own; the
        // sockaddr struct matches Linux `struct sockaddr_in`.
        unsafe {
            let fd = ffi::socket(ffi::AF_INET, ffi::SOCK_DGRAM | ffi::SOCK_CLOEXEC, 0);
            if fd >= 0 {
                let one: i32 = 1;
                let addr = ffi::SockAddrIn {
                    sin_family: ffi::AF_INET as u16,
                    sin_port: port.to_be(),
                    sin_addr: 0,
                    sin_zero: [0; 8],
                };
                let ok = ffi::setsockopt(
                    fd,
                    ffi::SOL_SOCKET,
                    ffi::SO_REUSEADDR,
                    &one as *const i32 as *const core::ffi::c_void,
                    4,
                ) == 0
                    && ffi::bind(fd, &addr, std::mem::size_of::<ffi::SockAddrIn>() as u32) == 0;
                if ok {
                    return Ok(UdpSocket::from_raw_fd(fd));
                }
                let err = io::Error::last_os_error();
                ffi::close(fd);
                if err.kind() == io::ErrorKind::AddrInUse {
                    return Err(err);
                }
            }
        }
    }
    UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))
}

// ---------------------------------------------------------------------------
// Daemon loop
// ---------------------------------------------------------------------------

/// What [`run`] announces.
#[derive(Debug, Clone)]
pub struct Config {
    pub adverts: Vec<Advertisement>,
    pub http_port: u16,
    /// UPnP 1.1 BOOTID (e.g. start time in seconds).
    pub boot_id: u32,
}

fn send_multicast(iface: Ipv4Addr, messages: &[String]) {
    // Binding to the interface address makes Linux route the multicast out
    // of that interface (no IP_MULTICAST_IF needed).
    let Ok(sock) = UdpSocket::bind((iface, 0)) else {
        return;
    };
    let _ = sock.set_multicast_ttl_v4(2);
    for m in messages {
        let _ = sock.send_to(m.as_bytes(), (MULTICAST_ADDR, SSDP_PORT));
    }
}

fn alive_messages(cfg: &Config, ip: Ipv4Addr) -> Vec<String> {
    let mut v = Vec::new();
    for ad in &cfg.adverts {
        let loc = location_url(ip, cfg.http_port, &ad.location_path);
        for (nt, usn) in ad.notification_types() {
            v.push(notify_alive(&nt, &usn, &loc, cfg.boot_id));
        }
    }
    v
}

fn byebye_messages(cfg: &Config) -> Vec<String> {
    cfg.adverts
        .iter()
        .flat_map(|ad| ad.notification_types())
        .map(|(nt, usn)| notify_byebye(&nt, &usn, cfg.boot_id))
        .collect()
}

/// Run SSDP until `stop` becomes true, then send `byebye`. Never busy-loops:
/// the socket has a 1 s read timeout.
pub fn run(cfg: Config, runner: &mut dyn Runner, stop: &AtomicBool) -> io::Result<()> {
    let sock = bind_reuse(SSDP_PORT)?;
    sock.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut joined: Vec<Ipv4Addr> = Vec::new();
    let mut joined_any = false;
    let mut last_refresh: Option<Instant> = None;
    let mut last_notify: Option<Instant> = None;
    let mut window_start = Instant::now();
    let mut sent_in_window = 0u32;
    let mut buf = [0u8; MAX_DATAGRAM];

    while !stop.load(Ordering::SeqCst) {
        if last_refresh.is_none_or(|t| t.elapsed() >= INTERFACE_REFRESH) {
            last_refresh = Some(Instant::now());
            let now = interface_addrs(runner);
            for ip in &now {
                if !joined.contains(ip) {
                    // Errors (e.g. already joined) are harmless.
                    let _ = sock.join_multicast_v4(&MULTICAST_ADDR, ip);
                    joined.push(*ip);
                    send_multicast(*ip, &alive_messages(&cfg, *ip));
                }
            }
            joined.retain(|ip| {
                let keep = now.contains(ip);
                if !keep {
                    let _ = sock.leave_multicast_v4(&MULTICAST_ADDR, ip);
                }
                keep
            });
            if joined.is_empty() && !joined_any {
                joined_any = sock
                    .join_multicast_v4(&MULTICAST_ADDR, &Ipv4Addr::UNSPECIFIED)
                    .is_ok();
            }
        }
        if last_notify.is_none_or(|t| t.elapsed() >= NOTIFY_INTERVAL) {
            last_notify = Some(Instant::now());
            for ip in &joined {
                send_multicast(*ip, &alive_messages(&cfg, *ip));
            }
        }
        let (n, src) = match sock.recv_from(&mut buf) {
            Ok(x) => x,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        let SocketAddr::V4(src) = src else { continue };
        if !is_lan_source(*src.ip()) || src.port() == 0 {
            continue;
        }
        let Some(local) = local_ip_for(*src.ip()).or_else(|| joined.first().copied()) else {
            continue;
        };
        if window_start.elapsed() >= Duration::from_secs(1) {
            window_start = Instant::now();
            sent_in_window = 0;
        }
        for msg in responses_for(&buf[..n], &cfg.adverts, local, cfg.http_port, cfg.boot_id) {
            if sent_in_window >= MAX_RESPONSES_PER_SEC {
                break;
            }
            sent_in_window += 1;
            let _ = sock.send_to(msg.as_bytes(), SocketAddrV4::new(*src.ip(), src.port()));
        }
    }
    let bye = byebye_messages(&cfg);
    for ip in &joined {
        send_multicast(*ip, &bye);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Output, fake::FakeRunner};

    fn renderer() -> Advertisement {
        Advertisement {
            uuid: "11111111-2222-4333-8444-555555555555".into(),
            location_path: "/dlna/description.xml".into(),
            device_type: "urn:schemas-upnp-org:device:MediaRenderer:1".into(),
            service_types: vec![
                "urn:schemas-upnp-org:service:AVTransport:1".into(),
                "urn:schemas-upnp-org:service:RenderingControl:1".into(),
            ],
        }
    }

    fn msearch(st: &str) -> Vec<u8> {
        format!(
            "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n"
        )
        .into_bytes()
    }

    #[test]
    fn parses_msearch() {
        let m = parse_msearch(&msearch("ssdp:all")).unwrap();
        assert_eq!(m.st, "ssdp:all");
        assert_eq!(m.mx, 2);
        // Lower-case header names, bare LF, no MX.
        let m = parse_msearch(b"M-SEARCH * HTTP/1.1\nman: ssdp:discover\nst: upnp:rootdevice\n\n")
            .unwrap();
        assert_eq!(m.st, "upnp:rootdevice");
        assert_eq!(m.mx, 1);
        assert!(parse_msearch(b"NOTIFY * HTTP/1.1\r\nNT: x\r\n\r\n").is_none());
        assert!(parse_msearch(b"M-SEARCH * HTTP/1.1\r\nST: ssdp:all\r\n\r\n").is_none());
        assert!(parse_msearch(b"M-SEARCH * HTTP/1.1\r\nMAN: \"ssdp:discover\"\r\n\r\n").is_none());
        assert!(parse_msearch(b"\xff\xfe").is_none());
        assert!(parse_msearch(b"").is_none());
    }

    #[test]
    fn garbage_datagrams_never_panic() {
        let mut seed: u64 = 42;
        for len in 0..600 {
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    (seed >> 33) as u8
                })
                .collect();
            assert!(responses_for(&data, &[renderer()], Ipv4Addr::LOCALHOST, 1, 1).is_empty());
        }
    }

    #[test]
    fn search_matching() {
        let ad = renderer();
        assert_eq!(ad.search_matches("ssdp:all").len(), 5);
        assert_eq!(
            ad.search_matches("upnp:rootdevice"),
            vec![(
                "upnp:rootdevice".to_string(),
                "uuid:11111111-2222-4333-8444-555555555555::upnp:rootdevice".to_string()
            )]
        );
        assert_eq!(
            ad.search_matches("uuid:11111111-2222-4333-8444-555555555555")[0].1,
            "uuid:11111111-2222-4333-8444-555555555555"
        );
        let st = "urn:schemas-upnp-org:service:AVTransport:1";
        assert_eq!(
            ad.search_matches(st),
            vec![(
                st.to_string(),
                format!("uuid:11111111-2222-4333-8444-555555555555::{st}")
            )]
        );
        assert!(
            ad.search_matches("urn:schemas-upnp-org:service:AVTransport:2")
                .is_empty()
        );
        assert!(ad.search_matches(DIAL_SERVICE).is_empty());
        assert!(ad.search_matches("uuid:someone-else").is_empty());
    }

    #[test]
    fn versioned_type_matching() {
        assert!(type_matches("urn:x:device:Foo:2", "urn:x:device:Foo:1"));
        assert!(type_matches("urn:x:device:Foo:2", "urn:x:device:Foo:2"));
        assert!(!type_matches("urn:x:device:Foo:1", "urn:x:device:Foo:2"));
        assert!(!type_matches("urn:x:device:Foo:1", "urn:x:device:Foo:0"));
        assert!(!type_matches("urn:x:device:Foo:1", "urn:x:device:Bar:1"));
        assert!(!type_matches("urn:x:device:Foo:1", "garbage"));
    }

    #[test]
    fn dial_search_gets_dial_response() {
        let dial = Advertisement {
            uuid: "aaaaaaaa-2222-4333-8444-555555555555".into(),
            location_path: "/dial/description.xml".into(),
            device_type: "urn:dial-multiscreen-org:device:dial:1".into(),
            service_types: vec![DIAL_SERVICE.into()],
        };
        let out = responses_for(
            &msearch(DIAL_SERVICE),
            &[renderer(), dial],
            Ipv4Addr::new(192, 168, 1, 9),
            49494,
            7,
        );
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert!(r.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(r.contains("\r\nLOCATION: http://192.168.1.9:49494/dial/description.xml\r\n"));
        assert!(r.contains(&format!("\r\nST: {DIAL_SERVICE}\r\n")));
        assert!(r.contains(&format!(
            "\r\nUSN: uuid:aaaaaaaa-2222-4333-8444-555555555555::{DIAL_SERVICE}\r\n"
        )));
        assert!(r.contains("\r\nEXT:\r\n"));
        assert!(r.contains("CACHE-CONTROL: max-age=1800"));
        assert!(r.ends_with("\r\n\r\n"));
    }

    #[test]
    fn notify_builders() {
        let a = notify_alive(
            "upnp:rootdevice",
            "uuid:x::upnp:rootdevice",
            "http://1.2.3.4:5/d",
            3,
        );
        assert!(a.starts_with("NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\n"));
        assert!(a.contains("NTS: ssdp:alive\r\n"));
        assert!(a.contains("LOCATION: http://1.2.3.4:5/d\r\n"));
        assert!(a.contains("BOOTID.UPNP.ORG: 3\r\n"));
        let b = notify_byebye("upnp:rootdevice", "uuid:x::upnp:rootdevice", 3);
        assert!(b.contains("NTS: ssdp:byebye\r\n"));
        assert!(!b.contains("LOCATION"));
        let cfg = Config {
            adverts: vec![renderer()],
            http_port: 1,
            boot_id: 1,
        };
        assert_eq!(alive_messages(&cfg, Ipv4Addr::LOCALHOST).len(), 5);
        assert_eq!(byebye_messages(&cfg).len(), 5);
    }

    #[test]
    fn ip_addr_parsing() {
        let text = "\
1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever preferred_lft forever
2: enp3s0    inet 192.168.1.23/24 brd 192.168.1.255 scope global dynamic noprefixroute enp3s0\\       valid_lft 85000sec
3: wlan0    inet 10.0.0.5/16 brd 10.0.255.255 scope global wlan0\\       valid_lft forever
3: wlan0    inet 10.0.0.5/16 secondary duplicate
garbage line inet notanip/3
";
        assert_eq!(
            parse_ip_addr_output(text),
            vec![Ipv4Addr::new(192, 168, 1, 23), Ipv4Addr::new(10, 0, 0, 5)]
        );
        let mut r = FakeRunner::new();
        r.push(Output::ok(text));
        assert_eq!(interface_addrs(&mut r).len(), 2);
        assert_eq!(r.seen[0].args, ["-4", "-o", "addr", "show", "up"]);
        r.push(Output::failed(1, "no ip"));
        assert!(interface_addrs(&mut r).is_empty());
    }

    #[test]
    fn lan_sources() {
        assert!(is_lan_source(Ipv4Addr::new(192, 168, 0, 10)));
        assert!(is_lan_source(Ipv4Addr::new(10, 1, 2, 3)));
        assert!(is_lan_source(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_lan_source(Ipv4Addr::new(169, 254, 3, 4)));
        assert!(is_lan_source(Ipv4Addr::new(100, 100, 1, 1)));
        assert!(is_lan_source(Ipv4Addr::LOCALHOST));
        assert!(!is_lan_source(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_lan_source(Ipv4Addr::new(100, 128, 0, 1)));
    }

    #[test]
    fn local_ip_for_loopback() {
        assert_eq!(local_ip_for(Ipv4Addr::LOCALHOST), Some(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn reuse_bind_allows_two_listeners() {
        let port = UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let a = bind_reuse(port).unwrap();
        let b = bind_reuse(port).unwrap();
        assert_eq!(a.local_addr().unwrap().port(), port);
        assert_eq!(b.local_addr().unwrap().port(), port);
    }
}
