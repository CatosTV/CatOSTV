//! CatTube (the YouTube app based on SmartTube) on CatosTV's Home: a row
//! of the newest videos from the signed-in account's subscriptions, led by
//! a "Recommended for you" button that opens CatTube on YouTube's own
//! recommendations.
//!
//! CatTube keeps the sign-in and the YouTube logic; CatosTV only runs
//! `cattube --feed subscriptions` now and then and reads its lines
//! (tab-separated: id, title, channel, facts, length, picture). Exit status
//! 2 means not signed in: no row.

use crate::jellyfin::Item;
use crate::process::Cmd;
use std::time::Duration;

pub const BINARY: &str = "cattube";
/// The row's title on Home.
pub const ROW: &str = "From your subscriptions";
/// `kind_raw` of a CatTube video and of the button on Home.
pub const VIDEO: &str = "YouTube";
pub const BUTTON: &str = "YouTubeHome";

pub fn feed_cmd() -> Cmd {
    Cmd::new(BINARY)
        .args(["--feed", "subscriptions"])
        .timeout(Duration::from_secs(45))
}

pub fn is_cattube(it: &Item) -> bool {
    it.kind_raw == VIDEO || it.kind_raw == BUTTON
}

fn is_video_id(s: &str) -> bool {
    s.len() == 11
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The button that opens CatTube on YouTube's recommendations.
pub fn button() -> Item {
    Item {
        id: "cattube-home".into(),
        name: "Recommended for you".into(),
        kind_raw: BUTTON.into(),
        overview: "Your YouTube home, in CatTube".into(),
        ..Item::default()
    }
}

/// `cattube --feed` lines → Home items (the button first).
pub fn parse_feed(text: &str) -> Vec<Item> {
    let videos: Vec<Item> = text
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let (id, title) = (*f.first()?, *f.get(1)?);
            if !is_video_id(id) || title.trim().is_empty() {
                return None;
            }
            let thumb = f.get(5).copied().unwrap_or("");
            Some(Item {
                id: id.to_string(),
                name: title.trim().to_string(),
                kind_raw: VIDEO.into(),
                // Channel and facts for the hero ("Blender Studio · 2 days ago").
                overview: [f.get(2).copied(), f.get(3).copied()]
                    .into_iter()
                    .flatten()
                    .filter(|s| !s.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(" · "),
                age_rating: f.get(4).copied().unwrap_or("").to_string(),
                thumb: if thumb.starts_with("https://i.ytimg.com/") {
                    thumb.to_string()
                } else {
                    format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")
                },
                ..Item::default()
            })
        })
        .take(24)
        .collect();
    if videos.is_empty() {
        return Vec::new();
    }
    std::iter::once(button()).chain(videos).collect()
}

/// What OK on a CatTube item runs.
pub fn open_args(it: &Item) -> Vec<String> {
    if it.kind_raw == VIDEO && is_video_id(&it.id) {
        vec![
            "--play".into(),
            it.id.clone(),
            "--title".into(),
            it.name
                .chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect(),
        ]
    } else {
        vec!["--section".into(), "home".into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_lines_become_home_items() {
        let items = parse_feed(
            "WhWc3b3KhnY\tSpring - Blender Open Movie\tBlender Studio\t10M views • 7 years ago\t7:45\thttps://i.ytimg.com/vi/WhWc3b3KhnY/hqdefault.jpg\n\
             bad\tnope\n\
             B4EPW7JUMTM\tForevergreen\tSomeone\t\t\thttps://evil.example/x.jpg\n",
        );
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].kind_raw, BUTTON);
        let v = &items[1];
        assert_eq!(
            (v.id.as_str(), v.name.as_str()),
            ("WhWc3b3KhnY", "Spring - Blender Open Movie")
        );
        assert_eq!(v.overview, "Blender Studio · 10M views • 7 years ago");
        assert_eq!(v.age_rating, "7:45");
        assert!(is_cattube(v));
        // Pictures only from YouTube's own address.
        assert_eq!(
            items[2].thumb,
            "https://i.ytimg.com/vi/B4EPW7JUMTM/hqdefault.jpg"
        );
        assert_eq!(
            open_args(v),
            [
                "--play",
                "WhWc3b3KhnY",
                "--title",
                "Spring - Blender Open Movie"
            ]
        );
        assert_eq!(open_args(&items[0]), ["--section", "home"]);
        // Nothing to show: no row at all (not a lone button).
        assert!(parse_feed("").is_empty());
        assert_eq!(feed_cmd().args, ["--feed", "subscriptions"]);
    }
}
