use super::*;
use crate::apps::{self, AppSource};
use crate::audio::{SinkKind, VolumeState};
use crate::bluetooth::BluetoothStatus;
use crate::display::DisplayPrefs;
use crate::prefs::UiPrefs;
use crate::settings::{AppView, SystemSnapshot};

struct Fixture {
    apps: Vec<AppView>,
    sys: SystemSnapshot,
    prefs: UiPrefs,
    display: DisplayPrefs,
    media: crate::fin::Media,
}

const RANDR: &str = "HDMI-A-1 \"Samsung (HDMI-A-1)\"\n  Enabled: yes\n  Modes:\n    1920x1080 px, 60.000000 Hz\n    3840x2160 px, 60.000000 Hz (preferred, current)\n    3840x2160 px, 30.000000 Hz\n  Scale: 1.000000\n";

impl Fixture {
    fn new() -> Self {
        // A fixed set of six apps (the v0.3 defaults), independent of the
        // shipped registry so registry edits do not reshuffle these tests.
        let apps = [
            ("Jellyfin", "media", "violet"),
            ("YouTube", "play", "red"),
            ("Steam", "game", "blue"),
            ("Spotify", "music", "green"),
            ("Moonlight", "stream", "sky"),
            ("Firefox", "browser", "orange"),
        ]
        .into_iter()
        .map(|(name, icon, accent)| apps::AppEntry {
            id: name.to_lowercase(),
            name: name.into(),
            subtitle: String::new(),
            icon: icon.into(),
            accent: accent.into(),
            command: name.to_lowercase(),
            singleton: true,
            icon_path: None,
            source: apps::AppSource::Configured,
        })
        .map(|entry| AppView {
            entry,
            installed: true,
            running: false,
        })
        .collect();
        Self {
            apps,
            sys: SystemSnapshot {
                wifi_on: Some(true),
                bluetooth: BluetoothStatus {
                    available: true,
                    powered: false,
                },
                volume: Some(VolumeState {
                    percent: 40,
                    muted: false,
                }),
                ..SystemSnapshot::default()
            },
            prefs: UiPrefs::default(),
            display: DisplayPrefs::default(),
            media: crate::fin::Media::default(),
        }
    }
    fn ctx(&self) -> ShellContext<'_> {
        ShellContext {
            media: &self.media,
            apps: &self.apps,
            sys: &self.sys,
            prefs: &self.prefs,
            display: &self.display,
        }
    }
}

fn press(s: &mut Shell, f: &Fixture, actions: &[Action]) -> Vec<Effect> {
    let mut all = Vec::new();
    for a in actions {
        all.extend(s.handle(*a, &f.ctx()));
    }
    all
}

fn cat_index(c: Category) -> usize {
    Category::ALL.iter().position(|x| *x == c).unwrap()
}

fn open_settings(s: &mut Shell, f: &Fixture, c: Category) {
    s.handle_ipc(ShellCommand::Settings);
    s.settings_cat = 0;
    for _ in 0..cat_index(c) {
        s.handle(Action::Down, &f.ctx());
    }
    assert_eq!(s.current_category(), c);
    s.handle(Action::Right, &f.ctx());
    assert!(s.settings_rows_focus);
}

use Action::*;

#[test]
fn home_rail_launches_and_resumes() {
    let mut f = Fixture::new();
    let mut s = Shell::new();
    assert_eq!(press(&mut s, &f, &[Right, Ok]), vec![Effect::Launch(1)]);
    f.apps[1].running = true;
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::Resume(1)]);
    press(&mut s, &f, &[Right; 9]);
    assert_eq!(s.rail_index, 5);
    press(&mut s, &f, &[Left; 9]);
    assert_eq!(s.rail_index, 0);
    // There is no "running" row any more: Down does nothing on Home.
    press(&mut s, &f, &[Down]);
    assert_eq!(s.page, Page::Home);
    assert!(!s.top_focus);
}

#[test]
fn long_press_opens_app_options() {
    let mut f = Fixture::new();
    let mut s = Shell::new();
    press(&mut s, &f, &[OkLong]);
    assert!(matches!(
        s.overlay(),
        Some(Overlay::AppOptions { app: 0, .. })
    ));
    // Not running, configured app: Open / Cancel.
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::Launch(0)]);
    assert!(s.overlay().is_none());
    f.apps[0].running = true;
    press(&mut s, &f, &[Options]);
    assert_eq!(press(&mut s, &f, &[Down, Down, Ok]), vec![Effect::Close(0)]);
}

#[test]
fn store_apps_can_be_uninstalled() {
    let mut f = Fixture::new();
    f.apps[3].entry.source = AppSource::Flatpak {
        app_id: "tv.kodi.Kodi".into(),
    };
    let mut s = Shell::new();
    press(&mut s, &f, &[Right, Right, Right, OkLong]);
    assert_eq!(press(&mut s, &f, &[Down, Ok]), vec![Effect::Uninstall(3)]);
    assert_eq!(app_options(true, true), app_options(true, false));
}

#[test]
fn top_bar_navigation() {
    let f = Fixture::new();
    let mut s = Shell::new();
    press(&mut s, &f, &[Up]);
    assert!(s.top_focus);
    // The bar follows focus: no OK needed to switch pages.
    press(&mut s, &f, &[Right]);
    assert_eq!(s.page, Page::Recents);
    assert!(s.top_focus);
    press(&mut s, &f, &[Right]);
    assert_eq!(s.page, Page::Apps);
    // OK (or Down) leaves the bar into the page.
    press(&mut s, &f, &[Ok]);
    assert!(!s.top_focus && s.page == Page::Apps);
    press(&mut s, &f, &[Up, Right]);
    assert_eq!(s.page, Page::Store);
    press(&mut s, &f, &[Right]);
    assert_eq!(s.page, Page::Search);
    // Back leaves the bar first, then the page.
    press(&mut s, &f, &[Back, Back]);
    assert_eq!(s.page, Page::Home);
    // No Settings tab (it lives in the Control Center); the last item is
    // the Control Center pill, which needs OK.
    press(&mut s, &f, &[Up, Right, Right, Right, Right, Right]);
    assert_eq!(s.page, Page::Search);
    press(&mut s, &f, &[Ok]);
    assert!(matches!(s.overlay(), Some(Overlay::Quick { .. })));
}

#[test]
fn apps_grid_moves_in_two_dimensions() {
    let mut f = Fixture::new();
    // 8 apps → rows [0..6] [6..8]
    let extra = f.apps[0].clone();
    f.apps.push(extra.clone());
    f.apps.push(extra);
    let mut s = Shell::new();
    s.handle_ipc(ShellCommand::Apps);
    press(&mut s, &f, &[Right; 5]);
    assert_eq!(s.grid_index, 5);
    press(&mut s, &f, &[Down]);
    assert_eq!(
        s.grid_index, 7,
        "down into a short last row lands on the last app"
    );
    press(&mut s, &f, &[Up]);
    assert_eq!(s.grid_index, 1);
    press(&mut s, &f, &[Up]);
    assert!(s.top_focus);
    assert_eq!(s.top_index, top_index_of(Page::Apps));
}

#[test]
fn quick_panel_actions() {
    let f = Fixture::new();
    let mut s = Shell::new();
    press(&mut s, &f, &[Quick]);
    // Network and Bluetooth open their panels; the switch is the first row.
    press(&mut s, &f, &[Ok]);
    assert!(matches!(s.overlay(), Some(Overlay::QuickNetwork { .. })));
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::SetWifi(false)]);
    press(&mut s, &f, &[Back]);
    press(&mut s, &f, &[Right, Ok]);
    assert!(matches!(s.overlay(), Some(Overlay::QuickBluetooth { .. })));
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::SetBluetooth(true)]);
    press(&mut s, &f, &[Back]);
    assert_eq!(
        press(&mut s, &f, &[Down, Left, Ok]),
        vec![Effect::ToggleMute]
    );
    press(&mut s, &f, &[Down, Right, Ok]); // Power → power menu
    assert!(matches!(s.overlay(), Some(Overlay::Power { index: 0 })));
    assert_eq!(
        press(&mut s, &f, &[Down, Down, Ok]),
        vec![Effect::Power(PowerAction::PowerOff)]
    );
    press(&mut s, &f, &[Back]);
    assert!(s.overlay().is_none());
}

#[test]
fn settings_sliders_choices_and_soon_rows() {
    let f = Fixture::new();
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Sound);
    assert_eq!(press(&mut s, &f, &[Left]), vec![Effect::VolumeStep(-5)]);
    assert_eq!(press(&mut s, &f, &[Right]), vec![Effect::VolumeStep(5)]);
    assert_eq!(press(&mut s, &f, &[Down, Ok]), vec![Effect::ToggleMute]);
    press(&mut s, &f, &[Back]);
    assert!(!s.settings_rows_focus);
    press(&mut s, &f, &[Back]);
    assert_eq!(s.page, Page::Home);

    // Accent: Left/Right move the ring, OK picks; nothing else moves.
    open_settings(&mut s, &f, Category::Customization);
    assert_eq!(press(&mut s, &f, &[Right]), vec![]);
    assert_eq!(
        press(&mut s, &f, &[Ok]),
        vec![Effect::SetAccent("periwinkle".into())]
    );
    // OK alone doesn't change the colour (see accent_colour_slides_then_ok_picks).
    assert_eq!(press(&mut s, &f, &[Ok]), vec![]);
    assert!(s.settings_rows_focus);

    // Updates: "Check for updates" asks for a check.
    open_settings(&mut s, &f, Category::Updates);
    assert_eq!(press(&mut s, &f, &[Down, Ok]), vec![Effect::CheckUpdates]);

    // Customization: clock format flips with Left/Right/OK, glow toggles.
    open_settings(&mut s, &f, Category::Customization);
    assert_eq!(
        press(&mut s, &f, &[Down, Right]),
        vec![Effect::SetClock24(false)]
    );
    assert_eq!(press(&mut s, &f, &[Down, Ok]), vec![Effect::SetGlow(false)]);
}

#[test]
fn power_rows_ask_for_confirmation() {
    let f = Fixture::new();
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::System);
    assert_eq!(press(&mut s, &f, &[Down, Down, Down, Down, Ok]), vec![]);
    assert!(matches!(s.overlay(), Some(Overlay::Power { index: 2 })));
    assert_eq!(press(&mut s, &f, &[Back]), vec![]);
    assert!(s.overlay().is_none());
}

#[test]
fn wifi_join_flow() {
    let f = Fixture::new();
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Network);
    // Wi-Fi on: the networks are rows of the page, after Connection.
    s.wifi_scan = Scan::Busy;
    let rows = s.settings_rows(&f.ctx());
    let heading = rows.last().unwrap();
    assert_eq!(
        (heading.title.as_str(), heading.value.as_str()),
        ("Wi-Fi networks", "Looking…")
    );
    s.on_wifi_scan(Result::Ok(vec![
        WifiNetwork {
            ssid: "Home".into(),
            signal: 80,
            secure: true,
            in_use: false,
        },
        WifiNetwork {
            ssid: "Cafe".into(),
            signal: 50,
            secure: false,
            in_use: false,
        },
    ]));
    let rows = s.settings_rows(&f.ctx());
    assert_eq!(rows[2].kind, crate::settings::RowKind::Heading);
    assert_eq!(rows[2].value, "");
    let home = &rows[3];
    assert_eq!(
        (
            home.title.as_str(),
            home.subtitle.as_str(),
            home.value.as_str()
        ),
        ("Home", "Secured", "Strong")
    );
    assert_eq!(rows.len(), 5);
    // Focus never rests on the heading.
    s.settings_row = 2;
    s.clamp(&f.ctx());
    assert_eq!(s.settings_row, 3);
    s.settings_row = 0;
    // Focus steps over the heading: Wi-Fi, Connection, Home, Cafe.
    assert_eq!(
        press(&mut s, &f, &[Down, Down, Down, Ok]),
        vec![Effect::ConnectWifi {
            ssid: "Cafe".into(),
            password: None
        }]
    );
    s.on_wifi_connect("Cafe", Err("Could not join Cafe: timed out".into()));
    press(&mut s, &f, &[Up, Ok]);
    assert!(s.text_mode());
    let short = press(&mut s, &f, &[Char('a'), Ok]);
    assert!(matches!(&short[0], Effect::Toast(t) if t.contains("8 characters")));
    let eff = press(
        &mut s,
        &f,
        &"bcdefgh".chars().map(Char).chain([Ok]).collect::<Vec<_>>(),
    );
    assert_eq!(
        eff,
        vec![Effect::ConnectWifi {
            ssid: "Home".into(),
            password: Some("abcdefgh".into())
        }]
    );
    let done = s.on_wifi_connect("Home", Result::Ok(()));
    assert_eq!(done, vec![Effect::Toast("Connected to Home".into())]);
    assert!(s.overlay().is_none());
    assert!(!s.text_mode());
}

#[test]
fn search_finds_apps_settings_and_store() {
    let f = Fixture::new();
    let mut s = Shell::new();
    s.handle_ipc(ShellCommand::Search);
    assert!(s.text_mode());
    assert_eq!(s.search_results(&f.ctx()).len(), 6);
    for c in "vol".chars() {
        s.handle(Char(c), &f.ctx());
    }
    let hits = s.search_results(&f.ctx());
    assert!(
        matches!(
            &hits[0],
            SearchHit::Setting {
                row: RowId::Volume,
                ..
            }
        ),
        "{hits:?}"
    );
    press(&mut s, &f, &[Ok]);
    assert_eq!(s.search_focus, SearchFocus::Results);
    press(&mut s, &f, &[Ok]);
    assert_eq!(s.page, Page::Settings);
    assert_eq!(s.current_category(), Category::Sound);
    assert!(s.settings_rows_focus);

    s.handle_ipc(ShellCommand::Search);
    s.search_osk.clear();
    for c in "steam".chars() {
        s.handle(Char(c), &f.ctx());
    }
    assert_eq!(press(&mut s, &f, &[Ok, Ok]), vec![Effect::Launch(2)]);

    // Not-installed store apps are found too and open the store panel.
    s.handle_ipc(ShellCommand::Search);
    s.search_osk.clear();
    for c in "kodi".chars() {
        s.handle(Char(c), &f.ctx());
    }
    let hits = s.search_results(&f.ctx());
    assert!(
        matches!(&hits[0], SearchHit::Store(i) if i.id == "tv.kodi.Kodi"),
        "{hits:?}"
    );
    press(&mut s, &f, &[Ok, Ok]);
    assert!(matches!(s.overlay(), Some(Overlay::StoreApp { .. })));
}

#[test]
fn search_asks_catfyn_and_lists_its_results() {
    let mut f = Fixture::new();
    f.media.accounts.add(crate::jellyfin::Account {
        server: "http://10.0.0.5:8096".into(),
        server_name: "Den".into(),
        user_id: "u1".into(),
        user_name: "Ok".into(),
        token: "t".into(),
        incognito: false,
    });
    let mut s = Shell::new();
    s.handle_ipc(ShellCommand::Search);
    s.search_osk.clear();
    assert!(
        s.handle(Char('d'), &f.ctx()).is_empty(),
        "one letter: no request"
    );
    assert_eq!(
        s.handle(Char('o'), &f.ctx()),
        vec![Effect::SearchMedia("do".into())]
    );
    let item = crate::jellyfin::Item {
        id: "s1".into(),
        kind_raw: "Series".into(),
        name: "Doraemon".into(),
        ..Default::default()
    };
    // A stale answer is ignored; the current one is listed.
    s.on_search_media("d".into(), vec![item.clone()]);
    assert!(
        !s.search_results(&f.ctx())
            .iter()
            .any(|h| matches!(h, SearchHit::Media(_)))
    );
    s.on_search_media("do".into(), vec![item.clone()]);
    let hits = s.search_results(&f.ctx());
    let pos = hits
        .iter()
        .position(|h| matches!(h, SearchHit::Media(i) if i.id == "s1"))
        .expect("Catfyn result listed");
    press(&mut s, &f, &[Right]);
    s.search_index = pos;
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::FinDetails(item)]);
}

#[test]
fn typing_anywhere_starts_a_search() {
    let f = Fixture::new();
    let mut s = Shell::new();
    s.handle_ipc(ShellCommand::Settings);
    assert!(press(&mut s, &f, &[Char('y'), Char('o')]).is_empty());
    assert_eq!(s.page, Page::Search);
    assert_eq!(s.search_osk.text(), "yo");
    assert!(s.text_mode());
    // Inside an overlay, stray characters are ignored.
    s.go_home();
    press(&mut s, &f, &[Quick, Char('x')]);
    assert_eq!(s.page, Page::Home);
    assert!(matches!(s.overlay(), Some(Overlay::Quick { .. })));
}

#[test]
fn home_key_and_ipc_always_recover() {
    let f = Fixture::new();
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Network);
    s.handle_ipc(ShellCommand::Quick);
    s.handle_ipc(ShellCommand::Power);
    assert_eq!(s.overlays.len(), 2);
    press(&mut s, &f, &[Home]);
    assert_eq!(s.page, Page::Home);
    assert!(s.overlays.is_empty());
    s.handle_ipc(ShellCommand::Quick);
    assert!(matches!(s.overlay(), Some(Overlay::Quick { .. })));
    assert_eq!(s.handle_ipc(ShellCommand::Reload), vec![Effect::ReloadApps]);
    assert_eq!(s.handle_ipc(ShellCommand::Escape), vec![Effect::LeaveApp]);
}

#[test]
fn empty_registry_does_not_panic() {
    let mut f = Fixture::new();
    f.apps.clear();
    f.sys = SystemSnapshot::default();
    let mut s = Shell::new();
    for a in [Left, Right, Up, Down, Ok, OkLong, Back, Options] {
        s.handle(a, &f.ctx());
    }
    s.handle_ipc(ShellCommand::Apps);
    for a in [Left, Right, Up, Down, Ok, OkLong, Back] {
        s.handle(a, &f.ctx());
    }
    s.handle_ipc(ShellCommand::Settings);
    for _ in 0..Category::ALL.len() {
        for a in [Right, Ok, Down, Ok, Left, Right, Back, Down] {
            s.handle(a, &f.ctx());
        }
        s.overlays.clear();
    }
    for t in [
        Target::Rail(3),
        Target::Grid(9),
        Target::Item(4),
        Target::SettingsRow(40),
        Target::Outside,
    ] {
        s.pointer(t, true, &f.ctx());
    }
}

#[test]
fn store_browse_install_and_search() {
    let mut f = Fixture::new();
    let mut s = Shell::new();
    press(&mut s, &f, &[Up, Right, Right, Right, Ok]);
    assert_eq!(s.page, Page::Store);
    assert_eq!(s.current_store_cat(), StoreCat::Featured);
    let n = s.store_items(&f.ctx()).len();
    assert!(n >= 10, "curated catalogue is shown");
    // Category chips run left to right; Down enters the grid.
    press(&mut s, &f, &[Right]);
    assert_eq!(s.current_store_cat(), STORE_CATS[1]);
    press(&mut s, &f, &[Left, Down]);
    assert!(s.store_grid);
    assert_eq!(s.current_store_cat(), StoreCat::Featured);
    press(&mut s, &f, &[Up]);
    assert!(!s.store_grid, "Up from the first row returns to the chips");
    press(&mut s, &f, &[Down, Right, Down]);
    assert_eq!(s.store_index, 1 + STORE_COLS);
    let item = s.store_items(&f.ctx())[s.store_index].clone();
    // The full-screen page loads its details.
    assert_eq!(
        press(&mut s, &f, &[Ok]),
        vec![Effect::StoreDetails(item.id.clone())]
    );
    assert!(matches!(s.overlay(), Some(Overlay::StoreApp { .. })));
    s.on_store_details(
        &item.id,
        Result::Ok(crate::flatpak::AppDetails {
            id: item.id.clone(),
            screenshots: vec!["https://a/1.png".into(), "https://a/2.png".into()],
            ..Default::default()
        }),
    );
    // Down → first screenshot (after Install, Close), Right → second,
    // Up → back to the buttons.
    press(&mut s, &f, &[Down, Right, Right]);
    assert!(matches!(
        s.overlay(),
        Some(Overlay::StoreApp { index: 3, .. })
    ));
    press(&mut s, &f, &[Up]);
    assert!(matches!(
        s.overlay(),
        Some(Overlay::StoreApp { index: 0, .. })
    ));
    assert_eq!(
        press(&mut s, &f, &[Ok]),
        vec![Effect::StoreInstall(item.id.clone())]
    );
    assert!(s.store_job(&item.id).is_some());
    // While installing only "Close" is offered.
    assert_eq!(press(&mut s, &f, &[Ok]), vec![]);
    assert!(s.overlay().is_none());
    s.on_store_job(&item.id, None);
    f.sys.flatpaks.push(crate::flatpak::InstalledApp {
        id: item.id.clone(),
        name: item.name.clone(),
        version: "1".into(),
        installation: "user".into(),
    });
    press(&mut s, &f, &[Ok]);
    assert_eq!(
        press(&mut s, &f, &[Ok]),
        vec![Effect::StoreOpen(item.id.clone())]
    );

    // Search category opens a keyboard and searches Flathub.
    s.store_grid = false;
    s.store_cat = STORE_CATS.len() - 1;
    press(&mut s, &f, &[Ok]);
    assert!(s.text_mode());
    let eff = press(
        &mut s,
        &f,
        &"jelly".chars().map(Char).chain([Ok]).collect::<Vec<_>>(),
    );
    // Typing jumps focus to Done, so the final OK submits.
    assert_eq!(eff, vec![Effect::StoreSearch("jelly".into())]);
    assert_eq!(s.store_results, Scan::Busy);
    s.on_store_search(Result::Ok(vec![StoreItem {
        id: "org.jellyfin.JellyfinDesktop".into(),
        name: "Jellyfin".into(),
        blurb: String::new(),
    }]));
    assert_eq!(s.store_items(&f.ctx()).len(), 1);
}

#[test]
fn display_mode_change_needs_confirmation() {
    let mut f = Fixture::new();
    f.sys.outputs = crate::display::parse_wlr_randr(RANDR);
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Display);
    press(&mut s, &f, &[Ok]);
    let modes = s.menu_modes(&f.ctx());
    assert_eq!(modes.len(), 3);
    match s.overlay() {
        Some(Overlay::DisplayModes { index }) => assert!(modes[*index].current),
        o => panic!("{o:?}"),
    }
    // Choose 1080p (the last entry: sorted by resolution, desc).
    let eff = press(&mut s, &f, &[Down, Down, Ok]);
    let target = modes[2];
    assert_eq!(target.width, 1920);
    assert_eq!(
        eff,
        vec![Effect::SetMode {
            output: "HDMI-A-1".into(),
            mode: target
        }]
    );
    assert!(matches!(
        s.overlay(),
        Some(Overlay::DisplayConfirm { index: 1 })
    ));
    // Revert is focused, so a blind OK cannot keep a broken mode.
    // Timeout reverts.
    let eff = s.on_display_timeout();
    assert!(matches!(&eff[0], Effect::RevertMode { mode: Some(m), .. } if m.width == 3840));
    assert!(s.overlay().is_none());
    // Keep persists.
    press(&mut s, &f, &[Ok]);
    press(&mut s, &f, &[Down, Down, Ok]);
    assert_eq!(
        press(&mut s, &f, &[Left, Ok]),
        vec![Effect::KeepMode {
            output: "HDMI-A-1".into(),
            mode: target
        }]
    );
    // Back on the confirmation reverts.
    press(&mut s, &f, &[Ok, Down, Down, Ok]);
    assert!(matches!(
        press(&mut s, &f, &[Back])[0],
        Effect::RevertMode { .. }
    ));
}

#[test]
fn display_scale_and_picture_fit() {
    let mut f = Fixture::new();
    f.sys.outputs = crate::display::parse_wlr_randr(RANDR);
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Display);
    press(&mut s, &f, &[Down]); // Interface size
    assert_eq!(
        press(&mut s, &f, &[Right]),
        vec![Effect::SetScale {
            output: "HDMI-A-1".into(),
            percent: 125
        }]
    );
    assert_eq!(press(&mut s, &f, &[Left]), vec![], "already at the minimum");
    // A size just chosen (saved, not yet reported by the compositor) is
    // the one the next press steps from.
    f.display.output = "HDMI-A-1".into();
    f.display.scale = Some(1.25);
    assert_eq!(
        press(&mut s, &f, &[Right]),
        vec![Effect::SetScale {
            output: "HDMI-A-1".into(),
            percent: 150
        }]
    );
    press(&mut s, &f, &[Down]); // Picture fit
    assert_eq!(press(&mut s, &f, &[Right]), vec![Effect::SetSafeArea(1)]);
    f.display.safe_area_pct = crate::display::MAX_SAFE_AREA_PCT;
    assert_eq!(press(&mut s, &f, &[Right]), vec![]);
}

#[test]
fn bluetooth_pairing_flow() {
    let mut f = Fixture::new();
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Bluetooth);
    // Radio off: pairing explains instead of scanning.
    let eff = press(&mut s, &f, &[Down, Ok]);
    assert!(matches!(&eff[0], Effect::Toast(t) if t.contains("on first")));
    f.sys.bluetooth.powered = true;
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::BtScan]);
    s.on_bt_scan(Result::Ok(vec![Device {
        address: "AA:BB:CC:DD:EE:FF".into(),
        name: "Remote".into(),
    }]));
    assert_eq!(
        press(&mut s, &f, &[Ok]),
        vec![Effect::BtPair("AA:BB:CC:DD:EE:FF".into())]
    );
    assert_eq!(press(&mut s, &f, &[Ok]), vec![], "busy while pairing");
    let eff = s.on_bt_paired("AA:BB:CC:DD:EE:FF", Result::Ok("Remote".into()));
    assert_eq!(eff, vec![Effect::Toast("Connected to Remote".into())]);
    assert!(s.overlay().is_none());
    // Paired devices get options.
    f.sys.bt_devices.push(Device {
        address: "AA:BB:CC:DD:EE:FF".into(),
        name: "Remote".into(),
    });
    // Rows: Bluetooth, Pair a new device, then the paired devices.
    s.settings_row = 2;
    press(&mut s, &f, &[Ok]);
    assert!(matches!(
        s.overlay(),
        Some(Overlay::BtDevice { device: 0, .. })
    ));
    assert_eq!(
        press(&mut s, &f, &[Down, Down, Ok]),
        vec![Effect::BtForget("AA:BB:CC:DD:EE:FF".into())]
    );
}

#[test]
fn audio_output_choice() {
    let mut f = Fixture::new();
    f.sys.sinks = vec![
        crate::audio::Sink {
            id: 48,
            name: "Speakers".into(),
            node_name: None,
            is_default: true,
            kind: SinkKind::Analog,
        },
        crate::audio::Sink {
            id: 52,
            name: "Samsung TV".into(),
            node_name: None,
            is_default: false,
            kind: SinkKind::Hdmi,
        },
    ];
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Sound);
    press(&mut s, &f, &[Down, Down, Ok]);
    assert!(matches!(
        s.overlay(),
        Some(Overlay::AudioOutputs { index: 0 })
    ));
    assert_eq!(press(&mut s, &f, &[Down, Ok]), vec![Effect::SetSink(52)]);
}

#[test]
fn android_rows_trigger_setup_and_apk_install() {
    let mut f = Fixture::new();
    f.sys.android.installed = true;
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Android);
    // The setup row opens the store chooser: Right = Aurora, OK
    // saves the choice and starts the setup; Back would just close it.
    assert_eq!(press(&mut s, &f, &[Ok]), vec![]);
    assert!(matches!(
        s.overlays.last(),
        Some(Overlay::StoreChoice { index: 0 })
    ));
    assert_eq!(
        press(&mut s, &f, &[Right, Ok]),
        vec![
            Effect::SetAndroidVariant("vanilla".into()),
            Effect::AndroidSetup
        ]
    );
    assert!(s.overlays.is_empty());
    f.sys.android.initialized = true;
    f.sys.android_image = Some("atv16-vanilla".into());
    // Status, Install an APK, Repair (no Aurora row:).
    assert_eq!(
        press(&mut s, &f, &[Up, Down, Ok]),
        vec![Effect::BrowseApks(None)]
    );
    let dir = |name: &str, path: &str| crate::android::FileEntry {
        name: name.into(),
        path: path.into(),
        dir: true,
    };
    s.on_apks(Result::Ok(vec![
        dir("Downloads", "/home/catostv/Downloads"),
        dir("USB", "/run/media/catostv/USB"),
    ]));
    // Open the second place, then go back up to it with Back.
    assert_eq!(
        press(&mut s, &f, &[Down, Ok]),
        vec![Effect::BrowseApks(Some("/run/media/catostv/USB".into()))]
    );
    s.on_apks(Result::Ok(vec![crate::android::FileEntry {
        name: "app.apk".into(),
        path: "/run/media/catostv/USB/app.apk".into(),
        dir: false,
    }]));
    assert_eq!(press(&mut s, &f, &[Back]), vec![Effect::BrowseApks(None)]);
    assert!(matches!(s.overlay(), Some(Overlay::ApkList { index: 1 })));
    s.on_apks(Result::Ok(vec![
        dir("Downloads", "/home/catostv/Downloads"),
        dir("USB", "/run/media/catostv/USB"),
    ]));
    press(&mut s, &f, &[Ok]);
    s.on_apks(Result::Ok(vec![crate::android::FileEntry {
        name: "app.apk".into(),
        path: "/run/media/catostv/USB/app.apk".into(),
        dir: false,
    }]));
    assert_eq!(
        press(&mut s, &f, &[Ok]),
        vec![Effect::InstallApk("/run/media/catostv/USB/app.apk".into())]
    );
    assert!(s.overlay().is_none());
    // Reset Android asks first: Cancel is the default, Back
    // and OK on Cancel change nothing, OK on Reset Android resets.
    assert_eq!(press(&mut s, &f, &[Down, Ok]), vec![]);
    assert!(matches!(
        s.overlay(),
        Some(Overlay::RepairConfirm { index: 0 })
    ));
    assert_eq!(press(&mut s, &f, &[Right, Back]), vec![]);
    assert!(s.overlay().is_none());
    assert_eq!(press(&mut s, &f, &[Ok, Ok]), vec![]);
    assert!(s.overlay().is_none());
    assert_eq!(
        press(&mut s, &f, &[Ok, Right, Ok]),
        vec![Effect::AndroidRepair]
    );
    assert!(s.overlay().is_none());
}

#[test]
fn key_guide_closes_and_is_remembered() {
    let f = Fixture::new();
    let mut s = Shell::new();
    s.show_key_guide();
    s.show_key_guide();
    assert_eq!(s.overlays.len(), 1);
    assert_eq!(press(&mut s, &f, &[Back]), vec![Effect::KeyGuideSeen]);
    assert!(s.overlay().is_none());
}

#[test]
fn pointer_hover_focuses_and_click_activates() {
    let mut f = Fixture::new();
    let mut s = Shell::new();
    assert!(s.pointer(Target::Rail(3), false, &f.ctx()).is_empty());
    assert_eq!(s.rail_index, 3);
    assert_eq!(
        s.pointer(Target::Rail(4), true, &f.ctx()),
        vec![Effect::Launch(4)]
    );
    // Targets of another page are ignored.
    assert!(s.pointer(Target::Grid(1), true, &f.ctx()).is_empty());
    // Top bar click opens the page.
    s.pointer(Target::Top(2), true, &f.ctx());
    assert_eq!(s.page, Page::Apps);
    // Overlay items and outside clicks.
    f.apps[0].running = true;
    s.handle(OkLong, &f.ctx());
    assert!(
        s.pointer(Target::Grid(2), true, &f.ctx()).is_empty(),
        "blocked by overlay"
    );
    s.pointer(Target::Item(2), true, &f.ctx());
    assert!(s.overlay().is_none());
    s.handle(Quick, &f.ctx());
    s.pointer(Target::Outside, true, &f.ctx());
    assert!(s.overlay().is_none());
    // Settings rows.
    s.handle_ipc(ShellCommand::Settings);
    s.pointer(
        Target::SettingsCat(cat_index(Category::Customization)),
        true,
        &f.ctx(),
    );
    assert!(s.settings_rows_focus);
    // A click on the accent row only focuses it (no colour jump).
    assert_eq!(s.pointer(Target::SettingsRow(0), true, &f.ctx()), vec![]);
    // On-screen keyboard keys.
    s.handle_ipc(ShellCommand::Search);
    s.pointer(Target::SearchKey(1, 0), true, &f.ctx());
    assert_eq!(s.search_osk.text(), "q");
}

#[test]
fn escape_gesture_maps_to_leave_app() {
    let mut s = Shell::new();
    assert_eq!(s.handle_ipc(ShellCommand::Escape), vec![Effect::LeaveApp]);
}

#[test]
fn quick_settings_volume_row() {
    let f = Fixture::new();
    let mut s = Shell::new();
    s.handle_ipc(ShellCommand::Quick);
    // 6 tiles in 3 rows of 2; Down from the last row reaches the volume row.
    press(&mut s, &f, &[Down, Down, Down]);
    assert!(matches!(s.overlay(), Some(Overlay::Quick { index }) if *index == QUICK_ITEMS.len()));
    assert_eq!(press(&mut s, &f, &[Left]), vec![Effect::VolumeStep(-5)]);
    assert_eq!(press(&mut s, &f, &[Right]), vec![Effect::VolumeStep(5)]);
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::ToggleMute]);
    press(&mut s, &f, &[Up]);
    assert!(matches!(s.overlay(), Some(Overlay::Quick { index: 4 })));
}

#[test]
fn recents_resume_close_and_close_all() {
    let mut f = Fixture::new();
    f.apps[1].running = true;
    f.apps[3].running = true;
    let mut s = Shell::new();
    press(&mut s, &f, &[Up, Right, Ok]);
    assert_eq!(s.page, Page::Recents);
    // Cards follow ctx.running(): [1, 3]; "Close all" sits under them.
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::Resume(1)]);
    assert_eq!(press(&mut s, &f, &[Right, Up, Ok]), vec![Effect::Close(3)]);
    assert!(!s.recents_close);
    // Right stops at the last card (Close all is not in the row).
    assert!(press(&mut s, &f, &[Right]).is_empty());
    assert_eq!(s.recents_index, 1);
    // Card → Down → Close all.
    assert_eq!(
        press(&mut s, &f, &[Down, Ok]),
        vec![Effect::Close(1), Effect::Close(3)]
    );
    // Up from Close all returns to the card.
    press(&mut s, &f, &[Up]);
    assert_eq!((s.recents_index, s.recents_close), (1, false));
    // Card → X → top bar.
    press(&mut s, &f, &[Up, Up]);
    assert!(s.top_focus);
    // Mouse: the X of card 0.
    s.top_focus = false;
    assert_eq!(
        s.pointer(Target::RecentClose(0), true, &f.ctx()),
        vec![Effect::Close(1)]
    );
    // Nothing running: Up reaches the top bar, nothing else happens.
    f.apps[1].running = false;
    f.apps[3].running = false;
    s.top_focus = false;
    assert!(press(&mut s, &f, &[Ok]).is_empty());
}

#[test]
fn controller_settings_effects() {
    let mut f = Fixture::new();
    f.sys.controllers = vec!["8BitDo Pro 2".into()];
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Keys);
    // Pair (first) → the controller's layout → Use controllers.
    assert_eq!(
        press(&mut s, &f, &[Down, Right]),
        vec![Effect::SetPadLayout {
            device: "8BitDo Pro 2".into(),
            nintendo: true
        }]
    );
    assert_eq!(
        press(&mut s, &f, &[Down, Ok]),
        vec![Effect::SetPadNav(false)]
    );
    // No controller: no controller rows and no "Use controllers" toggle.
    f.sys.controllers.clear();
    let rows = crate::settings::rows(Category::Keys, &f.ctx());
    assert!(
        rows.iter()
            .all(|r| !matches!(r.id, crate::settings::RowId::PadNav))
    );
}

#[test]
fn hdr_confirm_keeps_or_turns_off() {
    let f = Fixture::new();
    let mut s = Shell::new();
    s.confirm_hdr();
    // Revert is focused: a blind OK on a black screen turns HDR off.
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::SetHdr(false)]);
    assert!(s.overlay().is_none() && !s.hdr_pending);
    s.confirm_hdr();
    assert_eq!(press(&mut s, &f, &[Left, Ok]), vec![Effect::KeepHdr]);
    s.confirm_hdr();
    assert_eq!(s.on_display_timeout(), vec![Effect::SetHdr(false)]);
    assert!(s.overlay().is_none());
}

#[test]
fn quick_settings_over_an_app_return_to_it() {
    let f = Fixture::new();
    let mut s = Shell::new();
    s.open_quick(true);
    s.quick_app = Some(("Jellyfin".into(), false));
    assert!(matches!(s.overlay(), Some(Overlay::Quick { .. })));
    // App controls first: Back to app · Full screen · Close.
    assert_eq!(
        press(&mut s, &f, &[Right, Ok]),
        vec![
            Effect::AppWindow(crate::toplevel::WindowAction::Fullscreen(true)),
            Effect::ReturnToApp
        ]
    );
    s.open_quick(true);
    assert_eq!(
        press(&mut s, &f, &[Right, Right, Ok]),
        vec![Effect::AppWindow(crate::toplevel::WindowAction::Close)]
    );
    s.open_quick(true);
    // Down reaches the normal tiles; Up from the first row comes back.
    press(&mut s, &f, &[Down]);
    assert!(matches!(s.overlay(), Some(Overlay::Quick { index: 3 })));
    press(&mut s, &f, &[Up]);
    assert!(matches!(s.overlay(), Some(Overlay::Quick { index: 0 })));
    assert_eq!(press(&mut s, &f, &[Back]), vec![Effect::ReturnToApp]);
    assert!(!s.quick_over_app);
    // Opened inside CatosTV: Back just closes it, the page stays.
    s.page = Page::Apps;
    s.open_quick(false);
    assert!(press(&mut s, &f, &[Back]).is_empty());
    assert_eq!(s.page, Page::Apps);
}

#[test]
fn kde_pairing_request_pops_up_once() {
    let mut f = Fixture::new();
    f.sys.kde = vec![crate::kdeconnect::KdeDevice {
        id: "abc".into(),
        name: "Pixel".into(),
        paired: false,
        reachable: true,
        pair_requested_by_peer: true,
    }];
    let mut s = Shell::new();
    s.on_kde_requests(&f.ctx());
    s.on_kde_requests(&f.ctx());
    assert_eq!(s.overlays.len(), 1);
    assert_eq!(
        press(&mut s, &f, &[Ok]),
        vec![Effect::KdeAccept("abc".into())]
    );
    s.kde_prompted.clear();
    s.on_kde_requests(&f.ctx());
    assert_eq!(
        press(&mut s, &f, &[Right, Ok]),
        vec![Effect::KdeReject("abc".into())]
    );
}

#[test]
fn catfyn_tile_home_row_and_input_routing() {
    let mut f = Fixture::new();
    f.apps[0].entry.command = crate::apps::FIN_COMMAND.into();
    let mut s = Shell::new();
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::OpenFin]);
    // The GUI opens it; input then goes to Catfyn, Back closes it.
    let (fin, _) = crate::fin::Fin::open(&f.media.accounts);
    s.fin = Some(fin);
    assert!(!s.text_mode(), "Connect is a list, not a keyboard");
    s.fin.as_mut().unwrap().on_discovered(vec![]);
    press(&mut s, &f, &[Back]);
    assert!(s.fin.is_none());
    // Continue watching on Home.
    let item = crate::jellyfin::Item {
        id: "m1".into(),
        kind_raw: "Movie".into(),
        ..Default::default()
    };
    f.media.home = vec![
        crate::fin::Row {
            title: "Continue watching".into(),
            items: vec![item.clone()],
            libraries: false,
        },
        crate::fin::Row {
            title: "Movies".into(),
            items: vec![item.clone(), item.clone()],
            libraries: false,
        },
    ];
    press(&mut s, &f, &[Down]);
    assert_eq!(s.home_media, Some((0, 0)));
    assert_eq!(
        press(&mut s, &f, &[Ok]),
        vec![Effect::FinPlayHome(item.clone())]
    );
    assert_eq!(
        press(&mut s, &f, &[Options]),
        vec![Effect::FinDetails(item.clone())]
    );
    press(&mut s, &f, &[Down, Right]);
    assert_eq!(s.home_media, Some((1, 1)));
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::FinDetails(item)]);
    press(&mut s, &f, &[Up, Up]);
    assert_eq!(s.home_media, None);
}

#[test]
fn settings_apps_search_filters_the_list() {
    let mut f = Fixture::new();
    // Ten apps: the search row appears.
    for i in 0..6 {
        let mut a = f.apps[0].clone();
        a.entry.name = format!("Zed {i}");
        a.entry.id = format!("zed{i}");
        f.apps.push(a);
    }
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Apps);
    let rows = s.settings_rows(&f.ctx());
    assert_eq!(rows[0].id, crate::settings::RowId::AppSearch);
    // (The fixture has no Flatpak, so no "Update store apps" row.)
    assert_eq!(rows.len(), 1 + f.apps.len());
    press(&mut s, &f, &[Ok]);
    assert!(matches!(s.overlay(), Some(Overlay::AppsSearch { .. })));
    press(&mut s, &f, &[Char('z'), Char('e')]);
    let rows = s.settings_rows(&f.ctx());
    assert_eq!(
        rows.len(),
        1 + 6,
        "search row + the six Zed apps, no Update row"
    );
    assert_eq!(rows[0].title, "ze");
    press(&mut s, &f, &[Back]);
    assert!(s.overlay().is_none());
    assert_eq!(s.apps_filter, "ze");
}

#[test]
fn store_downloads_log_and_installed_override() {
    let f = Fixture::new();
    let mut s = Shell::new();
    let item = StoreItem {
        id: "org.example.App".into(),
        name: "Example".into(),
        blurb: String::new(),
    };
    s.overlays.push(Overlay::StoreApp {
        item: item.clone(),
        index: 0,
    });
    let ctx = f.ctx();
    let eff = s.handle(Ok, &ctx);
    assert_eq!(eff, vec![Effect::StoreInstall("org.example.App".into())]);
    assert_eq!(s.store_log[0].state, StoreLogState::Installing);
    assert!(!s.store_installed(&ctx, "org.example.App"));
    // Finished: "Installed" at once, before the system list knows.
    s.on_store_done("org.example.App", Result::Ok(()));
    assert_eq!(s.store_log[0].state, StoreLogState::Installed);
    assert!(s.store_installed(&ctx, "org.example.App"));
    assert_eq!(
        s.actions_for(&item, &ctx),
        vec![StoreAction::Open, StoreAction::Remove, StoreAction::Close]
    );
    // Downloads lists it.
    s.store_cat = STORE_CATS
        .iter()
        .position(|c| *c == StoreCat::Downloads)
        .unwrap();
    assert_eq!(s.store_items(&ctx), vec![item.clone()]);
    // Three system lists without it: the list decides again.
    for _ in 0..3 {
        s.settle_store_log(&ctx);
    }
    assert_eq!(s.store_log[0].state, StoreLogState::Done);
    assert!(!s.store_installed(&ctx, "org.example.App"));
    // A failure is recorded.
    s.on_store_job("org.example.App", Some(StoreJob::Installing(None)));
    s.on_store_done("org.example.App", Err("no space".into()));
    assert_eq!(
        s.store_log[0].state,
        StoreLogState::Failed("no space".into())
    );
}

#[test]
fn empty_recents_keeps_focus_in_the_top_bar() {
    // Down on an empty Recents page used to leave focus on nothing.
    let f = Fixture::new();
    let mut s = Shell::new();
    press(&mut s, &f, &[Up, Right, Ok]);
    assert_eq!(s.page, Page::Recents);
    press(&mut s, &f, &[Down]);
    assert!(s.top_focus);
    // The tabs still work at once.
    press(&mut s, &f, &[Right]);
    assert_eq!(s.top_index, top_index_of(Page::Recents) + 1);
}

#[test]
fn debug_page_ssh_password_and_old_login() {
    // Settings → Debug.
    let mut f = Fixture::new();
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Debug);
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::DebugSsh(true)]);
    f.sys.debug.ssh_on = true;
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::DebugSsh(false)]);
    // Change password: typed twice; a mismatch starts over.
    press(&mut s, &f, &[Down, Down, Ok]);
    assert!(matches!(s.overlay(), Some(Overlay::DebugPassword { .. })));
    let type_text = |s: &mut Shell, t: &str| {
        for c in t.chars() {
            press(s, &f, &[Char(c)]);
        }
    };
    type_text(&mut s, "abc");
    assert!(matches!(
        press(&mut s, &f, &[Ok]).as_slice(),
        [Effect::Toast(_)]
    ));
    type_text(&mut s, "d");
    assert_eq!(press(&mut s, &f, &[Ok]), vec![]);
    type_text(&mut s, "abce");
    assert!(matches!(
        press(&mut s, &f, &[Ok]).as_slice(),
        [Effect::Toast(_)]
    ));
    type_text(&mut s, "abcd");
    press(&mut s, &f, &[Ok]);
    type_text(&mut s, "abcd");
    assert_eq!(
        press(&mut s, &f, &[Ok]),
        vec![Effect::DebugPassword("abcd".into())]
    );
    assert!(s.overlay().is_none());
    // "Remove the old ok login" only once a catostv SSH login was seen.
    let ids = |f: &Fixture| {
        crate::settings::rows(Category::Debug, &f.ctx())
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>()
    };
    f.sys.debug.old_login_present = true;
    assert!(!ids(&f).contains(&RowId::DebugRemoveOld));
    f.sys.debug.ssh_login_seen = true;
    assert!(ids(&f).contains(&RowId::DebugRemoveOld));
}

#[test]
fn quick_settings_network_ethernet_and_bluetooth() {
    // Network and Bluetooth open panels inside Quick settings.
    let mut f = Fixture::new();
    f.sys.wifi_on = Some(true);
    f.sys.ethernet = Some(crate::net::Ethernet {
        device: "eno1".into(),
        connection: Some("Wired connection 1".into()),
        connected: true,
        address: Some("192.168.1.88/24".into()),
        gateway: Some("192.168.1.1".into()),
        dns: vec!["192.168.1.44".into()],
        ..Default::default()
    });
    f.sys.bluetooth.available = true;
    f.sys.bluetooth.powered = true;
    f.sys.bt_devices = vec![crate::bluetooth::Device {
        address: "AA:BB:CC:DD:EE:01".into(),
        name: "Pad".into(),
    }];
    let mut s = Shell::new();
    press(&mut s, &f, &[Quick]);
    // Network tile (first) → panel, and a Wi-Fi scan starts.
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::ScanWifi]);
    assert!(matches!(s.overlay(), Some(Overlay::QuickNetwork { .. })));
    // Ethernet row → details with the current settings as the draft.
    press(&mut s, &f, &[Down, Ok]);
    assert!(matches!(s.overlay(), Some(Overlay::Ethernet { .. })));
    assert_eq!(s.eth_rows(), vec![EthRow::Method, EthRow::Save]);
    // Manual: the fields appear, prefilled from the current lease.
    press(&mut s, &f, &[Ok]);
    assert_eq!(s.eth_rows().len(), 5);
    assert_eq!(s.eth_draft.as_ref().unwrap().address, "192.168.1.88/24");
    // Edit the address: a bad value is refused, a good one kept.
    press(&mut s, &f, &[Down, Ok]);
    assert!(matches!(s.overlay(), Some(Overlay::EthField { .. })));
    for _ in 0..20 {
        press(&mut s, &f, &[Erase]);
    }
    let typed: Vec<Action> = "192.168.1.90".chars().map(Char).collect();
    press(&mut s, &f, &typed);
    assert_eq!(press(&mut s, &f, &[Ok]), vec![]);
    assert!(matches!(s.overlay(), Some(Overlay::Ethernet { .. })));
    assert_eq!(s.eth_draft.as_ref().unwrap().address, "192.168.1.90/24");
    // Save.
    let eff = press(&mut s, &f, &[Down, Down, Down, Ok]);
    assert!(matches!(
        &eff[0],
        Effect::SetEthernetIp { connection, config }
            if connection == "Wired connection 1" && config.manual && config.address == "192.168.1.90/24"
    ));
    // Back to the grid, then Bluetooth → panel → connect the paired pad.
    press(&mut s, &f, &[Back]);
    assert!(matches!(s.overlay(), Some(Overlay::Quick { .. })));
    press(&mut s, &f, &[Right, Ok]);
    assert!(matches!(s.overlay(), Some(Overlay::QuickBluetooth { .. })));
    assert_eq!(
        press(&mut s, &f, &[Down, Ok]),
        vec![Effect::BtConnect("AA:BB:CC:DD:EE:01".into())]
    );
}

#[test]
fn accent_colour_slides_then_ok_picks() {
    // On the accent row Left/Right move a ring over the swatches without
    // changing anything; OK picks; moving off the row forgets the ring.
    let f = Fixture::new();
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Customization);
    let at = crate::settings::rows(Category::Customization, &f.ctx())
        .iter()
        .position(|r| r.id == RowId::Accent)
        .unwrap();
    s.settings_row = at;
    let original = f.ctx().prefs.accent.clone();
    assert_eq!(press(&mut s, &f, &[Right, Right]), vec![]);
    let two = crate::prefs::cycle_accent(&original, 2).to_string();
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::SetAccent(two)]);
    assert!(s.overlay().is_none());
    // OK with no ring moved does nothing (it used to jump a colour).
    assert_eq!(press(&mut s, &f, &[Ok]), vec![]);
    press(&mut s, &f, &[Left]);
    assert!(s.accent_pick.is_some());
    press(&mut s, &f, &[Back]);
    assert!(s.accent_pick.is_none());
}

#[test]
fn updates_page_offers_the_found_version() {
    // Plan C: a check that found a version shows Install (its notes are
    // drawn as one box under the rows, not as rows).
    let mut f = Fixture::new();
    f.sys.updates.check = Some(Result::Ok(crate::updates::Check::Available(
        crate::updates::Manifest {
            version: "9.0.0".into(),
            notes: vec!["Faster Home".into()],
            ..Default::default()
        },
    )));
    let ids: Vec<RowId> = crate::settings::rows(Category::Updates, &f.ctx())
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(
        ids,
        [
            RowId::UpdateStatus,
            RowId::UpdateInstall,
            RowId::UpdateCheck
        ]
    );
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Updates);
    assert_eq!(press(&mut s, &f, &[Down, Ok]), vec![Effect::InstallUpdate]);
    // While installing, neither button is offered.
    f.sys.updates.install = Some(crate::updates::InstallState::Running("x".into()));
    assert_eq!(crate::settings::rows(Category::Updates, &f.ctx()).len(), 1);
}

#[test]
fn android_try_again_keeps_the_store() {
    // E: after a failed setup, "Try again" starts the setup again with the
    // store chosen before, instead of asking for the store again.
    let mut f = Fixture::new();
    f.sys.android.installed = true;
    f.sys.android_setup = Some(crate::android::parse_setup_state(
        "step=2/5 Downloading\nresult=failed: Download failed\n",
    ));
    let ids: Vec<RowId> = crate::settings::rows(Category::Android, &f.ctx())
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, [RowId::AndroidState, RowId::AndroidRetry]);
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Android);
    assert_eq!(press(&mut s, &f, &[Down, Ok]), vec![Effect::AndroidSetup]);
    assert!(s.overlay().is_none(), "no store chooser");
}

#[test]
fn kde_connect_device_options() {
    // KDE Connect review: every device row opens its options.
    use crate::kdeconnect::KdeDevice;
    let dev = |paired, reachable, asked| KdeDevice {
        id: "abc".into(),
        name: "Pixel".into(),
        paired,
        reachable,
        pair_requested_by_peer: asked,
    };
    assert_eq!(
        kde_options(&dev(true, true, false)),
        [KdeOption::Ping, KdeOption::Unpair]
    );
    assert_eq!(kde_options(&dev(true, false, false)), [KdeOption::Unpair]);
    assert_eq!(kde_options(&dev(false, true, false)), [KdeOption::Pair]);
    assert_eq!(
        kde_options(&dev(false, true, true)),
        [KdeOption::Accept, KdeOption::Decline]
    );
    let mut f = Fixture::new();
    f.sys.kde = vec![dev(true, true, false)];
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Connect);
    let i = crate::settings::rows(Category::Connect, &f.ctx())
        .iter()
        .position(|r| r.id == RowId::KdeDevice(0))
        .unwrap();
    for _ in 0..i {
        press(&mut s, &f, &[Down]);
    }
    press(&mut s, &f, &[Ok]);
    assert!(matches!(
        s.overlay(),
        Some(Overlay::KdeDevice { device: 0, .. })
    ));
    assert_eq!(
        press(&mut s, &f, &[Down, Ok]),
        vec![Effect::KdeUnpair("abc".into())]
    );
}

#[test]
fn sleep_shows_the_screensaver_while_it_is_on() {
    let mut f = Fixture::new();
    let mut s = Shell::new();
    s.handle_ipc(ShellCommand::Power);
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::Screensaver]);
    f.prefs.screensaver = false;
    s.handle_ipc(ShellCommand::Power);
    assert_eq!(
        press(&mut s, &f, &[Ok]),
        vec![Effect::Power(PowerAction::Suspend)]
    );
}

#[test]
fn volume_panel_takes_the_keys() {
    let f = Fixture::new();
    let mut s = Shell::new();
    s.volume = Some(crate::volume_panel::VolumePanel::default());
    assert_eq!(press(&mut s, &f, &[Up]), vec![Effect::VolumeStep(5)]);
    assert_eq!(press(&mut s, &f, &[Ok]), vec![Effect::ToggleMute]);
    assert!(press(&mut s, &f, &[Back]).is_empty());
    assert!(s.volume.is_none());
    // Home closes it and goes Home as usual.
    s.volume = Some(crate::volume_panel::VolumePanel::default());
    s.handle(Action::Home, &f.ctx());
    assert!(s.volume.is_none());
}

#[test]
fn screensaver_options_grey_out_while_it_is_off() {
    let mut f = Fixture::new();
    let mut s = Shell::new();
    open_settings(&mut s, &f, Category::Customization);
    press(&mut s, &f, &[Down, Down, Down, Down]);
    assert_eq!(
        press(&mut s, &f, &[Right]),
        vec![Effect::SetSaverPictures("photos".into())]
    );
    f.prefs.screensaver = false;
    assert!(press(&mut s, &f, &[Right]).is_empty());
    assert!(press(&mut s, &f, &[Ok]).is_empty());
    // Still passed over to the next row.
    press(&mut s, &f, &[Down]);
    assert_eq!(s.settings_row, 5);
}
