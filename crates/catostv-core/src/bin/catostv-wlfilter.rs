//! `catostv-wlfilter`, a Wayland relay between the Waydroid session and the
//! compositor that hides `zwp_idle_inhibit_manager_v1` from Android's
//! display HAL, so the HAL ↔ WindowManager lock cycle cannot form.
//! The protocol logic and the why are in `catostv_core::wlfilter`; this
//! file owns the sockets and the file-descriptor passing.
//!
//! ```text
//! catostv-wlfilter                      listen on $XDG_RUNTIME_DIR/wayland-android,
//!                                     relay each client to $XDG_RUNTIME_DIR/$WAYLAND_DISPLAY
//! catostv-wlfilter --socket PATH        listen there instead
//! catostv-wlfilter --upstream PATH      relay to that compositor socket instead
//! ```
//!
//! Runs as the TV account in the labwc session (supervised by autostart,
//! before the shell). Never exits by itself; one log line per connection.
//! Every byte goes through unchanged except the dropped announcements;
//! file descriptors (dmabuf planes, shm pools, keymaps) travel with the
//! bytes they came with.

use catostv_core::wlfilter::{ANDROID_DISPLAY, Filter};
use std::io;
use std::net::Shutdown;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

const USAGE: &str = "usage: catostv-wlfilter [--socket PATH] [--upstream PATH]";

fn log(msg: &str) {
    eprintln!("catostv-wlfilter: {msg}");
}

// ---- sendmsg / recvmsg with SCM_RIGHTS (x86_64 Linux, glibc layout) -----

mod ffi {
    use std::ffi::c_void;

    #[repr(C)]
    pub struct iovec {
        pub iov_base: *mut c_void,
        pub iov_len: usize,
    }

    #[repr(C)]
    pub struct msghdr {
        pub msg_name: *mut c_void,
        pub msg_namelen: u32,
        pub msg_iov: *mut iovec,
        pub msg_iovlen: usize,
        pub msg_control: *mut c_void,
        pub msg_controllen: usize,
        pub msg_flags: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct cmsghdr {
        pub cmsg_len: usize,
        pub cmsg_level: i32,
        pub cmsg_type: i32,
    }

    unsafe extern "C" {
        pub fn sendmsg(fd: i32, msg: *const msghdr, flags: i32) -> isize;
        pub fn recvmsg(fd: i32, msg: *mut msghdr, flags: i32) -> isize;
        pub fn close(fd: i32) -> i32;
    }
}

const SOL_SOCKET: i32 = 1;
const SCM_RIGHTS: i32 = 1;
const MSG_CTRUNC: i32 = 0x08;
const MSG_NOSIGNAL: i32 = 0x4000;
const MSG_CMSG_CLOEXEC: i32 = 0x4000_0000;
const CMSG_HDR: usize = std::mem::size_of::<ffi::cmsghdr>();
/// Linux's SCM_MAX_FD; libwayland sends at most 28 per flush.
const MAX_FDS: usize = 253;

const fn cmsg_align(n: usize) -> usize {
    (n + 7) & !7
}

/// One `recvmsg`: bytes read into `buf` and the file descriptors that came
/// with them (close-on-exec). `Ok((0, _))` is end of stream.
fn recv_with_fds(fd: RawFd, buf: &mut [u8]) -> io::Result<(usize, Vec<RawFd>)> {
    let mut iov = ffi::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    // u64 storage keeps the control buffer 8-byte aligned.
    let mut control = [0u64; cmsg_align(CMSG_HDR + MAX_FDS * 4) / 8];
    let mut msg = ffi::msghdr {
        msg_name: std::ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &mut iov,
        msg_iovlen: 1,
        msg_control: control.as_mut_ptr().cast(),
        msg_controllen: std::mem::size_of_val(&control),
        msg_flags: 0,
    };
    let n = loop {
        // SAFETY: `msg` points at a valid header whose iov and control
        // buffers live for the call; the kernel writes within their lengths.
        let r = unsafe { ffi::recvmsg(fd, &mut msg, MSG_CMSG_CLOEXEC) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        break r as usize;
    };
    if msg.msg_flags & MSG_CTRUNC != 0 {
        return Err(io::Error::other(
            "control data truncated: file descriptors lost",
        ));
    }
    let mut fds = Vec::new();
    let base = control.as_ptr().cast::<u8>();
    let used = msg.msg_controllen.min(std::mem::size_of_val(&control));
    let mut off = 0;
    while off + CMSG_HDR <= used {
        // SAFETY: `off + CMSG_HDR <= used <= size of control`; unaligned
        // reads need no alignment.
        let hdr = unsafe { std::ptr::read_unaligned(base.add(off).cast::<ffi::cmsghdr>()) };
        if hdr.cmsg_len < CMSG_HDR || off + hdr.cmsg_len > used {
            break;
        }
        if hdr.cmsg_level == SOL_SOCKET && hdr.cmsg_type == SCM_RIGHTS {
            let count = (hdr.cmsg_len - CMSG_HDR) / 4;
            for i in 0..count {
                // SAFETY: within `cmsg_len`, which is within `used`.
                let v = unsafe {
                    std::ptr::read_unaligned(base.add(off + CMSG_HDR + 4 * i).cast::<i32>())
                };
                fds.push(v);
            }
        }
        off += cmsg_align(hdr.cmsg_len);
    }
    Ok((n, fds))
}

/// Write all of `data`; `fds` ride along with the first bytes (the kernel
/// attaches them there, which is where libwayland expects them). `data`
/// must not be empty when `fds` is not.
fn send_all(fd: RawFd, data: &[u8], fds: &[RawFd]) -> io::Result<()> {
    debug_assert!(!data.is_empty() || fds.is_empty());
    if fds.len() > MAX_FDS {
        return Err(io::Error::other("too many file descriptors in one message"));
    }
    let mut control = [0u64; cmsg_align(CMSG_HDR + MAX_FDS * 4) / 8];
    let mut sent = 0;
    let mut first = true;
    while sent < data.len() {
        let mut iov = ffi::iovec {
            iov_base: data[sent..].as_ptr().cast_mut().cast(),
            iov_len: data.len() - sent,
        };
        let mut msg = ffi::msghdr {
            msg_name: std::ptr::null_mut(),
            msg_namelen: 0,
            msg_iov: &mut iov,
            msg_iovlen: 1,
            msg_control: std::ptr::null_mut(),
            msg_controllen: 0,
            msg_flags: 0,
        };
        if first && !fds.is_empty() {
            let hdr = ffi::cmsghdr {
                cmsg_len: CMSG_HDR + 4 * fds.len(),
                cmsg_level: SOL_SOCKET,
                cmsg_type: SCM_RIGHTS,
            };
            let base = control.as_mut_ptr().cast::<u8>();
            // SAFETY: the control buffer holds CMSG_HDR + 4 * MAX_FDS bytes
            // and `fds.len() <= MAX_FDS`.
            unsafe {
                std::ptr::write_unaligned(base.cast::<ffi::cmsghdr>(), hdr);
                for (i, f) in fds.iter().enumerate() {
                    std::ptr::write_unaligned(base.add(CMSG_HDR + 4 * i).cast::<i32>(), *f);
                }
            }
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = cmsg_align(CMSG_HDR + 4 * fds.len());
        }
        // SAFETY: as in recv_with_fds; the kernel only reads the buffers.
        let r = unsafe { ffi::sendmsg(fd, &msg, MSG_NOSIGNAL) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        sent += r as usize;
        first = false;
    }
    Ok(())
}

fn close_all(fds: &mut Vec<RawFd>) {
    for fd in fds.drain(..) {
        // SAFETY: fds received by recvmsg belong to this process until
        // closed here; each is closed exactly once.
        unsafe {
            ffi::close(fd);
        }
    }
}

// ---- relaying ------------------------------------------------------------

#[derive(Clone, Copy)]
enum Dir {
    ToServer,
    ToClient,
}

/// Relay one direction until the source ends. Returns normally on end of
/// stream, with an error on a socket failure.
fn pump(from: &UnixStream, to: &UnixStream, filter: &Mutex<Filter>, dir: Dir) -> io::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    // Descriptors whose bytes have not gone out yet (a message held back
    // until it is complete, or only dropped messages so far).
    let mut pending: Vec<RawFd> = Vec::new();
    loop {
        let (n, mut fds) = match recv_with_fds(from.as_raw_fd(), &mut buf) {
            Ok(v) => v,
            Err(e) => {
                close_all(&mut pending);
                return Err(e);
            }
        };
        pending.append(&mut fds);
        if n == 0 {
            close_all(&mut pending);
            return Ok(());
        }
        let out = {
            let mut f = filter.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match dir {
                Dir::ToServer => {
                    f.from_client(&buf[..n]);
                    buf[..n].to_vec()
                }
                Dir::ToClient => f.from_server(&buf[..n]),
            }
        };
        if out.is_empty() {
            continue;
        }
        let r = send_all(to.as_raw_fd(), &out, &pending);
        close_all(&mut pending);
        r?;
    }
}

/// One client: connect to the compositor, relay both ways, log the result.
fn serve(client: UnixStream, upstream: &Path, id: u64) {
    let server = match UnixStream::connect(upstream) {
        Ok(s) => s,
        Err(e) => {
            log(&format!(
                "#{id}: cannot reach the compositor at {}: {e}",
                upstream.display()
            ));
            return;
        }
    };
    let client = Arc::new(client);
    let server = Arc::new(server);
    let filter = Arc::new(Mutex::new(Filter::new()));
    let up = {
        let (c, s, f) = (client.clone(), server.clone(), filter.clone());
        thread::Builder::new()
            .name(format!("wlfilter-{id}-up"))
            .spawn(move || {
                let r = pump(&c, &s, &f, Dir::ToServer);
                let _ = c.shutdown(Shutdown::Both);
                let _ = s.shutdown(Shutdown::Both);
                r
            })
    };
    let down = pump(&server, &client, &filter, Dir::ToClient);
    let _ = client.shutdown(Shutdown::Both);
    let _ = server.shutdown(Shutdown::Both);
    let up = match up {
        Ok(h) => h
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("relay thread panicked"))),
        Err(e) => Err(e),
    };
    let dropped = filter.lock().map_or(0, |f| f.dropped);
    match (down, up) {
        (Ok(()), Ok(())) => log(&format!("#{id}: closed ({dropped} announcements hidden)")),
        (Err(e), _) | (_, Err(e)) => log(&format!(
            "#{id}: ended with {e} ({dropped} announcements hidden)"
        )),
    }
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
}

/// Listen on `socket`, relaying every client to `upstream`. Only returns on
/// an accept error.
fn run(socket: &Path, upstream: &Path) -> io::Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(socket) {
        use std::os::unix::fs::FileTypeExt;
        if meta.file_type().is_socket() {
            // A previous instance's socket (the supervisor runs one at a
            // time); a live one would have refused the lock already.
            std::fs::remove_file(socket)?;
        }
    }
    let listener = UnixListener::bind(socket)?;
    log(&format!(
        "listening on {} → {} (hiding {})",
        socket.display(),
        upstream.display(),
        catostv_core::wlfilter::HIDDEN_GLOBALS.join(", ")
    ));
    let mut id = 0u64;
    for client in listener.incoming() {
        let client = client?;
        id += 1;
        log(&format!("#{id}: Android connected"));
        let upstream = upstream.to_path_buf();
        if let Err(e) = thread::Builder::new()
            .name(format!("wlfilter-{id}"))
            .spawn(move || serve(client, &upstream, id))
        {
            log(&format!("#{id}: cannot start a relay thread: {e}"));
        }
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run_dir = runtime_dir();
    let mut socket = run_dir.join(ANDROID_DISPLAY);
    let mut upstream =
        run_dir.join(std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into()));
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--socket" if i + 1 < args.len() => {
                i += 1;
                socket = PathBuf::from(&args[i]);
            }
            "--upstream" if i + 1 < args.len() => {
                i += 1;
                upstream = PathBuf::from(&args[i]);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            other => {
                eprintln!("catostv-wlfilter: unknown argument {other:?}\n{USAGE}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    if socket == upstream {
        eprintln!("catostv-wlfilter: the relay socket and the compositor socket are the same path");
        std::process::exit(2);
    }
    if let Err(e) = run(&socket, &upstream) {
        log(&format!("{}: {e}", socket.display()));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;
    use std::time::Duration;

    fn msg(obj: u32, op: u16, body: &[u8]) -> Vec<u8> {
        let size = (8 + body.len()) as u32;
        let mut v = obj.to_le_bytes().to_vec();
        v.extend_from_slice(&((size << 16) | op as u32).to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    fn global(registry: u32, name: u32, interface: &str) -> Vec<u8> {
        let mut body = name.to_le_bytes().to_vec();
        let len = interface.len() + 1;
        body.extend_from_slice(&(len as u32).to_le_bytes());
        body.extend_from_slice(interface.as_bytes());
        body.push(0);
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        body.extend_from_slice(&1u32.to_le_bytes());
        msg(registry, 0, &body)
    }

    fn read_exact_timeout(s: &mut UnixStream, n: usize) -> Vec<u8> {
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut v = vec![0u8; n];
        s.read_exact(&mut v).expect("relayed bytes");
        v
    }

    /// End to end over real sockets: a fake compositor, the relay, a fake
    /// HAL. The hidden global never reaches the client; a file descriptor
    /// sent with a later message arrives and works.
    #[test]
    fn relays_both_ways_and_hides_the_global() {
        let dir =
            std::env::temp_dir().join(format!("catostv-wlfilter-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let up_path = dir.join("wayland-0");
        let relay_path = dir.join(ANDROID_DISPLAY);
        let compositor = UnixListener::bind(&up_path).unwrap();
        {
            let (relay_path, up_path) = (relay_path.clone(), up_path.clone());
            thread::spawn(move || run(&relay_path, &up_path));
        }
        // Wait for the relay to listen.
        let mut client = None;
        for _ in 0..50 {
            if let Ok(c) = UnixStream::connect(&relay_path) {
                client = Some(c);
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let mut client = client.expect("relay socket");
        let (mut server, _) = compositor.accept().unwrap();

        // HAL → compositor: get_registry(2) arrives verbatim.
        let gr = msg(1, 1, &2u32.to_le_bytes());
        client.write_all(&gr).unwrap();
        assert_eq!(read_exact_timeout(&mut server, gr.len()), gr);

        // compositor → HAL: three globals in one flush; the middle one is hidden.
        let seat = global(2, 7, "wl_seat");
        let idle = global(2, 8, "zwp_idle_inhibit_manager_v1");
        let shm = global(2, 9, "wl_shm");
        let mut flush = seat.clone();
        flush.extend_from_slice(&idle);
        flush.extend_from_slice(&shm);
        server.write_all(&flush).unwrap();
        let mut want = seat.clone();
        want.extend_from_slice(&shm);
        assert_eq!(read_exact_timeout(&mut client, want.len()), want);

        // A message carrying a file descriptor (like wl_keyboard.keymap):
        // the fd must arrive with the bytes and be usable.
        let (pipe_r, mut pipe_w) = {
            let mut fds = [0i32; 2];
            unsafe extern "C" {
                fn pipe(fds: *mut i32) -> i32;
            }
            // SAFETY: `fds` has room for two descriptors.
            assert_eq!(unsafe { pipe(fds.as_mut_ptr()) }, 0);
            // SAFETY: fresh descriptors owned here.
            unsafe {
                (
                    std::fs::File::from_raw_fd(fds[0]),
                    std::fs::File::from_raw_fd(fds[1]),
                )
            }
        };
        let keymap = msg(4, 0, &[1, 0, 0, 0, 7, 0, 0, 0]);
        send_all(server.as_raw_fd(), &keymap, &[pipe_r.as_raw_fd()]).unwrap();
        let mut buf = vec![0u8; 256];
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (n, fds) = recv_with_fds(client.as_raw_fd(), &mut buf).unwrap();
        assert_eq!(&buf[..n], &keymap[..]);
        assert_eq!(fds.len(), 1, "the keymap fd must come through");
        pipe_w.write_all(b"ok").unwrap();
        // SAFETY: the received descriptor is ours to own.
        let mut got = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let mut two = [0u8; 2];
        got.read_exact(&mut two).unwrap();
        assert_eq!(&two, b"ok");
        drop(pipe_r);

        // Closing the compositor side ends the client's stream.
        drop(server);
        let mut rest = Vec::new();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let _ = client.read_to_end(&mut rest);
        assert!(rest.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_and_receive_without_fds() {
        let (a, b) = UnixStream::pair().unwrap();
        send_all(a.as_raw_fd(), b"hello", &[]).unwrap();
        let mut buf = [0u8; 16];
        let (n, fds) = recv_with_fds(b.as_raw_fd(), &mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
        assert!(fds.is_empty());
        drop(a);
        let (n, fds) = recv_with_fds(b.as_raw_fd(), &mut buf).unwrap();
        assert_eq!((n, fds.len()), (0, 0));
    }

    #[test]
    fn cmsg_sizes() {
        assert_eq!(CMSG_HDR, 16);
        assert_eq!(cmsg_align(16 + 4), 24);
        assert_eq!(cmsg_align(16 + 8), 24);
        assert_eq!(std::mem::size_of::<ffi::msghdr>(), 56);
        assert_eq!(std::mem::size_of::<ffi::iovec>(), 16);
    }
}
