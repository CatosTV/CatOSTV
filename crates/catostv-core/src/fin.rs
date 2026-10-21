//! Catfyn screens and navigation (pure state machine, like `shell.rs`).
//!
//! The shell owns one [`Fin`] while Catfyn is open. Input goes to
//! [`Fin::handle`], which returns [`FinEffect`]s the GUI performs (network
//! jobs, playback) and feeds back with the `on_*` methods. Drawing lives in
//! `src/ui/fin.rs`; the Jellyfin API in [`crate::jellyfin`].
//!
//! Screens (a stack; Back pops, Back on the last one closes Catfyn):
//!
//! ```text
//! Connect   servers found on the network · "Enter address"
//! Address   on-screen keyboard for a server address
//! SignIn    user name, then password (on-screen keyboard)
//! Browse    Continue watching · Next up · Libraries · Latest in <library>…
//!           (Up from the first row: the account button)
//! Library   poster grid of one library
//! Series    seasons row + episode list
//! Details   Resume / Play from start / Go to series
//! Accounts  accounts (OK = use), Add account; Options on an account =
//!           incognito on/off, sign out
//! ```

use crate::input::Action;
use crate::installer::Scan;
#[cfg(test)]
use crate::jellyfin::Account;
use crate::jellyfin::{Accounts, Backend, Item, Kind, Server};
use crate::osk::{Osk, OskEvent};
use crate::plex::PlexServer;

/// What the shell knows about Catfyn outside Catfyn itself: the accounts
/// and, for CatosTV Home, what the home account can continue watching.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Media {
    pub accounts: Accounts,
    /// Rows for CatosTV Home (Continue watching, Movies, TV shows), from the
    /// home account; empty until Catfyn is signed in.
    pub home: Vec<Row>,
    /// Why Home has no rows when it should have some ("Your Jellyfin
    /// server at … isn't answering"); `None` when there is nothing to say.
    pub home_note: Option<String>,
}

/// What Home says when no media server answered.
pub fn unreachable_note(failed: &[(crate::jellyfin::Backend, String)]) -> Option<String> {
    match failed {
        [] => None,
        [(backend, name)] => Some(format!(
            "Your {} server at {name} isn't answering, so there are no shows here right now.",
            backend.label()
        )),
        _ => Some(
            "Your media servers aren't answering, so there are no shows here right now.".into(),
        ),
    }
}

impl Media {
    /// Item at (row, column) of the Home rows.
    pub fn item(&self, row: usize, col: usize) -> Option<&Item> {
        self.home.get(row).and_then(|r| r.items.get(col))
    }
}

/// CatosTV Home's rows from several servers in one: rows with the same
/// title become one, taking an item from each server in turn (so neither
/// server's newest drown the other's), in the first server's row order.
pub fn merge_rows(per_server: Vec<Vec<Row>>) -> Vec<Row> {
    let mut titles: Vec<String> = Vec::new();
    for rows in &per_server {
        for r in rows {
            if !titles.contains(&r.title) {
                titles.push(r.title.clone());
            }
        }
    }
    titles
        .into_iter()
        .filter_map(|title| {
            let lists: Vec<&Vec<Item>> = per_server
                .iter()
                .filter_map(|rows| rows.iter().find(|r| r.title == title).map(|r| &r.items))
                .collect();
            let longest = lists.iter().map(|l| l.len()).max().unwrap_or(0);
            let mut items = Vec::new();
            for i in 0..longest {
                for l in &lists {
                    if let Some(it) = l.get(i) {
                        items.push(it.clone());
                    }
                }
            }
            items.truncate(24);
            (!items.is_empty()).then_some(Row {
                title,
                items,
                libraries: false,
            })
        })
        .collect()
}

/// No accounts, nothing to continue (tests, previews).
pub static NO_MEDIA: Media = Media {
    accounts: Accounts {
        device_id: String::new(),
        list: Vec::new(),
        active: 0,
    },
    home: Vec::new(),
    home_note: None,
};

/// Poster grid columns in a library.
pub const GRID_COLS: usize = 6;

#[derive(Debug, Clone, PartialEq)]
pub enum FinEffect {
    Discover,
    /// Look for Plex servers on this network.
    DiscoverPlex,
    Login {
        server: Server,
        user: String,
        password: String,
    },
    /// Load the Browse rows of the active account.
    LoadHome,
    LoadLibrary(String),
    LoadSeasons(String),
    LoadEpisodes {
        series: String,
        season: String,
    },
    Play {
        item: Box<Item>,
        from_start: bool,
    },
    /// Plex: get a code for plex.tv/link (the shell then waits for it to be
    /// entered and answers with [`Fin::on_plex_linked`]).
    PlexPin,
    /// Stop waiting for the code (left the screen).
    PlexStop,
    /// Plex: use this server of the linked plex.tv account.
    PlexUse {
        server: PlexServer,
        user_id: String,
        user_name: String,
        cloud_token: String,
    },
    /// Plex: a server typed in by address, used without a sign-in.
    PlexAddress(String),
    SwitchAccount(usize),
    SetIncognito(usize, bool),
    SignOut(usize),
    /// Settings: "Play next episode automatically" flipped.
    ToggleAutoplay,
    /// Settings: the next-episode countdown one step shorter (-1) / longer.
    CountdownStep(i8),
    /// Leave Catfyn.
    Close,
    Toast(String),
}

/// A row on the Browse screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub title: String,
    pub items: Vec<Item>,
    /// Library tiles (wide, text) instead of posters.
    pub libraries: bool,
}

/// Everything the Browse screen shows, loaded in one go.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HomeData {
    pub resume: Vec<Item>,
    pub next_up: Vec<Item>,
    pub libraries: Vec<Item>,
    /// Latest items per library (same order as `libraries`).
    pub latest: Vec<(String, Vec<Item>)>,
}

fn same_series(a: &Item, b: &Item) -> bool {
    !a.series_id.is_empty() && a.series_id == b.series_id
}

/// Continue watching lists each series once (its most recently played
/// episode, which the server lists first); half-watched movies all stay
/// ("same series, multiple half-played episodes all show up").
pub fn one_per_series(items: &[Item]) -> Vec<Item> {
    let mut v: Vec<Item> = Vec::new();
    for i in items {
        if !v.iter().any(|x| same_series(x, i)) {
            v.push(i.clone());
        }
    }
    v
}

impl HomeData {
    pub fn rows(&self) -> Vec<Row> {
        let mut v = Vec::new();
        if !self.resume.is_empty() {
            v.push(Row {
                title: "Continue watching".into(),
                items: one_per_series(&self.resume),
                libraries: false,
            });
        }
        if !self.next_up.is_empty() {
            v.push(Row {
                title: "Next up".into(),
                items: self.next_up.clone(),
                libraries: false,
            });
        }
        if !self.libraries.is_empty() {
            v.push(Row {
                title: "Libraries".into(),
                items: self.libraries.clone(),
                libraries: true,
            });
        }
        for (lib, items) in &self.latest {
            if !items.is_empty() {
                v.push(Row {
                    title: format!("Latest in {lib}"),
                    items: items.clone(),
                    libraries: false,
                });
            }
        }
        v
    }

    /// CatosTV Home's rows: Continue watching, then the latest movies and TV
    /// shows (from the libraries of those kinds). Empty rows are left out.
    pub fn home_rows(&self) -> Vec<Row> {
        let mut v = Vec::new();
        let cont = self.continue_items();
        if !cont.is_empty() {
            v.push(Row {
                title: "Continue watching".into(),
                items: cont,
                libraries: false,
            });
        }
        for (title, kind) in [("Movies", "movies"), ("TV shows", "tvshows")] {
            let items: Vec<Item> = self
                .libraries
                .iter()
                .filter(|l| l.collection == kind)
                .filter_map(|l| self.latest.iter().find(|(n, _)| *n == l.name))
                .flat_map(|(_, items)| items.iter().cloned())
                .take(16)
                .collect();
            if !items.is_empty() {
                v.push(Row {
                    title: title.into(),
                    items,
                    libraries: false,
                });
            }
        }
        v
    }

    /// For CatosTV Home: what to continue, newest first, no duplicates.
    pub fn continue_items(&self) -> Vec<Item> {
        let mut v = one_per_series(&self.resume);
        for i in &self.next_up {
            if !v.iter().any(|x| x.id == i.id || same_series(x, i)) {
                v.push(i.clone());
            }
        }
        v.truncate(12);
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInStep {
    User,
    Password,
}

#[derive(Debug, Clone)]
pub enum Screen {
    /// Add a server: the Jellyfin | Plex switch (index 0), then Jellyfin's
    /// servers found on the network and "Enter address", or Plex's "Sign in
    /// with Plex" and "Enter address".
    Connect {
        backend: Backend,
        servers: Scan<Server>,
        index: usize,
    },
    Address {
        backend: Backend,
        osk: Box<Osk>,
    },
    /// The code to enter at plex.tv/link (`Busy` while Plex makes one).
    PlexLink {
        code: Scan<String>,
    },
    /// The linked plex.tv account's servers, to pick one.
    PlexServers {
        list: Vec<PlexServer>,
        user_id: String,
        user_name: String,
        cloud_token: String,
        index: usize,
        busy: bool,
    },
    SignIn {
        server: Server,
        step: SignInStep,
        user: String,
        osk: Box<Osk>,
        busy: bool,
    },
    Browse,
    Library {
        lib: Item,
        items: Scan<Item>,
        index: usize,
    },
    Series {
        series: Item,
        seasons: Scan<Item>,
        season: usize,
        episodes: Scan<Item>,
        /// false = seasons row, true = episode list.
        in_episodes: bool,
        index: usize,
    },
    Details {
        item: Item,
        index: usize,
    },
    /// Catfyn's settings: the
    /// accounts, "Add account", then playback, [`SETTINGS_AUTOPLAY`] and
    /// [`SETTINGS_COUNTDOWN`] rows after the accounts.
    Settings {
        index: usize,
        /// Options of account `i` open: (i, focused option).
        menu: Option<(usize, usize)>,
    },
}

/// Details buttons for an item.
pub fn detail_actions(item: &Item) -> Vec<&'static str> {
    let mut v = Vec::new();
    if item.playable() {
        if item.position > 5.0 {
            v.push("Resume");
            v.push("Play from start");
        } else {
            v.push("Play");
        }
    }
    if item.kind() == Kind::Episode && !item.series_id.is_empty() {
        v.push("Go to series");
    }
    if item.kind() == Kind::Series {
        v.push("Episodes");
    }
    v
}

pub const ACCOUNT_OPTIONS: [&str; 3] = ["Use this account", "Incognito", "Sign out"];

/// Settings rows after the accounts: Add account, then playback.
pub const SETTINGS_ADD: usize = 0;
pub const SETTINGS_AUTOPLAY: usize = 1;
pub const SETTINGS_COUNTDOWN: usize = 2;
/// Rows after the accounts.
pub const SETTINGS_EXTRA: usize = 3;

#[derive(Debug, Clone)]
pub struct Fin {
    pub stack: Vec<Screen>,
    pub home: Scan<HomeData>,
    /// Browse focus: row, and the column in each row.
    pub row: usize,
    pub cols: Vec<usize>,
    /// The header above the rows has focus (pill tabs: the libraries, then
    /// the account button at the end).
    pub header: bool,
    /// Which header pill: 0..n = library tabs, n = account.
    pub header_index: usize,
    /// Last error to show (sign-in, loading).
    pub error: Option<String>,
}

impl Fin {
    /// Open Catfyn: Browse when an account exists, else Connect (and look
    /// for servers right away).
    pub fn open(accounts: &Accounts) -> (Self, Vec<FinEffect>) {
        let mut f = Fin {
            stack: Vec::new(),
            home: Scan::Idle,
            row: 0,
            cols: Vec::new(),
            header: false,
            header_index: 0,
            error: None,
        };
        let eff = if accounts.active().is_some() {
            f.stack.push(Screen::Browse);
            f.home = Scan::Busy;
            vec![FinEffect::LoadHome]
        } else {
            f.stack.push(connect_screen());
            vec![FinEffect::Discover]
        };
        (f, eff)
    }

    pub fn screen(&self) -> Option<&Screen> {
        self.stack.last()
    }

    /// An on-screen keyboard has focus (typed characters go to it).
    pub fn text_mode(&self) -> bool {
        matches!(
            self.screen(),
            Some(Screen::Address { .. } | Screen::SignIn { .. })
        )
    }

    pub fn rows(&self) -> Vec<Row> {
        match &self.home {
            Scan::Done(v) => v.first().map(HomeData::rows).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn col(&self, row: usize) -> usize {
        self.cols.get(row).copied().unwrap_or(0)
    }

    fn set_col(&mut self, row: usize, c: usize) {
        if self.cols.len() <= row {
            self.cols.resize(row + 1, 0);
        }
        self.cols[row] = c;
    }

    /// The Browse item with focus.
    pub fn focused_item(&self) -> Option<Item> {
        if self.header {
            return None;
        }
        let rows = self.rows();
        rows.get(self.row)
            .and_then(|r| r.items.get(self.col(self.row)))
            .cloned()
    }

    fn back(&mut self) -> Vec<FinEffect> {
        self.error = None;
        self.stack.pop();
        if self.stack.is_empty() {
            return vec![FinEffect::Close];
        }
        vec![]
    }

    /// Open an item from anywhere: libraries, series, details.
    fn open_item(&mut self, item: Item) -> Vec<FinEffect> {
        match item.kind() {
            Kind::Library => {
                let id = item.id.clone();
                self.stack.push(Screen::Library {
                    lib: item,
                    items: Scan::Busy,
                    index: 0,
                });
                vec![FinEffect::LoadLibrary(id)]
            }
            Kind::Series => self.open_series(item),
            Kind::Season => {
                // A season tile: its series' episodes.
                let series = Item {
                    id: item.series_id.clone(),
                    name: item.series_name.clone(),
                    kind_raw: "Series".into(),
                    ..Item::default()
                };
                self.open_series(series)
            }
            _ => {
                self.stack.push(Screen::Details { item, index: 0 });
                vec![]
            }
        }
    }

    pub fn open_series(&mut self, series: Item) -> Vec<FinEffect> {
        let id = series.id.clone();
        self.stack.push(Screen::Series {
            series,
            seasons: Scan::Busy,
            season: 0,
            episodes: Scan::Idle,
            in_episodes: false,
            index: 0,
        });
        vec![FinEffect::LoadSeasons(id)]
    }

    pub fn handle(&mut self, action: Action, accounts: &Accounts) -> Vec<FinEffect> {
        let Some(screen) = self.stack.last_mut() else {
            return vec![FinEffect::Close];
        };
        match screen {
            Screen::Connect {
                backend,
                servers,
                index,
            } => {
                let n = 1 + connect_rows(*backend, servers);
                // Each side looks for its own servers when it is chosen.
                let toggle = |backend: &mut Backend| {
                    *backend = match backend {
                        Backend::Jellyfin => Backend::Plex,
                        Backend::Plex => Backend::Jellyfin,
                    };
                    vec![match backend {
                        Backend::Jellyfin => FinEffect::Discover,
                        Backend::Plex => FinEffect::DiscoverPlex,
                    }]
                };
                match action {
                    Action::Up => *index = index.saturating_sub(1),
                    Action::Down => *index = (*index + 1).min(n - 1),
                    Action::Left | Action::Right | Action::Ok | Action::OkLong if *index == 0 => {
                        *servers = Scan::Busy;
                        return toggle(backend);
                    }
                    Action::Ok | Action::OkLong => {
                        let i = *index - 1;
                        match backend {
                            Backend::Jellyfin => {
                                if let Some(s) = servers.items().get(i).cloned() {
                                    self.stack.push(sign_in(s));
                                } else {
                                    self.stack.push(address_screen(Backend::Jellyfin));
                                }
                            }
                            Backend::Plex if i == 0 => {
                                self.error = None;
                                self.stack.push(Screen::PlexLink { code: Scan::Busy });
                                return vec![FinEffect::PlexPin];
                            }
                            // A server found on this network.
                            Backend::Plex if i <= servers.items().len() => {
                                self.error = None;
                                let s = &servers.items()[i - 1];
                                return vec![FinEffect::PlexAddress(s.address.clone())];
                            }
                            Backend::Plex => self.stack.push(address_screen(Backend::Plex)),
                        }
                    }
                    Action::Options => {
                        *servers = Scan::Busy;
                        return vec![match backend {
                            Backend::Jellyfin => FinEffect::Discover,
                            Backend::Plex => FinEffect::DiscoverPlex,
                        }];
                    }
                    Action::Back => return self.back(),
                    _ => {}
                }
                vec![]
            }
            Screen::Address { backend, osk } => match osk.handle(action) {
                OskEvent::Submitted => {
                    let text = osk.text().to_string();
                    if *backend == Backend::Plex {
                        return match crate::plex::normalize_address(&text) {
                            Some(address) => {
                                self.error = None;
                                vec![FinEffect::PlexAddress(address)]
                            }
                            None => {
                                self.error = Some("That doesn't look like a server address".into());
                                vec![]
                            }
                        };
                    }
                    match crate::jellyfin::normalize_address(&text) {
                        Some(address) => {
                            self.error = None;
                            self.stack.pop();
                            self.stack.push(sign_in(Server {
                                id: String::new(),
                                name: text.trim().to_string(),
                                address,
                            }));
                            vec![]
                        }
                        None => {
                            self.error = Some("That doesn't look like a server address".into());
                            vec![]
                        }
                    }
                }
                OskEvent::Unhandled if action == Action::Back => self.back(),
                _ => vec![],
            },
            Screen::PlexLink { code } => match action {
                Action::Back => {
                    let mut v = vec![FinEffect::PlexStop];
                    v.extend(self.back());
                    v
                }
                // Expired or failed: OK asks for a new code.
                Action::Ok | Action::OkLong if matches!(code, Scan::Failed(_)) => {
                    *code = Scan::Busy;
                    self.error = None;
                    vec![FinEffect::PlexStop, FinEffect::PlexPin]
                }
                _ => vec![],
            },
            Screen::PlexServers {
                list,
                user_id,
                user_name,
                cloud_token,
                index,
                busy,
            } => {
                if *busy {
                    return if action == Action::Back {
                        self.back()
                    } else {
                        vec![]
                    };
                }
                match action {
                    Action::Back => return self.back(),
                    Action::Up => *index = index.saturating_sub(1),
                    Action::Down => *index = (*index + 1).min(list.len().saturating_sub(1)),
                    Action::Ok | Action::OkLong if *index < list.len() => {
                        *busy = true;
                        self.error = None;
                        return vec![FinEffect::PlexUse {
                            server: list[*index].clone(),
                            user_id: user_id.clone(),
                            user_name: user_name.clone(),
                            cloud_token: cloud_token.clone(),
                        }];
                    }
                    _ => {}
                }
                vec![]
            }
            Screen::SignIn {
                server,
                step,
                user,
                osk,
                busy,
            } => {
                if *busy {
                    return vec![];
                }
                match osk.handle(action) {
                    OskEvent::Submitted => match step {
                        SignInStep::User => {
                            let name = osk.text().trim().to_string();
                            if name.is_empty() {
                                return vec![];
                            }
                            *user = name;
                            *step = SignInStep::Password;
                            **osk = Osk::new(true, "Sign in");
                            vec![]
                        }
                        SignInStep::Password => {
                            *busy = true;
                            self.error = None;
                            vec![FinEffect::Login {
                                server: server.clone(),
                                user: user.clone(),
                                password: osk.text().to_string(),
                            }]
                        }
                    },
                    OskEvent::Unhandled if action == Action::Back => {
                        if *step == SignInStep::Password {
                            *step = SignInStep::User;
                            let mut o = Osk::new(false, "Next");
                            o.set_text(user);
                            **osk = o;
                            vec![]
                        } else {
                            self.back()
                        }
                    }
                    _ => vec![],
                }
            }
            Screen::Browse => self.handle_browse(action, accounts),
            Screen::Library { lib, items, index } => {
                let n = items.items().len();
                match action {
                    Action::Back => return self.back(),
                    // A failed load: OK tries again.
                    Action::Ok if matches!(items, Scan::Failed(_)) => {
                        *items = Scan::Busy;
                        return vec![FinEffect::LoadLibrary(lib.id.clone())];
                    }
                    Action::Ok | Action::OkLong if *index < n => {
                        let it = items.items()[*index].clone();
                        return self.open_item(it);
                    }
                    a => {
                        if let Some(i) = crate::shell::grid_move(*index, a, GRID_COLS, n) {
                            *index = i;
                        }
                    }
                }
                vec![]
            }
            Screen::Series {
                series,
                seasons,
                season,
                episodes,
                in_episodes,
                index,
            } => {
                let ns = seasons.items().len();
                let ne = episodes.items().len();
                match action {
                    Action::Back => return self.back(),
                    Action::Left if !*in_episodes && *season > 0 => {
                        *season -= 1;
                        return load_season(series, seasons, *season, episodes, index);
                    }
                    Action::Right if !*in_episodes && *season + 1 < ns => {
                        *season += 1;
                        return load_season(series, seasons, *season, episodes, index);
                    }
                    Action::Ok | Action::OkLong if !*in_episodes => {
                        return load_season(series, seasons, *season, episodes, index);
                    }
                    Action::Down if !*in_episodes && ne > 0 => *in_episodes = true,
                    Action::Down if *in_episodes => *index = (*index + 1).min(ne.saturating_sub(1)),
                    Action::Up if *in_episodes && *index == 0 => *in_episodes = false,
                    Action::Up if *in_episodes => *index -= 1,
                    Action::Ok if *in_episodes && *index < ne => {
                        let it = episodes.items()[*index].clone();
                        return vec![FinEffect::Play {
                            item: Box::new(it),
                            from_start: false,
                        }];
                    }
                    Action::OkLong | Action::Options if *in_episodes && *index < ne => {
                        let it = episodes.items()[*index].clone();
                        self.stack.push(Screen::Details { item: it, index: 0 });
                    }
                    _ => {}
                }
                vec![]
            }
            Screen::Details { item, index } => {
                let acts = detail_actions(item);
                match action {
                    Action::Back => return self.back(),
                    Action::Left | Action::Up => *index = index.saturating_sub(1),
                    Action::Right | Action::Down => {
                        *index = (*index + 1).min(acts.len().saturating_sub(1))
                    }
                    Action::Ok | Action::OkLong => {
                        let item = item.clone();
                        return match acts.get(*index).copied() {
                            Some("Resume") | Some("Play") => vec![FinEffect::Play {
                                item: Box::new(item),
                                from_start: false,
                            }],
                            Some("Play from start") => vec![FinEffect::Play {
                                item: Box::new(item),
                                from_start: true,
                            }],
                            Some("Go to series") => {
                                let series = Item {
                                    server: item.server.clone(),
                                    backend: item.backend,
                                    id: item.series_id.clone(),
                                    name: item.series_name.clone(),
                                    kind_raw: "Series".into(),
                                    thumb: item.series_thumb.clone(),
                                    art: item.art.clone(),
                                    ..Item::default()
                                };
                                self.open_series(series)
                            }
                            Some("Episodes") => self.open_series(item),
                            _ => vec![],
                        };
                    }
                    _ => {}
                }
                vec![]
            }
            Screen::Settings { index, menu } => {
                let accs = accounts.list.len();
                let n = accs + SETTINGS_EXTRA;
                if let Some((i, opt)) = menu {
                    match action {
                        Action::Back => *menu = None,
                        Action::Up => *opt = opt.saturating_sub(1),
                        Action::Down => *opt = (*opt + 1).min(ACCOUNT_OPTIONS.len() - 1),
                        Action::Ok | Action::OkLong => {
                            let (i, opt) = (*i, *opt);
                            *menu = None;
                            return match opt {
                                0 => {
                                    self.stack.pop();
                                    self.home = Scan::Busy;
                                    self.row = 0;
                                    self.cols.clear();
                                    vec![FinEffect::SwitchAccount(i), FinEffect::LoadHome]
                                }
                                1 => {
                                    let on = !accounts.list.get(i).is_some_and(|a| a.incognito);
                                    vec![FinEffect::SetIncognito(i, on)]
                                }
                                _ => {
                                    let last = accounts.list.len() <= 1;
                                    let mut v = vec![FinEffect::SignOut(i)];
                                    if last {
                                        // No account left: start over.
                                        self.stack.clear();
                                        self.stack.push(connect_screen());
                                        self.home = Scan::Idle;
                                        v.push(FinEffect::Discover);
                                    } else if i == accounts.active {
                                        self.home = Scan::Busy;
                                        v.push(FinEffect::LoadHome);
                                    }
                                    v
                                }
                            };
                        }
                        _ => {}
                    }
                    return vec![];
                }
                match action {
                    Action::Back => return self.back(),
                    Action::Up => *index = index.saturating_sub(1),
                    Action::Down => *index = (*index + 1).min(n - 1),
                    Action::Ok if *index == accs + SETTINGS_ADD => {
                        self.stack.push(connect_screen());
                        return vec![FinEffect::Discover];
                    }
                    Action::Ok if *index == accs + SETTINGS_AUTOPLAY => {
                        return vec![FinEffect::ToggleAutoplay];
                    }
                    Action::Left if *index == accs + SETTINGS_COUNTDOWN => {
                        return vec![FinEffect::CountdownStep(-1)];
                    }
                    Action::Right | Action::Ok if *index == accs + SETTINGS_COUNTDOWN => {
                        return vec![FinEffect::CountdownStep(1)];
                    }
                    Action::Ok | Action::OkLong | Action::Options if *index < accs => {
                        *menu = Some((*index, 0));
                    }
                    _ => {}
                }
                vec![]
            }
        }
    }

    /// The libraries shown as header tabs.
    pub fn header_libraries(&self) -> Vec<Item> {
        match &self.home {
            Scan::Done(v) => v.first().map(|d| d.libraries.clone()).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn handle_browse(&mut self, action: Action, accounts: &Accounts) -> Vec<FinEffect> {
        let rows = self.rows();
        if self.header {
            let libs = self.header_libraries();
            let n = libs.len();
            // The Jellyfin | Plex switch, when both have an account, sits
            // between the library tabs and the account button.
            let switch = header_switch(accounts);
            let last = n + usize::from(switch);
            self.header_index = self.header_index.min(last);
            match action {
                Action::Down if !rows.is_empty() => self.header = false,
                Action::Left => self.header_index = self.header_index.saturating_sub(1),
                Action::Right => self.header_index = (self.header_index + 1).min(last),
                Action::Ok | Action::OkLong => {
                    if self.header_index < n {
                        return self.open_item(libs[self.header_index].clone());
                    }
                    if switch && self.header_index == n {
                        return self.switch_backend(accounts);
                    }
                    self.stack.push(Screen::Settings {
                        index: accounts.active,
                        menu: None,
                    });
                }
                Action::Back => return self.back(),
                _ => {}
            }
            return vec![];
        }
        match action {
            Action::Back => return self.back(),
            Action::Up if self.row == 0 => self.header = true,
            Action::Up => self.row -= 1,
            Action::Down if self.row + 1 < rows.len() => self.row += 1,
            Action::Left => {
                let c = self.col(self.row);
                self.set_col(self.row, c.saturating_sub(1));
            }
            Action::Right => {
                let n = rows.get(self.row).map_or(0, |r| r.items.len());
                let c = self.col(self.row);
                self.set_col(self.row, (c + 1).min(n.saturating_sub(1)));
            }
            Action::Options | Action::OkLong => {
                if let Some(it) = self.focused_item() {
                    self.stack.push(Screen::Details { item: it, index: 0 });
                }
            }
            Action::Ok if matches!(self.home, Scan::Failed(_)) => {
                self.home = Scan::Busy;
                return vec![FinEffect::LoadHome];
            }
            Action::Ok => {
                if let Some(it) = self.focused_item() {
                    // Continue watching / Next up: play right away.
                    if it.playable() && self.row < 2 && !rows[self.row].libraries {
                        return vec![FinEffect::Play {
                            item: Box::new(it),
                            from_start: false,
                        }];
                    }
                    return self.open_item(it);
                }
                if rows.is_empty() {
                    self.header = true;
                }
            }
            _ => {}
        }
        vec![]
    }

    /// Pointer: focus (row, col) on the current screen. Returns false
    /// when there is nothing there. Rows/columns per screen:
    /// Browse (row, item) with row `usize::MAX` = the account button;
    /// Library (0, index); Series (0, season) / (1, episode); Connect,
    /// Accounts (0, index); Details (0, button).
    pub fn focus(&mut self, row: usize, col: usize, accounts: &Accounts) -> bool {
        let rows = self.rows();
        match self.stack.last_mut() {
            Some(Screen::Browse) => {
                if row == usize::MAX {
                    self.header = true;
                    self.header_index = col
                        .min(self.header_libraries().len() + usize::from(header_switch(accounts)));
                    return true;
                }
                if rows.get(row).is_some_and(|r| col < r.items.len()) {
                    self.header = false;
                    self.row = row;
                    self.set_col(row, col);
                    return true;
                }
                false
            }
            Some(Screen::Library { items, index, .. }) if col < items.items().len() => {
                *index = col;
                true
            }
            Some(Screen::Series {
                seasons,
                season,
                episodes,
                in_episodes,
                index,
                series,
            }) => {
                if row == 0 && col < seasons.items().len() {
                    // Focus only; the click's OK loads the season.
                    let _ = series;
                    *in_episodes = false;
                    if *season != col {
                        *season = col;
                        *episodes = Scan::Idle;
                        *index = 0;
                    }
                    true
                } else if row == 1 && col < episodes.items().len() {
                    *in_episodes = true;
                    *index = col;
                    true
                } else {
                    false
                }
            }
            Some(Screen::Connect {
                backend,
                servers,
                index,
            }) if col <= connect_rows(*backend, servers) => {
                *index = col;
                true
            }
            Some(Screen::PlexServers { list, index, .. }) if col < list.len() => {
                *index = col;
                true
            }
            Some(Screen::Settings { index, menu: None })
                if col < accounts.list.len() + SETTINGS_EXTRA =>
            {
                *index = col;
                true
            }
            Some(Screen::Details { item, index }) if col < detail_actions(item).len() => {
                *index = col;
                true
            }
            _ => false,
        }
    }

    // ---------------------------------------------------------- results

    pub fn on_discovered(&mut self, servers: Vec<Server>) {
        self.on_discovered_for(Backend::Jellyfin, servers);
    }

    /// Servers found for `kind` (ignored when the switch moved on).
    pub fn on_discovered_for(&mut self, kind: Backend, servers: Vec<Server>) {
        for s in &mut self.stack {
            if let Screen::Connect {
                backend,
                servers: sc,
                index,
            } = s
                && *backend == kind
            {
                *sc = Scan::Done(servers.clone());
                // Focus stays where it is (on the switch too), within the list.
                *index = (*index).min(connect_rows(kind, sc));
            }
        }
    }

    /// Sign-in finished. On success the caller has stored the account;
    /// Catfyn goes to Browse.
    pub fn on_login(&mut self, result: Result<(), String>) -> Vec<FinEffect> {
        match result {
            Ok(()) => {
                self.stack.clear();
                self.stack.push(Screen::Browse);
                self.home = Scan::Busy;
                self.row = 0;
                self.cols.clear();
                self.header = false;
                self.error = None;
                vec![FinEffect::LoadHome]
            }
            Err(e) => {
                match self.stack.last_mut() {
                    Some(Screen::SignIn { busy, osk, .. }) => {
                        *busy = false;
                        osk.clear();
                    }
                    Some(Screen::PlexServers { busy, .. }) => *busy = false,
                    _ => {}
                }
                self.error = Some(e);
                vec![]
            }
        }
    }

    /// The code for plex.tv/link (or why there is none).
    pub fn on_plex_pin(&mut self, result: Result<String, String>) {
        if let Some(Screen::PlexLink { code }) = self.stack.last_mut() {
            *code = match result {
                Ok(c) => Scan::Done(vec![c]),
                Err(e) => Scan::Failed(e),
            };
        }
    }

    /// The code was entered (or it expired / failed): the account's
    /// servers. One server is used straight away; several are listed.
    pub fn on_plex_linked(
        &mut self,
        result: Result<(String, String, String, Vec<PlexServer>), String>,
    ) -> Vec<FinEffect> {
        if !matches!(self.stack.last(), Some(Screen::PlexLink { .. })) {
            return vec![];
        }
        let (user_id, user_name, cloud_token, list) = match result {
            Ok(r) if r.3.is_empty() => {
                self.on_plex_pin(Err("This Plex account has no servers".into()));
                return vec![];
            }
            Ok(r) => r,
            Err(e) => {
                self.on_plex_pin(Err(e));
                return vec![];
            }
        };
        let one = list.len() == 1;
        self.stack.pop();
        self.stack.push(Screen::PlexServers {
            list: list.clone(),
            user_id: user_id.clone(),
            user_name: user_name.clone(),
            cloud_token: cloud_token.clone(),
            index: 0,
            busy: one,
        });
        if one {
            vec![FinEffect::PlexUse {
                server: list[0].clone(),
                user_id,
                user_name,
                cloud_token,
            }]
        } else {
            vec![]
        }
    }

    /// Browse's Jellyfin | Plex switch: the other kind's account (the first
    /// one shown on Home, else the first).
    fn switch_backend(&mut self, accounts: &Accounts) -> Vec<FinEffect> {
        let now = accounts.active().map_or(Backend::Jellyfin, |a| a.backend);
        let want = if now == Backend::Plex {
            Backend::Jellyfin
        } else {
            Backend::Plex
        };
        let pick = accounts
            .list
            .iter()
            .position(|a| a.backend == want && !a.incognito)
            .or_else(|| accounts.list.iter().position(|a| a.backend == want));
        let Some(i) = pick else {
            return vec![];
        };
        self.home = Scan::Busy;
        self.row = 0;
        self.cols.clear();
        vec![FinEffect::SwitchAccount(i), FinEffect::LoadHome]
    }

    pub fn on_home(&mut self, result: Result<HomeData, String>) {
        match result {
            Ok(d) => {
                self.home = Scan::Done(vec![d]);
                let n = self.rows().len();
                self.row = self.row.min(n.saturating_sub(1));
                self.header = n == 0;
                self.error = None;
            }
            Err(e) => {
                self.home = Scan::Failed(e.clone());
                // Focus stays on the page: OK tries again, Up reaches
                // the tabs and settings.
                self.header = false;
                self.error = Some(e);
            }
        }
    }

    pub fn on_library(&mut self, id: &str, result: Result<Vec<Item>, String>) {
        for s in &mut self.stack {
            if let Screen::Library { lib, items, .. } = s
                && lib.id == id
            {
                *items = match &result {
                    Ok(v) => Scan::Done(v.clone()),
                    Err(e) => Scan::Failed(e.clone()),
                };
            }
        }
    }

    pub fn on_seasons(
        &mut self,
        series_id: &str,
        result: Result<Vec<Item>, String>,
    ) -> Vec<FinEffect> {
        let mut eff = vec![];
        for s in &mut self.stack {
            if let Screen::Series {
                series,
                seasons,
                season,
                episodes,
                index,
                ..
            } = s
                && series.id == series_id
            {
                *seasons = match &result {
                    Ok(v) => Scan::Done(v.clone()),
                    Err(e) => Scan::Failed(e.clone()),
                };
                // The first season not fully watched.
                *season = seasons.items().iter().position(|x| !x.played).unwrap_or(0);
                eff = load_season(series, seasons, *season, episodes, index);
            }
        }
        eff
    }

    pub fn on_episodes(&mut self, season_id: &str, result: Result<Vec<Item>, String>) {
        for s in &mut self.stack {
            if let Screen::Series {
                seasons,
                season,
                episodes,
                index,
                ..
            } = s
                && seasons
                    .items()
                    .get(*season)
                    .is_some_and(|x| x.id == season_id)
            {
                *episodes = match &result {
                    Ok(v) => Scan::Done(v.clone()),
                    Err(e) => Scan::Failed(e.clone()),
                };
                // Focus the first episode not watched yet.
                *index = episodes.items().iter().position(|x| !x.played).unwrap_or(0);
            }
        }
    }
}

/// "Add a server", on Jellyfin (looking for servers), the first entry
/// focused.
fn connect_screen() -> Screen {
    Screen::Connect {
        backend: Backend::Jellyfin,
        servers: Scan::Busy,
        index: 1,
    }
}

/// Entries under the Jellyfin | Plex switch.
fn connect_rows(backend: Backend, servers: &Scan<Server>) -> usize {
    match backend {
        Backend::Jellyfin => servers.items().len() + 1,
        Backend::Plex => servers.items().len() + 2,
    }
}

fn address_screen(backend: Backend) -> Screen {
    Screen::Address {
        backend,
        osk: Box::new(Osk::new(false, "Next")),
    }
}

/// Browse shows a Jellyfin | Plex switch when both have an account.
pub fn header_switch(accounts: &Accounts) -> bool {
    accounts.backends().len() > 1
}

fn sign_in(server: Server) -> Screen {
    Screen::SignIn {
        server,
        step: SignInStep::User,
        user: String::new(),
        osk: Box::new(Osk::new(false, "Next")),
        busy: false,
    }
}

fn load_season(
    series: &Item,
    seasons: &Scan<Item>,
    season: usize,
    episodes: &mut Scan<Item>,
    index: &mut usize,
) -> Vec<FinEffect> {
    *index = 0;
    match seasons.items().get(season) {
        Some(s) => {
            *episodes = Scan::Busy;
            vec![FinEffect::LoadEpisodes {
                series: series.id.clone(),
                season: s.id.clone(),
            }]
        }
        None => {
            *episodes = Scan::Done(Vec::new());
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action::*;

    fn item(id: &str, kind: &str) -> Item {
        Item {
            id: id.into(),
            name: id.into(),
            kind_raw: kind.into(),
            ..Item::default()
        }
    }

    fn signed_in() -> Accounts {
        let mut a = Accounts::default();
        a.add(Account {
            server: "http://s:8096".into(),
            user_id: "u".into(),
            token: "t".into(),
            ..Account::default()
        });
        a
    }

    fn type_text(f: &mut Fin, a: &Accounts, text: &str) -> Vec<FinEffect> {
        let mut out = vec![];
        for c in text.chars() {
            out.extend(f.handle(Char(c), a));
        }
        out
    }

    /// Press the OSK's Done key: Enter on a physical keyboard submits.
    fn submit(f: &mut Fin, a: &Accounts) -> Vec<FinEffect> {
        // Move focus to the Done key by typing then pressing OK on it:
        // the OSK treats Char('\n') as nothing, so use its API directly.
        if let Some(Screen::Address { osk, .. } | Screen::SignIn { osk, .. }) = f.stack.last_mut() {
            let rows = osk.rows();
            let r = rows.len() - 1;
            let c = rows[r]
                .iter()
                .position(|k| matches!(k.kind, crate::osk::KeyKind::Done))
                .unwrap();
            osk.set_focus(r, c);
        }
        f.handle(Ok, a)
    }

    #[test]
    fn first_open_connects_and_signs_in() {
        let none = Accounts::default();
        let (mut f, eff) = Fin::open(&none);
        assert_eq!(eff, vec![FinEffect::Discover]);
        f.on_discovered(vec![Server {
            id: "1".into(),
            name: "Den".into(),
            address: "http://10.0.0.5:8096".into(),
        }]);
        f.handle(Ok, &none);
        assert!(f.text_mode());
        type_text(&mut f, &none, "ok");
        assert!(submit(&mut f, &none).is_empty());
        type_text(&mut f, &none, "pw");
        let eff = submit(&mut f, &none);
        assert!(
            matches!(&eff[0], FinEffect::Login { user, password, .. } if user == "ok" && password == "pw")
        );
        // Wrong password: stay, show the error, keyboard cleared.
        assert!(
            f.on_login(Err("Wrong user name or password".into()))
                .is_empty()
        );
        assert!(f.error.is_some());
        assert_eq!(f.on_login(Result::Ok(())), vec![FinEffect::LoadHome]);
        assert!(matches!(f.screen(), Some(Screen::Browse)));
    }

    #[test]
    fn manual_address() {
        let none = Accounts::default();
        let (mut f, _) = Fin::open(&none);
        f.on_discovered(vec![]);
        f.handle(Ok, &none); // "Enter address"
        type_text(&mut f, &none, "nas");
        submit(&mut f, &none);
        assert!(
            matches!(f.screen(), Some(Screen::SignIn { server, .. }) if server.address == "http://nas:8096")
        );
    }

    #[test]
    fn plex_sign_in_with_a_code() {
        let none = Accounts::default();
        let (mut f, _) = Fin::open(&none);
        f.on_discovered(vec![]);
        // Up to the switch, over to Plex, down to "Sign in with Plex".
        f.handle(Up, &none);
        assert_eq!(f.handle(Right, &none), vec![FinEffect::DiscoverPlex]);
        assert!(matches!(
            f.screen(),
            Some(Screen::Connect {
                backend: Backend::Plex,
                ..
            })
        ));
        // A server found on the network is offered under "Sign in".
        f.on_discovered_for(
            Backend::Plex,
            vec![Server {
                id: "m1".into(),
                name: "Den".into(),
                address: "http://10.0.0.9:32400".into(),
            }],
        );
        // Jellyfin's late answer does not land on the Plex side.
        f.on_discovered(vec![]);
        f.handle(Down, &none);
        f.handle(Down, &none);
        assert_eq!(
            f.handle(Ok, &none),
            vec![FinEffect::PlexAddress("http://10.0.0.9:32400".into())]
        );
        f.handle(Up, &none);
        assert_eq!(f.handle(Ok, &none), vec![FinEffect::PlexPin]);
        f.on_plex_pin(Result::Ok("A1B2".into()));
        assert!(
            matches!(f.screen(), Some(Screen::PlexLink { code: Scan::Done(c) }) if c[0] == "A1B2")
        );
        let server = |name: &str| PlexServer {
            name: name.into(),
            id: "s1".into(),
            token: "t".into(),
            addresses: vec!["http://10.0.0.9:32400".into()],
            owned: true,
        };
        // Two servers: pick one.
        let eff = f.on_plex_linked(Result::Ok((
            "u".into(),
            "kat".into(),
            "cloud".into(),
            vec![server("Den"), server("Office")],
        )));
        assert!(eff.is_empty());
        f.handle(Down, &none);
        assert!(matches!(
            &f.handle(Ok, &none)[0],
            FinEffect::PlexUse { server, user_name, .. } if server.name == "Office" && user_name == "kat"
        ));
        // Can't reach it: the list again, with the error.
        f.on_login(Err("Can't reach Office".into()));
        assert!(matches!(
            f.screen(),
            Some(Screen::PlexServers { busy: false, .. })
        ));
        assert_eq!(f.on_login(Result::Ok(())), vec![FinEffect::LoadHome]);
    }

    #[test]
    fn plex_code_backs_out_and_retries() {
        let none = Accounts::default();
        let (mut f, _) = Fin::open(&none);
        f.handle(Up, &none);
        f.handle(Ok, &none); // the switch: Plex
        f.handle(Down, &none);
        f.handle(Ok, &none);
        // One server is used straight away.
        f.on_plex_pin(Result::Ok("ZZZZ".into()));
        f.on_plex_pin(Err("The code expired".into()));
        assert_eq!(
            f.handle(Ok, &none),
            vec![FinEffect::PlexStop, FinEffect::PlexPin]
        );
        let back = f.handle(Back, &none);
        assert_eq!(back[0], FinEffect::PlexStop);
        assert!(matches!(f.screen(), Some(Screen::Connect { .. })));
        // A Plex address goes straight to the server (no user name).
        f.handle(Down, &none);
        f.handle(Ok, &none);
        type_text(&mut f, &none, "10.0.0.9");
        assert_eq!(
            submit(&mut f, &none),
            vec![FinEffect::PlexAddress("http://10.0.0.9:32400".into())]
        );
    }

    #[test]
    fn switch_between_jellyfin_and_plex() {
        let mut a = signed_in();
        a.add(crate::jellyfin::Account {
            backend: Backend::Plex,
            server: "http://10.0.0.9:32400".into(),
            user_id: "p1".into(),
            ..Default::default()
        });
        a.active = 0;
        assert!(header_switch(&a));
        let (mut f, _) = Fin::open(&a);
        f.on_home(Result::Ok(HomeData::default()));
        // No rows: the header has focus; past the (no) library tabs is
        // the switch, then the account button.
        assert!(f.header);
        assert_eq!(f.header_index, 0);
        assert_eq!(
            f.handle(Ok, &a),
            vec![FinEffect::SwitchAccount(1), FinEffect::LoadHome]
        );
        f.handle(Right, &a);
        f.handle(Ok, &a);
        assert!(matches!(f.screen(), Some(Screen::Settings { .. })));
    }

    #[test]
    fn home_rows_from_two_servers_take_turns() {
        let row = |title: &str, ids: &[&str]| Row {
            title: title.into(),
            items: ids.iter().map(|i| item(i, "Movie")).collect(),
            libraries: false,
        };
        let merged = merge_rows(vec![
            vec![
                row("Continue watching", &["j1", "j2", "j3"]),
                row("Movies", &["jm"]),
            ],
            vec![row("Movies", &["pm1", "pm2"]), row("TV shows", &["ps"])],
        ]);
        let ids = |r: &Row| r.items.iter().map(|i| i.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            merged.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
            ["Continue watching", "Movies", "TV shows"]
        );
        assert_eq!(ids(&merged[0]), ["j1", "j2", "j3"]);
        assert_eq!(ids(&merged[1]), ["jm", "pm1", "pm2"]);
    }

    #[test]
    fn browse_rows_play_and_open() {
        let a = signed_in();
        let (mut f, eff) = Fin::open(&a);
        assert_eq!(eff, vec![FinEffect::LoadHome]);
        let mut ep = item("e1", "Episode");
        ep.position = 100.0;
        f.on_home(Result::Ok(HomeData {
            resume: vec![ep.clone()],
            next_up: vec![],
            libraries: vec![item("lib", "CollectionFolder")],
            latest: vec![("Movies".into(), vec![item("m1", "Movie")])],
        }));
        assert_eq!(f.rows().len(), 3);
        assert_eq!(
            f.handle(Ok, &a),
            vec![FinEffect::Play {
                item: Box::new(ep),
                from_start: false
            }]
        );
        // Libraries row → library grid.
        f.handle(Down, &a);
        assert_eq!(f.handle(Ok, &a), vec![FinEffect::LoadLibrary("lib".into())]);
        f.on_library(
            "lib",
            Result::Ok(vec![item("s1", "Series"), item("m2", "Movie")]),
        );
        assert_eq!(f.handle(Ok, &a), vec![FinEffect::LoadSeasons("s1".into())]);
        let mut s1 = item("se1", "Season");
        s1.played = true;
        let eff = f.on_seasons("s1", Result::Ok(vec![s1, item("se2", "Season")]));
        assert_eq!(
            eff,
            vec![FinEffect::LoadEpisodes {
                series: "s1".into(),
                season: "se2".into()
            }]
        );
        f.on_episodes("se2", Result::Ok(vec![item("x1", "Episode")]));
        f.handle(Down, &a);
        assert!(matches!(&f.handle(Ok, &a)[0], FinEffect::Play { item, .. } if item.id == "x1"));
        // Back ×3: Series → Library → Browse → close.
        f.handle(Back, &a);
        f.handle(Back, &a);
        assert!(matches!(f.screen(), Some(Screen::Browse)));
        assert_eq!(f.handle(Back, &a), vec![FinEffect::Close]);
    }

    #[test]
    fn details_actions() {
        let mut m = item("m", "Movie");
        assert_eq!(detail_actions(&m), vec!["Play"]);
        m.position = 600.0;
        assert_eq!(detail_actions(&m), vec!["Resume", "Play from start"]);
        let mut e = item("e", "Episode");
        e.series_id = "s".into();
        assert_eq!(detail_actions(&e), vec!["Play", "Go to series"]);
    }

    #[test]
    fn accounts_menu() {
        let mut a = signed_in();
        a.add(Account {
            server: "http://s:8096".into(),
            user_id: "u2".into(),
            token: "t2".into(),
            ..Account::default()
        });
        let (mut f, _) = Fin::open(&a);
        f.on_home(Result::Ok(HomeData::default()));
        assert!(f.header, "empty home focuses the account button");
        f.handle(Ok, &a);
        assert!(matches!(
            f.screen(),
            Some(Screen::Settings { index: 1, .. })
        ));
        f.handle(Ok, &a); // menu of account 1
        assert_eq!(
            f.handle(Down, &a)
                .into_iter()
                .chain(f.handle(Ok, &a))
                .collect::<Vec<_>>(),
            vec![FinEffect::SetIncognito(1, true)]
        );
        f.handle(Up, &a);
        f.handle(Ok, &a);
        assert_eq!(
            f.handle(Ok, &a),
            vec![FinEffect::SwitchAccount(0), FinEffect::LoadHome]
        );
        assert!(matches!(f.screen(), Some(Screen::Browse)));
        // After the accounts (the screen opens on the active one, 1):
        // Add account, then playback settings.
        f.header = true;
        f.handle(Ok, &a);
        f.handle(Down, &a);
        assert_eq!(f.handle(Down, &a), vec![]);
        assert_eq!(f.handle(Ok, &a), vec![FinEffect::ToggleAutoplay]);
        f.handle(Down, &a);
        assert_eq!(f.handle(Left, &a), vec![FinEffect::CountdownStep(-1)]);
        assert_eq!(f.handle(Right, &a), vec![FinEffect::CountdownStep(1)]);
        assert_eq!(f.handle(Down, &a), vec![], "Countdown is the last row");
        f.handle(Up, &a);
        f.handle(Up, &a);
        assert_eq!(f.handle(Ok, &a), vec![FinEffect::Discover]);
    }

    #[test]
    fn failed_home_retries_on_ok() {
        let a = signed_in();
        let (mut f, _) = Fin::open(&a);
        f.on_home(Err("Can't reach the server".into()));
        assert!(!f.header);
        assert_eq!(f.handle(Ok, &a), vec![FinEffect::LoadHome]);
        assert!(matches!(f.home, Scan::Busy));
    }

    #[test]
    fn home_continue_items_merge() {
        let d = HomeData {
            resume: vec![item("a", "Movie")],
            next_up: vec![item("a", "Movie"), item("b", "Episode")],
            ..Default::default()
        };
        let ids: Vec<String> = d.continue_items().into_iter().map(|i| i.id).collect();
        assert_eq!(ids, ["a", "b"]);
        // One entry per series: the most recent episode stays, the other
        // half-watched ones of the same show go; Next up's episode of that
        // show is a duplicate too.
        let ep = |id: &str, series: &str| {
            let mut e = item(id, "Episode");
            e.series_id = series.into();
            e
        };
        let d = HomeData {
            resume: vec![
                ep("s1e3", "show1"),
                item("film", "Movie"),
                ep("s1e2", "show1"),
                ep("x2", "show2"),
            ],
            next_up: vec![ep("s1e4", "show1"), ep("y1", "show3")],
            ..Default::default()
        };
        let ids: Vec<String> = d.continue_items().into_iter().map(|i| i.id).collect();
        assert_eq!(ids, ["s1e3", "film", "x2", "y1"]);
        let ids: Vec<String> = d.rows()[0].items.iter().map(|i| i.id.clone()).collect();
        assert_eq!(ids, ["s1e3", "film", "x2"]);
        let mut lib = item("l1", "CollectionFolder");
        lib.name = "Films".into();
        lib.collection = "movies".into();
        let d = HomeData {
            libraries: vec![lib],
            latest: vec![("Films".into(), vec![item("m", "Movie")])],
            ..Default::default()
        };
        let rows = d.home_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Movies");
        assert!(HomeData::default().home_rows().is_empty());
    }
}
