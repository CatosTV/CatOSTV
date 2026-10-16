//! Power actions through systemd-logind (`systemctl`).
//!
//! Only allowed on an installed appliance (`CATOSTV_APPLIANCE=1`), so a
//! developer running the shell on a workstation cannot power it off by
//! accident. Authorization on the appliance comes from the polkit rule in
//! `target-root/etc/polkit-1/rules.d/49-catostv.rules`.

use crate::error::{Error, Result};
use crate::process::{Cmd, Runner};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Suspend,
    Reboot,
    PowerOff,
}

impl PowerAction {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Suspend => "suspend",
            Self::Reboot => "reboot",
            Self::PowerOff => "poweroff",
        }
    }
}

pub fn perform(r: &mut dyn Runner, action: PowerAction, appliance: bool) -> Result<()> {
    if !appliance {
        return Err(Error::msg(format!(
            "{} is disabled outside an installed CatosTV appliance",
            action.verb()
        )));
    }
    r.run_ok(&Cmd::new("systemctl").arg(action.verb()))?;
    Ok(())
}

// ------------------------------------------------------------- waking up

/// Seconds since boot, *including* time asleep (`/proc/uptime`).
pub fn boot_seconds() -> Option<f64> {
    std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// After waking from Sleep, ignore keys this long: the key that woke the
/// TV reaches the interface too, and with Quick settings still on "Sleep"
/// it put the TV straight back to sleep, it took many presses to wake.
pub const INPUT_QUIET_SECS: f64 = 1.0;
/// … and refuse to go back to sleep this soon after waking.
pub const SLEEP_QUIET_SECS: f64 = 6.0;

/// Notices that the machine slept. The boot clock ([`boot_seconds`]) keeps
/// running during sleep, a monotonic clock does not: when the gap between
/// them grows, the machine was asleep in between.
#[derive(Debug, Default, Clone)]
pub struct WakeGuard {
    gap: Option<f64>,
    woke_at: Option<f64>,
}

impl WakeGuard {
    /// Call every frame with both clocks (seconds). True on the first call
    /// after a sleep.
    pub fn observe(&mut self, boot: f64, mono: f64) -> bool {
        let gap = boot - mono;
        let woke = self.gap.is_some_and(|g| gap - g > 1.0);
        // Only ever moves forward (/proc/uptime has 10 ms steps).
        self.gap = Some(self.gap.map_or(gap, |g| g.max(gap)));
        if woke {
            self.woke_at = Some(mono);
        }
        woke
    }

    pub fn input_blocked(&self, mono: f64) -> bool {
        self.woke_at.is_some_and(|w| mono - w < INPUT_QUIET_SECS)
    }

    pub fn sleep_blocked(&self, mono: f64) -> bool {
        self.woke_at.is_some_and(|w| mono - w < SLEEP_QUIET_SECS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::fake::FakeRunner;

    #[test]
    fn blocked_outside_appliance() {
        let mut r = FakeRunner::new();
        assert!(perform(&mut r, PowerAction::PowerOff, false).is_err());
        assert!(r.seen.is_empty());
        perform(&mut r, PowerAction::Suspend, true).unwrap();
        assert_eq!(r.seen[0].display(), "systemctl suspend");
    }

    #[test]
    fn wake_guard_notices_sleep_and_quiets_input() {
        let mut g = WakeGuard::default();
        // Awake: both clocks advance together (with /proc/uptime jitter).
        assert!(!g.observe(100.00, 10.0));
        assert!(!g.observe(100.51, 10.5));
        assert!(!g.observe(101.0, 11.0));
        assert!(!g.input_blocked(11.0) && !g.sleep_blocked(11.0));
        // Asleep for 60 s: only the boot clock moved.
        assert!(g.observe(161.1, 11.1));
        assert!(g.input_blocked(11.5));
        assert!(!g.input_blocked(12.2));
        assert!(g.sleep_blocked(15.0));
        assert!(!g.sleep_blocked(17.2));
        // Only once per sleep.
        assert!(!g.observe(161.2, 11.2));
    }
}
