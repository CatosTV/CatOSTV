//! `catostv-cecd`, HDMI-CEC TV remote → keyboard keys.
//!
//! Runs as the appliance user inside the Wayland session (it needs
//! `WAYLAND_DISPLAY` for `wtype`). Logic lives in `catostv_core::cec`; this
//! file only supervises processes. Details: docs/subsystems/CEC.md.
//!
//! ```text
//! catostv-cecd              wait for an adapter (re-check every 30 s), bridge keys,
//!                         restart cec-client with back-off; never exits by itself
//! catostv-cecd --no-wait    exit 0 (one log line) if no adapter is present now
//! catostv-cecd --list       print detected adapters and exit (0 found, 1 none)
//! catostv-cecd --dry-run    print keys instead of injecting them
//! catostv-cecd --osd-name NAME   name shown by the TV (default CatosTV, ≤ 14 chars)
//! ```
//!
//! Never busy-loops: every wait is a blocking read or a sleep, and missing
//! tools (`cec-client`, `wtype`) are logged and retried slowly.

use catostv_core::cec::{self, Adapter, Backoff, KeyRepeater};
use catostv_core::process::{SystemRunner, find_executable};
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const ADAPTER_POLL: Duration = Duration::from_secs(30);
const MISSING_TOOL_POLL: Duration = Duration::from_secs(300);
const WTYPE_WARN_EVERY: Duration = Duration::from_secs(60);

const USAGE: &str = "usage: catostv-cecd [--no-wait] [--list] [--dry-run] [--osd-name NAME]";

mod ffi {
    unsafe extern "C" {
        // prctl(2); variadic in glibc.
        pub fn prctl(option: i32, ...) -> i32;
    }
}
const PR_SET_PDEATHSIG: i32 = 1;
const SIGKILL: std::ffi::c_ulong = 9;

fn log(msg: &str) {
    eprintln!("catostv-cecd: {msg}");
}

struct Options {
    wait: bool,
    dry_run: bool,
    osd_name: String,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut opts = Options {
        wait: true,
        dry_run: false,
        osd_name: cec::DEFAULT_OSD_NAME.to_string(),
    };
    let mut list_only = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-wait" => opts.wait = false,
            "--list" => list_only = true,
            "--dry-run" => opts.dry_run = true,
            "--osd-name" if i + 1 < args.len() => {
                i += 1;
                opts.osd_name = args[i].clone();
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            other => {
                eprintln!("catostv-cecd: unknown argument {other:?}\n{USAGE}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    if !cec::is_valid_osd_name(&opts.osd_name) {
        eprintln!("catostv-cecd: --osd-name must be 1-14 letters, digits, spaces, . _ -");
        std::process::exit(2);
    }

    if list_only {
        match cec::list_adapters(&mut SystemRunner) {
            Ok(a) if !a.is_empty() => {
                for ad in a {
                    println!("{}\t{}", ad.com_port, ad.kind);
                }
            }
            Ok(_) => {
                println!("no CEC adapter found");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("catostv-cecd: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    supervise(&opts);
}

/// Wait for tools and an adapter, run the bridge, restart with back-off.
fn supervise(opts: &Options) {
    let mut backoff = Backoff::new();
    // Log only when the situation changes, so a PC without CEC does not
    // fill the log every 30 s.
    let mut last_state = String::new();
    let note = |last: &mut String, state: String| {
        if *last != state {
            log(&state);
            *last = state;
        }
    };
    loop {
        if find_executable("cec-client").is_none() {
            if !opts.wait {
                log("cec-client is not installed (Arch package libcec); exiting");
                return;
            }
            note(
                &mut last_state,
                "cec-client is not installed (Arch package libcec); checking again every 5 minutes"
                    .into(),
            );
            thread::sleep(MISSING_TOOL_POLL);
            continue;
        }
        let (adapters, problem) = match cec::list_adapters(&mut SystemRunner) {
            Ok(a) => (a, String::from("no CEC adapter found")),
            Err(e) => (Vec::new(), format!("could not list CEC adapters ({e})")),
        };
        let Some(adapter) = adapters.into_iter().next() else {
            if !opts.wait {
                log(&format!("{problem}; exiting"));
                return;
            }
            note(
                &mut last_state,
                format!("{problem}; checking again every 30 s"),
            );
            thread::sleep(ADAPTER_POLL);
            continue;
        };
        note(
            &mut last_state,
            format!("using CEC adapter {} {}", adapter.com_port, adapter.kind),
        );
        let started = Instant::now();
        if let Err(e) = run_bridge(&adapter, opts) {
            log(&e);
        }
        let delay = backoff.next_delay(started.elapsed());
        log(&format!(
            "cec-client stopped; restarting in {} s",
            delay.as_secs()
        ));
        last_state.clear();
        thread::sleep(delay);
    }
}

/// Run one `cec-client` until it exits, injecting keys as they arrive.
fn run_bridge(adapter: &Adapter, opts: &Options) -> Result<(), String> {
    let cmd = cec::client_cmd(adapter, &opts.osd_name).map_err(|e| e.to_string())?;
    let mut command = Command::new(&cmd.program);
    command
        .args(&cmd.args)
        // cec-client's console loop tolerates EOF on stdin (it sets stdin
        // non-blocking and sleeps 50 ms per pass), so no pipe is needed.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    // SAFETY: prctl(PR_SET_PDEATHSIG) is async-signal-safe and touches no
    // memory. If catostv-cecd dies, cec-client is killed (SIGKILL, so libcec
    // does not run its close/standby path) instead of keeping the adapter.
    unsafe {
        command.pre_exec(|| {
            ffi::prctl(PR_SET_PDEATHSIG, SIGKILL);
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start cec-client: {e}"))?;
    let stdout = child.stdout.take().ok_or("cec-client has no stdout")?;

    let (tx, rx) = mpsc::channel::<String>();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let epoch = Instant::now();
    let mut keys = KeyRepeater::new();
    let mut injector = Injector::new(opts.dry_run);
    loop {
        // Block until a line arrives or a held key needs finishing.
        let wait = match keys.deadline() {
            Some(d) => d
                .saturating_sub(epoch.elapsed())
                .max(Duration::from_millis(5)),
            None => Duration::from_secs(3600),
        };
        match rx.recv_timeout(wait) {
            Ok(line) => {
                if let Some(key) = cec::parse_key_line(&line) {
                    for k in keys.on_key(key, epoch.elapsed()) {
                        injector.tap(k);
                    }
                } else if cec::is_notable_log_line(&line) {
                    log(line.trim());
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                for k in keys.on_tick(epoch.elapsed()) {
                    injector.tap(k);
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.kill();
    let status = child.wait();
    let _ = reader.join();
    match status {
        Ok(s) => Err(format!("cec-client exited ({s})")),
        Err(e) => Err(format!("cec-client: {e}")),
    }
}

/// Injects keys on a virtual keyboard (`catostv_core::uinput`; `wtype` when
/// /dev/uinput is not usable), tolerating failures.
struct Injector {
    dry_run: bool,
    keys: catostv_core::uinput::KeySender,
    last_warning: Option<Instant>,
}

impl Injector {
    fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            keys: catostv_core::uinput::KeySender::new("CatosTV TV remote"),
            last_warning: None,
        }
    }

    fn tap(&mut self, keysym: &str) {
        if self.dry_run {
            println!("key {keysym}");
            return;
        }
        let result = self.keys.tap(keysym);
        if let Err(e) = result
            && self
                .last_warning
                .is_none_or(|t| t.elapsed() >= WTYPE_WARN_EVERY)
        {
            log(&format!("could not inject {keysym}: {e}"));
            self.last_warning = Some(Instant::now());
        }
    }
}
