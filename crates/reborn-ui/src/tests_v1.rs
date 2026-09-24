use super::*;
use reborn_core::{PlatformTask, PlaybackState};
use serde_json::json;

fn library() -> Vec<Track> {
    (0..9)
        .map(|i| Track {
            id: i,
            source_id: "internal".into(),
            title: format!("Song {i}"),
            artist: "Élodie · Μουσική · Музыка · 音楽".into(),
            album: format!("Album {}", i / 3),
            album_artist: "Artist".into(),
            path: format!("/data/music/album{}/song{i}.flac", i / 3).into(),
            filename: format!("song{i}.flac"),
            online: true,
            ..Default::default()
        })
        .collect()
}
fn app(screen: Screen, page: &str) -> AppModel {
    let mut m = AppModel {
        screen,
        ..Default::default()
    };
    m.library.tracks = library();
    m.replace_queue(library(), 0).unwrap();
    m.navigation.filter = page.into();
    m
}
fn activate(ui: &mut Ui, m: &mut AppModel, key: &str) -> Effect {
    m.navigation.focus = ui
        .rows(m, &m.library.tracks)
        .iter()
        .position(|r| r.key == key)
        .expect(key);
    ui.model_action(m, Action::Select)
}
fn focus(ui: &Ui, m: &AppModel) {
    let quads = ui.draw(m, &m.library.tracks, "ok", false, PowerView::default());
    assert_eq!(
        focus_target_count(&quads),
        1,
        "{:?} / {}",
        m.screen,
        m.navigation.filter
    );
    for q in quads {
        assert!(q.x.is_finite() && q.y.is_finite() && q.w.is_finite() && q.h.is_finite());
        if let Some(glyph) = q.glyph {
            assert!((glyph as usize) < glyphs::ADVANCE.len());
        }
    }
}

#[test]
fn every_screen_has_one_reachable_wheel_path_and_back_route() {
    let screens = [
        Screen::Home,
        Screen::Music,
        Screen::Albums,
        Screen::Artists,
        Screen::Tracks,
        Screen::Folders,
        Screen::Album,
        Screen::Artist,
        Screen::NowPlaying,
        Screen::Queue,
        Screen::QuickSettings,
        Screen::Connectivity,
        Screen::Wifi,
        Screen::Bluetooth,
        Screen::Settings,
        Screen::SettingsAudio,
        Screen::SettingsPlayback,
        Screen::SettingsLibrary,
        Screen::SettingsDisplay,
        Screen::SettingsPower,
        Screen::SettingsSystem,
        Screen::Diagnostics,
        Screen::TrackInfo,
        Screen::LibraryIndex,
    ];
    let pages = [
        "health",
        "capabilities",
        "cpu",
        "thermal",
        "storage",
        "power",
        "network",
        "bluetooth",
        "codec",
        "audio",
        "usb",
        "update",
        "clock",
        "about",
        "backup",
        "maintenance",
        "benchmarks",
        "boot",
        "result",
        "reset_settings",
        "reset_full-user",
        "value:Long label\u{1f}Long observed value",
    ];
    for (screen, page) in screens
        .into_iter()
        .map(|s| (s, ""))
        .chain(pages.into_iter().map(|p| (Screen::Platform, p)))
    {
        for populated in [false, true] {
            let mut ui = Ui::default();
            let mut m = app(screen, page);
            if !populated {
                m.library.tracks.clear();
                m.queue.clear();
            }
            let tracks = std::mem::take(&mut m.library.tracks);
            ui.normalize(&mut m, &tracks);
            m.library.tracks = tracks;
            let enabled: Vec<_> = ui
                .rows(&m, &m.library.tracks)
                .iter()
                .enumerate()
                .filter_map(|(i, r)| r.enabled.then_some(i))
                .collect();
            for expected in enabled {
                assert_eq!(m.navigation.focus, expected, "{screen:?}/{page}");
                focus(&ui, &m);
                ui.model_action(&mut m, Action::WheelClockwise(1));
            }
            focus(&ui, &m);
            ui.model_action(&mut m, Action::Back);
            assert_eq!(m.screen, Screen::Home);
        }
    }
}

#[test]
fn nested_album_and_folder_routes_restore_filter_focus_and_scroll() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Albums, "");
    m.navigation.focus = 2;
    m.navigation.scroll = 1;
    let before = m.navigation.clone();
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::Album);
    ui.model_action(&mut m, Action::WheelClockwise(2));
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.navigation, before);
    m.screen = Screen::Folders;
    m.navigation.focus = 0;
    m.navigation.scroll = 0;
    ui.model_action(&mut m, Action::Select);
    let parent = m.navigation.filter.clone();
    ui.model_action(&mut m, Action::Select);
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.navigation.filter, parent);
}

#[test]
fn screen_off_ignores_navigation_but_preserves_global_transport_and_volume() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Tracks, "");
    m.navigation.focus = 4;
    m.navigation.scroll = 2;
    m.screen_off = true;
    let nav = m.navigation.clone();
    for action in [
        Action::WheelClockwise(8),
        Action::WheelCounterClockwise(8),
        Action::Select,
        Action::Back,
        Action::ContextMenu,
        Action::Home,
        Action::ShowNowPlaying,
        Action::PowerMenu,
    ] {
        assert_eq!(ui.model_action(&mut m, action), Effect::None);
        assert_eq!(m.navigation, nav);
    }
    for (action, effect) in [
        (Action::VolumeUp, Effect::AdjustVolume(2)),
        (Action::VolumeDown, Effect::AdjustVolume(-2)),
        (Action::PlayPause, Effect::TogglePlayback),
        (Action::NextTrack, Effect::NextTrack),
        (Action::PreviousTrack, Effect::PreviousTrack),
        (Action::ScreenWake, Effect::ScreenWake),
    ] {
        assert_eq!(ui.model_action(&mut m, action), effect);
        assert_eq!(m.navigation, nav);
    }
    assert_eq!(
        focus_target_count(&ui.draw(&m, &m.library.tracks, "ok", false, PowerView::default())),
        0
    );
}

#[test]
fn dialogs_trap_focus_and_restore_background_even_with_global_buttons() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Tracks, "");
    m.navigation.focus = 7;
    m.navigation.scroll = 4;
    let before = m.navigation.clone();
    ui.model_action(&mut m, Action::ContextMenu);
    ui.model_action(&mut m, Action::WheelClockwise(40));
    assert_eq!(m.navigation.focus, 7);
    assert_eq!(
        ui.model_action(&mut m, Action::PlayPause),
        Effect::TogglePlayback
    );
    assert_eq!(
        ui.model_action(&mut m, Action::VolumeDown),
        Effect::AdjustVolume(-2)
    );
    focus(&ui, &m);
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.navigation, before);
}

#[test]
fn queue_context_tracks_the_occurrence_after_reordering_and_disappears_after_removal() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Queue, "");
    m.queue[2] = m.queue[1].clone();
    m.navigation.focus = 2;
    ui.model_action(&mut m, Action::ContextMenu);
    let target = m.queue_entry_ids[2];
    m.queue.swap(2, 4);
    m.queue_entry_ids.swap(2, 4);
    assert_eq!(m.queue_entry_ids[4], target);
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::PlayQueue(4)
    );
    m.navigation.focus = 4;
    ui.model_action(&mut m, Action::ContextMenu);
    m.queue.remove(4);
    m.queue_entry_ids.remove(4);
    // A stale menu must close instead of selecting the replacement occurrence.
    let tracks = std::mem::take(&mut m.library.tracks);
    ui.normalize(&mut m, &tracks);
    m.library.tracks = tracks;
    assert_eq!(m.navigation.modal, None);
}

#[test]
fn destructive_update_is_capability_gated_confirmed_and_locks_navigation() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Platform, "update");
    assert!(
        !ui.rows(&m, &[])
            .iter()
            .find(|r| r.key == "confirm:update_apply")
            .unwrap()
            .enabled
    );
    m.platform.capabilities = json!({"capabilities":{"ota":{"enabled":true}}});
    m.platform.status = json!({"system":{"update":{"state":"Queued"}}});
    assert_eq!(
        activate(&mut ui, &mut m, "confirm:update_apply"),
        Effect::None
    );
    assert_eq!(m.navigation.modal_focus, 0);
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    activate(&mut ui, &mut m, "confirm:update_apply");
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(
        ui.model_action(&mut m, Action::Select),
        Effect::Platform(PlatformTask::UpdateApply)
    );
    m.platform.busy = Some(PlatformTask::UpdateApply);
    for a in [
        Action::Back,
        Action::Home,
        Action::PowerMenu,
        Action::PlayPause,
        Action::NextTrack,
        Action::Select,
    ] {
        assert_eq!(ui.model_action(&mut m, a), Effect::None);
    }
}

#[test]
fn codec_auto_stays_disabled_and_active_codec_never_comes_from_preference() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Platform, "codec");
    m.platform.bluetooth = json!({"devices":[{"connected":true,"audio":true}],"pcms":[{"codec":"SBC","format":33296,"rate":44100,"channels":2}]});
    m.settings.codec_preference = reborn_core::CodecPreference::Auto;
    m.playback = PlaybackState::Stopped;
    let rows = ui.rows(&m, &[]);
    assert!(rows.iter().find(|r| r.key == "codec_sbc").unwrap().enabled);
    assert!(!rows.iter().find(|r| r.key == "codec_auto").unwrap().enabled);
    m.navigation.focus = 1;
    ui.model_action(&mut m, Action::WheelClockwise(1));
    assert_eq!(m.navigation.focus, 4);
    let rows = platform::rows(&m, "bluetooth");
    assert!(rows
        .iter()
        .any(|r| r.label == "Preference" && r.secondary == "Auto"));
    assert!(rows
        .iter()
        .any(|r| r.label == "Active codec" && r.secondary == "SBC"));
}

#[test]
fn observations_keep_source_and_sink_distinct_and_never_invent_power_or_clock() {
    let mut m = app(Screen::Platform, "audio");
    m.platform.audio = json!({"source":{"codec":"FLAC","sample_rate":96000,"source_bits":24},"alsa":{"rate":44100,"format":"S16_LE","channels":2}});
    let rows = platform::rows(&m, "audio");
    assert!(rows
        .iter()
        .any(|r| r.label == "Source rate (Hz)" && r.secondary == "96000"));
    assert!(rows
        .iter()
        .any(|r| r.label == "Observed ALSA rate" && r.secondary == "44100"));
    m.platform.status =
        json!({"record":{"wall_timestamp":"1970-01-01"},"system":{"time":{"tls_ready":false}}});
    assert!(!platform::rows(&m, "clock")
        .iter()
        .any(|r| r.secondary.contains("1970")));
    assert!(platform::rows(&m, "power")
        .iter()
        .all(|r| !r.secondary.contains('%')));
    assert!(platform::rows(&m, "capabilities")
        .iter()
        .any(|r| r.label == "Wired S32" && r.secondary == "Unavailable"));
}

#[test]
fn low_storage_sd_usb_and_service_failures_have_observed_states() {
    let mut m = app(Screen::Platform, "storage");
    m.platform.status = json!({"storage":{"volumes":[{"path":"/data","state":"Ready","space_state":"LowSpace","available_bytes":1024,"total_bytes":9000000000_u64},{"path":"/media/sd","state":"Unavailable"}]},"system":{"ssh":{"state":"Unavailable"}}});
    let rows = platform::rows(&m, "storage");
    assert!(rows.iter().any(|r| r.secondary.contains("Low space")));
    assert!(rows
        .iter()
        .any(|r| r.label == "SD card" && r.secondary.contains("Not mounted")));
    assert!(platform::rows(&m, "usb")
        .iter()
        .any(|r| r.label == "SFTP readiness" && r.secondary == "Unavailable"));
    m.platform.status["system"]["ssh"]["state"] = json!("Ready");
    assert!(platform::rows(&m, "usb")
        .iter()
        .any(|r| r.secondary == "Ready"));
    m.platform.failure = Some("Couldn't get an IP address. Check the router and retry.".into());
    assert!(platform::rows(&m, "result")
        .iter()
        .any(|r| r.secondary.contains("IP address")));
}

#[test]
fn long_unicode_text_is_bounded_and_never_samples_outside_the_atlas() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Tracks, "");
    m.library.tracks[0].title = "Éß αβ Жя 東京 🎵 \n\t".repeat(1000);
    m.library.tracks[0].artist = "A".repeat(50000);
    focus(&ui, &m);
    let draw = ui.draw(&m, &m.library.tracks, "ok", false, PowerView::default());
    assert!(draw.len() < 1500);
    for c in ['é', 'ß', 'α', 'Ж'] {
        assert_ne!(glyphs::index(c), glyphs::index('🎵'));
    }
    assert_eq!(glyphs::index('東'), glyphs::index('🎵'));
    ui.model_action(&mut m, Action::ContextMenu);
    focus(&ui, &m);
}

#[test]
fn pairing_and_password_entry_keep_global_controls_and_single_focus() {
    let mut ui = Ui {
        pairing: Some("Studio Headphones · confirm 123456".into()),
        pairing_focus: 1,
        ..Default::default()
    };
    let mut m = app(Screen::Bluetooth, "");
    focus(&ui, &m);
    assert_eq!(
        ui.model_action(&mut m, Action::NextTrack),
        Effect::NextTrack
    );
    assert_eq!(
        ui.model_action(&mut m, Action::Back),
        Effect::ConfirmPairing(false)
    );
    ui.text_entry = true;
    ui.password = "secretpassword".into();
    assert_eq!(
        ui.model_action(&mut m, Action::VolumeUp),
        Effect::AdjustVolume(2)
    );
    focus(&ui, &m);
    ui.model_action(&mut m, Action::Back);
    assert!(!ui.text_entry);
    assert!(ui.password.is_empty());
}

#[test]
fn unavailable_radio_and_zero_wheel_steps_do_not_focus_disabled_rows() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Bluetooth, "");
    ui.model_action(&mut m, Action::WheelClockwise(0));
    assert_eq!(m.navigation.focus, 2);
    focus(&ui, &m);
}

#[test]
fn letter_index_jumps_in_title_order_and_back_returns_to_collection() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Tracks, "");
    m.library.tracks[0].title = "Zulu".into();
    m.library.tracks[1].title = "Alpha".into();
    ui.model_action(&mut m, Action::ContextMenu);
    let index = ui
        .modal_rows_public(&m, &m.library.tracks)
        .iter()
        .position(|r| r.key == "letter_index")
        .unwrap();
    m.navigation.modal_focus = index;
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::LibraryIndex);
    focus(&ui, &m);
    m.navigation.focus = ui
        .rows(&m, &m.library.tracks)
        .iter()
        .position(|r| r.label == "Z")
        .unwrap();
    ui.model_action(&mut m, Action::Select);
    assert_eq!(m.screen, Screen::Tracks);
    assert_eq!(
        ui.row_at(&m, &m.library.tracks, m.navigation.focus)
            .unwrap()
            .label,
        "Zulu"
    );
}

#[test]
fn confirmation_rechecks_live_admission_and_toasts_request_their_final_redraw() {
    let mut ui = Ui::default();
    let mut m = app(Screen::Platform, "update");
    m.platform.capabilities = json!({"capabilities":{"ota":{"enabled":true}}});
    m.platform.status = json!({"system":{"update":{"state":"Queued"}}});
    activate(&mut ui, &mut m, "confirm:update_apply");
    m.platform.status = json!({"system":{"update":{"state":"Failed"}}});
    m.navigation.modal_focus = 1;
    assert_eq!(ui.model_action(&mut m, Action::Select), Effect::None);
    ui.flash("Volume 40");
    assert!(!ui.expire_notice());
    ui.notice_until = Some(Instant::now() - Duration::from_secs(1));
    assert!(ui.expire_notice());
    assert!(!ui.expire_notice());
}

#[test]
fn only_preferences_and_session_survive_serialization() {
    let mut m = app(Screen::Platform, "network");
    m.last_error = Some("stale error".into());
    m.library.scanning = true;
    m.library.error = Some("old error".into());
    m.platform.status = json!({"wifi":{"state":"Online"}});
    let encoded = serde_json::to_string(&m).unwrap();
    assert!(
        !encoded.contains("stale error")
            && !encoded.contains("old error")
            && !encoded.contains("Online")
    );
    let loaded: AppModel = serde_json::from_str(&encoded).unwrap();
    assert_eq!(loaded.screen, Screen::Home);
    assert!(!loaded.library.scanning);
    assert_eq!(
        loaded.settings.codec_preference,
        m.settings.codec_preference
    );
    assert_eq!(loaded.queue.len(), m.queue.len());
}
