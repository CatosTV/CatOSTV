//! Minimal HTTP/1.1 server (and tiny client) helpers for the LAN daemons
//! `catostv-castd` and `catostv-remoted`.
//!
//! Scope is deliberately small: one request per connection
//! (`Connection: close`), `Content-Length` bodies only, bounded everything.
//! Every byte read from the network is treated as hostile:
//!
//! * the request head is limited to [`MAX_HEAD_BYTES`] (16 KiB) and at most
//!   [`MAX_HEADERS`] header lines;
//! * bodies are limited to [`MAX_BODY_BYTES`] (64 KiB) and must declare a
//!   `Content-Length`; chunked request bodies are rejected with `411`, any
//!   other transfer coding with `501`;
//! * sockets get read/write timeouts, so a slow client cannot pin a thread;
//! * the accept loop caps concurrent connections and answers `503` beyond it;
//! * parsing never panics (garbage tests below), header values written back
//!   are stripped of CR/LF so a request can never split a response.
//!
//! The same module provides percent-decoding, form/query parsing,
//! percent-encoding and random identifiers (from `/dev/urandom`).

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

/// Largest accepted request line + headers.
pub const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Largest accepted request body.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Most header lines accepted in one request.
pub const MAX_HEADERS: usize = 64;
/// Default socket read/write timeout.
pub const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// Default cap on concurrently handled connections.
pub const MAX_CONNECTIONS: usize = 32;
/// At most this many `key=value` pairs are parsed from a query/form body.
pub const MAX_FORM_PAIRS: usize = 64;

/// A parsed request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// Upper-case method token (`GET`, `POST`, `SUBSCRIBE`, ...).
    pub method: String,
    /// Raw path (still percent-encoded), always starting with `/`, or `*`.
    pub path: String,
    /// Raw query string without the `?` (empty if none).
    pub query: String,
    /// Header (name, value) pairs in arrival order, names as sent.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Remote address of the client (set by the server loop).
    pub peer: Option<SocketAddr>,
    /// Local address the client connected to (set by the server loop).
    pub local: Option<SocketAddr>,
}

impl Request {
    /// First header with this name (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Body as UTF-8 (lossy).
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Decoded query parameters.
    pub fn query_pairs(&self) -> Vec<(String, String)> {
        parse_form(&self.query)
    }

    /// Decoded `application/x-www-form-urlencoded` body parameters.
    pub fn form_pairs(&self) -> Vec<(String, String)> {
        parse_form(&self.body_text())
    }

    /// The peer's IPv4 address (IPv4-mapped IPv6 is unwrapped).
    pub fn peer_ipv4(&self) -> Option<Ipv4Addr> {
        self.peer.and_then(|a| socket_ipv4(&a))
    }

    /// The local IPv4 address this request arrived on.
    pub fn local_ipv4(&self) -> Option<Ipv4Addr> {
        self.local.and_then(|a| socket_ipv4(&a))
    }

    /// True when the request came from the same machine.
    pub fn is_from_loopback(&self) -> bool {
        self.peer_ipv4().is_some_and(|ip| ip.is_loopback())
    }
}

fn socket_ipv4(a: &SocketAddr) -> Option<Ipv4Addr> {
    match a {
        SocketAddr::V4(v4) => Some(*v4.ip()),
        SocketAddr::V6(v6) => v6.ip().to_ipv4_mapped(),
    }
}

/// Why a request could not be read. Each maps to the status the server
/// answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    /// Malformed request line or headers.
    BadRequest,
    /// Head over [`MAX_HEAD_BYTES`] or too many headers.
    HeadTooLarge,
    /// Body over the limit.
    BodyTooLarge,
    /// Chunked request body (we require Content-Length).
    LengthRequired,
    /// Unknown transfer coding or unsupported HTTP version.
    NotImplemented,
    /// Client was too slow or the connection broke.
    Timeout,
    /// Connection closed before a full request arrived.
    Closed,
}

impl HttpError {
    pub fn status(self) -> u16 {
        match self {
            Self::BadRequest | Self::Closed => 400,
            Self::HeadTooLarge => 431,
            Self::BodyTooLarge => 413,
            Self::LengthRequired => 411,
            Self::NotImplemented => 501,
            Self::Timeout => 408,
        }
    }
}

/// Parse a request head (request line + headers, without the blank line).
pub fn parse_head(head: &[u8]) -> Result<Request, HttpError> {
    if head.len() > MAX_HEAD_BYTES {
        return Err(HttpError::HeadTooLarge);
    }
    let text = std::str::from_utf8(head).map_err(|_| HttpError::BadRequest)?;
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let request_line = lines.next().ok_or(HttpError::BadRequest)?;
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(HttpError::BadRequest);
    };
    if method.is_empty()
        || method.len() > 32
        || !method.bytes().all(|b| b.is_ascii_uppercase() || b == b'-')
    {
        return Err(HttpError::BadRequest);
    }
    match version {
        "HTTP/1.1" | "HTTP/1.0" => {}
        v if v.starts_with("HTTP/") => return Err(HttpError::NotImplemented),
        _ => return Err(HttpError::BadRequest),
    }
    let (path, query) = split_target(target)?;

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            // Obsolete line folding: refuse instead of guessing.
            return Err(HttpError::BadRequest);
        }
        let (name, value) = line.split_once(':').ok_or(HttpError::BadRequest)?;
        if name.is_empty() || !name.bytes().all(is_token_byte) {
            return Err(HttpError::BadRequest);
        }
        if value.bytes().any(|b| (b < 0x20 && b != b'\t') || b == 0x7f) {
            return Err(HttpError::BadRequest);
        }
        if headers.len() >= MAX_HEADERS {
            return Err(HttpError::HeadTooLarge);
        }
        headers.push((name.to_string(), value.trim().to_string()));
    }
    Ok(Request {
        method: method.to_string(),
        path,
        query,
        headers,
        ..Request::default()
    })
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

fn split_target(target: &str) -> Result<(String, String), HttpError> {
    if target == "*" {
        return Ok(("*".into(), String::new()));
    }
    // absolute-form: http://host[:port]/path → keep only the path.
    let origin_form = if let Some(rest) = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))
    {
        match rest.find('/') {
            Some(i) => &rest[i..],
            None => "/",
        }
    } else {
        target
    };
    if !origin_form.starts_with('/')
        || origin_form
            .bytes()
            .any(|b| b <= b' ' || b == 0x7f || b == b'#')
    {
        return Err(HttpError::BadRequest);
    }
    Ok(match origin_form.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (origin_form.to_string(), String::new()),
    })
}

/// Read one request from `stream` with the given body limit. The caller sets
/// socket timeouts; a timeout surfaces as [`HttpError::Timeout`].
pub fn read_request<R: Read>(stream: &mut R, max_body: usize) -> Result<Request, HttpError> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    let head_end = loop {
        if let Some(end) = find_head_end(&buf) {
            break end;
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(HttpError::HeadTooLarge);
        }
        let n = match stream.read(&mut chunk) {
            Ok(0) => {
                return Err(if buf.is_empty() {
                    HttpError::Closed
                } else {
                    HttpError::BadRequest
                });
            }
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(HttpError::Timeout),
        };
        buf.extend_from_slice(&chunk[..n]);
    };
    let (head_len, sep_len) = head_end;
    if head_len > MAX_HEAD_BYTES {
        return Err(HttpError::HeadTooLarge);
    }
    let mut req = parse_head(&buf[..head_len])?;
    let already = buf.split_off(head_len + sep_len);

    if let Some(te) = req.header("transfer-encoding") {
        return Err(if te.to_ascii_lowercase().contains("chunked") {
            HttpError::LengthRequired
        } else {
            HttpError::NotImplemented
        });
    }
    let mut length: Option<usize> = None;
    for (k, v) in &req.headers {
        if k.eq_ignore_ascii_case("content-length") {
            if v.is_empty() || v.len() > 12 || !v.bytes().all(|b| b.is_ascii_digit()) {
                return Err(HttpError::BadRequest);
            }
            let n: usize = v.parse().map_err(|_| HttpError::BadRequest)?;
            if length.is_some_and(|l| l != n) {
                return Err(HttpError::BadRequest);
            }
            length = Some(n);
        }
    }
    let length = length.unwrap_or(0);
    if length > max_body {
        return Err(HttpError::BodyTooLarge);
    }
    let mut body = already;
    body.truncate(length);
    while body.len() < length {
        let want = (length - body.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Err(HttpError::BadRequest),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(HttpError::Timeout),
        }
    }
    req.body = body;
    Ok(req)
}

/// Position of the end of the head and the separator length
/// (`\r\n\r\n` = 4 or a bare `\n\n` = 2).
fn find_head_end(buf: &[u8]) -> Option<(usize, usize)> {
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n");
    let lf = buf.windows(2).position(|w| w == b"\n\n");
    match (crlf, lf) {
        (Some(a), Some(b)) if b < a => Some((b, 2)),
        (Some(a), _) => Some((a, 4)),
        (None, Some(b)) => Some((b, 2)),
        (None, None) => None,
    }
}

/// An HTTP response. Always sent with `Connection: close`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn with_body(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self::new(status)
            .header("Content-Type", content_type)
            .body(body)
    }

    pub fn text(status: u16, body: impl Into<String>) -> Self {
        Self::with_body(status, "text/plain; charset=utf-8", body.into())
    }

    pub fn xml(status: u16, body: impl Into<String>) -> Self {
        Self::with_body(status, "text/xml; charset=\"utf-8\"", body.into())
    }

    pub fn html(status: u16, body: impl Into<String>) -> Self {
        Self::with_body(status, "text/html; charset=utf-8", body.into())
    }

    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self::with_body(status, "application/json", body.into())
    }

    /// Plain-text response with the standard reason phrase as body.
    pub fn status_only(status: u16) -> Self {
        Self::text(status, format!("{status} {}\n", reason(status)))
    }

    /// Add a header. CR/LF are removed from name and value so data from a
    /// request can never inject extra headers.
    pub fn header(mut self, name: &str, value: impl AsRef<str>) -> Self {
        let clean = |s: &str| s.chars().filter(|c| *c != '\r' && *c != '\n').collect();
        self.headers.push((clean(name), clean(value.as_ref())));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// Serialise. `head_only` omits the body (HEAD requests) but keeps the
    /// `Content-Length` of the full body.
    pub fn to_bytes(&self, head_only: bool) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {} {}\r\n", self.status, reason(self.status));
        for (k, v) in &self.headers {
            if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("connection") {
                continue;
            }
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        out.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        out.push_str("Connection: close\r\n\r\n");
        let mut bytes = out.into_bytes();
        if !head_only {
            bytes.extend_from_slice(&self.body);
        }
        bytes
    }

    pub fn write_to<W: Write>(&self, w: &mut W, head_only: bool) -> io::Result<()> {
        w.write_all(&self.to_bytes(head_only))?;
        w.flush()
    }
}

/// Standard reason phrase.
pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        421 => "Misdirected Request",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

// ---------------------------------------------------------------------------
// URL encoding
// ---------------------------------------------------------------------------

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode `%XX` escapes (and `+` as space when `plus_is_space`). A `%` not
/// followed by two hex digits is kept literally. Returns `None` if the
/// result is not valid UTF-8.
pub fn percent_decode(s: &str, plus_is_space: bool) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b'+' if plus_is_space => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Percent-encode everything except RFC 3986 unreserved characters.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Parse `a=1&b=x+y` (query string or form body). Pairs that do not decode
/// to UTF-8 are skipped; at most [`MAX_FORM_PAIRS`] pairs are returned.
pub fn parse_form(s: &str) -> Vec<(String, String)> {
    s.split('&')
        .filter(|p| !p.is_empty())
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            Some((percent_decode(k, true)?, percent_decode(v, true)?))
        })
        .take(MAX_FORM_PAIRS)
        .collect()
}

/// First value for `key` in decoded pairs.
pub fn form_value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// A parsed `http://` URL with an IPv4 literal host (used for GENA
/// callbacks; hostnames are refused so no DNS lookup is ever made from
/// network-supplied data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpUrl {
    pub addr: SocketAddrV4,
    /// Path plus query, starting with `/`.
    pub path: String,
}

pub fn parse_http_url(url: &str) -> Option<HttpUrl> {
    let rest = url.trim().strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (authority, 80),
    };
    let ip: Ipv4Addr = host.parse().ok()?;
    if port == 0 || path.bytes().any(|b| b <= b' ' || b == 0x7f) || path.len() > 1024 {
        return None;
    }
    Some(HttpUrl {
        addr: SocketAddrV4::new(ip, port),
        path: path.to_string(),
    })
}

/// True if `host_header` (the `Host:` value) names an IP literal, optionally
/// with a port. Used to refuse DNS-rebinding requests that arrive with a
/// foreign host name.
pub fn host_is_ip_literal(host_header: &str) -> bool {
    let h = host_header.trim();
    if let Some(rest) = h.strip_prefix('[') {
        // [v6]:port
        return rest
            .split_once(']')
            .is_some_and(|(ip, _)| ip.parse::<std::net::Ipv6Addr>().is_ok());
    }
    let host = match h.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => host,
        _ => h,
    };
    host.parse::<Ipv4Addr>().is_ok() || host == "localhost"
}

// ---------------------------------------------------------------------------
// Server loop
// ---------------------------------------------------------------------------

/// Server limits.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_connections: usize,
    pub io_timeout: Duration,
    pub max_body: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: MAX_CONNECTIONS,
            io_timeout: IO_TIMEOUT,
            max_body: MAX_BODY_BYTES,
        }
    }
}

/// A request handler shared by all connection threads.
pub type Handler = dyn Fn(&Request) -> Response + Send + Sync + 'static;

struct ConnGuard(Arc<AtomicUsize>);
impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Accept connections forever, one thread per connection, at most
/// `limits.max_connections` at once (extra connections get `503`).
pub fn serve(listener: TcpListener, limits: Limits, handler: Arc<Handler>) {
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let (stream, peer) = match listener.accept() {
            Ok(x) => x,
            Err(_) => {
                // EMFILE, ECONNABORTED, ...: back off instead of spinning.
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let _ = stream.set_read_timeout(Some(limits.io_timeout));
        let _ = stream.set_write_timeout(Some(limits.io_timeout));
        if active.fetch_add(1, Ordering::SeqCst) >= limits.max_connections {
            active.fetch_sub(1, Ordering::SeqCst);
            let mut s = stream;
            let _ = s.set_write_timeout(Some(Duration::from_secs(1)));
            let _ = Response::status_only(503)
                .header("Retry-After", "1")
                .write_to(&mut s, false);
            continue;
        }
        let guard = ConnGuard(active.clone());
        let handler = handler.clone();
        let spawned = thread::Builder::new()
            .name("catostv-http".into())
            .spawn(move || {
                let _guard = guard;
                handle_connection(stream, Some(peer), limits, &*handler);
            });
        if spawned.is_err() {
            thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Run [`serve`] on a background thread.
pub fn spawn_server(
    listener: TcpListener,
    limits: Limits,
    handler: Arc<Handler>,
) -> io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("catostv-http-accept".into())
        .spawn(move || serve(listener, limits, handler))
}

/// Read one request, answer it, close. Errors in the request are answered
/// with the matching status code.
pub fn handle_connection(
    mut stream: TcpStream,
    peer: Option<SocketAddr>,
    limits: Limits,
    handler: &Handler,
) {
    let local = stream.local_addr().ok();
    let response = match read_request(&mut stream, limits.max_body) {
        Ok(mut req) => {
            req.peer = peer.or_else(|| stream.peer_addr().ok());
            req.local = local;
            let head_only = req.method == "HEAD";
            let resp = handler(&req);
            let _ = resp.write_to(&mut stream, head_only);
            None
        }
        Err(HttpError::Closed) => None,
        Err(e) => Some(Response::status_only(e.status())),
    };
    if let Some(resp) = response {
        let _ = resp.write_to(&mut stream, false);
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

// ---------------------------------------------------------------------------
// Tiny client (GENA NOTIFY)
// ---------------------------------------------------------------------------

/// Send one request and return the response status code. Used to deliver
/// UPnP event notifications; the response body is ignored.
pub fn send_request(
    addr: SocketAddrV4,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &[u8],
    timeout: Duration,
) -> io::Result<u16> {
    let mut stream = TcpStream::connect_timeout(&SocketAddr::V4(addr), timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let clean = |s: &str| -> String { s.chars().filter(|c| *c != '\r' && *c != '\n').collect() };
    let mut head = format!("{method} {} HTTP/1.1\r\nHOST: {addr}\r\n", clean(path));
    for (k, v) in headers {
        head.push_str(&format!("{}: {}\r\n", clean(k), clean(v)));
    }
    head.push_str(&format!(
        "CONTENT-LENGTH: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    // Read just the status line (bounded).
    let mut buf = [0u8; 512];
    let mut got = 0;
    while got < buf.len() {
        let n = stream.read(&mut buf[got..])?;
        if n == 0 {
            break;
        }
        got += n;
        if buf[..got].contains(&b'\n') {
            break;
        }
    }
    let line = String::from_utf8_lossy(&buf[..got]);
    line.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad status line"))
}

// ---------------------------------------------------------------------------
// Random identifiers
// ---------------------------------------------------------------------------

/// `n` random bytes from the kernel CSPRNG.
pub fn random_bytes(n: usize) -> io::Result<Vec<u8>> {
    let mut f = std::fs::File::open("/dev/urandom")?;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// Lower-case hex of `n` random bytes.
pub fn random_hex(n: usize) -> io::Result<String> {
    Ok(to_hex(&random_bytes(n)?))
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A random (version 4) UUID, e.g. `1b4e28ba-2fa1-41d2-883f-0016d3cca427`.
pub fn uuid_v4() -> io::Result<String> {
    let mut b = random_bytes(16)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = to_hex(&b);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

/// True for the canonical 8-4-4-4-12 hex UUID form.
pub fn is_uuid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Constant-time byte comparison (for PINs and tokens).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn req(raw: &[u8]) -> Result<Request, HttpError> {
        read_request(&mut Cursor::new(raw.to_vec()), MAX_BODY_BYTES)
    }

    #[test]
    fn parses_request_line_headers_and_body() {
        let r = req(b"POST /a/b%20c?x=1&y=two+words HTTP/1.1\r\nHost: 10.0.0.2:49494\r\ncontent-LENGTH: 5\r\nSOAPACTION: \"urn:x#Play\"\r\n\r\nhelloEXTRA").unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/a/b%20c");
        assert_eq!(r.query, "x=1&y=two+words");
        assert_eq!(r.header("content-length"), Some("5"));
        assert_eq!(r.header("SoapAction"), Some("\"urn:x#Play\""));
        assert_eq!(r.body, b"hello");
        assert_eq!(
            r.query_pairs(),
            vec![("x".into(), "1".into()), ("y".into(), "two words".into())]
        );
    }

    #[test]
    fn bare_lf_and_absolute_form_and_star() {
        let r = req(b"GET http://1.2.3.4:80/desc.xml HTTP/1.0\nHost: x\n\n").unwrap();
        assert_eq!(r.path, "/desc.xml");
        let r = req(b"OPTIONS * HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(r.path, "*");
        let r = req(b"SUBSCRIBE /evt HTTP/1.1\r\nCALLBACK: <http://1.2.3.4:5/x>\r\n\r\n").unwrap();
        assert_eq!(r.method, "SUBSCRIBE");
    }

    #[test]
    fn rejects_bad_requests() {
        assert_eq!(
            req(b"GET / HTTP/1.1 extra\r\n\r\n"),
            Err(HttpError::BadRequest)
        );
        assert_eq!(req(b"get / HTTP/1.1\r\n\r\n"), Err(HttpError::BadRequest));
        assert_eq!(
            req(b"GET nopath HTTP/1.1\r\n\r\n"),
            Err(HttpError::BadRequest)
        );
        assert_eq!(
            req(b"GET / HTTP/2.0\r\n\r\n"),
            Err(HttpError::NotImplemented)
        );
        assert_eq!(req(b"GET / FTP\r\n\r\n"), Err(HttpError::BadRequest));
        assert_eq!(
            req(b"GET / HTTP/1.1\r\nNoColon\r\n\r\n"),
            Err(HttpError::BadRequest)
        );
        assert_eq!(
            req(b"GET / HTTP/1.1\r\nA: b\r\n folded\r\n\r\n"),
            Err(HttpError::BadRequest)
        );
        assert_eq!(
            req(b"GET / HTTP/1.1\r\nBad Name: x\r\n\r\n"),
            Err(HttpError::BadRequest)
        );
        assert_eq!(req(b""), Err(HttpError::Closed));
        assert_eq!(req(b"GET / HTTP/1.1\r\n"), Err(HttpError::BadRequest));
    }

    #[test]
    fn body_rules() {
        assert_eq!(
            req(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n"),
            Err(HttpError::LengthRequired)
        );
        assert_eq!(
            req(b"POST / HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\n"),
            Err(HttpError::NotImplemented)
        );
        assert_eq!(
            req(b"POST / HTTP/1.1\r\nContent-Length: 999999\r\n\r\n"),
            Err(HttpError::BodyTooLarge)
        );
        assert_eq!(
            req(b"POST / HTTP/1.1\r\nContent-Length: -1\r\n\r\n"),
            Err(HttpError::BadRequest)
        );
        assert_eq!(
            req(b"POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab"),
            Err(HttpError::BadRequest)
        );
        // Declared body longer than what arrives.
        assert_eq!(
            req(b"POST / HTTP/1.1\r\nContent-Length: 10\r\n\r\nabc"),
            Err(HttpError::BadRequest)
        );
        // No Content-Length → empty body.
        assert!(
            req(b"POST / HTTP/1.1\r\n\r\nignored")
                .unwrap()
                .body
                .is_empty()
        );
    }

    #[test]
    fn head_size_limit() {
        let mut big = b"GET / HTTP/1.1\r\nX: ".to_vec();
        big.extend(std::iter::repeat_n(b'a', MAX_HEAD_BYTES + 10));
        big.extend_from_slice(b"\r\n\r\n");
        assert_eq!(req(&big), Err(HttpError::HeadTooLarge));
        let mut many = b"GET / HTTP/1.1\r\n".to_vec();
        for i in 0..(MAX_HEADERS + 1) {
            many.extend_from_slice(format!("H{i}: v\r\n").as_bytes());
        }
        many.extend_from_slice(b"\r\n");
        assert_eq!(req(&many), Err(HttpError::HeadTooLarge));
    }

    #[test]
    fn garbage_never_panics() {
        // Deterministic pseudo-random garbage plus nasty fragments.
        let mut seed: u32 = 0x1234_5678;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let fragments: [&[u8]; 8] = [
            b"GET ",
            b"/",
            b" HTTP/1.1",
            b"\r\n",
            b"Content-Length: 3",
            b"%",
            b"\xff\xfe",
            b":",
        ];
        for _ in 0..2000 {
            let mut data = Vec::new();
            for _ in 0..(next() % 12) {
                if next() % 2 == 0 {
                    data.extend_from_slice(fragments[(next() % 8) as usize]);
                } else {
                    data.push((next() % 256) as u8);
                }
            }
            let _ = req(&data);
            let _ = parse_form(&String::from_utf8_lossy(&data));
            let _ = percent_decode(&String::from_utf8_lossy(&data), true);
            let _ = parse_http_url(&String::from_utf8_lossy(&data));
        }
    }

    #[test]
    fn response_serialisation_strips_crlf() {
        let r = Response::text(200, "hi")
            .header("X-Evil", "a\r\nSet-Cookie: x=1")
            .header("Content-Length", "999");
        let s = String::from_utf8(r.to_bytes(false)).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.contains("X-Evil: aSet-Cookie: x=1\r\n"));
        assert!(s.contains("Content-Length: 2\r\n"));
        assert!(!s.contains("999"));
        assert!(s.contains("Connection: close\r\n\r\nhi"));
        let head = String::from_utf8(r.to_bytes(true)).unwrap();
        assert!(head.ends_with("\r\n\r\n"));
        assert!(head.contains("Content-Length: 2"));
    }

    #[test]
    fn percent_coding() {
        assert_eq!(percent_decode("a%20b+c", true).unwrap(), "a b c");
        assert_eq!(percent_decode("a+b", false).unwrap(), "a+b");
        assert_eq!(percent_decode("100%", true).unwrap(), "100%");
        assert_eq!(percent_decode("%zz%4", true).unwrap(), "%zz%4");
        assert_eq!(percent_decode("%C3%A9", true).unwrap(), "é");
        assert_eq!(percent_decode("%ff", true), None);
        assert_eq!(percent_encode("a b/é~"), "a%20b%2F%C3%A9~");
        assert_eq!(
            percent_decode(&percent_encode("http://x:1/a?b=c&d"), false).unwrap(),
            "http://x:1/a?b=c&d"
        );
    }

    #[test]
    fn form_parsing() {
        let p = parse_form("a=1&&b&c=%41%42&bad=%ff&a=2");
        assert_eq!(
            p,
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "".into()),
                ("c".into(), "AB".into()),
                ("a".into(), "2".into())
            ]
        );
        assert_eq!(form_value(&p, "a"), Some("1"));
        assert_eq!(form_value(&p, "z"), None);
        let many = vec!["k=v"; 500].join("&");
        assert_eq!(parse_form(&many).len(), MAX_FORM_PAIRS);
    }

    #[test]
    fn http_url_parsing() {
        let u = parse_http_url("http://192.168.1.20:5000/upnp/event?x=1").unwrap();
        assert_eq!(u.addr, "192.168.1.20:5000".parse().unwrap());
        assert_eq!(u.path, "/upnp/event?x=1");
        assert_eq!(parse_http_url("http://10.0.0.1").unwrap().path, "/");
        assert_eq!(parse_http_url("http://10.0.0.1").unwrap().addr.port(), 80);
        assert!(parse_http_url("https://10.0.0.1/").is_none());
        assert!(parse_http_url("http://evil.example/").is_none());
        assert!(parse_http_url("http://10.0.0.1:0/").is_none());
        assert!(parse_http_url("http://10.0.0.1:99999/").is_none());
        assert!(parse_http_url("http://10.0.0.1/a b").is_none());
    }

    #[test]
    fn host_literal_check() {
        assert!(host_is_ip_literal("192.168.1.5:8099"));
        assert!(host_is_ip_literal("192.168.1.5"));
        assert!(host_is_ip_literal("[fe80::1]:8099"));
        assert!(host_is_ip_literal("localhost:8099"));
        assert!(!host_is_ip_literal("evil.example:8099"));
        assert!(!host_is_ip_literal(""));
    }

    #[test]
    fn random_ids() {
        let a = random_hex(16).unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, random_hex(16).unwrap());
        let u = uuid_v4().unwrap();
        assert!(is_uuid(&u), "{u}");
        assert_eq!(&u[14..15], "4");
        assert!(!is_uuid("not-a-uuid"));
        assert!(!is_uuid("1b4e28ba-2fa1-41d2-883f-0016d3cca42"));
        assert!(constant_time_eq(b"123456", b"123456"));
        assert!(!constant_time_eq(b"123456", b"123457"));
        assert!(!constant_time_eq(b"12345", b"123456"));
    }

    fn spawn_test_server(limits: Limits) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handler: Arc<Handler> = Arc::new(|r: &Request| {
            if r.path == "/slow" {
                thread::sleep(Duration::from_millis(600));
            }
            Response::text(200, format!("{} {} {}", r.method, r.path, r.body_text()))
        });
        spawn_server(listener, limits, handler).unwrap();
        addr
    }

    fn roundtrip(addr: SocketAddr, raw: &[u8]) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(raw).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    #[test]
    fn server_round_trip_and_errors() {
        let addr = spawn_test_server(Limits::default());
        let out = roundtrip(
            addr,
            b"POST /x HTTP/1.1\r\nHost: a\r\nContent-Length: 3\r\n\r\nabc",
        );
        assert!(out.starts_with("HTTP/1.1 200 OK"), "{out}");
        assert!(out.ends_with("POST /x abc"), "{out}");
        let out = roundtrip(addr, b"HEAD /x HTTP/1.1\r\n\r\n");
        assert!(out.ends_with("\r\n\r\n"), "{out}");
        let out = roundtrip(
            addr,
            b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
        );
        assert!(out.starts_with("HTTP/1.1 411"), "{out}");
        let out = roundtrip(addr, b"\x00\x01garbage\r\n\r\n");
        assert!(out.starts_with("HTTP/1.1 400"), "{out}");
    }

    #[test]
    fn server_times_out_slow_clients() {
        let addr = spawn_test_server(Limits {
            io_timeout: Duration::from_millis(300),
            ..Limits::default()
        });
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(b"GET / HTTP/1.1\r\n").unwrap(); // never finishes
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        assert!(out.starts_with("HTTP/1.1 408"), "{out}");
    }

    #[test]
    fn server_caps_concurrent_connections() {
        let addr = spawn_test_server(Limits {
            max_connections: 1,
            ..Limits::default()
        });
        let first = thread::spawn(move || roundtrip(addr, b"GET /slow HTTP/1.1\r\n\r\n"));
        thread::sleep(Duration::from_millis(200));
        let second = roundtrip(addr, b"GET /fast HTTP/1.1\r\n\r\n");
        assert!(second.starts_with("HTTP/1.1 503"), "{second}");
        assert!(first.join().unwrap().starts_with("HTTP/1.1 200"));
    }

    #[test]
    fn client_sends_and_reads_status() {
        let addr = spawn_test_server(Limits::default());
        let SocketAddr::V4(v4) = addr else { panic!() };
        let code = send_request(
            v4,
            "NOTIFY",
            "/cb",
            &[("NT", "upnp:event".into())],
            b"<x/>",
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(code, 200);
    }
}
