//! A Wayland relay in front of Android's display HAL that hides one
//! protocol from it (`catostv-wlfilter`).
//!
//! Why: the display HAL of every released WayDroid-ATV TV image deadlocks
//! against Android's WindowManager. Its Wayland thread calls into
//! Android while holding its `windowsMutex` (focus → `setFocusedTask`,
//! window creation → `getAppName`), and Android, holding its own lock,
//! calls the HAL's `setIdleInhibit` whenever an app toggles keep-screen-on
//! (video players do, constantly), which waits for `windowsMutex`. Both
//! wait forever; Android's Watchdog kills its core after 60 s ("Android
//! restarted"). `setIdleInhibit` is the only HAL call Android makes under
//! that lock, and it returns *before* taking the mutex when the compositor
//! offers no `zwp_idle_inhibit_manager_v1`. CatosTV never blanks the screen,
//! so the protocol does nothing here anyway.
//!
//! So the Waydroid session connects to this relay (socket
//! [`ANDROID_DISPLAY`] in `$XDG_RUNTIME_DIR`) instead of labwc. The relay
//! forwards every byte and file descriptor both ways and drops exactly one
//! kind of message: the registry's announcement of a hidden global. The
//! HAL never sees it, never binds it, and the lock-order cycle cannot form.
//!
//! This file is the protocol logic (std only, unit-tested); the sockets and
//! fd passing are in `bin/catostv-wlfilter.rs`.
//!
//! Wire format (wayland.freedesktop.org, "Wire Format"): a message is
//! `object id: u32`, `size << 16 | opcode: u32`, then arguments; `uint` and
//! `new_id` are 32-bit, a string is its length including the NUL then the
//! bytes padded to 4. Little-endian on all CatosTV targets. Registries are
//! created by `wl_display.get_registry(new_id)` (object 1, opcode 1);
//! `wl_registry.global(name: uint, interface: string, version: uint)` is
//! the registry's event 0.

/// Socket name (in `$XDG_RUNTIME_DIR`) the Waydroid session connects to.
pub const ANDROID_DISPLAY: &str = "wayland-android";

/// Globals the HAL must not see.
pub const HIDDEN_GLOBALS: &[&str] = &["zwp_idle_inhibit_manager_v1"];

const DISPLAY: u32 = 1;
const GET_REGISTRY: u16 = 1;
const GLOBAL: u16 = 0;
const HEADER: usize = 8;

/// Per-connection state: which objects are registries, and the partial
/// message at the end of each direction's stream.
#[derive(Debug, Default)]
pub struct Filter {
    registries: Vec<u32>,
    to_server: Vec<u8>,
    to_client: Vec<u8>,
    /// Set once a stream stops making sense (a size below the header):
    /// from then on everything is relayed untouched rather than guessed.
    passthrough: bool,
    /// Announcements dropped so far.
    pub dropped: u64,
}

fn rd_u32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// `(object, opcode, size)` of the message at the start of `b`, when the
/// header is complete and sane.
fn header(b: &[u8]) -> Option<(u32, u16, usize)> {
    let obj = rd_u32(b, 0)?;
    let w = rd_u32(b, 4)?;
    let size = (w >> 16) as usize;
    let op = (w & 0xffff) as u16;
    (size >= HEADER && size.is_multiple_of(4)).then_some((obj, op, size))
}

/// The `interface` string of a `wl_registry.global` message (`msg` is the
/// whole message), or None when it is malformed.
fn global_interface(msg: &[u8]) -> Option<&str> {
    let len = rd_u32(msg, 12)? as usize;
    if len == 0 {
        return None;
    }
    let bytes = msg.get(16..16 + len - 1)?;
    std::str::from_utf8(bytes).ok()
}

impl Filter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes the client (the HAL) sent, in order. The caller forwards them
    /// unchanged; this only learns which objects are registries.
    pub fn from_client(&mut self, data: &[u8]) {
        if self.passthrough {
            return;
        }
        self.to_server.extend_from_slice(data);
        let mut at = 0;
        while let Some(rest) = self.to_server.get(at..) {
            if rest.len() < HEADER {
                break;
            }
            let Some((obj, op, size)) = header(rest) else {
                self.passthrough = true;
                break;
            };
            if rest.len() < size {
                break;
            }
            if obj == DISPLAY
                && op == GET_REGISTRY
                && let Some(id) = rd_u32(rest, 8)
                && !self.registries.contains(&id)
            {
                self.registries.push(id);
            }
            at += size;
        }
        self.to_server.drain(..at);
    }

    /// Bytes the compositor sent, in order. Returns what the client gets:
    /// every complete message except hidden globals' announcements. A
    /// message split across calls is held back until it is complete.
    pub fn from_server(&mut self, data: &[u8]) -> Vec<u8> {
        if self.passthrough {
            return data.to_vec();
        }
        self.to_client.extend_from_slice(data);
        let mut out = Vec::with_capacity(data.len());
        let mut at = 0;
        loop {
            let rest = &self.to_client[at..];
            if rest.len() < HEADER {
                break;
            }
            let Some((obj, op, size)) = header(rest) else {
                // Give up parsing: pass the rest through as it is.
                self.passthrough = true;
                out.extend_from_slice(rest);
                at = self.to_client.len();
                break;
            };
            if rest.len() < size {
                break;
            }
            let msg = &rest[..size];
            let hidden = op == GLOBAL
                && self.registries.contains(&obj)
                && global_interface(msg).is_some_and(|i| HIDDEN_GLOBALS.contains(&i));
            if hidden {
                self.dropped += 1;
            } else {
                out.extend_from_slice(msg);
            }
            at += size;
        }
        self.to_client.drain(..at);
        out
    }

    /// Objects known to be registries (for logs and tests).
    pub fn registries(&self) -> &[u32] {
        &self.registries
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn msg(obj: u32, op: u16, body: &[u8]) -> Vec<u8> {
        let size = (HEADER + body.len()) as u32;
        let mut v = obj.to_le_bytes().to_vec();
        v.extend_from_slice(&((size << 16) | op as u32).to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    pub(crate) fn global(registry: u32, name: u32, interface: &str, version: u32) -> Vec<u8> {
        let mut body = name.to_le_bytes().to_vec();
        let len = interface.len() + 1;
        body.extend_from_slice(&(len as u32).to_le_bytes());
        body.extend_from_slice(interface.as_bytes());
        body.push(0);
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        body.extend_from_slice(&version.to_le_bytes());
        msg(registry, GLOBAL, &body)
    }

    fn get_registry(id: u32) -> Vec<u8> {
        msg(DISPLAY, GET_REGISTRY, &id.to_le_bytes())
    }

    #[test]
    fn hides_the_idle_inhibit_global_and_nothing_else() {
        let mut f = Filter::new();
        f.from_client(&get_registry(2));
        assert_eq!(f.registries(), [2]);
        let seat = global(2, 7, "wl_seat", 9);
        let idle = global(2, 8, "zwp_idle_inhibit_manager_v1", 1);
        let shm = global(2, 9, "wl_shm", 1);
        let mut stream = seat.clone();
        stream.extend_from_slice(&idle);
        stream.extend_from_slice(&shm);
        let mut want = seat.clone();
        want.extend_from_slice(&shm);
        assert_eq!(f.from_server(&stream), want);
        assert_eq!(f.dropped, 1);
        // global_remove (opcode 1) and events on other objects pass.
        let remove = msg(2, 1, &8u32.to_le_bytes());
        assert_eq!(f.from_server(&remove), remove);
        let other = msg(3, 0, &[1, 0, 0, 0]);
        assert_eq!(f.from_server(&other), other);
    }

    #[test]
    fn split_messages_are_reassembled() {
        let mut f = Filter::new();
        // get_registry arriving in two pieces still registers.
        let gr = get_registry(2);
        f.from_client(&gr[..5]);
        assert!(f.registries().is_empty());
        f.from_client(&gr[5..]);
        assert_eq!(f.registries(), [2]);
        let seat = global(2, 7, "wl_seat", 9);
        let idle = global(2, 8, "zwp_idle_inhibit_manager_v1", 1);
        let mut stream = seat.clone();
        stream.extend_from_slice(&idle);
        // Everything but the last 3 bytes: the seat message is complete and
        // goes out, the hidden one is still partial and is held.
        let cut = stream.len() - 3;
        assert_eq!(f.from_server(&stream[..cut]), seat);
        // The tail completes it: dropped, nothing out.
        assert_eq!(f.from_server(&stream[cut..]), Vec::<u8>::new());
        assert_eq!(f.dropped, 1);
        // A header split one byte at a time.
        let shm = global(2, 9, "wl_shm", 1);
        let mut got = Vec::new();
        for b in &shm {
            got.extend(f.from_server(&[*b]));
        }
        assert_eq!(got, shm);
    }

    #[test]
    fn every_registry_is_filtered_and_unknown_objects_are_not() {
        let mut f = Filter::new();
        f.from_client(&get_registry(2));
        f.from_client(&get_registry(5));
        let idle_on_5 = global(5, 8, "zwp_idle_inhibit_manager_v1", 1);
        assert_eq!(f.from_server(&idle_on_5), Vec::<u8>::new());
        // Object 9 was never announced as a registry: an event 0 with the
        // same shape is some other interface's and passes.
        let not_registry = global(9, 8, "zwp_idle_inhibit_manager_v1", 1);
        assert_eq!(f.from_server(&not_registry), not_registry);
        assert_eq!(f.dropped, 1);
    }

    #[test]
    fn garbage_switches_to_passthrough() {
        let mut f = Filter::new();
        f.from_client(&get_registry(2));
        // size 4 < header: not a Wayland stream any more.
        let bad = [2u8, 0, 0, 0, 0, 0, 4, 0, 0xaa, 0xbb];
        assert_eq!(f.from_server(&bad), bad);
        let idle = global(2, 8, "zwp_idle_inhibit_manager_v1", 1);
        assert_eq!(f.from_server(&idle), idle);
        assert_eq!(f.dropped, 0);
    }

    #[test]
    fn malformed_global_passes() {
        let mut f = Filter::new();
        f.from_client(&get_registry(2));
        // A "global" with a string length of 0: not ours to judge.
        let mut body = 8u32.to_le_bytes().to_vec();
        body.extend_from_slice(&0u32.to_le_bytes());
        let odd = msg(2, GLOBAL, &body);
        assert_eq!(f.from_server(&odd), odd);
    }

    #[test]
    fn constants_match_the_session_script() {
        let script = include_str!("../../../target-root/usr/local/lib/catostv/android-session");
        assert!(script.contains(&format!("FILTER=$RUN/{ANDROID_DISPLAY}")));
        assert!(script.contains("exec waydroid session start"));
        assert!(script.contains("OFF_FLAG=/etc/catostv/android-no-display-filter"));
        let autostart = include_str!("../../../target-root/home/catostv/.config/labwc/autostart");
        assert!(autostart.contains("supervise wlfilter"));
        assert_eq!(HIDDEN_GLOBALS, ["zwp_idle_inhibit_manager_v1"]);
    }
}
