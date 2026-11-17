//! Memory owned by DRM clients, counted once per client.
use std::{collections::BTreeMap, path::Path};

pub const SHELL_LIMIT: u64 = 1024 * 1024 * 1024;

pub fn bytes<'a>(descriptors: impl IntoIterator<Item = &'a str>) -> u64 {
    let mut clients = BTreeMap::new();
    for text in descriptors {
        let (mut device, mut client, mut total) = ("", None, 0u64);
        for line in text.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key {
                "drm-pdev" => device = value,
                "drm-client-id" => client = Some(value),
                key if key.starts_with("drm-total-") => {
                    let mut parts = value.split_whitespace();
                    let Some(amount) = parts.next().and_then(|part| part.parse::<u64>().ok())
                    else {
                        continue;
                    };
                    let scale = match parts.next() {
                        Some("KiB") => 1024,
                        Some("MiB") => 1024 * 1024,
                        Some("GiB") => 1024 * 1024 * 1024,
                        Some("B") => 1,
                        _ => continue,
                    };
                    total = total.saturating_add(amount.saturating_mul(scale));
                }
                _ => {}
            }
        }
        if let Some(client) = client {
            clients
                .entry((device, client))
                .and_modify(|old: &mut u64| *old = (*old).max(total))
                .or_insert(total);
        }
    }
    clients
        .values()
        .fold(0u64, |total, bytes| total.saturating_add(*bytes))
}

pub fn current() -> std::io::Result<u64> {
    read(Path::new("/proc/self/fdinfo"))
}
fn read(directory: &Path) -> std::io::Result<u64> {
    let files: Vec<String> = std::fs::read_dir(directory)?
        .filter_map(|entry| std::fs::read_to_string(entry.ok()?.path()).ok())
        .collect();
    Ok(bytes(files.iter().map(String::as_str)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_descriptors_do_not_multiply_a_clients_memory() {
        let first = "drm-pdev: 0000:00:02.0\ndrm-client-id: 419\ndrm-total-system0: 11973132 KiB\n";
        assert_eq!(bytes([first, first]), 11_973_132 * 1024);
        assert!(bytes([first]) > SHELL_LIMIT);
    }
    #[test]
    fn different_clients_and_regions_are_added_without_counting_resident_twice() {
        let first = "drm-pdev: a\ndrm-client-id: 1\ndrm-total-system0: 4 MiB\ndrm-total-vram0: 2 MiB\ndrm-resident-system0: 4 MiB";
        let second = "drm-pdev: b\ndrm-client-id: 1\ndrm-total-system0: 1 MiB";
        assert_eq!(bytes([first, second]), 7 * 1024 * 1024);
    }
    #[test]
    fn non_memory_counters_and_invalid_sizes_are_ignored() {
        let text = "drm-client-id: 2\ndrm-total-cycles-rcs0: 999999999999\ndrm-total-system0: invalid KiB\ndrm-engine-render: 888888888888 ns";
        assert_eq!(bytes([text, "drm-total-system0: 3 GiB"]), 0);
    }
}
