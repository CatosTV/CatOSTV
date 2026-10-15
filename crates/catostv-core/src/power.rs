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
}
