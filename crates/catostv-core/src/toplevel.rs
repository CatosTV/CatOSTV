//! Which app windows exist, and bringing one to the front, through the
//! compositor's `zwlr_foreign_toplevel_management_v1` protocol (labwc
//! implements it). A tiny std-only Wayland client: it speaks the wire
//! protocol over the compositor socket, binds `wl_seat` and the toplevel
//! manager, reads the window list, and can ask for one window to be
//! activated (labwc raises and focuses it, switching to its desktop).
//!
//! Why: every app shares the App desktop, and switching to it showed
//! whichever window was on top, FreeTube instead of the Android app you
//! had just opened.
//!
//! Wire format (wayland.freedesktop.org, "Wire Format"): each message is
//! `object id: u32`, `size << 16 | opcode: u32`, then arguments; `uint`,
//! `object` and `new_id` are 32-bit, a string is its length including the
//! NUL then the bytes padded to 4, an array is its byte length then the
//! bytes padded to 4. Little-endian on all CatosTV targets.

use crate::error::{Error, Result};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

pub const MANAGER_INTERFACE: &str = "zwlr_foreign_toplevel_manager_v1";

const DISPLAY: u32 = 1;
const REGISTRY: u32 = 2;
/// zwlr_foreign_toplevel_handle_v1 `state` values.
const STATE_MINIMIZED: u32 = 1;
const STATE_ACTIVATED: u32 = 2;
const STATE_FULLSCREEN: u32 = 3;

/// One app window as the compositor reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Toplevel {
    /// Protocol object id (valid only for the connection that listed it).
    pub handle: u32,
    pub app_id: String,
    pub title: String,
    pub activated: bool,
    pub minimized: bool,
    pub fullscreen: bool,
}

/// What CatosTV can do to an app window from the remote
/// (the window buttons cannot be clicked with a remote).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    Close,
    Minimize,
    Fullscreen(bool),
}

enum Arg<'a> {
    U(u32),
    S(&'a str),
}

struct Conn {
    s: UnixStream,
    buf: Vec<u8>,
    next_id: u32,
}

fn rd_u32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
}

/// A string argument at `*at`; advances `*at`.
fn rd_str(b: &[u8], at: &mut usize) -> Option<String> {
    let len = rd_u32(b, *at)? as usize;
    *at += 4;
    if len == 0 {
        return Some(String::new());
    }
    let s = b.get(*at..*at + len - 1)?;
    *at += len.div_ceil(4) * 4;
    Some(String::from_utf8_lossy(s).into_owned())
}

/// An array of u32 at `*at`; advances `*at`.
fn rd_u32_array(b: &[u8], at: &mut usize) -> Option<Vec<u32>> {
    let len = rd_u32(b, *at)? as usize;
    *at += 4;
    let data = b.get(*at..*at + len)?;
    *at += len.div_ceil(4) * 4;
    Some(
        data.as_chunks::<4>()
            .0
            .iter()
            .map(|&c| u32::from_le_bytes(c))
            .collect(),
    )
}

fn encode(obj: u32, op: u16, args: &[Arg]) -> Vec<u8> {
    let mut body = Vec::new();
    for a in args {
        match a {
            Arg::U(v) => body.extend_from_slice(&v.to_le_bytes()),
            Arg::S(s) => {
                let len = s.len() as u32 + 1;
                body.extend_from_slice(&len.to_le_bytes());
                body.extend_from_slice(s.as_bytes());
                body.push(0);
                while body.len() % 4 != 0 {
                    body.push(0);
                }
            }
        }
    }
    let size = (8 + body.len()) as u32;
    let mut msg = Vec::with_capacity(size as usize);
    msg.extend_from_slice(&obj.to_le_bytes());
    msg.extend_from_slice(&((size << 16) | op as u32).to_le_bytes());
    msg.extend_from_slice(&body);
    msg
}

impl Conn {
    fn new(s: UnixStream) -> Self {
        let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = s.set_write_timeout(Some(Duration::from_secs(3)));
        Self {
            s,
            buf: Vec::new(),
            next_id: REGISTRY + 1,
        }
    }

    fn send(&mut self, obj: u32, op: u16, args: &[Arg]) -> Result<()> {
        self.s
            .write_all(&encode(obj, op, args))
            .map_err(|e| Error::msg(format!("compositor connection: {e}")))
    }

    fn new_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Next message: (object, opcode, arguments).
    fn read_msg(&mut self) -> Result<(u32, u16, Vec<u8>)> {
        loop {
            if self.buf.len() >= 8 {
                let obj = rd_u32(&self.buf, 0).unwrap_or(0);
                let word = rd_u32(&self.buf, 4).unwrap_or(0);
                let size = (word >> 16) as usize;
                if size < 8 {
                    return Err(Error::msg("bad message from the compositor"));
                }
                if self.buf.len() >= size {
                    let args = self.buf[8..size].to_vec();
                    self.buf.drain(..size);
                    return Ok((obj, (word & 0xffff) as u16, args));
                }
            }
            let mut chunk = [0u8; 4096];
            let n = self
                .s
                .read(&mut chunk)
                .map_err(|e| Error::msg(format!("compositor connection: {e}")))?;
            if n == 0 {
                return Err(Error::msg("the compositor closed the connection"));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// `wl_display.sync`, then hand every message to `on` until the
    /// callback fires.
    fn roundtrip(&mut self, mut on: impl FnMut(u32, u16, &[u8])) -> Result<()> {
        let cb = self.new_id();
        self.send(DISPLAY, 0, &[Arg::U(cb)])?;
        loop {
            let (obj, op, args) = self.read_msg()?;
            if obj == cb && op == 0 {
                return Ok(());
            }
            if obj == DISPLAY && op == 0 {
                let mut at = 8;
                let msg = rd_str(&args, &mut at).unwrap_or_default();
                return Err(Error::msg(format!("compositor error: {msg}")));
            }
            on(obj, op, &args);
        }
    }
}

/// The compositor socket of this session (`$WAYLAND_DISPLAY`, relative to
/// `$XDG_RUNTIME_DIR`; default `wayland-0`).
pub fn socket_path() -> Option<PathBuf> {
    let name = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    let p = PathBuf::from(&name);
    if p.is_absolute() {
        return Some(p);
    }
    Some(PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join(name))
}

/// Windows plus the ids needed to act on them.
struct Session {
    conn: Conn,
    seat: u32,
    windows: Vec<Toplevel>,
}

fn session(s: UnixStream) -> Result<Session> {
    let mut conn = Conn::new(s);
    conn.send(DISPLAY, 1, &[Arg::U(REGISTRY)])?;
    let mut seat_global = None;
    let mut mgr_global = None;
    conn.roundtrip(|obj, op, args| {
        if obj == REGISTRY && op == 0 {
            let mut at = 4;
            let name = rd_u32(args, 0).unwrap_or(0);
            let iface = rd_str(args, &mut at).unwrap_or_default();
            let version = rd_u32(args, at).unwrap_or(1);
            match iface.as_str() {
                "wl_seat" if seat_global.is_none() => seat_global = Some((name, version)),
                MANAGER_INTERFACE => mgr_global = Some((name, version)),
                _ => {}
            }
        }
    })?;
    let (Some((seat_name, _)), Some((mgr_name, mgr_ver))) = (seat_global, mgr_global) else {
        return Err(Error::msg(
            "the compositor does not offer window management (zwlr_foreign_toplevel_manager_v1)",
        ));
    };
    let seat = conn.new_id();
    conn.send(
        REGISTRY,
        0,
        &[
            Arg::U(seat_name),
            Arg::S("wl_seat"),
            Arg::U(1),
            Arg::U(seat),
        ],
    )?;
    let mgr = conn.new_id();
    conn.send(
        REGISTRY,
        0,
        &[
            Arg::U(mgr_name),
            Arg::S(MANAGER_INTERFACE),
            Arg::U(mgr_ver.min(3)),
            Arg::U(mgr),
        ],
    )?;
    let mut windows: Vec<Toplevel> = Vec::new();
    conn.roundtrip(|obj, op, args| {
        if obj == mgr && op == 0 {
            if let Some(h) = rd_u32(args, 0) {
                windows.push(Toplevel {
                    handle: h,
                    ..Toplevel::default()
                });
            }
            return;
        }
        let Some(w) = windows.iter_mut().find(|w| w.handle == obj) else {
            return;
        };
        let mut at = 0;
        match op {
            0 => w.title = rd_str(args, &mut at).unwrap_or_default(),
            1 => w.app_id = rd_str(args, &mut at).unwrap_or_default(),
            4 => {
                let states = rd_u32_array(args, &mut at).unwrap_or_default();
                w.minimized = states.contains(&STATE_MINIMIZED);
                w.activated = states.contains(&STATE_ACTIVATED);
                w.fullscreen = states.contains(&STATE_FULLSCREEN);
            }
            6 => w.handle = 0, // closed
            _ => {}
        }
    })?;
    windows.retain(|w| w.handle != 0);
    Ok(Session {
        conn,
        seat,
        windows,
    })
}

fn connect() -> Result<UnixStream> {
    let p = socket_path().ok_or_else(|| Error::msg("XDG_RUNTIME_DIR is not set"))?;
    UnixStream::connect(&p).map_err(|e| Error::msg(format!("{}: {e}", p.display())))
}

/// Every app window, in the compositor's order.
pub fn list() -> Result<Vec<Toplevel>> {
    list_on(connect()?)
}

pub fn list_on(s: UnixStream) -> Result<Vec<Toplevel>> {
    Ok(session(s)?.windows)
}

/// Does `app_id` belong to the window pattern `want`? Case-insensitive;
/// a trailing `*` matches a prefix (`waydroid*`).
pub fn matches(app_id: &str, want: &str) -> bool {
    let a = app_id.to_ascii_lowercase();
    let w = want.to_ascii_lowercase();
    match w.strip_suffix('*') {
        Some(prefix) => a.starts_with(prefix),
        None => a == w,
    }
}

/// Bring the first window matching `want` (see [`matches`]) to the front.
/// `Ok(false)` when there is no such window.
pub fn activate(want: &str) -> Result<bool> {
    activate_on(connect()?, want)
}

pub fn activate_on(s: UnixStream, want: &str) -> Result<bool> {
    let mut ses = session(s)?;
    // The newest matching window (windows are listed in creation order):
    // Android's composer can leave an older Waydroid window behind for a
    // moment after a restart, holding its last frame.
    let Some(w) = ses
        .windows
        .iter()
        .rev()
        .find(|w| matches(&w.app_id, want))
        .cloned()
    else {
        return Ok(false);
    };
    if w.minimized {
        ses.conn.send(w.handle, 3, &[])?; // unset_minimized
    }
    ses.conn.send(w.handle, 4, &[Arg::U(ses.seat)])?; // activate(seat)
    ses.conn.roundtrip(|_, _, _| {})?;
    Ok(true)
}

/// Close, minimise or (un)fullscreen the newest window matching `want`
/// (Minimize applies to every match: a stale duplicate must go too).
/// `Ok(false)` when there is no such window.
pub fn control(want: &str, action: WindowAction) -> Result<bool> {
    control_on(connect()?, want, action)
}

pub fn control_on(s: UnixStream, want: &str, action: WindowAction) -> Result<bool> {
    let mut ses = session(s)?;
    let targets: Vec<Toplevel> = ses
        .windows
        .iter()
        .rev()
        .filter(|w| matches(&w.app_id, want))
        .take(if action == WindowAction::Minimize {
            usize::MAX
        } else {
            1
        })
        .cloned()
        .collect();
    if targets.is_empty() {
        return Ok(false);
    }
    for w in targets {
        match action {
            WindowAction::Close => ses.conn.send(w.handle, 5, &[])?,
            WindowAction::Minimize => {
                if !w.minimized {
                    ses.conn.send(w.handle, 2, &[])?
                }
            }
            // set_fullscreen(output: nullable), null = the compositor picks.
            WindowAction::Fullscreen(true) => ses.conn.send(w.handle, 8, &[Arg::U(0)])?,
            WindowAction::Fullscreen(false) => ses.conn.send(w.handle, 9, &[])?,
        }
    }
    ses.conn.roundtrip(|_, _, _| {})?;
    Ok(true)
}

/// Minimise every window matching `want` except the newest. Waydroid
/// keeps an older window of the same app around after its display
/// re-plugs, holding the last frame; shown on top of the live one it
/// freezes Android's whole screen path (seen on the TV: two
/// `waydroid.org.smarttube.stable` windows). Returns how many were put
/// away.
pub fn minimize_stale(want: &str) -> Result<usize> {
    minimize_stale_on(connect()?, want)
}

pub fn minimize_stale_on(s: UnixStream, want: &str) -> Result<usize> {
    let mut ses = session(s)?;
    let mut matches_: Vec<Toplevel> = ses
        .windows
        .iter()
        .filter(|w| matches(&w.app_id, want))
        .cloned()
        .collect();
    matches_.pop(); // the newest stays
    let stale: Vec<Toplevel> = matches_.into_iter().filter(|w| !w.minimized).collect();
    for w in &stale {
        ses.conn.send(w.handle, 2, &[])?; // set_minimized
    }
    if !stale.is_empty() {
        ses.conn.roundtrip(|_, _, _| {})?;
    }
    Ok(stale.len())
}

/// Bring the newest window matching `want` forward only if it is not the
/// active one already. Every configure event the compositor sends to a
/// Waydroid window is a display "re-plug" to Waydroid's HAL on the
/// Android TV image (a screen reset and a stale window left behind
/// each time), so Android windows are touched as rarely as possible:
/// never minimized, activated once per app switch. `Ok(false)` = no
/// such window.
pub fn raise_if_needed(want: &str) -> Result<bool> {
    raise_if_needed_on(connect()?, want)
}

pub fn raise_if_needed_on(s: UnixStream, want: &str) -> Result<bool> {
    let mut ses = session(s)?;
    let Some(w) = ses
        .windows
        .iter()
        .rev()
        .find(|w| matches(&w.app_id, want))
        .cloned()
    else {
        return Ok(false);
    };
    if w.activated && !w.minimized {
        return Ok(true);
    }
    if w.minimized {
        ses.conn.send(w.handle, 3, &[])?; // unset_minimized
    }
    ses.conn.send(w.handle, 4, &[Arg::U(ses.seat)])?; // activate(seat)
    ses.conn.roundtrip(|_, _, _| {})?;
    Ok(true)
}

/// Keep the newest window matching `keep` in front and put every other
/// window matching `others` away. With the Android TV image each Android
/// app has its own window (`waydroid.<package>`), so "the newest Waydroid
/// window" is no longer the one in front (a background app popped
/// up on top of the one being used); the package Android reports in front
/// decides instead. `Ok(false)` when no window matches `keep` (the caller
/// falls back to the newest-window rule).
pub fn keep_front(keep: &str, others: &str) -> Result<bool> {
    keep_front_on(connect()?, keep, others)
}

pub fn keep_front_on(s: UnixStream, keep: &str, others: &str) -> Result<bool> {
    let mut ses = session(s)?;
    let Some(front) = ses
        .windows
        .iter()
        .rev()
        .find(|w| matches(&w.app_id, keep))
        .cloned()
    else {
        return Ok(false);
    };
    let away: Vec<Toplevel> = ses
        .windows
        .iter()
        .filter(|w| w.handle != front.handle && matches(&w.app_id, others) && !w.minimized)
        .cloned()
        .collect();
    for w in &away {
        ses.conn.send(w.handle, 2, &[])?; // set_minimized
    }
    if front.minimized || !front.activated {
        if front.minimized {
            ses.conn.send(front.handle, 3, &[])?; // unset_minimized
        }
        ses.conn.send(front.handle, 4, &[Arg::U(ses.seat)])?; // activate(seat)
    }
    if !away.is_empty() || front.minimized || !front.activated {
        ses.conn.roundtrip(|_, _, _| {})?;
    }
    Ok(true)
}

/// Is the window matching `want` full screen? `None` when it is not there.
pub fn is_fullscreen(want: &str) -> Option<bool> {
    list()
        .ok()?
        .into_iter()
        .rev()
        .find(|w| matches(&w.app_id, want))
        .map(|w| w.fullscreen)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted compositor on the other end of a socket pair.
    struct Fake {
        s: UnixStream,
        buf: Vec<u8>,
    }

    impl Fake {
        fn read(&mut self) -> (u32, u16, Vec<u8>) {
            loop {
                if self.buf.len() >= 8 {
                    let size = (rd_u32(&self.buf, 4).unwrap() >> 16) as usize;
                    if self.buf.len() >= size {
                        let obj = rd_u32(&self.buf, 0).unwrap();
                        let op = (rd_u32(&self.buf, 4).unwrap() & 0xffff) as u16;
                        let args = self.buf[8..size].to_vec();
                        self.buf.drain(..size);
                        return (obj, op, args);
                    }
                }
                let mut c = [0u8; 1024];
                let n = self.s.read(&mut c).unwrap();
                assert!(n > 0, "client hung up");
                self.buf.extend_from_slice(&c[..n]);
            }
        }
        fn send(&mut self, obj: u32, op: u16, args: &[Arg]) {
            self.s.write_all(&encode(obj, op, args)).unwrap();
        }
        fn send_array(&mut self, obj: u32, op: u16, vals: &[u32]) {
            let mut body = ((vals.len() * 4) as u32).to_le_bytes().to_vec();
            for v in vals {
                body.extend_from_slice(&v.to_le_bytes());
            }
            let mut m = obj.to_le_bytes().to_vec();
            m.extend_from_slice(&((((8 + body.len()) as u32) << 16) | op as u32).to_le_bytes());
            m.extend_from_slice(&body);
            self.s.write_all(&m).unwrap();
        }
        /// Answer the registry handshake and list two windows.
        fn handshake(&mut self) {
            assert_eq!(self.read().0, DISPLAY); // get_registry
            let (obj, op, args) = self.read(); // sync
            assert_eq!((obj, op), (DISPLAY, 0));
            let cb = rd_u32(&args, 0).unwrap();
            self.send(REGISTRY, 0, &[Arg::U(7), Arg::S("wl_output"), Arg::U(4)]);
            self.send(REGISTRY, 0, &[Arg::U(8), Arg::S("wl_seat"), Arg::U(7)]);
            self.send(
                REGISTRY,
                0,
                &[Arg::U(9), Arg::S(MANAGER_INTERFACE), Arg::U(3)],
            );
            self.send(cb, 0, &[Arg::U(1)]);
            let (_, _, seat_bind) = self.read();
            let mut at = 4;
            assert_eq!(rd_str(&seat_bind, &mut at).unwrap(), "wl_seat");
            let (_, _, mgr_bind) = self.read();
            let mut at = 4;
            assert_eq!(rd_str(&mgr_bind, &mut at).unwrap(), MANAGER_INTERFACE);
            let mgr = rd_u32(&mgr_bind, at + 4).unwrap();
            let (_, _, sync) = self.read();
            let cb = rd_u32(&sync, 0).unwrap();
            // Creation order: a stale Waydroid window, FreeTube, then the
            // live Waydroid window (minimized).
            for (h, app, title, states) in [
                (
                    0xff00_0002u32,
                    "waydroid.org.smarttube.stable",
                    "SmartTube",
                    vec![],
                ),
                (
                    0xff00_0000,
                    "io.freetubeapp.FreeTube",
                    "FreeTube",
                    vec![STATE_ACTIVATED],
                ),
                (0xff00_0001, "Waydroid", "Waydroid", vec![STATE_MINIMIZED]),
            ] {
                self.send(mgr, 0, &[Arg::U(h)]);
                self.send(h, 0, &[Arg::S(title)]);
                self.send(h, 1, &[Arg::S(app)]);
                self.send_array(h, 4, &states);
                self.send(h, 5, &[]);
            }
            self.send(cb, 0, &[Arg::U(2)]);
        }
    }

    fn pair() -> (UnixStream, Fake) {
        let (a, b) = UnixStream::pair().unwrap();
        (
            a,
            Fake {
                s: b,
                buf: Vec::new(),
            },
        )
    }

    #[test]
    fn lists_windows() {
        let (client, mut fake) = pair();
        let t = std::thread::spawn(move || fake.handshake());
        let w = list_on(client).unwrap();
        t.join().unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].app_id, "waydroid.org.smarttube.stable");
        assert_eq!(w[1].app_id, "io.freetubeapp.FreeTube");
        assert!(w[1].activated && !w[1].minimized);
        assert_eq!(w[2].title, "Waydroid");
        assert!(w[2].minimized);
    }

    #[test]
    fn activates_a_minimized_window() {
        let (client, mut fake) = pair();
        let t = std::thread::spawn(move || {
            fake.handshake();
            let (obj, op, _) = fake.read();
            assert_eq!((obj, op), (0xff00_0001, 3), "unset_minimized first");
            let (obj, op, args) = fake.read();
            assert_eq!((obj, op), (0xff00_0001, 4), "then activate");
            assert_eq!(
                rd_u32(&args, 0),
                Some(4),
                "with the bound seat (id 4: 3 was the first sync)"
            );
            let (_, _, sync) = fake.read();
            fake.send(rd_u32(&sync, 0).unwrap(), 0, &[Arg::U(3)]);
        });
        assert!(activate_on(client, "waydroid*").unwrap());
        t.join().unwrap();
    }

    #[test]
    fn controls_a_window() {
        let (client, mut fake) = pair();
        let t = std::thread::spawn(move || {
            fake.handshake();
            let (obj, op, args) = fake.read();
            assert_eq!((obj, op), (0xff00_0001, 8), "set_fullscreen");
            assert_eq!(rd_u32(&args, 0), Some(0), "null output");
            let (_, _, sync) = fake.read();
            fake.send(rd_u32(&sync, 0).unwrap(), 0, &[Arg::U(3)]);
        });
        assert!(control_on(client, "waydroid*", WindowAction::Fullscreen(true)).unwrap());
        t.join().unwrap();
        let (client, mut fake) = pair();
        let t = std::thread::spawn(move || {
            fake.handshake();
            let (obj, op, _) = fake.read();
            assert_eq!((obj, op), (0xff00_0001, 5), "close");
            let (_, _, sync) = fake.read();
            fake.send(rd_u32(&sync, 0).unwrap(), 0, &[Arg::U(3)]);
        });
        assert!(control_on(client, "waydroid*", WindowAction::Close).unwrap());
        t.join().unwrap();
    }

    #[test]
    fn minimizes_stale_duplicates() {
        let (client, mut fake) = pair();
        let t = std::thread::spawn(move || {
            fake.handshake();
            let (obj, op, _) = fake.read();
            assert_eq!((obj, op), (0xff00_0002, 2), "the older window is minimized");
            let (_, _, sync) = fake.read();
            fake.send(rd_u32(&sync, 0).unwrap(), 0, &[Arg::U(3)]);
        });
        assert_eq!(minimize_stale_on(client, "waydroid*").unwrap(), 1);
        t.join().unwrap();
        // Nothing to do when only one matches.
        let (client, mut fake) = pair();
        let t = std::thread::spawn(move || fake.handshake());
        assert_eq!(minimize_stale_on(client, "io.freetubeapp.*").unwrap(), 0);
        t.join().unwrap();
    }

    #[test]
    fn keeps_the_front_android_app_and_puts_the_others_away() {
        // SmartTube (older, not activated) is the app Android reports in
        // front; the plain "Waydroid" window (minimized) stays away and
        // FreeTube is not an Android window at all.
        let (client, mut fake) = pair();
        let t = std::thread::spawn(move || {
            fake.handshake();
            let (obj, op, args) = fake.read();
            assert_eq!((obj, op), (0xff00_0002, 4), "activate(seat)");
            assert_eq!(rd_u32(&args, 0), Some(4));
            let (_, _, sync) = fake.read();
            fake.send(rd_u32(&sync, 0).unwrap(), 0, &[Arg::U(3)]);
        });
        assert!(keep_front_on(client, "waydroid.org.smarttube.stable", "waydroid*").unwrap());
        t.join().unwrap();
        // No window for that package: the caller falls back.
        let (client, mut fake) = pair();
        let t = std::thread::spawn(move || fake.handshake());
        assert!(!keep_front_on(client, "waydroid.com.android.vending", "waydroid*").unwrap());
        t.join().unwrap();
    }

    #[test]
    fn raise_only_when_not_in_front() {
        // FreeTube is activated already: nothing is sent.
        let (client, mut fake) = pair();
        let t = std::thread::spawn(move || fake.handshake());
        assert!(raise_if_needed_on(client, "io.freetubeapp.FreeTube").unwrap());
        t.join().unwrap();
        // SmartTube is not: activated, nothing minimized.
        let (client, mut fake) = pair();
        let t = std::thread::spawn(move || {
            fake.handshake();
            let (obj, op, _) = fake.read();
            assert_eq!((obj, op), (0xff00_0002, 4), "activate");
            let (_, _, sync) = fake.read();
            fake.send(rd_u32(&sync, 0).unwrap(), 0, &[Arg::U(3)]);
        });
        assert!(raise_if_needed_on(client, "waydroid.org.smarttube.stable").unwrap());
        t.join().unwrap();
    }

    #[test]
    fn missing_window_and_patterns() {
        let (client, mut fake) = pair();
        let t = std::thread::spawn(move || fake.handshake());
        assert!(!activate_on(client, "org.nothing").unwrap());
        t.join().unwrap();
        assert!(matches("waydroid.org.smarttube.stable", "Waydroid*"));
        assert!(matches("Waydroid", "waydroid"));
        assert!(!matches("waydroidx", "waydroid"));
    }
}
