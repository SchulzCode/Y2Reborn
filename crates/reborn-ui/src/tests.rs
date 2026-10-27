//! Product-surface, navigation, focus and wheel-context tests for Product UI v2.
use super::*;
use reborn_core::{
    platform::{
        BatteryState, ChargingState, DiagnosticSection, Fact, LowBattery, PlatformSnapshot, SdCard,
        StorageState, UpdatePhase, UpdateProblem, UpdateState, UsbTransfer, VolumeSpace,
        VolumeState, WifiProblem,
    },
    AppModel, DateField, Effect, PlaybackState, Source,
};
use std::path::PathBuf;

fn library() -> Vec<Track> {
    (0..9)
        .map(|i| Track {
            id: i,
            source_id: "internal".into(),
            title: format!("Song {i}"),
            artist: "Élodie · Μουσική · Музыка · 音楽".into(),
            album: format!("Album {}", i / 3),
            album_artist: "Élodie · Μουσική · Музыка · 音楽".into(),
            path: format!("/data/music/album{}/song{i}.flac", i / 3).into(),
            filename: format!("song{i}.flac"),
            duration_ms: 200_000,
            codec: "flac".into(),
            sample_rate: 44_100,
            online: true,
            ..Default::default()
        })
        .collect()
}

fn snapshot() -> PlatformSnapshot {
    let space = VolumeSpace {
        state: VolumeState::Ready,
        free_bytes: Some(3_200_000_000),
        total_bytes: Some(6_400_000_000),
    };
    PlatformSnapshot {
        observed: true,
        storage: StorageState {
            internal: space,
            sd: SdCard::Absent,
        },
        usb: UsbTransfer::Disconnected,
        update: UpdateState {
            phase: UpdatePhase::UpToDate,
            can_check: true,
            ..Default::default()
        },
        enabled: vec!["ota".into()],
        diagnostics: vec![DiagnosticSection {
            id: "usb",
            title: "USB",
            facts: vec![Fact::new("Address", "10.42.0.1:22")],
        }],
        ..Default::default()
    }
}

fn app(screen: Screen) -> AppModel {
    let mut m = AppModel {
        screen,
        ..Default::default()
    };
    m.library.tracks = library();
    m.replace_queue(library(), 0).unwrap();
    m.sources = vec![Source {
        id: "internal".into(),
        kind: reborn_core::MediaSource::Internal,
        root: "/data/music".into(),
        online: true,
        mount: "internal".into(),
        mount_id: None,
    }];
    m.platform.snapshot = snapshot();
    m.platform.battery = BatteryState {
        percent: Some(72),
        estimated: true,
        charging: ChargingState::Charging,
        level: LowBattery::Normal,
    };
    m.platform.brightness_available = true;
    m
}

fn radios() -> Ui {
    Ui {
        wifi: radio_wifi(),
        bluetooth: radio_bluetooth(),
        ..Default::default()
    }
}

fn radio_wifi() -> WifiView {
    WifiView {
        available: true,
        powered: true,
        status: WifiStatus::Connected("Home".into()),
        networks: vec![
            NetworkView {
                ssid: "Home".into(),
                bars: 3,
                secured: Some(true),
                saved_id: Some(7),
                visible: true,
            },
            NetworkView {
                ssid: "Café".into(),
                bars: 2,
                secured: Some(true),
                saved_id: None,
                visible: true,
            },
        ],
        ..Default::default()
    }
}

fn radio_bluetooth() -> BluetoothView {
    BluetoothView {
        available: true,
        powered: true,
        devices: vec![BluetoothDeviceView {
            path: "/bt/a".into(),
            address: "AA".into(),
            name: "AirPods Pro".into(),
            paired: true,
            bonded: true,
            connected: true,
            audio_ready: true,
            codec: Some("SBC".into()),
            connecting: false,
        }],
        ..Default::default()
    }
}

fn select_key(ui: &mut Ui, m: &mut AppModel, key: &str) -> Effect {
    let tracks = std::mem::take(&mut m.library.tracks);
    let rows = ui.rows(m, &tracks);
    m.library.tracks = tracks;
    m.navigation.focus = rows
        .iter()
        .position(|r| r.key == key)
        .unwrap_or_else(|| panic!("{key} not on {:?}: {rows:?}", m.screen));
    ui.model_action(m, Action::Select)
}

/// The characters a frame draws, recovered from the glyph cells it samples.
pub(crate) fn drawn_text(quads: &[reborn_graphics::Quad]) -> String {
    use reborn_graphics::Tex;
    let faces = [
        text::Face::R11,
        text::Face::R13,
        text::Face::R15,
        text::Face::R17,
        text::Face::L23,
        text::Face::L44,
        text::Face::S10,
        text::Face::S13,
        text::Face::S15,
        text::Face::S17,
        text::Face::S20,
        text::Face::S25,
        text::Face::S28,
        text::Face::S36,
    ];
    let mut cells = std::collections::HashMap::new();
    for face in faces {
        let set = face.set();
        for (ch, x, y, _) in set.glyphs {
            cells.insert((set.page, *x, *y), char::from_u32(*ch).unwrap());
        }
    }
    for (ch, x, y, _) in fontdata::FALLBACK {
        cells.insert((2, *x, *y), char::from_u32(*ch).unwrap());
    }
    quads
        .iter()
        .filter_map(|q| match q.tex {
            Tex::Font(page) => cells.get(&(page, q.src[0], q.src[1])).copied(),
            _ => None,
        })
        .collect()
}

/// Every word a normal user could read on a screen, modal included.
fn visible_text(ui: &Ui, m: &AppModel) -> String {
    let tracks = &m.library.tracks;
    let mut text = String::new();
    let page = pages::page(ui, m, tracks);
    if let Some(h) = &page.hero {
        text += &format!("{} {} ", h.title, h.body);
    }
    for (label, value) in &page.facts {
        text += &format!("{label} {value} ");
    }
    if let Some(s) = &page.status {
        text += s;
    }
    for row in ui.rows(m, tracks) {
        text += &format!(" {} {} ", row.label, row.secondary);
    }
    if m.navigation.modal.is_some() {
        let (title, body) = ui.modal_copy(m);
        text += &format!(" {title} {body} ");
        for row in ui.modal_rows_public(m, tracks) {
            text += &format!(" {} {} ", row.label, row.secondary);
        }
    }
    text
}

const NORMAL_SCREENS: [Screen; 34] = [
    Screen::Home,
    Screen::Music,
    Screen::Albums,
    Screen::Artists,
    Screen::Tracks,
    Screen::Folders,
    Screen::NowPlaying,
    Screen::Queue,
    Screen::TrackInfo,
    Screen::Settings,
    Screen::Wifi,
    Screen::Bluetooth,
    Screen::PcTransfer,
    Screen::SettingsAudio,
    Screen::Equalizer,
    Screen::SettingsPlayback,
    Screen::Crossfade,
    Screen::ScreenTimeout,
    Screen::Output,
    Screen::HeadphoneLevel,
    Screen::DacFilter,
    Screen::ErrorReports,
    Screen::SettingsLibrary,
    Screen::SettingsDisplay,
    Screen::DateTime,
    Screen::SettingsSystem,
    Screen::Battery,
    Screen::Sleep,
    Screen::Storage,
    Screen::Update,
    Screen::About,
    Screen::Radio,
    Screen::RadioStations,
    Screen::RadioCategories,
];

const ENGINEERING: [&str; 30] = [
    "Unavailable",
    "Not available",
    "Not implemented",
    "Coming soon",
    "BlueZ",
    "BlueALSA",
    "PCM",
    "ALSA",
    "kernel",
    "Kernel",
    "mmcblk",
    "UUID",
    "SFTP",
    "SSH",
    "10.42",
    "DMA",
    "cpufreq",
    "CIRQ",
    "SPM",
    "D-Bus",
    "supplicant",
    "manifest",
    "Ed25519",
    "signing",
    "schema",
    "y2-platform",
    "Platform",
    "qualif",
    "readiness",
    "DHCP",
];

#[test]
fn music_holds_the_library_lists_and_back_returns_through_it() {
    let mut ui = radios();
    let mut m = app(Screen::Home);
    let tracks = m.library.tracks.clone();
    m.navigation.focus = 1;
    ui.action(&mut m, &tracks, Action::Select);
    assert_eq!(m.screen, Screen::Music);
    let rows = ui.rows(&m, &tracks);
    let keys: Vec<_> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, ["artists", "albums", "tracks", "genres", "folders"]);
    assert!(rows[0].secondary.ends_with("artists") || rows[0].secondary.ends_with("artist"));
    ui.action(&mut m, &tracks, Action::Select);
    assert_eq!(m.screen, Screen::Artists);
    ui.action(&mut m, &tracks, Action::Back);
    assert_eq!(m.screen, Screen::Music);
    ui.action(&mut m, &tracks, Action::Back);
    assert_eq!(m.screen, Screen::Home);
    assert_eq!(m.navigation.focus, 1);
}

#[test]
fn pinned_music_lists_stand_after_music_on_home() {
    let ui = radios();
    let mut m = app(Screen::Home);
    m.settings.pin_home("genres", true);
    m.settings.pin_home("albums", true);
    let labels: Vec<_> = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(
        labels,
        [
            "Now Playing",
            "Music",
            "Albums",
            "Genres",
            "Playlists",
            "Radio",
            "Search",
            "Settings"
        ]
    );
    // Every state of Home draws with one focus target, eleven rows included.
    for key in ["artists", "tracks", "folders"] {
        m.settings.pin_home(key, true);
    }
    m.navigation.focus = 10;
    let quads = ui.draw(&m, &m.library.tracks, false);
    assert_eq!(crate::focus_target_count(&quads), 1);
}

#[test]
fn home_menu_switches_pin_and_unpin_from_customize() {
    let mut ui = radios();
    let mut m = app(Screen::Customize);
    let tracks = m.library.tracks.clone();
    let rows = ui.rows(&m, &tracks);
    m.navigation.focus = rows.iter().position(|r| r.key == "home_menu").unwrap();
    assert_eq!(rows[m.navigation.focus].secondary, "Standard");
    ui.action(&mut m, &tracks, Action::Select);
    assert_eq!(m.screen, Screen::HomeMenu);
    // Audiobooks first (Beta 8.19), then the Music lists.
    let rows = ui.rows(&m, &tracks);
    assert_eq!(rows[0].key, "home_audiobooks");
    assert_eq!(rows[0].kind, crate::RowKind::Toggle(true));
    m.navigation.focus = 1;
    assert_eq!(
        ui.action(&mut m, &tracks, Action::Select),
        Effect::SetHomePin("artists".into(), true)
    );
    m.settings.pin_home("artists", true);
    assert_eq!(ui.rows(&m, &tracks)[1].kind, crate::RowKind::Toggle(true));
    assert_eq!(
        ui.action(&mut m, &tracks, Action::Select),
        Effect::SetHomePin("artists".into(), false)
    );
}

/// The library plus "Momo", three 60-s parts in a Hörbücher folder.
fn with_book(m: &mut AppModel) {
    for n in 1..=3u32 {
        let mut t = m.library.tracks[0].clone();
        t.id = 100 + n as i64;
        t.title = format!("Teil {n}");
        t.album = "Momo".into();
        t.artist = "Michael Ende".into();
        t.album_artist = "Michael Ende".into();
        t.track = n;
        t.duration_ms = 60_000;
        t.filename = format!("0{n}.mp3");
        t.size = 1000 + n as u64;
        t.path = format!("/data/music/Hörbücher/Momo/0{n}.mp3").into();
        t.book = reborn_core::books::is_book(&t);
        m.library.tracks.push(t);
    }
}

#[test]
fn audiobooks_stand_on_home_once_there_is_a_book_and_can_be_switched_off() {
    let mut ui = radios();
    let mut m = app(Screen::Home);
    let keys = |ui: &Ui, m: &AppModel| -> Vec<String> {
        ui.rows(m, &m.library.tracks)
            .into_iter()
            .map(|r| r.key)
            .collect()
    };
    assert!(
        !keys(&ui, &m).contains(&"audiobooks".to_string()),
        "no book, no entry"
    );
    with_book(&mut m);
    ui.invalidate_catalog();
    assert_eq!(keys(&ui, &m)[..3], ["now_playing", "music", "audiobooks"]);
    m.screen = Screen::HomeMenu;
    assert_eq!(
        select_key(&mut ui, &mut m, "home_audiobooks"),
        Effect::SetAudiobooksOnHome(false)
    );
    m.settings.audiobooks_on_home = false;
    m.screen = Screen::Home;
    assert!(!keys(&ui, &m).contains(&"audiobooks".to_string()));
}

#[test]
fn the_book_list_and_page_follow_progress_and_start_books() {
    use reborn_core::books::{books, Shelf};
    let mut ui = radios();
    let mut m = app(Screen::Audiobooks);
    with_book(&mut m);
    let tracks = m.library.tracks.clone();
    let row = |ui: &Ui, m: &AppModel| ui.rows(m, &tracks)[0].clone();
    let fresh = row(&ui, &m);
    assert_eq!(fresh.label, "Momo");
    assert_eq!(fresh.secondary, "Michael Ende · 3 min");
    assert!(fresh.art.is_none(), "no cover in the fixture");
    // Opening the book asks for its chapter marks.
    let key = fresh.key.strip_prefix("book:").unwrap().to_owned();
    assert_eq!(
        select_key(&mut ui, &mut m, &fresh.key),
        Effect::LoadChapters(key.clone())
    );
    assert_eq!(
        (m.screen, m.navigation.filter.as_str()),
        (Screen::Book, key.as_str())
    );
    let page: Vec<String> = ui.rows(&m, &tracks).into_iter().map(|r| r.label).collect();
    assert_eq!(
        page,
        ["Play", "Teil 1", "Teil 2", "Teil 3"],
        "a new book: Play only"
    );
    // Halfway through part two.
    let book = books(&tracks).remove(0);
    let mut shelf = Shelf::default();
    shelf.record(&book, &tracks, 1, 30_000, 1.0, 1);
    m.library.shelf = shelf;
    let page: Vec<String> = ui.rows(&m, &tracks).into_iter().map(|r| r.label).collect();
    assert_eq!(page[..2], ["Continue", "Start Over"]);
    assert_eq!(
        select_key(&mut ui, &mut m, "chapter:2"),
        Effect::PlayBook(key.clone(), reborn_core::BookStart::Chapter(2))
    );
    assert_eq!(m.screen, Screen::NowPlaying);
    m.screen = Screen::Audiobooks;
    assert_eq!(row(&ui, &m).secondary, "Chapter 2 of 3 · 50 %");
    // The hold menu.
    m.navigation.focus = 0;
    ui.model_action(&mut m, Action::ContextMenu);
    let menu: Vec<String> = ui
        .modal_rows_public(&m, &tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(
        menu,
        [
            "Continue",
            "Start Over",
            "Mark as Finished",
            "Forget Progress"
        ]
    );
    m.navigation.modal_focus = 2;
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::SetBookFinished(key.clone(), true)
    );
    m.library.shelf.set_finished(&book, &tracks, true, 2);
    assert_eq!(row(&ui, &m).secondary, "Finished");
    // Search finds it by author.
    m.screen = Screen::Search;
    m.navigation.filter = "ende".into();
    ui.invalidate_catalog();
    let found = ui.rows(&m, &tracks);
    assert!(
        found
            .iter()
            .any(|r| r.key == format!("book:{key}") && r.secondary == "Michael Ende"),
        "{found:?}"
    );
}

#[test]
fn now_playing_shows_the_chapter_the_book_and_what_is_left() {
    use reborn_core::books::{books, Listening};
    let ui = radios();
    let mut m = app(Screen::NowPlaying);
    with_book(&mut m);
    let tracks = m.library.tracks.clone();
    let book = books(&tracks).remove(0);
    let parts: Vec<Track> = book.parts.iter().map(|&i| tracks[i].clone()).collect();
    m.replace_queue(parts, 1).unwrap();
    m.book = Some(Listening::new(&book, &tracks, 1.0));
    m.position_ms = 30_000;
    let text = drawn_text(&ui.draw(&m, &tracks, false));
    for expected in [
        "Teil 2",
        "Momo",
        "Michael Ende",
        "50 % · 1 min left",
        "0:30",
        "-0:30",
    ] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    // Its hold menu offers the chapters, not shuffle or repeat.
    let mut ui = ui;
    ui.model_action(&mut m, Action::ContextMenu);
    let menu: Vec<String> = ui
        .modal_rows_public(&m, &tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(menu, ["Chapters", "Speed", "Sleep Timer", "Up Next"]);
    m.navigation.modal_focus = 1;
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::SetBookSpeed(110)
    );
    assert!(
        m.navigation.modal.is_some(),
        "the menu stays open for the next step"
    );
    m.book.as_mut().unwrap().speed = 1.1;
    let rows = ui.modal_rows_public(&m, &tracks);
    assert_eq!(rows[1].secondary, "1.1×");
}

#[test]
fn the_sleep_timer_is_picked_with_the_wheel_and_shows_on_now_playing() {
    use reborn_core::SleepChoice;
    let mut ui = radios();
    let mut m = app(Screen::NowPlaying);
    let tracks = m.library.tracks.clone();
    ui.model_action(&mut m, Action::ContextMenu);
    let rows = ui.modal_rows_public(&m, &tracks);
    let at = rows.iter().position(|r| r.key == "sleep_timer").unwrap();
    assert_eq!(rows[at].secondary, "Off");
    // #29: Select opens the lengths, on 30 min while the timer is off.
    m.navigation.modal_focus = at;
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    assert_eq!(m.navigation.modal, Some(Modal::SleepTimer));
    let picker: Vec<String> = ui
        .modal_rows_public(&m, &tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(
        picker,
        [
            "Off",
            "End of Song",
            "5 min",
            "10 min",
            "15 min",
            "20 min",
            "30 min",
            "45 min",
            "1 h",
            "1 h 15 min",
            "1 h 30 min",
            "2 h",
            "2 h 30 min",
            "3 h"
        ]
    );
    assert_eq!(picker[m.navigation.modal_focus], "30 min");
    // The wheel stops at the ends; Select starts the length chosen.
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::SetSleepTimer(SleepChoice::Minutes(45))
    );
    assert_eq!(m.navigation.modal, None);
    // Running: the picker opens on it and says how long it has left.
    m.sleep_timer = Some(reborn_core::SleepTimer {
        choice: SleepChoice::Minutes(45),
        until: Some(Instant::now() + Duration::from_secs(14 * 60 + 30)),
        item: m.sleep_item(),
    });
    ui.model_action(&mut m, Action::ContextMenu);
    assert_eq!(ui.modal_rows_public(&m, &tracks)[at].secondary, "15 min");
    m.navigation.modal_focus = at;
    ui.model_action(&mut m, Action::Select);
    let rows = ui.modal_rows_public(&m, &tracks);
    assert_eq!(rows[m.navigation.modal_focus].label, "45 min");
    assert_eq!(rows[m.navigation.modal_focus].secondary, "15 min left");
    // Back leaves it running.
    assert_eq!(ui.model_action(&mut m, Action::Back), Effect::None);
    assert_eq!(m.navigation.modal, None);
    let text = drawn_text(&ui.draw(&m, &tracks, false));
    assert!(text.contains("Sleeps in 15 min"), "{text}");
    m.sleep_timer = Some(reborn_core::SleepTimer {
        choice: SleepChoice::EndOfItem,
        until: None,
        item: m.sleep_item(),
    });
    let text = drawn_text(&ui.draw(&m, &tracks, false));
    assert!(text.contains("Sleeps after song"), "{text}");
    assert_eq!(
        crate::sleep_choice_label(SleepChoice::Minutes(150), false),
        "2 h 30 min"
    );
    assert_eq!(
        crate::sleep_choice_label(SleepChoice::EndOfItem, true),
        "End of Chapter"
    );
    // Long times read in hours on Now Playing too, not "120 min".
    m.sleep_timer = Some(reborn_core::SleepTimer {
        choice: SleepChoice::Minutes(120),
        until: Some(Instant::now() + Duration::from_secs(119 * 60 + 30)),
        item: m.sleep_item(),
    });
    let text = drawn_text(&ui.draw(&m, &tracks, false));
    assert!(text.contains("Sleeps in 2 h"), "{text}");
    assert_eq!(crate::minutes_label(119), "1 h 59 min");
}

#[test]
fn times_from_an_hour_on_show_hours() {
    assert_eq!(components::time(61_000), "1:01");
    assert_eq!(components::time(3_600_000 + 62_000), "1:01:02");
}

#[test]
fn audiobooks_stay_out_of_the_music_lists_counts_and_search() {
    let ui = radios();
    let mut m = app(Screen::Tracks);
    let mut book = m.library.tracks[0].clone();
    book.id = 100;
    book.title = "Kapitel Eins".into();
    book.album = "Momo".into();
    book.artist = "Michael Ende".into();
    book.album_artist = "Michael Ende".into();
    book.path = "/data/music/Hörbücher/Momo/01.mp3".into();
    book.book = reborn_core::books::is_book(&book);
    assert!(book.book);
    m.library.tracks.push(book);
    ui.invalidate_catalog();
    let tracks = m.library.tracks.clone();
    let text = |ui: &Ui, m: &AppModel| {
        ui.rows(m, &tracks)
            .iter()
            .map(|r| format!("{} {}", r.label, r.secondary))
            .collect::<Vec<_>>()
            .join("\n")
    };
    for screen in [
        Screen::Tracks,
        Screen::Albums,
        Screen::Artists,
        Screen::Folders,
    ] {
        m.screen = screen;
        ui.invalidate_catalog();
        let shown = text(&ui, &m);
        assert!(
            !shown.contains("Momo") && !shown.contains("Kapitel"),
            "{screen:?}: {shown}"
        );
        assert!(
            !shown.contains("Michael Ende") && !shown.contains("Hörbücher"),
            "{screen:?}"
        );
    }
    m.screen = Screen::Music;
    assert!(text(&ui, &m).contains("9 songs"), "{}", text(&ui, &m));
    m.screen = Screen::Search;
    m.navigation.filter = "Momo".into();
    ui.invalidate_catalog();
    assert!(!text(&ui, &m).contains("Kapitel"));
    // Books are not scrobbled.
    assert!(reborn_core::scrobble::play_of(&tracks[9]).is_none());
}

#[test]
fn root_is_a_small_music_player_menu() {
    let ui = radios();
    let m = app(Screen::Home);
    let labels: Vec<_> = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(
        labels,
        [
            "Now Playing",
            "Music",
            "Playlists",
            "Radio",
            "Search",
            "Settings"
        ]
    );
    // Power, Quick Settings and diagnostics have no place on Home.
    assert!(!ui
        .rows(&m, &m.library.tracks)
        .iter()
        .any(|r| r.key.contains("power")));
}

#[test]
fn normal_screens_never_show_engineering_terms_or_generic_unavailable() {
    let mut cases = vec![];
    for screen in NORMAL_SCREENS.into_iter().chain([Screen::Maintenance]) {
        cases.push((screen, radios(), app(screen)));
    }
    // Problem, absent and off states as well.
    let mut off = radios();
    off.wifi.powered = false;
    off.bluetooth.powered = false;
    cases.push((Screen::Wifi, off.clone_view(), app(Screen::Wifi)));
    cases.push((Screen::Bluetooth, off, app(Screen::Bluetooth)));
    let mut failed = radios();
    failed.wifi.problem = Some(WifiProblem::NoAddress);
    cases.push((Screen::Wifi, failed, app(Screen::Wifi)));
    let mut starting = Ui::default();
    starting.wifi.available = false;
    cases.push((Screen::Wifi, starting, app(Screen::Wifi)));
    let mut stale = app(Screen::Update);
    stale.platform.snapshot = PlatformSnapshot::default();
    cases.push((Screen::Update, radios(), stale));
    let mut problem = app(Screen::Update);
    problem.platform.snapshot.update.phase = UpdatePhase::Failed(UpdateProblem::NoUpdateSource);
    cases.push((Screen::Update, radios(), problem));
    let mut confirm = app(Screen::Home);
    confirm.navigation.modal = Some(Modal::Confirm(reborn_core::ConfirmAction::PowerOff));
    cases.push((Screen::Home, radios(), confirm));
    let mut missing_song = app(Screen::TrackInfo);
    missing_song.navigation.filter = "999".into();
    missing_song.queue.clear();
    cases.push((Screen::TrackInfo, radios(), missing_song));
    for (screen, ui, m) in cases {
        let mut text = visible_text(&ui, &m);
        if is_empty_catalog(&ui, &m) {
            text += &super::views_library::empty_catalog_copy(&m);
        }
        for term in ENGINEERING {
            assert!(!text.contains(term), "{screen:?} shows {term:?}: {text}");
        }
    }
}

fn is_empty_catalog(ui: &Ui, m: &AppModel) -> bool {
    matches!(
        m.screen,
        Screen::Albums | Screen::Artists | Screen::Tracks | Screen::Folders | Screen::Queue
    ) && ui.row_count(m, &m.library.tracks) == 0
}

impl Ui {
    fn clone_view(&self) -> Ui {
        Ui {
            wifi: self.wifi.clone(),
            bluetooth: self.bluetooth.clone(),
            ..Default::default()
        }
    }
}

#[test]
fn empty_library_states_name_the_problem_without_generic_unavailable() {
    for setup in [
        |m: &mut AppModel| m.sources.clear(),
        |m: &mut AppModel| m.library.scanning = true,
        |m: &mut AppModel| m.library.error = Some("x".into()),
        |_: &mut AppModel| {},
    ] {
        let mut m = app(Screen::Albums);
        m.library.tracks.clear();
        setup(&mut m);
        let copy = super::views_library::empty_catalog_copy(&m);
        for term in ENGINEERING {
            assert!(!copy.contains(term), "{copy}");
        }
    }
}

#[test]
fn diagnostics_is_the_only_technical_subtree_and_lives_under_system() {
    let mut ui = radios();
    let mut m = app(Screen::Settings);
    select_key(&mut ui, &mut m, "update");
    assert_eq!(m.screen, Screen::Update);
    // Only a deliberate run of taps on the version opens the service menu.
    for _ in 0..6 {
        select_key(&mut ui, &mut m, "version");
        assert_eq!(m.screen, Screen::Update);
    }
    select_key(&mut ui, &mut m, "version");
    assert_eq!(m.screen, Screen::SettingsSystem);
    select_key(&mut ui, &mut m, "diagnostics");
    assert_eq!(m.screen, Screen::Diagnostics);
    select_key(&mut ui, &mut m, "diag:usb");
    assert_eq!(m.screen, Screen::DiagnosticSection);
    let text = visible_text(&ui, &m);
    assert!(
        text.contains("10.42.0.1"),
        "technical facts stay available here"
    );
    // Not reachable from root or Settings directly.
    for screen in [Screen::Home, Screen::Settings] {
        let m = app(screen);
        assert!(!ui
            .rows(&m, &m.library.tracks)
            .iter()
            .any(|r| r.key == "diagnostics"));
    }
}

#[test]
fn storage_offers_the_computer_page_only_while_a_cable_is_in() {
    let ui = radios();
    let mut m = app(Screen::Storage);
    m.platform.snapshot.observed = true;
    let has = |m: &AppModel| {
        ui.rows(m, &m.library.tracks)
            .iter()
            .any(|r| r.key == "pc_transfer")
    };
    m.platform.snapshot.usb = UsbTransfer::Disconnected;
    assert!(!has(&m));
    m.platform.snapshot.usb = UsbTransfer::Ready;
    assert!(has(&m));
}

#[test]
fn every_normal_route_is_reachable_and_back_restores_parent_focus() {
    let mut ui = radios();
    let mut seen = std::collections::BTreeSet::new();
    let mut frontier = vec![(Screen::Home, String::new())];
    while let Some((screen, filter)) = frontier.pop() {
        if !seen.insert(format!("{screen:?}{filter}")) || seen.len() > 200 {
            continue;
        }
        let mut m = app(screen);
        m.navigation.filter = filter.clone();
        let rows = ui.rows(&m, &m.library.tracks);
        for (i, row) in rows.iter().enumerate() {
            if !row.enabled || route(&row.key).is_none() && !row.key.starts_with("diag:") {
                continue;
            }
            let mut child = app(screen);
            child.navigation.filter = filter.clone();
            child.navigation.focus = i;
            ui.model_action(&mut child, Action::Select);
            assert_ne!(child.screen, screen, "{:?} → {}", screen, row.key);
            frontier.push((child.screen, child.navigation.filter.clone()));
            ui.model_action(&mut child, Action::Back);
            assert_eq!(child.screen, screen, "Back from {}", row.key);
            assert_eq!(
                child.navigation.focus, i,
                "focus restored after {}",
                row.key
            );
        }
    }
    // Everything a listener uses is reachable from Home by rows alone; the
    // service pages, the queue (from Now Playing) and song info (from a
    // sheet) are not part of the walk.
    for screen in [
        Screen::Home,
        Screen::Albums,
        Screen::Artists,
        Screen::Tracks,
        Screen::Folders,
        Screen::NowPlaying,
        Screen::Settings,
        Screen::Wifi,
        Screen::Bluetooth,
        Screen::SettingsAudio,
        Screen::Equalizer,
        Screen::Crossfade,
        Screen::SettingsDisplay,
        Screen::ScreenTimeout,
        Screen::Storage,
        Screen::Update,
    ] {
        assert!(
            seen.iter().any(|s| s.starts_with(&format!("{screen:?}"))),
            "{screen:?} unreachable"
        );
    }
}

#[test]
fn album_back_returns_to_the_same_album_offset_and_focus() {
    let mut ui = radios();
    let mut m = app(Screen::Albums);
    ui.model_action(&mut m, Action::WheelClockwise(1));
    ui.model_action(&mut m, Action::WheelClockwise(1));
    let (focus, scroll) = (m.navigation.focus, m.navigation.scroll);
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::Album);
    ui.model_action(&mut m, Action::WheelClockwise(1));
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.screen, Screen::Albums);
    assert_eq!((m.navigation.focus, m.navigation.scroll), (focus, scroll));
}

#[test]
fn every_interactive_screen_draws_exactly_one_focus() {
    let ui = radios();
    let static_pages = [Screen::TrackInfo, Screen::Battery];
    for screen in NORMAL_SCREENS.into_iter().chain([
        Screen::Maintenance,
        Screen::Diagnostics,
        Screen::DiagnosticSection,
    ]) {
        let mut m = app(screen);
        if screen == Screen::DiagnosticSection {
            m.navigation.filter = "usb".into();
        }
        let tracks = std::mem::take(&mut m.library.tracks);
        ui.normalize(&mut m, &tracks);
        m.library.tracks = tracks;
        let quads = ui.draw(&m, &m.library.tracks, false);
        let expected = usize::from(!static_pages.contains(&screen));
        assert_eq!(focus_target_count(&quads), expected, "{screen:?}");
        for q in &quads {
            assert!(q.x.is_finite() && q.y.is_finite() && q.w >= 0. && q.h >= 0.);
            if let reborn_graphics::Tex::Font(page) = q.tex {
                let [x, y, w, h] = q.src.map(usize::from);
                assert!(x + w <= 1024 && y + h <= fontdata::PAGE_HEIGHTS[usize::from(page)]);
            }
        }
    }
}

#[test]
fn modals_trap_focus_and_show_one_focus_over_the_screen() {
    let mut ui = radios();
    let mut m = app(Screen::Tracks);
    // Ends hold here; wrap_around_continues_menus_at_the_other_end covers wrapping.
    m.settings.wrap_around = WrapAround::Off;
    ui.model_action(&mut m, Action::ContextMenu);
    assert_eq!(m.navigation.modal, Some(Modal::ContextMenu));
    for _ in 0..20 {
        ui.model_action(&mut m, Action::WheelClockwise(4));
    }
    let rows = ui.modal_rows_public(&m, &m.library.tracks);
    assert_eq!(m.navigation.modal_focus, rows.len() - 1);
    assert_eq!(m.navigation.focus, 0, "background focus untouched");
    assert_eq!(
        focus_target_count(&ui.draw(&m, &m.library.tracks, false)),
        1
    );
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.navigation.modal, None);
    assert_eq!(m.screen, Screen::Tracks);
}

#[test]
fn power_hold_fills_a_ring_and_release_cancels_it_without_any_menu() {
    let mut ui = radios();
    let mut m = app(Screen::NowPlaying);
    assert_eq!(ui.model_action(&mut m, Action::PowerHold), Effect::None);
    assert_eq!(m.navigation.modal, None, "no Power menu, no Quick Settings");
    assert!(ui.power_hold_progress().is_some_and(|p| p < 0.5));
    assert_eq!(
        ui.power_hold_complete(),
        None,
        "not before the ring is full"
    );
    ui.power_hold = Some(Instant::now() - POWER_HOLD / 3);
    let quads = ui.draw(&m, &m.library.tracks, false);
    assert!(drawn_text(&quads).contains("Hold to Turn Off"));
    ui.model_action(&mut m, Action::PowerHoldCancel);
    assert_eq!(ui.power_hold_progress(), None);
    assert!(!drawn_text(&ui.draw(&m, &m.library.tracks, false)).contains("Hold to Turn Off"));
    // Held to the end it turns the player off, once.
    ui.model_action(&mut m, Action::PowerHold);
    ui.power_hold = Some(Instant::now() - POWER_HOLD);
    assert_eq!(ui.power_hold_complete(), Some(Effect::PowerOff));
    assert_eq!(ui.power_hold_complete(), None);
}

#[test]
fn settings_switches_toggle_in_place() {
    let mut ui = radios();
    let mut m = app(Screen::Wifi);
    assert_eq!(select_key(&mut ui, &mut m, "wifi_power"), Effect::WifiPower);
    assert_eq!(m.navigation.modal, None);
    let mut m = app(Screen::Bluetooth);
    assert_eq!(
        select_key(&mut ui, &mut m, "bt_power"),
        Effect::BluetoothPower
    );
    let mut m = app(Screen::SettingsAudio);
    assert_eq!(
        select_key(&mut ui, &mut m, "gapless"),
        Effect::SetGapless(false)
    );
    assert_eq!(
        select_key(&mut ui, &mut m, "replay_gain"),
        Effect::SetReplayGain(reborn_core::ReplayGainMode::Track)
    );
    m.platform.brightness_available = false;
    let m = app(Screen::SettingsDisplay);
    assert!(ui
        .rows(&m, &m.library.tracks)
        .iter()
        .any(|r| r.key == "screen_timeout"));
}

#[test]
fn time_zone_opens_at_the_chosen_zone_and_the_status_bar_shows_its_time() {
    let mut ui = radios();
    // 2026-10-12 09:41 UTC.
    ui.clock_fixed = Some(1_791_798_060);
    let mut m = app(Screen::SettingsDisplay);
    let row = |ui: &Ui, m: &AppModel| {
        ui.rows(m, &m.library.tracks)
            .into_iter()
            .find(|r| r.key == "time_zone")
            .unwrap()
    };
    // Never chosen: found over Wi-Fi (Beta 7.1).
    assert_eq!(row(&ui, &m).secondary, "Automatic");
    let draw = ui.draw(&m, &m.library.tracks, false);
    assert!(drawn_text(&draw).contains("09:41"));

    m.settings.time_zone = "Asia/Tokyo".into();
    m.settings.time_zone_auto = Some(false);
    assert_eq!(row(&ui, &m).secondary, "Tokyo");
    select_key(&mut ui, &mut m, "time_zone");
    assert_eq!(m.screen, Screen::TimeZone);
    let rows = ui.rows(&m, &m.library.tracks);
    let focused = &rows[m.navigation.focus];
    assert_eq!(focused.key, "tz_country:JP");
    assert_eq!(focused.kind, RowKind::Check(true));
    assert_eq!(focused.secondary, "UTC+9");
    assert_eq!(
        select_key(&mut ui, &mut m, "tz_auto"),
        Effect::SetTimeZoneAuto(true)
    );
    // A country of one zone sets it; one of several opens its cities.
    assert_eq!(
        select_key(&mut ui, &mut m, "tz_country:CH"),
        Effect::SetTimeZone("Europe/Zurich".into())
    );
    // Berlin and Büsingen keep the same time: Germany needs no city.
    assert_eq!(
        select_key(&mut ui, &mut m, "tz_country:DE"),
        Effect::SetTimeZone("Europe/Berlin".into())
    );
    select_key(&mut ui, &mut m, "tz_country:US");
    assert_eq!(m.screen, Screen::TimeZone);
    assert!(drawn_text(&ui.draw(&m, &m.library.tracks, false)).contains("United States"));
    assert_eq!(
        select_key(&mut ui, &mut m, "tz_set:America/Chicago"),
        Effect::SetTimeZone("America/Chicago".into())
    );
    m.settings.time_zone = "America/Chicago".into();
    let draw = ui.draw(&m, &m.library.tracks, false);
    assert!(drawn_text(&draw).contains("4:41"), "12-hour, daylight time");
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.screen, Screen::TimeZone);
    assert!(m.navigation.filter.is_empty(), "back to the countries");
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.screen, Screen::SettingsDisplay);
}

#[test]
fn date_and_time_are_set_by_hand_from_display() {
    // #53: a player that never had Wi-Fi.
    let mut ui = radios();
    // 2026-10-12 09:41 UTC, 11:41 in Berlin.
    ui.clock_fixed = Some(1_791_798_060);
    let mut m = app(Screen::SettingsDisplay);
    m.settings.time_zone = "Europe/Berlin".into();
    m.settings.time_zone_auto = Some(false);
    m.platform.snapshot.info.build_epoch = Some(1_791_000_000);
    let row = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .find(|r| r.key == "date_time")
        .unwrap();
    assert_eq!(row.secondary, "12 Oct 2026, 11:41");
    select_key(&mut ui, &mut m, "date_time");
    assert_eq!(m.screen, Screen::DateTime);
    let draw = drawn_text(&ui.draw(&m, &m.library.tracks, false));
    assert!(
        draw.contains("Date & Time") && draw.contains("October"),
        "{draw}"
    );
    // A field opens its picker at the current value; the wheel steps it.
    select_key(&mut ui, &mut m, "date_field:hour");
    assert_eq!(m.navigation.modal, Some(Modal::DateField(DateField::Hour)));
    assert_eq!(m.navigation.modal_focus, 11);
    for _ in 0..3 {
        ui.model_action(&mut m, Action::WheelClockwise(1));
    }
    // Time: the hour, then the minute.
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    assert_eq!(
        m.navigation.modal,
        Some(Modal::DateField(DateField::Minute))
    );
    assert_eq!(m.navigation.modal_focus, 41);
    ui.model_action(&mut m, Action::WheelCounterClockwise(1));
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.navigation.modal, None);
    // Back leaves a picker without a change.
    select_key(&mut ui, &mut m, "date_field:day");
    ui.model_action(&mut m, Action::WheelClockwise(1));
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.screen, Screen::DateTime);
    let values: Vec<_> = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.secondary)
        .collect();
    assert_eq!(values, ["2026", "October", "12", "14:40", ""]);
    // Busy with another task: nothing is sent.
    m.platform.busy = Some(PlatformTask::Refresh);
    assert_eq!(select_key(&mut ui, &mut m, "date_set"), Effect::None);
    assert_eq!(m.screen, Screen::DateTime);
    m.platform.busy = None;
    // 14:40 in Berlin (summer time) is 12:40 UTC; back on Display.
    assert_eq!(
        select_key(&mut ui, &mut m, "date_set"),
        Effect::Platform(PlatformTask::SetClock(1_791_763_200 + 12 * 3600 + 40 * 60))
    );
    assert_eq!(m.screen, Screen::SettingsDisplay);
    // Before the build date: refused here already.
    select_key(&mut ui, &mut m, "date_time");
    select_key(&mut ui, &mut m, "date_field:month");
    m.navigation.modal_focus = 0;
    ui.model_action(&mut m, Action::Select);
    assert_eq!(select_key(&mut ui, &mut m, "date_set"), Effect::None);
    assert_eq!(ui.notice, "Choose a later date.");
}

#[test]
fn sound_pickers_set_exact_values_and_show_the_current_one() {
    let mut ui = radios();
    let mut m = app(Screen::Crossfade);
    m.settings.crossfade_ms = 4_000;
    let rows = ui.rows(&m, &m.library.tracks);
    let checked: Vec<_> = rows
        .iter()
        .filter(|r| r.kind == RowKind::Check(true))
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(checked, ["4 seconds"]);
    assert_eq!(
        select_key(&mut ui, &mut m, "crossfade_set:12000"),
        Effect::SetCrossfade(12_000)
    );
    let mut m = app(Screen::ScreenTimeout);
    assert_eq!(
        select_key(&mut ui, &mut m, "timeout_set:0"),
        Effect::SetScreenTimeout(0)
    );
    let mut m = app(Screen::Equalizer);
    assert_eq!(
        select_key(&mut ui, &mut m, "eq_preset:bass"),
        Effect::SetEqPreset([6, 4, 0, 0, 0])
    );
    assert_eq!(
        select_key(&mut ui, &mut m, "eq_preset:flat"),
        Effect::SetEqPreset([0; 5])
    );
    // The preset in use is recognised from the stored gains.
    m.settings.eq_enabled = true;
    m.settings.eq_bands = reborn_core::flat_eq_bands();
    m.settings.eq_bands[0].gain_db = 6.;
    m.settings.eq_bands[1].gain_db = 4.;
    assert_eq!(pages::eq_preset_name(&m), "Bass Boost");
    m.settings.eq_bands[2].gain_db = 1.;
    assert_eq!(pages::eq_preset_name(&m), "Custom");
}

#[test]
fn headphone_level_and_dac_filter_pickers_show_and_set_the_choice() {
    let mut ui = radios();
    // Sound fits about one screen: the jack's settings sit on the Output page.
    let mut m = app(Screen::SettingsAudio);
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(rows.len(), 6);
    assert_eq!(pages::jack_summary(&m), "Level Low · Filter Fast");
    assert_eq!(select_key(&mut ui, &mut m, "output_page"), Effect::None);
    assert_eq!(m.screen, Screen::Output);
    let rows = ui.rows(&m, &m.library.tracks);
    let row = |label: &str| rows.iter().find(|r| r.label == label).unwrap().clone();
    assert_eq!(row("Headphone Jack").kind, RowKind::Check(true));
    // #28: the jack output's speaker, on by default; its name when it plays.
    assert_eq!(row("Speaker").kind, RowKind::Toggle(true));
    assert_eq!(pages::output_name(&ui, &m), "Headphone Jack");
    m.speaker_active = true;
    assert_eq!(pages::output_name(&ui, &m), "Speaker");
    m.speaker_active = false;
    assert_eq!(
        select_key(&mut ui, &mut m, "speaker"),
        Effect::SetSpeaker(false)
    );
    assert_eq!(row("Headphone Level").secondary, "Low");
    assert_eq!(row("DAC Filter").secondary, "Fast");
    assert_eq!(
        select_key(&mut ui, &mut m, "output_set:wired"),
        Effect::Output(AudioOutput::Wired)
    );
    let mut m = app(Screen::HeadphoneLevel);
    m.settings.hp_level = reborn_core::HpLevel::Medium;
    let checked: Vec<_> = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .filter(|r| r.kind == RowKind::Check(true))
        .map(|r| r.label)
        .collect();
    assert_eq!(checked, ["Medium"]);
    assert_eq!(
        select_key(&mut ui, &mut m, "hp_level_set:high"),
        Effect::SetHpLevel(reborn_core::HpLevel::High)
    );
    let mut m = app(Screen::DacFilter);
    let labels: Vec<_> = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(labels.len(), 5);
    assert_eq!(
        select_key(&mut ui, &mut m, "dac_filter_set:slow_phase"),
        Effect::SetDacFilter(reborn_core::DacFilter::SlowPhaseCompensated)
    );
}

#[test]
fn headphone_profiles_are_listed_imported_and_shown_instead_of_presets() {
    let mut ui = radios();
    let mut m = app(Screen::Equalizer);
    assert_eq!(
        select_key(&mut ui, &mut m, "eq_profile"),
        Effect::ScanEqProfiles
    );
    assert_eq!(
        (m.screen, m.navigation.filter.as_str()),
        (Screen::Equalizer, "profile")
    );
    ui.eq_profiles = vec![
        reborn_core::eq::ProfileFile {
            name: "HD 650".into(),
            path: "/m/EQ/HD 650 ParametricEQ.txt".into(),
        },
        reborn_core::eq::ProfileFile {
            name: "Aria".into(),
            path: "/m/EQ/Aria.txt".into(),
        },
    ];
    assert_eq!(
        select_key(&mut ui, &mut m, "eq_profile_file:1"),
        Effect::ImportEqProfile("/m/EQ/Aria.txt".into())
    );
    // Playing a profile: checked in the list, named on Sound, presets clear.
    let profile =
        reborn_core::eq::parse("HD 650", "Filter 1: ON PK Fc 100 Hz Gain 3 dB Q 1\n").unwrap();
    m.settings.eq_profile = Some(profile);
    m.settings.eq_profile_on = true;
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(rows[0].kind, RowKind::Check(true));
    assert_eq!(rows[1].kind, RowKind::Check(false));
    assert_eq!(pages::eq_summary(&m), "HD 650");
    // Its file removed: still listed as saved.
    ui.eq_profiles.clear();
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(
        (rows[0].label.as_str(), rows[0].secondary.as_str()),
        ("HD 650", "Saved")
    );
    m.navigation.filter.clear();
    let rows = ui.rows(&m, &m.library.tracks);
    let checked: Vec<_> = rows
        .iter()
        .filter(|r| r.kind == RowKind::Check(true))
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(checked, ["Headphone Profile"]);
    m.settings.eq_profile_on = false;
    assert_eq!(pages::eq_summary(&m), "Flat");
}

#[test]
fn wheel_one_detent_is_one_step_outside_long_library_lists() {
    let mut ui = radios();
    // Settings, queue, modal and short lists ignore acceleration. (Up Next
    // steps over its "Next from" heading.)
    for screen in [Screen::Settings, Screen::Queue, Screen::Tracks] {
        let mut m = app(screen);
        ui.model_action(&mut m, Action::WheelClockwise(4));
        let one = if screen == Screen::Queue { 2 } else { 1 };
        assert_eq!(m.navigation.focus, one, "{screen:?}");
        ui.model_action(&mut m, Action::WheelCounterClockwise(3));
        assert_eq!(m.navigation.focus, 0, "{screen:?}");
    }
    let mut m = app(Screen::Tracks);
    m.navigation.modal = Some(Modal::ContextMenu);
    m.navigation.context_key = Some("track:0".into());
    ui.model_action(&mut m, Action::WheelClockwise(4));
    assert_eq!(m.navigation.modal_focus, 1);
    // Long library lists accept the input layer's sustained-rotation steps.
    let tracks: Vec<_> = (0..500)
        .map(|i| Track {
            id: i,
            title: format!("T{i:03}"),
            online: true,
            ..Default::default()
        })
        .collect();
    let mut m = app(Screen::Tracks);
    m.library.tracks = tracks;
    ui.invalidate_catalog();
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(m.navigation.focus, 1, "isolated detent");
    ui.model_action(&mut m, Action::WheelClockwise(4));
    assert_eq!(m.navigation.focus, 5, "sustained rotation");
}

#[test]
fn customize_holds_how_reborn_behaves() {
    let mut ui = radios();
    let mut m = app(Screen::Settings);
    let tracks = m.library.tracks.clone();
    let row =
        |ui: &Ui, m: &AppModel, key: &str| ui.rows(m, &tracks).into_iter().find(|r| r.key == key);
    assert_eq!(row(&ui, &m, "customize").unwrap().secondary, "Wrap: Menus");
    select_key(&mut ui, &mut m, "customize");
    assert_eq!(m.screen, Screen::Customize);
    let keys: Vec<String> = ui.rows(&m, &tracks).into_iter().map(|r| r.key).collect();
    assert_eq!(
        keys,
        [
            "home_menu",
            "album_view",
            "artists_list",
            "now_playing_wheel",
            "wrap_around",
            "screen_off_buttons",
            "haptics"
        ]
    );
    let labels: Vec<String> = ui.rows(&m, &tracks).into_iter().map(|r| r.label).collect();
    assert_eq!(
        labels,
        [
            "Home Menu",
            "Cover Flow",
            "Artists",
            "Now Playing Wheel",
            "Wrap Around",
            "When Screen Off",
            "Haptics"
        ]
    );
    // #28: Off by default, Select steps Light, Strong, Off.
    assert_eq!(row(&ui, &m, "haptics").unwrap().secondary, "Off");
    assert_eq!(
        select_key(&mut ui, &mut m, "haptics"),
        Effect::SetHaptics(reborn_core::Haptics::Light)
    );
    m.settings.haptics = reborn_core::Haptics::Strong;
    assert_eq!(
        select_key(&mut ui, &mut m, "haptics"),
        Effect::SetHaptics(reborn_core::Haptics::Off)
    );
    m.settings.haptics = reborn_core::Haptics::Off;
    assert_eq!(row(&ui, &m, "artists_list").unwrap().secondary, "All");
    assert_eq!(
        select_key(&mut ui, &mut m, "artists_list"),
        Effect::SetArtistsList(reborn_core::ArtistsList::AlbumArtists)
    );
    assert_eq!(
        row(&ui, &m, "now_playing_wheel").unwrap().secondary,
        "Volume"
    );
    assert_eq!(
        select_key(&mut ui, &mut m, "now_playing_wheel"),
        Effect::SetNowPlayingWheel(reborn_core::NowPlayingWheel::Seek)
    );
    assert_eq!(
        row(&ui, &m, "screen_off_buttons").unwrap().secondary,
        "Buttons"
    );
    assert_eq!(
        select_key(&mut ui, &mut m, "screen_off_buttons"),
        Effect::SetScreenOffButtons(reborn_core::ScreenOffButtons::WheelAndButtons)
    );
    m.settings.screen_off_buttons = reborn_core::ScreenOffButtons::WheelAndButtons;
    assert_eq!(
        select_key(&mut ui, &mut m, "screen_off_buttons"),
        Effect::SetScreenOffButtons(reborn_core::ScreenOffButtons::Locked)
    );
    assert_eq!(row(&ui, &m, "wrap_around").unwrap().secondary, "Menus");
    assert_eq!(
        select_key(&mut ui, &mut m, "wrap_around"),
        Effect::SetWrapAround(WrapAround::Everywhere)
    );
    m.settings.wrap_around = WrapAround::Everywhere;
    assert_eq!(
        select_key(&mut ui, &mut m, "wrap_around"),
        Effect::SetWrapAround(WrapAround::Off)
    );
    // Display keeps how the screen looks; the behaviour moved here.
    m.screen = Screen::SettingsDisplay;
    for key in ["home_menu", "album_view"] {
        assert!(row(&ui, &m, key).is_none(), "{key} left Display");
    }
    // Settings draws every place, Customize included, without scrolling.
    m.screen = Screen::Settings;
    m.navigation.focus = ui.rows(&m, &tracks).len() - 1;
    assert!(visible_text(&ui, &m).contains("About & Updates"));
    assert!(visible_text(&ui, &m).contains("Customize"));
}

#[test]
fn wrap_around_continues_menus_at_the_other_end() {
    let mut ui = radios();
    let mut m = app(Screen::Settings);
    let tracks = m.library.tracks.clone();
    let last = ui.rows(&m, &tracks).len() - 1;
    // Menus (the default): Settings wraps both ways.
    assert_eq!(m.settings.wrap_around, WrapAround::Menus);
    ui.model_action(&mut m, Action::WheelCounterClockwise(1));
    assert_eq!(m.navigation.focus, last, "before the first: the last");
    ui.model_action(&mut m, Action::NavigateDown);
    assert_eq!(m.navigation.focus, 0, "past the last: the first");
    // A context menu too.
    let mut menu = app(Screen::Tracks);
    menu.navigation.modal = Some(Modal::ContextMenu);
    menu.navigation.context_key = Some("track:0".into());
    ui.model_action(&mut menu, Action::NavigateUp);
    assert!(menu.navigation.modal_focus > 0, "context menu wrapped");
    // Library lists keep their ends unless Everywhere.
    let mut songs = app(Screen::Tracks);
    ui.model_action(&mut songs, Action::NavigateUp);
    assert_eq!(songs.navigation.focus, 0);
    songs.settings.wrap_around = WrapAround::Everywhere;
    ui.model_action(&mut songs, Action::NavigateUp);
    assert!(songs.navigation.focus > 0, "Everywhere wraps the songs");
    // Off: every list stops.
    m.settings.wrap_around = WrapAround::Off;
    m.navigation.focus = 0;
    ui.model_action(&mut m, Action::NavigateUp);
    assert_eq!(m.navigation.focus, 0);
    m.navigation.focus = last;
    ui.model_action(&mut m, Action::NavigateDown);
    assert_eq!(m.navigation.focus, last);
}

#[test]
fn a_fast_spin_stops_at_the_end_of_a_long_list_before_wrapping() {
    let mut ui = radios();
    let tracks: Vec<_> = (0..500)
        .map(|i| Track {
            id: i,
            title: format!("T{i:03}"),
            online: true,
            ..Default::default()
        })
        .collect();
    let mut m = app(Screen::Tracks);
    m.library.tracks = tracks;
    m.settings.wrap_around = WrapAround::Everywhere;
    ui.invalidate_catalog();
    m.navigation.focus = 495;
    ui.model_action(&mut m, Action::WheelClockwise(8));
    assert_eq!(m.navigation.focus, 499, "the spin stops at the end");
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(m.navigation.focus, 499, "still turning: it stays");
    ui.model_action(&mut m, Action::WheelClockwise(3));
    assert_eq!(m.navigation.focus, 499, "an accelerated step never wraps");
    // The wheel rested at the end: one more detent goes to the top.
    ui.last_step = Some(Instant::now() - WRAP_REST);
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(m.navigation.focus, 0);
    assert_eq!(m.navigation.scroll, 0, "the top is in view");
    ui.last_step = Some(Instant::now() - WRAP_REST);
    ui.model_action(&mut m, Action::WheelCounterClockwise(1));
    assert_eq!(m.navigation.focus, 499);
    let visible = ui.visible_count(&m, &m.library.tracks.clone());
    assert_eq!(m.navigation.scroll, 500 - visible, "the end is in view");
}

#[test]
fn now_playing_wheel_is_a_fixed_volume_step_and_select_opens_options() {
    let mut ui = radios();
    let mut m = app(Screen::NowPlaying);
    assert_eq!(
        ui.model_action(&mut m, Action::WheelClockwise(4)),
        Effect::AdjustVolume(2)
    );
    assert_eq!(
        ui.model_action(&mut m, Action::WheelCounterClockwise(1)),
        Effect::AdjustVolume(-2)
    );
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    assert_eq!(m.navigation.modal, Some(Modal::ContextMenu));
    let labels: Vec<_> = ui
        .modal_rows_public(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(labels[0], "Up Next");
    assert!(labels.contains(&"Go to Album".to_owned()));
    assert!(!labels
        .iter()
        .any(|l| l.contains("info") || l.contains("Info")));
    // In the options sheet the wheel moves the sheet, not the volume.
    assert_eq!(
        ui.model_action(&mut m, Action::WheelClockwise(1)),
        Effect::None
    );
    assert_eq!(m.navigation.modal_focus, 1);
}

#[test]
fn password_keyboard_types_with_buttons_and_never_draws_the_password() {
    let mut ui = radios();
    let mut m = app(Screen::Wifi);
    select_key(&mut ui, &mut m, "ssid:Café");
    assert!(ui.keyboard.is_some());
    // The wheel does nothing on the keyboard; Next moves, Select types.
    ui.model_action(&mut m, Action::WheelClockwise(4));
    ui.model_action(&mut m, Action::NavigateRight);
    ui.model_action(&mut m, Action::Select);
    assert_eq!(ui.keyboard.as_ref().unwrap().text, "w");
    // Too short for WPA: Done stays on the keyboard.
    assert_eq!(ui.model_action(&mut m, Action::ContextMenu), Effect::None);
    assert!(ui.keyboard.is_some());
    ui.keyboard.as_mut().unwrap().text = "secretpass".into();
    let draw = ui.draw(&m, &m.library.tracks, false);
    assert!(!drawn_text(&draw).contains("secretpass"));
    assert!(drawn_text(&draw).contains("••••••••••"));
    assert_eq!(focus_target_count(&draw), 1);
    // Playback buttons never reach the keyboard as playback (router), and
    // holding Select joins.
    assert_eq!(
        ui.model_action(&mut m, Action::ContextMenu),
        Effect::WifiConnect {
            ssid: "Café".into(),
            password: "secretpass".into()
        }
    );
    assert!(ui.keyboard.is_none());
    // Holding Back cancels a new entry.
    select_key(&mut ui, &mut m, "ssid:Café");
    assert_eq!(ui.model_action(&mut m, Action::Home), Effect::None);
    assert!(ui.keyboard.is_none());
    assert_eq!(m.screen, Screen::Wifi, "cancel stays on the Wi-Fi page");
}

#[test]
fn a_wrong_wifi_password_reopens_the_keyboard_with_it_and_says_so() {
    // Y2Reborn #20: one wrong letter, 15 tries; the player showed
    // "Connecting…" then "Saved" and never said the password was wrong.
    use crate::{WifiJoin, WifiJoinOutcome};
    let mut ui = radios();
    let mut m = app(Screen::Wifi);
    select_key(&mut ui, &mut m, "ssid:Café");
    ui.keyboard.as_mut().unwrap().text = "secretpasz".into();
    assert!(matches!(
        ui.model_action(&mut m, Action::ContextMenu),
        Effect::WifiConnect { .. }
    ));
    let join = |outcome, attempt| WifiJoin {
        attempt,
        ssid: "Café".into(),
        outcome,
    };
    ui.wifi.join = Some(join(WifiJoinOutcome::Checking, 1));
    ui.wifi_join_changed();
    assert!(visible_text(&ui, &m).contains("Checking password…"));
    ui.wifi.join = Some(join(WifiJoinOutcome::WrongPassword, 1));
    ui.wifi_join_changed();
    let k = ui.keyboard.as_ref().expect("keyboard again");
    assert_eq!(k.text, "secretpasz");
    assert!(k.note.contains("Wrong password"));
    assert!(
        matches!(&k.purpose, crate::keyboard::Purpose::WifiPassword { ssid } if ssid == "Café")
    );
    // Said once: the same outcome again does nothing.
    ui.keyboard = None;
    ui.wifi_join_changed();
    assert!(ui.keyboard.is_none());
    // Fixed letter: joined.
    ui.wifi.join = Some(join(WifiJoinOutcome::Joined, 2));
    ui.wifi_join_changed();
    assert!(ui.notice.contains("Connected to Café"));
    // A saved network whose password stopped working: Wrong password on its
    // row, and selecting it asks for the new one instead of trying it again.
    ui.wifi.status = WifiStatus::Disconnected;
    ui.wifi.problem = Some(reborn_core::platform::WifiProblem::WrongPassword);
    ui.wifi.problem_network = Some("Home".into());
    assert!(visible_text(&ui, &m).contains("Wrong password"));
    assert!(visible_text(&ui, &m).contains("Check the password"));
    assert_eq!(select_key(&mut ui, &mut m, "saved:7"), Effect::None);
    assert!(matches!(&ui.keyboard.as_ref().unwrap().purpose,
        crate::keyboard::Purpose::WifiPassword { ssid } if ssid == "Home"));
    ui.keyboard = None;
    // Change Password is in the saved network's menu.
    ui.wifi.problem = None;
    ui.wifi.problem_network = None;
    let rows = ui.rows(&m, &[]);
    m.navigation.focus = rows.iter().position(|r| r.key == "saved:7").unwrap();
    ui.model_action(&mut m, Action::ContextMenu);
    let menu = ui.modal_rows_public(&m, &[]);
    let at = menu
        .iter()
        .position(|r| r.key == "wifi_change_password")
        .expect("Change Password");
    m.navigation.modal_focus = at;
    ui.model_action(&mut m, Action::Select);
    assert!(matches!(&ui.keyboard.as_ref().unwrap().purpose,
        crate::keyboard::Purpose::WifiPassword { ssid } if ssid == "Home"));
}

#[test]
fn library_search_counts_while_typing_and_lists_artists_albums_songs() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Home);
    m.library.tracks[4].title = "Beat It".into();
    m.library.tracks[4].artist = "Michael Jackson".into();
    // The playlists are searched too: they are read if they were not yet.
    assert!(matches!(
        select_key(&mut ui, &mut m, "search"),
        Effect::Playlist(_)
    ));
    {
        let k = ui.keyboard.as_mut().unwrap();
        k.text = "bea".into();
        // Delete one and type it again to trigger the live count.
        k.row = 2;
        k.col = 8;
    }
    ui.model_action(&mut m, Action::Select);
    assert_eq!(ui.keyboard.as_ref().unwrap().text, "be");
    {
        let k = ui.keyboard.as_mut().unwrap();
        k.row = 1;
        k.col = 0; // a
    }
    ui.model_action(&mut m, Action::Select);
    assert_eq!(ui.keyboard.as_ref().unwrap().note, "1 song");
    assert_eq!(ui.model_action(&mut m, Action::ContextMenu), Effect::None);
    assert!(ui.keyboard.is_none());
    assert_eq!(m.screen, Screen::Search);
    assert_eq!(m.navigation.filter, "bea");
    let rows = ui.rows(&m, &m.library.tracks.clone());
    assert_eq!(rows[0].key, "search_edit");
    assert_eq!(rows[1].key, "heading:songs");
    assert_eq!(rows[2].key, "track:4");
    assert!(rows[2].secondary.starts_with("Michael Jackson · "));
    // The focus is on it; Select plays the song.
    assert_eq!(m.navigation.focus, 2);
    assert!(matches!(
        ui.model_action(&mut m, Action::Select),
        Effect::PlayCollection { selected: 4, .. } | Effect::Play(4)
    ));
    // Search finds artists and albums too, and Edit Search reopens with the text.
    m.screen = Screen::Search;
    m.navigation.filter = "album 1".into();
    m.navigation.focus = 0;
    let rows = ui.rows(&m, &m.library.tracks.clone());
    assert!(rows.iter().any(|r| r.key.starts_with("album:")), "{rows:?}");
    ui.model_action(&mut m, Action::Select);
    assert_eq!(ui.keyboard.as_ref().unwrap().text, "album 1");
}

#[test]
fn screen_off_ignores_navigation_but_keeps_transport_and_volume() {
    let mut ui = radios();
    let mut m = app(Screen::Albums);
    m.screen_off = true;
    assert_eq!(
        ui.model_action(&mut m, Action::WheelClockwise(1)),
        Effect::None
    );
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    assert_eq!(m.navigation.focus, 0);
    assert_eq!(
        ui.model_action(&mut m, Action::PlayPause),
        Effect::TogglePlayback
    );
    assert_eq!(
        ui.model_action(&mut m, Action::VolumeUp),
        Effect::AdjustVolume(2)
    );
}

#[test]
fn wifi_off_absent_and_failed_states_offer_one_useful_action() {
    let mut ui = radios();
    ui.wifi.powered = false;
    let m = app(Screen::Wifi);
    let page = pages::page(&ui, &m, &m.library.tracks);
    assert_eq!(page.hero.unwrap().title, "Wi-Fi Is Off");
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].key, "wifi_power");

    let mut ui = radios();
    ui.wifi.problem = Some(WifiProblem::WrongPassword);
    let page = pages::page(&ui, &m, &m.library.tracks);
    assert!(page.status.unwrap().contains("Check the password"));
    assert_eq!(page.rows[1].secondary, "Couldn't Connect");

    for (problem, words) in [
        (WifiProblem::NoAddress, "couldn't get a network address"),
        (WifiProblem::NoInternetNames, "name lookup"),
        (WifiProblem::NetworkNotFound, "Network not found"),
    ] {
        ui.wifi.problem = Some(problem);
        assert!(pages::wifi_status_line(&ui).unwrap().contains(words));
    }
}

#[test]
fn bluetooth_codec_row_only_appears_with_a_real_choice() {
    let mut ui = radios();
    let mut m = app(Screen::Bluetooth);
    select_key(&mut ui, &mut m, "bt_device:/bt/a");
    let labels: Vec<_> = ui
        .modal_rows_public(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert!(!labels.contains(&"Codec".to_owned()));
    assert!(labels.contains(&"Use for Audio".to_owned()));

    ui.bluetooth.codec_choices = vec![CodecPreference::Sbc, CodecPreference::Aac];
    let rows = ui.modal_rows_public(&m, &m.library.tracks);
    m.navigation.modal_focus = rows.iter().position(|r| r.label == "Codec").unwrap();
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.navigation.modal, Some(Modal::CodecPicker));
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::SetCodecPreference(CodecPreference::Aac)
    );
}

#[test]
fn bluetooth_device_states_and_select_actions() {
    let mut ui = radios();
    ui.bluetooth.devices.push(BluetoothDeviceView {
        path: "/bt/new".into(),
        name: "New Speaker".into(),
        ..Default::default()
    });
    ui.bluetooth.devices.push(BluetoothDeviceView {
        path: "/bt/waiting".into(),
        name: "Soundbar".into(),
        paired: true,
        bonded: true,
        connecting: true,
        ..Default::default()
    });
    let mut m = app(Screen::Bluetooth);
    m.output = AudioOutput::Bluetooth("AA".into());
    let rows = ui.rows(&m, &m.library.tracks);
    // State only; the codec is on Sound > Output (UI v4).
    assert_eq!(rows[1].secondary, "Connected");
    // A Connect waiting for the automatic reconnect is not shown as idle.
    assert_eq!(rows[2].secondary, "Connecting…");
    assert_eq!(rows[3].secondary, "Not paired");
    assert_eq!(
        select_key(&mut ui, &mut m, "bt_device:/bt/new"),
        Effect::BluetoothDevice {
            path: "/bt/new".into(),
            operation: "pair".into()
        }
    );
}

#[test]
fn maintenance_actions_are_confirmed_and_explain_consequences() {
    let mut ui = radios();
    for (key, effect) in [
        ("confirm:reset_settings", Effect::ResetSettings),
        ("confirm:forget_wifi", Effect::ForgetAllWifi),
        ("confirm:forget_bluetooth", Effect::ForgetAllBluetooth),
        ("confirm:rebuild_library", Effect::RebuildLibrary),
        ("confirm:clear_cache", Effect::ClearCache),
    ] {
        let mut m = app(Screen::Maintenance);
        assert_eq!(select_key(&mut ui, &mut m, key), Effect::None);
        let (title, body) = ui.modal_copy(&m);
        assert!(title.ends_with('?') && body.len() > 20, "{key}");
        assert_eq!(m.navigation.modal_focus, 0);
        ui.model_action(&mut m, Action::WheelClockwise(1));
        assert_eq!(ui.model_action(&mut m, Action::Select), effect, "{key}");
    }
}

#[test]
fn update_page_follows_typed_state_and_capability() {
    let mut ui = radios();
    let mut m = app(Screen::Update);
    assert_eq!(
        select_key(&mut ui, &mut m, "task:update_check"),
        Effect::Platform(PlatformTask::UpdateCheck)
    );
    // One step (Beta 7.1): Update Now downloads, checks and installs.
    m.platform.snapshot.update = UpdateState {
        phase: UpdatePhase::Available {
            version: "1.0.0-beta.8".into(),
        },
        can_download: true,
        ..Default::default()
    };
    m.platform.battery.charging = reborn_core::platform::ChargingState::OnBattery;
    m.platform.battery.percent = Some(30);
    assert_eq!(select_key(&mut ui, &mut m, "update_now"), Effect::None);
    assert!(
        !ui.update_run,
        "too little charge: asked before the download"
    );
    m.platform.battery.percent = Some(80);
    assert_eq!(
        select_key(&mut ui, &mut m, "update_now"),
        Effect::Platform(PlatformTask::UpdateStage)
    );
    assert!(ui.update_run);
    // The update owns the screen; Select is Cancel while it downloads.
    m.platform.busy = Some(PlatformTask::UpdateStage);
    assert_eq!(ui.model_action(&mut m, Action::Back), Effect::None);
    assert_eq!(ui.model_action(&mut m, Action::Home), Effect::None);
    assert_eq!(m.screen, Screen::Update);
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::CancelDownload
    );
    let shown = drawn_text(&ui.draw(&m, &m.library.tracks, false));
    assert!(
        shown.contains("Downloading") && shown.contains("Cancel"),
        "{shown}"
    );
    m.platform.download = Some(reborn_core::platform::DownloadLive {
        verifying: true,
        ..Default::default()
    });
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    assert!(!drawn_text(&ui.draw(&m, &m.library.tracks, false)).contains("Cancel"));
    // Install Now, for a package downloaded earlier, needs no confirmation.
    ui.update_run = false;
    m.platform.busy = None;
    m.platform.download = None;
    m.platform.snapshot.update = UpdateState {
        phase: UpdatePhase::ReadyToInstall,
        can_install: true,
        can_cancel: true,
        ..Default::default()
    };
    assert_eq!(
        select_key(&mut ui, &mut m, "update_install"),
        Effect::Platform(PlatformTask::UpdateApply)
    );
    // While installing, only wake and volume are accepted.
    m.platform.busy = Some(PlatformTask::UpdateApply);
    assert_eq!(ui.model_action(&mut m, Action::Back), Effect::None);
    assert_eq!(m.screen, Screen::Update);
    // Without the capability there is no dead check button.
    let mut m = app(Screen::Update);
    m.platform.snapshot.enabled.clear();
    let page = pages::page(&ui, &m, &m.library.tracks);
    assert!(!page
        .rows
        .iter()
        .any(|r| r.key.starts_with("task:update") || r.key.starts_with("update_")));
    assert!(page.hero.unwrap().title.contains("Computer"));
}

#[test]
fn full_system_update_wording_and_versions() {
    let ui = radios();
    let mut m = app(Screen::Update);
    m.platform.snapshot.update = UpdateState {
        phase: UpdatePhase::ReadyToInstall,
        can_install: true,
        ..Default::default()
    };
    let page = pages::page(&ui, &m, &m.library.tracks);
    assert!(page.hero.unwrap().body.contains("restarts"));
    // One step (Beta 7.1): no second confirmation.
    assert!(page
        .rows
        .iter()
        .any(|r| r.label == "Install Now" && r.key == "update_install"));
    m.platform.snapshot.update.phase = UpdatePhase::Failed(UpdateProblem::NotEnoughSpace);
    m.platform.snapshot.update.space_needed_mb = Some(281);
    let page = pages::page(&ui, &m, &m.library.tracks);
    assert!(page.hero.unwrap().body.contains("281 MB"));
    m.platform.snapshot.update.phase = UpdatePhase::Restoring;
    let page = pages::page(&ui, &m, &m.library.tracks);
    assert!(page.hero.unwrap().title.contains("Previous"));
    m.platform.snapshot.update.phase = UpdatePhase::Failed(UpdateProblem::NeedsCharge);
    let page = pages::page(&ui, &m, &m.library.tracks);
    assert!(page.hero.unwrap().title.contains("Charger"));
    // Y2Linux 49: Wi-Fi and Bluetooth say how to supply the radio firmware.
    for (screen, title) in [
        (Screen::Wifi, "Wi-Fi Needs"),
        (Screen::Bluetooth, "Bluetooth Needs"),
    ] {
        let mut f = app(screen);
        f.platform.snapshot.radio_firmware = reborn_core::platform::RadioFirmware::Missing;
        let hero = pages::page(&ui, &f, &f.library.tracks).hero.unwrap();
        assert!(
            hero.title.starts_with(title) && hero.body.contains("system.img"),
            "{}",
            hero.title
        );
        f.platform.snapshot.radio_firmware = reborn_core::platform::RadioFirmware::Ready;
        assert!(!pages::page(&ui, &f, &f.library.tracks)
            .hero
            .is_some_and(|h| h.title.contains("Firmware")));
    }
    // OTA-48: the live record wins while the download task runs.
    m.platform.busy = Some(reborn_core::PlatformTask::UpdateStage);
    m.platform.download = Some(reborn_core::platform::DownloadLive {
        verifying: true,
        bytes_done: 1,
        bytes_total: 1,
        check_percent: Some(63),
        ..Default::default()
    });
    assert!(pages::page(&ui, &m, &m.library.tracks)
        .hero
        .unwrap()
        .title
        .ends_with("63%"));
    assert_eq!(pages::update_summary(&m), "Checking · 63%");
    m.platform.busy = None;
    m.platform.download = None;
    m.platform.snapshot.update.phase = UpdatePhase::Downloading;
    m.platform.snapshot.update.download_percent = Some(40);
    assert!(pages::page(&ui, &m, &m.library.tracks)
        .hero
        .unwrap()
        .title
        .ends_with("40%"));
    assert_eq!(
        pages::short_version("1.0.0-platform-v1-power-final-candidate.35"),
        "1.0.0 (35)"
    );
    assert_eq!(pages::short_version("1.0.0-beta.1"), "1.0 Beta 1");
    assert_eq!(pages::short_version("1.0.0-beta.4"), "1.0 Beta 4");
    assert_eq!(pages::short_version("1.0.0-beta.6.3"), "1.0 Beta 6.3");
    assert_eq!(pages::short_version("1.0.0"), "1.0");
    assert_eq!(pages::short_version("1.2.1"), "1.2.1");
    assert_eq!(pages::short_version("1.2"), "1.2");
}

#[test]
fn shutdown_locks_navigation_but_keeps_volume() {
    let mut ui = radios();
    let mut m = app(Screen::Albums);
    m.platform.shutting_down = Some(reborn_core::platform::ShutdownIntent {
        id: "x".into(),
        restart: false,
        low_battery: false,
    });
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    assert_eq!(ui.model_action(&mut m, Action::PowerHold), Effect::None);
    assert!(
        ui.power_hold.is_none(),
        "no turn-off hold while shutting down"
    );
    assert_eq!(
        ui.model_action(&mut m, Action::VolumeDown),
        Effect::AdjustVolume(-2)
    );
}

#[test]
fn empty_library_and_queue_offer_one_recovery_action() {
    let mut ui = radios();
    let mut m = app(Screen::Albums);
    m.library.tracks.clear();
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::ScanLibrary);
    let draw = ui.draw(&m, &[], false);
    assert_eq!(focus_target_count(&draw), 1);
    m.library.scanning = true;
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    let mut m = app(Screen::Queue);
    m.queue.clear();
    m.queue_entry_ids.clear();
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::Home);
}

#[test]
fn filtered_track_context_actions_stay_on_the_selected_track() {
    let mut tracks = library();
    tracks[0].album = "Other".into();
    for (screen, filter, row) in [
        (
            Screen::Album,
            "album:Élodie · Μουσική · Музыка · 音楽\u{1f}Album 0",
            3,
        ),
        (Screen::Tracks, "", 2),
    ] {
        for (menu, expected) in [(0, Effect::PlayNext(2)), (1, Effect::AddToQueue(2))] {
            let mut ui = radios();
            let mut m = app(screen);
            m.library.tracks = tracks.clone();
            m.navigation.filter = filter.into();
            m.navigation.focus = row;
            ui.model_action(&mut m, Action::ContextMenu);
            assert_eq!(m.navigation.modal, Some(Modal::ContextMenu), "{screen:?}");
            m.navigation.modal_focus = menu;
            assert_eq!(
                ui.model_action(&mut m, Action::Select),
                expected,
                "{screen:?}"
            );
        }
    }
}

#[test]
fn queue_context_follows_its_occurrence_and_closes_when_it_vanishes() {
    let mut ui = radios();
    let mut m = app(Screen::Queue);
    // Up Next: the song playing, the "Next from" heading, then queue index
    // 1 onwards: row 4 is queue index 3.
    m.navigation.focus = 4;
    ui.model_action(&mut m, Action::ContextMenu);
    let id = m.navigation.context_key.clone().unwrap();
    m.queue.swap(3, 4);
    m.queue_entry_ids.swap(3, 4);
    // Play Next, Add to Queue, Move, Remove from Queue: the entry where it is now.
    m.navigation.modal_focus = 3;
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::QueueRemove(4)
    );
    m.navigation.modal = Some(Modal::ContextMenu);
    m.navigation.context_key = Some(id);
    m.queue.truncate(2);
    m.queue_entry_ids.truncate(2);
    ui.normalize(&mut m, &[]);
    assert_eq!(m.navigation.modal, None);
}

#[test]
fn long_and_unicode_text_is_bounded_on_every_surface() {
    let mut ui = radios();
    let long = "Ein sehr langer Titel — 夜の静けさ — Северный свет — ".repeat(8);
    ui.wifi.networks[1].ssid = long.clone();
    ui.bluetooth.devices[0].name = long.clone();
    for screen in [
        Screen::NowPlaying,
        Screen::Wifi,
        Screen::Bluetooth,
        Screen::Tracks,
    ] {
        let mut m = app(screen);
        m.queue[0].title = long.clone();
        m.queue[0].artist = long.clone();
        m.queue[0].album = long.clone();
        m.library.tracks[0].title = long.clone();
        for q in ui.draw(&m, &m.library.tracks, false) {
            if matches!(q.tex, reborn_graphics::Tex::Font(_)) {
                assert!(
                    q.x + q.w <= 482.,
                    "{screen:?} glyph beyond the panel at {}",
                    q.x
                );
            }
        }
    }
}

#[test]
fn storage_battery_and_pc_transfer_present_user_states() {
    let ui = radios();
    let mut m = app(Screen::Storage);
    let text = visible_text(&ui, &m);
    assert!(text.contains("3.2 GB free of 6.4 GB") && text.contains("No SD card"));
    m.platform.snapshot.storage.internal.state = VolumeState::AlmostFull;
    assert!(visible_text(&ui, &m).contains("Storage Almost Full"));
    m.platform.snapshot.storage.sd = SdCard::Error;
    assert!(visible_text(&ui, &m).contains("SD card error"));
    // Issue #44: a card refused by its check says so, not "No SD card".
    m.platform.snapshot.storage.sd = SdCard::NeedsRepair;
    let text = visible_text(&ui, &m);
    assert!(text.contains("Check it on a computer") && !text.contains("No SD card"));

    let mut m = app(Screen::Battery);
    m.platform.battery.level = LowBattery::Critical;
    assert!(visible_text(&ui, &m).contains("Battery Critically Low"));

    let mut m = app(Screen::PcTransfer);
    assert!(visible_text(&ui, &m).contains("Connect Y2 to a computer"));
    m.platform.snapshot.usb = UsbTransfer::Ready;
    assert!(visible_text(&ui, &m).contains("USB File Transfer"));
    m.platform.snapshot.usb = UsbTransfer::Copying;
    assert!(visible_text(&ui, &m).contains("Keep the cable connected"));
    m.platform.snapshot.usb = UsbTransfer::Developer;
    assert!(visible_text(&ui, &m).contains("Advanced USB Transfer"));
    m.platform.snapshot.usb = UsbTransfer::Error;
    assert!(visible_text(&ui, &m).contains("Reconnect the USB cable"));
}

#[test]
fn boot_and_shutdown_frames_are_dark_and_end_black() {
    let boot = boot_screen(0., "Starting system");
    assert_eq!(boot[0].color, theme::color::BG);
    assert_eq!(focus_target_count(&boot), 0);
    let ui_frame = radios().draw(&app(Screen::Home), &library(), false);
    for low in [false, true] {
        let plan = shutdown_plan(low);
        for frame in 0..plan.frames {
            let quads = shutdown_frame(&ui_frame, frame, closing_label(false, low), plan);
            assert!(quads
                .iter()
                .all(|q| q.color & 0xFF != 0 || q.tex != reborn_graphics::Tex::Solid));
        }
    }
    let plan = shutdown_plan(false);
    let last = shutdown_frame(
        &ui_frame,
        plan.frames - 1,
        closing_label(false, false),
        plan,
    );
    assert!(
        drawn_text(&last).is_empty(),
        "the last frames carry no text"
    );
    assert_eq!(black_frame()[0].color, theme::color::BLACK);
    let halfway = boot_transition(ui_frame, 0.5);
    assert!(drawn_text(&halfway).contains("reborn"));
}

#[test]
fn only_preferences_and_session_survive_serialization() {
    let mut m = app(Screen::Albums);
    m.settings.brightness = 40;
    m.platform.busy = Some(PlatformTask::Health);
    let path = std::env::temp_dir().join(format!("reborn-ui-v2-{}.json", std::process::id()));
    m.checkpoint(&path).unwrap();
    let restored = AppModel::restore(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(restored.screen, Screen::Home);
    assert_eq!(restored.settings.brightness, 40);
    assert!(restored.platform.busy.is_none());
    assert_eq!(restored.playback, PlaybackState::Paused);
    let _ = PathBuf::new();
}

#[test]
fn custom_equalizer_edits_one_db_per_detent_and_only_applies_on_select() {
    let mut ui = radios();
    let mut m = app(Screen::Equalizer);
    // Presets first, then Custom (the five bands) and Headphone Profile.
    assert_eq!(ui.rows(&m, &m.library.tracks).len(), 6);
    select_key(&mut ui, &mut m, "eq_custom");
    assert_eq!(m.navigation.filter, "custom");
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(rows.len(), 6);
    m.navigation.focus = 0;
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    assert_eq!(m.navigation.modal, Some(Modal::EqBand(0)));
    assert_eq!(m.navigation.modal_focus, 12);
    assert_eq!(
        ui.model_action(&mut m, Action::WheelClockwise(8)),
        Effect::None
    );
    assert_eq!(m.navigation.modal_focus, 13);
    assert_eq!(
        ui.model_action(&mut m, Action::WheelCounterClockwise(8)),
        Effect::None
    );
    assert_eq!(m.navigation.modal_focus, 12);
    assert!(
        m.settings.eq_bands.is_empty(),
        "preview must not alter DSP or persisted state"
    );
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::SetEqBandGain {
            index: 0,
            gain_db: 0
        }
    );
    assert_eq!(m.navigation.modal, None);
    ui.model_action(&mut m, Action::Select);
    for _ in 0..40 {
        ui.model_action(&mut m, Action::WheelClockwise(5));
    }
    assert_eq!(m.navigation.modal_focus, 24);
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(
        m.navigation.modal_focus, 24,
        "a dB scale never wraps around"
    );
    let quads = ui.draw(&m, &m.library.tracks, false);
    assert_eq!(focus_target_count(&quads), 1);
    for q in quads
        .iter()
        .filter(|q| matches!(q.tex, reborn_graphics::Tex::Font(_)))
    {
        assert!(q.y >= 0. && q.y + q.h <= 360., "EQ sheet overflow: {q:?}");
    }
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.navigation.modal, None);
    assert!(m.settings.eq_bands.is_empty());
}

#[test]
fn sleep_and_screen_off_are_distinct_product_actions_with_recoverable_refusal() {
    use reborn_core::platform::{SleepPhase, SleepProblem};
    let mut ui = radios();
    let mut m = app(Screen::Sleep);
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Platform(PlatformTask::SleepRequest)
    );
    m.platform.snapshot.sleep.phase = SleepPhase::SleepRefused;
    for (problem, wording) in [
        (SleepProblem::QualificationPending, "turned off the screen"),
        (SleepProblem::ChargerActive, "charger disconnected"),
        (SleepProblem::PlaybackActive, "Pause music"),
    ] {
        m.platform.snapshot.sleep.problem = Some(problem);
        assert!(visible_text(&ui, &m).contains(wording));
    }
    m.navigation.focus = 1;
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::ScreenSleep);
    m.platform.snapshot.sleep.phase = SleepPhase::RestoreFailed;
    assert!(visible_text(&ui, &m).contains("Wake Needs Attention"));
    assert!(ui
        .rows(&m, &m.library.tracks)
        .iter()
        .any(|r| r.key == "confirm:power_off"));
}

#[test]
fn diagnostic_export_is_distinct_from_private_backup_and_soc_is_labelled() {
    let mut ui = radios();
    let mut m = app(Screen::Diagnostics);
    let rows = ui.rows(&m, &m.library.tracks);
    let index = rows
        .iter()
        .position(|r| r.key == "task:diagnostics_export")
        .unwrap();
    assert!(rows[index].secondary.contains("Redacted"));
    assert!(rows
        .iter()
        .any(|r| r.key == "task:export" && r.secondary.contains("Private")));
    m.navigation.focus = index;
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Platform(PlatformTask::DiagnosticsExport)
    );
    ui.show_operation_result(&mut m);
    assert_eq!(m.screen, Screen::DiagnosticSection);
    assert_eq!(m.navigation.filter, "result");
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.screen, Screen::Diagnostics);
    assert_eq!(m.navigation.focus, index);
    m.screen = Screen::Battery;
    assert!(visible_text(&ui, &m).contains("Estimated Charge"));
    m.platform.battery.estimated = false;
    assert!(!visible_text(&ui, &m).contains("Estimated"));
}

#[test]
fn ldac_controls_need_real_runtime_support_and_preference_is_not_actual() {
    use reborn_core::platform::{BluetoothQuality, LdacQuality};
    let mut ui = radios();
    let mut m = app(Screen::SettingsSystem);
    m.settings.codec_preference = CodecPreference::Ldac;
    ui.bluetooth.codec_choices = vec![CodecPreference::Sbc, CodecPreference::Ldac];
    assert!(!ui
        .rows(&m, &m.library.tracks)
        .iter()
        .any(|r| r.key == "ldac_quality"));
    m.platform.snapshot.bluetooth_quality = BluetoothQuality {
        ldac_supported: true,
        abr_supported: true,
        requested_quality: Some(LdacQuality::High),
        effective_quality: Some(LdacQuality::Standard),
        requested_abr: Some(true),
        effective_abr: Some(false),
        pending_restart: true,
        ..Default::default()
    };
    let rows = ui.rows(&m, &m.library.tracks);
    assert!(rows
        .iter()
        .any(|r| r.key == "codec_preference" && r.secondary == "LDAC"));
    m.navigation.focus = rows.iter().position(|r| r.key == "ldac_quality").unwrap();
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Platform(PlatformTask::LdacQuality(LdacQuality::Mobile))
    );
    m.navigation.focus = rows.iter().position(|r| r.key == "ldac_abr").unwrap();
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Platform(PlatformTask::LdacAbr(false))
    );
}

#[test]
fn aac_headphones_get_quality_and_auto_rate_rows() {
    use reborn_core::platform::{AacQuality, BluetoothQuality};
    let mut ui = radios();
    let mut m = app(Screen::SettingsSystem);
    ui.bluetooth.codec_choices = vec![CodecPreference::Sbc, CodecPreference::Aac];
    // No rows until the platform reports AAC settings.
    assert!(!ui
        .rows(&m, &m.library.tracks)
        .iter()
        .any(|r| r.key == "aac_quality" || r.key == "aac_abr"));
    m.platform.snapshot.bluetooth_quality = BluetoothQuality {
        aac_supported: true,
        requested_aac: Some(AacQuality::High),
        aac_abr_supported: true,
        requested_aac_abr: Some(true),
        ..Default::default()
    };
    let rows = ui.rows(&m, &m.library.tracks);
    assert!(rows
        .iter()
        .any(|r| r.key == "aac_quality" && r.secondary == "High · up to 256 kbps"));
    assert!(!rows.iter().any(|r| r.key == "ldac_quality"));
    assert_eq!(
        select_key(&mut ui, &mut m, "aac_quality"),
        Effect::Platform(PlatformTask::AacQuality(AacQuality::Standard))
    );
    assert_eq!(
        select_key(&mut ui, &mut m, "aac_abr"),
        Effect::Platform(PlatformTask::AacAbr(false))
    );
}

#[test]
fn sbc_only_peer_can_enable_xq_without_a_hidden_codec_picker() {
    use reborn_core::platform::{BluetoothQuality, SbcQuality};
    let mut ui = radios();
    let mut m = app(Screen::SettingsSystem);
    ui.bluetooth.codec_choices.clear();
    ui.bluetooth.sbc_quality_available = true;
    m.platform.snapshot.bluetooth_quality = BluetoothQuality {
        sbc_supported: true,
        requested_sbc: Some(SbcQuality::High),
        ..Default::default()
    };
    assert!(!ui
        .rows(&m, &m.library.tracks)
        .iter()
        .any(|r| r.key == "codec_preference"));
    assert_eq!(
        select_key(&mut ui, &mut m, "sbc_quality"),
        Effect::Platform(PlatformTask::SbcQuality(SbcQuality::Xq))
    );
}

#[test]
fn wheel_keeps_selection_visible_at_both_ends_of_realistic_library_sizes() {
    for count in [10, 100, 1_000, 10_000] {
        let mut ui = Ui::default();
        let mut m = app(Screen::Tracks);
        m.library.tracks = (0..count)
            .map(|i| Track {
                id: i as i64,
                title: format!("{i:05} — 夜の静けさ — long title"),
                source_id: "internal".into(),
                path: format!("/data/music/{i}.flac").into(),
                filename: format!("{i}.flac"),
                online: true,
                ..Default::default()
            })
            .collect();
        let visible = screens::visible_rows_for(Screen::Tracks);
        for _ in 0..(count / 4 + 12) {
            ui.model_action(&mut m, Action::WheelClockwise(4));
            assert!(m.navigation.focus < count);
            assert!(m.navigation.focus >= m.navigation.scroll);
            assert!(m.navigation.focus < m.navigation.scroll + visible);
        }
        // Short lists deliberately ignore acceleration: finish with slow detents.
        for _ in 0..count.min(40) {
            ui.model_action(&mut m, Action::WheelClockwise(1));
        }
        assert_eq!(m.navigation.focus, count - 1);
        assert_eq!(
            focus_target_count(&ui.draw(&m, &m.library.tracks, false)),
            1
        );
        let before = m.navigation.focus;
        ui.model_action(&mut m, Action::WheelCounterClockwise(1));
        assert_eq!(
            m.navigation.focus,
            before - 1,
            "first reversed detent is precise"
        );
        for _ in 0..count {
            ui.model_action(&mut m, Action::WheelCounterClockwise(4));
        }
        assert_eq!((m.navigation.focus, m.navigation.scroll), (0, 0));
        assert_eq!(
            focus_target_count(&ui.draw(&m, &m.library.tracks, false)),
            1
        );
    }
}

#[test]
fn power_short_sleeps_when_paused_and_blanks_during_playback() {
    let mut ui = radios();
    let mut m = app(Screen::Home);
    m.playback = reborn_core::PlaybackState::Paused;
    assert_eq!(
        ui.model_action(&mut m, Action::PowerShort),
        Effect::Platform(PlatformTask::SleepRequest)
    );
    assert_eq!(m.screen, Screen::Sleep);
    for state in [
        reborn_core::PlaybackState::Playing,
        reborn_core::PlaybackState::Buffering,
    ] {
        m.playback = state;
        assert_eq!(
            ui.model_action(&mut m, Action::PowerShort),
            Effect::ScreenSleep
        );
    }
    ui.model_action(&mut m, Action::PowerHold);
    assert_eq!(
        m.navigation.modal, None,
        "a hold shows the ring, never a menu"
    );
    assert!(ui.power_hold.is_some());
}

#[test]
fn now_playing_shows_icons_only_for_options_that_differ_from_the_default_and_never_a_volume_number()
{
    use reborn_graphics::Tex;
    let ui = radios();
    let mut m = app(Screen::NowPlaying);
    m.settings.volume = 77;
    m.settings.gapless_enabled = false;
    let icons = |m: &AppModel| -> usize {
        ui.draw(m, &m.library.tracks, false)
            .iter()
            .filter(|q| {
                q.tex == Tex::Mask
                    && [
                        maskdata::SHUFFLE,
                        maskdata::REPEAT,
                        maskdata::REPEAT_ONE,
                        maskdata::GAPLESS,
                        maskdata::EQ,
                        maskdata::NORM,
                    ]
                    .contains(&q.src)
            })
            .count()
    };
    assert_eq!(icons(&m), 0, "all options off: no icons");
    m.settings.shuffle = true;
    m.settings.repeat = RepeatMode::Track;
    assert_eq!(icons(&m), 2);
    // Gapless is on by default: no icon for it.
    m.settings.gapless_enabled = true;
    m.settings.eq_enabled = true;
    m.settings.replay_gain = reborn_core::ReplayGainMode::Track;
    assert_eq!(icons(&m), 4);
    let text = drawn_text(&ui.draw(&m, &m.library.tracks, false));
    assert!(
        !text.contains("77") && !text.to_lowercase().contains("vol"),
        "{text}"
    );
}

#[test]
fn volume_is_a_bare_line_that_fades_and_carries_no_text() {
    use reborn_graphics::Tex;
    let mut ui = radios();
    let mut m = app(Screen::Albums);
    m.settings.volume = 64;
    let plain = ui.draw(&m, &m.library.tracks, false).len();
    ui.show_volume();
    assert!(ui.volume_visibility() > 0.99);
    let shown = ui.draw(&m, &m.library.tracks, false);
    assert!(shown.len() > plain, "the line is drawn on any screen");
    assert!(!drawn_text(&shown).contains("64"));
    // The line is thin, near the right edge, and rises with the volume.
    let line: Vec<_> = shown
        .iter()
        .filter(|q| q.tex == Tex::Solid && q.w <= 4. && q.x > 440. && q.h > 20.)
        .collect();
    assert!(!line.is_empty());
    ui.volume_until = Some(Instant::now() - Duration::from_millis(1));
    assert_eq!(ui.volume_visibility(), 0.);
    assert!(ui.expire_notice());
    assert_eq!(ui.draw(&m, &m.library.tracks, false).len(), plain);
}

#[test]
fn covers_are_requested_while_missing_and_drawn_once_resident() {
    use reborn_graphics::Tex;
    let mut ui = radios();
    let mut m = app(Screen::Tracks);
    for t in &mut m.library.tracks {
        t.artwork = true;
    }
    let tracks = m.library.tracks.clone();
    let quads = ui.draw(&m, &tracks, false);
    assert!(!quads.iter().any(|q| matches!(q.tex, Tex::Thumb(_))));
    let wanted = ui.take_wanted_thumbs();
    assert!(!wanted.is_empty() && wanted.len() <= 64);
    let keys: Vec<u64> = tracks.iter().map(art_key).collect();
    assert!(wanted
        .iter()
        .all(|(key, index)| keys.contains(key) && *index < tracks.len()));
    // Tracks of one album share one cover.
    assert_eq!(art_key(&tracks[0]), art_key(&tracks[1]));
    assert_ne!(art_key(&tracks[0]), art_key(&tracks[3]));
    // Songs without an album, even of one folder and artist, keep their own (#43).
    let loose = |name: &str| Track {
        path: format!("/music/Singles/{name}.mp3").into(),
        artist: "Artist".into(),
        album_artist: "Artist".into(),
        ..Default::default()
    };
    assert_ne!(art_key(&loose("One")), art_key(&loose("Two")));
    assert_eq!(art_key(&loose("One")), art_key(&loose("One")));
    ui.set_resident_thumbs(keys);
    let quads = ui.draw(&m, &tracks, false);
    assert!(quads.iter().any(|q| matches!(q.tex, Tex::Thumb(_))));
    assert!(ui.take_wanted_thumbs().is_empty());
}

#[test]
fn album_page_plays_and_shuffles_and_artist_page_lists_albums() {
    let mut ui = radios();
    let mut m = app(Screen::Album);
    m.navigation.filter = "album:Élodie · Μουσική · Музыка · 音楽\u{1f}Album 0".into();
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(rows[0].key, "play:collection");
    assert_eq!(rows[1].key, "shuffle:collection");
    assert!(matches!(
        select_key(&mut ui, &mut m, "shuffle:collection"),
        Effect::PlayCollection {
            shuffle: ShuffleMode::Songs,
            ..
        }
    ));
    let mut m = app(Screen::Artist);
    m.navigation.filter = "artist:Élodie · Μουσική · Музыка · 音楽".into();
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(rows[0].key, "play:collection");
    let albums: Vec<_> = rows.iter().skip(1).collect();
    assert_eq!(albums.len(), 3, "one row per album, not per song");
    assert!(albums
        .iter()
        .all(|r| r.key.starts_with("album:") && r.art.is_some()));
    m.navigation.focus = 1;
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::Album);
}

#[test]
fn controls_keep_one_accent_while_the_cover_colour_follows_the_song() {
    let mut ui = radios();
    assert_eq!(ui.accent(), theme::color::ACCENT);
    assert_eq!(ui.cover_accent(), theme::color::ACCENT);
    ui.accent = 0xFF965AFF;
    assert_eq!(ui.accent(), theme::color::ACCENT);
    assert_eq!(ui.cover_accent(), 0xFF965AFF);
}

#[test]
fn a_computer_connecting_shows_a_message_and_the_usb_symbol_but_never_blocks() {
    // Beta 7.3: no screen covers the player while a computer has the drives.
    let mut ui = radios();
    let mut m = app(Screen::Albums);
    let tracks = m.library.tracks.clone();
    m.platform.snapshot.usb = UsbTransfer::Disconnected;
    ui.observe(&m);
    // The status bar's symbol (right side, visible); a message's own icon
    // sits in its pill in the middle, and the bar gives way under it.
    let usb_symbol = |q: &[reborn_graphics::Quad]| {
        q.iter()
            .any(|q| q.src == maskdata::USB && q.x > 300. && q.color & 0xFF != 0)
    };
    assert!(!usb_symbol(&ui.draw(&m, &tracks, false)));
    m.platform.snapshot.usb = UsbTransfer::Ready;
    ui.observe(&m);
    let frame = ui.draw(&m, &tracks, false);
    assert!(drawn_text(&frame).contains("Connected to a computer"));
    assert!(!usb_symbol(&frame), "the bar gives way to the message");
    ui.notice.clear();
    assert!(usb_symbol(&ui.draw(&m, &tracks, false)));
    // Everything keeps working: the wheel moves, Back goes back.
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(m.navigation.focus, 1);
    let mut m2 = app(Screen::Home);
    m2.platform.snapshot.usb = UsbTransfer::Ready;
    ui.observe(&m2);
    m2.navigation.focus = 1;
    ui.model_action(&mut m2, Action::Select);
    assert_eq!(m2.screen, Screen::Music);
    // Copying says so; the symbol stays while the cable is in.
    m.platform.snapshot.usb = UsbTransfer::Copying;
    ui.observe(&m);
    let frame = ui.draw(&m, &tracks, false);
    assert!(drawn_text(&frame).contains("Copying files"));
    ui.notice.clear();
    assert!(usb_symbol(&ui.draw(&m, &tracks, false)));
    m.platform.snapshot.usb = UsbTransfer::Disconnected;
    ui.observe(&m);
    assert!(!usb_symbol(&ui.draw(&m, &tracks, false)));
}

#[test]
fn low_battery_raises_one_dialog_per_level_and_ok_acknowledges_it() {
    let mut ui = radios();
    let mut m = app(Screen::Home);
    ui.observe(&m);
    assert_eq!(ui.alert, None);
    m.platform.battery.level = LowBattery::Low;
    ui.observe(&m);
    assert_eq!(ui.alert, Some(Alert::BatteryLow));
    assert_eq!(
        focus_target_count(&ui.draw(&m, &m.library.tracks, false)),
        1
    );
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    assert_eq!(ui.alert, None);
    ui.observe(&m);
    assert_eq!(ui.alert, None, "not raised again at the same level");
    m.platform.battery.level = LowBattery::Critical;
    ui.observe(&m);
    assert_eq!(
        ui.alert,
        Some(Alert::BatteryLow),
        "a worse level warns again"
    );
    ui.model_action(&mut m, Action::Back);
    m.platform.battery.level = LowBattery::Normal;
    ui.observe(&m);
    m.platform.battery.level = LowBattery::Low;
    ui.observe(&m);
    assert_eq!(ui.alert, Some(Alert::BatteryLow), "recharged and low again");
}

#[test]
fn bluetooth_pairing_shows_progress_and_a_failure_offers_try_again() {
    let mut ui = radios();
    ui.bluetooth.devices.push(BluetoothDeviceView {
        path: "/bt/new".into(),
        address: "NN".into(),
        name: "New Speaker".into(),
        ..Default::default()
    });
    let mut m = app(Screen::Bluetooth);
    assert_eq!(
        select_key(&mut ui, &mut m, "bt_device:/bt/new"),
        Effect::BluetoothDevice {
            path: "/bt/new".into(),
            operation: "pair".into()
        }
    );
    assert_eq!(ui.bluetooth_busy(), Some(("New Speaker", "pair")));
    assert!(drawn_text(&ui.draw(&m, &m.library.tracks, false)).contains("Pairing with New Speaker"));
    ui.observe(&m);
    assert!(ui.alert.is_none(), "still waiting");
    // The request fails.
    ui.bluetooth.problem = Some("Couldn't complete that.".into());
    ui.bt_busy.as_mut().unwrap().3 = Instant::now() - Duration::from_secs(2);
    ui.observe(&m);
    assert!(matches!(ui.alert, Some(Alert::BluetoothFailed { .. })));
    assert!(ui.bluetooth_busy().is_none());
    assert!(drawn_text(&ui.draw(&m, &m.library.tracks, false)).contains("Couldn't Connect"));
    // Cancel is the first button; Try again repeats the same operation.
    assert_eq!(ui.alert_focus(), 1);
    ui.model_action(&mut m, Action::WheelCounterClockwise(1));
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    assert!(ui.alert.is_none());
    ui.alert = Some(Alert::BluetoothFailed {
        path: "/bt/new".into(),
        name: "New Speaker".into(),
        operation: "pair".into(),
    });
    ui.alert_focus = 1;
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::BluetoothDevice {
            path: "/bt/new".into(),
            operation: "pair".into()
        }
    );
    assert_eq!(ui.bluetooth_busy(), Some(("New Speaker", "pair")));
}

#[test]
fn update_download_and_scanning_show_a_sweeping_ring_without_a_made_up_percentage() {
    let mut ui = radios();
    ui.spin_fixed = Some(0.3);
    let mut m = app(Screen::Update);
    m.platform.snapshot.update.phase = UpdatePhase::Downloading;
    ui.observe(&m);
    assert!(ui.animating());
    let text = drawn_text(&ui.draw(&m, &m.library.tracks, false));
    assert!(
        text.contains("Downloading Update") && text.contains("Connecting") && !text.contains('%'),
        "{text}"
    );
    // OTA-48: with the platform's live record, real numbers.
    m.platform.busy = Some(reborn_core::PlatformTask::UpdateStage);
    m.platform.download = Some(reborn_core::platform::DownloadLive {
        verifying: false,
        bytes_done: 23_400_000,
        bytes_total: 58_700_000,
        speed_bytes_per_s: Some(3_250_000),
        seconds_left: Some(11),
        check_percent: None,
    });
    ui.observe(&m);
    assert!(ui.animating());
    let text = drawn_text(&ui.draw(&m, &m.library.tracks, false));
    for want in [
        "39%",
        "23.4 of 58.7 MB",
        "3.2 MB/s",
        "About 10 seconds left",
        // Beta 7.6: the step tracker instead of "STEP 1 OF 3".
        "Download",
        "Check",
        "Install",
    ] {
        assert!(text.contains(want), "{want}: {text}");
    }
    let mut m = app(Screen::Albums);
    m.library.tracks.clear();
    m.library.scanning = true;
    ui.observe(&m);
    assert!(ui.animating());
    assert!(drawn_text(&ui.draw(&m, &[], false)).contains("Finding Your Music"));
    m.library.scanning = false;
    m.platform.snapshot.update.phase = UpdatePhase::UpToDate;
    ui.observe(&m);
    ui.volume_until = None;
    assert!(!ui.animating());
}

#[test]
fn crossfade_gets_its_own_option_icon() {
    let ui = radios();
    let mut m = app(Screen::NowPlaying);
    m.settings.gapless_enabled = false;
    let count = |m: &AppModel| {
        ui.draw(m, &m.library.tracks, false)
            .iter()
            .filter(|q| q.src == maskdata::CROSSFADE)
            .count()
    };
    assert_eq!(count(&m), 0);
    m.settings.crossfade_ms = 6_000;
    assert_eq!(count(&m), 1);
}

#[test]
fn genres_open_their_albums_then_all_songs_or_one_album() {
    let ui = Ui::default();
    let mut m = app(Screen::Genres);
    // Albums 0 and 1 are Jazz, album 2 has no genre tag.
    for t in m.library.tracks.iter_mut() {
        t.genre = if t.id < 6 {
            "Jazz".into()
        } else {
            String::new()
        };
    }
    let tracks = m.library.tracks.clone();
    let genres = ui.rows(&m, &tracks);
    assert_eq!(
        genres.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
        ["Jazz", "No Genre"]
    );
    assert_eq!(genres[0].secondary, "2 albums · 6 songs");
    assert_eq!(genres[1].key, "genre:");
    assert_eq!(ui.library_counts(&m, &tracks)[3], 2);
    // Jazz -> its albums, headed by All Songs.
    let mut ui = ui;
    ui.action(&mut m, &tracks, Action::Select);
    assert_eq!(
        (m.screen, m.navigation.filter.as_str()),
        (Screen::Albums, "genre:Jazz")
    );
    assert_eq!(screens::screen_title(&m), "Jazz");
    let albums = ui.rows(&m, &tracks);
    assert_eq!(
        albums.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
        ["All Songs", "Shuffle", "Album 0", "Album 1"]
    );
    assert_eq!(albums[0].secondary, "6 songs");
    // All Songs -> the genre's six songs.
    ui.action(&mut m, &tracks, Action::Select);
    assert_eq!(
        (m.screen, m.navigation.filter.as_str()),
        (Screen::Tracks, "genre:Jazz")
    );
    assert_eq!(ui.row_count(&m, &tracks), 6);
    // Back, then one album -> its page.
    ui.action(&mut m, &tracks, Action::Back);
    m.navigation.focus = 3;
    ui.action(&mut m, &tracks, Action::Select);
    assert_eq!(m.screen, Screen::Album);
    // Opened from the genre: that genre's songs of it (#50).
    assert!(m.navigation.filter.ends_with("Album 1\u{1f}genre:Jazz"));
    // Back, then Shuffle -> the genre's six songs, shuffled.
    ui.action(&mut m, &tracks, Action::Back);
    m.navigation.focus = 1;
    match ui.action(&mut m, &tracks, Action::Select) {
        Effect::PlayCollection {
            members, shuffle, ..
        } => {
            assert_eq!(shuffle, ShuffleMode::Songs);
            assert_eq!(members.len(), 6);
            assert!(members.iter().all(|&i| tracks[i].genre == "Jazz"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn home_with_genres_fits_the_screen() {
    let ui = radios();
    let m = app(Screen::Home);
    for focus in 0..7 {
        let mut m = m.clone();
        m.navigation.focus = focus;
        let quads = ui.draw(&m, &m.library.tracks, false);
        // Every menu label ends above the bottom edge.
        assert!(
            quads
                .iter()
                .all(|q| q.y + q.h <= crate::theme::HEIGHT + 0.5),
            "focus {focus}"
        );
    }
}

#[test]
fn albums_use_cover_flow_by_default_and_the_grid_by_setting() {
    let mut ui = radios();
    ui.motion_fixed = true;
    let mut m = app(Screen::Albums);
    let tracks = m.library.tracks.clone();
    let flow = ui.draw(&m, &tracks, false);
    // Cover Flow draws its covers as warped quads; the grid draws none.
    assert!(flow.iter().filter(|q| q.warp.is_some()).count() >= 2);
    assert_eq!(crate::focus_target_count(&flow), 1);
    m.settings.album_view = reborn_core::AlbumView::Grid;
    let grid = ui.draw(&m, &tracks, false);
    assert!(grid.iter().all(|q| q.warp.is_none()));
    // The setting toggles from Settings > Customize.
    let mut m = app(Screen::Customize);
    let rows = ui.rows(&m, &tracks);
    m.navigation.focus = rows.iter().position(|r| r.key == "album_view").unwrap();
    assert_eq!(rows[m.navigation.focus].label, "Cover Flow");
    assert_eq!(rows[m.navigation.focus].secondary, "On");
    assert_eq!(
        ui.action(&mut m, &tracks, Action::Select),
        Effect::SetAlbumView(reborn_core::AlbumView::Grid)
    );
}

#[test]
fn cover_flow_glides_opens_with_a_flight_and_asks_for_the_sharp_cover_at_rest() {
    let mut ui = radios();
    let mut m = app(Screen::Albums);
    let tracks = m.library.tracks.clone();
    ui.draw(&m, &tracks, false); // first frame anchors the row at focus 0
    m.navigation.focus = 2;
    ui.draw(&m, &tracks, false);
    assert!(ui.animating(), "the row glides toward the new focus");
    // While gliding nothing asks for the full-resolution cover.
    assert!(ui.take_cover_request().is_none());
    // Select flies the centre cover into the album page.
    ui.action(&mut m, &tracks, Action::Select);
    assert_eq!(m.screen, Screen::Album);
    assert!(ui.animating());
    let page = ui.draw(&m, &tracks, false);
    assert!(
        page.iter().any(|q| q.warp.is_some()),
        "the cover is in flight"
    );
    // Frozen motion: no glide, no flight; the cover is wanted once at rest.
    let mut ui = radios();
    ui.motion_fixed = true;
    let m = app(Screen::Albums);
    ui.draw(&m, &tracks, false);
    std::thread::sleep(std::time::Duration::from_millis(130));
    ui.draw(&m, &tracks, false);
    let (key, index) = ui
        .take_cover_request()
        .expect("centre cover requested at rest");
    assert_eq!(key, crate::art_key(&tracks[index]));
    ui.collection_art.set(Some(key));
    ui.draw(&m, &tracks, false);
    assert!(ui.take_cover_request().is_none(), "never asked twice");
    assert!(ui
        .draw(&m, &tracks, false)
        .iter()
        .any(|q| q.tex == reborn_graphics::Tex::CollectionArt));
}

#[test]
fn album_detail_shows_short_albums_in_seconds_and_follows_the_library() {
    let ui = radios();
    let m = app(Screen::Albums);
    let mut tracks = library();
    let key = album_filter(&tracks[0]);
    assert_eq!(ui.album_detail(&m, &tracks, &key), "3 songs · 10 min");
    for t in &mut tracks {
        t.duration_ms = 14_000;
    }
    // A different library (a rescan) is never answered from the old line.
    let short: Vec<Track> = tracks.clone();
    assert_eq!(ui.album_detail(&m, &short, &key), "3 songs · 0:42");
    assert_eq!(ui.album_detail(&m, &short[..1], &key), "1 song · 0:14");
}

#[test]
fn only_cover_flow_motion_asks_for_the_panel_rate() {
    let ui = radios();
    let mut m = app(Screen::Albums);
    let tracks = m.library.tracks.clone();
    ui.draw(&m, &tracks, false);
    assert!(!ui.gliding());
    m.navigation.focus = 2;
    ui.draw(&m, &tracks, false);
    assert!(ui.gliding() && ui.animating());
}

#[test]
fn library_lists_sort_a_to_z_ignoring_case_accents_and_a_leading_article() {
    let ui = Ui::default();
    let mut m = app(Screen::Artists);
    let named = [
        (
            "Such Great Heights",
            "The Postal Service",
            "Give Up",
            "Indie",
        ),
        ("The Wall", "Pink Floyd", "The Wall", "rock"),
        (
            "Bohemian Rhapsody",
            "Queen",
            "A Night at the Opera",
            "Rock & Roll",
        ),
        (
            "Can I Kick It?",
            "A Tribe Called Quest",
            "People's Instinctive Travels",
            "Hip Hop",
        ),
        ("Bloodbuzz Ohio", "the National", "High Violet", "Indie"),
        ("Été", "Émilie Simon", "Végétal", "Électronique"),
        (
            "1979",
            "The Smashing Pumpkins",
            "Mellon Collie",
            "The Blues",
        ),
        (
            "an end has a start",
            "Editors",
            "An End Has a Start",
            "Alternative",
        ),
    ];
    m.library.tracks = named
        .iter()
        .enumerate()
        .map(|(i, (title, artist, album, genre))| Track {
            id: i as i64,
            title: (*title).into(),
            artist: (*artist).into(),
            album: (*album).into(),
            album_artist: (*artist).into(),
            genre: (*genre).into(),
            online: true,
            ..Default::default()
        })
        .collect();
    let tracks = m.library.tracks.clone();
    let mut labels = |screen: Screen| {
        m.screen = screen;
        ui.rows(&m, &tracks)
            .into_iter()
            .map(|r| r.label)
            .collect::<Vec<_>>()
    };
    // Artists skip a leading "The" but keep "A" (A Tribe Called Quest).
    assert_eq!(
        labels(Screen::Artists),
        [
            "A Tribe Called Quest",
            "Editors",
            "Émilie Simon",
            "the National",
            "Pink Floyd",
            "The Postal Service",
            "Queen",
            "The Smashing Pumpkins",
        ]
    );
    // Albums, songs and genres skip "The", "A" and "An".
    assert_eq!(
        labels(Screen::Albums),
        [
            "An End Has a Start",
            "Give Up",
            "High Violet",
            "Mellon Collie",
            "A Night at the Opera",
            "People's Instinctive Travels",
            "Végétal",
            "The Wall",
        ]
    );
    assert_eq!(
        labels(Screen::Tracks),
        [
            "1979",
            "Bloodbuzz Ohio",
            "Bohemian Rhapsody",
            "Can I Kick It?",
            "an end has a start",
            "Été",
            "Such Great Heights",
            "The Wall",
        ]
    );
    assert_eq!(
        labels(Screen::Genres),
        [
            "Alternative",
            "The Blues",
            "Électronique",
            "Hip Hop",
            "Indie",
            "rock",
            "Rock & Roll",
        ]
    );
    // The fast-scroll letter follows the order.
    assert_eq!(initial("The Postal Service", Screen::Artists), 'P');
    assert_eq!(initial("A Tribe Called Quest", Screen::Artists), 'A');
    assert_eq!(initial("Émilie Simon", Screen::Artists), 'E');
    assert_eq!(initial("A Night at the Opera", Screen::Albums), 'N');
    assert_eq!(initial("1979", Screen::Tracks), '#');
}

#[test]
fn the_album_grid_of_a_genre_is_titled_with_the_genre() {
    let ui = radios();
    let mut m = app(Screen::Albums);
    m.settings.album_view = reborn_core::AlbumView::Grid;
    for t in m.library.tracks.iter_mut() {
        t.genre = "Jazz".into();
    }
    let tracks = m.library.tracks.clone();
    assert!(drawn_text(&ui.draw(&m, &tracks, false)).contains("Albums"));
    m.navigation.filter = "genre:Jazz".into();
    let text = drawn_text(&ui.draw(&m, &tracks, false));
    assert!(text.contains("Jazz") && !text.contains("Albums"), "{text}");
}

fn station(name: &str, uuid: &str) -> reborn_core::RadioStation {
    reborn_core::RadioStation {
        uuid: uuid.into(),
        name: name.into(),
        url: format!("http://{name}.example/stream")
            .to_ascii_lowercase()
            .replace(' ', ""),
        country: "Germany".into(),
        codec: "MP3".into(),
        bitrate: 128,
        tags: "jazz".into(),
        ..Default::default()
    }
}

#[test]
fn radio_menu_lists_own_and_directory_sources_and_explains_wifi() {
    use reborn_core::RadioRequest;
    let mut ui = radios();
    let mut m = app(Screen::Home);
    assert_eq!(select_key(&mut ui, &mut m, "radio"), Effect::None);
    assert_eq!(m.screen, Screen::Radio);
    // Offline: a first row leads to Wi-Fi; favorites and own lists stay.
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(rows[0].key, "radio_wifi");
    assert_eq!(rows[0].label, "Connect to Wi-Fi");
    ui.radio.online = true;
    let labels: Vec<_> = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(
        labels,
        [
            "Favorites",
            "Popular",
            "Countries",
            "Genres",
            "Search",
            "My Stations"
        ]
    );
    assert_eq!(
        select_key(&mut ui, &mut m, "radio_popular"),
        Effect::Radio(RadioRequest::Popular)
    );
    assert_eq!(
        (m.screen, m.navigation.filter.as_str()),
        (Screen::RadioStations, "Popular")
    );
    assert!(ui.radio.loading);
    let page = crate::pages::page(&ui, &m, &m.library.tracks.clone());
    assert_eq!(page.hero.map(|h| h.title), Some("Loading Stations…".into()));
}

#[test]
fn radio_lists_sort_a_to_z_and_the_song_splits_into_title_and_artist() {
    let mut ui = radios();
    ui.radio.set_stations(vec![
        station("the zebra fm", "z"),
        station("Älpler Radio", "a"),
        station("BBC Radio 1", "b"),
    ]);
    let names: Vec<_> = ui.radio.stations.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Älpler Radio", "BBC Radio 1", "the zebra fm"]);
    assert_eq!(ui.radio.ranked, 0);
    // A country's list: its most listened stations first, then A to Z.
    ui.radio.set_stations_ranked(
        vec![
            station("Zeta", "1"),
            station("Mitte", "2"),
            station("delta", "3"),
            station("Alpha", "4"),
        ],
        2,
    );
    let names: Vec<_> = ui.radio.stations.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Zeta", "Mitte", "Alpha", "delta"]);
    assert_eq!(ui.radio.ranked, 2);
    ui.radio.genres = true;
    ui.radio.set_categories(vec![
        crate::RadioCategory {
            id: "pop".into(),
            name: "pop".into(),
            stations: 5,
        },
        crate::RadioCategory {
            id: "hip hop".into(),
            name: "hip hop".into(),
            stations: 5,
        },
    ]);
    let names: Vec<_> = ui
        .radio
        .categories
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, ["Hip Hop", "Pop"]);
    assert_eq!(
        ui.radio.categories[0].id, "hip hop",
        "the directory still gets its tag"
    );
    ui.radio.set_song(Some("Ahmad Jamal - Poinciana"));
    assert_eq!(
        ui.radio.song,
        Some(("Poinciana".into(), "Ahmad Jamal".into()))
    );
    ui.radio.set_song(Some("News at nine"));
    assert_eq!(ui.radio.song, Some(("News at nine".into(), String::new())));
}

#[test]
fn station_info_shows_a_station_and_plays_or_saves_it() {
    use reborn_core::RadioRequest;
    let mut ui = radios();
    ui.radio.online = true;
    let mut m = app(Screen::RadioStations);
    ui.radio.set_stations(vec![station("Jazz One", "a-1")]);
    m.navigation.focus = 0;
    ui.model_action(&mut m, Action::ContextMenu);
    m.navigation.modal_focus = 2;
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::RadioInfo);
    assert_eq!(ui.radio.info.as_ref().map(|s| s.uuid.as_str()), Some("a-1"));
    let labels: Vec<_> = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(labels, ["Play", "Add to Favorites"]);
    m.navigation.focus = 1;
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Radio(RadioRequest::ToggleFavoriteStation(Box::new(station(
            "Jazz One", "a-1"
        ))))
    );
    m.navigation.focus = 0;
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Radio(RadioRequest::PlayStation(Box::new(station(
            "Jazz One", "a-1"
        ))))
    );
    assert_eq!(m.screen, Screen::NowPlaying);
    // Drawn: the facts the directory has, never an empty label.
    let mut m = app(Screen::RadioInfo);
    let mut s = station("Jazz One", "a-1");
    s.language = "german,english".into();
    ui.radio.info = Some(s);
    let text = drawn_text(&ui.draw(&m, &m.library.tracks.clone(), false));
    assert!(
        text.contains("German, English") && text.contains("Jazz One"),
        "{text}"
    );
    m.screen = Screen::Radio;
}

#[test]
fn radio_stations_play_mark_favorites_and_offer_options() {
    use reborn_core::RadioRequest;
    let mut ui = radios();
    ui.radio.online = true;
    let mut m = app(Screen::RadioStations);
    ui.radio.stations = vec![station("Jazz One", "a-1"), station("Rock Two", "b-2")];
    ui.radio.favorites = vec!["b-2".into()];
    let rows = ui.rows(&m, &m.library.tracks);
    // Country and genre only; codec and bitrate are on Station Info.
    assert_eq!(rows[0].secondary, "Germany · Jazz");
    assert_eq!(rows[1].secondary, "Favorite · Germany · Jazz");
    // Hold: options for that station.
    m.navigation.focus = 1;
    ui.model_action(&mut m, Action::ContextMenu);
    let options: Vec<_> = ui
        .modal_rows_public(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(options, ["Play", "Remove from Favorites", "Station Info"]);
    m.navigation.modal_focus = 1;
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Radio(RadioRequest::ToggleFavorite(1))
    );
    // Select plays it and shows Now Playing.
    assert_eq!(
        select_key(&mut ui, &mut m, "station:0"),
        Effect::Radio(RadioRequest::Play(0))
    );
    assert_eq!(m.screen, Screen::NowPlaying);
}

#[test]
fn radio_countries_and_genres_open_their_station_lists() {
    use reborn_core::RadioRequest;
    let mut ui = radios();
    ui.radio.online = true;
    let mut m = app(Screen::Radio);
    assert_eq!(
        select_key(&mut ui, &mut m, "radio_countries"),
        Effect::Radio(RadioRequest::Countries)
    );
    assert_eq!(m.screen, Screen::RadioCategories);
    ui.radio.loading = false;
    ui.radio.categories = vec![crate::RadioCategory {
        id: "AT".into(),
        name: "Austria".into(),
        stations: 420,
    }];
    assert_eq!(ui.rows(&m, &m.library.tracks)[0].secondary, "420 stations");
    assert_eq!(
        select_key(&mut ui, &mut m, "category:0"),
        Effect::Radio(RadioRequest::Country {
            code: "AT".into(),
            name: "Austria".into()
        })
    );
    assert_eq!(
        (m.screen, m.navigation.filter.as_str()),
        (Screen::RadioStations, "Austria")
    );
    // A failed request offers to try again with the same request.
    ui.radio.loading = false;
    ui.radio.error = Some("The server didn't answer.".into());
    assert_eq!(
        select_key(&mut ui, &mut m, "radio_retry"),
        Effect::Radio(RadioRequest::Country {
            code: "AT".into(),
            name: "Austria".into()
        })
    );
}

#[test]
fn the_search_page_previews_while_typing_and_folds_the_keys_into_its_results() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Home);
    m.library.tracks[4].title = "Beat It".into();
    m.library.tracks[4].artist = "Michael Jackson".into();
    let tracks = m.library.tracks.clone();
    select_key(&mut ui, &mut m, "search");
    // The search page, with the keys over it.
    assert_eq!(m.screen, Screen::Search);
    assert_eq!(m.navigation.stack.last(), Some(&Screen::Home));
    assert!(ui.search_preview(&m, &tracks, 3).is_empty());
    // b, e: the song shows above the keys while typing.
    for a in [
        Action::NavigateDown,
        Action::NavigateDown,
        Action::NavigateRight,
        Action::NavigateRight,
        Action::NavigateRight,
        Action::NavigateRight,
        Action::NavigateRight,
        Action::Select,
    ] {
        ui.model_action(&mut m, a);
    }
    assert_eq!(ui.keyboard.as_ref().unwrap().text, "b");
    {
        let k = ui.keyboard.as_mut().unwrap();
        k.row = 0;
        k.col = 2;
    }
    ui.model_action(&mut m, Action::Select);
    assert_eq!(ui.keyboard.as_ref().unwrap().text, "be");
    assert_eq!(m.navigation.filter, "be");
    let preview = ui.search_preview(&m, &tracks, 3);
    assert_eq!(preview[0].key, "track:4");
    assert_eq!(preview[0].secondary, "Song · Michael Jackson");
    assert_eq!(ui.keyboard.as_ref().unwrap().found, 1);
    // Up from the top row: the keys fold away, the first result has the focus.
    ui.model_action(&mut m, Action::NavigateUp);
    assert!(ui.keyboard.is_none());
    assert_eq!(m.screen, Screen::Search);
    let rows = ui.rows(&m, &tracks);
    assert_eq!(rows[m.navigation.focus].key, "track:4");
    assert!(matches!(
        ui.model_action(&mut m, Action::Select),
        Effect::PlayCollection { selected: 4, .. } | Effect::Play(4)
    ));
    // An artist opens its page; Back returns to the search as it was left.
    let mut m = app(Screen::Home);
    select_key(&mut ui, &mut m, "search");
    ui.keyboard.as_mut().unwrap().text = "Élo".into();
    ui.keyboard.as_mut().unwrap().row = 2;
    ui.keyboard.as_mut().unwrap().col = 8; // Delete, then type "o" again
    ui.model_action(&mut m, Action::Select);
    {
        let k = ui.keyboard.as_mut().unwrap();
        k.row = 0;
        k.col = 8;
    }
    ui.model_action(&mut m, Action::Select);
    assert!(ui.search_preview(&m, &tracks, 3)[0]
        .key
        .starts_with("artist:"));
    // The wheel folds the keys away too.
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert!(ui.keyboard.is_none());
    let focus = m.navigation.focus;
    assert!(ui.rows(&m, &tracks)[focus].key.starts_with("artist:"));
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::Artist);
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.screen, Screen::Search);
    assert_eq!(m.navigation.filter, "Élo");
    assert_eq!(m.navigation.focus, focus);
    // Up to the field (over the heading) and Select: the keys come back with
    // the text; Cancel gives the search back as it was.
    ui.model_action(&mut m, Action::WheelCounterClockwise(1));
    assert_eq!(m.navigation.focus, 0, "headings are not stops");
    ui.model_action(&mut m, Action::Select);
    assert_eq!(ui.keyboard.as_ref().unwrap().text, "Élo");
    {
        let k = ui.keyboard.as_mut().unwrap();
        k.row = 2;
        k.col = 8;
    }
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.navigation.filter, "Él");
    ui.model_action(&mut m, Action::Home);
    assert!(ui.keyboard.is_none());
    assert_eq!(m.screen, Screen::Search);
    assert_eq!(m.navigation.filter, "Élo");
    // Back goes up one level (#44): from the results to typing, with the
    // text; from the keys' top row out of the search.
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.screen, Screen::Search);
    assert_eq!(ui.keyboard.as_ref().unwrap().text, "Élo");
    assert_eq!(
        ui.keyboard.as_ref().unwrap().row,
        0,
        "the keys rise on their top row"
    );
    ui.model_action(&mut m, Action::Back);
    assert!(ui.keyboard.is_none());
    assert_eq!(m.screen, Screen::Home);
    // A search left is not offered again: the next one starts empty.
    select_key(&mut ui, &mut m, "search");
    assert_eq!(ui.keyboard.as_ref().unwrap().text, "");
    ui.model_action(&mut m, Action::Home);
    assert!(ui.keyboard.is_none());
    assert_eq!(m.screen, Screen::Home);
}

#[test]
fn the_search_page_lists_each_kind_under_a_heading_with_play_all() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Search);
    let tracks = m.library.tracks.clone();
    m.navigation.filter = "album".into();
    let rows = ui.rows(&m, &tracks);
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys[0], "search_edit");
    assert_eq!(keys[1], "heading:albums");
    assert!(!rows[1].enabled);
    let songs = keys.iter().position(|k| *k == "heading:songs").unwrap();
    assert_eq!(keys[songs + 1], "search_play_all");
    // Nine songs: the first four, then See All for the rest (#44).
    assert_eq!(rows[songs + 1].secondary, "9 songs");
    assert_eq!(rows.last().unwrap().key, "search_all:songs");
    assert_eq!(rows.last().unwrap().label, "See All 9 Songs");
    let shown: Vec<usize> = rows[songs + 2..rows.len() - 1]
        .iter()
        .map(|r| r.key["track:".len()..].parse().unwrap())
        .collect();
    assert_eq!(shown.len(), crate::catalog::SECTION_PREVIEW);
    assert!(rows[0].secondary.ends_with("songs"), "{:?}", rows[0]);
    // Play All plays every song found, in the page's order.
    m.navigation.focus = songs + 1;
    match ui.model_action(&mut m, Action::Select) {
        Effect::PlayCollection {
            members, shuffle, ..
        } => {
            assert_eq!(shuffle, ShuffleMode::Off);
            assert_eq!(members.len(), 9);
            assert_eq!(members[..shown.len()], shown[..]);
        }
        other => panic!("{other:?}"),
    }
    // A long page scrolls so the focus stays in view under its heading.
    m.screen = Screen::Search;
    m.navigation.filter = "a".into();
    m.navigation.focus = 0;
    m.navigation.scroll = 0;
    let count = ui.row_count(&m, &tracks);
    for _ in 0..count {
        ui.model_action(&mut m, Action::WheelClockwise(1));
        let row = &ui.rows(&m, &tracks)[m.navigation.focus];
        assert!(row.enabled, "{row:?}");
    }
    assert_eq!(m.navigation.focus, count - 1);
    assert!(m.navigation.scroll > 0);
    // Nothing found: the field, and the page says so.
    m.navigation.filter = "zzq".into();
    assert_eq!(ui.row_count(&m, &tracks), 1);
    assert_eq!(ui.search_summary(&m, &tracks), ("No matches".into(), 0));
}

#[test]
fn station_search_uses_the_keyboard_and_remembers_the_last_search() {
    use reborn_core::RadioRequest;
    let mut ui = radios();
    ui.radio.online = true;
    let mut m = app(Screen::Radio);
    assert_eq!(select_key(&mut ui, &mut m, "radio_search"), Effect::None);
    assert!(ui.keyboard.is_some());
    // One letter is not enough.
    ui.model_action(&mut m, Action::NavigateDown);
    ui.model_action(&mut m, Action::Select); // "a"
    assert_eq!(ui.model_action(&mut m, Action::ContextMenu), Effect::None);
    assert!(ui.keyboard.is_some());
    ui.model_action(&mut m, Action::NavigateDown);
    ui.model_action(&mut m, Action::NavigateRight);
    ui.model_action(&mut m, Action::NavigateRight);
    ui.model_action(&mut m, Action::NavigateRight);
    ui.model_action(&mut m, Action::Select); // "c" (z x c)
    assert_eq!(
        ui.model_action(&mut m, Action::ContextMenu),
        Effect::Radio(RadioRequest::Search("ac".into()))
    );
    assert!(ui.keyboard.is_none());
    assert_eq!(m.screen, Screen::RadioStations);
    assert!(ui.radio.loading);
    // Reopened, the keyboard starts from the last search.
    m.screen = Screen::Radio;
    select_key(&mut ui, &mut m, "radio_search");
    assert_eq!(ui.keyboard.as_ref().unwrap().text, "ac");
}

#[test]
fn now_playing_radio_offers_favorite_and_stop_only() {
    use reborn_core::RadioRequest;
    let mut ui = radios();
    let mut m = app(Screen::NowPlaying);
    m.replace_queue(vec![Track::radio(&station("Jazz One", "a-1"))], 0)
        .unwrap();
    ui.model_action(&mut m, Action::Select);
    let options: Vec<_> = ui
        .modal_rows_public(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(options, ["Add to Favorites", "Stop Radio"]);
    m.navigation.modal_focus = 0;
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Radio(RadioRequest::ToggleFavoriteCurrent)
    );
}

fn playlist(path: &str, locations: &[&str]) -> reborn_core::playlist::Playlist {
    reborn_core::playlist::Playlist {
        path: path.into(),
        name: std::path::Path::new(path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        entries: locations
            .iter()
            .map(|l| reborn_core::playlist::PlaylistEntry {
                location: (*l).into(),
                ..Default::default()
            })
            .collect(),
        stamp: (1, 1),
        ..Default::default()
    }
}

fn playlists_app() -> (Ui, AppModel) {
    let mut ui = Ui::default();
    let mut m = app(Screen::Playlists);
    m.settings.music_directory = "/data/music".into();
    ui.playlists.set_lists(vec![
        playlist(
            "/data/music/Playlists/Road Trip.m3u8",
            &[
                "../album0/song1.flac",
                r"C:\PC\Music\album1\song4.flac",
                "../gone/Lost Song.mp3",
                "../album0/song2.flac",
            ],
        ),
        playlist(
            "/data/music/Playlists/Favorites.m3u8",
            &["../album2/song7.flac"],
        ),
        playlist("/data/music/Mixes/another.m3u", &[]),
    ]);
    (ui, m)
}

#[test]
fn playlists_section_lists_favorites_first_then_a_to_z_and_opens_a_playlist() {
    let (mut ui, mut m) = playlists_app();
    let labels: Vec<_> = ui
        .rows(&m, &m.library.tracks.clone())
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(
        labels,
        ["New Playlist", "Favorites", "another", "Road Trip"]
    );
    select_key(&mut ui, &mut m, "playlist:2");
    assert_eq!(m.screen, Screen::Playlist);
    let rows = ui.rows(&m, &m.library.tracks.clone());
    let keys: Vec<_> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "pl_play",
            "pl_shuffle",
            "pl_entry:0",
            "pl_entry:1",
            "pl_entry:2",
            "pl_entry:3"
        ]
    );
    assert_eq!(rows[2].art, Some(1), "relative path");
    assert_eq!(rows[3].art, Some(4), "a Windows path from a computer");
    assert_eq!(rows[4].label, "Lost Song");
    assert_eq!(rows[4].secondary, "Not on the player");
    // Play: the songs it has, in order; the missing one is skipped.
    assert_eq!(
        select_key(&mut ui, &mut m, "pl_play"),
        Effect::PlayCollection {
            selected: 1,
            members: vec![1, 4, 2],
            shuffle: ShuffleMode::Off,
            source: "Road Trip".into()
        }
    );
    m.screen = Screen::Playlist;
    m.navigation.filter = "/data/music/Playlists/Road Trip.m3u8".into();
    assert_eq!(
        select_key(&mut ui, &mut m, "pl_entry:2"),
        Effect::None,
        "missing: a notice"
    );
}

#[test]
fn songs_are_removed_and_moved_in_their_playlist_by_place() {
    use reborn_core::{playlist::PlaylistEdit, PlaylistRequest};
    let (mut ui, mut m) = playlists_app();
    m.screen = Screen::Playlist;
    m.navigation.filter = "/data/music/Playlists/Road Trip.m3u8".into();
    let path = std::path::PathBuf::from(&m.navigation.filter);
    // Hold Select on the third song: Remove from Playlist removes that entry.
    m.navigation.focus = 4;
    ui.model_action(&mut m, Action::ContextMenu);
    let options: Vec<_> = ui
        .modal_rows_public(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.key)
        .collect();
    assert!(options.contains(&"ple_move".to_owned()) && options.contains(&"ple_remove".to_owned()));
    m.navigation.modal_focus = options.iter().position(|k| k == "ple_remove").unwrap();
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Playlist(Box::new(PlaylistRequest::Edit {
            path: path.clone(),
            edit: PlaylistEdit::Remove {
                index: 2,
                location: "../gone/Lost Song.mp3".into()
            },
            create: false,
        }))
    );
    // Move the first song two places down with the wheel; Select drops it.
    m.navigation.focus = 2;
    ui.model_action(&mut m, Action::ContextMenu);
    let options: Vec<_> = ui
        .modal_rows_public(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.key)
        .collect();
    m.navigation.modal_focus = options.iter().position(|k| k == "ple_move").unwrap();
    ui.model_action(&mut m, Action::Select);
    assert_eq!(ui.playlists.moving, Some((0, 0)));
    ui.model_action(&mut m, Action::WheelClockwise(1));
    ui.model_action(&mut m, Action::WheelClockwise(1));
    let keys: Vec<_> = ui
        .rows(&m, &m.library.tracks.clone())
        .into_iter()
        .map(|r| r.key)
        .collect();
    assert_eq!(
        &keys[2..],
        ["pl_entry:1", "pl_entry:2", "pl_entry:0", "pl_entry:3"]
    );
    assert_eq!(m.navigation.focus, 4, "the moving song keeps the focus");
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Playlist(Box::new(PlaylistRequest::Edit {
            path,
            edit: PlaylistEdit::Move {
                from: 0,
                to: 2,
                location: "../album0/song1.flac".into()
            },
            create: false,
        }))
    );
    assert_eq!(ui.playlists.moving, None);
    // Back while moving puts it back without a change.
    ui.playlists.moving = Some((1, 3));
    assert_eq!(ui.model_action(&mut m, Action::Back), Effect::None);
    assert_eq!((ui.playlists.moving, m.screen), (None, Screen::Playlist));
}

#[test]
fn add_to_playlist_from_a_song_new_playlist_and_the_favorites_heart() {
    use reborn_core::{playlist::PlaylistEdit, PlaylistRequest};
    let (mut ui, mut m) = playlists_app();
    m.screen = Screen::Tracks;
    m.navigation.filter.clear();
    let tracks = m.library.tracks.clone();
    let row = ui
        .rows(&m, &tracks)
        .iter()
        .position(|r| r.key == "track:5")
        .unwrap();
    m.navigation.focus = row;
    ui.model_action(&mut m, Action::ContextMenu);
    let options: Vec<_> = ui
        .modal_rows_public(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.key)
        .collect();
    assert!(options.contains(&"fav_toggle".to_owned()));
    m.navigation.modal_focus = options.iter().position(|k| k == "add_playlist").unwrap();
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.navigation.modal, Some(reborn_core::Modal::PlaylistPicker));
    let picks: Vec<_> = ui
        .modal_rows_public(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(
        picks,
        ["New Playlist…", "Favorites", "another", "Road Trip"]
    );
    m.navigation.modal_focus = 3;
    let effect = ui.model_action(&mut m, Action::Select);
    assert_eq!(
        effect,
        Effect::Playlist(Box::new(PlaylistRequest::Edit {
            path: "/data/music/Playlists/Road Trip.m3u8".into(),
            edit: PlaylistEdit::Append(vec![reborn_core::playlist::PlaylistEntry {
                location: "../album1/song5.flac".into(),
                info: Some(format!(
                    "#EXTINF:200,{} - Song 5",
                    m.library.tracks[5].artist
                )),
                extra: vec![],
            }]),
            create: true,
        }))
    );
    // The picker now offers Road Trip first (used last).
    ui.open_picker(&mut m, vec![5]);
    let picks: Vec<_> = ui
        .modal_rows_public(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(picks[1], "Road Trip");
    // New Playlist…: name it on the keyboard; the song goes in.
    m.navigation.modal_focus = 0;
    ui.model_action(&mut m, Action::Select);
    assert!(ui.keyboard.is_some());
    ui.keyboard.as_mut().unwrap().text = "Late Night".into();
    assert_eq!(
        ui.model_action(&mut m, Action::ContextMenu),
        Effect::Playlist(Box::new(PlaylistRequest::Create {
            name: "Late Night".into(),
            tracks: vec![5]
        }))
    );
    // The heart: song 7 is a favorite (removed), song 5 is not (added).
    assert!(ui.is_favorite_track(&m, &tracks, 7));
    assert_eq!(
        ui.toggle_favorite_track(&m, &tracks, 7),
        Effect::Playlist(Box::new(PlaylistRequest::Edit {
            path: "/data/music/Playlists/Favorites.m3u8".into(),
            edit: PlaylistEdit::RemoveLocations(vec!["../album2/song7.flac".into()]),
            create: true,
        }))
    );
    assert!(matches!(
        ui.toggle_favorite_track(&m, &tracks, 5),
        Effect::Playlist(request) if matches!(*request, PlaylistRequest::Edit { edit: PlaylistEdit::Append(_), .. })
    ));
}

#[test]
fn deleting_a_playlist_asks_first_and_favorites_cannot_be_deleted() {
    use reborn_core::PlaylistRequest;
    let (mut ui, mut m) = playlists_app();
    // Favorites (row 1): no Rename, no Delete.
    m.navigation.focus = 1;
    ui.model_action(&mut m, Action::ContextMenu);
    let options: Vec<_> = ui
        .modal_rows_public(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(
        options,
        [
            "Play",
            "Shuffle",
            "Shuffle Albums",
            "Play Next",
            "Add to Queue",
            "Sort",
            "Duplicate"
        ]
    );
    ui.model_action(&mut m, Action::Back);
    m.navigation.focus = 3;
    ui.model_action(&mut m, Action::ContextMenu);
    let options: Vec<_> = ui
        .modal_rows_public(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.key)
        .collect();
    m.navigation.modal_focus = options.iter().position(|k| k == "pl_delete").unwrap();
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    assert_eq!(
        m.navigation.modal,
        Some(reborn_core::Modal::Confirm(
            reborn_core::ConfirmAction::DeletePlaylist
        ))
    );
    let confirm: Vec<_> = ui
        .modal_rows_public(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.key)
        .collect();
    m.navigation.modal_focus = confirm.iter().position(|k| k != "cancel").unwrap();
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Playlist(Box::new(PlaylistRequest::Delete(
            "/data/music/Playlists/Road Trip.m3u8".into()
        )))
    );
}

/// Timings for a 20 000-entry playlist on a 20 000-song library
/// (`cargo test -p reborn-ui --release -- --ignored --nocapture playlist_speed`).
#[test]
#[ignore]
fn playlist_speed_on_a_large_playlist() {
    use std::time::Instant;
    let tracks: Vec<Track> = (0..20_000)
        .map(|i| Track {
            id: i,
            title: format!("Song {i}"),
            artist: format!("Artist {}", i % 400),
            album: format!("Album {}", i / 10),
            path: format!(
                "/data/music/Artist {}/Album {}/{i:05} Song.flac",
                i % 400,
                i / 10
            )
            .into(),
            duration_ms: 200_000,
            online: true,
            ..Default::default()
        })
        .collect();
    let locations: Vec<String> = (0..20_000)
        .map(|i| match i % 3 {
            0 => format!("../Artist {}/Album {}/{i:05} Song.flac", i % 400, i / 10),
            1 => format!(
                r"C:\Users\me\Music\Artist {}\Album {}\{i:05} Song.flac",
                i % 400,
                i / 10
            ),
            _ => format!("../Gone/{i} Missing.mp3"),
        })
        .collect();
    let refs: Vec<&str> = locations.iter().map(String::as_str).collect();
    let mut ui = Ui::default();
    let mut m = app(Screen::Playlist);
    m.library.tracks = tracks;
    m.settings.music_directory = "/data/music".into();
    m.navigation.filter = "/data/music/Playlists/Big.m3u8".into();
    ui.playlists
        .set_lists(vec![playlist("/data/music/Playlists/Big.m3u8", &refs)]);
    let tracks = m.library.tracks.clone();
    let started = Instant::now();
    let count = ui.row_count(&m, &tracks);
    eprintln!(
        "first open (match + rows): {:?} for {count} rows",
        started.elapsed()
    );
    let started = Instant::now();
    for _ in 0..10 {
        let _ = ui.row_count(&m, &tracks);
        let _ = ui.visible_rows(&m, &tracks, 5_000, 6);
    }
    eprintln!("a frame's lookups after that: {:?}", started.elapsed() / 10);
    let resolved = ui.resolved(&tracks, &ui.playlists.lists[0].clone());
    assert_eq!(resolved.iter().filter(|r| r.is_some()).count(), 13_334);
}

#[test]
fn motion_waves_while_playing_slides_screens_and_glides_focus() {
    // Beta 7.2 UI motion, as the player runs it.
    let mut ui = Ui {
        ui_motion: true,
        ..radios()
    };
    let mut m = app(Screen::NowPlaying);
    let warped = |q: &[reborn_graphics::Quad]| q.iter().filter(|q| q.warp.is_some()).count();
    m.playback = reborn_core::PlaybackState::Playing;
    m.position_ms = 60_000;
    let frame = ui.draw(&m, &m.library.tracks, false);
    assert!(warped(&frame) > 20, "the played part waves");
    assert!(
        ui.gliding(),
        "the wave keeps the full frame rate while playing"
    );
    // …without the Interactive CPU lease: its frames fit the lowest clock.
    ui.draw(&m, &m.library.tracks, false);
    assert!(!ui.responding(), "the wave alone takes no CPU boost");
    m.playback = reborn_core::PlaybackState::Paused;
    // Paused: the wave settles flat and the frame rate drops back.
    for _ in 0..60 {
        ui.draw(&m, &m.library.tracks, false);
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    let flat = ui.draw(&m, &m.library.tracks, false);
    let ys: Vec<f32> = flat
        .iter()
        .filter_map(|q| q.warp.map(|w| w.corners[0][1]))
        .collect();
    let spread =
        ys.iter().cloned().fold(f32::MIN, f32::max) - ys.iter().cloned().fold(f32::MAX, f32::min);
    assert!(spread < 1.5, "flat when paused: {spread}");

    // A list: the extra rows stay hidden at rest; the focus glides.
    let mut m = app(Screen::Tracks);
    let tracks = m.library.tracks.clone();
    m.navigation.focus = 2;
    ui.draw(&m, &tracks, false);
    // Coming from Now Playing, the list slides in first.
    assert!(ui.gliding());
    std::thread::sleep(std::time::Duration::from_millis(220));
    ui.draw(&m, &tracks, false);
    ui.draw(&m, &tracks, false);
    assert!(!ui.gliding(), "nothing moves at rest");
    let rest = ui.draw(&m, &tracks, false);
    // The rows drawn to scroll in are invisible at rest.
    assert!(rest
        .iter()
        .filter(|q| q.color & 0xFF != 0)
        .all(|q| q.y < 360. && q.y + q.h > 0.));
    let focus_y = |q: &[reborn_graphics::Quad]| q.iter().find(|q| q.focus_target).unwrap().y;
    let before = focus_y(&ui.draw(&m, &tracks, false));
    ui.model_action(&mut m, Action::NavigateDown);
    let during = focus_y(&ui.draw(&m, &tracks, false));
    assert!(
        during >= before && during < before + 53.,
        "{before} → {during}"
    );
    assert!(ui.gliding());

    // A new screen slides in over the last one; one focus target only.
    let mut m = app(Screen::Home);
    ui.draw(&m, &tracks, false);
    m.navigation.focus = 1;
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::Music);
    let sliding = ui.draw(&m, &tracks, false);
    assert_eq!(crate::focus_target_count(&sliding), 1);
    assert!(ui.gliding(), "the slide runs at the full frame rate");
    std::thread::sleep(std::time::Duration::from_millis(220));
    ui.draw(&m, &tracks, false);
    let rested = ui.draw(&m, &tracks, false);
    assert!(!ui.gliding() && crate::focus_target_count(&rested) == 1);
}

#[test]
fn a_long_wheel_jump_glides_only_its_last_row() {
    let ui = Ui {
        ui_motion: true,
        ..radios()
    };
    let mut m = app(Screen::Tracks);
    let tracks = m.library.tracks.clone();
    ui.draw(&m, &tracks, false);
    m.navigation.focus = tracks.len() - 1;
    ui.normalize(&mut m, &tracks);
    let frame = ui.draw(&m, &tracks, false);
    // The focused (last) row is on screen at once, not a long slide away.
    let focus = frame.iter().find(|q| q.focus_target).unwrap();
    assert!(focus.y > 0. && focus.y < 360., "{}", focus.y);
}

#[test]
fn the_update_download_draws_at_the_full_frame_rate_without_the_cpu_boost() {
    // Beta 7.6: the download drew at the spinners' calm rate (≤ 29 fps).
    let mut ui = Ui {
        ui_motion: true,
        ..radios()
    };
    let mut m = app(Screen::Settings);
    m.platform.busy = Some(reborn_core::PlatformTask::UpdateStage);
    ui.observe(&m);
    assert!(!ui.gliding(), "a download behind another screen stays calm");
    m.screen = Screen::Update;
    ui.observe(&m);
    assert!(ui.gliding(), "the visible download runs at the full rate");
    assert!(!ui.responding(), "and leaves the CPU clock to the download");
    m.screen_off = true;
    ui.observe(&m);
    assert!(!ui.gliding(), "nothing draws with the screen off");
    m.screen_off = false;
    m.platform.busy = None;
    ui.observe(&m);
    assert!(!ui.gliding(), "done: back to rest");
}

#[test]
fn the_download_percentage_counts_with_the_bar() {
    let ui = Ui {
        ui_motion: true,
        ..radios()
    };
    let mut m = app(Screen::Update);
    m.platform.busy = Some(reborn_core::PlatformTask::UpdateStage);
    let live = |done| reborn_core::platform::DownloadLive {
        verifying: false,
        bytes_done: done,
        bytes_total: 100_000_000,
        speed_bytes_per_s: Some(3_000_000),
        seconds_left: Some(20),
        check_percent: None,
    };
    m.platform.download = Some(live(10_000_000));
    // The bar starts empty and glides; the number follows it, not the report.
    let first = drawn_text(&ui.draw(&m, &m.library.tracks, false));
    assert!(!first.contains("10%"), "{first}");
    for _ in 0..80 {
        ui.draw(&m, &m.library.tracks, false);
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    let caught_up = drawn_text(&ui.draw(&m, &m.library.tracks, false));
    assert!(caught_up.contains("10%"), "{caught_up}");
}

#[test]
fn lastfm_settings_follow_the_account_and_the_sign_in() {
    use crate::LastFmView;
    use reborn_core::LastFmRequest;
    let mut ui = radios();
    let mut m = app(Screen::Settings);
    let row = |ui: &Ui, m: &AppModel| {
        ui.rows(m, &m.library.tracks)
            .into_iter()
            .find(|r| r.key == "lastfm")
            .expect("Last.fm row")
    };
    assert_eq!(row(&ui, &m).secondary, "Off");
    m.settings.scrobble_log = true;
    assert_eq!(row(&ui, &m).secondary, "Log file");
    // Nine places fit on the Settings root (Beta 8.18: Customize).
    assert_eq!(ui.rows(&m, &m.library.tracks).len(), 9);
    assert_eq!(select_key(&mut ui, &mut m, "lastfm"), Effect::None);
    assert_eq!(m.screen, Screen::LastFm);
    // A build without the app key: the log still works.
    let page = crate::pages::page(&ui, &m, &m.library.tracks.clone());
    assert_eq!(page.hero.expect("no key").title, "Last.fm Isn't Available");
    assert_eq!(page.rows[0].key, "scrobble_log");
    assert_eq!(
        select_key(&mut ui, &mut m, "scrobble_log"),
        Effect::SetScrobbleLog(false)
    );
    // Not signed in: phone first, password second, the log.
    ui.lastfm = LastFmView {
        available: true,
        ..Default::default()
    };
    let keys: Vec<_> = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.key)
        .collect();
    assert_eq!(keys, ["lastfm_connect", "lastfm_password", "scrobble_log"]);
    ui.lastfm.waiting = 2;
    assert_eq!(
        ui.rows(&m, &m.library.tracks)[0].secondary,
        "2 plays waiting",
        "a session Last.fm ended keeps its plays for the next sign-in"
    );
    ui.lastfm.waiting = 0;
    assert_eq!(
        select_key(&mut ui, &mut m, "lastfm_connect"),
        Effect::LastFm(LastFmRequest::Connect)
    );
    // Signing in: the code page with Cancel; the QR code is drawn.
    ui.lastfm.approval = Some(format!(
        "https://www.last.fm/api/auth/?api_key={}&token={}",
        "a".repeat(32),
        "b".repeat(32)
    ));
    let page = crate::pages::page(&ui, &m, &m.library.tracks.clone());
    assert_eq!(page.hero.expect("code").title, "Scan to Sign In");
    assert_eq!(
        select_key(&mut ui, &mut m, "lastfm_cancel"),
        Effect::LastFm(LastFmRequest::Cancel)
    );
    let draw = ui.draw(&m, &m.library.tracks, true);
    assert!(drawn_text(&draw).contains("Allow Y2Reborn"));
    // Signed in: the account, its switches, what waits, Disconnect.
    ui.lastfm = LastFmView {
        available: true,
        user: Some("luca".into()),
        waiting: 3,
        ..Default::default()
    };
    let rows = ui.rows(&m, &m.library.tracks);
    let keys: Vec<_> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "lastfm_account",
            "lastfm_scrobble",
            "lastfm_now_playing",
            "lastfm_send",
            "scrobble_log",
            "lastfm_disconnect"
        ]
    );
    assert_eq!(rows[0].secondary, "luca");
    assert_eq!(rows[3].secondary, "3 plays waiting");
    assert_eq!(
        select_key(&mut ui, &mut m, "lastfm_scrobble"),
        Effect::SetLastFmScrobble(false)
    );
    assert_eq!(
        select_key(&mut ui, &mut m, "lastfm_now_playing"),
        Effect::SetLastFmNowPlaying(false)
    );
    assert_eq!(
        select_key(&mut ui, &mut m, "lastfm_send"),
        Effect::LastFm(LastFmRequest::SendNow)
    );
    assert_eq!(
        select_key(&mut ui, &mut m, "lastfm_disconnect"),
        Effect::LastFm(LastFmRequest::Disconnect)
    );
    m.screen = Screen::Settings;
    assert_eq!(row(&ui, &m).secondary, "luca");
}

#[test]
fn lastfm_password_sign_in_never_shows_or_prints_the_password() {
    use crate::LastFmView;
    use reborn_core::LastFmRequest;
    let mut ui = radios();
    ui.lastfm = LastFmView {
        available: true,
        ..Default::default()
    };
    let mut m = app(Screen::LastFm);
    assert_eq!(select_key(&mut ui, &mut m, "lastfm_password"), Effect::None);
    ui.keyboard.as_mut().unwrap().text = "luca".into();
    assert_eq!(ui.model_action(&mut m, Action::ContextMenu), Effect::None);
    assert!(matches!(
        ui.keyboard.as_ref().map(|k| &k.purpose),
        Some(crate::keyboard::Purpose::LastFmPassword { .. })
    ));
    ui.keyboard.as_mut().unwrap().text = "hunter22".into();
    let draw = ui.draw(&m, &m.library.tracks, false);
    assert!(!drawn_text(&draw).contains("hunter22"));
    let effect = ui.model_action(&mut m, Action::ContextMenu);
    assert_eq!(
        effect,
        Effect::LastFm(LastFmRequest::Login {
            username: "luca".into(),
            password: "hunter22".into()
        })
    );
    assert!(!format!("{effect:?}").contains("hunter22"));
    assert!(ui.keyboard.is_none());
}

#[test]
fn internal_storage_setup_is_offered_only_for_a_waiting_old_library_and_confirmed_red() {
    use reborn_core::platform::{InternalSetup, SetupWait};
    let mut ui = radios();
    let mut m = app(Screen::Storage);
    assert!(!ui
        .rows(&m, &[])
        .iter()
        .any(|r| r.key == "confirm:internal_setup"));
    m.platform.snapshot.internal_setup = InternalSetup::Pending(SetupWait::Restart);
    assert!(visible_text(&ui, &m).contains("Restart to finish setting up"));
    // Report 20261019-153610: restarting on a low battery never sets it up.
    m.platform.snapshot.internal_setup = InternalSetup::Pending(SetupWait::Battery);
    assert!(visible_text(&ui, &m).contains("Connect a charger and restart"));
    m.platform.snapshot.internal_setup = InternalSetup::Waiting {
        files: 1234,
        bytes: 3_100_000_000,
    };
    let row = ui
        .rows(&m, &[])
        .into_iter()
        .find(|r| r.key == "confirm:internal_setup")
        .expect("Set Up Internal Storage row");
    assert!(row.label.contains("Set Up Internal Storage"));
    assert!(visible_text(&ui, &m).contains("1234 old files"));
    assert_eq!(
        select_key(&mut ui, &mut m, "confirm:internal_setup"),
        Effect::None
    );
    let (title, body) = ui.modal_copy(&m);
    assert_eq!(title, "Set Up Internal Storage?");
    assert!(body.contains("erased") && body.contains("Old Storage") && body.contains("restarts"));
    assert!(crate::confirm_is_danger(ConfirmAction::Platform(
        PlatformTask::InternalStorageSetup
    )));
    assert!(!crate::confirm_is_danger(ConfirmAction::Platform(
        PlatformTask::StorageBenchmark
    )));
    // Cancel has the focus; Set Up Now runs the platform task.
    assert_eq!(m.navigation.modal_focus, 0);
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Platform(PlatformTask::InternalStorageSetup)
    );
    // A stale question cannot erase anything once the platform no longer waits.
    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::Platform(
        PlatformTask::InternalStorageSetup,
    )));
    m.platform.snapshot.internal_setup = InternalSetup::NotNeeded;
    assert!(!ui.modal_rows_public(&m, &[])[1].enabled);
}

#[test]
fn a_card_with_errors_plays_and_offers_a_confirmed_repair() {
    use reborn_core::platform::{SdCard, VolumeSpace};
    let mut ui = Ui::default();
    let mut m = app(Screen::Storage);
    m.platform.snapshot.observed = true;
    m.platform.snapshot.storage.sd = SdCard::Ready(VolumeSpace::default());
    assert!(!ui
        .rows(&m, &[])
        .iter()
        .any(|r| r.key == "confirm:sd_repair"));
    m.platform.snapshot.storage.sd = SdCard::ReadOnly(VolumeSpace::default());
    assert!(visible_text(&ui, &m).contains("Has errors · Playing only"));
    let row = ui
        .rows(&m, &[])
        .into_iter()
        .find(|r| r.key == "confirm:sd_repair")
        .expect("Repair SD Card row");
    assert_eq!(row.label, "Repair SD Card");
    assert_eq!(
        select_key(&mut ui, &mut m, "confirm:sd_repair"),
        Effect::None
    );
    let (title, body) = ui.modal_copy(&m);
    assert_eq!(title, "Repair SD Card?");
    assert!(body.contains("shortened or removed") && body.contains("30 %"));
    assert!(crate::confirm_is_danger(ConfirmAction::Platform(
        PlatformTask::SdRepair
    )));
    // Cancel has the focus; Repair runs the platform task.
    assert_eq!(m.navigation.modal_focus, 0);
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Platform(PlatformTask::SdRepair)
    );
    // A stale question cannot run once the card is fine (or gone).
    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::Platform(
        PlatformTask::SdRepair,
    )));
    m.platform.snapshot.storage.sd = SdCard::Ready(VolumeSpace::default());
    assert!(!ui.modal_rows_public(&m, &[])[1].enabled);
    // A refused card (ext4 with errors) and an unfinished check are repairable too.
    m.navigation.modal = None;
    m.platform.snapshot.storage.sd = SdCard::Unchecked(VolumeSpace::default());
    assert!(visible_text(&ui, &m).contains("Not checked"));
    assert!(ui
        .rows(&m, &[])
        .iter()
        .any(|r| r.key == "confirm:sd_repair"));
}

#[test]
fn radio_firmware_page_names_the_right_place_and_a_space_refusal() {
    use reborn_core::platform::RadioFirmware;
    let ui = radios();
    let mut m = app(Screen::Wifi);
    m.platform.snapshot.radio_firmware = RadioFirmware::Missing;
    assert!(visible_text(&ui, &m).contains("Firmware drive"));
    m.platform.snapshot.firmware_in_internal = true;
    assert!(visible_text(&ui, &m).contains("Firmware folder on Internal Storage"));
    m.platform.snapshot.firmware_no_space = true;
    let text = visible_text(&ui, &m);
    assert!(text.contains("Not Enough Space") && text.contains("SD card"));
}

#[test]
fn error_reports_page_switches_sends_and_names_the_last_report() {
    use reborn_core::platform::{ReportLast, ReportState};
    let mut ui = radios();
    // About & Updates names the state; an older platform has no row.
    let mut m = app(Screen::Update);
    m.platform.snapshot.observed = true;
    let labels = |ui: &Ui, m: &AppModel| -> Vec<String> {
        ui.rows(m, &m.library.tracks)
            .into_iter()
            .map(|r| r.label)
            .collect()
    };
    assert!(!labels(&ui, &m).contains(&"Error Reports".to_owned()));
    m.platform.snapshot.reports = Some(ReportState {
        upload: true,
        ..Default::default()
    });
    let row = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .find(|r| r.label == "Error Reports")
        .unwrap();
    assert_eq!(row.secondary, "Not Set");
    assert_eq!(select_key(&mut ui, &mut m, "error_reports"), Effect::None);
    assert_eq!(m.screen, Screen::ErrorReports);
    // Off: the switch turns it on; nothing can be sent yet; no ID shown.
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(rows[0].kind, RowKind::Toggle(false));
    assert!(
        !rows
            .iter()
            .find(|r| r.key == "report_send")
            .unwrap()
            .enabled
    );
    assert!(!rows.iter().any(|r| r.label == "Install ID"));
    assert!(visible_text(&ui, &m).contains("nothing is collected or sent"));
    assert_eq!(
        select_key(&mut ui, &mut m, "report_toggle"),
        Effect::Platform(PlatformTask::ReportEnable(true))
    );
    // On, with a queued report.
    m.platform.snapshot.reports = Some(ReportState {
        enabled: Some(true),
        install_id: Some("da2c48ea-218a-40c6-b0fc-144cbff777de".into()),
        queued: 1,
        last: Some(ReportLast {
            trigger: "reborn_crash".into(),
            state: "queued".into(),
            ..Default::default()
        }),
        upload: true,
        destinations: Vec::new(),
    });
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(rows[0].kind, RowKind::Toggle(true));
    assert_eq!(
        rows.iter()
            .find(|r| r.label == "Install ID")
            .unwrap()
            .secondary,
        "DA2C48EA"
    );
    assert!(visible_text(&ui, &m).contains("player app crash"));
    assert_eq!(
        select_key(&mut ui, &mut m, "report_toggle"),
        Effect::Platform(PlatformTask::ReportEnable(false))
    );
    assert_eq!(select_key(&mut ui, &mut m, "report_info"), Effect::None);
    let text = visible_text(&ui, &m);
    assert!(
        text.contains("Never your music") && text.contains("kept 180 days") && !text.contains('…')
    );
    m.navigation.filter.clear();
    // A new ID is confirmed first.
    assert_eq!(
        select_key(&mut ui, &mut m, "confirm:report_new_id"),
        Effect::None
    );
    assert_eq!(
        m.navigation.modal,
        Some(Modal::Confirm(ConfirmAction::Platform(
            PlatformTask::ReportNewId
        )))
    );
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Platform(PlatformTask::ReportNewId)
    );
    // Send Report Now: at once, no topic (every report holds everything); the
    // page shows the progress, with nothing to press meanwhile.
    assert_eq!(
        select_key(&mut ui, &mut m, "report_send"),
        Effect::Platform(PlatformTask::ReportSend)
    );
    assert_eq!(m.screen, Screen::ErrorReports);
    m.platform.busy = Some(PlatformTask::ReportSend);
    assert!(visible_text(&ui, &m).contains("Preparing the Report"));
    assert!(ui.rows(&m, &m.library.tracks).is_empty());
    m.platform.busy = None;
    // What became of the last report, in the uploader's words.
    for (state, text) in [
        ("sent", "Last report (sent by you) sent."),
        ("failed", "Last report (sent by you) couldn't be sent yet."),
        ("dropped", "Last report (sent by you) couldn't be sent."),
    ] {
        if let Some(r) = m.platform.snapshot.reports.as_mut() {
            r.queued = 0;
            r.last = Some(ReportLast {
                trigger: "manual".into(),
                state: state.into(),
                ..Default::default()
            });
        }
        assert!(visible_text(&ui, &m).contains(text), "{state}");
    }
}

#[test]
fn without_an_uploader_reports_are_saved_to_the_chosen_storage() {
    use reborn_core::platform::{ReportDestination, ReportLast, ReportState};
    let mut ui = radios();
    let mut m = app(Screen::Update);
    m.platform.snapshot.observed = true;
    m.platform.snapshot.reports = Some(ReportState {
        destinations: vec![ReportDestination::Internal, ReportDestination::Sd],
        ..Default::default()
    });
    // No upload: no On/Off on About & Updates, no switch, no Send Report Now.
    let row = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .find(|r| r.label == "Error Reports")
        .unwrap();
    assert_eq!(row.secondary, "");
    assert_eq!(select_key(&mut ui, &mut m, "error_reports"), Effect::None);
    let labels: Vec<_> = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(labels, ["Save Report", "What Is Saved"]);
    assert!(visible_text(&ui, &m).contains("for you to send"));
    assert_eq!(select_key(&mut ui, &mut m, "report_info"), Effect::None);
    let text = visible_text(&ui, &m);
    assert!(text.contains("Attach the file") && !text.contains("Wi-Fi, kept"));
    ui.model_action(&mut m, Action::Back);
    // Save Report: where (no topic: a saved report has everything), then back
    // on the page itself.
    assert_eq!(select_key(&mut ui, &mut m, "report_save"), Effect::None);
    let labels: Vec<_> = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(labels, ["Internal Storage", "SD Card"]);
    assert_eq!(crate::screens::screen_title(&m), "Save Report");
    assert_eq!(
        select_key(&mut ui, &mut m, "report_dest:sd"),
        Effect::Platform(PlatformTask::ReportSave(ReportDestination::Sd))
    );
    assert_eq!(
        (m.screen, m.navigation.filter.as_str()),
        (Screen::ErrorReports, "")
    );
    m.platform.busy = Some(PlatformTask::ReportSave(ReportDestination::Sd));
    assert!(visible_text(&ui, &m).contains("Preparing the Report"));
    m.platform.busy = None;
    m.platform.snapshot.reports = Some(ReportState {
        destinations: vec![ReportDestination::Internal],
        last: Some(ReportLast {
            trigger: "manual".into(),
            state: "saved".into(),
            destination: Some(ReportDestination::Sd),
            ..Default::default()
        }),
        ..Default::default()
    });
    assert!(visible_text(&ui, &m).contains("saved to SD Card"));
    // Back leaves the page for About & Updates, not the pickers.
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.screen, Screen::Update);
    // Nothing writable: the picker says so.
    m.platform.snapshot.reports = Some(ReportState::default());
    assert_eq!(select_key(&mut ui, &mut m, "error_reports"), Effect::None);
    assert_eq!(select_key(&mut ui, &mut m, "report_save"), Effect::None);
    assert!(visible_text(&ui, &m).contains("No Storage Available"));
}

#[test]
fn the_error_report_question_records_no_as_well_as_yes() {
    let mut ui = radios();
    for (step, expected) in [(0, false), (1, true)] {
        let mut m = app(Screen::Home);
        m.navigation.modal = Some(Modal::Confirm(ConfirmAction::ReportsQuestion));
        m.navigation.modal_focus = 0;
        let rows = ui.modal_rows_public(&m, &[]);
        let labels: Vec<_> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["No Thanks", "Send Reports"]);
        assert!(ui.modal_copy(&m).0.contains("Help Improve Y2Linux"));
        for _ in 0..step {
            ui.model_action(&mut m, Action::WheelClockwise(1));
        }
        assert_eq!(
            ui.model_action(&mut m, Action::Select),
            Effect::Platform(PlatformTask::ReportEnable(expected))
        );
        assert_eq!(m.navigation.modal, None);
    }
}

#[test]
fn the_loud_volume_warning_defaults_to_cancel_and_allows_on_confirm() {
    let mut ui = radios();
    for (step, expected) in [(0, Effect::None), (1, Effect::AllowLoudVolume)] {
        let mut m = app(Screen::Home);
        m.navigation.modal = Some(Modal::Confirm(ConfirmAction::LoudVolume));
        m.navigation.modal_focus = 0;
        let rows = ui.modal_rows_public(&m, &[]);
        let labels: Vec<_> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Cancel", "Raise Volume"]);
        assert!(ui.modal_copy(&m).1.contains("damage your hearing"));
        for _ in 0..step {
            ui.model_action(&mut m, Action::WheelClockwise(1));
        }
        assert_eq!(ui.model_action(&mut m, Action::Select), expected);
        assert_eq!(m.navigation.modal, None);
    }
}

#[test]
fn a_running_report_shows_its_steps_percent_and_what_happens() {
    use reborn_core::platform::{ReportDestination, ReportLive, ReportState, ReportStep};
    let mut ui = radios();
    ui.motion_fixed = true;
    let mut m = app(Screen::ErrorReports);
    m.platform.snapshot.observed = true;
    m.platform.snapshot.reports = Some(ReportState {
        enabled: Some(true),
        upload: true,
        destinations: vec![ReportDestination::Sd],
        ..Default::default()
    });
    let drawn = |m: &AppModel| drawn_text(&ui.draw(m, &m.library.tracks, false));
    // Before the platform's first record.
    m.platform.busy = Some(PlatformTask::ReportSend);
    let text = drawn(&m);
    assert!(text.contains("Starting") && text.contains("Collect") && text.contains("Send"));
    let live = |step, percent, bytes: Option<(u64, u64)>| ReportLive {
        step,
        percent,
        seconds_left: Some(14),
        bytes_done: bytes.map(|b| b.0),
        bytes_total: bytes.map(|b| b.1),
    };
    m.platform.report = Some(live(ReportStep::Clean, 47., None));
    let text = drawn(&m);
    assert!(text.contains("Removing Personal Details") && text.contains("47%"));
    assert!(text.contains("About 15 seconds left"));
    m.platform.report = Some(live(ReportStep::Finish, 91., Some((260_000, 503_213))));
    let text = drawn(&m);
    assert!(text.contains("Sending the Report") && text.contains("260 of 504 KB"));
    // Saving: the last step is Save and names the storage.
    m.platform.busy = Some(PlatformTask::ReportSave(ReportDestination::Sd));
    m.platform.report = Some(live(ReportStep::Finish, 88., None));
    let text = drawn(&m);
    assert!(
        text.contains("Saving to SD Card") && text.contains("Save") && !text.contains("Sending")
    );
    // Done: the page is back.
    m.platform.busy = None;
    m.platform.report = None;
    assert!(drawn(&m).contains("Send Report Now"));
}

#[test]
fn cjk_titles_draw_from_the_runtime_glyph_page_or_show_the_replacement() {
    use reborn_graphics::Tex;
    let mut m = app(Screen::NowPlaying);
    for t in m.library.tracks.iter_mut() {
        t.title = "残酷な天使のテーゼ".into();
        t.artist = "아이유".into();
    }
    m.replace_queue(m.library.tracks.clone(), 0).unwrap();
    let ui = Ui::default();
    let quads = ui.draw(&m, &m.library.tracks, true);
    let runtime: Vec<_> = quads.iter().filter(|q| q.tex == Tex::Glyphs).collect();
    if glyph_cache::available() {
        assert!(runtime.len() >= 12, "{} runtime glyphs", runtime.len());
        assert!(runtime.iter().all(|q| {
            let [x, y, w, h] = q.src.map(usize::from);
            w > 0
                && h > 0
                && x + w <= reborn_graphics::GLYPH_PAGE
                && y + h <= reborn_graphics::GLYPH_PAGE
        }));
        assert!(!drawn_text(&quads).contains('\u{FFFD}'));
        // Drawn again, every glyph is a cache hit at the same place.
        let again = ui.draw(&m, &m.library.tracks, true);
        let srcs = |q: &[reborn_graphics::Quad]| {
            q.iter()
                .filter(|q| q.tex == Tex::Glyphs)
                .map(|q| q.src)
                .collect::<Vec<_>>()
        };
        assert_eq!(srcs(&again), srcs(&quads));
    } else {
        assert!(runtime.is_empty());
        assert!(drawn_text(&quads).contains('\u{FFFD}'));
    }
}

#[test]
fn music_queues_skip_audiobooks_and_songs_play_in_list_order() {
    // Y2Linux #58: audiobook parts never join a music queue; Y2Reborn #21:
    // a song picked on Songs is followed by the row below it.
    let mut ui = radios();
    let mut m = app(Screen::Tracks);
    for (i, t) in m.library.tracks.iter_mut().enumerate() {
        t.title = format!("Song {}", 8 - i);
        t.genre = "Jazz".into();
        t.book = i >= 6;
    }
    let rows = ui.rows(&m, &m.library.tracks);
    let keys: Vec<_> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(
        keys,
        ["track:5", "track:4", "track:3", "track:2", "track:1", "track:0"]
    );
    match select_key(&mut ui, &mut m, "track:3") {
        Effect::PlayCollection {
            selected, members, ..
        } => {
            assert_eq!(selected, 3);
            assert_eq!(members, [5, 4, 3, 2, 1, 0]);
        }
        other => panic!("{other:?}"),
    }
    let mut m = app(Screen::Albums);
    for (i, t) in m.library.tracks.iter_mut().enumerate() {
        t.genre = "Jazz".into();
        t.book = i >= 6;
    }
    m.navigation.filter = "genre:Jazz".into();
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(rows[0].secondary, "6 songs");
    match select_key(&mut ui, &mut m, "shuffle:collection") {
        Effect::PlayCollection {
            members, shuffle, ..
        } => {
            assert_eq!(shuffle, ShuffleMode::Songs);
            assert_eq!(members, [0, 1, 2, 3, 4, 5]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_long_message_takes_two_lines_instead_of_being_cut() {
    // Y2Linux #55 (recovery screen) had the same fault in Reborn's toasts.
    let mut ui = radios();
    let m = app(Screen::Tracks);
    ui.flash("Couldn't reach the update server. Try again in a moment.");
    let text = drawn_text(&ui.draw(&m, &m.library.tracks, false));
    assert!(text.contains("Couldn") && text.contains("moment.") && !text.contains('…'));
}

#[test]
fn balance_opens_on_the_current_side_and_sets_it() {
    // A hearing difference between the ears: Settings > Sound > Balance.
    let mut ui = radios();
    let mut m = app(Screen::SettingsAudio);
    let rows = ui.rows(&m, &m.library.tracks);
    let row = rows.iter().find(|r| r.key == "balance").unwrap();
    assert_eq!(row.secondary, "Center");
    assert_eq!(select_key(&mut ui, &mut m, "balance"), Effect::None);
    assert_eq!(m.screen, Screen::Balance);
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(rows.len(), 21);
    assert_eq!(rows[0].label, "Left +10 dB");
    assert_eq!(rows[m.navigation.focus].label, "Center");
    assert_eq!(rows[20].label, "Right +10 dB");
    assert_eq!(
        select_key(&mut ui, &mut m, "balance_set:-1"),
        Effect::SetBalance(-1)
    );
    m.settings.balance = -1;
    m.screen = Screen::SettingsAudio;
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(
        rows.iter().find(|r| r.key == "balance").unwrap().secondary,
        "Left +1 dB"
    );
}

#[test]
fn a_collaboration_album_shows_once_under_each_of_its_artists() {
    // Y2Reborn #38 / Y2Linux #40: "Calvin Harris;Kelis" and "feat." tags
    // were artists of their own, each listing the album again.
    let collaboration = |screen| {
        let mut m = app(screen);
        for (t, artist) in m.library.tracks.iter_mut().zip([
            "Calvin Harris;Frank Ocean",
            "Calvin Harris feat. Kelis",
            "Calvin Harris",
        ]) {
            t.artist = artist.into();
            t.album = "Funk Wav Bounces".into();
            t.album_artist = "Calvin Harris".into();
        }
        m
    };
    let ui = radios();
    let m = collaboration(Screen::Artists);
    let rows = ui.rows(&m, &m.library.tracks);
    let keys: Vec<_> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "artist:Calvin Harris",
            "artist:Élodie · Μουσική · Музыка · 音楽",
            "artist:Frank Ocean",
            "artist:Kelis"
        ]
    );
    assert_eq!(rows[0].secondary, "1 album");
    let mut m = collaboration(Screen::Artist);
    m.navigation.filter = "artist:Calvin Harris".into();
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(rows.len(), 2, "Play and the album once");
    assert!(rows[0].secondary.starts_with("3 songs"));
    assert_eq!(rows[1].key, "album:Calvin Harris\u{1f}Funk Wav Bounces");
    m.navigation.filter = "artist:Kelis".into();
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].secondary.starts_with("1 song ·"));
    // Search finds the guest as an artist; Go to Artist opens the first name.
    let found = ui
        .catalog
        .borrow_mut()
        .index
        .search(&m.library.tracks, "kel");
    assert_eq!(found.artists, vec![("Kelis".to_owned(), 1, 1)]);
    assert_eq!(artist_filter(&m.library.tracks[1]), "artist:Calvin Harris");
}

#[test]
fn joined_genres_split_and_joined_artists_read_with_commas() {
    // Y2Linux #40: "Jazz;R&B" was a genre of its own, and song rows showed
    // "Lauryn Hill;Mary J. Blige" as tagged.
    let tagged = |screen| {
        let mut m = app(screen);
        for (t, genre) in m.library.tracks.iter_mut().zip(["Jazz;R&B", "Jazz", "R&B"]) {
            t.genre = genre.into();
        }
        m.library.tracks[0].artist = "Lauryn Hill;Mary J. Blige".into();
        m
    };
    let ui = radios();
    let m = tagged(Screen::Genres);
    let rows = ui.rows(&m, &m.library.tracks);
    let jazz = rows.iter().find(|r| r.key == "genre:Jazz").unwrap();
    assert!(jazz.secondary.ends_with("2 songs"), "{}", jazz.secondary);
    assert!(rows.iter().any(|r| r.key == "genre:R&B"));
    assert!(!rows.iter().any(|r| r.key.contains(';')));
    let mut m = tagged(Screen::Tracks);
    m.navigation.filter = "genre:R&B".into();
    let rows = ui.rows(&m, &m.library.tracks);
    let song = rows.iter().find(|r| r.key == "track:0").unwrap();
    assert_eq!(song.secondary, "Lauryn Hill, Mary J. Blige");
    assert!(rows.iter().any(|r| r.key == "track:2"));
    assert!(!rows.iter().any(|r| r.key == "track:1"));
}

/// The search page rebuilt per keystroke on a 20 000-song library
/// (`cargo test -p reborn-ui --release -- --ignored --nocapture search_page_speed`).
#[test]
#[ignore]
fn search_page_speed_on_a_large_library() {
    let mut ui = Ui::default();
    let tracks = crate::search::speed::large_library();
    let mut m = app(Screen::Home);
    m.library.tracks = tracks.clone();
    select_key(&mut ui, &mut m, "search");
    // The index is built when the search opens (here for this copy).
    ui.search_typed(&mut m, &tracks);
    let mut worst = std::time::Duration::ZERO;
    for text in [
        "t", "th", "the", "the ", "the l", "the li", "the l", "the", "t",
    ] {
        ui.keyboard.as_mut().unwrap().text = text.to_owned();
        let started = std::time::Instant::now();
        ui.search_typed(&mut m, &tracks);
        let searched = started.elapsed();
        let frame = ui.draw(&m, &tracks, false);
        let took = started.elapsed();
        worst = worst.max(took);
        eprintln!(
            "{text:>8}: search {searched:>10?}, with frame {took:>10?}: {} rows, {} quads",
            ui.row_count(&m, &tracks),
            frame.len()
        );
    }
    eprintln!("worst keystroke (search + frame): {worst:?}");
}

/// Open the menu on the focused row and choose `menu_key` in it.
fn menu_select(ui: &mut Ui, m: &mut AppModel, menu_key: &str) -> Effect {
    ui.model_action(m, Action::ContextMenu);
    let tracks = m.library.tracks.clone();
    let rows = ui.modal_rows_public(m, &tracks);
    m.navigation.modal_focus = rows
        .iter()
        .position(|r| r.key == menu_key)
        .unwrap_or_else(|| panic!("{menu_key} not in {rows:?}"));
    ui.model_action(m, Action::Select)
}

#[test]
fn up_next_opens_on_the_song_playing_with_history_above_and_its_parts_named() {
    let mut ui = radios();
    let mut m = app(Screen::NowPlaying);
    let tracks = m.library.tracks.clone();
    m.step(1);
    m.step(1); // Song 2 plays
    m.note_played(&tracks[0]);
    m.note_played(&tracks[1]);
    m.queue_source = "Album 0".into();
    m.queue_last(vec![tracks[7].clone()]).unwrap();
    // Now Playing's menu names the song that comes next: yours first.
    ui.model_action(&mut m, Action::Select);
    let rows = ui.modal_rows_public(&m, &tracks);
    let up_next = rows.iter().position(|r| r.key == "show_queue").unwrap();
    assert_eq!(rows[up_next].secondary, "Song 7");
    m.navigation.modal_focus = up_next;
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::Queue);
    let rows = ui.rows(&m, &tracks);
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(
        keys[..7],
        [
            "heading:history",
            "hist:0",
            "hist:1",
            "queue:3",
            "heading:queue",
            "queue:10",
            "heading:source"
        ]
    );
    assert_eq!(rows[6].label, "Next from Album 0");
    assert_eq!(rows[6].secondary, "6");
    assert_eq!(m.navigation.focus, 3, "it opens on the song playing");
    // Down skips the heading: your song.
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(m.navigation.focus, 5);
    // A song from the history plays now.
    m.navigation.focus = 1;
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::PlayNow(0));
    assert_eq!(m.screen, Screen::NowPlaying);
}

#[test]
fn a_moved_song_follows_the_wheel_until_select_puts_it_down() {
    let mut ui = radios();
    let mut m = app(Screen::Queue);
    let tracks = m.library.tracks.clone();
    // The song playing, the source's heading, then queue index 1 at row 2.
    m.navigation.focus = 2;
    assert_eq!(menu_select(&mut ui, &mut m, "queue_move"), Effect::None);
    assert_eq!(ui.list_title(&m), "Moving");
    let id = m.queue_entry_ids[1];
    assert_eq!(
        ui.model_action(&mut m, Action::WheelClockwise(3)),
        Effect::QueueMove { index: 1, delta: 1 },
        "one place per detent"
    );
    // The player moves it; the focus follows it there.
    assert!(m.move_entry(id, false));
    ui.normalize(&mut m, &tracks);
    assert_eq!(m.navigation.focus, 3);
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    assert_eq!(ui.list_title(&m), "Up Next");
    // Down again is an ordinary step now.
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(m.navigation.focus, 4);
}

#[test]
fn up_next_keeps_the_focused_song_when_rows_move_under_it() {
    let mut ui = radios();
    let mut m = app(Screen::Queue);
    let tracks = m.library.tracks.clone();
    let key = |ui: &Ui, m: &AppModel| ui.rows(m, &tracks)[m.navigation.focus].key.clone();
    // The song playing, the source's heading, then its songs: the second one.
    m.navigation.focus = 3;
    ui.normalize(&mut m, &tracks);
    let second = key(&ui, &m);
    // Play Next puts it under a new Queue heading; the old row is now the
    // source's heading, which used to send the focus to the top (#32).
    let id = m.queue_entry_ids[2];
    assert_eq!(m.entries_next(&[id]), 1);
    ui.normalize(&mut m, &tracks);
    assert_eq!(key(&ui, &m), second);
    assert_eq!(m.navigation.focus, 2);
    // The song ends: the focus stays on it, now the song playing.
    m.step(1);
    ui.normalize(&mut m, &tracks);
    assert_eq!(key(&ui, &m), second);
    // The wheel moves the focus as before.
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_ne!(key(&ui, &m), second);
    // A removed song leaves the focus nearby, not at the top.
    let at = m.navigation.focus;
    let gone = Ui::entry_of(&m, &key(&ui, &m)).unwrap();
    m.remove_entries(&[gone]);
    ui.normalize(&mut m, &tracks);
    assert!(
        m.navigation.focus + 1 >= at,
        "{} from {at}",
        m.navigation.focus
    );
}

#[test]
fn select_songs_chooses_rows_then_acts_on_them_together() {
    let mut ui = radios();
    let mut m = app(Screen::Tracks);
    m.navigation.focus = 4;
    assert_eq!(menu_select(&mut ui, &mut m, "select_songs"), Effect::None);
    // Up one and choose it too; Select toggles, it never plays.
    ui.model_action(&mut m, Action::WheelCounterClockwise(1));
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    assert_eq!(ui.list_title(&m), "2 Selected");
    let first = ui.rows(&m, &m.library.tracks.clone())[3].key.clone();
    let second = ui.rows(&m, &m.library.tracks.clone())[4].key.clone();
    let index = |k: &str| k.strip_prefix("track:").unwrap().parse::<usize>().unwrap();
    // Hold Select: what to do with them, in the list's order.
    assert_eq!(
        menu_select(&mut ui, &mut m, "sel_queue"),
        Effect::AddToQueueCollection(vec![index(&first), index(&second)])
    );
    assert!(ui.selection.is_none(), "done after acting");
    // Back ends choosing without leaving the list.
    menu_select(&mut ui, &mut m, "select_songs");
    ui.model_action(&mut m, Action::Back);
    assert!(ui.selection.is_none());
    assert_eq!(m.screen, Screen::Tracks);
    // Up Next: two songs removed together, the song playing never chosen.
    let mut m = app(Screen::Queue);
    m.navigation.focus = 2;
    menu_select(&mut ui, &mut m, "select_songs");
    ui.model_action(&mut m, Action::WheelClockwise(1));
    ui.model_action(&mut m, Action::Select);
    m.navigation.focus = 0;
    ui.model_action(&mut m, Action::Select);
    assert_eq!(ui.list_title(&m), "2 Selected");
    assert_eq!(
        menu_select(&mut ui, &mut m, "sel_remove"),
        Effect::QueueRemoveEntries(vec![m.queue_entry_ids[1], m.queue_entry_ids[2]])
    );
}

#[test]
fn albums_artists_genres_and_search_results_queue_as_a_whole() {
    let mut ui = radios();
    let mut m = app(Screen::Albums);
    m.settings.album_view = reborn_core::AlbumView::Grid;
    let tracks = m.library.tracks.clone();
    let rows = ui.rows(&m, &tracks);
    m.navigation.focus = 1;
    let album = rows[1].key.clone();
    let members: Vec<usize> = (0..9)
        .filter(|&i| tracks[i].album == rows[1].label)
        .collect();
    assert_eq!(members.len(), 3);
    ui.model_action(&mut m, Action::ContextMenu);
    let menu: Vec<String> = ui
        .modal_rows_public(&m, &tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(
        menu,
        [
            "Play",
            "Shuffle",
            "Play Next",
            "Add to Queue",
            "Add to Playlist…"
        ]
    );
    m.navigation.modal = None;
    assert_eq!(
        menu_select(&mut ui, &mut m, "group_next"),
        Effect::PlayNextCollection(members.clone()),
        "{album}"
    );
    m.navigation.focus = 1;
    assert_eq!(
        menu_select(&mut ui, &mut m, "group_play"),
        Effect::PlayCollection {
            selected: members[0],
            members,
            shuffle: ShuffleMode::Off,
            source: rows[1].label.clone(),
        }
    );
    // The search's Play All row: its songs, named after the search.
    let mut m = app(Screen::Search);
    m.navigation.filter = "song".into();
    let rows = ui.rows(&m, &tracks);
    m.navigation.focus = rows
        .iter()
        .position(|r| r.key == "search_play_all")
        .unwrap();
    match menu_select(&mut ui, &mut m, "group_shuffle") {
        Effect::PlayCollection {
            members,
            shuffle,
            source,
            ..
        } => {
            assert_eq!(members.len(), 9);
            assert_eq!(shuffle, ShuffleMode::Songs);
            assert_eq!(source, "Search “song”");
        }
        other => panic!("{other:?}"),
    }
}

/// A menu's labels; the menu closes again.
fn menu_labels(ui: &mut Ui, m: &mut AppModel) -> Vec<String> {
    ui.model_action(m, Action::ContextMenu);
    let tracks = m.library.tracks.clone();
    let labels = ui
        .modal_rows_public(m, &tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    ui.model_action(m, Action::Back);
    labels
}

#[test]
fn every_menu_lists_its_shared_actions_in_one_order() {
    // 8.36: play it, queue it, what this place changes, keep it, go to it,
    // then many songs at once; the same everywhere.
    const ORDER: [&str; 13] = [
        "Play",
        "Shuffle",
        "Shuffle Albums",
        "Play Next",
        "Add to Queue",
        "Move",
        "Remove from Queue",
        "Remove from Playlist",
        "Add to Favorites",
        "Add to Playlist…",
        "Go to Album",
        "Go to Artist",
        "Select Songs…",
    ];
    let in_order = |menu: &[String], place: &str| {
        let ranks: Vec<usize> = menu
            .iter()
            .filter_map(|l| ORDER.iter().position(|o| o == l))
            .collect();
        assert!(ranks.windows(2).all(|w| w[0] < w[1]), "{place}: {menu:?}");
    };
    let mut ui = radios();
    let mut m = app(Screen::Tracks);
    m.navigation.focus = 1;
    let songs = menu_labels(&mut ui, &mut m);
    assert_eq!(
        songs,
        [
            "Play Next",
            "Add to Queue",
            "Add to Favorites",
            "Add to Playlist…",
            "Go to Album",
            "Go to Artist",
            "Select Songs…"
        ]
    );
    // Up Next: a song to come, then the song playing.
    let mut m = app(Screen::Queue);
    m.navigation.focus = 3;
    let coming = menu_labels(&mut ui, &mut m);
    assert_eq!(
        coming,
        [
            "Play Next",
            "Add to Queue",
            "Move",
            "Remove from Queue",
            "Add to Playlist…",
            "Go to Album",
            "Go to Artist",
            "Select Songs…"
        ]
    );
    m.navigation.focus = 0;
    let playing = menu_labels(&mut ui, &mut m);
    assert_eq!(
        playing,
        [
            "Add to Playlist…",
            "Go to Album",
            "Go to Artist",
            "Save Up Next as Playlist…",
            "Clear Up Next"
        ]
    );
    // Now Playing leaves the whole list to Up Next.
    let mut m = app(Screen::NowPlaying);
    let now = menu_labels(&mut ui, &mut m);
    assert_eq!(now.len(), 8, "{now:?}");
    assert!(!now.iter().any(|l| l.contains("as Playlist")));
    // An album, a playlist and a playlist's song.
    let mut m = app(Screen::Albums);
    m.settings.album_view = reborn_core::AlbumView::Grid;
    let album = menu_labels(&mut ui, &mut m);
    assert_eq!(album[..2], ["Play", "Shuffle"]);
    let (mut pui, mut pm) = playlists_app();
    pm.navigation.focus = 3;
    let playlist = menu_labels(&mut pui, &mut pm);
    assert_eq!(
        playlist[..5],
        [
            "Play",
            "Shuffle",
            "Shuffle Albums",
            "Play Next",
            "Add to Queue"
        ]
    );
    for (menu, place) in [
        (&songs, "songs"),
        (&coming, "up next"),
        (&playing, "playing"),
        (&now, "now playing"),
        (&album, "album"),
        (&playlist, "playlist"),
    ] {
        in_order(menu, place);
    }
}

#[test]
fn up_next_adds_to_your_queue_as_every_other_list_does() {
    let mut ui = radios();
    let mut m = app(Screen::Queue);
    // The song playing, the source's heading, then queue index 1 at row 2.
    m.navigation.focus = 3;
    let id = m.queue_entry_ids[2];
    assert_eq!(
        menu_select(&mut ui, &mut m, "queue_last"),
        Effect::QueueEntriesLast(vec![id])
    );
    // Chosen songs too.
    let mut m = app(Screen::Queue);
    m.navigation.focus = 2;
    menu_select(&mut ui, &mut m, "select_songs");
    ui.model_action(&mut m, Action::WheelClockwise(1));
    ui.model_action(&mut m, Action::Select);
    assert_eq!(
        menu_select(&mut ui, &mut m, "sel_queue"),
        Effect::QueueEntriesLast(vec![m.queue_entry_ids[1], m.queue_entry_ids[2]])
    );
    // Your only queued song can go neither first nor last: neither is offered.
    let mut m = app(Screen::Queue);
    let tracks = m.library.tracks.clone();
    m.entries_next(&[m.queue_entry_ids[4]]);
    ui.normalize(&mut m, &tracks);
    m.navigation.focus = 2;
    let menu = menu_labels(&mut ui, &mut m);
    assert_eq!(menu[0], "Move", "{menu:?}");
}

#[test]
fn the_now_playing_wheel_seeks_when_customize_says_so() {
    let mut ui = radios();
    let mut m = app(Screen::NowPlaying);
    assert!(matches!(
        ui.model_action(&mut m, Action::WheelClockwise(1)),
        Effect::AdjustVolume(_)
    ));
    m.settings.now_playing_wheel = reborn_core::NowPlayingWheel::Seek;
    // As holding Previous/Next: one step a detent, the player sizes it.
    assert_eq!(
        ui.model_action(&mut m, Action::WheelClockwise(3)),
        Effect::Seek(1)
    );
    assert_eq!(
        ui.model_action(&mut m, Action::WheelCounterClockwise(1)),
        Effect::Seek(-1)
    );
    // A station has no place to move to: the wheel stays the volume.
    m.queue[0].source_id = reborn_core::RADIO_SOURCE.into();
    assert!(matches!(
        ui.model_action(&mut m, Action::WheelClockwise(1)),
        Effect::AdjustVolume(_)
    ));
}

#[test]
fn artists_can_list_album_artists_only() {
    // Y2Linux #41: a compilation is one artist, guests are not listed.
    let tagged = |screen| {
        let mut m = app(screen);
        m.library.tracks[1].artist = "Élodie · Μουσική · Музыка · 音楽 feat. Kelis".into();
        m.library.tracks[2].album_artist = String::new();
        m.library.tracks[2].artist = "Solo".into();
        for (t, artist) in m.library.tracks[6..].iter_mut().zip(["A", "B", "C"]) {
            t.album_artist = "Various Artists".into();
            t.artist = artist.into();
        }
        m
    };
    let ui = radios();
    let keys = |m: &AppModel| -> Vec<String> {
        ui.rows(m, &m.library.tracks)
            .into_iter()
            .map(|r| r.key)
            .collect()
    };
    let mut m = tagged(Screen::Artists);
    assert!(keys(&m).contains(&"artist:Kelis".to_owned()));
    assert!(keys(&m).contains(&"artist:B".to_owned()));
    m.settings.artists_list = reborn_core::ArtistsList::AlbumArtists;
    assert_eq!(
        keys(&m),
        [
            "albumartist:Élodie · Μουσική · Музыка · 音楽",
            "albumartist:Solo",
            "albumartist:Various Artists"
        ]
    );
    assert_eq!(ui.library_counts(&m, &m.library.tracks)[0], 3);
    // Its page: Play, then the compilation once.
    let mut m = tagged(Screen::Artist);
    m.navigation.filter = "albumartist:Various Artists".into();
    let rows = ui.rows(&m, &m.library.tracks);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].secondary.starts_with("3 songs"));
    assert_eq!(source_label(&m), "Various Artists");
}

#[test]
fn an_album_opened_from_a_genre_shows_that_genres_songs_and_offers_the_rest() {
    // Y2Linux #50: songs 1-2 of the album are Jazz, song 3 is Rock.
    let tagged = |screen| {
        let mut m = app(screen);
        for (t, genre) in m.library.tracks.iter_mut().zip(["Jazz", "Jazz", "Rock"]) {
            t.genre = genre.into();
        }
        m
    };
    let mut ui = radios();
    let mut m = tagged(Screen::Albums);
    m.settings.album_view = reborn_core::AlbumView::Grid;
    m.navigation.filter = "genre:Jazz".into();
    let tracks = m.library.tracks.clone();
    let rows = ui.rows(&m, &tracks);
    let at = rows.iter().position(|r| r.label == "Album 0").unwrap();
    // What the row plays is what it opens: the genre's two songs.
    m.navigation.focus = at;
    match menu_select(&mut ui, &mut m, "group_play") {
        Effect::PlayCollection { members, .. } => assert_eq!(members, [0, 1]),
        other => panic!("{other:?}"),
    }
    let mut m = tagged(Screen::Albums);
    m.settings.album_view = reborn_core::AlbumView::Grid;
    m.navigation.filter = "genre:Jazz".into();
    m.navigation.focus = at;
    ui.action(&mut m, &tracks, Action::Select);
    assert_eq!(m.screen, Screen::Album);
    let rows = ui.rows(&m, &tracks);
    let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Play", "Shuffle", "Song 0", "Song 1", "Show All 3 Songs"]
    );
    assert!(rows[0].secondary.starts_with("Jazz · 2 of 3 songs"));
    assert_eq!(source_label(&m), "Album 0");
    // The last row opens the whole album.
    m.navigation.focus = 4;
    ui.action(&mut m, &tracks, Action::Select);
    assert_eq!(m.screen, Screen::Album);
    let rows = ui.rows(&m, &tracks);
    assert_eq!(rows.len(), 5, "Play, Shuffle and its three songs");
    assert!(!rows.iter().any(|r| r.label.starts_with("Show All")));
    ui.action(&mut m, &tracks, Action::Back);
    assert!(m.navigation.filter.ends_with("genre:Jazz"));
}

#[test]
fn audiobooks_use_cover_flow_with_albums() {
    let ui = radios();
    let mut m = app(Screen::Audiobooks);
    with_book(&mut m);
    for t in m.library.tracks.iter_mut().filter(|t| t.book) {
        t.artwork = true;
    }
    let tracks = m.library.tracks.clone();
    let flow = ui.draw(&m, &tracks, false);
    assert!(flow.iter().filter(|q| q.warp.is_some()).count() >= 2);
    assert_eq!(crate::focus_target_count(&flow), 1);
    // Its lines: the title and its progress; songs and length are an album's.
    let text = drawn_text(&flow);
    assert!(text.contains("Momo"), "{text}");
    assert!(!text.contains("songs ·"), "{text}");
    m.settings.album_view = reborn_core::AlbumView::Grid;
    let list = ui.draw(&m, &tracks, false);
    assert!(list.iter().all(|q| q.warp.is_none()));
}

#[test]
fn see_all_shows_one_kind_of_result_and_back_returns_to_all_of_them() {
    // Y2Reborn #44: long results need no scrolling past kinds not wanted.
    let mut ui = Ui::default();
    let mut m = app(Screen::Search);
    let tracks = m.library.tracks.clone();
    m.navigation.filter = "song".into();
    ui.normalize(&mut m, &tracks);
    let rows = ui.rows(&m, &tracks);
    let see_all = rows
        .iter()
        .position(|r| r.key == "search_all:songs")
        .unwrap();
    m.navigation.focus = see_all;
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::Search);
    assert_eq!(screens::screen_title(&m), "Songs");
    let rows = ui.rows(&m, &tracks);
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    // No field, the heading, Play All, all nine songs; no See All.
    assert_eq!(keys[..2], ["heading:songs", "search_play_all"]);
    assert_eq!(rows.len(), 2 + 9);
    assert!(!keys.iter().any(|k| k.starts_with("search_all:")));
    // A song here still queues every song found.
    m.navigation.focus = 5;
    match ui.model_action(&mut m, Action::Select) {
        Effect::PlayCollection { members, .. } => assert_eq!(members.len(), 9),
        other => panic!("{other:?}"),
    }
    // Back: all the results again, not the keys.
    m.screen = Screen::Search;
    m.navigation.filter = "song".into();
    m.navigation.focus = see_all;
    ui.model_action(&mut m, Action::Select);
    ui.model_action(&mut m, Action::Back);
    assert!(ui.keyboard.is_none());
    assert_eq!(m.navigation.filter, "song");
    assert_eq!(m.navigation.focus, see_all);
}

#[test]
fn playlists_are_found_by_name_and_open_or_play_from_the_results() {
    let (mut ui, mut m) = playlists_app();
    let tracks = m.library.tracks.clone();
    m.screen = Screen::Search;
    m.navigation.filter = "road".into();
    let rows = ui.rows(&m, &tracks);
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys[1], "heading:playlists");
    let at = keys
        .iter()
        .position(|k| k.starts_with("playlist:"))
        .unwrap();
    assert_eq!(rows[at].label, "Road Trip");
    assert!(
        rows[0].secondary.contains("1 playlist"),
        "{}",
        rows[0].secondary
    );
    // Hold Select: the playlist's own menu.
    m.navigation.focus = at;
    let menu = menu_labels(&mut ui, &mut m);
    assert_eq!(
        menu[..5],
        [
            "Play",
            "Shuffle",
            "Shuffle Albums",
            "Play Next",
            "Add to Queue"
        ]
    );
    // Select opens it; Back returns to the results.
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::Playlist);
    assert!(m.navigation.filter.ends_with("Road Trip.m3u8"));
    ui.model_action(&mut m, Action::Back);
    assert_eq!(
        (m.screen, m.navigation.filter.as_str()),
        (Screen::Search, "road")
    );
}

#[test]
fn a_playlist_page_keeps_the_focused_song_on_screen() {
    // Owner on the device: the focus went below the screen in a short playlist.
    let (mut ui, mut m) = playlists_app();
    let songs: Vec<String> = (0..9)
        .map(|i| format!("../album{}/song{i}.flac", i / 3))
        .collect();
    let songs: Vec<&str> = songs.iter().map(String::as_str).collect();
    ui.playlists
        .set_lists(vec![playlist("/data/music/Playlists/Nine.m3u8", &songs)]);
    let tracks = m.library.tracks.clone();
    select_key(&mut ui, &mut m, "playlist:0");
    assert_eq!(m.screen, Screen::Playlist);
    for step in 0..12 {
        let focus = m.navigation.focus;
        let row = ui.rows(&m, &tracks)[focus].clone();
        if row.key.starts_with("pl_entry:") {
            let text = drawn_text(&ui.draw(&m, &tracks, false));
            assert!(
                text.contains(&row.label),
                "step {step}: {} not drawn",
                row.label
            );
        }
        ui.model_action(&mut m, Action::WheelClockwise(1));
    }
    // It stops on the last song.
    assert_eq!(m.navigation.focus, ui.row_count(&m, &tracks) - 1);
}

#[test]
fn a_page_header_stays_and_its_list_scrolls_one_row_at_a_time() {
    use components::{header_list_first, header_list_y, PAGE_LIST_TOP, PAGE_ROW};
    // Play and Shuffle in the header, then twenty songs.
    let (lead, count) = (2, 22);
    let mut scroll = 0;
    let mut last = 0;
    let walk: Vec<usize> = (0..count).chain((0..count).rev()).collect();
    for focus in walk {
        scroll = header_list_first(focus, lead, count, scroll);
        if focus >= lead {
            let y = header_list_y(focus, lead, scroll);
            assert!(
                y >= PAGE_LIST_TOP && y + PAGE_ROW <= crate::theme::HEIGHT,
                "focus {focus}: {y}"
            );
        }
        // One row a step at most, as every list.
        assert!(scroll.abs_diff(last) <= 1, "focus {focus}");
        last = scroll;
    }
    assert_eq!(scroll, 0, "back at the top");
    // Play or Shuffle focused: the list stays where it is.
    assert_eq!(header_list_first(1, lead, count, 7), 7);
    // A short page never scrolls.
    for focus in 0..5 {
        assert_eq!(header_list_first(focus, 2, 5, 0), 0);
    }
}

#[test]
fn settings_pages_keep_their_rows_until_the_focus_reaches_an_edge() {
    // As every list (8.37): a window of 5 over 10 rows, down and back up.
    let ui = radios();
    let m = app(Screen::Customize);
    let down: Vec<usize> = (0..10).map(|f| ui.group_window(&m, f, 5, 10)).collect();
    assert_eq!(down, [0, 0, 0, 0, 0, 1, 2, 3, 4, 5]);
    let up: Vec<usize> = (0..10)
        .rev()
        .map(|f| ui.group_window(&m, f, 5, 10))
        .collect();
    assert_eq!(up, [5, 5, 5, 5, 5, 4, 3, 2, 1, 0]);
}

/// A genre folder as the Reddit report (2026-10-26) keeps it: loose songs
/// named "Title - Artist", album folders, a compilation without an album
/// artist; in library order (artist first), as the database hands it over.
fn folder_library() -> Vec<Track> {
    let song = |path: &str, title: &str, artist: &str, album: &str, track: u32| Track {
        source_id: "internal".into(),
        title: title.into(),
        artist: artist.into(),
        album: album.into(),
        track,
        path: format!("/data/music/Hardcore/{path}").into(),
        filename: path.rsplit('/').next().unwrap().into(),
        duration_ms: 200_000,
        online: true,
        ..Default::default()
    };
    let mut tracks = vec![
        song(
            "Skydivin' - Darren Styles.mp3",
            "Skydiving",
            "Darren Styles",
            "",
            0,
        ),
        song("abc - Zed.mp3", "Zzz", "Aaa", "", 0),
        song(
            "Clubland 28/10 - Xan - Ten.mp3",
            "Ten",
            "Xan",
            "Clubland 28",
            10,
        ),
        song(
            "Clubland 28/2 - Bea - Two.mp3",
            "Two",
            "Bea",
            "Clubland 28",
            2,
        ),
        song(
            "Clubland 28/01 - Zoe - One.mp3",
            "One",
            "Zoe",
            "Clubland 28",
            1,
        ),
        song("zeta/01 - z.mp3", "z", "Darren Styles", "Zeta", 1),
        song("Alpha/01 - a.mp3", "a", "Yves", "Alpha", 1),
    ];
    tracks.sort_by(|a, b| (&a.artist, &a.album).cmp(&(&b.artist, &b.album)));
    for (i, t) in tracks.iter_mut().enumerate() {
        t.id = i as i64;
    }
    tracks
}

fn folder_app(filter: &str) -> (Ui, AppModel) {
    let mut m = app(Screen::Folders);
    m.library.tracks = folder_library();
    m.navigation.filter = filter.into();
    (Ui::default(), m)
}

fn at(tracks: &[Track], file: &str) -> usize {
    tracks.iter().position(|t| t.filename == file).unwrap()
}

#[test]
fn a_folder_lists_its_folders_then_its_songs_by_file_name() {
    let (mut ui, mut m) = folder_app("folder:/data/music/Hardcore");
    let tracks = m.library.tracks.clone();
    let rows = ui.rows(&m, &tracks);
    let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
    // Not by artist: the loose song shows its file name where the file
    // manager has it, not "Skydiving" among Darren Styles' songs.
    assert_eq!(
        labels,
        [
            "Alpha",
            "Clubland 28",
            "zeta",
            "abc - Zed",
            "Skydivin' - Darren Styles"
        ]
    );
    assert_eq!(
        rows[1].secondary, "3 songs",
        "counted when the folder opened"
    );
    assert_eq!(rows[4].secondary, "Darren Styles");
    // The compilation by its numbered files, numbers as numbers.
    select_key(&mut ui, &mut m, "folder:/data/music/Hardcore/Clubland 28");
    let rows = ui.rows(&m, &tracks);
    let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        ["01 - Zoe - One", "2 - Bea - Two", "10 - Xan - Ten"]
    );
    // A song plays the folder in that order: Next is the row below.
    let (one, two, ten) = (
        at(&tracks, "01 - Zoe - One.mp3"),
        at(&tracks, "2 - Bea - Two.mp3"),
        at(&tracks, "10 - Xan - Ten.mp3"),
    );
    assert_eq!(
        select_key(&mut ui, &mut m, &format!("track:{two}")),
        Effect::PlayCollection {
            selected: two,
            members: vec![one, two, ten],
            shuffle: ShuffleMode::Off,
            source: "Clubland 28".into(),
        }
    );
    // A loose song plays the folder's own songs, not its albums too.
    let (mut ui, mut m) = folder_app("folder:/data/music/Hardcore");
    let sky = at(&tracks, "Skydivin' - Darren Styles.mp3");
    let Effect::PlayCollection { members, .. } =
        select_key(&mut ui, &mut m, &format!("track:{sky}"))
    else {
        panic!("a song plays");
    };
    assert_eq!(members, [at(&tracks, "abc - Zed.mp3"), sky]);
}

#[test]
fn playing_a_folder_follows_folders_order_and_offers_shuffle_albums() {
    let (mut ui, mut m) = folder_app("folder:/data/music");
    let tracks = m.library.tracks.clone();
    assert_eq!(ui.rows(&m, &tracks)[0].label, "Hardcore");
    // A storage is named for what it is, not its mount folder.
    assert_eq!(crate::screens::screen_title(&m), "Internal Storage");
    m.navigation.focus = 0;
    let menu = menu_labels(&mut ui, &mut m);
    assert_eq!(menu[..3], ["Play", "Shuffle", "Shuffle Albums"]);
    let Effect::PlayCollection {
        members, shuffle, ..
    } = menu_select(&mut ui, &mut m, "group_play")
    else {
        panic!("the folder plays");
    };
    assert_eq!(shuffle, ShuffleMode::Off);
    let files: Vec<&str> = members
        .iter()
        .map(|&i| tracks[i].filename.as_str())
        .collect();
    assert_eq!(
        files,
        [
            "01 - a.mp3",
            "01 - Zoe - One.mp3",
            "2 - Bea - Two.mp3",
            "10 - Xan - Ten.mp3",
            "01 - z.mp3",
            "abc - Zed.mp3",
            "Skydivin' - Darren Styles.mp3",
        ]
    );
    let (mut ui, mut m) = folder_app("folder:/data/music");
    m.navigation.focus = 0;
    assert!(matches!(
        menu_select(&mut ui, &mut m, "group_shuffle_albums"),
        Effect::PlayCollection {
            shuffle: ShuffleMode::Albums,
            ..
        }
    ));
    // One album has nothing to mix.
    let mut m = app(Screen::Albums);
    m.settings.album_view = reborn_core::AlbumView::Grid;
    let album = menu_labels(&mut Ui::default(), &mut m);
    assert!(!album.contains(&"Shuffle Albums".to_string()), "{album:?}");
}

#[test]
fn shuffle_steps_through_off_songs_and_albums() {
    let mut ui = radios();
    let mut m = app(Screen::NowPlaying);
    for (now, next) in [
        (ShuffleMode::Off, ShuffleMode::Songs),
        (ShuffleMode::Songs, ShuffleMode::Albums),
        (ShuffleMode::Albums, ShuffleMode::Off),
    ] {
        m.settings.set_shuffle_mode(now);
        ui.model_action(&mut m, Action::ContextMenu);
        let rows = ui.modal_rows_public(&m, &m.library.tracks.clone());
        let row = rows
            .iter()
            .find(|r| r.key == "toggle_shuffle")
            .unwrap_or_else(|| panic!("{rows:?}"));
        assert_eq!(row.secondary, now.label());
        ui.model_action(&mut m, Action::Back);
        assert_eq!(
            menu_select(&mut ui, &mut m, "toggle_shuffle"),
            Effect::SetShuffle(next)
        );
        if m.navigation.modal.is_some() {
            ui.model_action(&mut m, Action::Back);
        }
        assert_eq!(m.screen, Screen::NowPlaying);
    }
    // Older sessions stored only `shuffle`: on is Songs.
    let old: reborn_core::Settings = serde_json::from_value({
        let mut v = serde_json::to_value(reborn_core::Settings::default()).unwrap();
        v["shuffle"] = true.into();
        v.as_object_mut().unwrap().remove("shuffle_albums");
        v
    })
    .unwrap();
    assert_eq!(old.shuffle_mode(), ShuffleMode::Songs);
}

#[test]
fn a_playlist_sorts_its_page_and_plays_in_that_order() {
    let (mut ui, mut m) = playlists_app();
    select_key(&mut ui, &mut m, "playlist:2");
    let path = PathBuf::from("/data/music/Playlists/Road Trip.m3u8");
    let tracks = m.library.tracks.clone();
    // Hold Select on a song: Sort steps the order on, the menu stays open.
    m.navigation.focus = 2;
    assert_eq!(
        menu_select(&mut ui, &mut m, "ple_sort"),
        Effect::SetPlaylistSort(path.clone(), reborn_core::PlaylistSort::Album)
    );
    assert_eq!(m.navigation.modal, Some(Modal::ContextMenu));
    ui.model_action(&mut m, Action::Back);
    // By title: Song 1, Song 2, Song 4, the missing song last.
    m.settings
        .set_playlist_sort(&path, reborn_core::PlaylistSort::Title);
    let labels: Vec<String> = ui.rows(&m, &tracks).into_iter().map(|r| r.label).collect();
    assert_eq!(labels[2..], ["Song 1", "Song 2", "Song 4", "Lost Song"]);
    assert_eq!(
        select_key(&mut ui, &mut m, "pl_play"),
        Effect::PlayCollection {
            selected: 1,
            members: vec![1, 2, 4],
            shuffle: ShuffleMode::Off,
            source: "Road Trip".into()
        }
    );
    ui.model_action(&mut m, Action::Back);
    // Moving a song only means something in the file's order.
    m.navigation.focus = 2;
    let menu = menu_labels(&mut ui, &mut m);
    assert!(!menu.contains(&"Move".to_string()), "{menu:?}");
    assert!(menu.contains(&"Sort".to_string()));
    // Holding Play or Shuffle gives the playlist's own menu.
    m.navigation.focus = 0;
    let menu = menu_labels(&mut ui, &mut m);
    assert_eq!(menu[..3], ["Play", "Shuffle", "Shuffle Albums"]);
    assert!(matches!(
        menu_select(&mut ui, &mut m, "pl_shuffle_albums"),
        Effect::PlayCollection {
            shuffle: ShuffleMode::Albums,
            ..
        }
    ));
    // Playlist Order again: the file's order, and Move is back.
    m.settings
        .set_playlist_sort(&path, reborn_core::PlaylistSort::Playlist);
    assert!(
        m.settings.playlist_sort.is_empty(),
        "nothing kept for the default"
    );
}

#[test]
#[ignore]
fn profile_folder_group_120k() {
    let mut tracks = vec![];
    for g in 0..12 {
        for a in 0..90 {
            for al in 0..10 {
                for n in 0..11 {
                    tracks.push(Track {
                        id: tracks.len() as i64,
                        source_id: "sd".into(),
                        title: format!("t{n}"),
                        artist: format!("Artist {a}"),
                        album: format!("Album {al}"),
                        track: n,
                        path: format!("/media/sd/Y2Bench120k/Genre {g}/Artist {a}/Album {al} (1999)/{n:02} - Title {n}.mp3").into(),
                        filename: format!("{n:02} - Title {n}.mp3"),
                        online: true,
                        ..Default::default()
                    });
                }
            }
        }
    }
    let ui = Ui::default();
    let m = app(Screen::Folders);
    let key = "folder:/media/sd";
    let t = std::time::Instant::now();
    let members = ui.group_members(&m, &tracks, key);
    eprintln!("group_members {} in {:?}", members.len(), t.elapsed());
    let t = std::time::Instant::now();
    let all: Vec<usize> = (0..tracks.len())
        .filter(|&i| crate::track_matches(&tracks[i], key))
        .collect();
    eprintln!("track_matches pass {} in {:?}", all.len(), t.elapsed());
    let t = std::time::Instant::now();
    let order = ui.group_members(&m, &tracks, key);
    eprintln!(
        "group_members again (index built) {} in {:?}",
        order.len(),
        t.elapsed()
    );
    let t = std::time::Instant::now();
    let subject = ui.menu_subject(&m, &tracks, key);
    eprintln!("menu_subject {subject:?} in {:?}", t.elapsed());
    let refs: Vec<&Track> = order.iter().map(|&i| &tracks[i]).collect();
    let t = std::time::Instant::now();
    let shuffled = reborn_core::queue::album_shuffle(&refs, None, 7);
    eprintln!("album_shuffle {} in {:?}", shuffled.len(), t.elapsed());
    let mut m = app(Screen::Folders);
    m.library.tracks = tracks.clone();
    m.navigation.filter = "folder:/media/sd/Y2Bench120k".into();
    let t = std::time::Instant::now();
    let n = ui.rows(&m, &tracks).len();
    eprintln!("folders rows {n} in {:?}", t.elapsed());
    m.navigation.filter = "folder:/media/sd/Y2Bench120k/Genre 3".into();
    let t = std::time::Instant::now();
    let n = ui.rows(&m, &tracks).len();
    eprintln!("genre folder rows {n} in {:?}", t.elapsed());
    let mut m = app(Screen::Tracks);
    m.library.tracks = tracks.clone();
    for pass in 0..2 {
        m.screen = Screen::Tracks;
        let t = std::time::Instant::now();
        let n = ui.rows(&m, &tracks).len();
        eprintln!("songs rows {n} pass {pass} in {:?}", t.elapsed());
        m.screen = Screen::Artists;
        let t = std::time::Instant::now();
        let n = ui.rows(&m, &tracks).len();
        eprintln!("artists rows {n} pass {pass} in {:?}", t.elapsed());
    }
    let key = crate::album_filter(&tracks[500]);
    let t = std::time::Instant::now();
    let a = ui.album_detail(&m, &tracks, &key);
    let b = ui.album_detail(&m, &tracks, &crate::album_filter(&tracks[900]));
    eprintln!("album_detail {a} / {b} in {:?}", t.elapsed());
    let t = std::time::Instant::now();
    let _ = ui.album_detail(&m, &tracks, &crate::album_filter(&tracks[1900]));
    eprintln!("album_detail next album in {:?}", t.elapsed());
    let mut index = crate::search::Index::default();
    let t = std::time::Instant::now();
    let found = index.search(&tracks, "a");
    eprintln!(
        "search index + 'a' ({} songs) in {:?}",
        found.songs.len(),
        t.elapsed()
    );
    let mut ui = Ui::default();
    let mut m = app(Screen::Home);
    m.library.tracks = tracks.clone();
    let lib = m.library.tracks.clone();
    m.library.tracks = vec![];
    while ui.prewarm(&m, &lib) {}
    let rows = ui.rows(&m, &lib);
    m.navigation.focus = rows.iter().position(|r| r.key == "search").unwrap();
    let t = std::time::Instant::now();
    m.library.tracks = lib;
    ui.model_action(&mut m, Action::Select);
    eprintln!("search opened after prewarm in {:?}", t.elapsed());
    eprintln!("screen {:?} keyboard {}", m.screen, ui.keyboard.is_some());
    let fresh = Ui::default();
    let list = playlist(
        "/media/sd/Mix.m3u8",
        &["Y2Bench120k/Genre 1/Artist 1/Album 1 (1999)/01 - Title 1.mp3"],
    );
    let t = std::time::Instant::now();
    let facts = fresh.facts(&m.library.tracks, &list);
    eprintln!(
        "playlist matched ({:?}) in {:?}",
        facts.resolved,
        t.elapsed()
    );
    for pass in 0..3 {
        let t = std::time::Instant::now();
        let quads = ui.draw(&m, &m.library.tracks, false);
        eprintln!(
            "search draw {pass}: {} quads in {:?}",
            quads.len(),
            t.elapsed()
        );
    }
}

#[test]
fn lists_made_ahead_match_lists_made_on_opening_and_leave_the_screen_alone() {
    let (mut ui, mut m) = folder_app("folder:/data/music/Hardcore/Clubland 28");
    let tracks = m.library.tracks.clone();
    let labels = |ui: &Ui, m: &AppModel| -> Vec<String> {
        ui.rows(m, &tracks).into_iter().map(|r| r.label).collect()
    };
    let shown = labels(&ui, &m);
    let mut pieces = 0;
    while ui.prewarm(&m, &tracks) {
        pieces += 1;
        assert!(pieces < 20);
    }
    // The last piece waits for the playlists to be read.
    assert_eq!(pieces, 8);
    ui.playlists.set_lists(vec![]);
    assert!(ui.prewarm(&m, &tracks));
    assert!(!ui.prewarm(&m, &tracks), "done once per library");
    assert_eq!(labels(&ui, &m), shown, "the folder on screen is unchanged");
    for screen in [
        Screen::Tracks,
        Screen::Artists,
        Screen::Albums,
        Screen::Genres,
    ] {
        m.screen = screen;
        m.navigation.filter.clear();
        let fresh = Ui::default();
        assert_eq!(labels(&ui, &m), labels(&fresh, &m), "{screen:?}");
    }
}
