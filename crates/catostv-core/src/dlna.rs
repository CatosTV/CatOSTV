//! DLNA / UPnP AV **MediaRenderer:1** ("Cast to TV" from BubbleUPnP,
//! Windows "Cast to device", VLC, Kodi, …).
//!
//! Served by `catostv-castd` under `/dlna/`:
//!
//! | Path | Method | What |
//! |---|---|---|
//! | `/dlna/description.xml` | GET | device description |
//! | `/dlna/{AVTransport,RenderingControl,ConnectionManager}.xml` | GET | SCPDs |
//! | `/dlna/control/<service>` | POST | SOAP actions |
//! | `/dlna/event/<service>` | SUBSCRIBE / UNSUBSCRIBE | GENA eventing |
//!
//! Playback goes to an [`mpv::Player`](crate::mpv::Player); volume and mute
//! map to the default PipeWire sink through `wpctl` (via a [`Runner`]).
//!
//! Security: like every DLNA TV, the renderer plays media from anyone on the
//! LAN (disable with `catostv-castd --no-dlna`). It only accepts `http(s)`
//! URLs, passes them to mpv as one argv element/IPC string, requires the
//! `SOAPACTION` header (a custom header, so a web page cannot forge control
//! requests without a CORS preflight, which we never grant), refuses control
//! requests that carry an `Origin` header, and only sends event
//! notifications to the IP address that subscribed.
//!
//! Eventing: SUBSCRIBE/renew/UNSUBSCRIBE are implemented with SIDs and
//! timeouts (max 1800 s, at most 32 subscriptions). The daemon's event
//! thread sends the initial NOTIFY (`LastChange` for AVTransport and
//! RenderingControl, plain variables for ConnectionManager) and a new one
//! whenever the evented state changes (see [`Subscriptions::collect`]).

use crate::error::Result;
use crate::http::{self, HttpUrl, Request, Response};
use crate::mpv::{Player, is_playable_url};
use crate::process::{Cmd, Runner};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const DEVICE_TYPE: &str = "urn:schemas-upnp-org:device:MediaRenderer:1";
pub const DESCRIPTION_PATH: &str = "/dlna/description.xml";

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// `$XDG_STATE_HOME/catostv/cast-uuid` (default `~/.local/state/catostv/cast-uuid`).
pub fn uuid_path() -> PathBuf {
    crate::env::state_dir().join("cast-uuid")
}

/// Read the persisted device UUID, creating it on first use, so phones keep
/// recognising the TV across restarts.
pub fn load_or_create_uuid(path: &Path) -> Result<String> {
    if let Ok(text) = std::fs::read_to_string(path) {
        let t = text.trim();
        if http::is_uuid(t) {
            return Ok(t.to_ascii_lowercase());
        }
    }
    let uuid = http::uuid_v4()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, format!("{uuid}\n"))?;
    Ok(uuid)
}

/// "CatosTV" or "CatosTV (<hostname>)".
pub fn friendly_name_for(hostname: &str) -> String {
    let h: String = hostname
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    if h.is_empty() || h.eq_ignore_ascii_case("catostv") || h == "localhost" || h == "archiso" {
        "CatosTV".into()
    } else {
        format!("CatosTV ({h})")
    }
}

/// Friendly name from `/proc/sys/kernel/hostname`.
pub fn default_friendly_name() -> String {
    friendly_name_for(&std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default())
}

/// SSDP advertisement of the renderer root device.
pub fn advertisement(uuid: &str) -> crate::ssdp::Advertisement {
    crate::ssdp::Advertisement {
        uuid: uuid.to_string(),
        location_path: DESCRIPTION_PATH.into(),
        device_type: DEVICE_TYPE.into(),
        service_types: service_types(),
    }
}

// ---------------------------------------------------------------------------
// XML helpers
// ---------------------------------------------------------------------------

/// Escape text for XML element content and attribute values.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Characters not allowed in XML 1.0 are dropped.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// Decode the five named entities and numeric character references.
/// Unknown or malformed entities are kept literally.
pub fn xml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest.find(';').filter(|&j| j <= 12).and_then(|j| {
            let ent = &rest[1..j];
            let c = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => {
                    let num = if let Some(h) =
                        ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X"))
                    {
                        u32::from_str_radix(h, 16).ok()
                    } else if let Some(d) = ent.strip_prefix('#') {
                        d.parse::<u32>().ok()
                    } else {
                        None
                    };
                    num.and_then(char::from_u32)
                }
            };
            c.map(|c| (c, j))
        });
        match decoded {
            Some((c, j)) => {
                out.push(c);
                rest = &rest[j + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Text content of the first element whose local name is `name` (any
/// namespace prefix). Handles `<x/>` (empty), attributes, CDATA and
/// entities. Tolerant by design: this is not a full XML parser.
pub fn xml_element_text(xml: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(off) = xml[from..].find('<') {
        let start = from + off;
        let tag_body = &xml[start + 1..];
        from = start + 1;
        if tag_body.starts_with('/') || tag_body.starts_with('?') || tag_body.starts_with('!') {
            continue;
        }
        let tag_name_end = tag_body
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(tag_body.len());
        let full = &tag_body[..tag_name_end];
        let local = full.rsplit(':').next().unwrap_or(full);
        if local != name {
            continue;
        }
        let gt = tag_body.find('>')?;
        if tag_body[..gt].ends_with('/') {
            return Some(String::new());
        }
        let content_start = start + 1 + gt + 1;
        let close = format!("</{full}>");
        let end = xml[content_start..].find(&close)? + content_start;
        let raw = &xml[content_start..end];
        return Some(decode_content(raw));
    }
    None
}

fn decode_content(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(i) = rest.find("<![CDATA[") {
        out.push_str(&xml_unescape(&rest[..i]));
        let after = &rest[i + 9..];
        match after.find("]]>") {
            Some(j) => {
                out.push_str(&after[..j]);
                rest = &after[j + 3..];
            }
            None => {
                out.push_str(after);
                rest = "";
            }
        }
    }
    out.push_str(&xml_unescape(rest));
    out.trim().to_string()
}

/// Value of `attr="…"` on the first `<element …>` in `xml` (unescaped).
fn xml_attr(xml: &str, element: &str, attr: &str) -> Option<String> {
    let start = xml.find(&format!("<{element}"))?;
    let tag = &xml[start..start + xml[start..].find('>')?];
    let key = format!("{attr}=\"");
    let a = tag.find(&key)? + key.len();
    let b = tag[a..].find('"')? + a;
    Some(xml_unescape(&tag[a..b]))
}

/// A cheap well-formedness check used by tests: tags balance and nest.
pub fn xml_is_balanced(xml: &str) -> bool {
    let mut stack: Vec<&str> = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        let Some(j) = rest.find('>') else {
            return false;
        };
        let tag = &rest[..j];
        rest = &rest[j + 1..];
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            if stack.pop() != Some(name.trim()) {
                return false;
            }
        } else if !tag.ends_with('/') {
            stack.push(tag.split_whitespace().next().unwrap_or(""));
        }
    }
    stack.is_empty()
}

// ---------------------------------------------------------------------------
// Services and descriptions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Service {
    AvTransport,
    RenderingControl,
    ConnectionManager,
}

impl Service {
    pub const ALL: [Service; 3] = [
        Service::AvTransport,
        Service::RenderingControl,
        Service::ConnectionManager,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::AvTransport => "AVTransport",
            Self::RenderingControl => "RenderingControl",
            Self::ConnectionManager => "ConnectionManager",
        }
    }

    pub fn service_type(self) -> String {
        format!("urn:schemas-upnp-org:service:{}:1", self.name())
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.name() == s)
    }

    pub fn scpd_path(self) -> String {
        format!("/dlna/{}.xml", self.name())
    }
    pub fn control_path(self) -> String {
        format!("/dlna/control/{}", self.name())
    }
    pub fn event_path(self) -> String {
        format!("/dlna/event/{}", self.name())
    }
}

/// Service types for the SSDP advertisement.
pub fn service_types() -> Vec<String> {
    Service::ALL.iter().map(|s| s.service_type()).collect()
}

/// The MediaRenderer device description.
pub fn device_description(friendly_name: &str, uuid: &str) -> String {
    let mut services = String::new();
    for s in Service::ALL {
        services.push_str(&format!(
            "<service><serviceType>{}</serviceType><serviceId>urn:upnp-org:serviceId:{}</serviceId>\
             <SCPDURL>{}</SCPDURL><controlURL>{}</controlURL><eventSubURL>{}</eventSubURL></service>",
            s.service_type(),
            s.name(),
            s.scpd_path(),
            s.control_path(),
            s.event_path()
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<root xmlns=\"urn:schemas-upnp-org:device-1-0\" xmlns:dlna=\"urn:schemas-dlna-org:device-1-0\">\
<specVersion><major>1</major><minor>0</minor></specVersion>\
<device>\
<deviceType>{DEVICE_TYPE}</deviceType>\
<friendlyName>{}</friendlyName>\
<manufacturer>CatosTV</manufacturer>\
<modelDescription>CatosTV media renderer</modelDescription>\
<modelName>CatosTV</modelName>\
<modelNumber>{}</modelNumber>\
<UDN>uuid:{}</UDN>\
<dlna:X_DLNADOC>DMR-1.50</dlna:X_DLNADOC>\
<serviceList>{services}</serviceList>\
</device>\
</root>\n",
        xml_escape(friendly_name),
        xml_escape(crate::env::version()),
        xml_escape(uuid)
    )
}

struct Var {
    name: &'static str,
    ty: &'static str,
    events: bool,
    allowed: &'static [&'static str],
    range: Option<(u32, u32)>,
}

const fn var(name: &'static str, ty: &'static str) -> Var {
    Var {
        name,
        ty,
        events: false,
        allowed: &[],
        range: None,
    }
}

type ArgSpec = (&'static str, bool, &'static str); // (name, is_out, related var)

fn scpd(actions: &[(&str, &[ArgSpec])], vars: &[Var]) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<scpd xmlns=\"urn:schemas-upnp-org:service-1-0\">\
<specVersion><major>1</major><minor>0</minor></specVersion><actionList>",
    );
    for (name, args) in actions {
        s.push_str(&format!("<action><name>{name}</name>"));
        if !args.is_empty() {
            s.push_str("<argumentList>");
            for (arg, out, rel) in args.iter() {
                s.push_str(&format!(
                    "<argument><name>{arg}</name><direction>{}</direction>\
                     <relatedStateVariable>{rel}</relatedStateVariable></argument>",
                    if *out { "out" } else { "in" }
                ));
            }
            s.push_str("</argumentList>");
        }
        s.push_str("</action>");
    }
    s.push_str("</actionList><serviceStateTable>");
    for v in vars {
        s.push_str(&format!(
            "<stateVariable sendEvents=\"{}\"><name>{}</name><dataType>{}</dataType>",
            if v.events { "yes" } else { "no" },
            v.name,
            v.ty
        ));
        if !v.allowed.is_empty() {
            s.push_str("<allowedValueList>");
            for a in v.allowed {
                s.push_str(&format!("<allowedValue>{a}</allowedValue>"));
            }
            s.push_str("</allowedValueList>");
        }
        if let Some((min, max)) = v.range {
            s.push_str(&format!(
                "<allowedValueRange><minimum>{min}</minimum><maximum>{max}</maximum><step>1</step></allowedValueRange>"
            ));
        }
        s.push_str("</stateVariable>");
    }
    s.push_str("</serviceStateTable></scpd>\n");
    s
}

const IID: ArgSpec = ("InstanceID", false, "A_ARG_TYPE_InstanceID");

fn avt_scpd() -> String {
    let last_change = Var {
        events: true,
        ..var("LastChange", "string")
    };
    scpd(
        &[
            (
                "SetAVTransportURI",
                &[
                    IID,
                    ("CurrentURI", false, "AVTransportURI"),
                    ("CurrentURIMetaData", false, "AVTransportURIMetaData"),
                ],
            ),
            (
                "GetMediaInfo",
                &[
                    IID,
                    ("NrTracks", true, "NumberOfTracks"),
                    ("MediaDuration", true, "CurrentMediaDuration"),
                    ("CurrentURI", true, "AVTransportURI"),
                    ("CurrentURIMetaData", true, "AVTransportURIMetaData"),
                    ("NextURI", true, "NextAVTransportURI"),
                    ("NextURIMetaData", true, "NextAVTransportURIMetaData"),
                    ("PlayMedium", true, "PlaybackStorageMedium"),
                    ("RecordMedium", true, "RecordStorageMedium"),
                    ("WriteStatus", true, "RecordMediumWriteStatus"),
                ],
            ),
            (
                "GetTransportInfo",
                &[
                    IID,
                    ("CurrentTransportState", true, "TransportState"),
                    ("CurrentTransportStatus", true, "TransportStatus"),
                    ("CurrentSpeed", true, "TransportPlaySpeed"),
                ],
            ),
            (
                "GetPositionInfo",
                &[
                    IID,
                    ("Track", true, "CurrentTrack"),
                    ("TrackDuration", true, "CurrentTrackDuration"),
                    ("TrackMetaData", true, "CurrentTrackMetaData"),
                    ("TrackURI", true, "CurrentTrackURI"),
                    ("RelTime", true, "RelativeTimePosition"),
                    ("AbsTime", true, "AbsoluteTimePosition"),
                    ("RelCount", true, "RelativeCounterPosition"),
                    ("AbsCount", true, "AbsoluteCounterPosition"),
                ],
            ),
            (
                "GetDeviceCapabilities",
                &[
                    IID,
                    ("PlayMedia", true, "PossiblePlaybackStorageMedia"),
                    ("RecMedia", true, "PossibleRecordStorageMedia"),
                    ("RecQualityModes", true, "PossibleRecordQualityModes"),
                ],
            ),
            (
                "GetTransportSettings",
                &[
                    IID,
                    ("PlayMode", true, "CurrentPlayMode"),
                    ("RecQualityMode", true, "CurrentRecordQualityMode"),
                ],
            ),
            ("Stop", &[IID]),
            ("Play", &[IID, ("Speed", false, "TransportPlaySpeed")]),
            ("Pause", &[IID]),
            (
                "Seek",
                &[
                    IID,
                    ("Unit", false, "A_ARG_TYPE_SeekMode"),
                    ("Target", false, "A_ARG_TYPE_SeekTarget"),
                ],
            ),
            ("Next", &[IID]),
            ("Previous", &[IID]),
            (
                "GetCurrentTransportActions",
                &[IID, ("Actions", true, "CurrentTransportActions")],
            ),
        ],
        &[
            Var {
                allowed: &[
                    "STOPPED",
                    "PLAYING",
                    "PAUSED_PLAYBACK",
                    "TRANSITIONING",
                    "NO_MEDIA_PRESENT",
                ],
                ..var("TransportState", "string")
            },
            Var {
                allowed: &["OK", "ERROR_OCCURRED"],
                ..var("TransportStatus", "string")
            },
            var("PlaybackStorageMedium", "string"),
            var("RecordStorageMedium", "string"),
            var("PossiblePlaybackStorageMedia", "string"),
            var("PossibleRecordStorageMedia", "string"),
            Var {
                allowed: &["NORMAL"],
                ..var("CurrentPlayMode", "string")
            },
            Var {
                allowed: &["1"],
                ..var("TransportPlaySpeed", "string")
            },
            var("RecordMediumWriteStatus", "string"),
            var("CurrentRecordQualityMode", "string"),
            var("PossibleRecordQualityModes", "string"),
            var("NumberOfTracks", "ui4"),
            var("CurrentTrack", "ui4"),
            var("CurrentTrackDuration", "string"),
            var("CurrentMediaDuration", "string"),
            var("CurrentTrackMetaData", "string"),
            var("CurrentTrackURI", "string"),
            var("AVTransportURI", "string"),
            var("AVTransportURIMetaData", "string"),
            var("NextAVTransportURI", "string"),
            var("NextAVTransportURIMetaData", "string"),
            var("RelativeTimePosition", "string"),
            var("AbsoluteTimePosition", "string"),
            var("RelativeCounterPosition", "i4"),
            var("AbsoluteCounterPosition", "i4"),
            var("CurrentTransportActions", "string"),
            last_change,
            Var {
                allowed: &["REL_TIME", "ABS_TIME"],
                ..var("A_ARG_TYPE_SeekMode", "string")
            },
            var("A_ARG_TYPE_SeekTarget", "string"),
            var("A_ARG_TYPE_InstanceID", "ui4"),
        ],
    )
}

const CHANNEL: ArgSpec = ("Channel", false, "A_ARG_TYPE_Channel");

fn rcs_scpd() -> String {
    scpd(
        &[
            (
                "ListPresets",
                &[IID, ("CurrentPresetNameList", true, "PresetNameList")],
            ),
            (
                "SelectPreset",
                &[IID, ("PresetName", false, "A_ARG_TYPE_PresetName")],
            ),
            (
                "GetVolume",
                &[IID, CHANNEL, ("CurrentVolume", true, "Volume")],
            ),
            (
                "SetVolume",
                &[IID, CHANNEL, ("DesiredVolume", false, "Volume")],
            ),
            ("GetMute", &[IID, CHANNEL, ("CurrentMute", true, "Mute")]),
            ("SetMute", &[IID, CHANNEL, ("DesiredMute", false, "Mute")]),
        ],
        &[
            Var {
                events: true,
                ..var("LastChange", "string")
            },
            var("PresetNameList", "string"),
            Var {
                allowed: &["FactoryDefaults"],
                ..var("A_ARG_TYPE_PresetName", "string")
            },
            var("A_ARG_TYPE_InstanceID", "ui4"),
            Var {
                allowed: &["Master"],
                ..var("A_ARG_TYPE_Channel", "string")
            },
            Var {
                range: Some((0, 100)),
                ..var("Volume", "ui2")
            },
            var("Mute", "boolean"),
        ],
    )
}

fn cms_scpd() -> String {
    scpd(
        &[
            (
                "GetProtocolInfo",
                &[
                    ("Source", true, "SourceProtocolInfo"),
                    ("Sink", true, "SinkProtocolInfo"),
                ],
            ),
            (
                "GetCurrentConnectionIDs",
                &[("ConnectionIDs", true, "CurrentConnectionIDs")],
            ),
            (
                "GetCurrentConnectionInfo",
                &[
                    ("ConnectionID", false, "A_ARG_TYPE_ConnectionID"),
                    ("RcsID", true, "A_ARG_TYPE_RcsID"),
                    ("AVTransportID", true, "A_ARG_TYPE_AVTransportID"),
                    ("ProtocolInfo", true, "A_ARG_TYPE_ProtocolInfo"),
                    (
                        "PeerConnectionManager",
                        true,
                        "A_ARG_TYPE_ConnectionManager",
                    ),
                    ("PeerConnectionID", true, "A_ARG_TYPE_ConnectionID"),
                    ("Direction", true, "A_ARG_TYPE_Direction"),
                    ("Status", true, "A_ARG_TYPE_ConnectionStatus"),
                ],
            ),
        ],
        &[
            Var {
                events: true,
                ..var("SourceProtocolInfo", "string")
            },
            Var {
                events: true,
                ..var("SinkProtocolInfo", "string")
            },
            Var {
                events: true,
                ..var("CurrentConnectionIDs", "string")
            },
            Var {
                allowed: &[
                    "OK",
                    "ContentFormatMismatch",
                    "InsufficientBandwidth",
                    "UnreliableChannel",
                    "Unknown",
                ],
                ..var("A_ARG_TYPE_ConnectionStatus", "string")
            },
            var("A_ARG_TYPE_ConnectionManager", "string"),
            Var {
                allowed: &["Input", "Output"],
                ..var("A_ARG_TYPE_Direction", "string")
            },
            var("A_ARG_TYPE_ProtocolInfo", "string"),
            var("A_ARG_TYPE_ConnectionID", "i4"),
            var("A_ARG_TYPE_AVTransportID", "i4"),
            var("A_ARG_TYPE_RcsID", "i4"),
        ],
    )
}

/// SCPD document for a service.
pub fn scpd_for(service: Service) -> String {
    match service {
        Service::AvTransport => avt_scpd(),
        Service::RenderingControl => rcs_scpd(),
        Service::ConnectionManager => cms_scpd(),
    }
}

/// MIME types announced in `SinkProtocolInfo`. Explicit types first (some
/// control points only match exact types), then wildcards.
pub const SINK_MIME_TYPES: &[&str] = &[
    "video/mp4",
    "video/x-matroska",
    "video/webm",
    "video/mpeg",
    "video/mp2t",
    "video/quicktime",
    "video/x-msvideo",
    "video/avi",
    "video/x-ms-wmv",
    "video/x-flv",
    "video/3gpp",
    "audio/mpeg",
    "audio/mp4",
    "audio/aac",
    "audio/flac",
    "audio/x-flac",
    "audio/ogg",
    "audio/opus",
    "audio/wav",
    "audio/x-wav",
    "audio/L16",
    "audio/x-ms-wma",
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/bmp",
    "application/vnd.apple.mpegurl",
    "application/x-mpegurl",
    "video/*",
    "audio/*",
    "image/*",
];

pub fn sink_protocol_info() -> String {
    SINK_MIME_TYPES
        .iter()
        .map(|m| format!("http-get:*:{m}:*"))
        .collect::<Vec<_>>()
        .join(",")
}

/// True if a MIME type (from DIDL-Lite `protocolInfo`) can be played.
pub fn mime_supported(mime: &str) -> bool {
    let m = mime.trim().to_ascii_lowercase();
    m.starts_with("video/")
        || m.starts_with("audio/")
        || m.starts_with("image/")
        || matches!(
            m.as_str(),
            "application/vnd.apple.mpegurl"
                | "application/x-mpegurl"
                | "application/octet-stream"
                | "application/ogg"
                | "application/dash+xml"
                | "*"
        )
}

/// MIME type from the first `<res protocolInfo="http-get:*:MIME:…">` of a
/// DIDL-Lite document.
pub fn didl_mime(didl: &str) -> Option<String> {
    let info = xml_attr(didl, "res", "protocolInfo")?;
    info.split(':').nth(2).map(|s| s.trim().to_string())
}

// ---------------------------------------------------------------------------
// SOAP
// ---------------------------------------------------------------------------

/// Split a `SOAPACTION` header (`"urn:…:AVTransport:1#Play"`) into
/// (service type, action).
pub fn parse_soap_action(header: &str) -> Option<(String, String)> {
    let h = header.trim().trim_matches('"');
    let (ty, action) = h.rsplit_once('#')?;
    if ty.is_empty()
        || action.is_empty()
        || action.len() > 64
        || !action
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    Some((ty.to_string(), action.to_string()))
}

/// SOAP success envelope; argument values are XML-escaped.
pub fn soap_response(service_type: &str, action: &str, args: &[(String, String)]) -> String {
    let mut inner = String::new();
    for (k, v) in args {
        inner.push_str(&format!("<{k}>{}</{k}>", xml_escape(v)));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
<s:Body><u:{action}Response xmlns:u=\"{}\">{inner}</u:{action}Response></s:Body></s:Envelope>\n",
        xml_escape(service_type)
    )
}

/// UPnP error codes used by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpnpError {
    InvalidAction,
    InvalidArgs,
    ActionFailed,
    TransitionNotAvailable,
    SeekModeNotSupported,
    IllegalSeekTarget,
    IllegalMimeType,
    ResourceNotFound,
    InvalidInstanceId,
    InvalidConnectionReference,
}

impl UpnpError {
    pub fn code(self) -> u16 {
        match self {
            Self::InvalidAction => 401,
            Self::InvalidArgs => 402,
            Self::ActionFailed => 501,
            Self::TransitionNotAvailable => 701,
            Self::InvalidConnectionReference => 706,
            Self::SeekModeNotSupported => 710,
            Self::IllegalSeekTarget => 711,
            Self::IllegalMimeType => 714,
            Self::ResourceNotFound => 716,
            Self::InvalidInstanceId => 718,
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::InvalidAction => "Invalid Action",
            Self::InvalidArgs => "Invalid Args",
            Self::ActionFailed => "Action Failed",
            Self::TransitionNotAvailable => "Transition not available",
            Self::InvalidConnectionReference => "Invalid connection reference",
            Self::SeekModeNotSupported => "Seek mode not supported",
            Self::IllegalSeekTarget => "Illegal seek target",
            Self::IllegalMimeType => "Illegal MIME-type",
            Self::ResourceNotFound => "Resource not found",
            Self::InvalidInstanceId => "Invalid InstanceID",
        }
    }
}

/// SOAP fault envelope (sent with HTTP 500).
pub fn soap_fault(err: UpnpError) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
<s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring>\
<detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>{}</errorCode>\
<errorDescription>{}</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>\n",
        err.code(),
        err.description()
    )
}

// ---------------------------------------------------------------------------
// Time strings
// ---------------------------------------------------------------------------

/// `H:MM:SS` (UPnP duration format).
pub fn format_hms(seconds: f64) -> String {
    let s = if seconds.is_finite() && seconds > 0.0 {
        seconds as u64
    } else {
        0
    };
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// Parse `H+:MM:SS[.fff]` (also `MM:SS` and plain seconds). `None` for
/// garbage or negative values.
pub fn parse_hms(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || s.len() > 32 || s.starts_with('-') {
        return None;
    }
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut total = 0.0;
    for (i, p) in parts.iter().enumerate() {
        let last = i == parts.len() - 1;
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit() || (last && b == b'.')) {
            return None;
        }
        let v: f64 = p.parse().ok()?;
        if !last && parts.len() > 1 && i > 0 && v >= 60.0 {
            return None;
        }
        total = total * 60.0 + v;
    }
    total.is_finite().then_some(total)
}

// ---------------------------------------------------------------------------
// Renderer state machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    NoMedia,
    Stopped,
    Playing,
    Paused,
}

impl TransportState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoMedia => "NO_MEDIA_PRESENT",
            Self::Stopped => "STOPPED",
            Self::Playing => "PLAYING",
            Self::Paused => "PAUSED_PLAYBACK",
        }
    }
}

const SINK: &str = "@DEFAULT_AUDIO_SINK@";

/// The MediaRenderer: transport state plus the player and audio runner.
pub struct Renderer {
    player: Box<dyn Player>,
    audio: Box<dyn Runner + Send>,
    state: TransportState,
    uri: String,
    metadata: String,
    /// URI currently loaded in the player (may differ after a new
    /// SetAVTransportURI until Play).
    loaded: Option<String>,
    position: Option<f64>,
    duration: Option<f64>,
    /// Set when the player window went away (end of media or Stop).
    ended: bool,
}

type ActionResult = std::result::Result<Vec<(String, String)>, UpnpError>;

fn out(pairs: &[(&str, String)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

impl Renderer {
    pub fn new(player: Box<dyn Player>, audio: Box<dyn Runner + Send>) -> Self {
        Self {
            player,
            audio,
            state: TransportState::NoMedia,
            uri: String::new(),
            metadata: String::new(),
            loaded: None,
            position: None,
            duration: None,
            ended: false,
        }
    }

    /// True once after the player closed (media ended or Stop). The daemon
    /// uses it to bring the CatosTV desktop back instead of leaving the user
    /// on an empty App desktop.
    pub fn take_ended(&mut self) -> bool {
        std::mem::take(&mut self.ended)
    }

    fn mark_closed(&mut self) {
        if self.loaded.take().is_some() {
            self.ended = true;
        }
    }

    pub fn state(&self) -> TransportState {
        self.state
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Pull fresh status from the player (call before answering queries and
    /// periodically from the event thread). Detects the end of playback and
    /// pauses made with a keyboard/remote inside mpv.
    pub fn refresh(&mut self) {
        let st = self.player.status();
        if !st.running {
            self.mark_closed();
            self.position = None;
            if matches!(self.state, TransportState::Playing | TransportState::Paused) {
                self.state = TransportState::Stopped;
            }
            return;
        }
        self.position = st.position;
        if st.duration.is_some() {
            self.duration = st.duration;
        }
        match self.state {
            TransportState::Playing if st.paused => self.state = TransportState::Paused,
            TransportState::Paused if !st.paused => self.state = TransportState::Playing,
            _ => {}
        }
    }

    /// Stop playback (daemon shutdown).
    pub fn shutdown(&mut self) {
        self.player.stop();
    }

    fn actions(&self) -> &'static str {
        match self.state {
            TransportState::NoMedia => "",
            TransportState::Stopped => "Play",
            TransportState::Playing => "Pause,Stop,Seek",
            TransportState::Paused => "Play,Stop,Seek",
        }
    }

    /// Execute one SOAP action. `body` is the raw SOAP request.
    pub fn handle(&mut self, service: Service, action: &str, body: &str) -> ActionResult {
        let arg = |name: &str| xml_element_text(body, name);
        if service != Service::ConnectionManager {
            match arg("InstanceID").as_deref().map(str::trim) {
                Some("0") => {}
                Some(s) if s.parse::<u32>().is_ok() => return Err(UpnpError::InvalidInstanceId),
                _ => return Err(UpnpError::InvalidArgs),
            }
        }
        match service {
            Service::AvTransport => self.avt(action, &arg),
            Service::RenderingControl => self.rcs(action, &arg),
            Service::ConnectionManager => cms(action, &arg),
        }
    }

    fn avt(&mut self, action: &str, arg: &dyn Fn(&str) -> Option<String>) -> ActionResult {
        use TransportState::*;
        match action {
            "SetAVTransportURI" => {
                let uri = arg("CurrentURI").ok_or(UpnpError::InvalidArgs)?;
                let meta = arg("CurrentURIMetaData").unwrap_or_default();
                if uri.is_empty() {
                    self.player.stop();
                    self.uri.clear();
                    self.metadata.clear();
                    self.mark_closed();
                    self.state = NoMedia;
                    return Ok(vec![]);
                }
                if !is_playable_url(&uri) {
                    return Err(UpnpError::ResourceNotFound);
                }
                if let Some(mime) = didl_mime(&meta)
                    && !mime_supported(&mime)
                {
                    return Err(UpnpError::IllegalMimeType);
                }
                let was_active = matches!(self.state, Playing | Paused);
                self.uri = uri;
                self.metadata = meta;
                self.duration = None;
                self.position = None;
                if was_active && self.player.status().running {
                    // Swap the media without closing the window; Play starts it.
                    self.player
                        .load(&self.uri, true)
                        .map_err(|_| UpnpError::ActionFailed)?;
                    self.loaded = Some(self.uri.clone());
                }
                self.state = Stopped;
                Ok(vec![])
            }
            "SetNextAVTransportURI" | "Next" | "Previous" => {
                if action == "SetNextAVTransportURI" {
                    Err(UpnpError::InvalidAction)
                } else {
                    Err(UpnpError::TransitionNotAvailable)
                }
            }
            "Play" => {
                if self.uri.is_empty() {
                    return Err(UpnpError::TransitionNotAvailable);
                }
                self.refresh();
                if self.loaded.as_deref() == Some(self.uri.as_str()) && self.player.status().running
                {
                    self.player
                        .set_paused(false)
                        .map_err(|_| UpnpError::ActionFailed)?;
                } else {
                    self.player
                        .load(&self.uri, false)
                        .map_err(|_| UpnpError::ActionFailed)?;
                    self.loaded = Some(self.uri.clone());
                }
                self.state = Playing;
                Ok(vec![])
            }
            "Pause" => {
                self.refresh();
                match self.state {
                    Playing | Paused => {
                        self.player
                            .set_paused(true)
                            .map_err(|_| UpnpError::ActionFailed)?;
                        self.state = Paused;
                        Ok(vec![])
                    }
                    _ => Err(UpnpError::TransitionNotAvailable),
                }
            }
            "Stop" => {
                self.player.stop();
                self.mark_closed();
                self.position = None;
                if self.state != NoMedia {
                    self.state = Stopped;
                }
                Ok(vec![])
            }
            "Seek" => {
                let unit = arg("Unit").ok_or(UpnpError::InvalidArgs)?;
                let target = arg("Target").ok_or(UpnpError::InvalidArgs)?;
                if unit != "REL_TIME" && unit != "ABS_TIME" {
                    return Err(UpnpError::SeekModeNotSupported);
                }
                let secs = parse_hms(&target).ok_or(UpnpError::IllegalSeekTarget)?;
                self.refresh();
                if !matches!(self.state, Playing | Paused) {
                    return Err(UpnpError::TransitionNotAvailable);
                }
                self.player
                    .seek(secs)
                    .map_err(|_| UpnpError::ActionFailed)?;
                Ok(vec![])
            }
            "GetTransportInfo" => {
                self.refresh();
                Ok(out(&[
                    ("CurrentTransportState", self.state.as_str().into()),
                    ("CurrentTransportStatus", "OK".into()),
                    ("CurrentSpeed", "1".into()),
                ]))
            }
            "GetPositionInfo" => {
                self.refresh();
                let has = !self.uri.is_empty();
                let pos = format_hms(self.position.unwrap_or(0.0));
                Ok(out(&[
                    ("Track", if has { "1" } else { "0" }.into()),
                    ("TrackDuration", format_hms(self.duration.unwrap_or(0.0))),
                    ("TrackMetaData", self.metadata.clone()),
                    ("TrackURI", self.uri.clone()),
                    ("RelTime", pos.clone()),
                    ("AbsTime", pos),
                    ("RelCount", "2147483647".into()),
                    ("AbsCount", "2147483647".into()),
                ]))
            }
            "GetMediaInfo" => {
                self.refresh();
                let has = !self.uri.is_empty();
                Ok(out(&[
                    ("NrTracks", if has { "1" } else { "0" }.into()),
                    ("MediaDuration", format_hms(self.duration.unwrap_or(0.0))),
                    ("CurrentURI", self.uri.clone()),
                    ("CurrentURIMetaData", self.metadata.clone()),
                    ("NextURI", String::new()),
                    ("NextURIMetaData", String::new()),
                    ("PlayMedium", if has { "NETWORK" } else { "NONE" }.into()),
                    ("RecordMedium", "NOT_IMPLEMENTED".into()),
                    ("WriteStatus", "NOT_IMPLEMENTED".into()),
                ]))
            }
            "GetTransportSettings" => Ok(out(&[
                ("PlayMode", "NORMAL".into()),
                ("RecQualityMode", "NOT_IMPLEMENTED".into()),
            ])),
            "GetCurrentTransportActions" => {
                self.refresh();
                Ok(out(&[("Actions", self.actions().into())]))
            }
            "GetDeviceCapabilities" => Ok(out(&[
                ("PlayMedia", "NETWORK".into()),
                ("RecMedia", "NOT_IMPLEMENTED".into()),
                ("RecQualityModes", "NOT_IMPLEMENTED".into()),
            ])),
            _ => Err(UpnpError::InvalidAction),
        }
    }

    fn volume(&mut self) -> Result<crate::audio::VolumeState> {
        crate::audio::get(self.audio.as_mut())
    }

    fn rcs(&mut self, action: &str, arg: &dyn Fn(&str) -> Option<String>) -> ActionResult {
        let check_channel = || match arg("Channel").as_deref() {
            Some("Master") => Ok(()),
            _ => Err(UpnpError::InvalidArgs),
        };
        match action {
            "ListPresets" => Ok(out(&[("CurrentPresetNameList", "FactoryDefaults".into())])),
            "SelectPreset" => match arg("PresetName").as_deref() {
                Some("FactoryDefaults") => Ok(vec![]),
                _ => Err(UpnpError::InvalidArgs),
            },
            "GetVolume" => {
                check_channel()?;
                let v = self.volume().map_err(|_| UpnpError::ActionFailed)?;
                Ok(out(&[("CurrentVolume", v.percent.to_string())]))
            }
            "GetMute" => {
                check_channel()?;
                let v = self.volume().map_err(|_| UpnpError::ActionFailed)?;
                Ok(out(&[(
                    "CurrentMute",
                    if v.muted { "1" } else { "0" }.into(),
                )]))
            }
            "SetVolume" => {
                check_channel()?;
                let v: u8 = arg("DesiredVolume")
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .filter(|v| *v <= 100)
                    .ok_or(UpnpError::InvalidArgs)? as u8;
                set_volume(self.audio.as_mut(), v).map_err(|_| UpnpError::ActionFailed)?;
                Ok(vec![])
            }
            "SetMute" => {
                check_channel()?;
                let m = match arg("DesiredMute").as_deref().map(str::trim) {
                    Some("1") | Some("true") | Some("yes") => true,
                    Some("0") | Some("false") | Some("no") => false,
                    _ => return Err(UpnpError::InvalidArgs),
                };
                set_mute(self.audio.as_mut(), m).map_err(|_| UpnpError::ActionFailed)?;
                Ok(vec![])
            }
            _ => Err(UpnpError::InvalidAction),
        }
    }

    /// Evented AVTransport state (LastChange body, unescaped).
    pub fn avt_last_change(&self) -> String {
        let v = |name: &str, val: &str| format!("<{name} val=\"{}\"/>", xml_escape(val));
        let has = !self.uri.is_empty();
        let dur = format_hms(self.duration.unwrap_or(0.0));
        format!(
            "<Event xmlns=\"urn:schemas-upnp-org:metadata-1-0/AVT/\"><InstanceID val=\"0\">{}{}{}{}{}{}{}{}{}{}{}{}</InstanceID></Event>",
            v("TransportState", self.state.as_str()),
            v("TransportStatus", "OK"),
            v("CurrentTransportActions", self.actions()),
            v("AVTransportURI", &self.uri),
            v("AVTransportURIMetaData", &self.metadata),
            v("CurrentTrackURI", &self.uri),
            v("CurrentTrackMetaData", &self.metadata),
            v("CurrentMediaDuration", &dur),
            v("CurrentTrackDuration", &dur),
            v("NumberOfTracks", if has { "1" } else { "0" }),
            v("CurrentPlayMode", "NORMAL"),
            v("TransportPlaySpeed", "1"),
        )
    }

    /// Evented RenderingControl state (reads the volume via wpctl).
    pub fn rcs_last_change(&mut self) -> Option<String> {
        let vol = self.volume().ok()?;
        Some(format!(
            "<Event xmlns=\"urn:schemas-upnp-org:metadata-1-0/RCS/\"><InstanceID val=\"0\">\
<Volume channel=\"Master\" val=\"{}\"/><Mute channel=\"Master\" val=\"{}\"/>\
<PresetNameList val=\"FactoryDefaults\"/></InstanceID></Event>",
            vol.percent,
            if vol.muted { 1 } else { 0 }
        ))
    }
}

fn cms(action: &str, arg: &dyn Fn(&str) -> Option<String>) -> ActionResult {
    match action {
        "GetProtocolInfo" => Ok(out(&[
            ("Source", String::new()),
            ("Sink", sink_protocol_info()),
        ])),
        "GetCurrentConnectionIDs" => Ok(out(&[("ConnectionIDs", "0".into())])),
        "GetCurrentConnectionInfo" => match arg("ConnectionID").as_deref().map(str::trim) {
            Some("0") => Ok(out(&[
                ("RcsID", "0".into()),
                ("AVTransportID", "0".into()),
                ("ProtocolInfo", String::new()),
                ("PeerConnectionManager", String::new()),
                ("PeerConnectionID", "-1".into()),
                ("Direction", "Input".into()),
                ("Status", "OK".into()),
            ])),
            Some(_) => Err(UpnpError::InvalidConnectionReference),
            None => Err(UpnpError::InvalidArgs),
        },
        _ => Err(UpnpError::InvalidAction),
    }
}

/// `wpctl set-volume @DEFAULT_AUDIO_SINK@ 0.NN` (0–100 %).
pub fn set_volume(r: &mut dyn Runner, percent: u8) -> Result<()> {
    let p = percent.min(100);
    r.run_ok(&Cmd::new("wpctl").args([
        "set-volume".to_string(),
        SINK.to_string(),
        format!("{}.{:02}", p / 100, p % 100),
    ]))?;
    Ok(())
}

/// `wpctl set-mute @DEFAULT_AUDIO_SINK@ 1|0`.
pub fn set_mute(r: &mut dyn Runner, muted: bool) -> Result<()> {
    r.run_ok(&Cmd::new("wpctl").args(["set-mute", SINK, if muted { "1" } else { "0" }]))?;
    Ok(())
}

/// GENA property set wrapping evented variables (values escaped).
pub fn property_set(vars: &[(&str, String)]) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<e:propertyset xmlns:e=\"urn:schemas-upnp-org:event-1-0\">",
    );
    for (k, v) in vars {
        s.push_str(&format!(
            "<e:property><{k}>{}</{k}></e:property>",
            xml_escape(v)
        ));
    }
    s.push_str("</e:propertyset>\n");
    s
}

/// The ConnectionManager event body (static).
pub fn cms_event_body() -> String {
    property_set(&[
        ("SourceProtocolInfo", String::new()),
        ("SinkProtocolInfo", sink_protocol_info()),
        ("CurrentConnectionIDs", "0".into()),
    ])
}

// ---------------------------------------------------------------------------
// GENA subscriptions
// ---------------------------------------------------------------------------

pub const MAX_SUBSCRIPTIONS: usize = 32;
pub const MAX_SUBSCRIPTION_SECS: u64 = 1800;
const MIN_SUBSCRIPTION_SECS: u64 = 60;
const MAX_NOTIFY_FAILURES: u8 = 3;

#[derive(Debug, Clone)]
struct Subscription {
    sid: String,
    service: Service,
    callback: HttpUrl,
    expires: Instant,
    seq: u32,
    last_body: Option<String>,
    failures: u8,
}

/// One NOTIFY to send (outside any lock).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub sid: String,
    pub callback: HttpUrl,
    pub seq: u32,
    pub body: String,
}

#[derive(Debug, Default)]
pub struct Subscriptions {
    subs: Vec<Subscription>,
}

/// `TIMEOUT: Second-N` / `infinite` → seconds, clamped to 60..=1800.
pub fn parse_timeout(header: Option<&str>) -> u64 {
    let n = header
        .and_then(|h| h.trim().strip_prefix("Second-"))
        .and_then(|n| n.trim().parse::<u64>().ok())
        .unwrap_or(MAX_SUBSCRIPTION_SECS);
    n.clamp(MIN_SUBSCRIPTION_SECS, MAX_SUBSCRIPTION_SECS)
}

/// `CALLBACK: <http://a/b><http://c/d>` → URLs (IPv4 literals only).
pub fn parse_callbacks(header: &str) -> Vec<HttpUrl> {
    header
        .split('<')
        .filter_map(|part| part.split_once('>').map(|(u, _)| u))
        .filter_map(http::parse_http_url)
        .take(4)
        .collect()
}

impl Subscriptions {
    pub fn len(&self) -> usize {
        self.subs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }

    pub fn has_service(&self, service: Service) -> bool {
        self.subs.iter().any(|s| s.service == service)
    }

    /// New subscription. The callback must point at the subscriber's own
    /// address (no third-party targets). Errors are HTTP status codes.
    #[allow(clippy::too_many_arguments)]
    pub fn subscribe(
        &mut self,
        service: Service,
        callback_header: Option<&str>,
        nt: Option<&str>,
        timeout: Option<&str>,
        peer: Option<Ipv4Addr>,
        sid: String,
        now: Instant,
    ) -> std::result::Result<u64, u16> {
        if nt.map(str::trim) != Some("upnp:event") {
            return Err(412);
        }
        let peer = peer.ok_or(412u16)?;
        let callback = parse_callbacks(callback_header.ok_or(412u16)?)
            .into_iter()
            .find(|u| *u.addr.ip() == peer)
            .ok_or(412u16)?;
        self.expire(now);
        if self.subs.len() >= MAX_SUBSCRIPTIONS {
            return Err(503);
        }
        let secs = parse_timeout(timeout);
        self.subs.push(Subscription {
            sid,
            service,
            callback,
            expires: now + Duration::from_secs(secs),
            seq: 0,
            last_body: None,
            failures: 0,
        });
        Ok(secs)
    }

    pub fn renew(
        &mut self,
        service: Service,
        sid: &str,
        timeout: Option<&str>,
        now: Instant,
    ) -> std::result::Result<u64, u16> {
        self.expire(now);
        let s = self
            .subs
            .iter_mut()
            .find(|s| s.sid == sid && s.service == service)
            .ok_or(412u16)?;
        let secs = parse_timeout(timeout);
        s.expires = now + Duration::from_secs(secs);
        Ok(secs)
    }

    pub fn unsubscribe(&mut self, service: Service, sid: &str) -> bool {
        let before = self.subs.len();
        self.subs
            .retain(|s| !(s.sid == sid && s.service == service));
        self.subs.len() != before
    }

    pub fn expire(&mut self, now: Instant) {
        self.subs.retain(|s| s.expires > now);
    }

    /// Notifications due for the current event bodies: the initial event
    /// for new subscriptions and an update whenever a body changed.
    pub fn collect(&mut self, bodies: &[(Service, String)], now: Instant) -> Vec<Notification> {
        self.expire(now);
        let mut out = Vec::new();
        for s in &mut self.subs {
            let Some((_, body)) = bodies.iter().find(|(svc, _)| *svc == s.service) else {
                continue;
            };
            if s.last_body.as_deref() == Some(body.as_str()) {
                continue;
            }
            let seq = match s.last_body {
                None => 0,
                Some(_) => s.seq.checked_add(1).unwrap_or(1),
            };
            s.seq = seq;
            s.last_body = Some(body.clone());
            out.push(Notification {
                sid: s.sid.clone(),
                callback: s.callback.clone(),
                seq,
                body: body.clone(),
            });
        }
        out
    }

    /// Record a delivery result; subscriptions that keep failing are dropped.
    pub fn report(&mut self, sid: &str, ok: bool) {
        if let Some(s) = self.subs.iter_mut().find(|s| s.sid == sid) {
            s.failures = if ok { 0 } else { s.failures.saturating_add(1) };
        }
        self.subs.retain(|s| s.failures < MAX_NOTIFY_FAILURES);
    }
}

/// Deliver one notification (blocking, short timeout).
pub fn send_notification(n: &Notification) -> bool {
    http::send_request(
        n.callback.addr,
        "NOTIFY",
        &n.callback.path,
        &[
            ("CONTENT-TYPE", "text/xml; charset=\"utf-8\"".into()),
            ("NT", "upnp:event".into()),
            ("NTS", "upnp:propchange".into()),
            ("SID", n.sid.clone()),
            ("SEQ", n.seq.to_string()),
        ],
        n.body.as_bytes(),
        Duration::from_secs(3),
    )
    .is_ok_and(|code| (200..300).contains(&code))
}

/// Current event bodies for the services that have subscribers.
pub fn event_bodies(renderer: &mut Renderer, subs: &Subscriptions) -> Vec<(Service, String)> {
    let mut v = Vec::new();
    if subs.has_service(Service::AvTransport) {
        renderer.refresh();
        v.push((
            Service::AvTransport,
            property_set(&[("LastChange", renderer.avt_last_change())]),
        ));
    }
    if subs.has_service(Service::RenderingControl)
        && let Some(lc) = renderer.rcs_last_change()
    {
        v.push((
            Service::RenderingControl,
            property_set(&[("LastChange", lc)]),
        ));
    }
    if subs.has_service(Service::ConnectionManager) {
        v.push((Service::ConnectionManager, cms_event_body()));
    }
    v
}

// ---------------------------------------------------------------------------
// HTTP routing
// ---------------------------------------------------------------------------

/// Shared renderer context for the HTTP handler.
pub struct Dlna {
    pub friendly_name: String,
    pub uuid: String,
    pub renderer: std::sync::Mutex<Renderer>,
    pub subscriptions: std::sync::Mutex<Subscriptions>,
}

impl Dlna {
    pub fn new(friendly_name: String, uuid: String, renderer: Renderer) -> Self {
        Self {
            friendly_name,
            uuid,
            renderer: std::sync::Mutex::new(renderer),
            subscriptions: std::sync::Mutex::new(Subscriptions::default()),
        }
    }
}

fn upnp_headers(r: Response) -> Response {
    r.header("Server", crate::ssdp::server_string())
        .header("EXT", "")
}

/// Handle `/dlna/*`; `None` for other paths.
pub fn handle_http(req: &Request, dlna: &Dlna) -> Option<Response> {
    let path = req.path.as_str();
    if !path.starts_with("/dlna/") {
        return None;
    }
    let get = req.method == "GET" || req.method == "HEAD";
    if path == DESCRIPTION_PATH {
        return Some(if get {
            upnp_headers(Response::xml(
                200,
                device_description(&dlna.friendly_name, &dlna.uuid),
            ))
        } else {
            Response::status_only(405)
        });
    }
    if let Some(name) = path
        .strip_prefix("/dlna/")
        .and_then(|p| p.strip_suffix(".xml"))
        .and_then(Service::from_name)
    {
        return Some(if get {
            upnp_headers(Response::xml(200, scpd_for(name)))
        } else {
            Response::status_only(405)
        });
    }
    if let Some(service) = path
        .strip_prefix("/dlna/control/")
        .and_then(Service::from_name)
    {
        return Some(control(req, dlna, service));
    }
    if let Some(service) = path
        .strip_prefix("/dlna/event/")
        .and_then(Service::from_name)
    {
        return Some(event(req, dlna, service));
    }
    Some(Response::status_only(404))
}

fn fault(err: UpnpError) -> Response {
    upnp_headers(Response::xml(500, soap_fault(err)))
}

fn control(req: &Request, dlna: &Dlna, service: Service) -> Response {
    if req.method != "POST" {
        return Response::status_only(405);
    }
    // Browsers send Origin on cross-site requests; real control points don't.
    if req.header("origin").is_some() {
        return Response::status_only(403);
    }
    let Some((ty, action)) = req.header("soapaction").and_then(parse_soap_action) else {
        return fault(UpnpError::InvalidAction);
    };
    if ty != service.service_type() {
        return fault(UpnpError::InvalidAction);
    }
    let Ok(body) = std::str::from_utf8(&req.body) else {
        return fault(UpnpError::InvalidArgs);
    };
    let result = match dlna.renderer.lock() {
        Ok(mut r) => r.handle(service, &action, body),
        Err(_) => Err(UpnpError::ActionFailed),
    };
    match result {
        Ok(args) => upnp_headers(Response::xml(200, soap_response(&ty, &action, &args))),
        Err(e) => fault(e),
    }
}

fn event(req: &Request, dlna: &Dlna, service: Service) -> Response {
    let Ok(mut subs) = dlna.subscriptions.lock() else {
        return Response::status_only(500);
    };
    let now = Instant::now();
    let sid_header = req.header("sid");
    match req.method.as_str() {
        "SUBSCRIBE" => {
            let has_cb = req.header("callback").is_some() || req.header("nt").is_some();
            let result = match (sid_header, has_cb) {
                (Some(_), true) => return Response::status_only(400),
                (Some(sid), false) => subs
                    .renew(service, sid.trim(), req.header("timeout"), now)
                    .map(|secs| (sid.trim().to_string(), secs)),
                (None, _) => {
                    let Ok(id) = http::uuid_v4() else {
                        return Response::status_only(500);
                    };
                    let sid = format!("uuid:{id}");
                    subs.subscribe(
                        service,
                        req.header("callback"),
                        req.header("nt"),
                        req.header("timeout"),
                        req.peer_ipv4(),
                        sid.clone(),
                        now,
                    )
                    .map(|secs| (sid, secs))
                }
            };
            match result {
                Ok((sid, secs)) => upnp_headers(Response::new(200))
                    .header("SID", sid)
                    .header("TIMEOUT", format!("Second-{secs}")),
                Err(code) => Response::status_only(code),
            }
        }
        "UNSUBSCRIBE" => match sid_header {
            Some(sid) if subs.unsubscribe(service, sid.trim()) => Response::new(200),
            Some(_) => Response::status_only(412),
            None => Response::status_only(412),
        },
        _ => Response::status_only(405),
    }
}

/// One pass of the event loop: compute bodies, send due notifications.
pub fn pump_events(dlna: &Dlna) {
    let notes = {
        let Ok(mut subs) = dlna.subscriptions.lock() else {
            return;
        };
        subs.expire(Instant::now());
        if subs.is_empty() {
            return;
        }
        let bodies = match dlna.renderer.lock() {
            Ok(mut r) => event_bodies(&mut r, &subs),
            Err(_) => return,
        };
        subs.collect(&bodies, Instant::now())
    };
    for n in notes {
        let ok = send_notification(&n);
        if let Ok(mut subs) = dlna.subscriptions.lock() {
            subs.report(&n.sid, ok);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::mpv::PlayerStatus;
    use crate::process::{Output, fake::FakeRunner};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeState {
        calls: Vec<String>,
        status: PlayerStatus,
        fail: bool,
    }

    #[derive(Clone, Default)]
    struct FakePlayer(Arc<Mutex<FakeState>>);

    impl Player for FakePlayer {
        fn load(&mut self, url: &str, paused: bool) -> Result<()> {
            let mut s = self.0.lock().unwrap();
            if s.fail {
                return Err(Error::msg("boom"));
            }
            s.calls.push(format!("load {url} paused={paused}"));
            s.status = PlayerStatus {
                running: true,
                paused,
                position: Some(0.0),
                duration: Some(125.0),
            };
            Ok(())
        }
        fn set_paused(&mut self, paused: bool) -> Result<()> {
            let mut s = self.0.lock().unwrap();
            s.calls.push(format!("paused={paused}"));
            s.status.paused = paused;
            Ok(())
        }
        fn seek(&mut self, seconds: f64) -> Result<()> {
            let mut s = self.0.lock().unwrap();
            s.calls.push(format!("seek {seconds}"));
            s.status.position = Some(seconds);
            Ok(())
        }
        fn stop(&mut self) {
            let mut s = self.0.lock().unwrap();
            s.calls.push("stop".into());
            s.status = PlayerStatus::default();
        }
        fn status(&mut self) -> PlayerStatus {
            self.0.lock().unwrap().status
        }
    }

    fn renderer() -> (Renderer, FakePlayer) {
        let p = FakePlayer::default();
        let mut audio = FakeRunner::new();
        audio.default = Output::ok("Volume: 0.40");
        (Renderer::new(Box::new(p.clone()), Box::new(audio)), p)
    }

    fn soap(action: &str, args: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:{action} xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><InstanceID>0</InstanceID>{args}</u:{action}></s:Body></s:Envelope>"
        )
    }

    fn call(r: &mut Renderer, svc: Service, action: &str, args: &str) -> ActionResult {
        r.handle(svc, action, &soap(action, args))
    }

    fn get(v: &[(String, String)], k: &str) -> String {
        v.iter().find(|(a, _)| a == k).unwrap().1.clone()
    }

    #[test]
    fn identity() {
        let dir = std::env::temp_dir().join(format!("catostv-uuid-test-{}", std::process::id()));
        let path = dir.join("sub/cast-uuid");
        let a = load_or_create_uuid(&path).unwrap();
        assert!(http::is_uuid(&a));
        assert_eq!(
            load_or_create_uuid(&path).unwrap(),
            a,
            "stable across restarts"
        );
        std::fs::write(&path, "garbage").unwrap();
        let b = load_or_create_uuid(&path).unwrap();
        assert_ne!(a, b);
        assert!(http::is_uuid(&b));
        let _ = std::fs::remove_dir_all(dir);
        assert!(uuid_path().ends_with("catostv/cast-uuid"));
        assert_eq!(friendly_name_for("catostv\n"), "CatosTV");
        assert_eq!(friendly_name_for(""), "CatosTV");
        assert_eq!(friendly_name_for("living-room\n"), "CatosTV (living-room)");
        assert_eq!(advertisement("u").service_types.len(), 3);
    }

    #[test]
    fn xml_escape_round_trip() {
        let s = "a<b>&\"c'\u{1}";
        assert_eq!(xml_escape(s), "a&lt;b&gt;&amp;&quot;c&apos;");
        assert_eq!(xml_unescape("a&lt;b&gt;&amp;&quot;c&apos;"), "a<b>&\"c'");
        assert_eq!(xml_unescape("&#65;&#x42;&bogus;&;&amp"), "AB&bogus;&;&amp");
        assert_eq!(xml_unescape("&#xD800;"), "&#xD800;");
    }

    #[test]
    fn element_extraction() {
        let body = soap(
            "SetAVTransportURI",
            "<CurrentURI>http://h/a.mp4?x=1&amp;y=2</CurrentURI><CurrentURIMetaData>&lt;DIDL-Lite&gt;&lt;/DIDL-Lite&gt;</CurrentURIMetaData>",
        );
        assert_eq!(
            xml_element_text(&body, "CurrentURI").unwrap(),
            "http://h/a.mp4?x=1&y=2"
        );
        assert_eq!(
            xml_element_text(&body, "CurrentURIMetaData").unwrap(),
            "<DIDL-Lite></DIDL-Lite>"
        );
        assert_eq!(xml_element_text(&body, "InstanceID").unwrap(), "0");
        // Prefixes, attributes, empty element, CDATA.
        assert_eq!(
            xml_element_text("<a:X dt=\"s\"> v </a:X>", "X").unwrap(),
            "v"
        );
        assert_eq!(xml_element_text("<X/>", "X").unwrap(), "");
        assert_eq!(xml_element_text("<X />", "X").unwrap(), "");
        assert_eq!(
            xml_element_text("<X><![CDATA[<b>&amp;</b>]]>&amp;</X>", "X").unwrap(),
            "<b>&amp;</b>&"
        );
        // Prefix match must be on the local name only.
        assert!(
            xml_element_text("<CurrentURIMetaData>x</CurrentURIMetaData>", "CurrentURI").is_none()
        );
        assert!(xml_element_text("<X>unterminated", "X").is_none());
    }

    #[test]
    fn xml_garbage_never_panics() {
        let mut seed: u32 = 99;
        let alphabet = "<>/!?[]CDATA&;#x: =\"'Xé";
        let chars: Vec<char> = alphabet.chars().collect();
        for len in 0..400 {
            let s: String = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    chars[(seed >> 8) as usize % chars.len()]
                })
                .collect();
            let _ = xml_element_text(&s, "X");
            let _ = xml_unescape(&s);
            let _ = didl_mime(&s);
            let _ = xml_is_balanced(&s);
            let (mut r, _) = renderer();
            let _ = r.handle(Service::AvTransport, "SetAVTransportURI", &s);
            let _ = parse_soap_action(&s);
        }
    }

    #[test]
    fn descriptions_are_well_formed() {
        let d = device_description("Living <Room> & TV", "11111111-2222-4333-8444-555555555555");
        assert!(xml_is_balanced(&d), "{d}");
        assert!(d.contains("<friendlyName>Living &lt;Room&gt; &amp; TV</friendlyName>"));
        assert!(d.contains("<UDN>uuid:11111111-2222-4333-8444-555555555555</UDN>"));
        assert!(d.contains("<controlURL>/dlna/control/AVTransport</controlURL>"));
        for s in Service::ALL {
            let x = scpd_for(s);
            assert!(xml_is_balanced(&x), "{}", s.name());
            // Every relatedStateVariable must be declared.
            let mut rest = x.as_str();
            while let Some(i) = rest.find("<relatedStateVariable>") {
                let after = &rest[i + 22..];
                let end = after.find('<').unwrap();
                let name = &after[..end];
                assert!(
                    x.contains(&format!("<name>{name}</name><dataType>")),
                    "{}: {name} undeclared",
                    s.name()
                );
                rest = &after[end..];
            }
        }
        assert!(scpd_for(Service::RenderingControl).contains("<maximum>100</maximum>"));
    }

    #[test]
    fn soap_action_header() {
        assert_eq!(
            parse_soap_action("\"urn:schemas-upnp-org:service:AVTransport:1#Play\""),
            Some((
                "urn:schemas-upnp-org:service:AVTransport:1".into(),
                "Play".into()
            ))
        );
        assert!(parse_soap_action("nohash").is_none());
        assert!(parse_soap_action("\"x#\"").is_none());
        assert!(parse_soap_action("x#Pl<ay").is_none());
    }

    #[test]
    fn soap_envelopes() {
        let r = soap_response(
            "urn:schemas-upnp-org:service:AVTransport:1",
            "GetTransportInfo",
            &[
                ("CurrentTransportState".into(), "PLAYING".into()),
                ("M".into(), "<x&>".into()),
            ],
        );
        assert!(xml_is_balanced(&r), "{r}");
        assert!(r.contains(
            "<u:GetTransportInfoResponse xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\">"
        ));
        assert!(r.contains("<M>&lt;x&amp;&gt;</M>"));
        let f = soap_fault(UpnpError::TransitionNotAvailable);
        assert!(xml_is_balanced(&f));
        assert!(f.contains("<errorCode>701</errorCode>"));
    }

    #[test]
    fn hms() {
        assert_eq!(format_hms(0.0), "0:00:00");
        assert_eq!(format_hms(3725.9), "1:02:05");
        assert_eq!(format_hms(f64::NAN), "0:00:00");
        assert_eq!(format_hms(-5.0), "0:00:00");
        assert_eq!(parse_hms("1:02:05"), Some(3725.0));
        assert_eq!(parse_hms("0:00:10.500"), Some(10.5));
        assert_eq!(parse_hms("02:05"), Some(125.0));
        assert_eq!(parse_hms("42"), Some(42.0));
        assert_eq!(parse_hms("100:00:00"), Some(360000.0));
        for bad in [
            "",
            "-0:00:01",
            "1:2:3:4",
            "a:00:00",
            "0:60:00",
            "0:00:6x",
            "1.5:00:00",
            ":",
        ] {
            assert_eq!(parse_hms(bad), None, "{bad}");
        }
    }

    #[test]
    fn mime_rules() {
        let didl = "<DIDL-Lite><item><res protocolInfo=\"http-get:*:video/mp4:DLNA.ORG_PN=AVC\">http://x</res></item></DIDL-Lite>";
        assert_eq!(didl_mime(didl).as_deref(), Some("video/mp4"));
        assert!(mime_supported("video/mp4"));
        assert!(mime_supported("IMAGE/JPEG"));
        assert!(mime_supported("application/vnd.apple.mpegurl"));
        assert!(!mime_supported("application/x-sh"));
        assert!(!mime_supported("text/html"));
        assert!(sink_protocol_info().starts_with("http-get:*:video/mp4:*,"));
        assert!(sink_protocol_info().ends_with("http-get:*:image/*:*"));
    }

    #[test]
    fn full_playback_flow() {
        let (mut r, p) = renderer();
        let info = call(&mut r, Service::AvTransport, "GetTransportInfo", "").unwrap();
        assert_eq!(get(&info, "CurrentTransportState"), "NO_MEDIA_PRESENT");
        assert_eq!(
            call(&mut r, Service::AvTransport, "Play", "<Speed>1</Speed>"),
            Err(UpnpError::TransitionNotAvailable)
        );
        call(
            &mut r,
            Service::AvTransport,
            "SetAVTransportURI",
            "<CurrentURI>http://192.168.1.4/v.mp4</CurrentURI><CurrentURIMetaData></CurrentURIMetaData>",
        )
        .unwrap();
        assert_eq!(r.state(), TransportState::Stopped);
        assert!(
            p.0.lock().unwrap().calls.is_empty(),
            "nothing plays before Play"
        );
        call(&mut r, Service::AvTransport, "Play", "<Speed>1</Speed>").unwrap();
        assert_eq!(r.state(), TransportState::Playing);
        call(&mut r, Service::AvTransport, "Pause", "").unwrap();
        assert_eq!(r.state(), TransportState::Paused);
        call(&mut r, Service::AvTransport, "Play", "<Speed>1</Speed>").unwrap();
        call(
            &mut r,
            Service::AvTransport,
            "Seek",
            "<Unit>REL_TIME</Unit><Target>0:01:00</Target>",
        )
        .unwrap();
        let pos = call(&mut r, Service::AvTransport, "GetPositionInfo", "").unwrap();
        assert_eq!(get(&pos, "RelTime"), "0:01:00");
        assert_eq!(get(&pos, "TrackDuration"), "0:02:05");
        assert_eq!(get(&pos, "TrackURI"), "http://192.168.1.4/v.mp4");
        let acts = call(
            &mut r,
            Service::AvTransport,
            "GetCurrentTransportActions",
            "",
        )
        .unwrap();
        assert_eq!(get(&acts, "Actions"), "Pause,Stop,Seek");
        // User paused inside mpv with the remote: state follows.
        p.0.lock().unwrap().status.paused = true;
        let info = call(&mut r, Service::AvTransport, "GetTransportInfo", "").unwrap();
        assert_eq!(get(&info, "CurrentTransportState"), "PAUSED_PLAYBACK");
        p.0.lock().unwrap().status.paused = false;
        // Media ends (mpv exits by itself).
        p.0.lock().unwrap().status = PlayerStatus::default();
        let info = call(&mut r, Service::AvTransport, "GetTransportInfo", "").unwrap();
        assert_eq!(get(&info, "CurrentTransportState"), "STOPPED");
        // Play again restarts the media.
        call(&mut r, Service::AvTransport, "Play", "<Speed>1</Speed>").unwrap();
        assert!(r.take_ended(), "end of media is reported");
        assert!(!r.take_ended(), "only once");
        call(&mut r, Service::AvTransport, "Stop", "").unwrap();
        assert_eq!(r.state(), TransportState::Stopped);
        assert!(r.take_ended(), "Stop closes the window");
        call(&mut r, Service::AvTransport, "Stop", "").unwrap();
        assert!(!r.take_ended(), "nothing was open");
        assert_eq!(
            p.0.lock().unwrap().calls[..6],
            vec![
                "load http://192.168.1.4/v.mp4 paused=false",
                "paused=true",
                "paused=false",
                "seek 60",
                "load http://192.168.1.4/v.mp4 paused=false",
                "stop"
            ]
        );
    }

    #[test]
    fn new_uri_while_playing_swaps_media_paused() {
        let (mut r, p) = renderer();
        call(
            &mut r,
            Service::AvTransport,
            "SetAVTransportURI",
            "<CurrentURI>http://h/1.mp4</CurrentURI>",
        )
        .unwrap();
        call(&mut r, Service::AvTransport, "Play", "").unwrap();
        call(
            &mut r,
            Service::AvTransport,
            "SetAVTransportURI",
            "<CurrentURI>http://h/2.mp4</CurrentURI>",
        )
        .unwrap();
        assert_eq!(r.state(), TransportState::Stopped);
        call(&mut r, Service::AvTransport, "Play", "").unwrap();
        assert_eq!(
            p.0.lock().unwrap().calls,
            vec![
                "load http://h/1.mp4 paused=false",
                "load http://h/2.mp4 paused=true",
                "paused=false"
            ]
        );
    }

    #[test]
    fn avt_errors() {
        let (mut r, p) = renderer();
        let set = |r: &mut Renderer, uri: &str, meta: &str| {
            call(
                r,
                Service::AvTransport,
                "SetAVTransportURI",
                &format!(
                    "<CurrentURI>{uri}</CurrentURI><CurrentURIMetaData>{meta}</CurrentURIMetaData>"
                ),
            )
        };
        assert_eq!(
            set(&mut r, "file:///etc/passwd", ""),
            Err(UpnpError::ResourceNotFound)
        );
        assert_eq!(
            set(&mut r, "--script=x", ""),
            Err(UpnpError::ResourceNotFound)
        );
        assert_eq!(
            set(
                &mut r,
                "http://h/x.sh",
                "&lt;res protocolInfo=&quot;http-get:*:application/x-sh:*&quot;&gt;"
            ),
            Err(UpnpError::IllegalMimeType)
        );
        assert_eq!(
            r.handle(
                Service::AvTransport,
                "SetAVTransportURI",
                "<InstanceID>0</InstanceID>"
            ),
            Err(UpnpError::InvalidArgs)
        );
        assert_eq!(
            r.handle(Service::AvTransport, "Play", "<InstanceID>3</InstanceID>"),
            Err(UpnpError::InvalidInstanceId)
        );
        assert_eq!(
            r.handle(Service::AvTransport, "Play", "<x/>"),
            Err(UpnpError::InvalidArgs)
        );
        assert_eq!(
            call(&mut r, Service::AvTransport, "Frobnicate", ""),
            Err(UpnpError::InvalidAction)
        );
        assert_eq!(
            call(&mut r, Service::AvTransport, "SetNextAVTransportURI", ""),
            Err(UpnpError::InvalidAction)
        );
        assert_eq!(
            call(&mut r, Service::AvTransport, "Pause", ""),
            Err(UpnpError::TransitionNotAvailable)
        );
        set(&mut r, "http://h/a.mp4", "").unwrap();
        assert_eq!(
            call(
                &mut r,
                Service::AvTransport,
                "Seek",
                "<Unit>REL_TIME</Unit><Target>0:00:05</Target>"
            ),
            Err(UpnpError::TransitionNotAvailable)
        );
        call(&mut r, Service::AvTransport, "Play", "").unwrap();
        assert_eq!(
            call(
                &mut r,
                Service::AvTransport,
                "Seek",
                "<Unit>TRACK_NR</Unit><Target>1</Target>"
            ),
            Err(UpnpError::SeekModeNotSupported)
        );
        assert_eq!(
            call(
                &mut r,
                Service::AvTransport,
                "Seek",
                "<Unit>ABS_TIME</Unit><Target>soon</Target>"
            ),
            Err(UpnpError::IllegalSeekTarget)
        );
        // Player failure → Action Failed.
        call(&mut r, Service::AvTransport, "Stop", "").unwrap();
        p.0.lock().unwrap().fail = true;
        assert_eq!(
            call(&mut r, Service::AvTransport, "Play", ""),
            Err(UpnpError::ActionFailed)
        );
        // Empty URI clears the media.
        set(&mut r, "", "").unwrap();
        assert_eq!(r.state(), TransportState::NoMedia);
    }

    #[test]
    fn rendering_control_uses_wpctl() {
        let p = FakePlayer::default();
        let audio = Arc::new(Mutex::new(Vec::<Cmd>::new()));
        struct Rec(Arc<Mutex<Vec<Cmd>>>);
        impl Runner for Rec {
            fn run(&mut self, cmd: &Cmd) -> Result<Output> {
                self.0.lock().unwrap().push(cmd.clone());
                Ok(Output::ok("Volume: 0.30 [MUTED]"))
            }
        }
        let mut r = Renderer::new(Box::new(p), Box::new(Rec(audio.clone())));
        let ch = "<Channel>Master</Channel>";
        let v = r
            .handle(
                Service::RenderingControl,
                "GetVolume",
                &soap("GetVolume", ch),
            )
            .unwrap();
        assert_eq!(get(&v, "CurrentVolume"), "30");
        let m = r
            .handle(Service::RenderingControl, "GetMute", &soap("GetMute", ch))
            .unwrap();
        assert_eq!(get(&m, "CurrentMute"), "1");
        r.handle(
            Service::RenderingControl,
            "SetVolume",
            &soap(
                "SetVolume",
                &format!("{ch}<DesiredVolume>7</DesiredVolume>"),
            ),
        )
        .unwrap();
        r.handle(
            Service::RenderingControl,
            "SetVolume",
            &soap(
                "SetVolume",
                &format!("{ch}<DesiredVolume>100</DesiredVolume>"),
            ),
        )
        .unwrap();
        r.handle(
            Service::RenderingControl,
            "SetMute",
            &soap("SetMute", &format!("{ch}<DesiredMute>false</DesiredMute>")),
        )
        .unwrap();
        assert_eq!(
            r.handle(
                Service::RenderingControl,
                "SetVolume",
                &soap(
                    "SetVolume",
                    &format!("{ch}<DesiredVolume>101</DesiredVolume>")
                )
            ),
            Err(UpnpError::InvalidArgs)
        );
        assert_eq!(
            r.handle(
                Service::RenderingControl,
                "GetVolume",
                &soap("GetVolume", "<Channel>LF</Channel>")
            ),
            Err(UpnpError::InvalidArgs)
        );
        let seen: Vec<Vec<String>> = audio
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.args.clone())
            .collect();
        assert_eq!(seen[2], ["set-volume", "@DEFAULT_AUDIO_SINK@", "0.07"]);
        assert_eq!(seen[3], ["set-volume", "@DEFAULT_AUDIO_SINK@", "1.00"]);
        assert_eq!(seen[4], ["set-mute", "@DEFAULT_AUDIO_SINK@", "0"]);
        let lc = r.rcs_last_change().unwrap();
        assert!(lc.contains("<Volume channel=\"Master\" val=\"30\"/>"));
        assert!(xml_is_balanced(&lc));
    }

    #[test]
    fn connection_manager() {
        let (mut r, _) = renderer();
        let pi = r
            .handle(Service::ConnectionManager, "GetProtocolInfo", "<x/>")
            .unwrap();
        assert!(get(&pi, "Sink").contains("http-get:*:video/mp4:*"));
        assert_eq!(get(&pi, "Source"), "");
        let ids = r
            .handle(Service::ConnectionManager, "GetCurrentConnectionIDs", "")
            .unwrap();
        assert_eq!(get(&ids, "ConnectionIDs"), "0");
        let info = r
            .handle(
                Service::ConnectionManager,
                "GetCurrentConnectionInfo",
                "<ConnectionID>0</ConnectionID>",
            )
            .unwrap();
        assert_eq!(get(&info, "Direction"), "Input");
        assert_eq!(
            r.handle(
                Service::ConnectionManager,
                "GetCurrentConnectionInfo",
                "<ConnectionID>5</ConnectionID>"
            ),
            Err(UpnpError::InvalidConnectionReference)
        );
    }

    #[test]
    fn last_change_is_escaped_twice() {
        let (mut r, _) = renderer();
        call(
            &mut r,
            Service::AvTransport,
            "SetAVTransportURI",
            "<CurrentURI>http://h/a.mp4?a=1&amp;b=2</CurrentURI>",
        )
        .unwrap();
        let lc = r.avt_last_change();
        assert!(lc.contains("<AVTransportURI val=\"http://h/a.mp4?a=1&amp;b=2\"/>"));
        assert!(xml_is_balanced(&lc));
        let ps = property_set(&[("LastChange", lc.clone())]);
        assert!(xml_is_balanced(&ps));
        assert_eq!(xml_element_text(&ps, "LastChange").unwrap(), lc);
    }

    #[test]
    fn subscriptions_lifecycle() {
        let mut subs = Subscriptions::default();
        let now = Instant::now();
        let peer = Some(Ipv4Addr::new(192, 168, 1, 50));
        let cb = Some("<http://192.168.1.50:4000/ev>");
        // Wrong NT, missing callback, third-party callback.
        assert_eq!(
            subs.subscribe(
                Service::AvTransport,
                cb,
                Some("x"),
                None,
                peer,
                "uuid:a".into(),
                now
            ),
            Err(412)
        );
        assert_eq!(
            subs.subscribe(
                Service::AvTransport,
                None,
                Some("upnp:event"),
                None,
                peer,
                "uuid:a".into(),
                now
            ),
            Err(412)
        );
        assert_eq!(
            subs.subscribe(
                Service::AvTransport,
                Some("<http://10.9.9.9:80/steal>"),
                Some("upnp:event"),
                None,
                peer,
                "uuid:a".into(),
                now
            ),
            Err(412)
        );
        assert_eq!(
            subs.subscribe(
                Service::AvTransport,
                cb,
                Some("upnp:event"),
                Some("Second-30"),
                peer,
                "uuid:a".into(),
                now
            ),
            Ok(60)
        );
        assert_eq!(
            subs.renew(Service::AvTransport, "uuid:a", Some("Second-99999"), now),
            Ok(1800)
        );
        assert_eq!(
            subs.renew(Service::AvTransport, "uuid:zzz", None, now),
            Err(412)
        );
        // Initial event, no duplicate, then change.
        let bodies = vec![(Service::AvTransport, "A".to_string())];
        let n = subs.collect(&bodies, now);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].seq, 0);
        assert_eq!(n[0].callback.path, "/ev");
        assert!(subs.collect(&bodies, now).is_empty());
        let n = subs.collect(&[(Service::AvTransport, "B".to_string())], now);
        assert_eq!(n[0].seq, 1);
        // Failures drop the subscription.
        for _ in 0..3 {
            subs.report("uuid:a", false);
        }
        assert!(subs.is_empty());
        // Expiry.
        subs.subscribe(
            Service::AvTransport,
            cb,
            Some("upnp:event"),
            None,
            peer,
            "uuid:b".into(),
            now,
        )
        .unwrap();
        subs.expire(now + Duration::from_secs(1801));
        assert!(subs.is_empty());
        // Cap.
        for i in 0..MAX_SUBSCRIPTIONS {
            subs.subscribe(
                Service::AvTransport,
                cb,
                Some("upnp:event"),
                None,
                peer,
                format!("uuid:{i}"),
                now,
            )
            .unwrap();
        }
        assert_eq!(
            subs.subscribe(
                Service::AvTransport,
                cb,
                Some("upnp:event"),
                None,
                peer,
                "uuid:x".into(),
                now
            ),
            Err(503)
        );
        assert!(subs.unsubscribe(Service::AvTransport, "uuid:0"));
        assert!(!subs.unsubscribe(Service::AvTransport, "uuid:0"));
    }

    #[test]
    fn callback_and_timeout_parsing() {
        let c = parse_callbacks("<http://1.2.3.4:5/a><http://host.example/b><junk>");
        assert_eq!(c.len(), 1);
        assert_eq!(parse_timeout(Some("Second-1800")), 1800);
        assert_eq!(parse_timeout(Some("infinite")), 1800);
        assert_eq!(parse_timeout(None), 1800);
        assert_eq!(parse_timeout(Some("Second-5")), 60);
    }

    fn dlna() -> Dlna {
        let (r, _) = renderer();
        Dlna::new(
            "CatosTV".into(),
            "11111111-2222-4333-8444-555555555555".into(),
            r,
        )
    }

    fn request(method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
            peer: Some("192.168.1.50:5555".parse().unwrap()),
            local: Some("192.168.1.2:49494".parse().unwrap()),
            ..Request::default()
        }
    }

    #[test]
    fn http_routes() {
        let d = dlna();
        assert!(handle_http(&request("GET", "/apps/YouTube", &[], ""), &d).is_none());
        let r = handle_http(&request("GET", DESCRIPTION_PATH, &[], ""), &d).unwrap();
        assert_eq!(r.status, 200);
        let r = handle_http(&request("GET", "/dlna/AVTransport.xml", &[], ""), &d).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(
            handle_http(&request("GET", "/dlna/Nope.xml", &[], ""), &d)
                .unwrap()
                .status,
            404
        );
        let sa = "\"urn:schemas-upnp-org:service:AVTransport:1#GetTransportInfo\"";
        let body = soap("GetTransportInfo", "");
        let r = handle_http(
            &request(
                "POST",
                "/dlna/control/AVTransport",
                &[("SOAPACTION", sa)],
                &body,
            ),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 200);
        assert!(String::from_utf8_lossy(&r.body).contains("NO_MEDIA_PRESENT"));
        // Missing SOAPACTION (CSRF-style simple POST) → fault, nothing done.
        let r = handle_http(
            &request("POST", "/dlna/control/AVTransport", &[], &body),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 500);
        assert!(String::from_utf8_lossy(&r.body).contains("<errorCode>401</errorCode>"));
        // Browser origin → refused.
        let r = handle_http(
            &request(
                "POST",
                "/dlna/control/AVTransport",
                &[("SOAPACTION", sa), ("Origin", "https://evil.example")],
                &body,
            ),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 403);
        // Service mismatch.
        let r = handle_http(
            &request(
                "POST",
                "/dlna/control/RenderingControl",
                &[("SOAPACTION", sa)],
                &body,
            ),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 500);
        // Subscribe / renew / unsubscribe.
        let r = handle_http(
            &request(
                "SUBSCRIBE",
                "/dlna/event/AVTransport",
                &[
                    ("CALLBACK", "<http://192.168.1.50:4000/e>"),
                    ("NT", "upnp:event"),
                    ("TIMEOUT", "Second-300"),
                ],
                "",
            ),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 200);
        let sid = r
            .headers
            .iter()
            .find(|(k, _)| k == "SID")
            .unwrap()
            .1
            .clone();
        assert!(sid.starts_with("uuid:"));
        let r = handle_http(
            &request("SUBSCRIBE", "/dlna/event/AVTransport", &[("SID", &sid)], ""),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 200);
        let r = handle_http(
            &request(
                "SUBSCRIBE",
                "/dlna/event/AVTransport",
                &[("SID", &sid), ("NT", "upnp:event")],
                "",
            ),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 400);
        let r = handle_http(
            &request(
                "UNSUBSCRIBE",
                "/dlna/event/AVTransport",
                &[("SID", &sid)],
                "",
            ),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 200);
        let r = handle_http(
            &request(
                "UNSUBSCRIBE",
                "/dlna/event/AVTransport",
                &[("SID", &sid)],
                "",
            ),
            &d,
        )
        .unwrap();
        assert_eq!(r.status, 412);
    }

    #[test]
    fn event_pump_delivers_initial_notify() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let got = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let req = crate::http::read_request(&mut s, crate::http::MAX_BODY_BYTES).unwrap();
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            let mut rest = Vec::new();
            let _ = s.read_to_end(&mut rest);
            req
        });
        let d = dlna();
        let mut req = request("SUBSCRIBE", "/dlna/event/AVTransport", &[], "");
        let cb = format!("<http://127.0.0.1:{port}/cb>");
        req.headers = vec![("CALLBACK".into(), cb), ("NT".into(), "upnp:event".into())];
        req.peer = Some("127.0.0.1:1".parse().unwrap());
        assert_eq!(handle_http(&req, &d).unwrap().status, 200);
        pump_events(&d);
        let notify = got.join().unwrap();
        assert_eq!(notify.method, "NOTIFY");
        assert_eq!(notify.path, "/cb");
        assert_eq!(notify.header("SEQ"), Some("0"));
        assert_eq!(notify.header("NTS"), Some("upnp:propchange"));
        let body = notify.body_text();
        let lc = xml_element_text(&body, "LastChange").unwrap();
        assert!(
            lc.contains("<TransportState val=\"NO_MEDIA_PRESENT\"/>"),
            "{lc}"
        );
    }
}
