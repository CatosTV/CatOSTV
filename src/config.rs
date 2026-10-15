//! Loading `~/.config/catostv/apps.toml` (TOML parsing needs the `toml`
//! crate, so it lives here rather than in the dependency-free core).

use catostv_core::apps::{self, AppEntry, AppSpec};
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
struct AppFile {
    #[serde(default)]
    app: Vec<AppToml>,
}

#[derive(Deserialize)]
struct AppToml {
    id: Option<String>,
    name: String,
    subtitle: Option<String>,
    icon: Option<String>,
    accent: Option<String>,
    command: String,
    singleton: Option<bool>,
}

impl From<AppToml> for AppSpec {
    fn from(a: AppToml) -> Self {
        AppSpec {
            id: a.id,
            name: a.name,
            subtitle: a.subtitle,
            icon: a.icon,
            accent: a.accent,
            command: a.command,
            singleton: a.singleton,
        }
    }
}

/// Parse an apps.toml document into validated apps plus warnings.
pub fn parse_apps(text: &str) -> Result<(Vec<AppEntry>, Vec<String>), String> {
    let file: AppFile = toml::from_str(text).map_err(|e| e.to_string())?;
    Ok(apps::validate(
        file.app.into_iter().map(AppSpec::from).collect(),
    ))
}

/// Load the registry. Falls back to the built-in defaults when the file is
/// missing, unreadable, invalid or yields no usable apps; the reason is
/// returned as a warning so the shell can show it.
pub fn load_apps(path: &Path) -> (Vec<AppEntry>, Vec<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (apps::defaults(), vec![]),
    };
    match parse_apps(&text) {
        Ok((list, warnings)) if !list.is_empty() => (list, warnings),
        Ok((_, mut warnings)) => {
            warnings.push("apps.toml has no usable apps; using defaults".into());
            (apps::defaults(), warnings)
        }
        Err(e) => (
            apps::defaults(),
            vec![format!("apps.toml is invalid ({e}); using defaults")],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_registry_parses_to_defaults() {
        let text = include_str!("../target-root/home/catostv/.config/catostv/apps.toml");
        let (apps, warnings) = parse_apps(text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(apps, catostv_core::apps::defaults());
    }

    #[test]
    fn invalid_file_falls_back() {
        let dir = std::env::temp_dir().join(format!("catostv-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("apps.toml");
        std::fs::write(&p, "[[app]]\nname = 3").unwrap();
        let (apps, warnings) = load_apps(&p);
        assert_eq!(apps, catostv_core::apps::defaults());
        assert_eq!(warnings.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
