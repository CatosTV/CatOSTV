//! Graphics: which drivers a new install gets.
//!
//! The live USB's boot menu passes `catostv.gpu=auto|nvidia|open|safe`
//! (iso-overlay/grub/grub.cfg). The installer reads it from the kernel
//! command line and shows it on its summary; the install plan picks the
//! driver packages from it.

/// What the boot menu asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GpuChoice {
    /// Detect the graphics card(s) and choose.
    #[default]
    Auto,
    /// NVIDIA's own driver (`nvidia-open`).
    Nvidia,
    /// Open-source drivers only (Mesa; nouveau on NVIDIA).
    Open,
    /// No hardware acceleration: basic display, software rendering.
    Safe,
}

impl GpuChoice {
    /// From a kernel command line (`/proc/cmdline`); `Auto` when absent or
    /// unknown.
    pub fn from_cmdline(cmdline: &str) -> Self {
        cmdline
            .split_whitespace()
            .find_map(|w| w.strip_prefix("catostv.gpu="))
            .map(|v| match v {
                "nvidia" => Self::Nvidia,
                "open" => Self::Open,
                "safe" => Self::Safe,
                _ => Self::Auto,
            })
            .unwrap_or_default()
    }

    /// The running system's choice.
    pub fn current() -> Self {
        Self::from_cmdline(&std::fs::read_to_string("/proc/cmdline").unwrap_or_default())
    }

    /// For the installer's summary: "Graphics: …".
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Graphics: detected automatically",
            Self::Nvidia => "Graphics: NVIDIA driver",
            Self::Open => "Graphics: open drivers",
            Self::Safe => "Graphics: safe mode (no acceleration)",
        }
    }
}

/// A graphics card from `lspci -mm -nn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpu {
    /// PCI address, "00:02.0".
    pub slot: String,
    pub vendor: Vendor,
    pub device_id: u16,
    /// "CometLake-S GT2 [UHD Graphics 630]".
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Intel,
    Amd,
    Nvidia,
    Other,
}

/// The `[xxxx]` id at the end of an `lspci -mm -nn` field.
fn bracket_id(field: &str) -> Option<u16> {
    let start = field.rfind('[')?;
    u16::from_str_radix(field[start + 1..].trim_end_matches(']'), 16).ok()
}

/// Display controllers (classes 0300 VGA, 0302 3D, 0380 display) from
/// `lspci -mm -nn`.
pub fn parse_lspci(text: &str) -> Vec<Gpu> {
    text.lines()
        .filter_map(|l| {
            let (slot, rest) = l.split_once(' ')?;
            let f: Vec<&str> = rest.split('"').skip(1).step_by(2).collect();
            let class = bracket_id(f.first()?)?;
            if !matches!(class, 0x0300 | 0x0302 | 0x0380) {
                return None;
            }
            let vendor = match bracket_id(f.get(1)?)? {
                0x8086 => Vendor::Intel,
                0x1002 | 0x1022 => Vendor::Amd,
                0x10de => Vendor::Nvidia,
                _ => Vendor::Other,
            };
            let dev = f.get(2)?;
            let name = dev.rfind(" [").map_or(*dev, |i| &dev[..i]).to_string();
            Some(Gpu {
                slot: slot.to_string(),
                vendor,
                device_id: bracket_id(dev)?,
                name,
            })
        })
        .collect()
}

/// The graphics cards of this machine (`lspci`, package pciutils).
pub fn detect(r: &mut dyn crate::process::Runner) -> Vec<Gpu> {
    r.run_ok(&crate::process::Cmd::new("lspci").args(["-mm", "-nn"]))
        .map(|o| parse_lspci(&o.stdout))
        .unwrap_or_default()
}

/// NVIDIA's open kernel modules (`nvidia-open`) need Turing (GTX 16 /
/// RTX 20) or newer; Turing's device ids start at 0x1e00.
pub fn nvidia_open_supported(device_id: u16) -> bool {
    device_id >= 0x1e00
}

/// What a new install gets for graphics.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DriverPlan {
    /// Packages on top of the base Mesa / Intel / AMD ones.
    pub packages: Vec<&'static str>,
    pub kernel_params: Vec<&'static str>,
    /// Android draws in software (its fast path needs Mesa on Intel/AMD).
    pub android_software: bool,
    /// The TV interface draws in software too (safe graphics).
    pub safe: bool,
    /// For the installer's summary: "Graphics: …".
    pub summary: String,
}

/// Drivers for `choice` on these cards.
pub fn plan(choice: GpuChoice, gpus: &[Gpu]) -> DriverPlan {
    let nvidia: Vec<&Gpu> = gpus.iter().filter(|g| g.vendor == Vendor::Nvidia).collect();
    let modern_nvidia = nvidia.iter().any(|g| nvidia_open_supported(g.device_id));
    let mesa_summary = || {
        let names: Vec<&str> = gpus
            .iter()
            .filter_map(|g| match g.vendor {
                Vendor::Intel => Some("Intel"),
                Vendor::Amd => Some("AMD"),
                _ => None,
            })
            .collect();
        if names.is_empty() {
            "Graphics: open drivers".to_string()
        } else {
            format!("Graphics: {} (open drivers)", names.join(" + "))
        }
    };
    let nouveau = |why: &str| DriverPlan {
        packages: vec!["vulkan-nouveau"],
        android_software: true,
        summary: format!("Graphics: NVIDIA, open drivers{why}"),
        ..Default::default()
    };
    match choice {
        GpuChoice::Safe => DriverPlan {
            kernel_params: vec!["nomodeset"],
            android_software: true,
            safe: true,
            summary: "Graphics: safe mode (no acceleration)".into(),
            ..Default::default()
        },
        GpuChoice::Nvidia | GpuChoice::Auto if modern_nvidia => DriverPlan {
            packages: vec!["nvidia-open", "nvidia-utils", "libva-nvidia-driver"],
            kernel_params: vec!["nvidia_drm.modeset=1", "nvidia_drm.fbdev=1"],
            android_software: true,
            summary: "Graphics: NVIDIA driver".into(),
            ..Default::default()
        },
        GpuChoice::Nvidia if !nvidia.is_empty() => {
            nouveau(" (this card is too old for NVIDIA's current driver)")
        }
        GpuChoice::Nvidia => DriverPlan {
            summary: format!("{}. No NVIDIA card found", mesa_summary()),
            ..Default::default()
        },
        _ if !nvidia.is_empty() && gpus.iter().all(|g| g.vendor == Vendor::Nvidia) => nouveau(""),
        _ => DriverPlan {
            summary: mesa_summary(),
            ..Default::default()
        },
    }
}

/// `/etc/catostv/gpu.conf` for the TV session (`catostv-session`).
pub fn gpu_conf(plan: &DriverPlan) -> String {
    format!(
        "# Written by the CatosTV installer.\nmode={}\n",
        if plan.safe { "safe" } else { "accelerated" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real `lspci -mm -nn` lines (the TV, a server, made-up NVIDIA ones).
    const INTEL: &str = r#"00:02.0 "VGA compatible controller [0300]" "Intel Corporation [8086]" "CometLake-S GT2 [UHD Graphics 630] [9bc5]" -r05 -p00 "Dell [1028]" "Device [0b8a]""#;
    const ASPEED: &str = r#"09:00.0 "VGA compatible controller [0300]" "ASPEED Technology, Inc. [1a03]" "ASPEED Graphics Family [2000]" -r21 -p00 "ASPEED Technology, Inc. [1a03]" "ASPEED Graphics Family [2000]""#;
    const RTX: &str = r#"01:00.0 "VGA compatible controller [0300]" "NVIDIA Corporation [10de]" "AD106 [GeForce RTX 4060 Ti] [2803]" -ra1 -p00 "Device [1462]" "Device [5170]""#;
    const GTX970: &str = r#"01:00.0 "VGA compatible controller [0300]" "NVIDIA Corporation [10de]" "GM204 [GeForce GTX 970] [13c2]" -ra1 -p00 "Device [1462]" "Device [3160]""#;
    const AUDIO: &str = r#"00:1f.3 "Audio device [0403]" "Intel Corporation [8086]" "Comet Lake PCH cAVS [06c8]" -r00 -p00 "Dell [1028]" "Device [0b8a]""#;

    #[test]
    fn lspci_parsing() {
        let g = parse_lspci(&format!("{INTEL}\n{AUDIO}\n{RTX}\n"));
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].vendor, Vendor::Intel);
        assert_eq!(g[0].device_id, 0x9bc5);
        assert_eq!(g[0].name, "CometLake-S GT2 [UHD Graphics 630]");
        assert_eq!(g[1].vendor, Vendor::Nvidia);
        assert_eq!(parse_lspci(ASPEED)[0].vendor, Vendor::Other);
    }

    #[test]
    fn driver_plans() {
        let intel = parse_lspci(INTEL);
        let p = plan(GpuChoice::Auto, &intel);
        assert!(p.packages.is_empty() && !p.android_software);
        assert_eq!(p.summary, "Graphics: Intel (open drivers)");
        // A current NVIDIA card next to the Intel one: NVIDIA's driver.
        let both = parse_lspci(&format!("{INTEL}\n{RTX}"));
        let p = plan(GpuChoice::Auto, &both);
        assert_eq!(
            p.packages,
            ["nvidia-open", "nvidia-utils", "libva-nvidia-driver"]
        );
        assert!(p.kernel_params.contains(&"nvidia_drm.modeset=1"));
        assert!(p.android_software);
        // "Open drivers" keeps Mesa, also with that card.
        assert!(plan(GpuChoice::Open, &both).packages.is_empty());
        // Too old for nvidia-open: nouveau, Android in software.
        let old = parse_lspci(GTX970);
        let p = plan(GpuChoice::Nvidia, &old);
        assert_eq!(p.packages, ["vulkan-nouveau"]);
        assert!(p.summary.contains("too old"));
        assert_eq!(plan(GpuChoice::Auto, &old).packages, ["vulkan-nouveau"]);
        // Safe graphics.
        let p = plan(GpuChoice::Safe, &both);
        assert!(p.safe && p.kernel_params == ["nomodeset"] && p.packages.is_empty());
        assert!(gpu_conf(&p).contains("mode=safe"));
    }

    #[test]
    fn choice_from_the_kernel_command_line() {
        let base = "archisobasedir=arch quiet";
        assert_eq!(GpuChoice::from_cmdline(base), GpuChoice::Auto);
        assert_eq!(
            GpuChoice::from_cmdline(&format!("{base} catostv.gpu=nvidia")),
            GpuChoice::Nvidia
        );
        assert_eq!(
            GpuChoice::from_cmdline("catostv.gpu=open x"),
            GpuChoice::Open
        );
        assert_eq!(
            GpuChoice::from_cmdline("catostv.gpu=safe nomodeset"),
            GpuChoice::Safe
        );
        assert_eq!(
            GpuChoice::from_cmdline("catostv.gpu=weird"),
            GpuChoice::Auto
        );
    }
}
