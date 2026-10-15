//! Keyboard layouts offered by the installer.
//!
//! The layout is an XKB layout name. It is applied:
//! * live, in the installer session, by rewriting labwc's `environment`
//!   file (`XKB_DEFAULT_LAYOUT=…`) and running `labwc --reconfigure`
//!   (labwc re-reads environment files on reconfigure, labwc-config(5));
//! * on the installed system, in `/home/catostv/.config/labwc/environment`
//!   (Wayland session) and `/etc/vconsole.conf` (text console, rarely seen).

/// `(xkb layout, label, console keymap)`. The first entry is the default.
/// Console keymap names are from kbd's `/usr/share/kbd/keymaps` (verify a
/// new one with `localectl list-keymaps` on Arch before adding it).
pub const LAYOUTS: &[(&str, &str, &str)] = &[
    ("us", "English (US)", "us"),
    ("gb", "English (UK)", "uk"),
    ("de", "German", "de-latin1"),
    ("fr", "French", "fr"),
    ("es", "Spanish", "es"),
    ("latam", "Spanish (Latin America)", "la-latin1"),
    ("it", "Italian", "it"),
    ("pt", "Portuguese", "pt-latin1"),
    ("br", "Portuguese (Brazil)", "br-abnt2"),
    ("nl", "Dutch", "nl"),
    ("be", "Belgian", "be-latin1"),
    ("ch", "Swiss", "fr_CH"),
    ("se", "Swedish", "sv-latin1"),
    ("no", "Norwegian", "no-latin1"),
    ("dk", "Danish", "dk-latin1"),
    ("fi", "Finnish", "fi"),
    ("pl", "Polish", "pl"),
    ("cz", "Czech", "cz"),
    ("hu", "Hungarian", "hu"),
    ("tr", "Turkish", "trq"),
    ("gr", "Greek", "gr"),
    ("ru", "Russian", "ru"),
    ("ua", "Ukrainian", "ua"),
    ("jp", "Japanese", "jp106"),
    ("kr", "Korean", "us"),
    ("in", "Hindi (Devanagari)", "us"),
    ("ara", "Arabic", "us"),
    ("il", "Hebrew", "us"),
];

pub const DEFAULT: &str = "us";

pub fn is_valid(layout: &str) -> bool {
    LAYOUTS.iter().any(|(l, _, _)| *l == layout)
}

pub fn label(layout: &str) -> &'static str {
    LAYOUTS
        .iter()
        .find(|(l, _, _)| *l == layout)
        .map(|(_, n, _)| *n)
        .unwrap_or("English (US)")
}

pub fn console_keymap(layout: &str) -> &'static str {
    LAYOUTS
        .iter()
        .find(|(l, _, _)| *l == layout)
        .map(|(_, _, k)| *k)
        .unwrap_or("us")
}

/// Replace (or add) `XKB_DEFAULT_LAYOUT=` in a labwc `environment` file,
/// keeping every other line.
pub fn set_layout_in_environment(text: &str, layout: &str) -> String {
    let mut out: Vec<String> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("XKB_DEFAULT_LAYOUT="))
        .map(str::to_string)
        .collect();
    out.insert(0, format!("XKB_DEFAULT_LAYOUT={layout}"));
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// `/etc/vconsole.conf` for the installed system.
pub fn vconsole_conf(layout: &str) -> String {
    format!("KEYMAP={}\nXKBLAYOUT={layout}\n", console_keymap(layout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_are_unique_and_safe() {
        let mut seen = std::collections::HashSet::new();
        for (l, name, km) in LAYOUTS {
            assert!(seen.insert(*l), "duplicate {l}");
            assert!(!name.is_empty());
            for s in [l, km] {
                assert!(
                    s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)),
                    "{s}"
                );
            }
        }
        assert_eq!(LAYOUTS[0].0, DEFAULT);
        assert!(is_valid("de"));
        assert!(!is_valid("de; rm -rf /"));
    }

    #[test]
    fn environment_rewrite_keeps_other_lines() {
        let env = "XKB_DEFAULT_LAYOUT=us\nMOZ_ENABLE_WAYLAND=1\n";
        assert_eq!(
            set_layout_in_environment(env, "de"),
            "XKB_DEFAULT_LAYOUT=de\nMOZ_ENABLE_WAYLAND=1\n"
        );
        assert_eq!(
            set_layout_in_environment("", "fr"),
            "XKB_DEFAULT_LAYOUT=fr\n"
        );
        assert_eq!(vconsole_conf("gb"), "KEYMAP=uk\nXKBLAYOUT=gb\n");
    }
}
