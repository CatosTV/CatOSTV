//! Reusable remote-controlled on-screen keyboard (OSK).
//!
//! Used by the installer Wi-Fi password page, shell Wi-Fi settings and Search.
//! The model is pure state; the GUI draws [`Osk::rows`] and highlights
//! [`Osk::focus`].
//!
//! Rules:
//! * D-pad moves between keys; Up/Down picks the key whose centre is closest
//!   horizontally, so rows with wide keys still feel natural.
//! * OK activates the focused key.
//! * Physical keyboard typing works at the same time. After a typed
//!   character, focus jumps to **Done** so a physical Enter submits instead
//!   of typing whatever key the remote cursor was resting on.
//! * Moving past an edge returns [`OskEvent::Edge`] so the parent screen can
//!   move focus elsewhere (e.g. from the Search keyboard to its results).

use crate::input::Action;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Lower,
    Upper,
    Symbols,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Char(char),
    Shift,
    Symbols,
    Space,
    Erase,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCap {
    pub kind: KeyKind,
    pub label: String,
    /// Width in grid units; every row sums to [`ROW_UNITS`].
    pub width: u8,
}

pub const ROW_UNITS: u8 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OskEvent {
    /// Focus moved or page changed; nothing for the parent to do.
    Moved,
    /// Text changed.
    Edited,
    /// Done was activated.
    Submitted,
    /// Focus tried to leave the keyboard in this direction.
    Edge(Edge),
    /// The action is not a keyboard action (Back, Home, ...).
    Unhandled,
}

#[derive(Debug, Clone)]
pub struct Osk {
    page: Page,
    row: usize,
    col: usize,
    text: String,
    /// Caret position in characters (0 ..= text length).
    cursor: usize,
    /// The text field has focus (Up from the top key row): Left/Right
    /// move the caret, Backspace deletes before it.
    in_field: bool,
    pub max_len: usize,
    pub masked: bool,
    /// Label for the Done key ("Connect", "Search", ...).
    pub done_label: String,
}

impl Osk {
    pub fn new(masked: bool, done_label: impl Into<String>) -> Self {
        Self {
            page: Page::Lower,
            row: 1,
            col: 0,
            text: String::new(),
            cursor: 0,
            in_field: false,
            max_len: 63, // WPA2 passphrase maximum
            masked,
            done_label: done_label.into(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.chars().take(self.max_len).collect();
        self.cursor = self.text.chars().count();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Caret position (characters from the start).
    pub fn cursor(&self) -> usize {
        self.cursor.min(self.text.chars().count())
    }

    /// The text field (not a key) has focus.
    pub fn in_field(&self) -> bool {
        self.in_field
    }

    /// Display text split at the caret: (before, after).
    pub fn display_split(&self) -> (String, String) {
        let shown = self.display_text();
        let n = self.cursor();
        (
            shown.chars().take(n).collect(),
            shown.chars().skip(n).collect(),
        )
    }

    fn byte_at(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn erase(&mut self) -> OskEvent {
        let n = self.cursor();
        if n == 0 {
            return OskEvent::Moved;
        }
        let (a, b) = (self.byte_at(n - 1), self.byte_at(n));
        self.text.replace_range(a..b, "");
        self.cursor = n - 1;
        OskEvent::Edited
    }

    pub fn page(&self) -> Page {
        self.page
    }

    /// Text as it should be displayed (bullets when masked).
    pub fn display_text(&self) -> String {
        if self.masked {
            "•".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        }
    }

    /// `(row, column)` of the focused key within [`Osk::rows`].
    pub fn focus(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// Move focus to a key (pointer hover/click). Out-of-range positions
    /// are clamped, so a stale hit-test can never panic.
    pub fn set_focus(&mut self, row: usize, col: usize) {
        let rows = self.rows();
        self.row = row.min(rows.len() - 1);
        self.col = col.min(rows[self.row].len() - 1);
    }

    pub fn focused_key(&self) -> KeyCap {
        self.rows()[self.row][self.col].clone()
    }

    pub fn rows(&self) -> Vec<Vec<KeyCap>> {
        let chars = |s: &str| -> Vec<KeyCap> {
            s.chars()
                .map(|c| KeyCap {
                    kind: KeyKind::Char(c),
                    label: c.to_string(),
                    width: 1,
                })
                .collect()
        };
        let (r1, r2, r3, r4) = match self.page {
            Page::Lower => ("1234567890", "qwertyuiop", "asdfghjkl@", "zxcvbnm-_."),
            Page::Upper => ("1234567890", "QWERTYUIOP", "ASDFGHJKL@", "ZXCVBNM-_."),
            Page::Symbols => ("1234567890", "!@#$%^&*()", "-_=+[]{};:", "'\",./?\\|~`"),
        };
        let shift_label = match self.page {
            Page::Lower => "ABC",
            Page::Upper | Page::Symbols => "abc",
        };
        let symbols_label = if self.page == Page::Symbols {
            "ABC"
        } else {
            "#+="
        };
        let special = vec![
            KeyCap {
                kind: KeyKind::Shift,
                label: shift_label.into(),
                width: 2,
            },
            KeyCap {
                kind: KeyKind::Symbols,
                label: symbols_label.into(),
                width: 1,
            },
            KeyCap {
                kind: KeyKind::Space,
                label: "space".into(),
                width: 4,
            },
            KeyCap {
                kind: KeyKind::Erase,
                label: "delete".into(),
                width: 1,
            },
            KeyCap {
                kind: KeyKind::Done,
                label: self.done_label.clone(),
                width: 2,
            },
        ];
        vec![chars(r1), chars(r2), chars(r3), chars(r4), special]
    }

    fn done_position(&self) -> (usize, usize) {
        (4, 4)
    }

    fn push_char(&mut self, c: char) -> OskEvent {
        if self.text.chars().count() < self.max_len {
            let n = self.cursor();
            let at = self.byte_at(n);
            self.text.insert(at, c);
            self.cursor = n + 1;
            OskEvent::Edited
        } else {
            OskEvent::Moved
        }
    }

    pub fn handle(&mut self, action: Action) -> OskEvent {
        let rows = self.rows();
        if self.in_field {
            let n = self.cursor();
            return match action {
                Action::Left => {
                    self.cursor = n.saturating_sub(1);
                    OskEvent::Moved
                }
                Action::Right => {
                    self.cursor = (n + 1).min(self.text.chars().count());
                    OskEvent::Moved
                }
                Action::Down | Action::Ok => {
                    self.in_field = false;
                    OskEvent::Moved
                }
                Action::Up => OskEvent::Edge(Edge::Up),
                Action::Char(c) => self.push_char(c),
                Action::Erase => self.erase(),
                _ => OskEvent::Unhandled,
            };
        }
        match action {
            Action::Left => {
                if self.col == 0 {
                    OskEvent::Edge(Edge::Left)
                } else {
                    self.col -= 1;
                    OskEvent::Moved
                }
            }
            Action::Right => {
                if self.col + 1 >= rows[self.row].len() {
                    OskEvent::Edge(Edge::Right)
                } else {
                    self.col += 1;
                    OskEvent::Moved
                }
            }
            Action::Up => {
                if self.row == 0 {
                    // Into the text, to move the caret.
                    self.in_field = true;
                    OskEvent::Moved
                } else {
                    self.col = nearest_col(&rows[self.row], self.col, &rows[self.row - 1]);
                    self.row -= 1;
                    OskEvent::Moved
                }
            }
            Action::Down => {
                if self.row + 1 >= rows.len() {
                    OskEvent::Edge(Edge::Down)
                } else {
                    self.col = nearest_col(&rows[self.row], self.col, &rows[self.row + 1]);
                    self.row += 1;
                    OskEvent::Moved
                }
            }
            Action::Char(c) => {
                let ev = self.push_char(c);
                (self.row, self.col) = self.done_position();
                ev
            }
            Action::Erase => self.erase(),
            Action::Ok => self.activate(rows[self.row][self.col].kind),
            _ => OskEvent::Unhandled,
        }
    }

    fn activate(&mut self, kind: KeyKind) -> OskEvent {
        match kind {
            KeyKind::Char(c) => {
                let ev = self.push_char(c);
                // One-shot capitals, like a phone keyboard.
                if self.page == Page::Upper {
                    self.page = Page::Lower;
                }
                ev
            }
            KeyKind::Space => self.push_char(' '),
            KeyKind::Erase => self.erase(),
            KeyKind::Shift => {
                self.page = match self.page {
                    Page::Lower => Page::Upper,
                    Page::Upper | Page::Symbols => Page::Lower,
                };
                OskEvent::Moved
            }
            KeyKind::Symbols => {
                self.page = match self.page {
                    Page::Symbols => Page::Upper,
                    _ => Page::Symbols,
                };
                OskEvent::Moved
            }
            KeyKind::Done => OskEvent::Submitted,
        }
    }
}

/// Index of the key in `to` whose horizontal centre is closest to the centre
/// of key `from_col` in `from`.
fn nearest_col(from: &[KeyCap], from_col: usize, to: &[KeyCap]) -> usize {
    let center = |row: &[KeyCap], i: usize| -> f32 {
        let start: u32 = row[..i].iter().map(|k| k.width as u32).sum();
        start as f32 + row[i].width as f32 / 2.0
    };
    let x = center(from, from_col);
    (0..to.len())
        .min_by(|&a, &b| {
            (center(to, a) - x)
                .abs()
                .partial_cmp(&(center(to, b) - x).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_fills_the_grid() {
        for page in [Page::Lower, Page::Upper, Page::Symbols] {
            let mut k = Osk::new(false, "Done");
            k.page = page;
            for row in k.rows() {
                let w: u32 = row.iter().map(|c| c.width as u32).sum();
                assert_eq!(w, ROW_UNITS as u32, "{page:?}");
            }
        }
    }

    #[test]
    fn type_with_remote() {
        let mut k = Osk::new(false, "Done");
        // starts on 'q'
        assert_eq!(k.focused_key().label, "q");
        assert_eq!(k.handle(Action::Ok), OskEvent::Edited);
        k.handle(Action::Right); // w
        k.handle(Action::Ok);
        assert_eq!(k.text(), "qw");
        // Shift is one-shot.
        k.handle(Action::Down);
        k.handle(Action::Down);
        k.handle(Action::Down); // special row
        let key = k.focused_key();
        assert_eq!(key.kind, KeyKind::Shift, "{key:?}");
        k.handle(Action::Ok);
        assert_eq!(k.page(), Page::Upper);
        k.handle(Action::Up); // row 3, upper
        k.handle(Action::Ok);
        assert_eq!(k.page(), Page::Lower);
        assert_eq!(k.text(), "qwZ");
    }

    #[test]
    fn vertical_moves_pick_nearest_key() {
        let mut k = Osk::new(false, "Done");
        // go to 'p' (row 1, last column)
        for _ in 0..9 {
            k.handle(Action::Right);
        }
        assert_eq!(k.focused_key().label, "p");
        k.handle(Action::Down);
        k.handle(Action::Down);
        k.handle(Action::Down);
        assert_eq!(k.focused_key().kind, KeyKind::Done);
        // Done spans units 8..10 (centre 9.0); '_' (8.5) and '.' (9.5) tie,
        // and ties resolve to the leftmost key.
        k.handle(Action::Up);
        assert_eq!(k.focused_key().label, "_");
    }

    #[test]
    fn edges_are_reported() {
        let mut k = Osk::new(false, "Done");
        assert_eq!(k.handle(Action::Left), OskEvent::Edge(Edge::Left));
        k.handle(Action::Up); // row 0
        k.handle(Action::Up); // into the text field
        assert!(k.in_field());
        assert_eq!(k.handle(Action::Up), OskEvent::Edge(Edge::Up));
        k.handle(Action::Down);
        assert!(!k.in_field());
        for _ in 0..9 {
            k.handle(Action::Right);
        }
        assert_eq!(k.handle(Action::Right), OskEvent::Edge(Edge::Right));
    }

    #[test]
    fn physical_typing_moves_focus_to_done() {
        let mut k = Osk::new(true, "Connect");
        k.handle(Action::Char('h'));
        k.handle(Action::Char('i'));
        assert_eq!(k.text(), "hi");
        assert_eq!(k.display_text(), "••");
        assert_eq!(k.focused_key().kind, KeyKind::Done);
        assert_eq!(k.focused_key().label, "Connect");
        assert_eq!(k.handle(Action::Ok), OskEvent::Submitted);
        assert_eq!(k.handle(Action::Erase), OskEvent::Edited);
        assert_eq!(k.text(), "h");
    }

    #[test]
    fn max_len_is_enforced() {
        let mut k = Osk::new(false, "Done");
        k.max_len = 3;
        for c in "abcdef".chars() {
            k.handle(Action::Char(c));
        }
        assert_eq!(k.text(), "abc");
    }

    #[test]
    fn back_is_not_a_keyboard_action() {
        let mut k = Osk::new(false, "Done");
        assert_eq!(k.handle(Action::Back), OskEvent::Unhandled);
        assert_eq!(k.handle(Action::Home), OskEvent::Unhandled);
    }

    #[test]
    fn caret_moves_inside_the_text() {
        let mut k = Osk::new(false, "Done");
        for c in "helo".chars() {
            k.handle(Action::Char(c));
        }
        k.handle(Action::Up); // Done row → row 3
        for _ in 0..4 {
            k.handle(Action::Up);
        }
        assert!(k.in_field());
        k.handle(Action::Left);
        assert_eq!(k.cursor(), 3);
        k.handle(Action::Char('l'));
        assert_eq!(k.text(), "hello");
        assert_eq!(k.display_split(), ("hell".into(), "o".into()));
        k.handle(Action::Erase);
        k.handle(Action::Erase);
        assert_eq!(k.text(), "heo");
        assert_eq!(k.cursor(), 2);
        for _ in 0..5 {
            k.handle(Action::Right);
        }
        assert_eq!(k.cursor(), 3);
        k.set_text("ab");
        assert_eq!(k.cursor(), 2);
    }
}
