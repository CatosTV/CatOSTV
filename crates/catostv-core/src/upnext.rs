//! Catfyn's "Next episode" card: which episode comes next, when
//! the card shows, and what the countdown, Cancel and "Are you still
//! watching?" do. Pure logic; the player's reporter thread feeds it the
//! position and the card's button presses and carries out what it returns.
//! The control bar (`catfyn-osd.lua`, inside mpv) only draws.
//!
//! * The card shows when the end credits start: Jellyfin's `Outro` media
//!   segment when the server has one (intro-skipper plugin and similar),
//!   else the last [`FALLBACK_SECS`] seconds. Never for a movie or for the
//!   last episode of a series (no next episode = no card).
//! * The next episode is the following one in series order, across seasons
//!   not Jellyfin's "Next up", which can jump back to an older unwatched
//!   episode.
//! * Play now (focused) / OK plays at once; Cancel / Back hides the card and
//!   lets the credits run, after which the player ends as before. When the
//!   countdown runs out the next episode plays in the same player.
//! * After [`STILL_WATCHING_AFTER`] episodes in a row started by the
//!   countdown with no key pressed, the card asks "Are you still watching?"
//!   (Continue / Stop) instead of counting down.

use crate::jellyfin::{Item, Kind};
use crate::mpv::Json;

/// Without a server-side credits marker, the card shows this long before
/// the end.
pub const FALLBACK_SECS: f64 = 30.0;

/// Auto-played episodes in a row (no key pressed) before asking.
pub const STILL_WATCHING_AFTER: u32 = 3;

/// The episode after `current_id` in `episodes` (a series' episodes in
/// order, as `/Shows/{id}/Episodes` lists them). `None` for the last one,
/// for anything that is not an episode, and when `current_id` is not in
/// the list. Specials (season 0) are skipped unless the current one is a
/// special too.
pub fn next_in_order(episodes: &[Item], current_id: &str) -> Option<Item> {
    let at = episodes.iter().position(|e| e.id == current_id)?;
    let current = &episodes[at];
    if current.kind() != Kind::Episode {
        return None;
    }
    let special = current.season == Some(0);
    episodes[at + 1..]
        .iter()
        .find(|e| e.kind() == Kind::Episode && (special || e.season != Some(0)))
        .cloned()
}

/// The episode before `current_id` in series order (the player's
/// "Previous episode" button): like [`next_in_order`], backwards. Specials
/// (season 0) are skipped unless the current one is a special too.
pub fn prev_in_order(episodes: &[Item], current_id: &str) -> Option<Item> {
    let at = episodes.iter().position(|e| e.id == current_id)?;
    let current = &episodes[at];
    if current.kind() != Kind::Episode {
        return None;
    }
    let special = current.season == Some(0);
    episodes[..at]
        .iter()
        .rev()
        .find(|e| e.kind() == Kind::Episode && (special || e.season != Some(0)))
        .cloned()
}

/// Seconds at which the end credits start: the first `Outro` segment of a
/// `/MediaSegments` reply, else [`FALLBACK_SECS`] before `runtime`. `None`
/// when neither is known (runtime unknown or too short to matter).
pub fn credits_start(segments: Option<&str>, runtime: Option<f64>) -> Option<f64> {
    let outro = segments
        .and_then(|b| crate::mpv::parse_json(b.trim()))
        .and_then(|j| {
            j.get("Items")?
                .as_arr()?
                .iter()
                .filter(|s| s.get("Type").and_then(Json::as_str) == Some("Outro"))
                .filter_map(|s| s.get("StartTicks").and_then(Json::as_f64))
                .map(|t| t / 10_000_000.0)
                .filter(|&t| t > 0.0)
                .reduce(f64::min)
        });
    if let (Some(o), Some(rt)) = (outro, runtime)
        && o < rt
    {
        return Some(o);
    }
    if outro.is_some() && runtime.is_none() {
        return outro;
    }
    runtime
        .filter(|&rt| rt > FALLBACK_SECS * 2.0)
        .map(|rt| rt - FALLBACK_SECS)
}

/// "S2 · E5 · Title" for the card.
pub fn card_label(next: &Item) -> String {
    match next.episode_label() {
        Some(l) => format!("{l} · {}", next.name),
        None => next.name.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Watching,
    /// Counting down to `deadline` (clock seconds).
    Card {
        deadline: f64,
        left: u32,
    },
    Asking,
    Dismissed,
    Done,
}

/// What happened in the player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// Playback position and a monotonic clock, both in seconds.
    Tick { pos: f64, now: f64 },
    /// Play now / OK on the card.
    PlayNow,
    /// Cancel / Back on the card or the question.
    Cancel,
    /// "Continue" on "Are you still watching?".
    Continue,
    /// "Stop" on "Are you still watching?".
    Stop,
    /// Any key pressed in the player.
    Input,
    /// The file reached its end.
    Ended,
}

/// What to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ShowCard {
        left: u32,
    },
    Count {
        left: u32,
    },
    AskStillWatching,
    Hide,
    /// Play the next episode; `auto` when the countdown did it (no key).
    PlayNext {
        auto: bool,
    },
    /// Leave the player (back to the show page, as at any end).
    Quit,
}

/// One episode's card. `credits_at` is `None` when there is no card
/// (movie, last episode, autoplay off): the end just quits.
#[derive(Debug, Clone)]
pub struct UpNext {
    credits_at: Option<f64>,
    countdown: u32,
    /// Episodes in a row started by the countdown with no key pressed.
    pub streak: u32,
    phase: Phase,
}

impl UpNext {
    pub fn new(credits_at: Option<f64>, countdown_secs: u32, streak: u32) -> Self {
        Self {
            credits_at,
            countdown: countdown_secs.max(1),
            streak,
            phase: Phase::Watching,
        }
    }

    pub fn card_shown(&self) -> bool {
        matches!(self.phase, Phase::Card { .. } | Phase::Asking)
    }

    pub fn handle(&mut self, ev: Event) -> Option<Action> {
        if self.phase == Phase::Done {
            return None;
        }
        match ev {
            Event::Input => {
                self.streak = 0;
                None
            }
            Event::Tick { pos, now } => self.tick(pos, now),
            Event::PlayNow if matches!(self.phase, Phase::Card { .. }) => {
                self.phase = Phase::Done;
                self.streak = 0;
                Some(Action::PlayNext { auto: false })
            }
            Event::Continue if self.phase == Phase::Asking => {
                self.phase = Phase::Done;
                self.streak = 0;
                Some(Action::PlayNext { auto: false })
            }
            Event::Stop if self.phase == Phase::Asking => {
                self.phase = Phase::Done;
                Some(Action::Quit)
            }
            Event::Cancel if self.card_shown() => {
                self.phase = Phase::Dismissed;
                Some(Action::Hide)
            }
            Event::Ended => {
                let counting = matches!(self.phase, Phase::Card { .. });
                self.phase = Phase::Done;
                Some(if counting {
                    self.streak += 1;
                    Action::PlayNext { auto: true }
                } else {
                    Action::Quit
                })
            }
            _ => None,
        }
    }

    fn tick(&mut self, pos: f64, now: f64) -> Option<Action> {
        let at = self.credits_at?;
        match self.phase {
            Phase::Watching if pos >= at => {
                if self.streak >= STILL_WATCHING_AFTER {
                    self.phase = Phase::Asking;
                    return Some(Action::AskStillWatching);
                }
                let left = self.countdown;
                self.phase = Phase::Card {
                    deadline: now + left as f64,
                    left,
                };
                Some(Action::ShowCard { left })
            }
            // Rewound out of the credits: the card goes, and comes back
            // when they start again.
            Phase::Card { .. } | Phase::Asking | Phase::Dismissed if pos < at - 5.0 => {
                let shown = self.card_shown();
                self.phase = Phase::Watching;
                shown.then_some(Action::Hide)
            }
            Phase::Card { deadline, left } => {
                let now_left = (deadline - now).ceil().max(0.0) as u32;
                if now_left == 0 {
                    self.phase = Phase::Done;
                    self.streak += 1;
                    Some(Action::PlayNext { auto: true })
                } else if now_left != left {
                    self.phase = Phase::Card {
                        deadline,
                        left: now_left,
                    };
                    Some(Action::Count { left: now_left })
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(id: &str, season: u32, n: u32) -> Item {
        Item {
            id: id.into(),
            name: format!("Episode {n}"),
            kind_raw: "Episode".into(),
            season: Some(season),
            episode: Some(n),
            ..Item::default()
        }
    }

    #[test]
    fn next_episode_in_series_order() {
        let list = vec![
            ep("s1e1", 1, 1),
            ep("s1e2", 1, 2),
            ep("sp1", 0, 1),
            ep("s2e1", 2, 1),
            ep("s2e2", 2, 2),
        ];
        assert_eq!(next_in_order(&list, "s1e1").unwrap().id, "s1e2");
        // Crosses into the next season, past a special listed between.
        assert_eq!(next_in_order(&list, "s1e2").unwrap().id, "s2e1");
        // The last episode has none; an unknown id has none.
        assert_eq!(next_in_order(&list, "s2e2"), None);
        assert_eq!(next_in_order(&list, "nope"), None);
        // A special goes on in list order.
        assert_eq!(next_in_order(&list, "sp1").unwrap().id, "s2e1");
        // Movies never get a next episode.
        let mut movie = ep("m", 1, 1);
        movie.kind_raw = "Movie".into();
        assert_eq!(next_in_order(&[movie, ep("x", 1, 2)], "m"), None);
        assert_eq!(card_label(&list[3]), "S2 · E1 · Episode 1");
    }

    #[test]
    fn previous_episode_in_series_order() {
        let list = vec![
            ep("s1e1", 1, 1),
            ep("s1e2", 1, 2),
            ep("sp1", 0, 1),
            ep("s2e1", 2, 1),
        ];
        assert_eq!(prev_in_order(&list, "s1e2").unwrap().id, "s1e1");
        // Back across seasons, past a special listed between.
        assert_eq!(prev_in_order(&list, "s2e1").unwrap().id, "s1e2");
        // The first episode has none; an unknown id has none.
        assert_eq!(prev_in_order(&list, "s1e1"), None);
        assert_eq!(prev_in_order(&list, "nope"), None);
        // A special goes back in list order.
        assert_eq!(prev_in_order(&list, "sp1").unwrap().id, "s1e2");
    }

    #[test]
    fn credits_from_outro_segment_or_last_30_seconds() {
        let body = r#"{"Items":[{"Type":"Intro","StartTicks":0,"EndTicks":600000000},
            {"Type":"Outro","StartTicks":12300000000,"EndTicks":13200000000}],"TotalRecordCount":2}"#;
        assert_eq!(credits_start(Some(body), Some(1320.0)), Some(1230.0));
        // No segments (older server, no plugin): 30 s before the end.
        assert_eq!(credits_start(None, Some(1320.0)), Some(1290.0));
        assert_eq!(
            credits_start(Some(r#"{"Items":[]}"#), Some(1320.0)),
            Some(1290.0)
        );
        assert_eq!(credits_start(Some("garbage"), Some(1320.0)), Some(1290.0));
        // An outro past the runtime is ignored; nothing known → no card.
        let late = r#"{"Items":[{"Type":"Outro","StartTicks":99990000000}]}"#;
        assert_eq!(credits_start(Some(late), Some(1320.0)), Some(1290.0));
        assert_eq!(credits_start(None, None), None);
        assert_eq!(credits_start(None, Some(40.0)), None);
    }

    #[test]
    fn countdown_plays_next_and_counts_the_streak() {
        let mut u = UpNext::new(Some(100.0), 10, 0);
        assert_eq!(
            u.handle(Event::Tick {
                pos: 50.0,
                now: 0.0
            }),
            None
        );
        assert_eq!(
            u.handle(Event::Tick {
                pos: 100.0,
                now: 1.0
            }),
            Some(Action::ShowCard { left: 10 })
        );
        assert_eq!(
            u.handle(Event::Tick {
                pos: 101.0,
                now: 2.0
            }),
            Some(Action::Count { left: 9 })
        );
        assert_eq!(
            u.handle(Event::Tick {
                pos: 101.2,
                now: 2.2
            }),
            None
        );
        assert_eq!(
            u.handle(Event::Tick {
                pos: 110.0,
                now: 11.0
            }),
            Some(Action::PlayNext { auto: true })
        );
        assert_eq!(u.streak, 1);
        // Done: nothing more happens.
        assert_eq!(u.handle(Event::Ended), None);
    }

    #[test]
    fn play_now_cancel_and_end_of_file() {
        let mut u = UpNext::new(Some(100.0), 5, 2);
        u.handle(Event::Tick {
            pos: 100.0,
            now: 0.0,
        });
        assert_eq!(
            u.handle(Event::PlayNow),
            Some(Action::PlayNext { auto: false })
        );
        assert_eq!(u.streak, 0);

        // Cancel hides the card; the credits run out and the player ends.
        let mut u = UpNext::new(Some(100.0), 5, 0);
        u.handle(Event::Tick {
            pos: 100.0,
            now: 0.0,
        });
        assert_eq!(u.handle(Event::Cancel), Some(Action::Hide));
        assert_eq!(
            u.handle(Event::Tick {
                pos: 120.0,
                now: 30.0
            }),
            None
        );
        assert_eq!(u.handle(Event::Ended), Some(Action::Quit));

        // Credits shorter than the countdown: the end plays on.
        let mut u = UpNext::new(Some(100.0), 15, 0);
        u.handle(Event::Tick {
            pos: 100.0,
            now: 0.0,
        });
        assert_eq!(
            u.handle(Event::Ended),
            Some(Action::PlayNext { auto: true })
        );

        // Rewinding out of the credits takes the card away.
        let mut u = UpNext::new(Some(100.0), 10, 0);
        u.handle(Event::Tick {
            pos: 100.0,
            now: 0.0,
        });
        assert_eq!(
            u.handle(Event::Tick {
                pos: 40.0,
                now: 1.0
            }),
            Some(Action::Hide)
        );
        assert_eq!(
            u.handle(Event::Tick {
                pos: 100.0,
                now: 9.0
            }),
            Some(Action::ShowCard { left: 10 })
        );

        // No card (movie, last episode, autoplay off): the end quits.
        let mut u = UpNext::new(None, 10, 0);
        assert_eq!(u.handle(Event::Tick { pos: 1e6, now: 0.0 }), None);
        assert_eq!(u.handle(Event::PlayNow), None);
        assert_eq!(u.handle(Event::Ended), Some(Action::Quit));
    }

    #[test]
    fn asks_after_three_unattended_episodes() {
        let mut u = UpNext::new(Some(100.0), 10, STILL_WATCHING_AFTER);
        assert_eq!(
            u.handle(Event::Tick {
                pos: 100.0,
                now: 0.0
            }),
            Some(Action::AskStillWatching)
        );
        // No countdown while asking.
        assert_eq!(
            u.handle(Event::Tick {
                pos: 120.0,
                now: 60.0
            }),
            None
        );
        assert_eq!(
            u.handle(Event::Continue),
            Some(Action::PlayNext { auto: false })
        );
        assert_eq!(u.streak, 0);

        let mut u = UpNext::new(Some(100.0), 10, STILL_WATCHING_AFTER);
        u.handle(Event::Tick {
            pos: 100.0,
            now: 0.0,
        });
        assert_eq!(u.handle(Event::Stop), Some(Action::Quit));

        // Credits end while asking: back to the show page.
        let mut u = UpNext::new(Some(100.0), 10, STILL_WATCHING_AFTER);
        u.handle(Event::Tick {
            pos: 100.0,
            now: 0.0,
        });
        assert_eq!(u.handle(Event::Ended), Some(Action::Quit));

        // Any key during the episode resets the streak: a normal card.
        let mut u = UpNext::new(Some(100.0), 10, STILL_WATCHING_AFTER);
        u.handle(Event::Input);
        assert_eq!(
            u.handle(Event::Tick {
                pos: 100.0,
                now: 0.0
            }),
            Some(Action::ShowCard { left: 10 })
        );
    }
}
