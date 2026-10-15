//! Discover launchable Flatpak and Android (Waydroid) apps from freedesktop
//! `.desktop` files, so they appear in the CatosTV app drawer next to the
//! configured apps.
//!
//! Sources, in priority order (the first one that provides an app id wins):
//!
//! | Source | Directory | Desktop files written by |
//! |---|---|---|
//! | user Flatpak | `~/.local/share/flatpak/exports/share/applications/*.desktop` | `flatpak install --user` |
//! | system Flatpak | `/var/lib/flatpak/exports/share/applications/*.desktop` | `flatpak install` (system) |
//! | Waydroid | `~/.local/share/applications/waydroid.<package>.desktop` | the Waydroid session (`tools/services/user_manager.py`) |
//!
//! Only the `[Desktop Entry]` group is read (Waydroid also writes a
//! `[Desktop Action app-settings]` group with its own `Exec=`). Entries with
//! `NoDisplay=true` (Waydroid marks built-in system apps this way),
//! `Hidden=true` or a `Type` other than `Application` are skipped.
//!
//! Commands are **not** trusted blindly: a Flatpak entry must run
//! `flatpak run … <app-id>` (otherwise `flatpak run <app-id>` is used), and a
//! Waydroid entry must be exactly `waydroid app launch <package>` (the
//! command is rebuilt from the validated package name). Every command is
//! returned in a form accepted by [`apps::split_command`].

use crate::android;
use crate::apps::{self, AppEntry, AppSource};
use crate::error::{Error, Result};
use crate::flatpak;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The parts of a `[Desktop Entry]` group CatosTV uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopEntry {
    /// Untranslated `Name` (`Name[xx]` keys are ignored).
    pub name: String,
    /// Raw `Exec` value (string escapes already decoded).
    pub exec: String,
    pub icon: String,
    /// `Type` (`Application`, `Link`, `Directory`).
    pub kind: String,
    pub no_display: bool,
    pub hidden: bool,
    pub categories: Vec<String>,
    /// `X-Flatpak`, the Flatpak app id, set by Flatpak on exported files.
    pub flatpak_id: Option<String>,
}

impl DesktopEntry {
    /// True for a visible application with a command.
    pub fn is_launchable(&self) -> bool {
        self.kind == "Application"
            && !self.no_display
            && !self.hidden
            && !self.exec.trim().is_empty()
    }
}

/// Decode the desktop-entry string escapes `\s \n \t \r \\`. Other escapes
/// are kept verbatim (the `Exec` quoting layer handles them).
fn unescape_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn parse_bool(v: &str) -> bool {
    matches!(v.trim(), "true" | "1")
}

/// Parse the `[Desktop Entry]` group of a `.desktop` file. Returns `None`
/// when the file has no such group.
pub fn parse_desktop_entry(text: &str) -> Option<DesktopEntry> {
    let mut entry = DesktopEntry::default();
    let mut in_main = false;
    let mut seen_main = false;
    let mut seen_keys: HashSet<String> = HashSet::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let group = line.trim_start_matches('[').trim_end_matches(']');
            in_main = group == "Desktop Entry" && !seen_main;
            seen_main |= in_main;
            continue;
        }
        if !in_main {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        // Localised keys (Name[de]) are ignored; the first occurrence of a
        // key wins (duplicates are invalid per the spec).
        if key.contains('[') || !seen_keys.insert(key.to_string()) {
            continue;
        }
        let value = unescape_value(value.trim());
        match key {
            "Name" => entry.name = value.trim().to_string(),
            "Exec" => entry.exec = value,
            "Icon" => entry.icon = value.trim().to_string(),
            "Type" => entry.kind = value.trim().to_string(),
            "NoDisplay" => entry.no_display = parse_bool(&value),
            "Hidden" => entry.hidden = parse_bool(&value),
            "Categories" => {
                entry.categories = value
                    .split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            }
            "X-Flatpak" => {
                let id = value.trim();
                entry.flatpak_id = (!id.is_empty()).then(|| id.to_string());
            }
            _ => {}
        }
    }
    seen_main.then_some(entry)
}

/// Split an `Exec` value into arguments following the desktop-entry quoting
/// rules (double quotes; `\"`, `` \` ``, `\$`, `\\` escapes inside them).
fn tokenize_exec(exec: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(e) => cur.push(e),
                            None => return Err(Error::msg("unterminated quote in Exec")),
                        },
                        Some(c) => cur.push(c),
                        None => return Err(Error::msg("unterminated quote in Exec")),
                    }
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

/// Remove field codes from one argument: `%%` → `%`, every other `%x` is
/// dropped (CatosTV never passes files or URLs to apps it launches).
fn strip_field_codes(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len());
    let mut chars = arg.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // `%%` is a literal percent sign; %f %F %u %U %i %c %k, the
        // deprecated %d %D %n %N %v %m and unknown codes are dropped.
        if chars.next() == Some('%') {
            out.push('%');
        }
    }
    out
}

/// Quote one argument so [`apps::split_command`] returns it unchanged.
fn quote_arg(a: &str) -> String {
    if !a.is_empty()
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c))
    {
        a.to_string()
    } else if !a.contains('\'') {
        format!("'{a}'")
    } else {
        format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Join argv into a command string accepted by [`apps::split_command`].
pub fn join_command(argv: &[String]) -> String {
    argv.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Turn an `Exec` value into argv: field codes are removed, as are
/// Flatpak's file-forwarding markers `@@u … @@` / `@@ … @@` (only the markers;
/// whatever sat between them was a field code and is removed with it).
pub fn exec_argv(exec: &str) -> Result<Vec<String>> {
    let mut argv = Vec::new();
    for word in tokenize_exec(exec)? {
        if word == "@@" || word == "@@u" {
            continue;
        }
        let cleaned = strip_field_codes(&word);
        if cleaned.is_empty() && !word.is_empty() {
            continue; // the argument was only field codes
        }
        argv.push(cleaned);
    }
    if argv.is_empty() {
        return Err(Error::msg("Exec has no command"));
    }
    Ok(argv)
}

/// [`exec_argv`] rendered as a CatosTV `command` string.
pub fn clean_exec(exec: &str) -> Result<String> {
    let cmd = join_command(&exec_argv(exec)?);
    // Guarantee the result is usable by the lifecycle manager.
    apps::split_command(&cmd)?;
    Ok(cmd)
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// Deterministic accent for a discovered app (FNV-1a over the id).
pub fn accent_for(id: &str) -> &'static str {
    let mut h: u32 = 0x811c_9dc5;
    for b in id.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    apps::ACCENTS[(h as usize) % apps::ACCENTS.len()]
}

fn monogram(name: &str) -> String {
    let m: String = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    if m.is_empty() { "?".into() } else { m }
}

/// Where discovered apps are looked up. Build it with
/// [`DiscoverRoots::for_home`]; tests point it at a temporary tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverRoots {
    /// Flatpak `exports` directories, highest priority first.
    pub flatpak_exports: Vec<PathBuf>,
    /// Flatpak appstream icon directories (`…/appstream/flathub/<arch>/active/icons`).
    pub appstream_icons: Vec<PathBuf>,
    /// Directory holding `waydroid.<package>.desktop` files.
    pub waydroid_applications: PathBuf,
    /// Waydroid's icon cache (`~/.local/share/waydroid/data/icons`).
    pub waydroid_icons: PathBuf,
}

impl DiscoverRoots {
    /// Standard locations for a user whose home directory is `home`
    /// (`$XDG_DATA_HOME` is assumed to be the default `~/.local/share`, as
    /// in the CatosTV session).
    pub fn for_home(home: &Path) -> Self {
        let data = home.join(".local/share");
        Self {
            flatpak_exports: vec![
                data.join("flatpak/exports"),
                PathBuf::from(flatpak::SYSTEM_DIR).join("exports"),
            ],
            appstream_icons: flatpak::appstream_icon_dirs(home),
            waydroid_applications: data.join("applications"),
            waydroid_icons: data.join(android::WAYDROID_ICONS_REL),
        }
    }
}

/// Icon sizes searched in a Flatpak `exports` tree, best first.
const PREFERRED_PNG_SIZES: &[&str] = &["256x256", "128x128", "512x512"];
const SMALL_PNG_SIZES: &[&str] = &["192x192", "96x96", "64x64", "48x48"];

fn is_plain_icon_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

fn existing_png(p: PathBuf) -> Option<PathBuf> {
    (p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) && p.is_file()).then_some(p)
}

/// Resolve the PNG icon of a Flatpak app: an absolute `Icon=` path, then the
/// exported hicolor theme (≥128 px first), then Flathub's appstream cache,
/// then smaller exported sizes, then the exported scalable SVG (the GUI
/// rasterises SVGs with `rsvg-convert`; SVG-only apps
/// showed a letter instead of their icon).
pub fn resolve_flatpak_icon(
    exports: &Path,
    appstream_icons: &[PathBuf],
    icon: &str,
    app_id: &str,
) -> Option<PathBuf> {
    if icon.starts_with('/')
        && let Some(p) = existing_png(PathBuf::from(icon))
    {
        return Some(p);
    }
    let name = if is_plain_icon_name(icon) {
        icon
    } else {
        app_id
    };
    let hicolor = exports.join("share/icons/hicolor");
    let exported = |sizes: &[&str]| {
        sizes
            .iter()
            .find_map(|s| existing_png(hicolor.join(s).join("apps").join(format!("{name}.png"))))
    };
    let appstream = |size: &str| {
        appstream_icons
            .iter()
            .find_map(|d| existing_png(d.join(size).join(format!("{app_id}.png"))))
    };
    exported(PREFERRED_PNG_SIZES)
        .or_else(|| appstream("128x128"))
        .or_else(|| exported(SMALL_PNG_SIZES))
        .or_else(|| appstream("64x64"))
        .or_else(|| {
            Some(hicolor.join("scalable/apps").join(format!("{name}.svg"))).filter(|p| p.is_file())
        })
}

fn desktop_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "desktop"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn file_stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn flatpak_command(exec: &str, app_id: &str) -> String {
    match exec_argv(exec) {
        Ok(argv)
            if basename(&argv[0]) == "flatpak"
                && argv.iter().any(|a| a == "run")
                && argv.iter().any(|a| a == app_id) =>
        {
            join_command(&argv)
        }
        _ => join_command(&["flatpak".into(), "run".into(), app_id.into()]),
    }
}

/// Flatpak apps exported under one `exports` directory, keyed by app id.
fn scan_flatpak_exports(exports: &Path, appstream_icons: &[PathBuf]) -> Vec<(String, AppEntry)> {
    // app id → (entry, file stem equals app id)
    let mut found: HashMap<String, (AppEntry, bool)> = HashMap::new();
    for file in desktop_files(&exports.join("share/applications")) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Some(de) = parse_desktop_entry(&text) else {
            continue;
        };
        if !de.is_launchable() {
            continue;
        }
        let stem = file_stem(&file);
        let app_id = de.flatpak_id.clone().unwrap_or_else(|| stem.clone());
        if !flatpak::is_valid_app_id(&app_id) {
            continue;
        }
        let is_main = stem == app_id;
        if let Some((_, existing_main)) = found.get(&app_id)
            && (*existing_main || !is_main)
        {
            continue;
        }
        let name = if de.name.is_empty() {
            app_id.clone()
        } else {
            de.name.clone()
        };
        let entry = AppEntry {
            id: format!("flatpak-{}", apps::slug(&app_id)),
            icon: monogram(&name),
            name,
            subtitle: "Flatpak".into(),
            accent: String::new(),
            command: flatpak_command(&de.exec, &app_id),
            singleton: true,
            icon_path: resolve_flatpak_icon(exports, appstream_icons, &de.icon, &app_id)
                .map(|p| p.to_string_lossy().into_owned()),
            source: AppSource::Flatpak {
                app_id: app_id.clone(),
            },
        };
        found.insert(app_id, (entry, is_main));
    }
    let mut out: Vec<(String, AppEntry)> = found.into_iter().map(|(k, (e, _))| (k, e)).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Android apps published by Waydroid as `waydroid.<package>.desktop`.
fn scan_waydroid(dir: &Path, icons: &Path) -> Vec<(String, AppEntry)> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for file in desktop_files(dir) {
        let stem = file_stem(&file);
        // Waydroid's own full-UI launcher is `Waydroid.desktop` (and lives in
        // /usr/share/applications); only per-app files match this prefix.
        // TV-only apps (no phone launcher icon) get a file from CatosTV
        // itself, `catostv-android.<package>.desktop`.
        let Some(package) = stem
            .strip_prefix("waydroid.")
            .or_else(|| stem.strip_prefix(android::TV_DESKTOP_PREFIX))
        else {
            continue;
        };
        if !android::is_valid_package(package)
            || seen.contains(package)
            || android::is_hidden_package(package)
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Some(de) = parse_desktop_entry(&text) else {
            continue;
        };
        if !de.is_launchable() {
            continue;
        }
        let expected = ["waydroid", "app", "launch", package];
        match exec_argv(&de.exec) {
            Ok(argv)
                if argv.len() == expected.len()
                    && basename(&argv[0]) == "waydroid"
                    && argv[1..] == expected[1..] => {}
            _ => continue,
        }
        let icon_path = de
            .icon
            .starts_with('/')
            .then(|| PathBuf::from(&de.icon))
            .and_then(existing_png)
            .or_else(|| existing_png(icons.join(format!("{package}.png"))));
        let name = if de.name.is_empty() {
            package.to_string()
        } else {
            de.name.clone()
        };
        seen.insert(package.to_string());
        out.push((
            package.to_string(),
            AppEntry {
                id: format!("android-{}", apps::slug(package)),
                icon: monogram(&name),
                name,
                subtitle: "Android".into(),
                accent: String::new(),
                command: join_command(&android::launch_argv(package)),
                singleton: true,
                icon_path: icon_path.map(|p| p.to_string_lossy().into_owned()),
                source: AppSource::Android {
                    package: package.to_string(),
                },
            },
        ));
    }
    out
}

/// Discover Flatpak and Waydroid apps for the user whose home is `home`.
pub fn discover(home: &Path) -> Vec<AppEntry> {
    discover_in(&DiscoverRoots::for_home(home))
}

/// Discover apps under explicit roots. Result: sorted by name; ids are
/// `flatpak-<slug>` / `android-<slug>` (never colliding with configured apps,
/// whose ids are plain slugs of their names) and unique.
pub fn discover_in(roots: &DiscoverRoots) -> Vec<AppEntry> {
    let mut seen_flatpak: HashSet<String> = HashSet::new();
    let mut apps_out: Vec<AppEntry> = Vec::new();
    for exports in &roots.flatpak_exports {
        for (app_id, entry) in scan_flatpak_exports(exports, &roots.appstream_icons) {
            if seen_flatpak.insert(app_id) {
                apps_out.push(entry);
            }
        }
    }
    apps_out.extend(
        scan_waydroid(&roots.waydroid_applications, &roots.waydroid_icons)
            .into_iter()
            .map(|(_, e)| e),
    );
    apps_out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut ids: HashSet<String> = HashSet::new();
    for app in &mut apps_out {
        let base = app.id.clone();
        let mut n = 2;
        while !ids.insert(app.id.clone()) {
            app.id = format!("{base}-{n}");
            n += 1;
        }
        app.accent = accent_for(&app.id).to_string();
    }
    apps_out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const KODI: &str = "[Desktop Entry]
Version=1.0
Name=Kodi
Name[de]=Kodi DE
GenericName=Media Center
Comment=Manage and view your media
Exec=/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=kodi --file-forwarding tv.kodi.Kodi @@u %U @@
Icon=tv.kodi.Kodi
Terminal=false
Type=Application
Categories=AudioVideo;Video;Player;TV;
X-Flatpak=tv.kodi.Kodi
";

    /// As written by Waydroid 1.6 (GLib.KeyFile), note the second group
    /// with its own Exec, which must not override the main one.
    fn waydroid_file(pkg: &str, name: &str, home: &str, extra: &str) -> String {
        format!(
            "[Desktop Entry]
Type=Application
Name={name}
Exec=waydroid app launch {pkg}
Icon={home}/.local/share/waydroid/data/icons/{pkg}.png
Categories=X-WayDroid-App;
X-Purism-FormFactor=Workstation;Mobile;
Actions=app-settings;
{extra}
[Desktop Action app-settings]
Name=App Settings
Exec=waydroid app intent android.settings.APPLICATION_DETAILS_SETTINGS package:{pkg}
Icon={home}/.local/share/waydroid/data/icons/com.android.settings.png
"
        )
    }

    #[test]
    fn parses_main_group_only() {
        let de = parse_desktop_entry(KODI).unwrap();
        assert_eq!(de.name, "Kodi");
        assert_eq!(de.icon, "tv.kodi.Kodi");
        assert_eq!(de.kind, "Application");
        assert_eq!(de.flatpak_id.as_deref(), Some("tv.kodi.Kodi"));
        assert_eq!(de.categories, ["AudioVideo", "Video", "Player", "TV"]);
        assert!(de.is_launchable());

        let w = waydroid_file("com.aurora.store", "Aurora Store", "/h", "");
        let de = parse_desktop_entry(&w).unwrap();
        assert_eq!(de.exec, "waydroid app launch com.aurora.store");
        assert_eq!(de.name, "Aurora Store");

        assert!(parse_desktop_entry("[Other]\nName=x\n").is_none());
        let hidden = parse_desktop_entry(
            "[Desktop Entry]\nType=Application\nName=a\nExec=a\nNoDisplay=true\n",
        )
        .unwrap();
        assert!(!hidden.is_launchable());
        let link = parse_desktop_entry("[Desktop Entry]\nType=Link\nName=a\nExec=a\n").unwrap();
        assert!(!link.is_launchable());
    }

    #[test]
    fn exec_cleaning() {
        assert_eq!(
            clean_exec("/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=kodi --file-forwarding tv.kodi.Kodi @@u %U @@").unwrap(),
            "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=kodi --file-forwarding tv.kodi.Kodi"
        );
        assert_eq!(clean_exec("app @@ %F @@ --x").unwrap(), "app --x");
        assert_eq!(
            clean_exec("vlc --started-from-file %U").unwrap(),
            "vlc --started-from-file"
        );
        assert_eq!(
            clean_exec("app --rate=100%% %i %c %k").unwrap(),
            "app --rate=100%"
        );
        assert_eq!(
            clean_exec(r#"sh-free "/opt/My App/run" "say \"hi\"""#).unwrap(),
            r#"sh-free '/opt/My App/run' 'say "hi"'"#
        );
        assert!(clean_exec("%U").is_err());
        assert!(clean_exec("\"open").is_err());
    }

    #[test]
    fn cleaned_commands_round_trip_through_split_command() {
        for exec in [
            "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=com.stremio.Stremio com.stremio.Stremio @@u %U @@",
            r#""/opt/a b/c" "it's" "x;y" "a|b" "back\\slash""#,
            "app %f --opt=%%HOME",
        ] {
            let argv = exec_argv(exec).unwrap();
            let cmd = clean_exec(exec).unwrap();
            assert_eq!(apps::split_command(&cmd).unwrap(), argv, "{exec}");
        }
    }

    #[test]
    fn accents_are_deterministic_and_valid() {
        let a = accent_for("flatpak-tv-kodi-kodi");
        assert_eq!(a, accent_for("flatpak-tv-kodi-kodi"));
        assert!(apps::ACCENTS.contains(&a));
        let distinct: HashSet<&str> = (0..40).map(|i| accent_for(&format!("id{i}"))).collect();
        assert!(distinct.len() > 1);
    }

    fn png(t: &TempDir, rel: &str) -> String {
        t.write(rel, "\u{89}PNG").to_string_lossy().into_owned()
    }

    fn roots(t: &TempDir) -> DiscoverRoots {
        DiscoverRoots {
            flatpak_exports: vec![
                t.path().join("user/exports"),
                t.path().join("system/exports"),
            ],
            appstream_icons: vec![t.path().join("user/appstream/flathub/x86_64/active/icons")],
            waydroid_applications: t.path().join("home/.local/share/applications"),
            waydroid_icons: t.path().join("home/.local/share/waydroid/data/icons"),
        }
    }

    #[test]
    fn discovers_flatpak_and_waydroid_apps() {
        let t = TempDir::new("discover");
        let home = t.path().join("home").to_string_lossy().into_owned();
        // User Flatpak Kodi (exported 256 px icon) and an extra launcher of
        // the same app that must be deduplicated.
        t.write("user/exports/share/applications/tv.kodi.Kodi.desktop", KODI);
        t.write(
            "user/exports/share/applications/tv.kodi.Kodi.Extra.desktop",
            "[Desktop Entry]\nType=Application\nName=Kodi Extra\nExec=flatpak run tv.kodi.Kodi\nX-Flatpak=tv.kodi.Kodi\n",
        );
        let kodi_icon = png(
            &t,
            "user/exports/share/icons/hicolor/256x256/apps/tv.kodi.Kodi.png",
        );
        // System Kodi (user wins) and system Stremio with only an SVG
        // exported but an appstream PNG.
        t.write(
            "system/exports/share/applications/tv.kodi.Kodi.desktop",
            &KODI.replace("Name=Kodi\n", "Name=System Kodi\n"),
        );
        t.write(
            "system/exports/share/applications/com.stremio.Stremio.desktop",
            "[Desktop Entry]\nType=Application\nName=Stremio\nExec=/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=stremio com.stremio.Stremio\nIcon=com.stremio.Stremio\nX-Flatpak=com.stremio.Stremio\n",
        );
        t.write(
            "system/exports/share/icons/hicolor/scalable/apps/com.stremio.Stremio.svg",
            "<svg/>",
        );
        let stremio_icon = png(
            &t,
            "user/appstream/flathub/x86_64/active/icons/128x128/com.stremio.Stremio.png",
        );
        // A suspicious Flatpak export whose Exec is not `flatpak run`.
        t.write(
            "system/exports/share/applications/org.example.Evil.desktop",
            "[Desktop Entry]\nType=Application\nName=Evil\nExec=rm -rf /\nX-Flatpak=org.example.Evil\n",
        );
        // Waydroid: one visible app with icon, one NoDisplay system app,
        // Waydroid's full-UI launcher and an entry with a tampered Exec.
        let w = "home/.local/share/applications";
        t.write(
            &format!("{w}/waydroid.com.aurora.store.desktop"),
            &waydroid_file("com.aurora.store", "Aurora Store", &home, ""),
        );
        let aurora_icon = png(
            &t,
            "home/.local/share/waydroid/data/icons/com.aurora.store.png",
        );
        t.write(
            &format!("{w}/waydroid.com.android.settings.desktop"),
            &waydroid_file(
                "com.android.settings",
                "Settings",
                &home,
                "NoDisplay=true\n",
            ),
        );
        t.write(
            &format!("{w}/Waydroid.desktop"),
            "[Desktop Entry]\nType=Application\nName=Waydroid\nExec=waydroid\n",
        );
        t.write(
            &format!("{w}/waydroid.org.bad.app.desktop"),
            "[Desktop Entry]\nType=Application\nName=Bad\nExec=sh -c evil\n",
        );
        t.write(
            &format!("{w}/waydroid.org.videolan.vlc.desktop"),
            &waydroid_file("org.videolan.vlc", "VLC", "/nonexistent", ""),
        );
        t.write(
            &format!("{w}/firefox.desktop"),
            "[Desktop Entry]\nType=Application\nName=FF\nExec=firefox\n",
        );

        let apps = discover_in(&roots(&t));
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            ["Aurora Store", "Evil", "Kodi", "Stremio", "VLC"],
            "{apps:#?}"
        );

        let kodi = apps.iter().find(|a| a.name == "Kodi").unwrap();
        assert_eq!(kodi.id, "flatpak-tv-kodi-kodi");
        assert_eq!(kodi.subtitle, "Flatpak");
        assert_eq!(kodi.icon, "K");
        assert!(apps::ACCENTS.contains(&kodi.accent.as_str()));
        assert_eq!(kodi.icon_path.as_deref(), Some(kodi_icon.as_str()));
        assert_eq!(
            kodi.source,
            AppSource::Flatpak {
                app_id: "tv.kodi.Kodi".into()
            }
        );
        assert_eq!(
            kodi.argv().unwrap(),
            [
                "/usr/bin/flatpak",
                "run",
                "--branch=stable",
                "--arch=x86_64",
                "--command=kodi",
                "--file-forwarding",
                "tv.kodi.Kodi"
            ]
        );

        let stremio = apps.iter().find(|a| a.name == "Stremio").unwrap();
        assert_eq!(stremio.icon_path.as_deref(), Some(stremio_icon.as_str()));

        let evil = apps.iter().find(|a| a.name == "Evil").unwrap();
        assert_eq!(evil.argv().unwrap(), ["flatpak", "run", "org.example.Evil"]);

        let aurora = apps.iter().find(|a| a.name == "Aurora Store").unwrap();
        assert_eq!(aurora.id, "android-com-aurora-store");
        assert_eq!(aurora.subtitle, "Android");
        assert_eq!(aurora.icon_path.as_deref(), Some(aurora_icon.as_str()));
        assert_eq!(
            aurora.argv().unwrap(),
            ["waydroid", "app", "launch", "com.aurora.store"]
        );
        assert_eq!(
            aurora.source,
            AppSource::Android {
                package: "com.aurora.store".into()
            }
        );

        let vlc = apps.iter().find(|a| a.name == "VLC").unwrap();
        assert_eq!(vlc.icon_path, None, "missing icon file → monogram");
        assert_eq!(vlc.icon, "V");

        // Every command is acceptable to the lifecycle manager.
        for a in &apps {
            assert!(apps::split_command(&a.command).is_ok(), "{}", a.command);
        }
    }

    #[test]
    fn missing_directories_yield_nothing() {
        let t = TempDir::new("discover-empty");
        assert!(discover_in(&roots(&t)).is_empty());
    }

    #[test]
    fn colliding_slugs_get_unique_ids() {
        let t = TempDir::new("discover-collide");
        for id in ["org.Foo.App", "org.foo.app"] {
            t.write(
                &format!("user/exports/share/applications/{id}.desktop"),
                &format!("[Desktop Entry]\nType=Application\nName=Foo\nExec=flatpak run {id}\n"),
            );
        }
        let apps = discover_in(&roots(&t));
        assert_eq!(apps.len(), 2);
        assert_ne!(apps[0].id, apps[1].id);
    }

    #[test]
    fn icon_resolution_order() {
        let t = TempDir::new("icons");
        let exports = t.path().join("exports");
        let appstream = vec![t.path().join("as")];
        assert_eq!(
            resolve_flatpak_icon(&exports, &appstream, "a.b.C", "a.b.C"),
            None
        );
        let small = png(&t, "exports/share/icons/hicolor/64x64/apps/a.b.C.png");
        assert_eq!(
            resolve_flatpak_icon(&exports, &appstream, "a.b.C", "a.b.C")
                .unwrap()
                .to_string_lossy(),
            small
        );
        let store = png(&t, "as/128x128/a.b.C.png");
        assert_eq!(
            resolve_flatpak_icon(&exports, &appstream, "a.b.C", "a.b.C")
                .unwrap()
                .to_string_lossy(),
            store
        );
        let big = png(&t, "exports/share/icons/hicolor/128x128/apps/a.b.C.png");
        assert_eq!(
            resolve_flatpak_icon(&exports, &appstream, "a.b.C", "a.b.C")
                .unwrap()
                .to_string_lossy(),
            big
        );
        // Absolute Icon= wins; path-like or weird names fall back to the id.
        let abs = png(&t, "elsewhere/icon.png");
        assert_eq!(
            resolve_flatpak_icon(&exports, &appstream, &abs, "a.b.C")
                .unwrap()
                .to_string_lossy(),
            abs
        );
        assert_eq!(
            resolve_flatpak_icon(&exports, &appstream, "../../x", "a.b.C")
                .unwrap()
                .to_string_lossy(),
            big
        );
    }
}
