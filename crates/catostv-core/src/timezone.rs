//! Time zone choices offered by the installer.

/// Shown in this order. The first entry is the default selection.
pub const COMMON: &[&str] = &[
    "Asia/Kolkata",
    "UTC",
    "Europe/London",
    "Europe/Dublin",
    "Europe/Lisbon",
    "Europe/Paris",
    "Europe/Berlin",
    "Europe/Madrid",
    "Europe/Rome",
    "Europe/Amsterdam",
    "Europe/Stockholm",
    "Europe/Warsaw",
    "Europe/Athens",
    "Europe/Istanbul",
    "Europe/Moscow",
    "Africa/Lagos",
    "Africa/Cairo",
    "Africa/Nairobi",
    "Africa/Johannesburg",
    "Asia/Dubai",
    "Asia/Karachi",
    "Asia/Dhaka",
    "Asia/Bangkok",
    "Asia/Jakarta",
    "Asia/Singapore",
    "Asia/Shanghai",
    "Asia/Hong_Kong",
    "Asia/Manila",
    "Asia/Tokyo",
    "Asia/Seoul",
    "Australia/Perth",
    "Australia/Adelaide",
    "Australia/Sydney",
    "Pacific/Auckland",
    "America/Sao_Paulo",
    "America/Argentina/Buenos_Aires",
    "America/Bogota",
    "America/Mexico_City",
    "America/New_York",
    "America/Toronto",
    "America/Chicago",
    "America/Denver",
    "America/Phoenix",
    "America/Los_Angeles",
    "America/Anchorage",
    "Pacific/Honolulu",
];

/// Syntactic validation: rejects anything that could escape
/// `/usr/share/zoneinfo` when used to build the `/etc/localtime` symlink.
/// Existence is checked separately inside the installed system.
pub fn is_valid_name(tz: &str) -> bool {
    !tz.is_empty()
        && tz.len() <= 64
        && !tz.starts_with('/')
        && !tz.ends_with('/')
        && !tz
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && tz
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_common_zones_are_valid_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for tz in COMMON {
            assert!(is_valid_name(tz), "{tz}");
            assert!(seen.insert(tz), "duplicate {tz}");
        }
    }

    #[test]
    fn rejects_path_tricks() {
        for bad in [
            "",
            "../../etc/shadow",
            "/etc/passwd",
            "Asia//Kolkata",
            "Asia/",
            "a b",
            "Asia/./X",
            "Etc/GMT;rm",
        ] {
            assert!(!is_valid_name(bad), "{bad}");
        }
        assert!(is_valid_name("Etc/GMT+5"));
    }
}
