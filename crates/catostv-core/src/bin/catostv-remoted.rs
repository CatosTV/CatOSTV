//! `catostv-remoted`, phone remote web page on the LAN (default port 8099).
//!
//! Generates a 6-digit PIN at start and writes it to
//! `$XDG_RUNTIME_DIR/catostv-remote.pin` (0600) for the TV to display. Phones
//! open `http://<tv-ip>:8099`, enter the PIN once and get a D-pad, OK, Back,
//! Home, Control Center, volume, play/pause and a keyboard. Keys are sent
//! with `wtype`. See docs/subsystems/CASTING_AND_REMOTE.md.
//!
//! Usage: catostv-remoted [--port N]

use catostv_core::{http, process::SystemRunner, remote, signal};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

fn parse_port(args: &[String]) -> Result<u16, String> {
    let mut port = remote::PORT;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" => {
                port = it
                    .next()
                    .and_then(|p| p.parse::<u16>().ok())
                    .filter(|p| *p > 0)
                    .ok_or("--port needs a number between 1 and 65535")?;
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(port)
}

const USAGE: &str = "usage: catostv-remoted [--port N]\n\
\n\
Serves the CatosTV phone remote on http://<tv-ip>:8099 (default port).\n\
The pairing PIN is written to $XDG_RUNTIME_DIR/catostv-remote.pin.\n";

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let port = match parse_port(&raw) {
        Ok(p) => p,
        Err(e) if e.is_empty() => {
            print!("{USAGE}");
            return;
        }
        Err(e) => {
            eprintln!("catostv-remoted: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = signal::install_termination_handler() {
        eprintln!("catostv-remoted: cannot handle SIGTERM ({e})");
    }
    let pin = match remote::generate_pin() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("catostv-remoted: no randomness for the PIN: {e}");
            std::process::exit(1);
        }
    };
    let pin_file = remote::pin_path();
    if let Err(e) = remote::write_pin(&pin_file, &pin) {
        eprintln!(
            "catostv-remoted: cannot write the PIN to {}: {e}",
            pin_file.display()
        );
        std::process::exit(1);
    }
    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("catostv-remoted: cannot listen on TCP port {port}: {e}");
            let _ = std::fs::remove_file(&pin_file);
            std::process::exit(1);
        }
    };
    let state = Arc::new(remote::Remote::new(pin, Box::new(SystemRunner)).with_virtual_keyboard());
    let handler: Arc<http::Handler> =
        Arc::new(move |req: &http::Request| remote::handle_http(req, &state));
    if let Err(e) = http::spawn_server(listener, http::Limits::default(), handler) {
        eprintln!("catostv-remoted: cannot start the HTTP server: {e}");
        std::process::exit(1);
    }
    match remote::local_ipv4() {
        Some(ip) => eprintln!(
            "catostv-remoted: phone remote at http://{ip}:{port} (PIN in {})",
            pin_file.display()
        ),
        None => eprintln!("catostv-remoted: listening on port {port} (no network yet)"),
    }
    while !signal::termination_requested() {
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = std::fs::remove_file(&pin_file);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_flag() {
        let p = |a: &[&str]| parse_port(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(p(&[]).unwrap(), 8099);
        assert_eq!(p(&["--port", "9000"]).unwrap(), 9000);
        assert!(p(&["--port", "0"]).is_err());
        assert!(p(&["--nope"]).is_err());
        assert_eq!(p(&["--help"]).unwrap_err(), "");
    }
}
