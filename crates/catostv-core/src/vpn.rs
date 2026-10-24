//! VPN connections through NetworkManager (`nmcli`), the way GNOME and KDE
//! do it: WireGuard (built into NetworkManager) and OpenVPN (the
//! `networkmanager-openvpn` plugin). Every VPN provider offers its servers
//! as files of one of the two kinds, so one importer covers them all; VPN
//! apps that set up NetworkManager connections themselves show up too.
//!
//! A VPN is added switched off: adding one never reroutes the TV by
//! surprise. Turning one on is a separate, deliberate step.

use crate::error::{Error, Result};
use crate::net::split_terse;
use crate::process::{Cmd, Runner};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long `nmcli connection up` may take (a VPN handshake).
const UP_TIMEOUT: Duration = Duration::from_secs(45);
/// At most this many files are offered to add.
const MAX_FILES: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    WireGuard,
    /// OpenVPN or another NetworkManager VPN plugin.
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::WireGuard => "WireGuard",
            Kind::Other => "VPN",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Off,
    Connecting,
    On,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vpn {
    pub name: String,
    pub uuid: String,
    pub kind: Kind,
    /// NetworkManager turns it on by itself when the network comes up.
    pub autoconnect: bool,
    pub state: State,
}

impl Vpn {
    pub fn on(&self) -> bool {
        self.state == State::On
    }
}

/// Connection types that are VPNs (`nmcli` TYPE column).
pub fn is_vpn_type(t: &str) -> bool {
    matches!(t, "wireguard" | "vpn")
}

pub fn list_cmd() -> Cmd {
    Cmd::new("nmcli").args([
        "-t",
        "-f",
        "NAME,UUID,TYPE,AUTOCONNECT",
        "connection",
        "show",
    ])
}

pub fn active_cmd() -> Cmd {
    Cmd::new("nmcli").args([
        "-t",
        "-f",
        "UUID,TYPE,STATE",
        "connection",
        "show",
        "--active",
    ])
}

/// The VPNs in `nmcli` connection lists: `all` from [`list_cmd`], `active`
/// from [`active_cmd`]. Sorted by name.
pub fn parse(all: &str, active: &str) -> Vec<Vpn> {
    let states: Vec<(String, State)> = active
        .lines()
        .filter_map(|l| {
            let f = split_terse(l);
            (f.len() >= 3 && is_vpn_type(&f[1])).then(|| {
                let s = match f[2].as_str() {
                    "activated" => State::On,
                    "activating" => State::Connecting,
                    _ => State::Off,
                };
                (f[0].clone(), s)
            })
        })
        .collect();
    let mut v: Vec<Vpn> = all
        .lines()
        .filter_map(|l| {
            let f = split_terse(l);
            if f.len() < 4 || !is_vpn_type(&f[2]) || !is_uuid(&f[1]) {
                return None;
            }
            Some(Vpn {
                name: f[0].clone(),
                uuid: f[1].clone(),
                kind: if f[2] == "wireguard" {
                    Kind::WireGuard
                } else {
                    Kind::Other
                },
                autoconnect: f[3] == "yes",
                state: states
                    .iter()
                    .find(|(u, _)| *u == f[1])
                    .map_or(State::Off, |(_, s)| *s),
            })
        })
        .collect();
    v.sort_by_key(|x| x.name.to_lowercase());
    v
}

pub fn list(r: &mut dyn Runner) -> Result<Vec<Vpn>> {
    let all = r.run_ok(&list_cmd())?;
    let active = r.run_ok(&active_cmd())?;
    Ok(parse(&all.stdout, &active.stdout))
}

/// A NetworkManager connection UUID (what is passed to `nmcli`).
pub fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

fn check_uuid(uuid: &str) -> Result<()> {
    if is_uuid(uuid) {
        Ok(())
    } else {
        Err(Error::msg("That VPN can't be found"))
    }
}

/// Why `nmcli connection up` failed, said simply.
fn up_failure(name: &str, text: &str, timed_out: bool) -> Error {
    let why = if timed_out {
        "the server didn't answer"
    } else if text.contains("Secrets were required")
        || text.contains("no secrets")
        || text.contains("password")
    {
        "it needs a user name and password. Use the provider's WireGuard file, or its app from the Store"
    } else if text.contains("plugin") || text.contains("VPN service") {
        "OpenVPN isn't installed"
    } else {
        "the server didn't answer"
    };
    Error::msg(format!("Couldn't turn on {name}: {why}"))
}

/// Turn a VPN on (it may take a few seconds).
pub fn up(r: &mut dyn Runner, uuid: &str, name: &str) -> Result<()> {
    check_uuid(uuid)?;
    let out = r.run(
        &Cmd::new("nmcli")
            .args(["connection", "up", "uuid", uuid])
            .timeout(UP_TIMEOUT),
    )?;
    if out.success() {
        Ok(())
    } else {
        Err(up_failure(
            name,
            &format!("{} {}", out.stdout, out.stderr),
            out.timed_out,
        ))
    }
}

pub fn down(r: &mut dyn Runner, uuid: &str) -> Result<()> {
    check_uuid(uuid)?;
    r.run_ok(&Cmd::new("nmcli").args(["connection", "down", "uuid", uuid]))?;
    Ok(())
}

pub fn remove(r: &mut dyn Runner, uuid: &str) -> Result<()> {
    check_uuid(uuid)?;
    r.run_ok(&Cmd::new("nmcli").args(["connection", "delete", "uuid", uuid]))?;
    Ok(())
}

pub fn set_autoconnect(r: &mut dyn Runner, uuid: &str, on: bool) -> Result<()> {
    check_uuid(uuid)?;
    r.run_ok(&Cmd::new("nmcli").args([
        "connection",
        "modify",
        "uuid",
        uuid,
        "connection.autoconnect",
        if on { "yes" } else { "no" },
    ]))?;
    Ok(())
}

/// What kind of VPN file `path` is, by its name and (for WireGuard) by its
/// contents: `.conf` files are many things, only those with an
/// `[Interface]` section are WireGuard.
pub fn file_kind(path: &Path, text: &str) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "conf" if text.contains("[Interface]") && text.contains("PrivateKey") => {
            Some(Kind::WireGuard)
        }
        "ovpn" if text.contains("remote ") || text.contains("<ca>") => Some(Kind::Other),
        _ => None,
    }
}

/// The name the VPN gets: the file's name without `.conf` / `.ovpn`.
pub fn file_title(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("VPN")
        .to_string()
}

/// A WireGuard interface name is at most 15 characters of letters, digits,
/// `-` and `_`; providers' file names are often longer. The file is
/// imported under this name and the connection renamed afterwards.
pub fn interface_name(title: &str) -> String {
    let clean: String = title
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(12)
        .collect();
    if clean.is_empty() {
        "wg-vpn".into()
    } else {
        format!("wg-{}", &clean[..clean.len().min(12)])
    }
}

/// `nmcli connection import`'s answer: "Connection 'x' (UUID) successfully
/// added."
pub fn parse_import(out: &str) -> Option<String> {
    let a = out.find('(')? + 1;
    let b = a + out[a..].find(')')?;
    let u = &out[a..b];
    is_uuid(u).then(|| u.to_string())
}

/// Add a VPN from a WireGuard or OpenVPN file, switched off. `scratch` is
/// a private directory for the copy with a short name (the file holds the
/// private key). Returns the new VPN's name.
pub fn import(r: &mut dyn Runner, path: &Path, scratch: &Path) -> Result<String> {
    let text = std::fs::read_to_string(path).map_err(|_| Error::msg("That file can't be read"))?;
    let kind = file_kind(path, &text)
        .ok_or_else(|| Error::msg("That isn't a WireGuard or OpenVPN file"))?;
    let title = file_title(path);
    let (ty, file) = match kind {
        Kind::WireGuard => {
            let tmp = scratch.join(format!("{}.conf", interface_name(&title)));
            write_private(&tmp, &text)
                .map_err(|e| Error::msg(format!("Couldn't add the VPN ({e})")))?;
            ("wireguard", tmp)
        }
        Kind::Other => ("openvpn", path.to_path_buf()),
    };
    let out = r.run(&Cmd::new("nmcli").args([
        "connection".to_string(),
        "import".into(),
        "type".into(),
        ty.into(),
        "file".into(),
        file.display().to_string(),
    ]));
    if kind == Kind::WireGuard {
        let _ = std::fs::remove_file(&file);
    }
    let out = out?;
    let Some(uuid) = parse_import(&out.stdout).filter(|_| out.success()) else {
        let text = format!("{} {}", out.stdout, out.stderr);
        return Err(Error::msg(if text.contains("plugin") {
            "OpenVPN isn't installed".to_string()
        } else {
            format!("Couldn't add {title}")
        }));
    };
    // Off, and not by itself: the user turns it on when they want it.
    let _ = r.run(&Cmd::new("nmcli").args(["connection", "down", "uuid", &uuid]));
    r.run_ok(&Cmd::new("nmcli").args([
        "connection",
        "modify",
        "uuid",
        &uuid,
        "connection.autoconnect",
        "no",
        "connection.id",
        &title,
    ]))?;
    Ok(title)
}

/// Write `text` to a new file only this user can read (a VPN file holds
/// its private key).
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text.as_bytes())
}

/// VPN files in `dirs` (and one level of folders below them), newest
/// first: what "Add a VPN" offers. Downloads, a USB stick, files sent from
/// a phone.
pub fn find_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let look = |dir: &Path, found: &mut Vec<(std::time::SystemTime, PathBuf)>| {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut sub = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                sub.push(p);
                continue;
            }
            // A VPN file is small; anything big is not one.
            if md.len() > 64 * 1024 {
                continue;
            }
            let ext = p
                .extension()
                .and_then(|x| x.to_str())
                .map(str::to_ascii_lowercase);
            if !matches!(ext.as_deref(), Some("conf" | "ovpn")) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&p)
                && file_kind(&p, &text).is_some()
            {
                found.push((md.modified().unwrap_or(std::time::UNIX_EPOCH), p));
            }
        }
        sub
    };
    for d in dirs {
        for s in look(d, &mut found) {
            look(&s, &mut found);
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    found.dedup_by(|a, b| a.1 == b.1);
    found.into_iter().take(MAX_FILES).map(|(_, p)| p).collect()
}

/// Where VPN files are looked for: Downloads (KDE Connect puts files
/// sent from a phone there), the home folder, and USB sticks.
pub fn file_dirs(home: &Path) -> Vec<PathBuf> {
    let mut v = vec![home.join("Downloads"), home.to_path_buf()];
    for base in ["/run/media", "/media"] {
        if let Ok(rd) = std::fs::read_dir(base) {
            for e in rd.flatten() {
                // /run/media/USER/STICK, or /media/STICK
                v.push(e.path());
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Output, fake::FakeRunner};

    const U1: &str = "1b4e28ba-2fa1-11d2-883f-0016d3cca427";
    const U2: &str = "2c5f39cb-3fa1-11d2-883f-0016d3cca428";

    #[test]
    fn lists_only_vpns_with_their_state() {
        let all = format!(
            "Wired connection 1:{}:802-3-ethernet:yes\nmullvad-se:{U1}:wireguard:no\nOffice:{U2}:vpn:yes\nlo:ffffffff-ffff-ffff-ffff-ffffffffffff:loopback:no\n",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
        );
        let active = format!("{U1}:wireguard:activated\n");
        let v = parse(&all, &active);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "mullvad-se");
        assert_eq!(
            (v[0].kind, v[0].state, v[0].autoconnect),
            (Kind::WireGuard, State::On, false)
        );
        assert_eq!(
            (v[1].name.as_str(), v[1].kind, v[1].state),
            ("Office", Kind::Other, State::Off)
        );
        assert!(v[1].autoconnect);
    }

    #[test]
    fn up_and_down_by_uuid_only() {
        let mut r = FakeRunner::new();
        up(&mut r, U1, "x").unwrap();
        down(&mut r, U1).unwrap();
        assert_eq!(
            r.seen[0].display(),
            format!("nmcli connection up uuid {U1}")
        );
        assert_eq!(
            r.seen[1].display(),
            format!("nmcli connection down uuid {U1}")
        );
        assert!(up(&mut r, "; rm -rf /", "x").is_err());
        assert_eq!(r.seen.len(), 2, "nothing run for a bad id");
    }

    #[test]
    fn a_password_vpn_says_what_to_do() {
        let mut r = FakeRunner::new();
        r.push(Output::failed(
            4,
            "Error: Connection activation failed: Secrets were required, but not provided",
        ));
        let e = up(&mut r, U1, "Office").unwrap_err();
        assert!(e.message().contains("WireGuard file"), "{e}");
    }

    #[test]
    fn knows_vpn_files_by_name_and_contents() {
        let wg = "[Interface]\nPrivateKey = abc=\nAddress = 10.0.0.2/32\n[Peer]\n";
        assert_eq!(file_kind(Path::new("se.conf"), wg), Some(Kind::WireGuard));
        assert_eq!(
            file_kind(Path::new("x.conf"), "[Unit]\nDescription=x"),
            None
        );
        assert_eq!(
            file_kind(Path::new("nl.ovpn"), "client\nremote nl.example 1194\n"),
            Some(Kind::Other)
        );
        assert_eq!(file_kind(Path::new("a.txt"), wg), None);
    }

    #[test]
    fn long_file_names_get_a_short_interface() {
        let n = interface_name("Mullvad-se-sto-wg-001");
        assert!(n.len() <= 15, "{n}");
        assert!(n.starts_with("wg-"));
        assert_eq!(interface_name("!!!"), "wg-vpn");
    }

    #[test]
    fn reads_the_new_connection_id() {
        assert_eq!(
            parse_import(&format!("Connection 'wg-se' ({U1}) successfully added.\n")).as_deref(),
            Some(U1)
        );
        assert_eq!(parse_import("Error: failed"), None);
    }

    #[test]
    fn import_adds_it_switched_off_under_its_file_name() {
        let t = crate::testutil::TempDir::new("vpnimport");
        let f = t.write(
            "Mullvad-se-sto-wg-001.conf",
            "[Interface]\nPrivateKey = k=\n[Peer]\n",
        );
        let scratch = t.path().join("s");
        std::fs::create_dir(&scratch).unwrap();
        let mut r = FakeRunner::new();
        r.push(Output::ok(format!(
            "Connection 'wg-Mullvad-se-s' ({U1}) successfully added."
        )));
        assert_eq!(
            import(&mut r, &f, &scratch).unwrap(),
            "Mullvad-se-sto-wg-001"
        );
        assert!(
            r.seen[0]
                .display()
                .starts_with("nmcli connection import type wireguard file ")
        );
        assert_eq!(
            r.seen[1].display(),
            format!("nmcli connection down uuid {U1}")
        );
        let m = r.seen[2].display();
        assert!(
            m.contains("connection.autoconnect no")
                && m.contains("connection.id Mullvad-se-sto-wg-001"),
            "{m}"
        );
        // The private key's copy is gone again.
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    }

    #[test]
    fn finds_vpn_files_one_folder_deep() {
        let t = crate::testutil::TempDir::new("vpnfind");
        t.write("se.conf", "[Interface]\nPrivateKey = k=\n");
        t.write("other.conf", "[Unit]\n");
        std::fs::create_dir(t.path().join("Proton")).unwrap();
        t.write("Proton/nl.ovpn", "client\nremote nl 1194\n");
        let found = find_files(&[t.path().to_path_buf()]);
        let names: Vec<String> = found.iter().map(|p| file_title(p)).collect();
        assert_eq!(found.len(), 2, "{names:?}");
        assert!(names.contains(&"se".to_string()) && names.contains(&"nl".to_string()));
    }
}
