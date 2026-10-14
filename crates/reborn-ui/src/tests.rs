//! Product-surface, navigation, focus and wheel-context tests for Product UI v2.
use super::*;
use reborn_core::{
    platform::{
        BatteryState, ChargingState, DiagnosticSection, Fact, LowBattery, PlatformSnapshot, SdCard,
        StorageState, UpdatePhase, UpdateProblem, UpdateState, UsbTransfer, VolumeSpace,
        VolumeState, WifiProblem,
    },
    AppModel, Effect, PlaybackState, Source,
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
fn drawn_text(quads: &[reborn_graphics::Quad]) -> String {
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

const NORMAL_SCREENS: [Screen; 29] = [
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
    Screen::SettingsLibrary,
    Screen::SettingsDisplay,
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
fn home_menu_switches_pin_and_unpin_from_display() {
    let mut ui = radios();
    let mut m = app(Screen::SettingsDisplay);
    let tracks = m.library.tracks.clone();
    let rows = ui.rows(&m, &tracks);
    m.navigation.focus = rows.iter().position(|r| r.key == "home_menu").unwrap();
    assert_eq!(rows[m.navigation.focus].secondary, "Standard");
    ui.action(&mut m, &tracks, Action::Select);
    assert_eq!(m.screen, Screen::HomeMenu);
    assert_eq!(
        ui.action(&mut m, &tracks, Action::Select),
        Effect::SetHomePin("artists".into(), true)
    );
    m.settings.pin_home("artists", true);
    assert_eq!(ui.rows(&m, &tracks)[0].kind, crate::RowKind::Toggle(true));
    assert_eq!(
        ui.action(&mut m, &tracks, Action::Select),
        Effect::SetHomePin("artists".into(), false)
    );
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
fn wheel_one_detent_is_one_step_outside_long_library_lists() {
    let mut ui = radios();
    // Settings, queue, modal and short lists ignore acceleration.
    for screen in [Screen::Settings, Screen::Queue, Screen::Tracks] {
        let mut m = app(screen);
        ui.model_action(&mut m, Action::WheelClockwise(4));
        assert_eq!(m.navigation.focus, 1, "{screen:?}");
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
fn library_search_counts_while_typing_and_lists_artists_albums_songs() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Home);
    m.library.tracks[4].title = "Beat It".into();
    m.library.tracks[4].artist = "Michael Jackson".into();
    assert_eq!(select_key(&mut ui, &mut m, "search"), Effect::None);
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
    assert_eq!(rows[1].key, "track:4");
    assert_eq!(rows[1].secondary, "Song · Michael Jackson");
    // Select plays the song.
    m.navigation.focus = 1;
    assert!(matches!(
        ui.model_action(&mut m, Action::Select),
        Effect::PlayCollection { selected: 4, .. } | Effect::Play(4)
    ));
    // Search finds artists and albums too, and Edit Search reopens with the text.
    m.screen = Screen::Search;
    m.navigation.filter = "album 1".into();
    m.navigation.focus = 0;
    let rows = ui.rows(&m, &m.library.tracks.clone());
    assert!(
        rows.iter().any(|r| r.secondary.starts_with("Album · ")),
        "{rows:?}"
    );
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
    let mut m = app(Screen::Bluetooth);
    m.output = AudioOutput::Bluetooth("AA".into());
    let rows = ui.rows(&m, &m.library.tracks);
    // State only; the codec is on Sound > Output (UI v4).
    assert_eq!(rows[1].secondary, "Connected");
    assert_eq!(rows[2].secondary, "Not paired");
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
    m.navigation.focus = 3;
    ui.model_action(&mut m, Action::ContextMenu);
    let id = m.navigation.context_key.clone().unwrap();
    m.queue.swap(3, 4);
    m.queue_entry_ids.swap(3, 4);
    m.navigation.modal_focus = 0;
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::PlayQueue(4)
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
    // Presets first; Custom opens the five bands.
    assert_eq!(ui.rows(&m, &m.library.tracks).len(), 5);
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
        Effect::PlayCollection { shuffle: true, .. }
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
fn a_spinner_in_view_asks_for_the_panel_rate_and_work_behind_a_list_does_not() {
    let mut ui = Ui {
        ui_motion: true,
        ..radios()
    };
    ui.spin_fixed = Some(0.3);
    // The first scan: Songs is empty and its ring turns at the panel's rate.
    let mut m = app(Screen::Tracks);
    m.library.tracks.clear();
    m.library.scanning = true;
    ui.observe(&m);
    ui.draw(&m, &[], false);
    assert!(ui.gliding(), "a spinner in view keeps the full frame rate");
    // The same scan behind a full list: once the list rests, the calm rate.
    let mut m = app(Screen::Tracks);
    m.library.scanning = true;
    ui.observe(&m);
    let tracks = m.library.tracks.clone();
    ui.draw(&m, &tracks, false);
    std::thread::sleep(std::time::Duration::from_millis(300));
    ui.draw(&m, &tracks, false);
    ui.draw(&m, &tracks, false);
    assert!(ui.animating() && !ui.gliding());
}

#[test]
fn the_spinner_segment_keeps_its_length_while_it_slides() {
    let lit = |phase: f32| {
        let mut c = crate::canvas::Canvas::new();
        c.spinner(240., 128., 44., 0xFFFFFFFF, 0x202020FF, phase);
        let arcs: Vec<_> = c
            .draw
            .iter()
            .map(|q| maskdata::ARCS.iter().position(|a| *a == q.src).unwrap() + 1)
            .collect();
        // Track ring, bright head, track over the tail.
        arcs[1] - arcs.get(2).copied().unwrap_or(0)
    };
    let lengths: std::collections::BTreeSet<_> = (30..70).map(|i| lit(i as f32 / 100.)).collect();
    assert_eq!(lengths.len(), 1, "{lengths:?}");
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
    assert_eq!(ui.library_counts(&tracks)[3], 2);
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
        ["All Songs", "Album 0", "Album 1"]
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
    m.navigation.focus = 2;
    ui.action(&mut m, &tracks, Action::Select);
    assert_eq!(m.screen, Screen::Album);
    assert!(m.navigation.filter.ends_with("Album 1"));
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
    // The setting toggles from Settings > Display.
    let mut m = app(Screen::SettingsDisplay);
    let rows = ui.rows(&m, &tracks);
    m.navigation.focus = rows.iter().position(|r| r.key == "album_view").unwrap();
    assert_eq!(rows[m.navigation.focus].secondary, "Cover Flow");
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
fn live_search_shows_results_while_typing_and_opens_them_with_back_to_the_results() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Home);
    m.library.tracks[4].title = "Beat It".into();
    m.library.tracks[4].artist = "Michael Jackson".into();
    select_key(&mut ui, &mut m, "search");
    assert!(ui.keyboard.as_ref().unwrap().results.is_empty());
    // b, e: the song shows up while typing, before Done.
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
    let k = ui.keyboard.as_ref().unwrap();
    assert_eq!(k.text, "be");
    assert_eq!(k.results[0].key, "track:4");
    assert_eq!(k.results[0].secondary, "Song · Michael Jackson");
    // Up from the top row reaches it; Select plays it from the results page.
    ui.model_action(&mut m, Action::NavigateUp);
    assert_eq!(ui.keyboard.as_ref().unwrap().result_focus, Some(0));
    assert!(matches!(
        ui.model_action(&mut m, Action::Select),
        Effect::PlayCollection { selected: 4, .. } | Effect::Play(4)
    ));
    assert!(ui.keyboard.is_none());
    assert_eq!(m.navigation.stack.last(), Some(&Screen::Search));
    // An artist opens its page; Back returns to the full results.
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
    let k = ui.keyboard.as_ref().unwrap();
    assert!(k.results[0].key.starts_with("artist:"), "{:?}", k.results);
    ui.model_action(&mut m, Action::NavigateUp);
    ui.model_action(&mut m, Action::NavigateUp);
    ui.model_action(&mut m, Action::NavigateUp);
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::Artist);
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.screen, Screen::Search);
    assert_eq!(m.navigation.filter, "Élo");
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
            shuffle: false
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
    assert_eq!(options, ["Play Next", "Add to Queue", "Duplicate"]);
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
