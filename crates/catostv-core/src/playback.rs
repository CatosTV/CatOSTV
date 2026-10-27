//! Live native-player state for Home. A running app alone is not playback.
use crate::mpv::{self, Arg};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    CatTube,
    Catfyn,
    Cast,
    Player,
}
impl Source {
    pub fn from_window(window: &str) -> Option<Self> {
        [Self::CatTube, Self::Catfyn, Self::Cast, Self::Player]
            .into_iter()
            .find(|source| source.window() == window)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::CatTube => "CatTube",
            Self::Catfyn => "Catfyn",
            Self::Cast => "Cast",
            Self::Player => "Player",
        }
    }
    pub fn window(self) -> &'static str {
        match self {
            Self::CatTube => "cattube-player",
            Self::Catfyn => "catfyn-player",
            Self::Cast => "tv.catostv.cast",
            Self::Player => "catostv-files-player",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Self::CatTube => "play",
            Self::Catfyn => "media",
            Self::Cast => "cast",
            Self::Player => "folder",
        }
    }
    pub fn app(self) -> &'static str {
        match self {
            Self::CatTube => "cattube",
            Self::Catfyn => "catfyn",
            Self::Cast => "cast",
            Self::Player => "catostv-files-player",
        }
    }
    pub fn socket(self) -> &'static str {
        match self {
            Self::CatTube => "cattube-mpv.sock",
            Self::Catfyn => "catfyn-mpv.sock",
            Self::Cast => "catostv-cast-mpv.sock",
            Self::Player => "catostv-files-mpv.sock",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NowPlaying {
    pub source: Source,
    pub title: String,
    pub paused: bool,
    pub position: f64,
    pub duration: Option<f64>,
    pub artwork: Vec<String>,
}
impl NowPlaying {
    pub fn fraction(&self) -> Option<f32> {
        self.duration
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(|d| (self.position / d).clamp(0.0, 1.0) as f32)
    }
}
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
fn read_source(path: &Path, source: Source) -> Option<NowPlaying> {
    let get = |name| mpv::ipc_request(path, &[Arg::Str("get_property"), Arg::Str(name)]).ok();
    let media_path = get("path")?.as_str()?.to_string();
    let position = get("time-pos")?
        .as_f64()
        .filter(|p| p.is_finite() && *p >= 0.0)?;
    let title: String = get("media-title")?
        .as_str()?
        .chars()
        .filter(|c| !c.is_control())
        .take(240)
        .collect();
    if title.trim().is_empty() {
        return None;
    }
    let paused = get("pause").and_then(|v| v.as_bool()).unwrap_or(false);
    let duration = get("duration")
        .and_then(|v| v.as_f64())
        .filter(|d| d.is_finite() && *d > 0.0);
    let metadata = get("user-data/catostv/now-playing");
    let artwork = artwork_for(
        source,
        &media_path,
        metadata.as_ref().and_then(|v| v.as_str()),
    );
    if get("path")?.as_str() != Some(media_path.as_str()) {
        return None;
    }
    Some(NowPlaying {
        source,
        title,
        position,
        paused,
        duration,
        artwork,
    })
}

/// The metadata belongs to one media path, so an episode change cannot
/// reuse the previous video's artwork while its reporter catches up.
pub fn artwork_metadata(path: &str, url: &str) -> String {
    format!(
        "{{\"path\":{},\"url\":{}}}",
        mpv::json_string(path),
        mpv::json_string(url)
    )
}

fn artwork_for(source: Source, path: &str, metadata: Option<&str>) -> Vec<String> {
    if let Some(value) = metadata.and_then(mpv::parse_json)
        && value.get("path").and_then(|v| v.as_str()) == Some(path)
        && let Some(url) = value.get("url").and_then(|v| v.as_str())
        && mpv::is_playable_url(url)
    {
        return vec![url.to_string()];
    }
    if source == Source::CatTube {
        let rest = [
            "https://www.youtube.com/",
            "https://youtube.com/",
            "https://m.youtube.com/",
            "https://youtu.be/",
        ]
        .into_iter()
        .find_map(|prefix| path.strip_prefix(prefix));
        if let Some(rest) = rest {
            let id = rest
                .strip_prefix("watch?")
                .and_then(|query| query.split('&').find_map(|p| p.strip_prefix("v=")))
                .or_else(|| rest.strip_prefix("shorts/"))
                .unwrap_or(rest)
                .split(['?', '&', '/', '#'])
                .next()
                .unwrap_or("");
            if id.len() == 11
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            {
                return vec![
                    format!("https://i.ytimg.com/vi/{id}/maxresdefault.jpg"),
                    format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"),
                ];
            }
        }
    }
    if source == Source::Catfyn
        && let Some((server, rest)) = path.split_once("/Videos/")
        && (server.starts_with("https://") || server.starts_with("http://"))
        && let Some(id) = rest
            .split('?')
            .next()
            .and_then(|p| p.strip_suffix("/stream"))
        && !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return vec![
            crate::jellyfin::image_url(server, id, "Backdrop", 3840),
            crate::jellyfin::image_url(server, id, "Primary", 3840),
        ];
    }
    Vec::new()
}
/// Called on a worker: a stalled player must never block navigation.
pub fn read(runtime: &Path) -> Vec<NowPlaying> {
    let mut items = Vec::new();
    for source in [
        Source::CatTube,
        Source::Catfyn,
        Source::Cast,
        Source::Player,
    ] {
        let path = runtime.join(source.socket());
        if !path.exists() {
            continue;
        }
        if let Some(item) = read_source(&path, source) {
            items.push(item);
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    #[test]
    fn reads_real_ipc_state_and_ignores_a_missing_player() {
        let dir = crate::testutil::TempDir::new("home-player");
        let path = dir.path().join("player.sock");
        assert!(read_source(&path, Source::CatTube).is_none());
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            for value in [
                "\"https://www.youtube.com/watch?v=abcdefghijk\"",
                "75.5",
                "\"A video\\nwith a title\"",
                "true",
                "300",
                "null",
                "\"https://www.youtube.com/watch?v=abcdefghijk\"",
            ] {
                let (mut socket, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(socket.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request = mpv::parse_json(&line).unwrap();
                let id = request.get("request_id").unwrap().as_f64().unwrap() as u64;
                writeln!(
                    socket,
                    "{{\"error\":\"success\",\"request_id\":{id},\"data\":{value}}}"
                )
                .unwrap();
            }
        });
        let state = read_source(&path, Source::CatTube).unwrap();
        assert_eq!(state.title, "A videowith a title");
        assert!(state.paused);
        assert_eq!(state.fraction(), Some(75.5 / 300.0));
        assert_eq!(clock(state.position), "1:15");
        assert_eq!(state.source.window(), "cattube-player");
        server.join().unwrap();
    }

    #[test]
    fn reads_all_loaded_players_including_paused_and_files() {
        let dir = crate::testutil::TempDir::new("home-players");
        assert!(read(dir.path()).is_empty());
        let mut servers = Vec::new();
        for source in [Source::CatTube, Source::Catfyn, Source::Player] {
            let listener = UnixListener::bind(dir.path().join(source.socket())).unwrap();
            servers.push(std::thread::spawn(move || {
                for value in [
                    "\"https://www.youtube.com/watch?v=abcdefghijk\"",
                    "45.5",
                    "\"Loaded video\"",
                    if source == Source::Catfyn {
                        "true"
                    } else {
                        "false"
                    },
                    "180",
                    "null",
                    "\"https://www.youtube.com/watch?v=abcdefghijk\"",
                ] {
                    let (mut socket, _) = listener.accept().unwrap();
                    let mut line = String::new();
                    BufReader::new(socket.try_clone().unwrap())
                        .read_line(&mut line)
                        .unwrap();
                    let request = mpv::parse_json(&line).unwrap();
                    let id = request.get("request_id").unwrap().as_f64().unwrap() as u64;
                    writeln!(
                        socket,
                        "{{\"error\":\"success\",\"request_id\":{id},\"data\":{value}}}"
                    )
                    .unwrap();
                }
            }));
        }
        let states = read(dir.path());
        assert_eq!(
            states.iter().map(|p| p.source).collect::<Vec<_>>(),
            vec![Source::CatTube, Source::Catfyn, Source::Player]
        );
        assert!(!states[0].paused);
        assert!(states[1].paused);
        assert_eq!(states[2].source.window(), "catostv-files-player");
        assert!(states.iter().all(|p| p.position == 45.5));
        for server in servers {
            server.join().unwrap();
        }
    }
}

#[cfg(test)]
mod artwork_tests {
    use super::*;

    #[test]
    fn artwork_is_bound_to_the_video_and_rejects_stale_metadata() {
        let path = "https://www.youtube.com/watch?v=abcdefghijk&list=PL-test";
        let first = artwork_for(Source::CatTube, path, None);
        assert_eq!(
            first[0],
            "https://i.ytimg.com/vi/abcdefghijk/maxresdefault.jpg"
        );
        let next = "https://youtu.be/123456789ab";
        let stale = artwork_metadata(path, &first[0]);
        assert!(artwork_for(Source::CatTube, next, Some(&stale))[0].contains("/123456789ab/"));
        assert!(
            artwork_for(
                Source::CatTube,
                "https://example.com/watch?v=abcdefghijk",
                None
            )
            .is_empty()
        );
        let movie = "https://media.example/Videos/movie123/stream?static=true";
        let url = "https://media.example/Items/movie123/Images/Backdrop/0?maxWidth=3840";
        assert_eq!(
            artwork_for(Source::Catfyn, movie, Some(&artwork_metadata(movie, url))),
            vec![url]
        );
        assert!(
            artwork_for(Source::Catfyn, movie, Some(&artwork_metadata("other", url)))[0]
                .contains("/movie123/")
        );
        assert!(
            !artwork_for(
                Source::Catfyn,
                movie,
                Some(&artwork_metadata(movie, "file:///etc/passwd"))
            )
            .iter()
            .any(|url| url.starts_with("file:"))
        );
    }

    #[test]
    fn native_volume_targets_exact_player_windows() {
        for source in [
            Source::CatTube,
            Source::Catfyn,
            Source::Player,
            Source::Cast,
        ] {
            assert_eq!(Source::from_window(source.window()), Some(source));
        }
        assert_eq!(Source::from_window("cattube"), None);
        assert_eq!(Source::from_window("catostv-shell"), None);
    }
}
