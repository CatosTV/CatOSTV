//! KDE Connect helpers (Arch package `kdeconnect`, extra; provides
//! `/usr/bin/kdeconnectd` and `/usr/bin/kdeconnect-cli`).
//!
//! `kdeconnectd` must run in the CatosTV session (labwc autostart starts
//! it). Phones running the KDE Connect app
//! then find the TV on the LAN (UDP/TCP 1714–1764).
//!
//! What works on labwc (expected; runtime UNVERIFIED):
//!
//! * pairing (see below), ping, share files/URLs/text, clipboard;
//! * media control: the phone controls MPRIS players on the TV, Firefox
//!   (YouTube) exposes MPRIS; mpv needs the `mpv-mpris` package;
//! * volume control (PulseAudio API via `pipewire-pulse`).
//!
//! What does **not** work on labwc: KDE Connect "Remote input" (mouse pad /
//! keyboard). On Wayland its mousepad plugin injects input through the
//! xdg-desktop-portal **RemoteDesktop** portal + libei, which the wlroots
//! portal (`xdg-desktop-portal-wlr`) does not implement. Use the CatosTV phone
//! remote ([`crate::remote`]) for D-pad and typing.
//!
//! Pairing: `kdeconnect-cli` can *request* pairing (`--pair -d <id>`) and
//! unpair, but it has no option to accept an **incoming** request; KDE
//! normally shows a notification for that, and CatosTV has no notification
//! daemon. We therefore read and answer requests over the session D-Bus
//! with `busctl` (systemd, always installed):
//!
//! ```text
//! busctl --user get-property org.kde.kdeconnect /modules/kdeconnect/devices/<id> \
//!        org.kde.kdeconnect.device isReachable isPaired isPairRequestedByPeer
//! busctl --user call org.kde.kdeconnect /modules/kdeconnect/devices/<id> \
//!        org.kde.kdeconnect.device acceptPairing      # or cancelPairing
//! ```
//!
//! Device ids are validated (`[A-Za-z0-9_]{1,64}`, the form KDE Connect uses
//! in D-Bus object paths) before they are put into any argv.

use crate::error::{Error, Result};
use crate::process::{Cmd, Runner};
use std::time::Duration;

pub const SERVICE: &str = "org.kde.kdeconnect";
pub const DEVICE_INTERFACE: &str = "org.kde.kdeconnect.device";
pub const DAEMON_INTERFACE: &str = "org.kde.kdeconnect.daemon";
pub const DAEMON_PATH: &str = "/modules/kdeconnect";
/// `kdeconnect-cli -l` waits ~2 s for discovery before printing.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KdeDevice {
    pub id: String,
    pub name: String,
    pub paired: bool,
    pub reachable: bool,
    /// The phone asked to pair and is waiting for the TV to accept.
    pub pair_requested_by_peer: bool,
}

/// `[A-Za-z0-9_]{1,64}`
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn check_id(id: &str) -> Result<()> {
    if is_valid_id(id) {
        Ok(())
    } else {
        Err(Error::msg("invalid KDE Connect device id"))
    }
}

/// D-Bus object path of a device.
pub fn device_path(id: &str) -> String {
    format!("{DAEMON_PATH}/devices/{id}")
}

/// Parse `kdeconnect-cli --list-devices --id-name-only` (`<id> <name>` per
/// line). Lines with an invalid id are skipped.
pub fn parse_id_name_list(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let (id, name) = l.split_once(' ').unwrap_or((l, ""));
            is_valid_id(id).then(|| (id.to_string(), name.trim().to_string()))
        })
        .collect()
}

/// Parse boolean lines of `busctl get-property` (`b true` / `b false`), in
/// order. Any other line yields `None` at that position.
pub fn parse_busctl_bools(text: &str) -> Vec<Option<bool>> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| match l.trim() {
            "b true" => Some(true),
            "b false" => Some(false),
            _ => None,
        })
        .collect()
}

fn busctl(args: &[&str]) -> Cmd {
    Cmd::new("busctl").arg("--user").args(args.iter().copied())
}

fn device_state(r: &mut dyn Runner, id: &str) -> (bool, bool, bool) {
    let path = device_path(id);
    let full = busctl(&[
        "get-property",
        SERVICE,
        &path,
        DEVICE_INTERFACE,
        "isReachable",
        "isPaired",
        "isPairRequestedByPeer",
    ]);
    if let Ok(out) = r.run(&full)
        && out.success()
        && let [Some(reach), Some(paired), Some(req)] = parse_busctl_bools(&out.stdout)[..]
    {
        return (reach, paired, req);
    }
    // Older daemons without isPairRequestedByPeer.
    let short = busctl(&[
        "get-property",
        SERVICE,
        &path,
        DEVICE_INTERFACE,
        "isReachable",
        "isPaired",
    ]);
    if let Ok(out) = r.run(&short)
        && out.success()
        && let [Some(reach), Some(paired)] = parse_busctl_bools(&out.stdout)[..]
    {
        return (reach, paired, false);
    }
    (false, false, false)
}

/// All devices kdeconnectd knows (paired or currently visible).
pub fn devices(r: &mut dyn Runner) -> Result<Vec<KdeDevice>> {
    let out = r.run_ok(
        &Cmd::new("kdeconnect-cli")
            .args(["--list-devices", "--id-name-only"])
            .timeout(LIST_TIMEOUT),
    )?;
    let mut list = Vec::new();
    for (id, name) in parse_id_name_list(&out.stdout) {
        let (reachable, paired, req) = device_state(r, &id);
        list.push(KdeDevice {
            id,
            name,
            paired,
            reachable,
            pair_requested_by_peer: req && !paired,
        });
    }
    Ok(list)
}

/// Devices waiting for the TV to accept their pairing request.
pub fn pending_requests(r: &mut dyn Runner) -> Result<Vec<KdeDevice>> {
    Ok(devices(r)?
        .into_iter()
        .filter(|d| d.pair_requested_by_peer)
        .collect())
}

/// Accept a pairing request made from the phone.
pub fn accept_pairing(r: &mut dyn Runner, id: &str) -> Result<()> {
    check_id(id)?;
    r.run_ok(&busctl(&[
        "call",
        SERVICE,
        &device_path(id),
        DEVICE_INTERFACE,
        "acceptPairing",
    ]))?;
    Ok(())
}

/// Decline a pairing request (or cancel our own).
pub fn reject_pairing(r: &mut dyn Runner, id: &str) -> Result<()> {
    check_id(id)?;
    r.run_ok(&busctl(&[
        "call",
        SERVICE,
        &device_path(id),
        DEVICE_INTERFACE,
        "cancelPairing",
    ]))?;
    Ok(())
}

/// Ask a phone to pair with the TV (the phone shows the request).
pub fn pair(r: &mut dyn Runner, id: &str) -> Result<()> {
    check_id(id)?;
    r.run_ok(
        &Cmd::new("kdeconnect-cli")
            .args(["--pair", "-d", id])
            .timeout(Duration::from_secs(35)),
    )?;
    Ok(())
}

pub fn unpair(r: &mut dyn Runner, id: &str) -> Result<()> {
    check_id(id)?;
    r.run_ok(&Cmd::new("kdeconnect-cli").args(["--unpair", "-d", id]))?;
    Ok(())
}

pub fn ping(r: &mut dyn Runner, id: &str) -> Result<()> {
    check_id(id)?;
    r.run_ok(&Cmd::new("kdeconnect-cli").args(["--ping", "-d", id]))?;
    Ok(())
}

/// Re-announce on the network (after Wi-Fi changes).
pub fn refresh(r: &mut dyn Runner) -> Result<()> {
    r.run_ok(&Cmd::new("kdeconnect-cli").arg("--refresh"))?;
    Ok(())
}

/// This TV's own KDE Connect id.
pub fn my_id(r: &mut dyn Runner) -> Result<String> {
    let out = r.run_ok(&Cmd::new("kdeconnect-cli").arg("--my-id"))?;
    let id = out.stdout.trim().to_string();
    if is_valid_id(&id) {
        Ok(id)
    } else {
        Err(Error::msg("kdeconnectd returned an unexpected id"))
    }
}

/// Set the name phones see (e.g. "CatosTV"). 1–32 characters, no control
/// characters.
pub fn set_name(r: &mut dyn Runner, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 32 || name.chars().any(char::is_control) {
        return Err(Error::msg("the name must be 1–32 characters"));
    }
    r.run_ok(&busctl(&[
        "call",
        SERVICE,
        DAEMON_PATH,
        DAEMON_INTERFACE,
        "setAnnouncedName",
        "s",
        name,
    ]))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Output, fake::FakeRunner};

    #[test]
    fn ids() {
        assert!(is_valid_id("a1b2c3d4e5f60718"));
        assert!(is_valid_id("_0f3c2b1a_4d5e_6f70_8192_a3b4c5d6e7f8_"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("../../etc"));
        assert!(!is_valid_id("a b"));
        assert!(!is_valid_id("a-b"));
        assert!(!is_valid_id(&"a".repeat(65)));
        assert_eq!(device_path("abc"), "/modules/kdeconnect/devices/abc");
    }

    #[test]
    fn list_parsing() {
        let text = "a1b2c3d4e5f60718 Pixel 8\n_0f3c_ab Living room tablet\n../bad Evil\n\nlonely\n";
        assert_eq!(
            parse_id_name_list(text),
            vec![
                ("a1b2c3d4e5f60718".to_string(), "Pixel 8".to_string()),
                ("_0f3c_ab".to_string(), "Living room tablet".to_string()),
                ("lonely".to_string(), String::new()),
            ]
        );
        assert_eq!(
            parse_busctl_bools("b true\nb false\n\ni 2\n"),
            vec![Some(true), Some(false), None]
        );
    }

    #[test]
    fn devices_and_pending_requests() {
        let mut r = FakeRunner::new();
        r.push(Output::ok("phone1 Pixel 8\nphone2 Old Phone\n"));
        r.push(Output::ok("b true\nb false\nb true\n")); // phone1: requested by peer
        r.push(Output::failed(1, "Unknown property")); // phone2: old daemon
        r.push(Output::ok("b false\nb true\n"));
        let list = devices(&mut r).unwrap();
        assert_eq!(
            list,
            vec![
                KdeDevice {
                    id: "phone1".into(),
                    name: "Pixel 8".into(),
                    paired: false,
                    reachable: true,
                    pair_requested_by_peer: true
                },
                KdeDevice {
                    id: "phone2".into(),
                    name: "Old Phone".into(),
                    paired: true,
                    reachable: false,
                    pair_requested_by_peer: false
                },
            ]
        );
        assert_eq!(r.seen[0].program, "kdeconnect-cli");
        assert_eq!(r.seen[0].args, ["--list-devices", "--id-name-only"]);
        assert_eq!(
            r.seen[1].args,
            [
                "--user",
                "get-property",
                "org.kde.kdeconnect",
                "/modules/kdeconnect/devices/phone1",
                "org.kde.kdeconnect.device",
                "isReachable",
                "isPaired",
                "isPairRequestedByPeer"
            ]
        );
        let mut r = FakeRunner::new();
        r.push(Output::ok("phone1 Pixel 8\n"));
        r.push(Output::ok("b true\nb false\nb true\n"));
        assert_eq!(pending_requests(&mut r).unwrap().len(), 1);
    }

    #[test]
    fn actions_build_exact_argv_and_validate_ids() {
        let mut r = FakeRunner::new();
        accept_pairing(&mut r, "phone1").unwrap();
        reject_pairing(&mut r, "phone1").unwrap();
        pair(&mut r, "phone1").unwrap();
        unpair(&mut r, "phone1").unwrap();
        ping(&mut r, "phone1").unwrap();
        refresh(&mut r).unwrap();
        set_name(&mut r, " CatosTV ").unwrap();
        let argv: Vec<String> = r.seen.iter().map(|c| c.display()).collect();
        assert_eq!(
            argv,
            vec![
                "busctl --user call org.kde.kdeconnect /modules/kdeconnect/devices/phone1 org.kde.kdeconnect.device acceptPairing",
                "busctl --user call org.kde.kdeconnect /modules/kdeconnect/devices/phone1 org.kde.kdeconnect.device cancelPairing",
                "kdeconnect-cli --pair -d phone1",
                "kdeconnect-cli --unpair -d phone1",
                "kdeconnect-cli --ping -d phone1",
                "kdeconnect-cli --refresh",
                "busctl --user call org.kde.kdeconnect /modules/kdeconnect org.kde.kdeconnect.daemon setAnnouncedName s CatosTV",
            ]
        );
        let n = r.seen.len();
        assert!(accept_pairing(&mut r, "x; rm").is_err());
        assert!(pair(&mut r, "-d").is_err());
        assert!(unpair(&mut r, "").is_err());
        assert!(set_name(&mut r, "").is_err());
        assert!(set_name(&mut r, &"x".repeat(33)).is_err());
        assert!(set_name(&mut r, "a\nb").is_err());
        assert_eq!(r.seen.len(), n, "nothing ran for invalid input");
    }

    #[test]
    fn my_id_is_validated() {
        let mut r = FakeRunner::new();
        r.push(Output::ok("abc_123\n"));
        assert_eq!(my_id(&mut r).unwrap(), "abc_123");
        r.push(Output::ok("not an id\n"));
        assert!(my_id(&mut r).is_err());
    }
}
