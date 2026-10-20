//! App registry model.
//!
//! Apps are configured in `~/.config/catostv/apps.toml` (TOML parsing lives in
//! the GUI crate, which converts each entry into an [`AppSpec`]). This module
//! validates specs into [`AppEntry`]s and turns the configured `command`
//! string into an argv list.
//!
//! Commands are **never run through a shell**. `command` is split with
//! POSIX-like quoting rules ([`split_command`]); the only expansions are a
//! leading `~/` and the literal `$HOME`. This keeps a config typo from
//! becoming a shell-injection bug and lets the lifecycle manager track the
//! real process instead of a wrapper shell.

use crate::error::{Error, Result};

/// One `[[app]]` entry as written by a user, before validation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppSpec {
    pub id: Option<String>,
    pub name: String,
    pub subtitle: Option<String>,
    pub icon: Option<String>,
    pub accent: Option<String>,
    pub command: String,
    /// Launch at most one instance; selecting a running app resumes it.
    /// Defaults to `true`.
    pub singleton: Option<bool>,
}

/// A validated app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// Stable identifier (slug of the name unless set explicitly).
    pub id: String,
    pub name: String,
    pub subtitle: String,
    /// Icon keyword (`play`, `media`, `game`, `music`, `stream`, `browser`,
    /// `tv`) or a 1–2 character monogram.
    pub icon: String,
    /// Accent name: `violet`, `red`, `blue`, `green`, `sky`, `orange`.
    pub accent: String,
    pub command: String,
    pub singleton: bool,
    /// Absolute path of a PNG icon (discovered Flatpak / Android apps).
    /// `None` → the GUI draws [`AppEntry::icon`] as a vector icon/monogram.
    pub icon_path: Option<String>,
    /// Where the entry came from.
    pub source: AppSource,
}

/// Origin of an [`AppEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AppSource {
    /// `apps.toml` or the built-in defaults.
    #[default]
    Configured,
    /// An installed Flatpak (`flatpak run <app_id>`).
    Flatpak { app_id: String },
    /// An Android app inside Waydroid (`waydroid app launch <package>`).
    Android { package: String },
}

impl AppEntry {
    /// The argv to execute, with `~/` and `$HOME` expanded.
    pub fn argv(&self) -> Result<Vec<String>> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/catostv".into());
        let argv: Vec<String> = split_command(&self.command)?
            .into_iter()
            .map(|a| expand_home(&a, &home))
            .collect();
        if argv.is_empty() {
            return Err(Error::msg(format!("{} has an empty command", self.name)));
        }
        Ok(argv)
    }

    /// True when the program in `command` exists on `$PATH`.
    pub fn is_installed(&self) -> bool {
        if self.is_builtin() {
            return true;
        }
        self.argv()
            .ok()
            .and_then(|a| crate::process::find_executable(&a[0]))
            .is_some()
    }

    /// A screen built into CatosTV (`catostv:<name>`, e.g. Catfyn), not a program.
    pub fn is_builtin(&self) -> bool {
        self.command.starts_with(BUILTIN_PREFIX)
    }

    /// The executable name, for messages.
    pub fn program(&self) -> String {
        self.argv()
            .ok()
            .and_then(|a| a.into_iter().next())
            .unwrap_or_default()
    }
}

fn expand_home(arg: &str, home: &str) -> String {
    let arg = if arg == "~" {
        home.to_string()
    } else if let Some(rest) = arg.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        arg.to_string()
    };
    arg.replace("$HOME", home)
}

/// Split a command line into words.
///
/// Supported: whitespace separation, `'single quotes'` (literal),
/// `"double quotes"` (with `\"` and `\\` escapes), and backslash escapes
/// outside quotes. Not supported (deliberately): variables other than
/// `$HOME`, globs, pipes, redirection, `;`, `&&`.
pub fn split_command(s: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err(Error::msg("unterminated ' quote in command")),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(e @ ('"' | '\\')) => cur.push(e),
                            Some(other) => {
                                cur.push('\\');
                                cur.push(other);
                            }
                            None => return Err(Error::msg("unterminated \" quote in command")),
                        },
                        Some(c) => cur.push(c),
                        None => return Err(Error::msg("unterminated \" quote in command")),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => cur.push(c),
                    None => return Err(Error::msg("command ends with a backslash")),
                }
            }
            '|' | ';' | '&' | '<' | '>' | '`' => {
                return Err(Error::msg(format!(
                    "'{c}' is shell syntax; CatosTV runs commands without a shell. \
                     Put complex launch logic in a script and point `command` at it"
                )));
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

/// Lowercase ASCII slug: "Moonlight PC" → "moonlight-pc".
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() { "app".into() } else { out }
}

pub const ACCENTS: &[&str] = &["violet", "red", "blue", "green", "sky", "orange"];

/// Validate user specs. Invalid entries are skipped with a warning instead of
/// breaking the whole registry; duplicate ids get a numeric suffix.
pub fn validate(specs: Vec<AppSpec>) -> (Vec<AppEntry>, Vec<String>) {
    let mut apps: Vec<AppEntry> = Vec::new();
    let mut warnings = Vec::new();
    for spec in specs {
        let name = spec.name.trim().to_string();
        if name.is_empty() {
            warnings.push("skipped an app with no name".to_string());
            continue;
        }
        match split_command(&spec.command) {
            Ok(words) if words.is_empty() => {
                warnings.push(format!("skipped {name}: empty command"));
                continue;
            }
            Err(e) => {
                warnings.push(format!("skipped {name}: {e}"));
                continue;
            }
            Ok(_) => {}
        }
        let base = spec.id.as_deref().map(slug).unwrap_or_else(|| slug(&name));
        let mut id = base.clone();
        let mut n = 2;
        while apps.iter().any(|a| a.id == id) {
            id = format!("{base}-{n}");
            n += 1;
        }
        let accent = spec
            .accent
            .map(|a| a.trim().to_ascii_lowercase())
            .filter(|a| ACCENTS.contains(&a.as_str()))
            .unwrap_or_else(|| "violet".into());
        let icon = spec
            .icon
            .map(|i| i.trim().to_string())
            .filter(|i| !i.is_empty())
            .unwrap_or_else(|| name.chars().take(1).collect::<String>().to_uppercase());
        apps.push(AppEntry {
            id,
            name,
            subtitle: spec.subtitle.unwrap_or_default(),
            icon,
            accent,
            command: spec.command,
            singleton: spec.singleton.unwrap_or(true),
            icon_path: None,
            source: AppSource::Configured,
        });
    }
    (apps, warnings)
}

/// Built-in registry used when `apps.toml` is missing, empty or invalid.
/// Keep in sync with `target-root/home/catostv/.config/catostv/apps.toml`
/// (a unit test in the GUI crate enforces this).
/// Commands of CatosTV's built-in screens.
pub const BUILTIN_PREFIX: &str = "catostv:";
/// Catfyn, CatosTV's Jellyfin client (`crate::fin`).
pub const FIN_COMMAND: &str = "catostv:fin";
/// The built-in Files app.
pub const FILES_COMMAND: &str = "catostv:files";

pub fn default_specs() -> Vec<AppSpec> {
    let spec = |name: &str, subtitle: &str, icon: &str, accent: &str, command: &str| AppSpec {
        id: None,
        name: name.into(),
        subtitle: Some(subtitle.into()),
        icon: Some(icon.into()),
        accent: Some(accent.into()),
        command: command.into(),
        singleton: None,
    };
    vec![
        // Catfyn: CatosTV's own Jellyfin client,
        // built into the shell; replaces the Jellyfin Media Player wrapper.
        spec("Catfyn", "Jellyfin", "catfyn", "violet", FIN_COMMAND),
        spec("Steam", "Games", "game", "blue", "steam -gamepadui"),
        // No Spotify by default (the Linux app is a web
        // wrapper); it stays available in the Store.
        spec("Moonlight", "PC streaming", "stream", "sky", "moonlight"),
        // CatosTV's own file browser, GNOME Files was unusable with
        // a remote.
        spec(
            "Files",
            "Drives & downloads",
            "folder",
            "orange",
            FILES_COMMAND,
        ),
        // No browser apps: YouTube-in-a-browser was not
        // a good TV experience; YouTube comes from the Store / Android.
    ]
}

pub fn defaults() -> Vec<AppEntry> {
    validate(default_specs()).0
}

/// The app list the shell shows: configured apps first, then discovered
/// (Flatpak / Android) apps. A discovered app with the same id or the same
/// name (ignoring case) as a configured one is the same app installed from
/// the Store: it is dropped when the configured one is installed, otherwise
/// it takes the configured entry's place (so "Moonlight" never shows twice).
/// `installed` tells whether a configured entry's program exists.
pub fn merge(
    configured: &[AppEntry],
    discovered: &[AppEntry],
    installed: impl Fn(&AppEntry) -> bool,
) -> Vec<AppEntry> {
    let mut apps = configured.to_vec();
    for d in discovered {
        let same = apps
            .iter()
            .position(|a| a.id == d.id || a.name.eq_ignore_ascii_case(&d.name));
        match same {
            None => apps.push(d.clone()),
            Some(i) if apps[i].source == AppSource::Configured && !installed(&apps[i]) => {
                apps[i] = d.clone()
            }
            Some(_) => {}
        }
    }
    apps
}

/// Put `apps` in the user's `order` (app ids, Home → hold OK → move).
/// Apps not in `order` (new ones) follow, in the order they came.
pub fn apply_order(apps: &mut [AppEntry], order: &[String]) {
    let rank = |a: &AppEntry| {
        order
            .iter()
            .position(|id| *id == a.id)
            .unwrap_or(usize::MAX)
    };
    apps.sort_by_key(rank);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_order_first_new_apps_after() {
        let mut apps = defaults();
        let ids: Vec<String> = apps.iter().map(|a| a.id.clone()).collect();
        assert!(ids.len() >= 3);
        let order = vec![ids[2].clone(), "gone.app".into(), ids[0].clone()];
        apply_order(&mut apps, &order);
        let got: Vec<&str> = apps.iter().map(|a| a.id.as_str()).collect();
        let mut want = vec![ids[2].as_str(), ids[0].as_str()];
        want.extend(
            ids.iter()
                .map(String::as_str)
                .filter(|id| *id != ids[0] && *id != ids[2]),
        );
        assert_eq!(got, want);
    }

    #[test]
    fn merge_dedups_store_copies_of_configured_apps() {
        let conf = defaults();
        let moon = conf
            .iter()
            .find(|a| a.name == "Moonlight")
            .cloned()
            .expect("Moonlight is a default app");
        let mut flat = moon.clone();
        flat.id = "com.moonlight_stream.Moonlight".into();
        flat.name = "moonlight".into();
        flat.source = AppSource::Flatpak {
            app_id: flat.id.clone(),
        };
        let mut other = flat.clone();
        other.id = "org.example.New".into();
        other.name = "New".into();
        let disc = vec![flat.clone(), other.clone()];

        // Native Moonlight installed: the Flatpak copy is hidden.
        let m = merge(&conf, &disc, |_| true);
        assert_eq!(m.len(), conf.len() + 1);
        assert!(m.contains(&other));
        // Not installed natively: the Flatpak copy takes its place.
        let m = merge(&conf, &disc, |_| false);
        assert_eq!(m.len(), conf.len() + 1);
        let pos = conf.iter().position(|a| a.id == moon.id).unwrap();
        assert_eq!(m[pos], flat);
    }

    #[test]
    fn split_basic_and_quotes() {
        assert_eq!(
            split_command("firefox --kiosk https://x.y/tv").unwrap(),
            ["firefox", "--kiosk", "https://x.y/tv"]
        );
        assert_eq!(
            split_command(r#"app 'a b' "c \"d\"" e\ f"#).unwrap(),
            ["app", "a b", "c \"d\"", "e f"]
        );
        assert_eq!(split_command("  ").unwrap(), Vec::<String>::new());
        assert_eq!(split_command("x ''").unwrap(), ["x", ""]);
    }

    #[test]
    fn split_rejects_shell_syntax_and_bad_quotes() {
        assert!(split_command("a; rm -rf /").is_err());
        assert!(split_command("a | b").is_err());
        assert!(split_command("a && b").is_err());
        assert!(split_command("'open").is_err());
        assert!(split_command("\"open").is_err());
        assert!(split_command("trailing\\").is_err());
        // Quoted shell characters are fine.
        assert_eq!(split_command("echo 'a;b'").unwrap(), ["echo", "a;b"]);
    }

    #[test]
    fn home_expansion() {
        assert_eq!(expand_home("~/x", "/h"), "/h/x");
        assert_eq!(expand_home("~", "/h"), "/h");
        assert_eq!(expand_home("--p=$HOME/y", "/h"), "--p=/h/y");
        assert_eq!(expand_home("a~b", "/h"), "a~b");
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Moonlight PC"), "moonlight-pc");
        assert_eq!(slug("  YouTube!! "), "youtube");
        assert_eq!(slug("✨"), "app");
    }

    #[test]
    fn validate_skips_bad_entries_and_dedupes_ids() {
        let specs = vec![
            AppSpec {
                name: "A".into(),
                command: "a".into(),
                ..AppSpec::default()
            },
            AppSpec {
                name: "A".into(),
                command: "a2".into(),
                accent: Some("Neon".into()),
                ..AppSpec::default()
            },
            AppSpec {
                name: "".into(),
                command: "x".into(),
                ..AppSpec::default()
            },
            AppSpec {
                name: "Bad".into(),
                command: "x; y".into(),
                ..AppSpec::default()
            },
            AppSpec {
                name: "Empty".into(),
                command: " ".into(),
                ..AppSpec::default()
            },
        ];
        let (apps, warnings) = validate(specs);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].id, "a");
        assert_eq!(apps[1].id, "a-2");
        assert_eq!(apps[1].accent, "violet");
        assert_eq!(apps[0].icon, "A");
        assert!(apps[0].singleton);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
    }

    #[test]
    fn defaults_are_valid_and_unique() {
        let (apps, warnings) = validate(default_specs());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(apps.len(), 4);
        let ids: std::collections::HashSet<_> = apps.iter().map(|a| a.id.clone()).collect();
        assert_eq!(ids.len(), 4);
        // A ready-made file manager.
        assert!(apps.iter().any(|a| a.command == FILES_COMMAND));
        // No browser apps in the drawer.
        assert!(apps.iter().all(|a| a.program() != "firefox"));
    }
}
