//! The "escape gesture": leave a full-screen app by repeating or holding Back.
//!
//! Full-screen apps (Firefox kiosk, YouTube TV) consume Escape/Backspace
//! themselves, and at their own top level "Back" does nothing, so the user is
//! trapped. The compositor can bind keys but cannot count presses or measure
//! holds, and binding Back globally would break every app. `catostv-inputd`
//! therefore watches the keyboards/remotes passively (see [`crate::evdev`])
//! and feeds their key events into this [`Detector`].
//!
//! Gestures (thresholds are the constants below):
//!
//! * **Repeated Back**, [`TRIPLE_PRESS_COUNT`] Back presses, each within
//!   [`TRIPLE_PRESS_GAP_SECS`] of the previous one. Only initial presses
//!   count; kernel autorepeat events do not.
//! * **Hold Back**, a Back key held for at least [`HOLD_SECS`]. Fires once
//!   per hold, while the key is still down. Autorepeat events count as
//!   "still held"; devices without autorepeat are covered by
//!   [`Detector::tick`].
//!
//! After a gesture fires, Back presses are ignored for [`COOLDOWN_SECS`] so
//! one gesture can never fire twice.
//!
//! Back-class keys are Escape, Backspace and the remote/browser Back key
//! (`KEY_BACK`). Any other key press resets the repeated-press count and
//! cancels a hold.
//!
//! **Typing guard.** Backspace is also the text-editing key. While the user
//! is typing (a text key was pressed less than [`TYPING_GUARD_SECS`] ago),
//! Backspace is treated as editing and starts no gesture; each such
//! Backspace event extends the guard, so a long run of corrections never
//! escapes. Escape and `KEY_BACK` are never guarded. Navigation keys (arrows,
//! Enter, Space, media keys) do not count as typing, so "browse with the
//! arrows, then press Backspace three times" still works.
//!
//! **Global, not per device.** One detector serves all devices. Remotes and
//! many keyboards expose several input devices (e.g. a "Consumer Control"
//! device for `KEY_BACK` next to the main keyboard device), so a per-device
//! detector would miss that letters typed on one device precede a Backspace
//! on another. Near-simultaneous duplicate presses (one physical press seen
//! on two devices, or switch bounce) are dropped by [`DEBOUNCE_SECS`].
//!
//! Times are `f64` seconds on any monotonic clock (`catostv-inputd` uses
//! [`std::time::Instant`]); they only need to be comparable with each other.
//! Slightly out-of-order times (events from different device threads) are
//! tolerated: negative gaps count as zero.
//!
//! Privacy: the detector never sees key codes other than the Back-class
//! keys, text and other keys arrive as [`KeyClass::Text`] / [`KeyClass::Other`]
//! and it keeps no history beyond the fields of [`Detector`].

use crate::ipc::ShellCommand;

/// Number of quick Back presses that make the repeated-press gesture.
pub const TRIPLE_PRESS_COUNT: u32 = 3;

/// Maximum time between one Back press and the next for them to belong to
/// the same repeated-press gesture, in seconds (inclusive). Human editing
/// ("a", Backspace, "b", ...) is slower and is also broken up by the
/// interleaved text keys.
pub const TRIPLE_PRESS_GAP_SECS: f64 = 0.6;

/// How long a Back key must be held to escape, in seconds. Longer than
/// CatosTV's own long-press (0.6 s) so the two are never confused.
pub const HOLD_SECS: f64 = 1.0;

/// After a gesture fires, Back presses are ignored for this long, in seconds,
/// so the remaining presses of a burst cannot fire a second gesture.
pub const COOLDOWN_SECS: f64 = 1.0;

/// Backspace pressed less than this long after a text key (or after another
/// guarded Backspace event) is text editing and starts no gesture, in seconds.
pub const TYPING_GUARD_SECS: f64 = 2.0;

/// Back presses closer together than this are one physical press seen twice
/// (two input devices reporting the same key, or switch bounce), in seconds.
/// No human presses a key twice in 40 ms.
pub const DEBOUNCE_SECS: f64 = 0.04;

/// The keys that mean "Back" on a TV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackKey {
    /// `KEY_ESC`.
    Escape,
    /// `KEY_BACKSPACE`, also a text-editing key, see the typing guard.
    Backspace,
    /// `KEY_BACK` (remote / browser Back button).
    Back,
}

impl BackKey {
    pub fn name(self) -> &'static str {
        match self {
            Self::Escape => "Escape",
            Self::Backspace => "Backspace",
            Self::Back => "Back",
        }
    }
}

/// What kind of key an event is about. Deliberately coarse: the detector
/// never learns which text key was typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyClass {
    Back(BackKey),
    /// A key that produces text or edits it with modifiers (letters, digits,
    /// punctuation, keypad digits, Shift/Ctrl/Alt/Super).
    Text,
    /// Anything else (arrows, Enter, Space, media keys, mouse buttons, ...).
    Other,
}

/// Linux `EV_KEY` values: 1 press, 2 autorepeat, 0 release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Press,
    Repeat,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyEvent {
    pub key: KeyClass,
    pub action: KeyAction,
    /// Monotonic time in seconds.
    pub time: f64,
}

impl KeyEvent {
    pub fn new(key: KeyClass, action: KeyAction, time: f64) -> Self {
        Self { key, action, time }
    }
}

/// A recognised escape gesture. Both kinds mean the same thing:
/// [`Gesture::command`] → [`ShellCommand::Escape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// [`TRIPLE_PRESS_COUNT`] quick presses; the key of the last press.
    RepeatedPress(BackKey),
    /// Held for [`HOLD_SECS`].
    Hold(BackKey),
}

impl Gesture {
    /// The shell command this gesture sends.
    pub fn command(self) -> ShellCommand {
        ShellCommand::Escape
    }

    /// Short description for logs (names only Back-class keys).
    pub fn describe(self) -> String {
        match self {
            Self::RepeatedPress(k) => format!("{}x {}", TRIPLE_PRESS_COUNT, k.name()),
            Self::Hold(k) => format!("hold {}", k.name()),
        }
    }
}

/// Gesture thresholds. [`Default`] gives the documented constants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    pub press_count: u32,
    pub press_gap: f64,
    pub hold: f64,
    pub cooldown: f64,
    pub typing_guard: f64,
    pub debounce: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            press_count: TRIPLE_PRESS_COUNT,
            press_gap: TRIPLE_PRESS_GAP_SECS,
            hold: HOLD_SECS,
            cooldown: COOLDOWN_SECS,
            typing_guard: TYPING_GUARD_SECS,
            debounce: DEBOUNCE_SECS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Held {
    key: BackKey,
    since: f64,
    fired: bool,
}

/// Escape-gesture state machine. Feed every key event to [`Detector::on_key`]
/// and call [`Detector::tick`] at (or soon after) [`Detector::next_deadline`].
#[derive(Debug, Clone)]
pub struct Detector {
    t: Thresholds,
    /// Back presses in the current quick sequence.
    count: u32,
    /// Time of the last counted Back press.
    last_press: Option<f64>,
    /// Time of the last Back press seen at all (for debounce).
    last_back_event: Option<f64>,
    held: Option<Held>,
    cooldown_until: Option<f64>,
    last_typing: Option<f64>,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

/// `later - earlier`, never negative (events from different threads may
/// arrive very slightly out of order).
fn gap(later: f64, earlier: f64) -> f64 {
    (later - earlier).max(0.0)
}

impl Detector {
    pub fn new() -> Self {
        Self::with_thresholds(Thresholds::default())
    }

    pub fn with_thresholds(t: Thresholds) -> Self {
        Self {
            t,
            count: 0,
            last_press: None,
            last_back_event: None,
            held: None,
            cooldown_until: None,
            last_typing: None,
        }
    }

    /// Process one key event. Returns a gesture if this event completes one.
    pub fn on_key(&mut self, ev: KeyEvent) -> Option<Gesture> {
        let t = ev.time;
        match (ev.key, ev.action) {
            (KeyClass::Text, KeyAction::Press | KeyAction::Repeat) => {
                self.last_typing = Some(t);
                self.reset_sequence();
                self.held = None;
                None
            }
            (KeyClass::Other, KeyAction::Press) => {
                self.reset_sequence();
                self.held = None;
                None
            }
            (KeyClass::Text | KeyClass::Other, _) => None,
            (KeyClass::Back(k), KeyAction::Press) => self.on_back_press(k, t),
            (KeyClass::Back(k), KeyAction::Repeat) => {
                if self.held.is_some_and(|h| h.key == k) {
                    self.tick(t)
                } else {
                    self.extend_guard(k, t);
                    None
                }
            }
            (KeyClass::Back(k), KeyAction::Release) => {
                if self.held.is_some_and(|h| h.key == k) {
                    self.held = None;
                } else {
                    self.extend_guard(k, t);
                }
                None
            }
        }
    }

    /// Advance time without an event. Fires the hold gesture for devices
    /// that send no autorepeat events.
    pub fn tick(&mut self, now: f64) -> Option<Gesture> {
        let h = self.held?;
        if h.fired || gap(now, h.since) < self.t.hold || self.in_cooldown(now) {
            return None;
        }
        self.held = Some(Held { fired: true, ..h });
        Some(self.fire(Gesture::Hold(h.key), now))
    }

    /// When [`Detector::tick`] must next be called for a hold to fire on
    /// time, if a hold is in progress.
    pub fn next_deadline(&self) -> Option<f64> {
        self.held
            .filter(|h| !h.fired)
            .map(|h| h.since + self.t.hold)
    }

    /// Forget the press sequence and any hold (key state became unknown:
    /// events were dropped or a device disappeared). The cooldown and the
    /// typing guard are kept.
    pub fn reset(&mut self) {
        self.reset_sequence();
        self.held = None;
        self.last_back_event = None;
    }

    fn on_back_press(&mut self, k: BackKey, t: f64) -> Option<Gesture> {
        if self
            .last_back_event
            .is_some_and(|prev| gap(t, prev) < self.t.debounce)
        {
            return None;
        }
        self.last_back_event = Some(t);
        if k == BackKey::Backspace && self.typing(t) {
            self.last_typing = Some(t);
            self.reset_sequence();
            self.held = None;
            return None;
        }
        if self.in_cooldown(t) {
            self.reset_sequence();
            self.held = None;
            return None;
        }
        self.held = Some(Held {
            key: k,
            since: t,
            fired: false,
        });
        let continues = self
            .last_press
            .is_some_and(|prev| gap(t, prev) <= self.t.press_gap);
        self.count = if continues { self.count + 1 } else { 1 };
        self.last_press = Some(t);
        if self.count >= self.t.press_count {
            // The press that completes the sequence must not also fire as
            // a hold if the user keeps it down.
            if let Some(h) = self.held.as_mut() {
                h.fired = true;
            }
            return Some(self.fire(Gesture::RepeatedPress(k), t));
        }
        None
    }

    /// A Backspace event while typing keeps the typing guard alive.
    fn extend_guard(&mut self, k: BackKey, t: f64) {
        if k == BackKey::Backspace && self.typing(t) {
            self.last_typing = Some(t);
        }
    }

    fn typing(&self, t: f64) -> bool {
        self.last_typing
            .is_some_and(|lt| gap(t, lt) < self.t.typing_guard)
    }

    fn in_cooldown(&self, t: f64) -> bool {
        self.cooldown_until.is_some_and(|until| t < until)
    }

    fn reset_sequence(&mut self) {
        self.count = 0;
        self.last_press = None;
    }

    fn fire(&mut self, g: Gesture, t: f64) -> Gesture {
        self.cooldown_until = Some(t + self.t.cooldown);
        self.reset_sequence();
        g
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use BackKey::{Back, Backspace, Escape};
    use KeyAction::{Press, Release, Repeat};

    /// Event script: (time, class, action).
    fn run(d: &mut Detector, script: &[(f64, KeyClass, KeyAction)]) -> Vec<(f64, Gesture)> {
        let mut out = Vec::new();
        for &(t, key, action) in script {
            if let Some(g) = d.on_key(KeyEvent::new(key, action, t)) {
                out.push((t, g));
            }
        }
        out
    }

    /// Tap (press + release 0.08 s later) each key at the given times.
    fn taps(keys: &[(f64, KeyClass)]) -> Vec<(f64, KeyClass, KeyAction)> {
        let mut s = Vec::new();
        for &(t, k) in keys {
            s.push((t, k, Press));
            s.push((t + 0.08, k, Release));
        }
        s
    }

    fn b(k: BackKey) -> KeyClass {
        KeyClass::Back(k)
    }

    const TEXT: KeyClass = KeyClass::Text;
    const OTHER: KeyClass = KeyClass::Other;

    #[test]
    fn three_quick_presses_fire_once_on_the_third() {
        let mut d = Detector::new();
        let got = run(
            &mut d,
            &taps(&[(10.0, b(Escape)), (10.3, b(Escape)), (10.6, b(Escape))]),
        );
        assert_eq!(got, vec![(10.6, Gesture::RepeatedPress(Escape))]);
        assert_eq!(got[0].1.command(), ShellCommand::Escape);
    }

    #[test]
    fn two_quick_presses_do_not_fire() {
        let mut d = Detector::new();
        assert!(run(&mut d, &taps(&[(1.0, b(Backspace)), (1.2, b(Backspace))])).is_empty());
        // ... nor does the third one if it comes too late.
        assert!(run(&mut d, &taps(&[(1.9, b(Backspace))])).is_empty());
    }

    #[test]
    fn a_long_gap_restarts_the_count() {
        let mut d = Detector::new();
        // 0.0, 0.5 (count 2), 1.2 (gap 0.7: restart at 1), 1.75 (2), 2.3 (3).
        let got = run(
            &mut d,
            &taps(&[
                (0.0, b(Back)),
                (0.5, b(Back)),
                (1.2, b(Back)),
                (1.75, b(Back)),
                (2.3, b(Back)),
            ]),
        );
        assert_eq!(got, vec![(2.3, Gesture::RepeatedPress(Back))]);
    }

    #[test]
    fn gap_is_inclusive() {
        // Binary-exact times and threshold, so the boundary is really tested.
        let t = Thresholds {
            press_gap: 0.5,
            ..Thresholds::default()
        };
        let mut d = Detector::with_thresholds(t);
        let got = run(
            &mut d,
            &taps(&[(0.0, b(Back)), (0.5, b(Back)), (1.0, b(Back))]),
        );
        assert_eq!(got, vec![(1.0, Gesture::RepeatedPress(Back))]);
        let mut d = Detector::with_thresholds(t);
        let got = run(
            &mut d,
            &taps(&[(0.0, b(Back)), (0.5, b(Back)), (1.0625, b(Back))]),
        );
        assert!(got.is_empty());
    }

    #[test]
    fn autorepeat_does_not_count_as_presses() {
        let mut d = Detector::new();
        let script = [
            (0.0, b(Escape), Press),
            (0.25, b(Escape), Repeat),
            (0.29, b(Escape), Repeat),
            (0.33, b(Escape), Repeat),
            (0.4, b(Escape), Release),
            (0.7, b(Escape), Press),
            (0.75, b(Escape), Release),
        ];
        assert!(run(&mut d, &script).is_empty());
    }

    #[test]
    fn different_back_keys_count_together() {
        let mut d = Detector::new();
        let got = run(
            &mut d,
            &taps(&[(0.0, b(Back)), (0.3, b(Escape)), (0.6, b(Back))]),
        );
        assert_eq!(got, vec![(0.6, Gesture::RepeatedPress(Back))]);
    }

    #[test]
    fn hold_fires_once_via_autorepeat() {
        let mut d = Detector::new();
        let mut script = vec![(5.0, b(Back), Press)];
        // Autorepeat every 40 ms after a 250 ms delay, held for 3 s.
        let mut t = 5.25;
        while t < 8.0 {
            script.push((t, b(Back), Repeat));
            t += 0.04;
        }
        script.push((8.0, b(Back), Release));
        let got = run(&mut d, &script);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].1, Gesture::Hold(Back));
        assert!(got[0].0 >= 6.0 && got[0].0 < 6.05, "{got:?}");
    }

    #[test]
    fn short_hold_does_not_fire() {
        let mut d = Detector::new();
        let script = [
            (0.0, b(Escape), Press),
            (0.25, b(Escape), Repeat),
            (0.9, b(Escape), Repeat),
            (0.95, b(Escape), Release),
        ];
        assert!(run(&mut d, &script).is_empty());
        assert_eq!(d.tick(5.0), None);
    }

    #[test]
    fn hold_fires_from_tick_without_autorepeat() {
        let mut d = Detector::new();
        assert_eq!(d.next_deadline(), None);
        assert!(run(&mut d, &[(2.0, b(Back), Press)]).is_empty());
        assert_eq!(d.next_deadline(), Some(3.0));
        assert_eq!(d.tick(2.99), None);
        assert_eq!(d.tick(3.0), Some(Gesture::Hold(Back)));
        assert_eq!(d.next_deadline(), None);
        assert_eq!(d.tick(4.5), None, "once per hold");
        assert!(run(&mut d, &[(6.0, b(Back), Release)]).is_empty());
    }

    #[test]
    fn cooldown_swallows_the_rest_of_a_burst() {
        let mut d = Detector::new();
        // Six fast presses: fires on the 3rd only; 4th-6th are in cooldown.
        let presses: Vec<_> = (0..6).map(|i| (i as f64 * 0.2, b(Escape))).collect();
        let got = run(&mut d, &taps(&presses));
        assert_eq!(got, vec![(0.4, Gesture::RepeatedPress(Escape))]);
        // After the cooldown a new gesture works again.
        let got = run(
            &mut d,
            &taps(&[(3.0, b(Escape)), (3.2, b(Escape)), (3.4, b(Escape))]),
        );
        assert_eq!(got, vec![(3.4, Gesture::RepeatedPress(Escape))]);
    }

    #[test]
    fn third_press_held_down_does_not_also_fire_a_hold() {
        let mut d = Detector::new();
        let mut script = taps(&[(0.0, b(Back)), (0.2, b(Back))]);
        script.push((0.4, b(Back), Press));
        let got = run(&mut d, &script);
        assert_eq!(got, vec![(0.4, Gesture::RepeatedPress(Back))]);
        assert_eq!(d.next_deadline(), None);
        assert_eq!(d.tick(2.0), None);
        assert!(run(&mut d, &[(2.5, b(Back), Repeat)]).is_empty());
    }

    #[test]
    fn press_during_cooldown_does_not_start_a_hold() {
        let mut d = Detector::new();
        run(
            &mut d,
            &taps(&[(0.0, b(Back)), (0.2, b(Back)), (0.4, b(Back))]),
        );
        assert!(run(&mut d, &[(0.9, b(Back), Press)]).is_empty());
        assert_eq!(d.tick(2.5), None);
    }

    #[test]
    fn other_keys_reset_the_sequence() {
        let mut d = Detector::new();
        let got = run(
            &mut d,
            &taps(&[
                (0.0, b(Escape)),
                (0.2, b(Escape)),
                (0.3, OTHER),
                (0.4, b(Escape)),
                (0.6, b(Escape)),
            ]),
        );
        assert!(got.is_empty());
    }

    #[test]
    fn other_key_cancels_a_hold() {
        let mut d = Detector::new();
        run(&mut d, &[(0.0, b(Escape), Press), (0.5, OTHER, Press)]);
        assert_eq!(d.next_deadline(), None);
        assert_eq!(d.tick(2.0), None);
    }

    #[test]
    fn navigation_then_backspaces_still_escapes() {
        // Arrows/Enter are Other: they reset the count but are not typing.
        let mut d = Detector::new();
        let got = run(
            &mut d,
            &taps(&[
                (0.0, OTHER),
                (0.3, OTHER),
                (0.6, b(Backspace)),
                (0.9, b(Backspace)),
                (1.2, b(Backspace)),
            ]),
        );
        assert_eq!(got, vec![(1.2, Gesture::RepeatedPress(Backspace))]);
    }

    #[test]
    fn human_speed_editing_does_not_fire() {
        // "a", Backspace, "b", Backspace, ... at typing speed.
        let mut keys = Vec::new();
        for i in 0..10 {
            let t = i as f64 * 0.7;
            keys.push((t, TEXT));
            keys.push((t + 0.35, b(Backspace)));
        }
        assert!(run(&mut Detector::new(), &taps(&keys)).is_empty());
        // The interleaved text keys alone prevent it, even without the guard.
        let mut unguarded = Detector::with_thresholds(Thresholds {
            typing_guard: 0.0,
            ..Thresholds::default()
        });
        assert!(run(&mut unguarded, &taps(&keys)).is_empty());
        // ... and so does the gap: Backspace every 0.7 s with nothing between.
        let slow: Vec<_> = (0..10).map(|i| (i as f64 * 0.7, b(Backspace))).collect();
        assert!(run(&mut Detector::new(), &taps(&slow)).is_empty());
    }

    #[test]
    fn fast_interleaved_editing_does_not_fire() {
        // "x⌫x⌫x⌫" typed fast.
        let mut d = Detector::new();
        let keys: Vec<_> = (0..12)
            .map(|i| {
                let k = if i % 2 == 0 { TEXT } else { b(Backspace) };
                (i as f64 * 0.12, k)
            })
            .collect();
        assert!(run(&mut d, &taps(&keys)).is_empty());
    }

    #[test]
    fn quick_corrections_after_typing_do_not_fire() {
        // "hello" then three fast Backspaces to fix a typo.
        let mut d = Detector::new();
        let mut keys: Vec<_> = (0..5).map(|i| (i as f64 * 0.15, TEXT)).collect();
        keys.extend((0..3).map(|i| (1.0 + i as f64 * 0.15, b(Backspace))));
        assert!(run(&mut d, &taps(&keys)).is_empty());
    }

    #[test]
    fn long_run_of_corrections_never_fires() {
        // Typing, then tapping Backspace 30 times at 0.25 s: each guarded
        // Backspace extends the guard, so the run never becomes a gesture.
        let mut d = Detector::new();
        let mut keys = vec![(0.0, TEXT)];
        keys.extend((0..30).map(|i| (0.5 + i as f64 * 0.25, b(Backspace))));
        assert!(run(&mut d, &taps(&keys)).is_empty());
    }

    #[test]
    fn holding_backspace_while_typing_does_not_fire() {
        let mut d = Detector::new();
        let mut script = taps(&[(0.0, TEXT)]);
        script.push((0.5, b(Backspace), Press));
        let mut t = 0.75;
        while t < 4.0 {
            script.push((t, b(Backspace), Repeat));
            t += 0.04;
        }
        assert!(run(&mut d, &script).is_empty());
        assert_eq!(d.next_deadline(), None);
        assert_eq!(d.tick(4.0), None);
        // Releasing after the long hold and tapping again right away is still
        // editing (the repeats kept the guard alive).
        script = taps(&[
            (4.1, b(Backspace)),
            (4.3, b(Backspace)),
            (4.5, b(Backspace)),
        ]);
        script.insert(0, (4.0, b(Backspace), Release));
        assert!(run(&mut d, &script).is_empty());
    }

    #[test]
    fn backspace_works_again_after_a_typing_pause() {
        let mut d = Detector::new();
        let got = run(
            &mut d,
            &taps(&[
                (0.0, TEXT),
                (2.5, b(Backspace)),
                (2.8, b(Backspace)),
                (3.1, b(Backspace)),
            ]),
        );
        assert_eq!(got, vec![(3.1, Gesture::RepeatedPress(Backspace))]);
    }

    #[test]
    fn escape_and_back_are_never_guarded() {
        for k in [Escape, Back] {
            let mut d = Detector::new();
            let got = run(
                &mut d,
                &taps(&[(0.0, TEXT), (0.1, b(k)), (0.3, b(k)), (0.5, b(k))]),
            );
            assert_eq!(got, vec![(0.5, Gesture::RepeatedPress(k))]);
        }
    }

    #[test]
    fn text_key_resets_the_sequence() {
        let mut d = Detector::new();
        let got = run(
            &mut d,
            &taps(&[
                (0.0, b(Escape)),
                (0.2, b(Escape)),
                (0.3, TEXT),
                (0.4, b(Escape)),
            ]),
        );
        assert!(got.is_empty());
    }

    #[test]
    fn duplicate_press_from_a_second_device_is_debounced() {
        let mut d = Detector::new();
        // Each physical press reported twice, 5 ms apart.
        let script = [
            (0.0, b(Escape), Press),
            (0.005, b(Escape), Press),
            (0.08, b(Escape), Release),
            (0.085, b(Escape), Release),
            (0.3, b(Escape), Press),
            (0.305, b(Escape), Press),
        ];
        assert!(run(&mut d, &script).is_empty());
        let got = run(
            &mut d,
            &[(0.6, b(Escape), Press), (0.605, b(Escape), Press)],
        );
        assert_eq!(got, vec![(0.6, Gesture::RepeatedPress(Escape))]);
    }

    #[test]
    fn reset_forgets_hold_and_sequence() {
        // A hold whose release is lost (events dropped / device unplugged).
        let mut d = Detector::new();
        run(&mut d, &taps(&[(0.0, b(Back))]));
        run(&mut d, &[(0.3, b(Back), Press)]);
        d.reset();
        assert_eq!(d.next_deadline(), None);
        assert_eq!(d.tick(5.0), None);
        // The count restarted: two more presses are not enough.
        assert!(run(&mut d, &taps(&[(5.0, b(Back)), (5.2, b(Back))])).is_empty());
    }

    #[test]
    fn repeat_or_release_without_a_tracked_press_is_ignored() {
        let mut d = Detector::new();
        let script = [
            (0.0, b(Back), Repeat),
            (0.5, b(Back), Repeat),
            (1.5, b(Back), Repeat),
            (1.6, b(Back), Release),
        ];
        assert!(run(&mut d, &script).is_empty());
        assert_eq!(d.tick(3.0), None);
    }

    #[test]
    fn out_of_order_times_are_tolerated() {
        let mut d = Detector::new();
        let got = run(
            &mut d,
            &taps(&[(1.0, b(Escape)), (1.3, b(Escape)), (1.29, OTHER)]),
        );
        assert!(got.is_empty());
        // A press stamped slightly earlier than the previous one counts as a
        // zero gap (and is then debounced), no panic, no negative maths.
        let mut d = Detector::new();
        let got = run(
            &mut d,
            &[
                (2.0, b(Back), Press),
                (2.3, b(Back), Press),
                (2.29, b(Back), Press),
                (2.6, b(Back), Press),
            ],
        );
        assert_eq!(got, vec![(2.6, Gesture::RepeatedPress(Back))]);
    }

    #[test]
    fn custom_thresholds() {
        let mut d = Detector::with_thresholds(Thresholds {
            press_count: 2,
            ..Thresholds::default()
        });
        let got = run(&mut d, &taps(&[(0.0, b(Back)), (0.3, b(Back))]));
        assert_eq!(got, vec![(0.3, Gesture::RepeatedPress(Back))]);
    }

    #[test]
    fn descriptions_name_only_back_keys() {
        assert_eq!(Gesture::RepeatedPress(Backspace).describe(), "3x Backspace");
        assert_eq!(Gesture::Hold(Back).describe(), "hold Back");
    }

    #[test]
    fn hold_is_longer_than_the_shell_long_press() {
        const { assert!(HOLD_SECS > crate::input::LONG_PRESS_SECS) };
        const { assert!(COOLDOWN_SECS >= TRIPLE_PRESS_GAP_SECS) };
    }
}
