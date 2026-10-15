//! Files: CatosTV's own file browser. Two
//! panes like any file manager, places on the left (Home folders and
//! drives), the folder's contents on the right, with the usual jobs:
//! open, rename, new folder, copy, cut, paste, delete. State and listing
//! helpers only; the shell binary runs listings and file operations on a
//! job thread and opens / installs what the user picks.

use crate::input::Action;
use crate::installer::Scan;
use crate::osk::{Osk, OskEvent};
use std::path::{Path, PathBuf};

/// Command of the built-in Files app (`apps.toml`).
pub const FILES_COMMAND: &str = "catostv:files";

/// Entries listed per folder at most (a camera card can hold thousands).
pub const LIST_LIMIT: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Folder,
    Drive,
    Video,
    Audio,
    Image,
    Apk,
    Archive,
    Text,
    Other,
}

impl Kind {
    /// Icon name (Papirus mimetypes/places/devices; the shell resolves it
    /// to a file, with a drawn fallback).
    pub fn icon(self) -> &'static str {
        match self {
            Kind::Folder => "folder",
            Kind::Drive => "drive-removable-media",
            Kind::Video => "video-x-generic",
            Kind::Audio => "audio-x-generic",
            Kind::Image => "image-x-generic",
            Kind::Apk => "android-package-archive",
            Kind::Archive => "package-x-generic",
            Kind::Text => "text-x-generic",
            Kind::Other => "unknown",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Folder => "Folder",
            Kind::Drive => "Drive",
            Kind::Video => "Video",
            Kind::Audio => "Music",
            Kind::Image => "Picture",
            Kind::Apk => "Android app",
            Kind::Archive => "Archive",
            Kind::Text => "Document",
            Kind::Other => "File",
        }
    }

    /// mpv plays it (pictures are shown as a still).
    pub fn playable(self) -> bool {
        matches!(self, Kind::Video | Kind::Audio | Kind::Image)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: Kind,
    /// Bytes; 0 for folders and drives.
    pub size: u64,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, Kind::Folder | Kind::Drive)
    }
}

pub fn kind_of(path: &Path) -> Kind {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "mkv" | "mp4" | "m4v" | "webm" | "avi" | "mov" | "ts" | "m2ts" | "mts" | "wmv" | "flv"
        | "mpg" | "mpeg" | "vob" | "3gp" | "ogv" => Kind::Video,
        "mp3" | "flac" | "m4a" | "aac" | "ogg" | "oga" | "opus" | "wav" | "wma" | "aiff"
        | "ape" | "mka" => Kind::Audio,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "heic" | "avif" | "tif" | "tiff"
        | "jxl" => Kind::Image,
        "apk" => Kind::Apk,
        "zip" | "7z" | "rar" | "tar" | "gz" | "xz" | "zst" | "bz2" | "iso" => Kind::Archive,
        "txt" | "md" | "pdf" | "srt" | "ass" | "nfo" | "log" | "json" | "toml" | "conf" => {
            Kind::Text
        }
        _ => Kind::Other,
    }
}

/// "1.4 GB", "812 MB", "12 KB".
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// The places pane: the usual Home folders that exist, Home itself, and
/// every mounted drive under `/run/media/<user>`, `/media` and `/mnt`.
pub fn places(home: &Path) -> Vec<Entry> {
    let mut v = Vec::new();
    let folder = |name: &str, p: &Path, kind: Kind| Entry {
        name: name.to_string(),
        path: p.to_string_lossy().into_owned(),
        kind,
        size: 0,
    };
    for name in ["Downloads", "Videos", "Music", "Pictures", "Documents"] {
        let p = home.join(name);
        if p.is_dir() {
            v.push(folder(name, &p, Kind::Folder));
        }
    }
    v.push(folder("Home", home, Kind::Folder));
    let user = home
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for base in [
        PathBuf::from("/run/media").join(&user),
        PathBuf::from("/media"),
        PathBuf::from("/mnt"),
    ] {
        for d in list_dir(&base).into_iter().filter(|e| e.is_dir()) {
            v.push(Entry {
                kind: Kind::Drive,
                ..d
            });
        }
    }
    v
}

/// Folders first, then files, each sorted case-insensitively; hidden and
/// Windows system entries skipped; symlinks not followed; at most
/// [`LIST_LIMIT`] rows.
pub fn list_dir(dir: &Path) -> Vec<Entry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<Entry> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.starts_with('$') || name == "System Volume Information"
            {
                return None;
            }
            let meta = std::fs::symlink_metadata(e.path()).ok()?;
            let path = e.path();
            let (kind, size) = if meta.is_dir() {
                (Kind::Folder, 0)
            } else if meta.is_file() {
                (kind_of(&path), meta.len())
            } else {
                return None;
            };
            Some(Entry {
                name,
                path: path.to_string_lossy().into_owned(),
                kind,
                size,
            })
        })
        .collect();
    v.sort_by(|a, b| {
        b.is_dir()
            .cmp(&a.is_dir())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    v.truncate(LIST_LIMIT);
    v
}

/// May the TV change `path` (delete, rename, move into)? Only inside Home
/// or a mounted drive, never the place itself, never system files.
pub fn writable(home: &Path, path: &Path) -> bool {
    let inside = |base: &Path| path.starts_with(base) && path != base;
    inside(home)
        || inside(Path::new("/run/media"))
        || inside(Path::new("/media"))
        || inside(Path::new("/mnt"))
}

/// A name typed for rename / new folder: no slashes, no control
/// characters, not "." or "..", not hidden, at most 200 bytes.
pub fn valid_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && n.len() <= 200
        && n != "."
        && n != ".."
        && !n.starts_with('.')
        && !n.contains('/')
        && !n.chars().any(|c| c.is_control())
}

/// What the shell binary must do for the Files screen. Every file
/// operation is answered with a fresh listing of the folder shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesEffect {
    /// List this folder (reply with `Files::on_listed`).
    List(String),
    /// Play / show in mpv.
    Play(Entry),
    /// Install an APK through Android.
    InstallApk(String),
    /// Delete this file or folder (with everything in it).
    Delete(String),
    /// Rename `path` to `new_name` inside its folder.
    Rename { path: String, new_name: String },
    /// Create a folder named `name` inside `dir`.
    NewFolder { dir: String, name: String },
    /// Copy (`cut == false`) or move `source` into `dir`.
    Paste {
        source: String,
        dir: String,
        cut: bool,
    },
    /// Nothing opens this on the TV.
    CannotOpen(Entry),
}

/// Row options when pressing Options / long OK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOption {
    Open,
    Rename,
    Copy,
    Cut,
    Delete,
    NewFolder,
    Paste,
}

impl FileOption {
    pub fn label(self) -> &'static str {
        match self {
            FileOption::Open => "Open",
            FileOption::Rename => "Rename",
            FileOption::Copy => "Copy",
            FileOption::Cut => "Cut",
            FileOption::Delete => "Delete",
            FileOption::NewFolder => "New folder",
            FileOption::Paste => "Paste",
        }
    }
}

/// Which pane has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Places,
    List,
}

/// What is being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditKind {
    Rename(Entry),
    NewFolder,
}

#[derive(Debug, Clone)]
pub struct Edit {
    pub kind: EditKind,
    pub osk: Osk,
}

#[derive(Debug, Clone)]
pub enum FilesMenu {
    /// Options for the focused entry (or the folder); index into
    /// `Files::options()`.
    Options(usize),
    /// "Delete <name>?", 0 = Delete, 1 = Keep.
    ConfirmDelete(usize),
    /// Typing a name.
    Edit(Edit),
}

/// One folder on the stack under the current place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Level {
    pub entry: Entry,
    pub index: usize,
}

/// What Copy / Cut remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clipboard {
    pub entry: Entry,
    pub cut: bool,
}

#[derive(Debug, Clone)]
pub struct Files {
    pub places: Vec<Entry>,
    /// Focused / shown place.
    pub place: usize,
    pub pane: Pane,
    /// Folders entered under the place (the place itself is not on it).
    pub stack: Vec<Level>,
    /// Focused row of the place's own listing.
    pub place_index: usize,
    /// The folder's entries (`Busy` while loading).
    pub entries: Scan<Entry>,
    pub menu: Option<FilesMenu>,
    pub clipboard: Option<Clipboard>,
}

impl Files {
    /// Open on the first place (its listing is requested).
    pub fn open(places: Vec<Entry>) -> (Self, Vec<FilesEffect>) {
        let f = Self {
            places,
            place: 0,
            pane: Pane::List,
            stack: Vec::new(),
            place_index: 0,
            entries: Scan::Busy,
            menu: None,
            clipboard: None,
        };
        let eff = f.current_dir().map(|d| FilesEffect::List(d.path.clone()));
        (f, eff.into_iter().collect())
    }

    /// The folder being shown: the deepest entered folder, else the place.
    pub fn current_dir(&self) -> Option<&Entry> {
        self.stack
            .last()
            .map(|l| &l.entry)
            .or_else(|| self.places.get(self.place))
    }

    pub fn rows(&self) -> &[Entry] {
        self.entries.items()
    }

    pub fn index(&self) -> usize {
        self.stack.last().map_or(self.place_index, |l| l.index)
    }

    fn set_index(&mut self, i: usize) {
        match self.stack.last_mut() {
            Some(l) => l.index = i,
            None => self.place_index = i,
        }
    }

    pub fn focused(&self) -> Option<&Entry> {
        self.rows().get(self.index())
    }

    /// Breadcrumb: "Downloads › 2026".
    pub fn title(&self) -> String {
        let mut t = self
            .places
            .get(self.place)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Files".into());
        for l in &self.stack {
            t.push_str(" › ");
            t.push_str(&l.entry.name);
        }
        t
    }

    /// True while a name is being typed (letters edit, not navigate).
    pub fn text_mode(&self) -> bool {
        matches!(self.menu, Some(FilesMenu::Edit(_)))
    }

    /// The options for the focused entry (entry options first, then the
    /// folder's own: New folder, Paste when something was copied).
    pub fn options(&self) -> Vec<FileOption> {
        let mut v = Vec::new();
        if self.focused().is_some() {
            v.extend([
                FileOption::Open,
                FileOption::Rename,
                FileOption::Copy,
                FileOption::Cut,
                FileOption::Delete,
            ]);
        }
        v.push(FileOption::NewFolder);
        if self.clipboard.is_some() {
            v.push(FileOption::Paste);
        }
        v
    }

    fn enter(&mut self, e: Entry) -> FilesEffect {
        self.stack.push(Level {
            entry: e.clone(),
            index: 0,
        });
        self.entries = Scan::Busy;
        FilesEffect::List(e.path)
    }

    fn open_entry(&mut self, e: Entry) -> Vec<FilesEffect> {
        if e.is_dir() {
            return vec![self.enter(e)];
        }
        vec![match e.kind {
            Kind::Video | Kind::Audio | Kind::Image => FilesEffect::Play(e),
            Kind::Apk => FilesEffect::InstallApk(e.path),
            _ => FilesEffect::CannotOpen(e),
        }]
    }

    fn show_place(&mut self, i: usize) -> Vec<FilesEffect> {
        self.place = i;
        self.stack.clear();
        self.place_index = 0;
        self.entries = Scan::Busy;
        self.places
            .get(i)
            .map(|p| FilesEffect::List(p.path.clone()))
            .into_iter()
            .collect()
    }

    /// `true` = the user left the Files app (Back in the places pane).
    pub fn handle(&mut self, action: Action) -> (Vec<FilesEffect>, bool) {
        if let Some(menu) = self.menu.clone() {
            return (self.handle_menu(menu, action), false);
        }
        let mut effects = Vec::new();
        match self.pane {
            Pane::Places => match action {
                Action::Up | Action::Down => {
                    let n = self.places.len();
                    let i = if action == Action::Up {
                        self.place.saturating_sub(1)
                    } else {
                        (self.place + 1).min(n.saturating_sub(1))
                    };
                    if i != self.place {
                        // The listing follows focus, like a TV sidebar.
                        effects.extend(self.show_place(i));
                    }
                }
                Action::Right | Action::Ok | Action::OkLong => self.pane = Pane::List,
                Action::Back => return (effects, true),
                _ => {}
            },
            Pane::List => {
                let n = self.rows().len();
                match action {
                    Action::Up => {
                        let i = self.index().saturating_sub(1);
                        self.set_index(i);
                    }
                    Action::Down => {
                        let i = (self.index() + 1).min(n.saturating_sub(1));
                        self.set_index(i);
                    }
                    Action::Left => self.pane = Pane::Places,
                    Action::Right => {
                        let i = (self.index() + 8).min(n.saturating_sub(1));
                        self.set_index(i);
                    }
                    Action::Ok => {
                        if let Some(e) = self.focused().cloned() {
                            effects.extend(self.open_entry(e));
                        }
                    }
                    Action::OkLong | Action::Options => {
                        self.menu = Some(FilesMenu::Options(0));
                    }
                    Action::Back => {
                        if self.stack.pop().is_none() {
                            self.pane = Pane::Places;
                        } else {
                            self.entries = Scan::Busy;
                            if let Some(d) = self.current_dir().cloned() {
                                effects.push(FilesEffect::List(d.path));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        (effects, false)
    }

    fn handle_menu(&mut self, menu: FilesMenu, action: Action) -> Vec<FilesEffect> {
        let mut effects = Vec::new();
        match menu {
            FilesMenu::Options(i) => {
                let opts = self.options();
                let n = opts.len().max(1);
                match action {
                    Action::Up => self.menu = Some(FilesMenu::Options(i.saturating_sub(1))),
                    Action::Down => self.menu = Some(FilesMenu::Options((i + 1).min(n - 1))),
                    Action::Back | Action::Options => self.menu = None,
                    Action::Ok | Action::OkLong => {
                        let Some(opt) = opts.get(i.min(n - 1)).copied() else {
                            self.menu = None;
                            return effects;
                        };
                        match opt {
                            FileOption::Open => {
                                self.menu = None;
                                if let Some(e) = self.focused().cloned() {
                                    effects.extend(self.open_entry(e));
                                }
                            }
                            FileOption::Rename => {
                                if let Some(e) = self.focused().cloned() {
                                    let mut osk = Osk::new(false, "Rename");
                                    osk.set_text(&e.name);
                                    self.menu = Some(FilesMenu::Edit(Edit {
                                        kind: EditKind::Rename(e),
                                        osk,
                                    }));
                                }
                            }
                            FileOption::Copy | FileOption::Cut => {
                                self.clipboard = self.focused().cloned().map(|entry| Clipboard {
                                    entry,
                                    cut: opt == FileOption::Cut,
                                });
                                self.menu = None;
                            }
                            FileOption::Delete => self.menu = Some(FilesMenu::ConfirmDelete(1)),
                            FileOption::NewFolder => {
                                self.menu = Some(FilesMenu::Edit(Edit {
                                    kind: EditKind::NewFolder,
                                    osk: Osk::new(false, "Create"),
                                }));
                            }
                            FileOption::Paste => {
                                self.menu = None;
                                if let (Some(cb), Some(dir)) =
                                    (self.clipboard.take(), self.current_dir().cloned())
                                {
                                    effects.push(FilesEffect::Paste {
                                        source: cb.entry.path,
                                        dir: dir.path,
                                        cut: cb.cut,
                                    });
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            FilesMenu::ConfirmDelete(i) => match action {
                Action::Left | Action::Up => self.menu = Some(FilesMenu::ConfirmDelete(0)),
                Action::Right | Action::Down => self.menu = Some(FilesMenu::ConfirmDelete(1)),
                Action::Back => self.menu = None,
                Action::Ok | Action::OkLong => {
                    self.menu = None;
                    if i == 0
                        && let Some(e) = self.focused().cloned()
                    {
                        effects.push(FilesEffect::Delete(e.path));
                    }
                }
                _ => {}
            },
            FilesMenu::Edit(mut edit) => match edit.osk.handle(action) {
                OskEvent::Submitted => {
                    let name = edit.osk.text().trim().to_string();
                    self.menu = None;
                    if !valid_name(&name) {
                        return effects;
                    }
                    match (&edit.kind, self.current_dir().cloned()) {
                        (EditKind::Rename(e), _) if name != e.name => {
                            effects.push(FilesEffect::Rename {
                                path: e.path.clone(),
                                new_name: name,
                            });
                        }
                        (EditKind::NewFolder, Some(dir)) => {
                            effects.push(FilesEffect::NewFolder {
                                dir: dir.path,
                                name,
                            });
                        }
                        _ => {}
                    }
                }
                OskEvent::Unhandled if action == Action::Back => self.menu = None,
                _ => self.menu = Some(FilesMenu::Edit(edit)),
            },
        }
        effects
    }

    /// A listing arrived (ignored unless it is for the folder shown).
    pub fn on_listed(&mut self, dir: &str, entries: Result<Vec<Entry>, String>) {
        if self.current_dir().is_some_and(|d| d.path == dir) {
            match entries {
                Ok(v) => {
                    let i = self.index().min(v.len().saturating_sub(1));
                    self.set_index(i);
                    self.entries = Scan::Done(v);
                }
                Err(e) => self.entries = Scan::Failed(e),
            }
        }
    }

    /// Fresh places (drives mounted meanwhile).
    pub fn on_places(&mut self, places: Vec<Entry>) {
        if !places.is_empty() {
            self.place = self.place.min(places.len() - 1);
            self.places = places;
        }
    }

    /// Pointer / click on a list row.
    pub fn focus_row(&mut self, i: usize) {
        if i < self.rows().len() {
            self.pane = Pane::List;
            self.set_index(i);
        }
    }

    /// Pointer / click on a place.
    pub fn focus_place(&mut self, i: usize) -> Vec<FilesEffect> {
        if i >= self.places.len() {
            return vec![];
        }
        self.pane = Pane::Places;
        if i == self.place {
            return vec![];
        }
        self.show_place(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: Kind) -> Entry {
        Entry {
            name: name.into(),
            path: format!("/home/catostv/{name}"),
            kind,
            size: 1500,
        }
    }

    #[test]
    fn kinds_sizes_and_names() {
        assert_eq!(kind_of(Path::new("a/B.MKV")), Kind::Video);
        assert_eq!(kind_of(Path::new("x.flac")), Kind::Audio);
        assert_eq!(kind_of(Path::new("x.apk")), Kind::Apk);
        assert_eq!(kind_of(Path::new("x")), Kind::Other);
        assert_eq!(human_size(900), "900 B");
        assert_eq!(human_size(1_500), "1.5 KB");
        assert_eq!(human_size(812_000_000), "812 MB");
        assert_eq!(human_size(1_400_000_000), "1.4 GB");
        assert!(valid_name("Holiday 2026"));
        assert!(
            !valid_name("a/b") && !valid_name("..") && !valid_name(".hidden") && !valid_name(" ")
        );
    }

    #[test]
    fn places_pane_and_browsing() {
        let (mut f, eff) = Files::open(vec![
            entry("Downloads", Kind::Folder),
            entry("USB", Kind::Drive),
        ]);
        assert_eq!(
            eff,
            vec![FilesEffect::List("/home/catostv/Downloads".into())]
        );
        assert_eq!(f.title(), "Downloads");
        f.on_listed(
            "/home/catostv/Downloads",
            Ok(vec![
                entry("film.mkv", Kind::Video),
                entry("notes.txt", Kind::Text),
            ]),
        );
        // OK plays a video; a document cannot be opened.
        assert_eq!(
            f.handle(Action::Ok).0,
            vec![FilesEffect::Play(entry("film.mkv", Kind::Video))]
        );
        f.handle(Action::Down);
        assert!(matches!(
            f.handle(Action::Ok).0.as_slice(),
            [FilesEffect::CannotOpen(e)] if e.name == "notes.txt"
        ));
        // Left → places; Down there switches the place and lists it.
        f.handle(Action::Left);
        assert_eq!(f.pane, Pane::Places);
        assert_eq!(
            f.handle(Action::Down).0,
            vec![FilesEffect::List("/home/catostv/USB".into())]
        );
        assert_eq!(f.title(), "USB");
        // Back in the places pane leaves the app.
        assert!(f.handle(Action::Back).1);
    }

    #[test]
    fn options_rename_copy_paste_delete() {
        let (mut f, _) = Files::open(vec![entry("Downloads", Kind::Folder)]);
        f.on_listed(
            "/home/catostv/Downloads",
            Ok(vec![
                entry("film.mkv", Kind::Video),
                entry("Old", Kind::Folder),
            ]),
        );
        // Options on a file: Open, Rename, Copy, Cut, Delete, New folder.
        f.handle(Action::Options);
        assert_eq!(
            f.options(),
            vec![
                FileOption::Open,
                FileOption::Rename,
                FileOption::Copy,
                FileOption::Cut,
                FileOption::Delete,
                FileOption::NewFolder
            ]
        );
        // Copy, then Paste appears.
        f.handle(Action::Down);
        f.handle(Action::Down);
        f.handle(Action::Ok);
        assert!(
            f.clipboard
                .as_ref()
                .is_some_and(|c| !c.cut && c.entry.name == "film.mkv")
        );
        f.handle(Action::Options);
        assert_eq!(f.options().last(), Some(&FileOption::Paste));
        for _ in 0..6 {
            f.handle(Action::Down);
        }
        assert_eq!(
            f.handle(Action::Ok).0,
            vec![FilesEffect::Paste {
                source: "/home/catostv/film.mkv".into(),
                dir: "/home/catostv/Downloads".into(),
                cut: false
            }]
        );
        assert!(f.clipboard.is_none());
        // Rename through the keyboard: the field starts with the name.
        f.handle(Action::Options);
        f.handle(Action::Down);
        f.handle(Action::Ok);
        let Some(FilesMenu::Edit(e)) = &f.menu else {
            panic!("rename editor");
        };
        assert_eq!(e.osk.text(), "film.mkv");
        assert!(f.text_mode());
        // Type on, then Done.
        let mut edit = match f.menu.take() {
            Some(FilesMenu::Edit(e)) => e,
            _ => unreachable!(),
        };
        edit.osk.set_text("film 2.mkv");
        let rows = edit.osk.rows();
        let r = rows.len() - 1;
        let c = rows[r]
            .iter()
            .position(|k| matches!(k.kind, crate::osk::KeyKind::Done))
            .unwrap();
        edit.osk.set_focus(r, c);
        f.menu = Some(FilesMenu::Edit(edit));
        assert_eq!(
            f.handle(Action::Ok).0,
            vec![FilesEffect::Rename {
                path: "/home/catostv/film.mkv".into(),
                new_name: "film 2.mkv".into()
            }]
        );
        // Delete asks; Keep is focused first.
        f.handle(Action::Options);
        for _ in 0..4 {
            f.handle(Action::Down);
        }
        f.handle(Action::Ok);
        assert!(matches!(f.menu, Some(FilesMenu::ConfirmDelete(1))));
        f.handle(Action::Left);
        assert_eq!(
            f.handle(Action::Ok).0,
            vec![FilesEffect::Delete("/home/catostv/film.mkv".into())]
        );
    }

    #[test]
    fn writable_only_in_user_places() {
        let home = Path::new("/home/catostv");
        assert!(!writable(home, Path::new("/etc/passwd")));
        assert!(!writable(home, home));
        assert!(writable(home, Path::new("/home/catostv/x.txt")));
        assert!(writable(home, Path::new("/run/media/catostv/USB/x")));
    }
}
