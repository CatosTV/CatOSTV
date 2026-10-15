//! App icons from the **Papirus** icon theme (Arch package
//! `papirus-icon-theme`).
//!
//! Papirus ships SVG only. The GUI draws PNG, so an SVG is rasterised once
//! with `rsvg-convert` (package `librsvg`) into `~/.cache/catostv/icons/` and
//! the PNG is used from then on. Everything here is pure path logic plus the
//! [`Cmd`] for the conversion; the GUI runs it off the UI thread.

use crate::apps::{AppEntry, AppSource};
use crate::process::Cmd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where Arch installs Papirus.
pub const PAPIRUS_ROOT: &str = "/usr/share/icons/Papirus";
/// Rasterised size in pixels (sharp up to a 4K tile).
pub const RASTER_PX: u32 = 256;

/// Papirus directories searched, largest first. `apps` icons are the
/// colourful application icons.
const SIZES: &[&str] = &["128x128", "96x96", "64x64", "48x48"];

/// Theme icon names to try for an app, best first.
pub fn icon_names(app: &AppEntry) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let s = s.trim();
        if !s.is_empty() && !v.iter().any(|x| x == s) {
            v.push(s.to_string());
        }
    };
    match &app.source {
        AppSource::Flatpak { app_id } => {
            push(app_id);
            push(&app_id.to_lowercase());
            if let Some(last) = app_id.rsplit('.').next() {
                push(&last.to_lowercase());
            }
        }
        // Only the full package name: its last segment ("store" for
        // com.aurora.store) would match unrelated Papirus icons.
        AppSource::Android { package } => push(package),
        AppSource::Configured => {
            let program = app.program();
            let base = program.rsplit('/').next().unwrap_or(&program).to_string();
            for alias in aliases(&base) {
                push(alias);
            }
            push(&base);
            push(&app.id);
        }
    }
    push(&app.name.to_lowercase().replace(' ', "-"));
    v
}

/// Known program → Papirus name differences.
fn aliases(program: &str) -> &'static [&'static str] {
    match program {
        "jellyfinmediaplayer" => &["jellyfin-media-player", "jellyfin"],
        "spotify-launcher" => &["spotify"],
        "moonlight" => &["moonlight", "com.moonlight_stream.Moonlight"],
        "steam" => &["steam"],
        "kodi" => &["kodi"],
        _ => &[],
    }
}

/// The first existing Papirus SVG for `names` under `root`.
pub fn find(root: &Path, names: &[String]) -> Option<PathBuf> {
    for name in names {
        if name.contains('/') || name.starts_with('.') {
            continue;
        }
        for size in SIZES {
            let p = root.join(size).join("apps").join(format!("{name}.svg"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Papirus icon for an app, if the theme has one.
pub fn papirus_for(app: &AppEntry) -> Option<PathBuf> {
    find(Path::new(PAPIRUS_ROOT), &icon_names(app))
}

/// Cache location of the rasterised PNG for `svg`.
pub fn cached_png(cache_dir: &Path, svg: &str) -> PathBuf {
    let flat: String = svg
        .trim_start_matches('/')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cache_dir.join(format!("{flat}-{RASTER_PX}.png"))
}

/// `~/.cache/catostv/icons` for `home`.
pub fn cache_dir(home: &Path) -> PathBuf {
    home.join(".cache/catostv/icons")
}

/// Rasterise `svg` to `png` (written to a temporary name first by the
/// caller, then renamed, so a half-written PNG is never read).
pub fn rsvg_cmd(svg: &str, png: &str) -> Cmd {
    Cmd::new("rsvg-convert")
        .args([
            "-w".to_string(),
            RASTER_PX.to_string(),
            "-h".to_string(),
            RASTER_PX.to_string(),
            "-o".to_string(),
            png.to_string(),
            svg.to_string(),
        ])
        .timeout(Duration::from_secs(10))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn entry(command: &str, source: AppSource) -> AppEntry {
        AppEntry {
            id: "jellyfin".into(),
            name: "Jellyfin".into(),
            subtitle: String::new(),
            icon: "media".into(),
            accent: "violet".into(),
            command: command.into(),
            singleton: true,
            icon_path: None,
            source,
        }
    }

    #[test]
    fn names_cover_aliases_and_ids() {
        let n = icon_names(&entry("jellyfinmediaplayer --tv", AppSource::Configured));
        assert_eq!(n[0], "jellyfin-media-player");
        assert!(n.contains(&"jellyfin".to_string()));
        let f = icon_names(&entry(
            "flatpak run tv.kodi.Kodi",
            AppSource::Flatpak {
                app_id: "tv.kodi.Kodi".into(),
            },
        ));
        assert_eq!(f[..3], ["tv.kodi.Kodi", "tv.kodi.kodi", "kodi"]);
        let a = icon_names(&entry(
            "waydroid app launch com.aurora.store",
            AppSource::Android {
                package: "com.aurora.store".into(),
            },
        ));
        assert_eq!(a[0], "com.aurora.store");
        assert!(!a.contains(&"store".to_string()));
    }

    #[test]
    fn finds_largest_existing_svg() {
        let t = TempDir::new("papirus");
        let root = t.path().to_path_buf();
        for size in ["48x48", "128x128"] {
            let d = root.join(size).join("apps");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("kodi.svg"), "<svg/>").unwrap();
        }
        let got = find(&root, &["nope".into(), "kodi".into()]).unwrap();
        assert!(got.ends_with("128x128/apps/kodi.svg"));
        assert_eq!(find(&root, &["../etc/x".into()]), None);
    }

    #[test]
    fn cache_name_and_command() {
        let p = cached_png(
            Path::new("/c"),
            "/usr/share/icons/Papirus/64x64/apps/kodi.svg",
        );
        assert_eq!(
            p,
            Path::new("/c/usr_share_icons_Papirus_64x64_apps_kodi.svg-256.png")
        );
        let c = rsvg_cmd("/a.svg", "/b.png");
        assert_eq!(c.display(), "rsvg-convert -w 256 -h 256 -o /b.png /a.svg");
    }
}
