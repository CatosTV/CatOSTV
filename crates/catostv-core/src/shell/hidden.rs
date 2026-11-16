use super::*;
use crate::hidden_apps::{Entry, Goal, Request, Stage};

impl Shell {
    pub fn request_hidden_apps(&mut self, goal: Goal, ctx: &ShellContext) {
        self.hidden_ticket = self.hidden_ticket.wrapping_add(1);
        self.overlays.push(Overlay::HiddenPin(Entry::new(
            self.hidden_ticket,
            goal,
            ctx.prefs.hidden_pin_hash.is_some(),
        )));
    }
    pub fn hidden_pin_pending(&self, ticket: u64) -> bool {
        matches!(self.overlay(), Some(Overlay::HiddenPin(entry)) if entry.ticket == ticket && entry.stage == Stage::Busy)
    }
    pub fn on_hidden_pin_error(&mut self, ticket: u64, message: String, saving: bool) {
        if !self.hidden_pin_pending(ticket) {
            return;
        }
        if let Some(Overlay::HiddenPin(entry)) = self.overlays.last_mut() {
            if saving {
                *entry = Entry::new(ticket, entry.goal.clone(), false);
                entry.error = Some(message);
            } else {
                entry.retry(message);
            }
        }
    }
    pub fn on_hidden_pin_granted(&mut self, ticket: u64) -> Vec<Effect> {
        if !self.hidden_pin_pending(ticket) {
            return vec![];
        }
        let Some(Overlay::HiddenPin(entry)) = self.overlays.pop() else {
            return vec![];
        };
        match entry.goal {
            Goal::Browse => {
                self.overlays.push(Overlay::HiddenApps { index: 0 });
                vec![]
            }
            Goal::Hide(id) => vec![Effect::HideApp(id)],
            Goal::Change => vec![Effect::Toast("PIN changed".into())],
        }
    }
    pub(super) fn handle_hidden(&mut self, action: Action, ctx: &ShellContext) -> Vec<Effect> {
        if action == Action::Back {
            self.overlays.pop();
            return vec![];
        }
        match self.overlays.last_mut() {
            Some(Overlay::HiddenPin(entry)) => {
                let ticket = entry.ticket;
                match entry.handle(action) {
                    Some(Request::Verify(pin)) => vec![Effect::CheckHiddenPin { ticket, pin }],
                    Some(Request::Save(pin)) => vec![Effect::SaveHiddenPin { ticket, pin }],
                    None => vec![],
                }
            }
            Some(Overlay::HiddenApps { index }) => {
                let count = ctx.sys.hidden_apps.len();
                match action {
                    Action::Up => *index = index.saturating_sub(1),
                    Action::Down => *index = (*index + 1).min(count + 1),
                    Action::Ok if *index < count => {
                        let id = ctx.sys.hidden_apps[*index].entry.id.clone();
                        self.overlays.clear();
                        return vec![Effect::LaunchHiddenApp(id)];
                    }
                    Action::OkLong | Action::Options if *index < count => {
                        let id = ctx.sys.hidden_apps[*index].entry.id.clone();
                        self.overlays
                            .push(Overlay::HiddenAppOptions { id, index: 0 });
                    }
                    Action::Ok if *index == count => self.request_hidden_apps(Goal::Change, ctx),
                    Action::Ok => {
                        self.overlays.pop();
                    }
                    _ => {}
                }
                vec![]
            }
            Some(Overlay::HiddenAppOptions { id, index }) => {
                match action {
                    Action::Up => *index = index.saturating_sub(1),
                    Action::Down => *index = (*index + 1).min(2),
                    Action::Ok | Action::OkLong => {
                        let id = id.clone();
                        let choice = *index;
                        self.overlays.pop();
                        if !ctx.sys.hidden_apps.iter().any(|app| app.entry.id == id) {
                            return vec![];
                        }
                        return match choice {
                            0 => {
                                self.overlays.clear();
                                vec![Effect::LaunchHiddenApp(id)]
                            }
                            1 => vec![Effect::UnhideApp(id)],
                            _ => vec![],
                        };
                    }
                    _ => {}
                }
                vec![]
            }
            _ => vec![],
        }
    }
}
