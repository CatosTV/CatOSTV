//! DIAL server for YouTube ("cast to TV" from the YouTube phone app).
//!
//! **Status: UNVERIFIED**, built from the DIAL 2.1 behaviour of the Netflix
//! reference server and Google's Cobalt (the YouTube TV runtime); it needs a
//! real phone with the YouTube app to confirm.
//!
//! How YouTube uses DIAL:
//!
//! 1. The phone multicasts `M-SEARCH ST: urn:dial-multiscreen-org:service:dial:1`
//!    ([`crate::ssdp`] answers with `LOCATION: http://<tv>:<port>/dial/description.xml`).
//! 2. It fetches the description; the response header
//!    `Application-URL: http://<tv>:<port>/apps/` tells it where apps live.
//! 3. `GET /apps/YouTube` → `<service xmlns="urn:dial-multiscreen-org:schemas:dial">`
//!    with `<state>running|stopped</state>` and `<additionalData>`. When the
//!    YouTube TV page is running it reports its `screenId` there (it POSTs it
//!    to the `additionalDataUrl` we passed in the launch URL); the phone then
//!    talks to YouTube's servers ("Lounge" API) directly.
//! 4. If stopped, `POST /apps/YouTube` with a form body such as
//!    `pairingCode=<uuid>&theme=cl&v=<video>&t=<secs>`. We launch
//!    `https://www.youtube.com/tv?<validated params>&additionalDataUrl=…`
//!    in the YouTube Firefox profile and answer `201 Created` with
//!    `LOCATION: http://<tv>:<port>/apps/YouTube/run`.
//! 5. `DELETE /apps/YouTube/run` closes it.
//!
//! Launch parameters are whitelisted ([`LAUNCH_KEYS`]) and every value must
//! match `[A-Za-z0-9_.-]{1,128}`; anything else is dropped, so nothing from
//! the request can change the URL's structure. Firefox gets the URL as a
//! single argv element (no shell). Requests with a browser `Origin` other
//! than YouTube's are refused (`403`), as DIAL 2.x requires.
//!
//! Firefox and the YouTube profile: the YouTube app in `apps.toml` runs
//! `firefox --kiosk --new-instance --profile ~/.local/share/catostv/firefox/youtube …`.
//! A second Firefox on the same profile cannot start while one runs, so if
//! a launch carries new parameters while YouTube is open (found by scanning
//! `/proc` for a Firefox main process with that `--profile`), the running
//! instance is closed (SIGTERM, then SIGKILL after 5 s) and relaunched with
//! the pairing URL. The shell will then report the YouTube app it started as
//! closed; the new window still appears on the App desktop (labwc rule).

use crate::error::{Error, Result};
use crate::http::{self, Request, Response};
use crate::ssdp::{Advertisement, DIAL_SERVICE};
use std::net::Ipv4Addr;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const APP_NAME: &str = "YouTube";
pub const DEVICE_TYPE: &str = "urn:dial-multiscreen-org:device:dial:1";
pub const DESCRIPTION_PATH: &str = "/dial/description.xml";
pub const SCPD_PATH: &str = "/dial/scpd.xml";
pub const YOUTUBE_TV_URL: &str = "https://www.youtube.com/tv";
/// Largest launch payload (DIAL limit).
pub const MAX_LAUNCH_PAYLOAD: usize = 4096;
/// Largest additional-data payload kept.
pub const MAX_ADDITIONAL_DATA: usize = 4096;
/// Launch parameters passed through to youtube.com/tv.
pub const LAUNCH_KEYS: &[&str] = &["pairingCode", "theme", "v", "t", "list", "index"];
/// Grace period between SIGTERM and SIGKILL when closing Firefox.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// A UUID for the DIAL device derived from the renderer UUID (stable, but
/// different, so control points don't mix the two root devices up).
pub fn dial_uuid(renderer_uuid: &str) -> String {
    if !http::is_uuid(renderer_uuid) {
        return renderer_uuid.to_string();
    }
    let first = u32::from_str_radix(&renderer_uuid[..8], 16).unwrap_or(0) ^ 0xd1a1_d1a1;
    format!("{first:08x}{}", &renderer_uuid[8..])
}

/// SSDP advertisement of the DIAL root device.
pub fn advertisement(uuid: &str) -> Advertisement {
    Advertisement {
        uuid: uuid.to_string(),
        location_path: DESCRIPTION_PATH.into(),
        device_type: DEVICE_TYPE.into(),
        service_types: vec![DIAL_SERVICE.into()],
    }
}

pub fn device_description(friendly_name: &str, uuid: &str) -> String {
    let esc = crate::dlna::xml_escape;
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<root xmlns=\"urn:schemas-upnp-org:device-1-0\">\
<specVersion><major>1</major><minor>0</minor></specVersion>\
<device><deviceType>{DEVICE_TYPE}</deviceType>\
<friendlyName>{}</friendlyName><manufacturer>CatosTV</manufacturer>\
<modelName>CatosTV</modelName><UDN>uuid:{}</UDN>\
<serviceList><service><serviceType>{DIAL_SERVICE}</serviceType>\
<serviceId>urn:dial-multiscreen-org:serviceId:dial</serviceId>\
<SCPDURL>{SCPD_PATH}</SCPDURL><controlURL>/dial/control</controlURL>\
<eventSubURL></eventSubURL></service></serviceList></device></root>\n",
        esc(friendly_name),
        esc(uuid)
    )
}

/// The (empty) SCPD for the DIAL service; DIAL itself is REST, not SOAP.
pub fn scpd() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<scpd xmlns=\"urn:schemas-upnp-org:service-1-0\">\
<specVersion><major>1</major><minor>0</minor></specVersion>\
<actionList></actionList><serviceStateTable></serviceStateTable></scpd>\n"
}

/// `Application-URL` header value.
pub fn application_url(ip: Ipv4Addr, port: u16) -> String {
    format!("http://{ip}:{port}/apps/")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Running,
    Stopped,
}

/// `GET /apps/YouTube` body.
pub fn app_status_xml(state: AppState, additional: &[(String, String)]) -> String {
    let esc = crate::dlna::xml_escape;
    let mut extra = String::new();
    for (k, v) in additional {
        if is_xml_name(k) {
            extra.push_str(&format!("<{k}>{}</{k}>", esc(v)));
        }
    }
    let (state_str, link) = match state {
        AppState::Running => ("running", "\n  <link rel=\"run\" href=\"run\"/>"),
        AppState::Stopped => ("stopped", ""),
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<service xmlns=\"urn:dial-multiscreen-org:schemas:dial\" dialVer=\"2.1\">\n  \
<name>{APP_NAME}</name>\n  <options allowStop=\"true\"/>\n  <state>{state_str}</state>{link}\n  \
<additionalData>{extra}</additionalData>\n</service>\n"
    )
}

fn is_xml_name(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 64
        && k.as_bytes()[0].is_ascii_alphabetic()
        && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `[A-Za-z0-9_.-]{1,128}`
pub fn is_safe_value(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
}

/// Whitelisted, validated launch parameters from a POST body (first
/// occurrence of each key wins).
pub fn launch_params(body: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v) in http::parse_form(body.trim()) {
        if LAUNCH_KEYS.contains(&k.as_str())
            && is_safe_value(&v)
            && !out.iter().any(|(ok, _)| *ok == k)
        {
            out.push((k, v));
        }
    }
    out
}

/// The URL Firefox opens.
pub fn youtube_launch_url(params: &[(String, String)], port: u16) -> String {
    let mut q: Vec<String> = params
        .iter()
        .filter(|(k, v)| LAUNCH_KEYS.contains(&k.as_str()) && is_safe_value(v))
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    // Same form as the DIAL reference server (note the encoded trailing '?').
    q.push(format!(
        "additionalDataUrl={}",
        http::percent_encode(&format!(
            "http://localhost:{port}/apps/{APP_NAME}/dial_data?"
        ))
    ));
    format!("{YOUTUBE_TV_URL}?{}", q.join("&"))
}

/// DIAL 2.x origin rule: no Origin (native apps) or a YouTube web origin or
/// an Android/iOS `package:` origin.
pub fn origin_allowed(origin: Option<&str>) -> bool {
    let Some(o) = origin.map(str::trim) else {
        return true;
    };
    if o.starts_with("package:") {
        return true;
    }
    let Some(host) = o.strip_prefix("https://") else {
        return false;
    };
    let host = host.strip_suffix(":443").unwrap_or(host);
    host == "youtube.com"
        || (host.ends_with(".youtube.com")
            && host[..host.len() - ".youtube.com".len()]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'))
}

/// Parse a `dial_data` payload (form encoded) into safe key/value pairs.
pub fn parse_additional_data(payload: &str) -> Vec<(String, String)> {
    http::parse_form(payload)
        .into_iter()
        .filter(|(k, v)| is_xml_name(k) && v.len() <= 512 && !v.chars().any(char::is_control))
        .take(16)
        .collect()
}

// ---------------------------------------------------------------------------
// Firefox process handling
// ---------------------------------------------------------------------------

/// How a DIAL "cast to TV" request opens YouTube. Not an app in the drawer
/// any more, only the cast target.
pub const YOUTUBE_CAST_COMMAND: &str = "firefox --kiosk --new-instance --profile ~/.local/share/catostv/firefox/youtube https://www.youtube.com/tv";

/// The YouTube cast command (HOME expanded).
pub fn youtube_command() -> Result<Vec<String>> {
    crate::apps::AppEntry {
        id: "youtube".into(),
        name: "YouTube".into(),
        subtitle: String::new(),
        icon: "play".into(),
        accent: "red".into(),
        command: YOUTUBE_CAST_COMMAND.into(),
        singleton: true,
        icon_path: None,
        source: crate::apps::AppSource::Configured,
    }
    .argv()
}

/// The command with the start URL replaced by `url`.
pub fn youtube_argv(url: &str) -> Result<Vec<String>> {
    let mut argv = youtube_command()?;
    match argv.iter().position(|a| a.starts_with(YOUTUBE_TV_URL)) {
        Some(i) => argv[i] = url.to_string(),
        None => argv.push(url.to_string()),
    }
    Ok(argv)
}

/// Value following `--profile` / `-profile`.
pub fn profile_of(argv: &[String]) -> Option<String> {
    argv.windows(2)
        .find(|w| w[0] == "--profile" || w[0] == "-profile")
        .map(|w| w[1].clone())
}

/// True if a `/proc/<pid>/cmdline` (NUL separated) is a Firefox **main**
/// process using `profile`.
pub fn cmdline_matches_profile(cmdline: &[u8], profile: &str) -> bool {
    let args: Vec<&[u8]> = cmdline
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .collect();
    let Some(argv0) = args.first() else {
        return false;
    };
    let base = argv0.rsplit(|b| *b == b'/').next().unwrap_or(argv0);
    if !base.starts_with(b"firefox") {
        return false;
    }
    if args.iter().any(|a| *a == b"-contentproc") {
        return false;
    }
    let trim = |s: &[u8]| -> Vec<u8> {
        let mut v = s.to_vec();
        while v.len() > 1 && v.last() == Some(&b'/') {
            v.pop();
        }
        v
    };
    let want = trim(profile.as_bytes());
    let mut eq_form = b"--profile=".to_vec();
    eq_form.extend_from_slice(&want);
    args.windows(2)
        .any(|w| (w[0] == b"--profile" || w[0] == b"-profile") && trim(w[1]) == want)
        || args.iter().any(|a| trim(a) == eq_form)
}

/// PIDs of our own Firefox main processes using `profile`.
pub fn find_processes(profile: &str) -> Vec<i32> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let uid = crate::mpv::current_uid();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        if pid <= 1 || entry.metadata().map(|m| m.uid()).ok() != Some(uid) {
            continue;
        }
        let mut buf = Vec::new();
        if let Ok(f) = std::fs::File::open(entry.path().join("cmdline")) {
            let _ = f.take(64 * 1024).read_to_end(&mut buf);
        }
        if cmdline_matches_profile(&buf, profile) {
            pids.push(pid);
        }
    }
    pids
}

mod ffi {
    unsafe extern "C" {
        pub fn kill(pid: i32, sig: i32) -> i32;
    }
}

fn signal_pid(pid: i32, sig: i32) {
    if pid > 1 {
        // SAFETY: kill(2) has no memory-safety preconditions; pid > 1 so it
        // never targets a process group or every process.
        unsafe {
            ffi::kill(pid, sig);
        }
    }
}

/// What DIAL needs from the YouTube app.
pub trait AppControl: Send {
    fn is_running(&mut self) -> bool;
    fn launch(&mut self, url: &str) -> Result<()>;
    /// Close the app; returns false if it was not running.
    fn stop(&mut self) -> bool;
}

/// The real YouTube app: Firefox kiosk with the CatosTV YouTube profile.
pub struct FirefoxYouTube {
    profile: String,
    children: Vec<Child>,
    log: Option<PathBuf>,
}

impl FirefoxYouTube {
    pub fn new(log: Option<PathBuf>) -> Result<Self> {
        let profile = profile_of(&youtube_command()?)
            .ok_or_else(|| Error::msg("the YouTube app has no Firefox profile"))?;
        Ok(Self {
            profile,
            children: Vec::new(),
            log,
        })
    }

    fn reap(&mut self) {
        self.children
            .retain_mut(|c| matches!(c.try_wait(), Ok(None)));
    }
}

impl AppControl for FirefoxYouTube {
    fn is_running(&mut self) -> bool {
        self.reap();
        !find_processes(&self.profile).is_empty()
    }

    fn launch(&mut self, url: &str) -> Result<()> {
        let argv = youtube_argv(url)?;
        if crate::process::find_executable(&argv[0]).is_none() {
            return Err(Error::msg(format!("{} is not installed", argv[0])));
        }
        let (out, err) = match self.log.as_ref().and_then(|p| {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            std::fs::File::create(p).ok()
        }) {
            Some(f) => match f.try_clone() {
                Ok(f2) => (Stdio::from(f), Stdio::from(f2)),
                Err(_) => (Stdio::from(f), Stdio::null()),
            },
            None => (Stdio::null(), Stdio::null()),
        };
        let child = Command::new(&argv[0])
            .args(&argv[1..])
            .env("CATOSTV_APP_ID", "youtube")
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .process_group(0)
            .spawn()
            .map_err(|e| Error::msg(format!("could not start YouTube: {e}")))?;
        self.children.push(child);
        Ok(())
    }

    fn stop(&mut self) -> bool {
        const SIGTERM: i32 = 15;
        const SIGKILL: i32 = 9;
        let pids = find_processes(&self.profile);
        if pids.is_empty() {
            self.reap();
            return false;
        }
        for &p in &pids {
            signal_pid(p, SIGTERM);
        }
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            self.reap();
            if find_processes(&self.profile).is_empty() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        for p in find_processes(&self.profile) {
            signal_pid(p, SIGKILL);
        }
        std::thread::sleep(Duration::from_millis(200));
        self.reap();
        true
    }
}

/// The pairing code in a launch URL (`pairingCode=…`).
pub fn pairing_code(url: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == "pairingCode")
        .map(|(_, v)| v.to_string())
        .filter(|v| is_safe_value(v))
}

/// YouTube through CatTube (the native YouTube app), when
/// it is installed: a cast pairs the phone with CatTube's remote service
/// (`cattube --pair CODE`); the phone then says what to play over
/// YouTube's own connection, and CatTube plays it.
pub struct CatTubeYouTube {
    launched: bool,
}

impl CatTubeYouTube {
    pub fn available() -> bool {
        crate::process::find_executable("cattube").is_some()
    }

    pub fn new() -> Self {
        Self { launched: false }
    }
}

impl Default for CatTubeYouTube {
    fn default() -> Self {
        Self::new()
    }
}

fn cattube(args: &[&str]) -> Result<()> {
    let out = Command::new("cattube")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| Error::msg(format!("could not start CatTube: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::msg(format!(
            "CatTube: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

impl AppControl for CatTubeYouTube {
    fn is_running(&mut self) -> bool {
        self.launched
    }

    fn launch(&mut self, url: &str) -> Result<()> {
        if let Some(code) = pairing_code(url) {
            cattube(&["--pair", &code])?;
        }
        self.launched = true;
        Ok(())
    }

    fn stop(&mut self) -> bool {
        let was = std::mem::take(&mut self.launched);
        let _ = cattube(&["--stop"]);
        was
    }
}

// ---------------------------------------------------------------------------
// HTTP routing
// ---------------------------------------------------------------------------

/// Shared DIAL state for the HTTP handler.
pub struct Dial {
    pub friendly_name: String,
    pub uuid: String,
    pub port: u16,
    app: Mutex<Box<dyn AppControl>>,
    additional: Mutex<Vec<(String, String)>>,
}

impl Dial {
    pub fn new(friendly_name: String, uuid: String, port: u16, app: Box<dyn AppControl>) -> Self {
        Self {
            friendly_name,
            uuid,
            port,
            app: Mutex::new(app),
            additional: Mutex::new(Vec::new()),
        }
    }

    fn clear_additional(&self) {
        if let Ok(mut a) = self.additional.lock() {
            a.clear();
        }
    }
}

fn with_cors(resp: Response, origin: Option<&str>) -> Response {
    match origin {
        Some(o) if origin_allowed(Some(o)) => resp
            .header("Access-Control-Allow-Origin", o)
            .header("Vary", "Origin"),
        _ => resp,
    }
}

/// Handle `/dial/*` and `/apps/*`; `None` for other paths.
pub fn handle_http(req: &Request, dial: &Dial) -> Option<Response> {
    let path = req.path.as_str();
    let get = req.method == "GET" || req.method == "HEAD";
    let local = req.local_ipv4().unwrap_or(Ipv4Addr::LOCALHOST);
    if path == DESCRIPTION_PATH {
        return Some(if get {
            Response::xml(200, device_description(&dial.friendly_name, &dial.uuid))
                .header("Application-URL", application_url(local, dial.port))
                .header("Access-Control-Expose-Headers", "Application-URL")
        } else {
            Response::status_only(405)
        });
    }
    if path == SCPD_PATH {
        return Some(Response::xml(200, scpd()));
    }
    if path.starts_with("/dial/") {
        return Some(Response::status_only(404));
    }
    let rest = path.strip_prefix("/apps/")?;
    let origin = req.header("origin");
    if !origin_allowed(origin) {
        return Some(Response::status_only(403));
    }
    let resp = match rest {
        "YouTube" => app_resource(req, dial, local),
        "YouTube/run" => match req.method.as_str() {
            "DELETE" => {
                let stopped = dial.app.lock().map(|mut a| a.stop()).unwrap_or(false);
                dial.clear_additional();
                if stopped {
                    Response::new(200)
                } else {
                    Response::status_only(404)
                }
            }
            "OPTIONS" => preflight("DELETE, OPTIONS"),
            _ => Response::status_only(501),
        },
        "YouTube/dial_data" => match req.method.as_str() {
            "OPTIONS" => preflight("POST, OPTIONS"),
            _ if !req.is_from_loopback() => Response::status_only(403),
            "POST" | "GET" => {
                let payload = if req.method == "POST" && !req.body.is_empty() {
                    req.body_text()
                } else {
                    req.query.clone()
                };
                if payload.len() > MAX_ADDITIONAL_DATA {
                    Response::status_only(413)
                } else {
                    if let Ok(mut a) = dial.additional.lock() {
                        *a = parse_additional_data(&payload);
                    }
                    Response::new(200)
                }
            }
            _ => Response::status_only(405),
        },
        _ => Response::status_only(404),
    };
    Some(with_cors(resp, origin))
}

fn preflight(methods: &str) -> Response {
    Response::new(204)
        .header("Access-Control-Allow-Methods", methods)
        .header("Access-Control-Allow-Headers", "Content-Type")
        .header("Access-Control-Max-Age", "600")
}

fn app_resource(req: &Request, dial: &Dial, local: Ipv4Addr) -> Response {
    match req.method.as_str() {
        "GET" | "HEAD" => {
            let running = dial.app.lock().map(|mut a| a.is_running()).unwrap_or(false);
            let additional = if running {
                dial.additional
                    .lock()
                    .map(|a| a.clone())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let state = if running {
                AppState::Running
            } else {
                AppState::Stopped
            };
            Response::xml(200, app_status_xml(state, &additional))
        }
        "POST" => {
            if req.body.len() > MAX_LAUNCH_PAYLOAD {
                return Response::status_only(413);
            }
            let params = launch_params(&req.body_text());
            let location = format!("http://{local}:{}/apps/{APP_NAME}/run", dial.port);
            let Ok(mut app) = dial.app.lock() else {
                return Response::status_only(503);
            };
            let running = app.is_running();
            if running && params.is_empty() {
                return Response::new(200).header("LOCATION", location);
            }
            if running {
                app.stop();
            }
            dial.clear_additional();
            match app.launch(&youtube_launch_url(&params, dial.port)) {
                Ok(()) => Response::text(201, "").header("LOCATION", location),
                Err(e) => Response::text(503, format!("{e}\n")),
            }
        }
        "OPTIONS" => preflight("GET, POST, OPTIONS"),
        _ => Response::status_only(405),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dlna::xml_is_balanced;
    use std::sync::Arc;

    #[derive(Default)]
    struct FakeState {
        running: bool,
        launched: Vec<String>,
        stops: usize,
        fail: bool,
    }

    #[derive(Clone, Default)]
    struct FakeApp(Arc<Mutex<FakeState>>);

    impl AppControl for FakeApp {
        fn is_running(&mut self) -> bool {
            self.0.lock().unwrap().running
        }
        fn launch(&mut self, url: &str) -> Result<()> {
            let mut s = self.0.lock().unwrap();
            if s.fail {
                return Err(Error::msg("firefox is not installed"));
            }
            s.launched.push(url.into());
            s.running = true;
            Ok(())
        }
        fn stop(&mut self) -> bool {
            let mut s = self.0.lock().unwrap();
            s.stops += 1;
            std::mem::replace(&mut s.running, false)
        }
    }

    fn dial() -> (Dial, FakeApp) {
        let app = FakeApp::default();
        (
            Dial::new(
                "CatosTV".into(),
                "aaaaaaaa-2222-4333-8444-555555555555".into(),
                49494,
                Box::new(app.clone()),
            ),
            app,
        )
    }

    fn req(method: &str, path: &str, headers: &[(&str, &str)], body: &str, peer: &str) -> Request {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        Request {
            method: method.into(),
            path: path.into(),
            query: query.into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
            peer: Some(peer.parse().unwrap()),
            local: Some("192.168.1.2:49494".parse().unwrap()),
        }
    }

    const PHONE: &str = "192.168.1.50:40000";

    fn header<'a>(r: &'a Response, k: &str) -> Option<&'a str> {
        r.headers
            .iter()
            .find(|(a, _)| a.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn uuid_derivation_is_stable_and_different() {
        let u = "11111111-2222-4333-8444-555555555555";
        let d = dial_uuid(u);
        assert!(http::is_uuid(&d));
        assert_ne!(d, u);
        assert_eq!(d, dial_uuid(u));
        assert_eq!(&d[8..], &u[8..]);
        assert_eq!(dial_uuid("junk"), "junk");
    }

    #[test]
    fn documents_are_well_formed() {
        let d = device_description("A & B", "aaaaaaaa-2222-4333-8444-555555555555");
        assert!(xml_is_balanced(&d));
        assert!(d.contains("<friendlyName>A &amp; B</friendlyName>"));
        assert!(xml_is_balanced(scpd()));
        let s = app_status_xml(
            AppState::Running,
            &[
                ("screenId".into(), "abc<def>".into()),
                ("bad key".into(), "x".into()),
            ],
        );
        assert!(xml_is_balanced(&s), "{s}");
        assert!(s.contains("<state>running</state>"));
        assert!(s.contains("<link rel=\"run\" href=\"run\"/>"));
        assert!(s.contains("<screenId>abc&lt;def&gt;</screenId>"));
        assert!(!s.contains("bad key"));
        let s = app_status_xml(AppState::Stopped, &[]);
        assert!(s.contains("<state>stopped</state>"));
        assert!(!s.contains("<link"));
        assert!(s.contains("xmlns=\"urn:dial-multiscreen-org:schemas:dial\""));
    }

    #[test]
    fn cast_pairing_code_for_cattube() {
        let url = youtube_launch_url(
            &launch_params("pairingCode=3f1c-ab_9&theme=cl&v=WhWc3b3KhnY"),
            49494,
        );
        assert_eq!(pairing_code(&url).as_deref(), Some("3f1c-ab_9"));
        assert_eq!(pairing_code("https://www.youtube.com/tv?v=x"), None);
    }

    #[test]
    fn launch_params_are_whitelisted_and_validated() {
        let p = launch_params(
            "pairingCode=3f1c2a9e-1111-2222-3333-444455556666&v=dQw4w9WgXcQ&t=12.5&theme=cl\
             &evil=1&list=PL%26x%3D1&v=second&index=%0A1&additionalDataUrl=http%3A%2F%2Fevil",
        );
        assert_eq!(
            p,
            vec![
                (
                    "pairingCode".into(),
                    "3f1c2a9e-1111-2222-3333-444455556666".into()
                ),
                ("v".into(), "dQw4w9WgXcQ".into()),
                ("t".into(), "12.5".into()),
                ("theme".into(), "cl".into()),
            ]
        );
        let url = youtube_launch_url(&p, 49494);
        assert_eq!(
            url,
            "https://www.youtube.com/tv?pairingCode=3f1c2a9e-1111-2222-3333-444455556666&v=dQw4w9WgXcQ&t=12.5&theme=cl\
             &additionalDataUrl=http%3A%2F%2Flocalhost%3A49494%2Fapps%2FYouTube%2Fdial_data%3F"
        );
        assert!(crate::mpv::is_playable_url(&url));
        assert_eq!(
            youtube_launch_url(&[], 1),
            "https://www.youtube.com/tv?additionalDataUrl=http%3A%2F%2Flocalhost%3A1%2Fapps%2FYouTube%2Fdial_data%3F"
        );
        // Even hand-built unsafe params are filtered at URL build time.
        assert!(
            !youtube_launch_url(&[("v".into(), "a&b".into()), ("x".into(), "1".into())], 1)
                .contains("a&b")
        );
        assert!(launch_params("\u{0}\u{1}%%%=&&=").is_empty());
    }

    #[test]
    fn origins() {
        assert!(origin_allowed(None));
        assert!(origin_allowed(Some("https://www.youtube.com")));
        assert!(origin_allowed(Some("https://m.youtube.com:443")));
        assert!(origin_allowed(Some("https://youtube.com")));
        assert!(origin_allowed(Some("package:com.google.android.youtube")));
        assert!(!origin_allowed(Some("https://evil.example")));
        assert!(!origin_allowed(Some("https://evilyoutube.com")));
        assert!(!origin_allowed(Some("https://youtube.com.evil.example")));
        assert!(!origin_allowed(Some("http://www.youtube.com")));
        assert!(!origin_allowed(Some("https://x/y.youtube.com")));
        assert!(!origin_allowed(Some("null")));
    }

    #[test]
    fn cmdline_matching() {
        let p = "/home/catostv/.local/share/catostv/firefox/youtube";
        let mk = |args: &[&str]| args.join("\0").into_bytes();
        assert!(cmdline_matches_profile(
            &mk(&[
                "/usr/lib/firefox/firefox",
                "--kiosk",
                "--new-instance",
                "--profile",
                p,
                "https://www.youtube.com/tv",
                ""
            ]),
            p
        ));
        assert!(cmdline_matches_profile(
            &mk(&["firefox", "-profile", &format!("{p}/")]),
            p
        ));
        assert!(cmdline_matches_profile(
            &mk(&["firefox", &format!("--profile={p}")]),
            p
        ));
        // Other profile, content process, other program, empty.
        assert!(!cmdline_matches_profile(
            &mk(&[
                "firefox",
                "--profile",
                "/home/catostv/.local/share/catostv/firefox/browser"
            ]),
            p
        ));
        assert!(!cmdline_matches_profile(
            &mk(&["/usr/lib/firefox/firefox", "-contentproc", "--profile", p]),
            p
        ));
        assert!(!cmdline_matches_profile(
            &mk(&["/usr/bin/mpv", "--profile", p]),
            p
        ));
        assert!(!cmdline_matches_profile(
            &mk(&["sh", "-c", &format!("firefox --profile {p}")]),
            p
        ));
        assert!(!cmdline_matches_profile(b"", p));
        assert!(!cmdline_matches_profile(b"\0\0\0", p));
        assert!(!cmdline_matches_profile(&mk(&["firefox", "--profile"]), p));
    }

    #[test]
    fn youtube_argv_reuses_the_app_definition() {
        let argv = youtube_argv("https://www.youtube.com/tv?v=abc").unwrap();
        assert_eq!(argv[0], "firefox");
        assert!(argv.contains(&"--kiosk".to_string()));
        assert!(argv.contains(&"--new-instance".to_string()));
        let profile = profile_of(&argv).unwrap();
        assert!(
            profile.ends_with("/.local/share/catostv/firefox/youtube"),
            "{profile}"
        );
        assert!(!profile.contains('~') && !profile.contains("$HOME"));
        assert_eq!(argv.last().unwrap(), "https://www.youtube.com/tv?v=abc");
        assert_eq!(argv.iter().filter(|a| a.starts_with("https://")).count(), 1);
    }

    #[test]
    fn find_processes_runs_without_matches() {
        assert!(find_processes("/nonexistent/profile/for/catostv/test").is_empty());
    }

    #[test]
    fn http_flow() {
        let (d, app) = dial();
        // Description with Application-URL using the local address.
        let r = handle_http(&req("GET", DESCRIPTION_PATH, &[], "", PHONE), &d).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(
            header(&r, "Application-URL"),
            Some("http://192.168.1.2:49494/apps/")
        );
        // Not ours.
        assert!(handle_http(&req("GET", "/dlna/description.xml", &[], "", PHONE), &d).is_none());
        assert_eq!(
            handle_http(&req("GET", "/apps/Netflix", &[], "", PHONE), &d)
                .unwrap()
                .status,
            404
        );
        // Stopped.
        let r = handle_http(&req("GET", "/apps/YouTube", &[], "", PHONE), &d).unwrap();
        assert!(String::from_utf8_lossy(&r.body).contains("<state>stopped</state>"));
        // Launch.
        let r = handle_http(
            &req(
                "POST",
                "/apps/YouTube",
                &[],
                "pairingCode=abc-123&v=xyz&evil=;rm",
                PHONE,
            ),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 201);
        assert_eq!(
            header(&r, "LOCATION"),
            Some("http://192.168.1.2:49494/apps/YouTube/run")
        );
        assert_eq!(
            app.0.lock().unwrap().launched,
            vec![youtube_launch_url(
                &[
                    ("pairingCode".into(), "abc-123".into()),
                    ("v".into(), "xyz".into())
                ],
                49494
            )]
        );
        // The TV page reports its screenId from localhost only.
        let r = handle_http(
            &req(
                "POST",
                "/apps/YouTube/dial_data",
                &[],
                "screenId=s123&theme=cl",
                PHONE,
            ),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 403);
        let r = handle_http(
            &req(
                "POST",
                "/apps/YouTube/dial_data",
                &[("Origin", "https://www.youtube.com")],
                "screenId=s123&theme=cl",
                "127.0.0.1:5000",
            ),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(
            header(&r, "Access-Control-Allow-Origin"),
            Some("https://www.youtube.com")
        );
        let r = handle_http(&req("GET", "/apps/YouTube", &[], "", PHONE), &d).unwrap();
        let body = String::from_utf8_lossy(&r.body).into_owned();
        assert!(body.contains("<state>running</state>"));
        assert!(body.contains("<screenId>s123</screenId>"), "{body}");
        // Running + empty POST → 200, no relaunch.
        let r = handle_http(&req("POST", "/apps/YouTube", &[], "", PHONE), &d).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(app.0.lock().unwrap().launched.len(), 1);
        // Running + new pairing → stop + relaunch, additional data cleared.
        let r = handle_http(
            &req("POST", "/apps/YouTube", &[], "pairingCode=new", PHONE),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 201);
        assert_eq!(app.0.lock().unwrap().stops, 1);
        let r = handle_http(&req("GET", "/apps/YouTube", &[], "", PHONE), &d).unwrap();
        assert!(!String::from_utf8_lossy(&r.body).contains("screenId"));
        // Stop.
        let r = handle_http(&req("DELETE", "/apps/YouTube/run", &[], "", PHONE), &d).unwrap();
        assert_eq!(r.status, 200);
        let r = handle_http(&req("DELETE", "/apps/YouTube/run", &[], "", PHONE), &d).unwrap();
        assert_eq!(r.status, 404);
        // Foreign web origin cannot launch.
        let r = handle_http(
            &req(
                "POST",
                "/apps/YouTube",
                &[("Origin", "https://evil.example")],
                "v=x",
                PHONE,
            ),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 403);
        // Oversized payload.
        let big = format!("v={}", "a".repeat(MAX_LAUNCH_PAYLOAD));
        let r = handle_http(&req("POST", "/apps/YouTube", &[], &big, PHONE), &d).unwrap();
        assert_eq!(r.status, 413);
        // Launch failure → 503.
        app.0.lock().unwrap().fail = true;
        let r = handle_http(&req("POST", "/apps/YouTube", &[], "v=x", PHONE), &d).unwrap();
        assert_eq!(r.status, 503);
    }
}
