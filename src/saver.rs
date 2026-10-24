//! The screensaver's slideshow: pictures from the cache, decoded off the
//! UI thread, shown one after another with a slow zoom and a cross-fade.
//! Downloading new ones and the weather run as jobs (see `fetch`).

use catostv_core::process::{Runner, SystemRunner};
use catostv_core::screensaver::{self as ss, Pictures, Weather};
use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct Slide {
    pub tex: TextureHandle,
    /// When it came on screen, and its number (the zoom alternates).
    pub since: Instant,
    pub n: usize,
}

/// Largest picture side that is decoded at all.
const MAX_SIDE: u32 = 8192;

type Decoded = Arc<Mutex<Option<Option<ColorImage>>>>;

pub struct Slideshow {
    pub started: Instant,
    pics: Vec<PathBuf>,
    next: usize,
    shown: usize,
    pub cur: Option<Slide>,
    pub prev: Option<Slide>,
    /// A decode in flight: `Some(result)` once it is done.
    decoding: Option<Decoded>,
    /// Seconds each picture stays.
    pub secs: f32,
    /// Softly blurred, for Home's background.
    blur: bool,
}

impl Slideshow {
    pub fn new(mut pics: Vec<PathBuf>) -> Self {
        shuffle(&mut pics);
        Self {
            started: Instant::now(),
            pics,
            next: 0,
            shown: 0,
            cur: None,
            prev: None,
            decoding: None,
            secs: ss::PICTURE_SECS,
            blur: false,
        }
    }

    /// Home's background: the same pictures, softly blurred, changing
    /// slowly.
    pub fn backdrop(pics: Vec<PathBuf>) -> Self {
        Self {
            secs: BACKDROP_SECS,
            blur: true,
            ..Self::new(pics)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pics.is_empty()
    }

    /// Newly downloaded pictures come next.
    pub fn add(&mut self, new: Vec<PathBuf>) {
        let at = self.next.min(self.pics.len());
        for (k, p) in new.into_iter().enumerate() {
            if !self.pics.contains(&p) {
                self.pics.insert(at + k, p);
            }
        }
    }

    /// Start the next decode when it is time, and put a decoded picture
    /// on screen. `px` is the screen size in pixels.
    pub fn tick(&mut self, ctx: &egui::Context, px: [u32; 2]) {
        if let Some(d) = &self.decoding {
            let done = d.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(img) = done {
                self.decoding = None;
                if let Some(img) = img {
                    let tex = ctx.load_texture(
                        format!("screensaver-{}", self.shown),
                        img,
                        TextureOptions::LINEAR,
                    );
                    self.prev = self.cur.take();
                    self.cur = Some(Slide {
                        tex,
                        since: Instant::now(),
                        n: self.shown,
                    });
                    self.shown += 1;
                }
            }
        }
        if self.prev.as_ref().is_some_and(|_| self.fade() >= 1.0) {
            self.prev = None;
        }
        let due = self
            .cur
            .as_ref()
            .is_none_or(|s| s.since.elapsed().as_secs_f32() >= self.secs);
        if due && self.decoding.is_none() && !self.pics.is_empty() {
            let path = self.pics[self.next % self.pics.len()].clone();
            self.next = (self.next + 1) % self.pics.len();
            let slot: Decoded = Arc::new(Mutex::new(None));
            self.decoding = Some(slot.clone());
            let ctx = ctx.clone();
            let blur = self.blur;
            std::thread::spawn(move || {
                // A decoder panic must not leave the slot empty for good:
                // the slideshow would wait for this picture forever.
                let guarded = std::panic::AssertUnwindSafe(|| decode(&path, px, blur));
                let img = std::panic::catch_unwind(guarded).unwrap_or(None);
                if img.is_none() {
                    // A broken download: gone, so it is not tried again.
                    let _ = std::fs::remove_file(&path);
                }
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(img);
                ctx.request_repaint();
            });
        }
    }

    /// Cross-fade progress of the current picture, 0 → 1.
    pub fn fade(&self) -> f32 {
        self.cur
            .as_ref()
            .map_or(0.0, |s| s.since.elapsed().as_secs_f32() / ss::FADE_SECS)
            .min(1.0)
    }
}

/// How long each picture stays behind Home (seconds).
pub const BACKDROP_SECS: f32 = 40.0;

/// Decode a JPEG and cut it to the screen's shape, no bigger than the
/// screen. `blur`: a quarter of the screen's size, softened, for Home's
/// background (stretched back up it stays soft and costs little memory).
fn decode(path: &Path, px: [u32; 2], blur: bool) -> Option<ColorImage> {
    let px = if blur {
        [(px[0] / 4).clamp(16, 960), (px[1] / 4).clamp(16, 540)]
    } else {
        px
    };
    use image::imageops::{self, FilterType};
    // Pictures come from the internet: nothing bigger than 8192 px a side
    // (and the decoder's default memory limit) is decoded.
    let mut reader = image::ImageReader::open(path).ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    reader.limits(limits);
    let img = reader.decode().ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let (sw, sh) = (px[0].max(1), px[1].max(1));
    if w < 16 || h < 16 {
        return None;
    }
    // Largest centred crop with the screen's aspect ratio.
    let (cw, ch) = if w as u64 * sh as u64 > h as u64 * sw as u64 {
        ((h as u64 * sw as u64 / sh as u64) as u32, h)
    } else {
        (w, (w as u64 * sh as u64 / sw as u64) as u32)
    };
    let crop = imageops::crop_imm(&img, (w - cw) / 2, (h - ch) / 2, cw, ch).to_image();
    let out = if cw > sw {
        imageops::resize(&crop, sw, sh, FilterType::Triangle)
    } else {
        crop
    };
    let out = if blur { imageops::blur(&out, 3.0) } else { out };
    let (ow, oh) = out.dimensions();
    Some(ColorImage::from_rgba_unmultiplied(
        [ow as usize, oh as usize],
        out.as_raw(),
    ))
}

/// Download a few new pictures of `kind`; returns the new files.
pub fn fetch(home: &Path, kind: Pictures) -> Vec<PathBuf> {
    let dir = ss::cache_dir(home, kind);
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    let mut r = SystemRunner;
    let urls: Vec<(String, String)> = match kind {
        Pictures::Cats => {
            let mut urls = Vec::new();
            for _ in 0..3 {
                if urls.len() >= 4 {
                    break;
                }
                if let Ok(out) = r.run(&ss::get_cmd(ss::CAT_LIST_URL))
                    && out.success()
                {
                    for u in ss::parse_cats(&out.stdout) {
                        let name = u.rsplit('/').next().unwrap_or("cat.jpg").to_string();
                        if !urls.iter().any(|(_, n)| *n == name) {
                            urls.push((u, name));
                        }
                    }
                }
            }
            urls
        }
        Pictures::Photos => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            (0..4)
                .map(|i| {
                    (
                        ss::photo_url(stamp * 10 + i),
                        format!("photo-{stamp}-{i}.jpg"),
                    )
                })
                .collect()
        }
    };
    let mut got = Vec::new();
    for (url, name) in urls {
        let path = dir.join(&name);
        if path.is_file() {
            continue;
        }
        let tmp = dir.join(format!("{name}.part"));
        let ok = r
            .run(&ss::download_cmd(&url, &tmp))
            .is_ok_and(|o| o.success())
            && std::fs::read(&tmp).is_ok_and(|b| b.starts_with(&[0xFF, 0xD8]))
            && std::fs::rename(&tmp, &path).is_ok();
        if ok {
            got.push(path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    ss::trim(&dir);
    got.retain(|p| p.is_file());
    got
}

pub fn fetch_weather() -> Option<Weather> {
    let out = SystemRunner.run(&ss::get_cmd(ss::WEATHER_URL)).ok()?;
    if !out.success() {
        return None;
    }
    ss::parse_weather(&out.stdout)
}

/// A different order each time (no need for a real random source).
fn shuffle(v: &mut [PathBuf]) {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64)
        | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}
