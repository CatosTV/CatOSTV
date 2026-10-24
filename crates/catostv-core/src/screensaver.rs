//! The screensaver: pictures, a clock and the weather.
//!
//! * Cats come from TheCatAPI (`api.thecatapi.com`, no key). Its pictures
//!   are all sizes; only landscape ones at least [`MIN_WIDTH`] wide are kept.
//! * Photos come from Lorem Picsum (`picsum.photos`), which hands out a
//!   random photo cut to exactly 3840 × 2160.
//! * The weather comes from wttr.in (`format=j1`), which places the TV by
//!   its IP address, so no location lookup is needed.
//!
//! Pictures are kept in `~/.cache/catostv/screensaver/<kind>/`, so the
//! slideshow still runs without a network.

use crate::mpv::{Json, parse_json};
use crate::process::Cmd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Pictures narrower than this are left out (full HD and up).
pub const MIN_WIDTH: f64 = 1920.0;
/// How many pictures of each kind are kept on disk.
pub const KEEP: usize = 40;
/// The idle times Settings offers, in minutes.
pub const MINUTES: [u32; 5] = [1, 3, 5, 10, 15];
pub const DEFAULT_MINUTES: u32 = 3;
/// Seconds each picture shows, and the cross-fade between two.
pub const PICTURE_SECS: f32 = 12.0;
pub const FADE_SECS: f32 = 1.6;

pub const CAT_LIST_URL: &str =
    "https://api.thecatapi.com/v1/images/search?limit=10&size=full&mime_types=jpg";
pub const PHOTO_URL: &str = "https://picsum.photos/3840/2160.jpg";
pub const WEATHER_URL: &str = "https://wttr.in/?format=j1";

/// A different picture every time: Lorem Picsum caches by URL, so four
/// identical requests could return the same photo four times.
pub fn photo_url(seed: u64) -> String {
    format!("{PHOTO_URL}?random={seed}")
}

/// Cat pictures come only from TheCatAPI's own servers.
const CAT_HOST_SUFFIX: &str = ".thecatapi.com";

/// The host of an `https://` URL, when it is a plain name (no user name,
/// port or odd characters).
fn https_host(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    let plain = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    plain.then_some(host)
}

/// A TheCatAPI picture address: https, one of its own servers, a `.jpg`.
fn is_cat_url(url: &str) -> bool {
    url.ends_with(".jpg")
        && https_host(url)
            .is_some_and(|h| h.len() > CAT_HOST_SUFFIX.len() && h.ends_with(CAT_HOST_SUFFIX))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pictures {
    Cats,
    Photos,
}

impl Pictures {
    pub fn from_key(s: &str) -> Self {
        if s == "photos" {
            Self::Photos
        } else {
            Self::Cats
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Cats => "cats",
            Self::Photos => "photos",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Cats => "GOOOD CATS!",
            Self::Photos => "Random photos",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Cats => Self::Photos,
            Self::Photos => Self::Cats,
        }
    }
}

/// The next idle time in `delta`'s direction (no wrap).
pub fn cycle_minutes(current: u32, delta: i32) -> u32 {
    let i = MINUTES.iter().position(|&m| m == current).unwrap_or(1) as i32;
    MINUTES[(i + delta).clamp(0, MINUTES.len() as i32 - 1) as usize]
}

pub fn cache_dir(home: &Path, kind: Pictures) -> PathBuf {
    home.join(".cache/catostv/screensaver").join(kind.key())
}

/// The pictures on disk, oldest first.
pub fn cached(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jpg"))
        .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
        .collect();
    files.sort();
    files.into_iter().map(|(_, p)| p).collect()
}

/// Remove the oldest pictures beyond [`KEEP`].
pub fn trim(dir: &Path) {
    let files = cached(dir);
    for p in files.iter().take(files.len().saturating_sub(KEEP)) {
        let _ = std::fs::remove_file(p);
    }
}

/// The big landscape cat pictures in a TheCatAPI search answer.
pub fn parse_cats(text: &str) -> Vec<String> {
    let Some(Json::Arr(items)) = parse_json(text) else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|i| {
            let w = i.get("width").and_then(Json::as_f64).unwrap_or(0.0);
            let h = i.get("height").and_then(Json::as_f64).unwrap_or(0.0);
            w >= MIN_WIDTH && w >= h
        })
        .filter_map(|i| i.get("url")?.as_str())
        .filter(|u| is_cat_url(u))
        .map(str::to_string)
        .collect()
}

/// `curl` fetching `url` (following redirects, but only to https) into `out`.
pub fn download_cmd(url: &str, out: &Path) -> Cmd {
    Cmd::new("curl")
        .args([
            "-sS".to_string(),
            "--fail".to_string(),
            "--proto".to_string(),
            "=https".to_string(),
            "--proto-redir".to_string(),
            "=https".to_string(),
            "-L".to_string(),
            "-m".to_string(),
            "40".to_string(),
            "--max-filesize".to_string(),
            "30000000".to_string(),
            "-o".to_string(),
            out.display().to_string(),
            "--".to_string(),
            url.to_string(),
        ])
        .timeout(Duration::from_secs(45))
}

/// `curl` printing `url`.
pub fn get_cmd(url: &str) -> Cmd {
    Cmd::new("curl")
        .args([
            "-sS",
            "--fail",
            "--proto",
            "=https",
            "-m",
            "15",
            "--max-filesize",
            "2000000",
            "--",
            url,
        ])
        .timeout(Duration::from_secs(20))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Weather {
    pub temp_c: i32,
    pub temp_f: i32,
    /// "Patchy rain nearby".
    pub desc: String,
    /// "Betim".
    pub place: String,
    pub country: String,
    /// worldweatheronline condition code, see [`weather_icon`].
    pub code: u32,
}

impl Weather {
    /// "28°", in Fahrenheit where people use it.
    pub fn temp_label(&self) -> String {
        let f = matches!(
            self.country.as_str(),
            "United States of America" | "United States" | "Liberia" | "Myanmar"
        );
        format!("{}°", if f { self.temp_f } else { self.temp_c })
    }
}

/// The current weather in a wttr.in `format=j1` answer.
pub fn parse_weather(text: &str) -> Option<Weather> {
    let j = parse_json(text)?;
    let first = |v: &Json, key: &str| v.get(key)?.as_arr()?.first().cloned();
    let value = |v: &Json, key: &str| -> Option<String> {
        Some(first(v, key)?.get("value")?.as_str()?.trim().to_string())
    };
    let num = |v: &Json, key: &str| -> Option<i32> { v.get(key)?.as_str()?.trim().parse().ok() };
    let now = first(&j, "current_condition")?;
    let area = first(&j, "nearest_area");
    Some(Weather {
        temp_c: num(&now, "temp_C")?,
        temp_f: num(&now, "temp_F")?,
        desc: value(&now, "weatherDesc").unwrap_or_default(),
        place: area
            .as_ref()
            .and_then(|a| value(a, "areaName"))
            .unwrap_or_default(),
        country: area
            .as_ref()
            .and_then(|a| value(a, "country"))
            .unwrap_or_default(),
        code: num(&now, "weatherCode").map_or(0, |c| c.max(0) as u32),
    })
}

/// Icon name for a weather code (worldweatheronline's list).
pub fn weather_icon(code: u32, night: bool) -> &'static str {
    match code {
        113 if night => "moon",
        113 => "sun",
        116 | 119 | 122 => "cloud",
        143 | 248 | 260 => "fog",
        200 | 386..=395 => "storm",
        179 | 182 | 185 | 227 | 230 | 281 | 284 | 311..=338 | 350 | 362..=377 => "snow",
        176 | 263..=308 | 353..=359 => "rain",
        _ => "cloud",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_big_landscape_cats() {
        let text = r#"[{"id":"a","url":"https://cdn2.thecatapi.com/images/a.jpg","width":4320,"height":3240},
            {"id":"b","url":"https://cdn2.thecatapi.com/images/b.jpg","width":500,"height":667},
            {"id":"c","url":"https://cdn2.thecatapi.com/images/c.jpg","width":1920,"height":2400},
            {"id":"d","url":"https://cdn2.thecatapi.com/images/d.jpg","width":1920,"height":1200},
            {"id":"e","url":"http://x/e.jpg","width":4000,"height":3000}]"#;
        assert_eq!(
            parse_cats(text),
            [
                "https://cdn2.thecatapi.com/images/a.jpg",
                "https://cdn2.thecatapi.com/images/d.jpg"
            ]
        );
        assert!(parse_cats("oops").is_empty());
    }

    #[test]
    fn cat_pictures_only_from_the_cat_servers() {
        let item = |u: &str| format!(r#"[{{"url":"{u}","width":4000,"height":3000}}]"#);
        for good in [
            "https://cdn2.thecatapi.com/images/a.jpg",
            "https://cdn.thecatapi.com/images/a.jpg",
        ] {
            assert_eq!(parse_cats(&item(good)), [good]);
        }
        for bad in [
            "https://evilthecatapi.com/a.jpg",
            "https://thecatapi.com.evil.example/a.jpg",
            "https://cdn2.thecatapi.com@evil.example/a.jpg",
            "https://cdn2.thecatapi.com:8443/a.jpg",
            "https://192.168.1.1/a.jpg",
            "https://thecatapi.com/a.jpg",
            "https://cdn2.thecatapi.com/a.png",
            "http://cdn2.thecatapi.com/a.jpg",
        ] {
            assert!(parse_cats(&item(bad)).is_empty(), "{bad}");
        }
    }

    #[test]
    fn every_photo_is_its_own_request() {
        assert_ne!(photo_url(1), photo_url(2));
        assert!(photo_url(7).starts_with("https://picsum.photos/3840/2160.jpg?"));
    }

    #[test]
    fn downloads_never_leave_https() {
        let args = download_cmd("https://x/a.jpg", Path::new("/tmp/a")).args;
        let at = args.iter().position(|a| a == "--proto").unwrap();
        assert_eq!(args[at + 1], "=https");
        let at = args.iter().position(|a| a == "--proto-redir").unwrap();
        assert_eq!(args[at + 1], "=https");
        assert!(
            get_cmd(WEATHER_URL)
                .args
                .windows(2)
                .any(|w| w == ["--proto", "=https"])
        );
    }

    #[test]
    fn weather_from_wttr() {
        let text = r#"{"current_condition":[{"temp_C":"28","temp_F":"82","weatherCode":"176",
            "weatherDesc":[{"value":"Patchy rain nearby"}]}],
            "nearest_area":[{"areaName":[{"value":"Betim"}],"country":[{"value":"India"}]}]}"#;
        let w = parse_weather(text).unwrap();
        assert_eq!(w.temp_label(), "28°");
        assert_eq!(w.desc, "Patchy rain nearby");
        assert_eq!(w.place, "Betim");
        assert_eq!(weather_icon(w.code, false), "rain");
        let us = Weather {
            country: "United States of America".into(),
            ..w
        };
        assert_eq!(us.temp_label(), "82°");
        assert_eq!(weather_icon(113, true), "moon");
        assert!(parse_weather(r#"{"current_condition":[]}"#).is_none());
    }

    #[test]
    fn minutes_and_kinds() {
        assert_eq!(cycle_minutes(3, 1), 5);
        assert_eq!(cycle_minutes(15, 1), 15);
        assert_eq!(cycle_minutes(1, -1), 1);
        assert_eq!(cycle_minutes(7, 1), 5);
        assert_eq!(Pictures::from_key("photos"), Pictures::Photos);
        assert_eq!(Pictures::from_key("junk"), Pictures::Cats);
        assert_eq!(Pictures::Cats.other().label(), "Random photos");
    }
}
