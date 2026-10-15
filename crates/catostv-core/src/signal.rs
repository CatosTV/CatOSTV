//! Graceful termination for the LAN daemons without the `libc` crate.
//!
//! [`install_termination_handler`] makes SIGTERM, SIGINT and SIGHUP set a
//! process-wide flag instead of killing the process, so `catostv-castd` can
//! send SSDP `byebye` messages and stop its mpv child before exiting. The
//! handler only stores to an atomic, which is async-signal-safe.
//!
//! Daemons poll [`termination_requested`] from their (timeout-driven) loops.

use std::sync::atomic::{AtomicBool, Ordering};

static TERMINATE: AtomicBool = AtomicBool::new(false);

const SIGHUP: i32 = 1;
const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;
/// `SIG_ERR` is `(sighandler_t) -1`.
const SIG_ERR: usize = usize::MAX;

mod ffi {
    unsafe extern "C" {
        // sighandler_t signal(int signum, sighandler_t handler);
        // glibc's signal() installs the handler with BSD semantics
        // (SA_RESTART, handler stays installed).
        pub fn signal(signum: i32, handler: usize) -> usize;
    }
}

extern "C" fn on_signal(_sig: i32) {
    TERMINATE.store(true, Ordering::SeqCst);
}

/// Route SIGTERM, SIGINT and SIGHUP to the termination flag.
pub fn install_termination_handler() -> std::io::Result<()> {
    let handler = on_signal as extern "C" fn(i32) as usize;
    for sig in [SIGTERM, SIGINT, SIGHUP] {
        // SAFETY: `on_signal` is a valid `extern "C" fn(i32)` that only
        // performs an atomic store (async-signal-safe).
        let prev = unsafe { ffi::signal(sig, handler) };
        if prev == SIG_ERR {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// True once a termination signal arrived (or [`request_termination`] ran).
pub fn termination_requested() -> bool {
    TERMINATE.load(Ordering::SeqCst)
}

/// Set the flag from code (e.g. a fatal error in a worker thread).
pub fn request_termination() {
    TERMINATE.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    mod raise_ffi {
        unsafe extern "C" {
            pub fn raise(sig: i32) -> i32;
        }
    }

    #[test]
    fn sigterm_sets_flag_instead_of_killing() {
        install_termination_handler().unwrap();
        // SAFETY: the handler was installed above, so SIGTERM only sets a flag.
        let r = unsafe { raise_ffi::raise(SIGTERM) };
        assert_eq!(r, 0);
        assert!(termination_requested());
    }
}
