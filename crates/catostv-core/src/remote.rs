//! Phone remote: a web page served on the LAN (port [`PORT`], 8099) that
//! works in any phone browser on the same Wi-Fi, no app install.
//!
//! * `GET /`, one self-contained page (inline CSS/JS, no external
//!   resources): D-pad, OK, Back, Home, Control Center, volume −/+, mute,
//!   play/pause, backspace and a text box.
//! * Pairing: `catostv-remoted` generates a random 6-digit PIN at start
//!   (from `/dev/urandom`) and writes it to [`pin_path`] (mode 0600) so the
//!   TV shell can show it next to [`url_hint`]. The page asks for the PIN
//!   once (`POST /api/pair`) and stores the returned 128-bit session token
//!   in `localStorage`; every action sends it in the `X-CatosTV-Token` header.
//!   Wrong PINs are rate limited: 5 per minute per IP and 20 per minute in
//!   total. Tokens live in memory, so restarting the daemon (new PIN)
//!   unpairs every phone.
//! * Actions: `POST /api/key` (`key=<name>` from a strict whitelist, sent as
//!   `wtype -k <keysym>`) and `POST /api/text` (`text=…`, at most 200
//!   characters, control characters removed, sent as `wtype -- <text>`).
//!   labwc matches key bindings by keysym for virtual keyboards, so Home
//!   (`XF86HomePage`) and Control Center (`F10`) trigger CatosTV's global
//!   bindings exactly like a real remote.
//! * Hardening: requests whose `Host` is not an IP literal are refused
//!   (blocks DNS rebinding), cross-origin POSTs are refused, the token is a
//!   custom header (no CORS is ever granted), responses carry a strict
//!   Content-Security-Policy.
//!
//! Runtime status: page, pairing and key mapping are unit-tested; typing
//! into real apps with `wtype` is UNVERIFIED on the appliance.

use crate::http::{self, Request, Response};
use crate::process::{Cmd, Runner};
use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// TCP port of the phone remote.
pub const PORT: u16 = 8099;
/// Header that carries the session token.
pub const TOKEN_HEADER: &str = "X-CatosTV-Token";
pub const MAX_TEXT_CHARS: usize = 200;
pub const MAX_SESSIONS: usize = 16;
/// Wrong PINs allowed per client IP per [`PIN_WINDOW`].
pub const PIN_ATTEMPTS_PER_IP: usize = 5;
/// Wrong PINs allowed from everyone together per [`PIN_WINDOW`].
pub const PIN_ATTEMPTS_TOTAL: usize = 20;
pub const PIN_WINDOW: Duration = Duration::from_secs(60);

/// Remote key names accepted by `/api/key` and the keysym `wtype` sends.
pub const KEYS: &[(&str, &str)] = &[
    ("up", "Up"),
    ("down", "Down"),
    ("left", "Left"),
    ("right", "Right"),
    ("ok", "Return"),
    ("back", "Escape"),
    ("home", "XF86HomePage"),
    ("menu", "F10"),
    ("volup", "XF86AudioRaiseVolume"),
    ("voldown", "XF86AudioLowerVolume"),
    ("mute", "XF86AudioMute"),
    ("playpause", "XF86AudioPlay"),
    ("backspace", "BackSpace"),
];

/// Keysym for a remote key name (strict whitelist).
pub fn keysym(name: &str) -> Option<&'static str> {
    KEYS.iter().find(|(k, _)| *k == name).map(|(_, s)| *s)
}

/// `$XDG_RUNTIME_DIR/catostv-remote.pin` (fallback
/// `/tmp/catostv-remote-<uid>.pin`).
pub fn pin_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("catostv-remote.pin"),
        None => PathBuf::from(format!(
            "/tmp/catostv-remote-{}.pin",
            crate::mpv::current_uid()
        )),
    }
}

pub fn is_valid_pin(s: &str) -> bool {
    s.len() == 6 && s.bytes().all(|b| b.is_ascii_digit())
}

/// The current PIN for the TV shell to display (`None` if the remote daemon
/// is not running or the file is malformed).
pub fn read_pin() -> Option<String> {
    read_pin_from(&pin_path())
}

pub fn read_pin_from(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let pin = text.trim();
    is_valid_pin(pin).then(|| pin.to_string())
}

/// A uniformly random 6-digit PIN (rejection sampling, no modulo bias).
pub fn generate_pin() -> io::Result<String> {
    loop {
        let b = http::random_bytes(4)?;
        let n = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        // Largest multiple of 1_000_000 that fits in u32.
        if n < 4_294_000_000 {
            return Ok(format!("{:06}", n % 1_000_000));
        }
    }
}

/// Write the PIN with mode 0600 (atomically via rename).
pub fn write_pin(path: &Path, pin: &str) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("pin.tmp");
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(format!("{pin}\n").as_bytes())?;
    }
    std::fs::rename(&tmp, path)
}

/// The LAN address phones should use: the source address of the default
/// route (UDP "connect" to 1.1.1.1:80, nothing is sent).
pub fn local_ipv4() -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    sock.connect((Ipv4Addr::new(1, 1, 1, 1), 80)).ok()?;
    match sock.local_addr().ok()? {
        SocketAddr::V4(a) if !a.ip().is_unspecified() && !a.ip().is_loopback() => Some(*a.ip()),
        _ => None,
    }
}

/// `http://<ip>:8099` for the TV to display.
pub fn url_hint(ip: Ipv4Addr) -> String {
    format!("http://{ip}:{PORT}")
}

/// Remove control characters and limit to [`MAX_TEXT_CHARS`].
pub fn sanitize_text(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT_CHARS)
        .collect()
}

pub fn key_cmd(keysym: &str) -> Cmd {
    Cmd::new("wtype").args(["-k", keysym])
}

pub fn text_cmd(text: &str) -> Cmd {
    Cmd::new("wtype").args(["--", text])
}

/// Sliding-window limiter for wrong PINs.
#[derive(Debug, Default)]
pub struct PinLimiter {
    per_ip: HashMap<Ipv4Addr, Vec<Instant>>,
    total: Vec<Instant>,
}

impl PinLimiter {
    fn prune(&mut self, now: Instant) {
        let fresh = |t: &Instant| now.duration_since(*t) < PIN_WINDOW;
        self.total.retain(fresh);
        self.per_ip.retain(|_, v| {
            v.retain(fresh);
            !v.is_empty()
        });
    }

    /// May this client try a PIN now?
    pub fn allowed(&mut self, ip: Ipv4Addr, now: Instant) -> bool {
        self.prune(now);
        self.total.len() < PIN_ATTEMPTS_TOTAL
            && self.per_ip.get(&ip).map_or(0, Vec::len) < PIN_ATTEMPTS_PER_IP
    }

    pub fn record_failure(&mut self, ip: Ipv4Addr, now: Instant) {
        self.total.push(now);
        self.per_ip.entry(ip).or_default().push(now);
    }
}

/// Remote daemon state.
pub struct Remote {
    pin: String,
    sessions: Mutex<Vec<(String, Instant)>>,
    limiter: Mutex<PinLimiter>,
    runner: Mutex<Box<dyn Runner + Send>>,
    /// Real key presses (uinput, see `crate::uinput`); `None` = through
    /// the runner with `wtype` (tests, and the fallback).
    keys: Option<Mutex<crate::uinput::KeySender>>,
}

impl Remote {
    /// Press keys on a virtual keyboard (so Android apps get them too).
    pub fn with_virtual_keyboard(mut self) -> Self {
        self.keys = Some(Mutex::new(crate::uinput::KeySender::new(
            "CatosTV phone remote",
        )));
        self
    }

    fn press(&self, keysym: &str) -> bool {
        match &self.keys {
            Some(k) => k.lock().is_ok_and(|mut k| k.tap(keysym).is_ok()),
            None => self.run(&key_cmd(keysym)),
        }
    }

    pub fn new(pin: String, runner: Box<dyn Runner + Send>) -> Self {
        Self {
            pin,
            sessions: Mutex::new(Vec::new()),
            limiter: Mutex::new(PinLimiter::default()),
            runner: Mutex::new(runner),
            keys: None,
        }
    }

    fn new_session(&self) -> Option<String> {
        let token = http::random_hex(16).ok()?;
        let mut s = self.sessions.lock().ok()?;
        if s.len() >= MAX_SESSIONS {
            // Drop the least recently used session.
            if let Some(i) = s
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(i, _)| i)
            {
                s.remove(i);
            }
        }
        s.push((token.clone(), Instant::now()));
        Some(token)
    }

    fn authorised(&self, req: &Request) -> bool {
        let Some(tok) = req.header(TOKEN_HEADER).map(str::trim) else {
            return false;
        };
        if tok.len() != 32 {
            return false;
        }
        let Ok(mut s) = self.sessions.lock() else {
            return false;
        };
        let mut found = false;
        for (t, used) in s.iter_mut() {
            if http::constant_time_eq(t.as_bytes(), tok.as_bytes()) {
                *used = Instant::now();
                found = true;
            }
        }
        found
    }

    fn run(&self, cmd: &Cmd) -> bool {
        match self.runner.lock() {
            Ok(mut r) => r.run_ok(cmd).is_ok(),
            Err(_) => false,
        }
    }
}

fn json_msg(status: u16, key: &str, value: &str) -> Response {
    Response::json(
        status,
        format!("{{\"{key}\":{}}}", crate::mpv::json_string(value)),
    )
}

fn secure(resp: Response) -> Response {
    resp.header(
        "Content-Security-Policy",
        "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; \
         connect-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
    )
    .header("X-Content-Type-Options", "nosniff")
    .header("Referrer-Policy", "no-referrer")
    .header("Cache-Control", "no-store")
}

/// Same-origin check for POSTs: an `Origin` header, if any, must be
/// `http://<Host>`.
fn same_origin(req: &Request) -> bool {
    match (req.header("origin"), req.header("host")) {
        (None, _) => true,
        (Some(o), Some(h)) => o.trim() == format!("http://{}", h.trim()),
        (Some(_), None) => false,
    }
}

/// Handle one request.
pub fn handle_http(req: &Request, remote: &Remote) -> Response {
    if let Some(host) = req.header("host")
        && !http::host_is_ip_literal(host)
    {
        return secure(Response::status_only(421));
    }
    let post = req.method == "POST";
    if post && !same_origin(req) {
        return secure(Response::status_only(403));
    }
    let resp = match (req.method.as_str(), req.path.as_str()) {
        ("GET" | "HEAD", "/") => Response::html(200, PAGE_HTML),
        ("GET" | "HEAD", "/favicon.ico") => Response::new(204),
        ("POST", "/api/pair") => pair(req, remote),
        ("GET", "/api/check") => {
            if remote.authorised(req) {
                json_msg(200, "ok", "paired")
            } else {
                json_msg(401, "error", "not paired")
            }
        }
        ("POST", "/api/key") => {
            if !remote.authorised(req) {
                json_msg(401, "error", "not paired")
            } else {
                let pairs = req.form_pairs();
                match http::form_value(&pairs, "key").and_then(keysym) {
                    None => json_msg(400, "error", "unknown key"),
                    Some(sym) if remote.press(sym) => json_msg(200, "ok", sym),
                    Some(_) => json_msg(500, "error", "the TV could not press the key"),
                }
            }
        }
        ("POST", "/api/text") => {
            if !remote.authorised(req) {
                json_msg(401, "error", "not paired")
            } else {
                let pairs = req.form_pairs();
                let text = sanitize_text(http::form_value(&pairs, "text").unwrap_or(""));
                if text.is_empty() {
                    json_msg(400, "error", "nothing to type")
                } else if remote.run(&text_cmd(&text)) {
                    json_msg(200, "ok", "typed")
                } else {
                    json_msg(500, "error", "the TV could not type the text")
                }
            }
        }
        (_, "/" | "/favicon.ico" | "/api/pair" | "/api/check" | "/api/key" | "/api/text") => {
            Response::status_only(405)
        }
        _ => Response::status_only(404),
    };
    secure(resp)
}

fn pair(req: &Request, remote: &Remote) -> Response {
    let ip = req.peer_ipv4().unwrap_or(Ipv4Addr::UNSPECIFIED);
    let now = Instant::now();
    let Ok(mut limiter) = remote.limiter.lock() else {
        return Response::status_only(500);
    };
    if !limiter.allowed(ip, now) {
        return json_msg(429, "error", "too many wrong PINs, wait a minute")
            .header("Retry-After", "60");
    }
    let pairs = req.form_pairs();
    let pin = http::form_value(&pairs, "pin").unwrap_or("").trim();
    if is_valid_pin(pin) && http::constant_time_eq(pin.as_bytes(), remote.pin.as_bytes()) {
        drop(limiter);
        match remote.new_session() {
            Some(token) => json_msg(200, "token", &token),
            None => json_msg(500, "error", "could not create a session"),
        }
    } else {
        limiter.record_failure(ip, now);
        json_msg(403, "error", "wrong PIN")
    }
}

/// The phone page. Static: nothing from requests is ever inserted.
pub const PAGE_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover, user-scalable=no">
<meta name="theme-color" content="#0d1020">
<title>CatosTV Remote</title>
<style>
:root{--bg:#0d1020;--glass:rgba(255,255,255,.08);--glass2:rgba(255,255,255,.14);--line:rgba(255,255,255,.16);--text:#f4efe6;--dim:rgba(244,239,230,.62);--accent:#f2c879;--ok:#f4efe6}
*{box-sizing:border-box;-webkit-tap-highlight-color:transparent}
html,body{margin:0;height:100%}
body{background:radial-gradient(120% 80% at 10% 0%,#2b2550 0%,transparent 60%),radial-gradient(90% 70% at 100% 100%,#123a4a 0%,transparent 55%),var(--bg);color:var(--text);font:16px/1.4 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;display:flex;justify-content:center;user-select:none;-webkit-user-select:none;touch-action:manipulation}
main{width:100%;max-width:420px;padding:max(18px,env(safe-area-inset-top)) 18px max(18px,env(safe-area-inset-bottom));display:flex;flex-direction:column;gap:18px}
header{display:flex;align-items:center;justify-content:space-between}
h1{font-size:20px;margin:0;letter-spacing:.3px;font-weight:700}
h1 span{color:var(--accent)}
#status{font-size:13px;color:var(--dim)}
.card{background:var(--glass);border:1px solid var(--line);border-radius:28px;padding:18px;backdrop-filter:blur(24px);-webkit-backdrop-filter:blur(24px);box-shadow:0 10px 40px rgba(0,0,0,.35)}
button{font:inherit;color:var(--text);background:var(--glass2);border:1px solid var(--line);border-radius:18px;min-height:56px;cursor:pointer;display:flex;align-items:center;justify-content:center;gap:8px;transition:transform .08s,background .15s}
button:active,button.down{transform:scale(.94);background:rgba(255,255,255,.26)}
button svg{width:26px;height:26px;fill:none;stroke:currentColor;stroke-width:2.2;stroke-linecap:round;stroke-linejoin:round}
.pad{position:relative;width:100%;aspect-ratio:1;max-width:300px;margin:0 auto;border-radius:50%;background:var(--glass);border:1px solid var(--line)}
.pad button{position:absolute;border-radius:50%;background:transparent;border:none;min-height:0}
.pad .u{top:2%;left:33%;width:34%;height:30%}.pad .d{bottom:2%;left:33%;width:34%;height:30%}
.pad .l{left:2%;top:33%;width:30%;height:34%}.pad .r{right:2%;top:33%;width:30%;height:34%}
.pad .ok{top:31%;left:31%;width:38%;height:38%;background:var(--ok);color:#1a1830;font-weight:800;font-size:20px;border:none;box-shadow:0 6px 24px rgba(0,0,0,.35)}
.pad .ok:active,.pad .ok.down{background:#fff}
.row{display:grid;grid-template-columns:repeat(3,1fr);gap:12px}
.row4{grid-template-columns:repeat(4,1fr)}
small{display:block;font-size:12px;color:var(--dim);text-align:center;margin-top:2px}
form{display:flex;gap:10px}
input{flex:1;min-width:0;font:inherit;color:var(--text);background:rgba(0,0,0,.25);border:1px solid var(--line);border-radius:16px;padding:14px 16px;outline:none;user-select:text;-webkit-user-select:text}
input:focus{border-color:var(--accent)}
form button{padding:0 18px}
#pair{display:none;flex-direction:column;gap:14px;text-align:center}
#pair input{text-align:center;font-size:28px;letter-spacing:10px}
#pair p{margin:0;color:var(--dim)}
.err{color:#ff9d8a;min-height:20px;font-size:14px}
body.unpaired #remote{display:none}body.unpaired #pair{display:flex}
</style>
</head>
<body class="unpaired">
<main>
<header><h1>Catos<span>TV</span> Remote</h1><div id="status"></div></header>

<section id="pair" class="card">
<p>Enter the 6-digit PIN shown on the TV</p>
<form id="pairform" autocomplete="off"><input id="pin" inputmode="numeric" pattern="[0-9]*" maxlength="6" placeholder="000000" aria-label="PIN"><button type="submit">Pair</button></form>
<div class="err" id="pairerr"></div>
</section>

<div id="remote" style="display:flex;flex-direction:column;gap:18px">
<section class="card">
<div class="pad">
<button class="u" data-key="up" data-repeat aria-label="Up"><svg viewBox="0 0 24 24"><path d="M6 15l6-6 6 6"/></svg></button>
<button class="l" data-key="left" data-repeat aria-label="Left"><svg viewBox="0 0 24 24"><path d="M15 6l-6 6 6 6"/></svg></button>
<button class="ok" data-key="ok" aria-label="OK">OK</button>
<button class="r" data-key="right" data-repeat aria-label="Right"><svg viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg></button>
<button class="d" data-key="down" data-repeat aria-label="Down"><svg viewBox="0 0 24 24"><path d="M6 9l6 6 6-6"/></svg></button>
</div>
</section>

<section class="card">
<div class="row">
<button data-key="back" aria-label="Back"><svg viewBox="0 0 24 24"><path d="M9 14L4 9l5-5"/><path d="M4 9h11a5 5 0 010 10h-3"/></svg></button>
<button data-key="home" aria-label="Home"><svg viewBox="0 0 24 24"><path d="M3 11l9-7 9 7"/><path d="M5 10v10h14V10"/></svg></button>
<button data-key="menu" aria-label="Control Center"><svg viewBox="0 0 24 24"><path d="M4 7h10M18 7h2M4 17h2M10 17h10"/><circle cx="16" cy="7" r="2"/><circle cx="8" cy="17" r="2"/></svg></button>
</div>
<div class="row" style="grid-template-columns:repeat(3,1fr);margin-top:6px"><small>Back</small><small>Home</small><small>Control</small></div>
</section>

<section class="card">
<div class="row row4">
<button data-key="voldown" data-repeat aria-label="Volume down"><svg viewBox="0 0 24 24"><path d="M5 12h14"/></svg></button>
<button data-key="mute" aria-label="Mute"><svg viewBox="0 0 24 24"><path d="M4 9h4l5-4v14l-5-4H4z"/><path d="M17 9l4 6M21 9l-4 6"/></svg></button>
<button data-key="playpause" aria-label="Play or pause"><svg viewBox="0 0 24 24"><path d="M6 5v14l7-7z"/><path d="M16 6v12M20 6v12"/></svg></button>
<button data-key="volup" data-repeat aria-label="Volume up"><svg viewBox="0 0 24 24"><path d="M5 12h14M12 5v14"/></svg></button>
</div>
</section>

<section class="card">
<form id="textform" autocomplete="off">
<input id="text" maxlength="200" placeholder="Type on the TV" aria-label="Text to type" enterkeyhint="send">
<button type="submit" aria-label="Send">Send</button>
<button type="button" data-key="backspace" aria-label="Backspace"><svg viewBox="0 0 24 24"><path d="M21 5H9l-6 7 6 7h12z"/><path d="M12 10l4 4M16 10l-4 4"/></svg></button>
</form>
</section>
</div>
</main>
<script>
"use strict";
const TOKEN_KEY = "catostv-remote-token";
let token = null;
try { token = localStorage.getItem(TOKEN_KEY); } catch (e) {}
const statusEl = document.getElementById("status");

function setPaired(on) {
  document.body.classList.toggle("unpaired", !on);
  statusEl.textContent = on ? "Connected" : "Not paired";
}
async function api(path, data) {
  const opts = { method: data ? "POST" : "GET", headers: {} };
  if (token) opts.headers["X-CatosTV-Token"] = token;
  if (data) {
    opts.headers["Content-Type"] = "application/x-www-form-urlencoded";
    opts.body = new URLSearchParams(data).toString();
  }
  let res;
  try { res = await fetch(path, opts); } catch (e) { statusEl.textContent = "TV not reachable"; return null; }
  let body = {};
  try { body = await res.json(); } catch (e) {}
  if (res.status === 401) { setPaired(false); }
  else if (res.ok && path !== "/api/pair") { statusEl.textContent = "Connected"; }
  return { status: res.status, body };
}
function press(key) {
  if (navigator.vibrate) { try { navigator.vibrate(12); } catch (e) {} }
  api("/api/key", { key });
}
document.querySelectorAll("button[data-key]").forEach((b) => {
  let delay = null, rep = null;
  const stop = () => { clearTimeout(delay); clearInterval(rep); delay = rep = null; b.classList.remove("down"); };
  b.addEventListener("pointerdown", (ev) => {
    ev.preventDefault();
    b.classList.add("down");
    press(b.dataset.key);
    if (b.hasAttribute("data-repeat")) {
      delay = setTimeout(() => { rep = setInterval(() => press(b.dataset.key), 140); }, 450);
    }
  });
  ["pointerup", "pointerleave", "pointercancel"].forEach((t) => b.addEventListener(t, stop));
  b.addEventListener("contextmenu", (ev) => ev.preventDefault());
});
document.getElementById("textform").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const input = document.getElementById("text");
  const text = input.value;
  if (!text) { press("ok"); return; }
  const r = await api("/api/text", { text });
  if (r && r.status === 200) input.value = "";
});
document.getElementById("pairform").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const err = document.getElementById("pairerr");
  const pin = document.getElementById("pin").value.trim();
  err.textContent = "";
  const r = await api("/api/pair", { pin });
  if (r && r.status === 200 && r.body.token) {
    token = r.body.token;
    try { localStorage.setItem(TOKEN_KEY, token); } catch (e) {}
    setPaired(true);
  } else if (r) {
    err.textContent = (r.body && r.body.error) || "Pairing failed";
  }
});
(async () => {
  if (!token) { setPaired(false); return; }
  const r = await api("/api/check");
  setPaired(!!(r && r.status === 200));
})();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Output, fake::FakeRunner};
    use std::sync::Arc;

    struct Shared(Arc<Mutex<FakeRunner>>);
    impl Runner for Shared {
        fn run(&mut self, cmd: &Cmd) -> crate::error::Result<Output> {
            self.0.lock().unwrap().run(cmd)
        }
    }

    fn remote() -> (Remote, Arc<Mutex<FakeRunner>>) {
        let r = Arc::new(Mutex::new(FakeRunner::new()));
        (Remote::new("123456".into(), Box::new(Shared(r.clone()))), r)
    }

    fn req(method: &str, path: &str, headers: &[(&str, &str)], body: &str, peer: &str) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
            peer: Some(peer.parse().unwrap()),
            local: Some("192.168.1.2:8099".parse().unwrap()),
            ..Request::default()
        }
    }

    const PHONE: &str = "192.168.1.50:40000";

    fn body(r: &Response) -> String {
        String::from_utf8_lossy(&r.body).into_owned()
    }

    fn pair_ok(remote: &Remote) -> String {
        let r = handle_http(&req("POST", "/api/pair", &[], "pin=123456", PHONE), remote);
        assert_eq!(r.status, 200, "{}", body(&r));
        let v = crate::mpv::parse_json(&body(&r)).unwrap();
        v.get("token").unwrap().as_str().unwrap().to_string()
    }

    #[test]
    fn pin_helpers() {
        for _ in 0..50 {
            let p = generate_pin().unwrap();
            assert!(is_valid_pin(&p), "{p}");
        }
        assert!(!is_valid_pin("12345"));
        assert!(!is_valid_pin("12345a"));
        assert!(!is_valid_pin("1234567"));
        let dir = std::env::temp_dir().join(format!("catostv-pin-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("catostv-remote.pin");
        write_pin(&path, "042137").unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(read_pin_from(&path).as_deref(), Some("042137"));
        write_pin(&path, "999999").unwrap();
        assert_eq!(read_pin_from(&path).as_deref(), Some("999999"));
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(read_pin_from(&path), None);
        assert_eq!(read_pin_from(&dir.join("missing")), None);
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            pin_path().ends_with("catostv-remote.pin")
                || pin_path().to_string_lossy().contains("catostv-remote-")
        );
    }

    #[test]
    fn hints_and_text() {
        assert_eq!(
            url_hint(Ipv4Addr::new(192, 168, 1, 7)),
            "http://192.168.1.7:8099"
        );
        assert_eq!(sanitize_text("hi\n\u{7}there\t"), "hithere");
        assert_eq!(
            sanitize_text(&"é".repeat(500)).chars().count(),
            MAX_TEXT_CHARS
        );
        assert_eq!(text_cmd("-k Up").args, ["--", "-k Up"]);
        assert_eq!(key_cmd("Up").args, ["-k", "Up"]);
        // local_ipv4 may be None in a sandbox without a default route; it
        // must never return loopback.
        if let Some(ip) = local_ipv4() {
            assert!(!ip.is_loopback());
        }
    }

    #[test]
    fn key_whitelist_matches_shell_bindings() {
        assert_eq!(keysym("home"), Some("XF86HomePage"));
        assert_eq!(keysym("menu"), Some("F10"));
        assert_eq!(keysym("back"), Some("Escape"));
        assert_eq!(keysym("rm -rf"), None);
        assert_eq!(keysym("Up"), None);
        let rc = std::fs::read_to_string(format!(
            "{}/../../target-root/home/catostv/.config/labwc/rc.xml",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        for sym in [
            "XF86HomePage",
            "F10",
            "XF86AudioRaiseVolume",
            "XF86AudioLowerVolume",
            "XF86AudioMute",
        ] {
            assert!(rc.contains(&format!("<keybind key=\"{sym}\">")), "{sym}");
        }
        // Every key the page sends is whitelisted.
        let mut rest = PAGE_HTML;
        let mut n = 0;
        while let Some(i) = rest.find("data-key=\"") {
            let after = &rest[i + 10..];
            let end = after.find('"').unwrap();
            assert!(keysym(&after[..end]).is_some(), "{}", &after[..end]);
            rest = &after[end..];
            n += 1;
        }
        assert!(n >= 12);
    }

    #[test]
    fn page_is_self_contained() {
        assert!(!PAGE_HTML.contains("http://"));
        assert!(!PAGE_HTML.contains("https://"));
        assert!(!PAGE_HTML.contains("src="));
        assert!(PAGE_HTML.contains(TOKEN_HEADER));
        let (r, _) = remote();
        let resp = handle_http(
            &req("GET", "/", &[("Host", "192.168.1.2:8099")], "", PHONE),
            &r,
        );
        assert_eq!(resp.status, 200);
        assert!(
            resp.headers
                .iter()
                .any(|(k, _)| k == "Content-Security-Policy")
        );
    }

    #[test]
    fn pairing_and_actions() {
        let (r, runner) = remote();
        // Not paired.
        let resp = handle_http(&req("POST", "/api/key", &[], "key=up", PHONE), &r);
        assert_eq!(resp.status, 401);
        let resp = handle_http(
            &req(
                "POST",
                "/api/key",
                &[(TOKEN_HEADER, &"0".repeat(32))],
                "key=up",
                PHONE,
            ),
            &r,
        );
        assert_eq!(resp.status, 401);
        assert!(runner.lock().unwrap().seen.is_empty());
        let token = pair_ok(&r);
        assert_eq!(token.len(), 32);
        let auth = [(TOKEN_HEADER, token.as_str())];
        assert_eq!(
            handle_http(&req("GET", "/api/check", &auth, "", PHONE), &r).status,
            200
        );
        assert_eq!(
            handle_http(&req("POST", "/api/key", &auth, "key=home", PHONE), &r).status,
            200
        );
        assert_eq!(
            handle_http(&req("POST", "/api/key", &auth, "key=evil", PHONE), &r).status,
            400
        );
        assert_eq!(
            handle_http(
                &req("POST", "/api/text", &auth, "text=Hello+%0Aworld", PHONE),
                &r
            )
            .status,
            200
        );
        assert_eq!(
            handle_http(&req("POST", "/api/text", &auth, "text=%0A", PHONE), &r).status,
            400
        );
        let seen: Vec<Vec<String>> = runner
            .lock()
            .unwrap()
            .seen
            .iter()
            .map(|c| c.args.clone())
            .collect();
        assert_eq!(
            seen,
            vec![vec!["-k", "XF86HomePage"], vec!["--", "Hello world"]]
        );
        // wtype failure is reported.
        runner.lock().unwrap().push(Output::failed(1, "no wayland"));
        assert_eq!(
            handle_http(&req("POST", "/api/key", &auth, "key=up", PHONE), &r).status,
            500
        );
    }

    #[test]
    fn wrong_pins_are_rate_limited() {
        let (r, _) = remote();
        for _ in 0..PIN_ATTEMPTS_PER_IP {
            let resp = handle_http(&req("POST", "/api/pair", &[], "pin=000000", PHONE), &r);
            assert_eq!(resp.status, 403);
        }
        // Even the right PIN is refused now from this IP…
        let resp = handle_http(&req("POST", "/api/pair", &[], "pin=123456", PHONE), &r);
        assert_eq!(resp.status, 429);
        // …but another phone can still pair.
        let resp = handle_http(
            &req("POST", "/api/pair", &[], "pin=123456", "192.168.1.51:1"),
            &r,
        );
        assert_eq!(resp.status, 200);
        // Global cap across many IPs.
        let mut limiter = PinLimiter::default();
        let now = Instant::now();
        for i in 0..PIN_ATTEMPTS_TOTAL {
            let ip = Ipv4Addr::new(10, 0, 0, i as u8);
            assert!(limiter.allowed(ip, now));
            limiter.record_failure(ip, now);
        }
        assert!(!limiter.allowed(Ipv4Addr::new(10, 0, 1, 1), now));
        assert!(limiter.allowed(Ipv4Addr::new(10, 0, 1, 1), now + PIN_WINDOW));
    }

    #[test]
    fn rebinding_and_csrf_are_refused() {
        let (r, _) = remote();
        let resp = handle_http(
            &req("GET", "/", &[("Host", "evil.example:8099")], "", PHONE),
            &r,
        );
        assert_eq!(resp.status, 421);
        let resp = handle_http(
            &req(
                "POST",
                "/api/pair",
                &[
                    ("Host", "192.168.1.2:8099"),
                    ("Origin", "http://evil.example"),
                ],
                "pin=123456",
                PHONE,
            ),
            &r,
        );
        assert_eq!(resp.status, 403);
        let resp = handle_http(
            &req(
                "POST",
                "/api/pair",
                &[
                    ("Host", "192.168.1.2:8099"),
                    ("Origin", "http://192.168.1.2:8099"),
                ],
                "pin=123456",
                PHONE,
            ),
            &r,
        );
        assert_eq!(resp.status, 200);
        assert_eq!(
            handle_http(&req("GET", "/nope", &[], "", PHONE), &r).status,
            404
        );
        assert_eq!(
            handle_http(&req("PUT", "/api/key", &[], "", PHONE), &r).status,
            405
        );
    }

    #[test]
    fn sessions_are_capped() {
        let (r, _) = remote();
        let first = pair_ok(&r);
        for i in 0..MAX_SESSIONS {
            let resp = handle_http(
                &req(
                    "POST",
                    "/api/pair",
                    &[],
                    "pin=123456",
                    &format!("192.168.1.{}:1", i + 60),
                ),
                &r,
            );
            assert_eq!(resp.status, 200);
        }
        assert_eq!(r.sessions.lock().unwrap().len(), MAX_SESSIONS);
        let resp = handle_http(
            &req("GET", "/api/check", &[(TOKEN_HEADER, &first)], "", PHONE),
            &r,
        );
        assert_eq!(resp.status, 401, "oldest session evicted");
    }
}
