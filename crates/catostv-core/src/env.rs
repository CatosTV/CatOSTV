//! Environment flags and well-known paths.
//!
//! These flags are **safety rails, not security boundaries** (anyone who can
//! start a process can set them). See docs/SECURITY_MODEL.md.

/// Set by `catostv-session` on an installed appliance. Enables power actions
/// and compositor workspace switching.
pub const APPLIANCE: &str = "CATOSTV_APPLIANCE";
/// Development: run shell/installer in a normal decorated window.
pub const WINDOWED: &str = "CATOSTV_WINDOWED";
/// Set only by the live ISO session. Without it the installer refuses to
/// touch disks.
pub const ALLOW_DESTRUCTIVE: &str = "CATOSTV_INSTALLER_ALLOW_DESTRUCTIVE";
/// Development: the installer simulates every step and changes nothing.
/// Takes precedence over [`ALLOW_DESTRUCTIVE`].
pub const DRY_RUN: &str = "CATOSTV_INSTALLER_DRY_RUN";

pub fn flag(name: &str) -> bool {
    std::env::var(name).ok().as_deref() == Some("1")
}

pub fn is_appliance() -> bool {
    flag(APPLIANCE)
}

pub fn is_windowed() -> bool {
    flag(WINDOWED)
}

pub fn is_dry_run() -> bool {
    flag(DRY_RUN)
}

pub fn destructive_allowed() -> bool {
    flag(ALLOW_DESTRUCTIVE) && !is_dry_run()
}

/// `~/.config/catostv/apps.toml`
pub fn apps_config_path() -> std::path::PathBuf {
    home().join(".config/catostv/apps.toml")
}

/// `~/.local/state/catostv`, logs written by the shell and apps.
pub fn state_dir() -> std::path::PathBuf {
    match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        Some(d) => std::path::PathBuf::from(d).join("catostv"),
        None => home().join(".local/state/catostv"),
    }
}

/// `$XDG_RUNTIME_DIR` (fallback `/tmp`), flags that end with the session.
pub fn runtime_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
}

fn home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/home/catostv"))
}

/// Project version from the repository `VERSION` file (e.g. `0.2.0-dev`).
/// Cargo versions cannot carry the `-dev` convention used by the project.
/// The git commit the binaries were built from (`CATOSTV_COMMIT` at build
/// time, set by scripts/deploy.sh and the ISO build), if known.
pub fn commit() -> Option<&'static str> {
    option_env!("CATOSTV_COMMIT").filter(|c| !c.is_empty())
}

pub fn version() -> &'static str {
    include_str!("../../../VERSION").trim()
}
