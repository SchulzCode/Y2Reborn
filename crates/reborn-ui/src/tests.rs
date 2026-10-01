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

const NORMAL_SCREENS: [Screen; 22] = [
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
    Screen::SettingsPlayback,
    Screen::SettingsLibrary,
    Screen::SettingsDisplay,
    Screen::SettingsSystem,
    Screen::Battery,
    Screen::Storage,
    Screen::Update,
    Screen::About,
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
fn root_is_a_small_music_player_menu() {
    let ui = radios();
    let m = app(Screen::Home);
    let labels: Vec<_> = ui
        .rows(&m, &m.library.tracks)
        .into_iter()
        .map(|r| r.label)
        .collect();
    assert_eq!(labels, ["Music", "Now Playing", "Queue", "Settings"]);
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
    let mut quick = app(Screen::Home);
    quick.navigation.modal = Some(Modal::QuickSettings);
    cases.push((Screen::Home, radios(), quick));
    let mut missing_song = app(Screen::TrackInfo);
    missing_song.navigation.filter = "999".into();
    missing_song.queue.clear();
    cases.push((Screen::TrackInfo, radios(), missing_song));
    for (screen, ui, m) in cases {
        let mut text = visible_text(&ui, &m);
        if super::screens::is_empty_catalog(&ui, &m) {
            text += super::screens::empty_catalog_copy(&m);
        }
        for term in ENGINEERING {
            assert!(!text.contains(term), "{screen:?} shows {term:?}: {text}");
        }
    }
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
        let copy = super::screens::empty_catalog_copy(&m);
        for term in ENGINEERING {
            assert!(!copy.contains(term), "{copy}");
        }
    }
}

#[test]
fn diagnostics_is_the_only_technical_subtree_and_lives_under_system() {
    let mut ui = radios();
    let mut m = app(Screen::Settings);
    select_key(&mut ui, &mut m, "system");
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
    for screen in NORMAL_SCREENS {
        if !matches!(screen, Screen::TrackInfo) {
            assert!(
                seen.iter().any(|s| s.starts_with(&format!("{screen:?}"))),
                "{screen:?} unreachable"
            );
        }
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
    let static_pages = [Screen::TrackInfo, Screen::Battery, Screen::Storage];
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
            if let Some(glyph) = q.glyph {
                assert!((glyph as usize) < glyphs::ADVANCE.len());
            }
        }
    }
}

#[test]
fn modals_trap_focus_and_show_one_focus_over_the_screen() {
    let mut ui = radios();
    let mut m = app(Screen::Albums);
    ui.model_action(&mut m, Action::PowerMenu);
    assert_eq!(m.navigation.modal, Some(Modal::QuickSettings));
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
    assert_eq!(m.screen, Screen::Albums);
}

#[test]
fn quick_settings_power_off_and_restart_default_to_cancel() {
    let mut ui = radios();
    for (label, expected) in [("Power Off", Effect::PowerOff), ("Restart", Effect::Reboot)] {
        let mut m = app(Screen::Home);
        ui.model_action(&mut m, Action::PowerMenu);
        let rows = ui.modal_rows_public(&m, &m.library.tracks);
        m.navigation.modal_focus = rows.iter().position(|r| r.label == label).unwrap();
        assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
        assert!(matches!(m.navigation.modal, Some(Modal::Confirm(_))));
        assert_eq!(m.navigation.modal_focus, 0, "starts on Cancel");
        assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
        assert_eq!(m.navigation.modal, None);
        ui.model_action(&mut m, Action::PowerMenu);
        m.navigation.modal_focus = rows.iter().position(|r| r.label == label).unwrap();
        ui.model_action(&mut m, Action::Select);
        ui.model_action(&mut m, Action::WheelClockwise(1));
        assert_eq!(ui.model_action(&mut m, Action::Select), expected);
    }
}

#[test]
fn quick_settings_toggle_radios_in_place_and_hide_missing_brightness() {
    let mut ui = radios();
    let mut m = app(Screen::Home);
    ui.model_action(&mut m, Action::PowerMenu);
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::WifiPower);
    assert_eq!(
        m.navigation.modal,
        Some(Modal::QuickSettings),
        "sheet stays open"
    );
    m.platform.brightness_available = false;
    assert!(!ui
        .modal_rows_public(&m, &m.library.tracks)
        .iter()
        .any(|r| r.label == "Brightness"));
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
    let mut m = app(Screen::Settings);
    m.navigation.modal = Some(Modal::QuickSettings);
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
    assert_eq!(labels[0], "Show Queue");
    assert!(labels.contains(&"Song Info".to_owned()));
    // In the options sheet the wheel moves the sheet, not the volume.
    assert_eq!(
        ui.model_action(&mut m, Action::WheelClockwise(1)),
        Effect::None
    );
    assert_eq!(m.navigation.modal_focus, 1);
}

#[test]
fn password_entry_moves_one_character_per_detent_and_is_never_drawn() {
    let mut ui = radios();
    let mut m = app(Screen::Wifi);
    select_key(&mut ui, &mut m, "ssid:Café");
    assert!(ui.text_entry);
    ui.model_action(&mut m, Action::WheelClockwise(4));
    ui.model_action(&mut m, Action::Select);
    assert_eq!(ui.password, "b");
    ui.password = "secretpass".into();
    let draw = ui.draw(&m, &m.library.tracks, false);
    let glyphs: String = draw
        .iter()
        .filter_map(|q| q.glyph)
        .map(|g| g.to_string())
        .collect();
    let secret: String = "secretpass"
        .chars()
        .map(|c| glyphs::index(c).to_string())
        .collect();
    assert!(!glyphs.contains(&secret));
    assert_eq!(focus_target_count(&draw), 1);
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
    assert_eq!(page.rows[1].secondary, "Couldn't connect");

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
    assert_eq!(rows[1].secondary, "Playing audio · SBC");
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
    m.platform.snapshot.update = UpdateState {
        phase: UpdatePhase::ReadyToInstall,
        can_install: true,
        can_cancel: true,
        ..Default::default()
    };
    select_key(&mut ui, &mut m, "confirm:update_apply");
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
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
    assert!(page.rows.is_empty());
    assert!(page.hero.unwrap().title.contains("Computer"));
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
    assert_eq!(ui.model_action(&mut m, Action::PowerMenu), Effect::None);
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
    assert_eq!(m.screen, Screen::Music);
}

#[test]
fn filtered_track_context_actions_stay_on_the_selected_track() {
    let mut tracks = library();
    tracks[0].album = "Other".into();
    for (screen, filter, row) in [
        (
            Screen::Album,
            "album:Élodie · Μουσική · Музыка · 音楽\u{1f}Album 0",
            2,
        ),
        (Screen::Tracks, "", 2),
    ] {
        for (menu, expected) in [
            (0, Effect::Play(2)),
            (1, Effect::PlayNext(2)),
            (2, Effect::AddToQueue(2)),
        ] {
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
fn letter_index_jumps_and_back_returns_to_the_list() {
    let mut ui = radios();
    let mut m = app(Screen::Tracks);
    m.library.tracks[4].title = "Zebra".into();
    ui.invalidate_catalog();
    ui.model_action(&mut m, Action::ContextMenu);
    let rows = ui.modal_rows_public(&m, &m.library.tracks);
    m.navigation.modal_focus = rows.iter().position(|r| r.key == "letter_index").unwrap();
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::LibraryIndex);
    select_key(&mut ui, &mut m, "jump_to:Z");
    assert_eq!(m.screen, Screen::Tracks);
    let row = ui
        .row_at(&m, &m.library.tracks, m.navigation.focus)
        .unwrap();
    assert_eq!(row.label, "Zebra");
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
            assert!(
                q.x + q.w <= 482.,
                "{screen:?} glyph beyond the panel at {}",
                q.x
            );
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
    assert!(visible_text(&ui, &m).contains("PC Transfer Ready"));
    m.platform.snapshot.usb = UsbTransfer::Error;
    assert!(visible_text(&ui, &m).contains("Reconnect the USB cable"));
}

#[test]
fn boot_and_shutdown_frames_are_dark_and_end_black() {
    let boot = boot_frame();
    assert_eq!(boot[0].color, theme::color::BG);
    assert_eq!(focus_target_count(&boot), 0);
    let ui_frame = radios().draw(&app(Screen::Home), &library(), false);
    for frame in 0..SHUTDOWN_FRAMES {
        let quads = shutdown_frame(&ui_frame, frame, None);
        assert!(quads
            .iter()
            .all(|q| q.color & 0xFF != 0 || q.glyph.is_some()));
    }
    let last = shutdown_frame(&ui_frame, SHUTDOWN_FRAMES - 1, None);
    let overlay = last.last().unwrap();
    assert_eq!(overlay.color, theme::color::BG, "fully faded");
    assert_eq!(black_frame()[0].color, theme::color::BLACK);
    let halfway = boot_transition(ui_frame, 0.5);
    assert!(halfway.iter().any(|q| q.glyph.is_some()));
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
