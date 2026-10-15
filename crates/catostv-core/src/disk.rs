//! Install-target discovery and safety rules.
//!
//! This module decides which disks the installer may offer. It is the
//! main protection against erasing the wrong device, so every rule has a
//! unit test with a realistic `lsblk` fixture.
//!
//! A whole disk (`TYPE=disk`) is **blocked** (shown greyed out with the
//! reason, never selectable) when any of these hold, checked in this order
//! (the first match is the reason shown):
//!
//! 1. it is the installer medium: the disk or any partition carries an
//!    `iso9660` filesystem, or it backs `/run/archiso/bootmnt` or
//!    `/run/archiso/cowspace`;
//! 2. it backs `/` (the running system);
//! 3. it is read-only;
//! 4. it is smaller than [`MIN_TARGET_BYTES`];
//! 5. the disk or anything on it is mounted or used as swap.
//!
//! Loop, ROM, zram, partitions and device-mapper nodes are never listed.
//! USB/removable disks are allowed but carry a warning, and existing
//! filesystems are listed so the confirmation page can say what will be lost.
//!
//! The installer re-runs this evaluation immediately before partitioning
//! (`install::Check::DiskEligible`, the first step of every install plan), so
//! a stale UI list cannot bypass it.

use crate::error::{Error, Result};
use crate::process::{Cmd, Runner};

/// Smallest disk CatosTV will install to: 20 GiB.
pub const MIN_TARGET_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// Mount points whose backing device must never be offered.
pub const PROTECTED_MOUNTS: &[&str] = &["/run/archiso/bootmnt", "/run/archiso/cowspace", "/"];

pub const LSBLK_COLUMNS: &str =
    "NAME,KNAME,PATH,PKNAME,TYPE,SIZE,RO,RM,TRAN,MODEL,SERIAL,FSTYPE,LABEL,MOUNTPOINTS";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockDevice {
    pub name: String,
    pub path: String,
    /// Parent kernel name (`sda` for `sda1`), empty for whole disks.
    pub pkname: String,
    /// lsblk TYPE: disk, part, rom, loop, crypt, lvm, ...
    pub kind: String,
    pub size: u64,
    pub ro: bool,
    pub removable: bool,
    /// Transport: nvme, sata, usb, mmc, ... (empty for virtio)
    pub tran: String,
    pub model: String,
    pub serial: String,
    pub fstype: String,
    pub label: String,
    pub mountpoints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskCandidate {
    pub disk: BlockDevice,
    /// `Some(reason)` when the disk must not be offered.
    pub blocked: Option<String>,
    pub warnings: Vec<String>,
}

impl DiskCandidate {
    pub fn eligible(&self) -> bool {
        self.blocked.is_none()
    }

    /// "Samsung SSD 970 EVO · 500 GB · NVMe"
    pub fn title(&self) -> String {
        let model = if self.disk.model.trim().is_empty() {
            "Disk".to_string()
        } else {
            self.disk.model.trim().to_string()
        };
        format!(
            "{model} · {} · {}",
            human_size(self.disk.size),
            transport_label(&self.disk)
        )
    }

    /// "/dev/nvme0n1 · serial S4EWNX0N123456"
    pub fn detail(&self) -> String {
        if self.disk.serial.trim().is_empty() {
            self.disk.path.clone()
        } else {
            format!("{} · serial {}", self.disk.path, self.disk.serial.trim())
        }
    }
}

/// Decimal units, matching what is printed on drive labels.
pub fn human_size(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1000.0 {
        format!("{:.1} TB", gb / 1000.0)
    } else if gb >= 10.0 {
        format!("{gb:.0} GB")
    } else {
        format!("{gb:.1} GB")
    }
}

pub fn transport_label(d: &BlockDevice) -> &'static str {
    match d.tran.as_str() {
        "nvme" => "NVMe",
        "sata" | "ata" => "SATA",
        "usb" => "USB",
        "mmc" => "eMMC / SD",
        "scsi" | "sas" => "SCSI",
        _ if d.name.starts_with("vd") => "Virtual",
        _ => "Disk",
    }
}

/// Partition device path for partition `n` of `disk`:
/// `/dev/sda` → `/dev/sda1`, `/dev/nvme0n1` → `/dev/nvme0n1p1`,
/// `/dev/mmcblk0` → `/dev/mmcblk0p1`.
pub fn partition_path(disk: &str, n: u32) -> String {
    if disk.ends_with(|c: char| c.is_ascii_digit()) {
        format!("{disk}p{n}")
    } else {
        format!("{disk}{n}")
    }
}

/// Parse `lsblk -P -b -o <LSBLK_COLUMNS>` output.
pub fn parse_lsblk_pairs(text: &str) -> Vec<BlockDevice> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let mut d = BlockDevice::default();
            for (k, v) in parse_pairs(line) {
                match k.as_str() {
                    "NAME" => d.name = v,
                    "PATH" => d.path = v,
                    "PKNAME" => d.pkname = v,
                    "TYPE" => d.kind = v,
                    "SIZE" => d.size = v.trim().parse().unwrap_or(0),
                    "RO" => d.ro = v.trim() == "1",
                    "RM" => d.removable = v.trim() == "1",
                    "TRAN" => d.tran = v,
                    "MODEL" => d.model = v,
                    "SERIAL" => d.serial = v,
                    "FSTYPE" => d.fstype = v,
                    "LABEL" => d.label = v,
                    "MOUNTPOINTS" | "MOUNTPOINT" => {
                        d.mountpoints = v
                            .split('\n')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .collect()
                    }
                    _ => {}
                }
            }
            if d.path.is_empty() && !d.name.is_empty() {
                d.path = format!("/dev/{}", d.name);
            }
            d
        })
        .collect()
}

/// Parse `KEY="value" KEY2="value"` with lsblk's `\xHH` escapes.
fn parse_pairs(line: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = line.trim();
    while let Some(eq) = rest.find("=\"") {
        let key = rest[..eq].trim().to_string();
        let mut bytes = Vec::new();
        let mut iter = rest[eq + 2..].char_indices();
        let mut end = rest.len() - (eq + 2);
        while let Some((i, c)) = iter.next() {
            match c {
                '"' => {
                    end = i + 1;
                    break;
                }
                '\\' => {
                    let tail = &rest[eq + 2 + i + 1..];
                    if let Some(hex) = tail.strip_prefix('x').and_then(|t| t.get(..2))
                        && let Ok(b) = u8::from_str_radix(hex, 16)
                    {
                        bytes.push(b);
                        iter.nth(2);
                        continue;
                    }
                    if let Some((_, n)) = iter.next() {
                        let mut buf = [0u8; 4];
                        bytes.extend_from_slice(n.encode_utf8(&mut buf).as_bytes());
                    }
                }
                c => {
                    let mut buf = [0u8; 4];
                    bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        out.push((key, String::from_utf8_lossy(&bytes).into_owned()));
        rest = rest[(eq + 2 + end).min(rest.len())..].trim_start();
    }
    out
}

/// All devices that sit on top of `disk` (partitions, and anything on them).
fn descendants<'a>(all: &'a [BlockDevice], disk: &BlockDevice) -> Vec<&'a BlockDevice> {
    let mut found: Vec<&BlockDevice> = Vec::new();
    let mut frontier = vec![disk.name.clone()];
    while let Some(parent) = frontier.pop() {
        for d in all.iter().filter(|d| d.pkname == parent) {
            if !found.iter().any(|f| f.path == d.path) {
                found.push(d);
                frontier.push(d.name.clone());
            }
        }
    }
    found
}

/// A device backing one of the [`PROTECTED_MOUNTS`] (from `findmnt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedSource {
    /// Device path, e.g. `/dev/sda1`.
    pub path: String,
    /// The protected mount point it backs, e.g. `/run/archiso/bootmnt`.
    pub mount: String,
}

impl ProtectedSource {
    pub fn new(path: &str, mount: &str) -> Self {
        Self {
            path: path.into(),
            mount: mount.into(),
        }
    }
}

/// Decide which disks may be offered.
pub fn evaluate(all: &[BlockDevice], protected: &[ProtectedSource]) -> Vec<DiskCandidate> {
    let mut out: Vec<DiskCandidate> = all
        .iter()
        .filter(|d| d.kind == "disk" && !d.name.starts_with("zram"))
        .map(|disk| {
            let desc = descendants(all, disk);
            let mut family: Vec<&BlockDevice> = vec![disk];
            family.extend(desc.iter().copied());

            let backs = |pred: &dyn Fn(&ProtectedSource) -> bool| {
                family
                    .iter()
                    .any(|d| protected.iter().any(|p| p.path == d.path && pred(p)))
            };
            let installer =
                family.iter().any(|d| d.fstype == "iso9660") || backs(&|p| p.mount != "/");
            let running_system = backs(&|p| p.mount == "/");
            let mounted: Vec<String> = family
                .iter()
                .flat_map(|d| d.mountpoints.iter().cloned())
                .collect();

            let blocked = if installer {
                Some("This is the CatosTV installer drive".to_string())
            } else if running_system {
                Some("This disk holds the running system".to_string())
            } else if disk.ro {
                Some("Read-only disk".to_string())
            } else if disk.size < MIN_TARGET_BYTES {
                Some(format!(
                    "Too small ({}; CatosTV needs at least {})",
                    human_size(disk.size),
                    human_size(MIN_TARGET_BYTES)
                ))
            } else if !mounted.is_empty() {
                Some(format!("In use (mounted at {})", mounted.join(", ")))
            } else {
                None
            };

            let mut warnings = Vec::new();
            if disk.tran == "usb" || disk.removable {
                warnings.push("Removable / USB drive".to_string());
            }
            let mut filesystems: Vec<String> = desc
                .iter()
                .filter(|d| !d.fstype.is_empty())
                .map(|d| {
                    if d.label.is_empty() {
                        d.fstype.clone()
                    } else {
                        format!("{} \"{}\"", d.fstype, d.label)
                    }
                })
                .collect();
            filesystems.dedup();
            if !filesystems.is_empty() {
                warnings.push(format!(
                    "Contains data that will be erased: {}",
                    filesystems.join(", ")
                ));
            }
            DiskCandidate {
                disk: disk.clone(),
                blocked,
                warnings,
            }
        })
        .collect();
    out.sort_by_key(|c| {
        (
            !c.eligible(),
            c.disk.tran == "usb" || c.disk.removable,
            c.disk.path.clone(),
        )
    });
    out
}

/// Run `lsblk` + `findmnt` and evaluate. Safe: read-only queries.
pub fn scan(r: &mut dyn Runner) -> Result<Vec<DiskCandidate>> {
    let out = r.run_ok(&Cmd::new("lsblk").args(["-P", "-b", "-o", LSBLK_COLUMNS]))?;
    let devices = parse_lsblk_pairs(&out.stdout);
    if devices.is_empty() {
        return Err(Error::msg("lsblk returned no devices"));
    }
    Ok(evaluate(&devices, &protected_sources(r)))
}

/// Devices backing the protected mounts (missing mounts are skipped).
pub fn protected_sources(r: &mut dyn Runner) -> Vec<ProtectedSource> {
    PROTECTED_MOUNTS
        .iter()
        .filter_map(|m| {
            let out = r
                .run(&Cmd::new("findmnt").args(["-n", "-o", "SOURCE", "--target", m]))
                .ok()
                .filter(|o| o.success())?;
            // findmnt may print "/dev/sda1[/subvol]" for bind/subvolume mounts.
            let source = out
                .stdout
                .trim()
                .split('[')
                .next()
                .unwrap_or("")
                .to_string();
            source
                .starts_with("/dev/")
                .then(|| ProtectedSource::new(&source, m))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Output, fake::FakeRunner};

    const GIB: u64 = 1024 * 1024 * 1024;

    /// A realistic live-USB session: installer stick, an internal NVMe with
    /// Windows, a virtio disk, a tiny disk, an eMMC with a mounted partition,
    /// zram swap and the archiso squashfs loop device.
    fn fixture() -> String {
        let rows: Vec<String> = vec![
            line(
                "sda",
                "",
                "disk",
                16 * GIB,
                "0",
                "1",
                "usb",
                "SanDisk Ultra",
                "4C53",
                "iso9660",
                "CATOSTV_202610",
                "",
            ),
            line(
                "sda1",
                "sda",
                "part",
                900_000_000,
                "0",
                "1",
                "",
                "",
                "",
                "iso9660",
                "CATOSTV_202610",
                "/run/archiso/bootmnt",
            ),
            line(
                "sda2",
                "sda",
                "part",
                100_000_000,
                "0",
                "1",
                "",
                "",
                "",
                "vfat",
                "ARCHISO_EFI",
                "",
            ),
            line(
                "nvme0n1",
                "",
                "disk",
                512 * GIB,
                "0",
                "0",
                "nvme",
                "Samsung SSD 970 EVO",
                "S4EW",
                "",
                "",
                "",
            ),
            line(
                "nvme0n1p1",
                "nvme0n1",
                "part",
                GIB / 10,
                "0",
                "0",
                "nvme",
                "",
                "",
                "vfat",
                "",
                "",
            ),
            line(
                "nvme0n1p3",
                "nvme0n1",
                "part",
                400 * GIB,
                "0",
                "0",
                "nvme",
                "",
                "",
                "ntfs",
                "Windows",
                "",
            ),
            line(
                "vda",
                "",
                "disk",
                32 * GIB,
                "0",
                "0",
                "",
                "",
                "",
                "",
                "",
                "",
            ),
            line(
                "sdb",
                "",
                "disk",
                8 * GIB,
                "0",
                "0",
                "sata",
                "Tiny SSD",
                "",
                "",
                "",
                "",
            ),
            line(
                "mmcblk0",
                "",
                "disk",
                64 * GIB,
                "0",
                "0",
                "mmc",
                "BJTD4R",
                "",
                "",
                "",
                "",
            ),
            line(
                "mmcblk0p2",
                "mmcblk0",
                "part",
                60 * GIB,
                "0",
                "0",
                "",
                "",
                "",
                "ext4",
                "data",
                "/mnt/data\\x0a/srv",
            ),
            line(
                "zram0",
                "",
                "disk",
                4 * GIB,
                "0",
                "0",
                "",
                "",
                "",
                "swap",
                "zram0",
                "[SWAP]",
            ),
            line(
                "loop0",
                "",
                "loop",
                800_000_000,
                "1",
                "0",
                "",
                "",
                "",
                "squashfs",
                "",
                "/run/archiso/airootfs",
            ),
            line(
                "sr0",
                "",
                "rom",
                1_000_000_000,
                "1",
                "1",
                "sata",
                "QEMU DVD-ROM",
                "",
                "iso9660",
                "CATOSTV_202610",
                "",
            ),
        ];
        rows.join("\n")
    }

    #[allow(clippy::too_many_arguments)]
    fn line(
        name: &str,
        pk: &str,
        kind: &str,
        size: u64,
        ro: &str,
        rm: &str,
        tran: &str,
        model: &str,
        serial: &str,
        fs: &str,
        label: &str,
        mounts: &str,
    ) -> String {
        format!(
            "NAME=\"{name}\" KNAME=\"{name}\" PATH=\"/dev/{name}\" PKNAME=\"{pk}\" TYPE=\"{kind}\" SIZE=\"{size}\" RO=\"{ro}\" RM=\"{rm}\" TRAN=\"{tran}\" MODEL=\"{model}\" SERIAL=\"{serial}\" FSTYPE=\"{fs}\" LABEL=\"{label}\" MOUNTPOINTS=\"{mounts}\""
        )
    }

    fn by_name<'a>(c: &'a [DiskCandidate], name: &str) -> &'a DiskCandidate {
        c.iter().find(|c| c.disk.name == name).unwrap()
    }

    #[test]
    fn parses_pairs_with_escapes() {
        let d = parse_lsblk_pairs(&fixture());
        assert_eq!(d.len(), 13);
        assert_eq!(d[3].model, "Samsung SSD 970 EVO");
        let mmc = d.iter().find(|d| d.name == "mmcblk0p2").unwrap();
        assert_eq!(mmc.mountpoints, vec!["/mnt/data", "/srv"]);
        let p = parse_pairs(r#"MODEL="WDC\x20WD10" LABEL="a\"b""#);
        assert_eq!(p[0].1, "WDC WD10");
        assert_eq!(p[1].1, "a\"b");
    }

    #[test]
    fn installer_media_is_blocked_even_without_findmnt() {
        let c = evaluate(&parse_lsblk_pairs(&fixture()), &[]);
        assert_eq!(
            by_name(&c, "sda").blocked.as_deref(),
            Some("This is the CatosTV installer drive")
        );
    }

    #[test]
    fn protected_mount_source_blocks_its_disk() {
        // copytoram case: no iso9660 visible, but findmnt reports the source.
        let text = line(
            "sdc",
            "",
            "disk",
            64 * GIB,
            "0",
            "1",
            "usb",
            "Stick",
            "",
            "",
            "",
            "",
        ) + "\n"
            + &line(
                "sdc1",
                "sdc",
                "part",
                60 * GIB,
                "0",
                "1",
                "",
                "",
                "",
                "exfat",
                "",
                "",
            );
        let c = evaluate(
            &parse_lsblk_pairs(&text),
            &[ProtectedSource::new("/dev/sdc1", "/run/archiso/bootmnt")],
        );
        assert_eq!(
            c[0].blocked.as_deref(),
            Some("This is the CatosTV installer drive")
        );
        // The disk behind `/` (e.g. a developer workstation) is named as such.
        let c = evaluate(
            &parse_lsblk_pairs(&text),
            &[ProtectedSource::new("/dev/sdc1", "/")],
        );
        assert_eq!(
            c[0].blocked.as_deref(),
            Some("This disk holds the running system")
        );
    }

    #[test]
    fn rules_for_every_disk() {
        let c = evaluate(&parse_lsblk_pairs(&fixture()), &[]);
        let names: Vec<_> = c.iter().map(|c| c.disk.name.as_str()).collect();
        // Not listed: partitions, loop, rom, zram.
        assert_eq!(names, vec!["nvme0n1", "vda", "mmcblk0", "sdb", "sda"]);

        let nvme = by_name(&c, "nvme0n1");
        assert!(nvme.eligible());
        assert!(
            nvme.warnings[0].contains("ntfs \"Windows\""),
            "{:?}",
            nvme.warnings
        );
        assert_eq!(nvme.title(), "Samsung SSD 970 EVO · 550 GB · NVMe");

        let vda = by_name(&c, "vda");
        assert!(vda.eligible());
        assert_eq!(vda.title(), "Disk · 34 GB · Virtual");

        assert!(
            by_name(&c, "sdb")
                .blocked
                .as_ref()
                .unwrap()
                .starts_with("Too small")
        );
        assert_eq!(
            by_name(&c, "mmcblk0").blocked.as_deref(),
            Some("In use (mounted at /mnt/data, /srv)")
        );
    }

    #[test]
    fn read_only_and_usb_rules() {
        let text = line(
            "sdd",
            "",
            "disk",
            64 * GIB,
            "1",
            "0",
            "sata",
            "RO",
            "",
            "",
            "",
            "",
        ) + "\n"
            + &line(
                "sde",
                "",
                "disk",
                64 * GIB,
                "0",
                "1",
                "usb",
                "Ext",
                "",
                "",
                "",
                "",
            );
        let c = evaluate(&parse_lsblk_pairs(&text), &[]);
        assert_eq!(
            by_name(&c, "sdd").blocked.as_deref(),
            Some("Read-only disk")
        );
        let sde = by_name(&c, "sde");
        assert!(sde.eligible());
        assert_eq!(sde.warnings, vec!["Removable / USB drive"]);
    }

    #[test]
    fn partition_paths() {
        assert_eq!(partition_path("/dev/sda", 1), "/dev/sda1");
        assert_eq!(partition_path("/dev/vda", 2), "/dev/vda2");
        assert_eq!(partition_path("/dev/nvme0n1", 1), "/dev/nvme0n1p1");
        assert_eq!(partition_path("/dev/mmcblk0", 2), "/dev/mmcblk0p2");
    }

    #[test]
    fn scan_uses_findmnt_results() {
        let mut r = FakeRunner::new();
        r.push(Output::ok(
            line(
                "sdc",
                "",
                "disk",
                64 * GIB,
                "0",
                "1",
                "usb",
                "Stick",
                "",
                "",
                "",
                "",
            ) + "\n"
                + &line(
                    "sdc1",
                    "sdc",
                    "part",
                    60 * GIB,
                    "0",
                    "1",
                    "",
                    "",
                    "",
                    "exfat",
                    "",
                    "",
                ),
        ));
        r.push(Output::ok("/dev/sdc1\n")); // bootmnt
        r.push(Output::failed(1, "")); // cowspace
        r.push(Output::ok("airootfs\n")); // / is an overlay, ignored
        let c = scan(&mut r).unwrap();
        assert!(!c[0].eligible());
        assert_eq!(r.seen[1].args.last().unwrap(), "/run/archiso/bootmnt");
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512 * GIB), "550 GB");
        assert_eq!(human_size(8_000_000_000), "8.0 GB");
        assert_eq!(human_size(2_000_000_000_000), "2.0 TB");
    }
}
