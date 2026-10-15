//! `catostv-castd`, makes the TV a cast target on the LAN.
//!
//! * DLNA/UPnP MediaRenderer (BubbleUPnP, Windows "Cast to device", VLC, …)
//!   playing through mpv;
//! * DIAL server for the YouTube phone app ("cast to TV");
//! * SSDP discovery for both (UDP 1900).
//!
//! One HTTP port serves everything (default 49494, `--port`). See
//! docs/subsystems/CASTING_AND_REMOTE.md.
//!
//! Usage: catostv-castd [--name <friendly name>] [--port N] [--no-dial] [--no-dlna]

use catostv_core::{compositor, dial, dlna, env, http, mpv, process::SystemRunner, signal, ssdp};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default HTTP port (fixed so a firewall rule can name it).
const DEFAULT_PORT: u16 = 49494;

static STOP: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
struct Args {
    name: Option<String>,
    port: u16,
    dial: bool,
    dlna: bool,
}

fn usage() -> &'static str {
    "usage: catostv-castd [--name <friendly name>] [--port N] [--no-dial] [--no-dlna]\n\
     \n\
     Makes this TV a cast target on the local network:\n\
       DLNA media renderer (phones/PCs push video, music, photos; plays in mpv)\n\
       DIAL server for the YouTube app (\"cast to TV\")\n\
     \n\
       --name NAME  name shown on phones (default: CatosTV or \"CatosTV (<hostname>)\")\n\
       --port N     HTTP port for descriptions/control (default 49494)\n\
       --no-dial    do not offer YouTube casting\n\
       --no-dlna    do not offer the DLNA media renderer\n"
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args {
        name: None,
        port: DEFAULT_PORT,
        dial: true,
        dlna: true,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--name" => {
                let n = it.next().ok_or("--name needs a value")?.trim().to_string();
                if n.is_empty() || n.chars().count() > 64 || n.chars().any(char::is_control) {
                    return Err("--name must be 1-64 printable characters".into());
                }
                a.name = Some(n);
            }
            "--port" => {
                a.port = it
                    .next()
                    .and_then(|p| p.parse::<u16>().ok())
                    .filter(|p| *p > 0)
                    .ok_or("--port needs a number between 1 and 65535")?;
            }
            "--no-dial" => a.dial = false,
            "--no-dlna" => a.dlna = false,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if !a.dial && !a.dlna {
        return Err("--no-dial and --no-dlna together leave nothing to run".into());
    }
    Ok(a)
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&raw) {
        Ok(a) => a,
        Err(e) if e.is_empty() => {
            print!("{}", usage());
            return;
        }
        Err(e) => {
            eprintln!("catostv-castd: {e}\n\n{}", usage());
            std::process::exit(2);
        }
    };
    if let Err(e) = signal::install_termination_handler() {
        eprintln!("catostv-castd: cannot handle SIGTERM ({e}); byebye will be skipped");
    }

    let uuid = dlna::load_or_create_uuid(&dlna::uuid_path()).unwrap_or_else(|e| {
        eprintln!("catostv-castd: cannot persist the device id ({e}); using a temporary one");
        http::uuid_v4().unwrap_or_else(|_| "00000000-0000-4000-8000-000000000000".into())
    });
    let name = args
        .name
        .clone()
        .unwrap_or_else(dlna::default_friendly_name);

    let listener = match TcpListener::bind(("0.0.0.0", args.port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "catostv-castd: cannot listen on TCP port {}: {e}",
                args.port
            );
            std::process::exit(1);
        }
    };

    let state = env::state_dir();
    let dlna_ctx = args.dlna.then(|| {
        let player = mpv::Mpv::new(mpv::socket_path(), Some(state.join("cast-mpv.log")));
        Arc::new(dlna::Dlna::new(
            name.clone(),
            uuid.clone(),
            dlna::Renderer::new(Box::new(player), Box::new(SystemRunner)),
        ))
    });
    let dial_ctx = if args.dial {
        match dial::FirefoxYouTube::new(Some(state.join("cast-youtube.log"))) {
            Ok(app) => Some(Arc::new(dial::Dial::new(
                name.clone(),
                dial::dial_uuid(&uuid),
                args.port,
                Box::new(app),
            ))),
            Err(e) => {
                eprintln!("catostv-castd: YouTube casting disabled: {e}");
                None
            }
        }
    } else {
        None
    };
    if dlna_ctx.is_none() && dial_ctx.is_none() {
        eprintln!("catostv-castd: nothing to serve");
        std::process::exit(1);
    }

    let mut adverts = Vec::new();
    if let Some(d) = &dlna_ctx {
        adverts.push(dlna::advertisement(&d.uuid));
    }
    if let Some(d) = &dial_ctx {
        adverts.push(dial::advertisement(&d.uuid));
    }

    let (h_dlna, h_dial) = (dlna_ctx.clone(), dial_ctx.clone());
    let handler: Arc<http::Handler> = Arc::new(move |req: &http::Request| {
        if let Some(d) = &h_dlna
            && let Some(resp) = dlna::handle_http(req, d)
        {
            return resp;
        }
        if let Some(d) = &h_dial
            && let Some(resp) = dial::handle_http(req, d)
        {
            return resp;
        }
        http::Response::status_only(404)
    });
    if let Err(e) = http::spawn_server(listener, http::Limits::default(), handler) {
        eprintln!("catostv-castd: cannot start the HTTP server: {e}");
        std::process::exit(1);
    }

    let boot_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(1);
    let cfg = ssdp::Config {
        adverts,
        http_port: args.port,
        boot_id,
    };
    let ssdp_thread = thread::Builder::new()
        .name("catostv-ssdp".into())
        .spawn(move || {
            let mut runner = SystemRunner;
            while !STOP.load(Ordering::SeqCst) {
                match ssdp::run(cfg.clone(), &mut runner, &STOP) {
                    Ok(()) => break,
                    Err(e) => {
                        eprintln!("catostv-castd: SSDP unavailable ({e}); retrying in 10 s");
                        for _ in 0..50 {
                            if STOP.load(Ordering::SeqCst) {
                                break;
                            }
                            thread::sleep(Duration::from_millis(200));
                        }
                    }
                }
            }
        })
        .ok();

    if let Some(d) = dlna_ctx.clone() {
        let _ = thread::Builder::new()
            .name("catostv-gena".into())
            .spawn(move || {
                let appliance = env::is_appliance();
                while !STOP.load(Ordering::SeqCst) {
                    // When the cast window closes (end of media or Stop),
                    // show the CatosTV desktop instead of an empty App desktop.
                    let ended = d
                        .renderer
                        .lock()
                        .map(|mut r| {
                            r.refresh();
                            r.take_ended()
                        })
                        .unwrap_or(false);
                    if ended && appliance {
                        let _ = compositor::show_shell_workspace(&mut SystemRunner, true);
                    }
                    dlna::pump_events(&d);
                    thread::sleep(Duration::from_secs(1));
                }
            });
    }

    eprintln!(
        "catostv-castd: \"{name}\" on TCP port {} (DLNA renderer: {}, YouTube DIAL: {})",
        args.port,
        if dlna_ctx.is_some() { "on" } else { "off" },
        if dial_ctx.is_some() { "on" } else { "off" },
    );

    while !signal::termination_requested() {
        thread::sleep(Duration::from_millis(200));
    }
    eprintln!("catostv-castd: stopping");
    STOP.store(true, Ordering::SeqCst);
    if let Some(t) = ssdp_thread {
        let _ = t.join(); // sends ssdp:byebye
    }
    if let Some(d) = &dlna_ctx
        && let Ok(mut r) = d.renderer.lock()
    {
        r.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Args, String> {
        parse_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn args() {
        let a = p(&[]).unwrap();
        assert_eq!(a.port, DEFAULT_PORT);
        assert!(a.dial && a.dlna && a.name.is_none());
        let a = p(&["--name", "Living room", "--port", "5000", "--no-dial"]).unwrap();
        assert_eq!(a.name.as_deref(), Some("Living room"));
        assert_eq!(a.port, 5000);
        assert!(!a.dial && a.dlna);
        assert!(p(&["--no-dial", "--no-dlna"]).is_err());
        assert!(p(&["--port", "0"]).is_err());
        assert!(p(&["--port", "x"]).is_err());
        assert!(p(&["--name"]).is_err());
        assert!(p(&["--name", ""]).is_err());
        assert!(p(&["--bogus"]).is_err());
        assert_eq!(p(&["--help"]).unwrap_err(), "");
    }
}
