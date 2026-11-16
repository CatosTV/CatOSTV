//! PIN entry for the private app list. Verification runs outside the UI.
use crate::input::Action;
use crate::{apps::AppEntry, jellyfin::Item, playback::Source, shell::Effect};

pub fn hides_player(hidden: &[AppEntry], source: Source) -> bool {
    match source {
        Source::Catfyn => hidden
            .iter()
            .any(|app| app.command == crate::apps::FIN_COMMAND),
        Source::CatTube => hidden
            .iter()
            .any(|app| app.command == crate::cattube::BINARY),
        _ => false,
    }
}
pub fn hides_item(hidden: &[AppEntry], item: &Item) -> bool {
    hides_player(
        hidden,
        if crate::cattube::is_cattube(item) {
            Source::CatTube
        } else {
            Source::Catfyn
        },
    )
}
pub fn blocks_shortcut(hidden: &[AppEntry], effect: &Effect) -> bool {
    match effect {
        Effect::OpenFin => hides_player(hidden, Source::Catfyn),
        Effect::OpenFiles => hidden
            .iter()
            .any(|app| app.command == crate::apps::FILES_COMMAND),
        Effect::FinDetails(item) | Effect::FinPlayHome(item) => hides_item(hidden, item),
        Effect::ResumeNowPlaying(source) => hides_player(hidden, *source),
        _ => false,
    }
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct Pin(String);
impl std::fmt::Debug for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pin([redacted])")
    }
}
impl Pin {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn valid(&self) -> bool {
        self.0.len() == 4 && self.0.bytes().all(|c| c.is_ascii_digit())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Goal {
    Browse,
    Hide(String),
    Change,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Create,
    Confirm,
    Verify,
    Busy,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub ticket: u64,
    pub goal: Goal,
    pub stage: Stage,
    pub index: usize,
    pub error: Option<String>,
    digits: Pin,
    first: Pin,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Verify(Pin),
    Save(Pin),
}
impl Entry {
    pub fn new(ticket: u64, goal: Goal, configured: bool) -> Self {
        let stage = if !configured || goal == Goal::Change {
            Stage::Create
        } else {
            Stage::Verify
        };
        Self {
            ticket,
            goal,
            stage,
            index: 0,
            error: None,
            digits: Pin::default(),
            first: Pin::default(),
        }
    }
    pub fn title(&self) -> &'static str {
        match self.stage {
            Stage::Create => "Create a PIN",
            Stage::Confirm => "Confirm PIN",
            Stage::Verify => "Enter PIN",
            Stage::Busy => "Checking PIN",
        }
    }
    pub fn filled(&self) -> usize {
        self.digits.0.len()
    }
    pub fn handle(&mut self, action: Action) -> Option<Request> {
        if self.stage == Stage::Busy {
            return None;
        }
        match action {
            Action::Char(c) if c.is_ascii_digit() => {
                self.digits.0.push(c);
                self.index = 11;
            }
            Action::Erase => {
                self.digits.0.pop();
            }
            Action::Left => self.index = self.index / 3 * 3 + (self.index % 3).saturating_sub(1),
            Action::Right => self.index = self.index / 3 * 3 + (self.index % 3 + 1).min(2),
            Action::Up => self.index = self.index.saturating_sub(3),
            Action::Down => self.index = (self.index + 3).min(9 + self.index % 3),
            Action::Ok | Action::OkLong => match self.index {
                0..=8 => self.digits.0.push(char::from(b'1' + self.index as u8)),
                9 => {
                    self.digits.0.pop();
                }
                10 => self.digits.0.push('0'),
                _ => {}
            },
            _ => {}
        }
        if !self.digits.valid() {
            return None;
        }
        self.error = None;
        let pin = std::mem::take(&mut self.digits);
        match self.stage {
            Stage::Create => {
                self.first = pin;
                self.stage = Stage::Confirm;
                None
            }
            Stage::Confirm if pin != self.first => {
                self.first = Pin::default();
                self.stage = Stage::Create;
                self.error = Some("PINs did not match".into());
                None
            }
            Stage::Confirm => {
                self.first = Pin::default();
                self.stage = Stage::Busy;
                Some(Request::Save(pin))
            }
            Stage::Verify => {
                self.stage = Stage::Busy;
                Some(Request::Verify(pin))
            }
            Stage::Busy => None,
        }
    }
    pub fn retry(&mut self, message: String) {
        self.stage = Stage::Verify;
        self.error = Some(message);
        self.digits = Pin::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hiding_one_media_app_keeps_the_other_apps_shortcuts_available() {
        let catfyn = crate::apps::defaults()
            .into_iter()
            .find(AppEntry::is_builtin)
            .unwrap();
        let mut tube = catfyn.clone();
        tube.id = "cattube".into();
        tube.command = crate::cattube::BINARY.into();
        let youtube = crate::cattube::button();
        let movie = Item::default();
        let fyn_only = [catfyn];
        let tube_only = [tube];
        for effect in [
            Effect::FinDetails(youtube.clone()),
            Effect::FinPlayHome(youtube),
            Effect::ResumeNowPlaying(Source::CatTube),
        ] {
            assert!(!blocks_shortcut(&fyn_only, &effect));
            assert!(blocks_shortcut(&tube_only, &effect));
        }
        for effect in [
            Effect::OpenFin,
            Effect::FinDetails(movie.clone()),
            Effect::FinPlayHome(movie),
            Effect::ResumeNowPlaying(Source::Catfyn),
        ] {
            assert!(blocks_shortcut(&fyn_only, &effect));
            assert!(!blocks_shortcut(&tube_only, &effect));
        }
        let files = crate::apps::defaults()
            .into_iter()
            .find(|app| app.command == crate::apps::FILES_COMMAND)
            .unwrap();
        let files_only = [files];
        assert!(blocks_shortcut(&files_only, &Effect::OpenFiles));
        assert!(!blocks_shortcut(&files_only, &Effect::OpenFin));
        assert!(!blocks_shortcut(&fyn_only, &Effect::OpenFiles));
    }
    fn type_pin(entry: &mut Entry, pin: &str) -> Option<Request> {
        pin.chars()
            .filter_map(|c| entry.handle(Action::Char(c)))
            .last()
    }
    #[test]
    fn creation_requires_matching_confirmation_and_redacts_secrets() {
        let mut entry = Entry::new(1, Goal::Browse, false);
        assert!(type_pin(&mut entry, "1234").is_none());
        assert_eq!(entry.stage, Stage::Confirm);
        assert!(!format!("{entry:?}").contains("1234"));
        assert!(type_pin(&mut entry, "4321").is_none());
        assert_eq!(entry.stage, Stage::Create);
        type_pin(&mut entry, "1234");
        let request = type_pin(&mut entry, "1234");
        assert!(matches!(request, Some(Request::Save(_))));
        assert!(!format!("{request:?}").contains("1234"));
        assert_eq!(entry.stage, Stage::Busy);
        assert!(entry.handle(Action::Ok).is_none());
    }
    #[test]
    fn keypad_only_accepts_four_digits_and_erases() {
        let mut entry = Entry::new(2, Goal::Browse, true);
        entry.handle(Action::Char('x'));
        assert_eq!(entry.filled(), 0);
        entry.handle(Action::Ok);
        entry.handle(Action::Erase);
        assert_eq!(entry.filled(), 0);
        let request = type_pin(&mut entry, "0123");
        assert!(matches!(request, Some(Request::Verify(_))));
    }
}
