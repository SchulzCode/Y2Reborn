#![forbid(unsafe_code)]
//! Isolated host fixture renderer. This binary and its demonstration state are
//! never installed by the production package. It uses the production UI and
//! the production platform projection; only the input records are fixtures.
use reborn_core::{
    platform::{BatteryState, ChargingState, LowBattery},
    Action, AppModel, AudioOutput, MediaSource, PlaybackState, Playlist, Screen, Source, Track,
};
use reborn_platform::client;
use reborn_ui::{BluetoothDeviceView, BluetoothView, NetworkView, Ui, WifiStatus, WifiView};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

#[path = "../skins.rs"]
mod skins;

fn tracks() -> Vec<Track> {
    let albums = [
        ("Northark", "Night Drive", ["A Brighter Silence", "Fields of Tomorrow", "The Weighing Sky"]),
        ("Northark", "Echoes of a Higher Place", ["Halcyon Lights", "Still Water", "Morning Ferry"]),
        ("Quiet Harbour", "Low Tide", ["Halcyon", "Harbour Lights", "Salt and Cedar"]),
        ("Émilie & the Northern Lights", "A Landscape in Sound", ["Lumière à l'horizon", "Северный свет", "A very long title that extends well beyond the edge of a small physical music player"]),
        ("Verdant", "Halcyon Days", ["Green Room", "Fern", "Understory"]),
        ("Marlowe", "Slow Light", ["Amber", "Late Afternoon", "Dust"]),
        ("Ines Calder", "Paper Moons", ["Paper Moons", "Orbit", "Quiet Hours"]),
        ("Lowlands", "Cold Water", ["Cold Water", "Deep End", "Kelp"]),
    ];
    let mut out = vec![];
    for (a, (artist, album, titles)) in albums.iter().enumerate() {
        for (n, title) in titles.iter().enumerate() {
            let i = out.len();
            out.push(Track {
                id: i as i64 + 1,
                source_id: if a < 6 { "internal" } else { "uuid:CARD" }.into(),
                path: format!("/media/internal/music/{artist}/{album}/{n:02}.flac").into(),
                filename: format!("{n:02} {title}.flac"),
                title: (*title).into(),
                artist: (*artist).into(),
                album: (*album).into(),
                album_artist: (*artist).into(),
                track: n as u32 + 1,
                duration_ms: 198_000 + i as u64 * 17_000,
                codec: "flac".into(),
                sample_rate: if i % 2 == 0 { 96_000 } else { 44_100 },
                channels: 2,
                artwork: true,
                online: true,
                genre: [
                    "Ambient",
                    "Ambient",
                    "Folk",
                    "Classical",
                    "Folk",
                    "Jazz",
                    "Jazz",
                    "",
                ][a]
                    .into(),
                ..Default::default()
            });
        }
    }
    out
}

fn playlists() -> Vec<Playlist> {
    let list = |id: i64, name: &str, ids: &[i64], unresolved: u32| Playlist {
        id,
        source_id: "uuid:CARD".into(),
        path: format!("/media/sd/Playlists/{name}.m3u8").into(),
        name: name.into(),
        track_ids: ids.to_vec(),
        unresolved,
        online: true,
    };
    vec![
        list(1, "Evening", &[3, 1, 8, 14, 2, 20, 9], 0),
        list(2, "Road Trip", &[5, 12, 7, 19, 22, 4, 10, 16, 12], 2),
        list(3, "Sunday Morning", &[17, 18, 13], 0),
        list(
            4,
            "Train Journey (A Rather Long Playlist Name)",
            &[21, 23, 24, 6, 11],
            3,
        ),
    ]
}

fn status() -> Value {
    json!({"schema":"org.y2linux.status/v1",
        "record":{"kernel":"6.18.0-y2linux-preview","boot_id":"preview-boot"},
        "cpu":{"online":"0-3","load_average":"0.24 0.18 0.12","policies":[{"scaling_cur_freq":"598000","affected_cpus":"0 1 2 3","scaling_governor":"schedutil"}],"timer":{"clocksource":"arch_sys_counter","highres_active":true,"no_hz_active":true}},
        "memory":{"meminfo":{"MemTotal":954376,"MemAvailable":742112},"processes":[{"pss_kib":22400}]},
        "thermal":{"zones":[{"type":"cpu","temperature_millicelsius":46800}]},
        "power":{"soc_percent":72,"soc_source":"voltage_estimate","soc_confidence":"provisional","low_battery":{"state":"Normal","voltage_uv":3912000},"supplies":[{"name":"BAT0","type":"Battery","status":"Discharging"}]},
        "storage":{"volumes":[
            {"path":"/data","state":"Ready","space_state":"Normal","filesystem":"ext4","total_bytes":6_442_450_944_u64,"available_bytes":3_221_225_472_u64,"uuid":"preview"},
            {"path":"/media/sd","state":"Ready","space_state":"Normal","filesystem":"exfat","total_bytes":64_000_000_000_u64,"available_bytes":48_000_000_000_u64},
            {"path":"/","state":"Ready","space_state":"Normal","filesystem":"ext4","total_bytes":536_870_912,"available_bytes":400_000_000}]},
        "wifi":{"state":"Online","ip_addresses":["192.0.2.42"],"default_route":[{"gateway":"192.0.2.1","dev":"wlan0"}],"dns_ready":true,"rssi_dbm":-51},
        "bluetooth":{"selected_peer":{"trusted":true},"reconnect":{"state":"Connected"}},
        "system":{"versions":{"release_version":"1.0.0-reborn-product-ui-v2-candidate.1","rootfs_version":"2025.02.18-platform-v1.10","build_id":"Y2LINUX-REBORN-PRODUCT-UI-V2","reborn_source_commit":"preview","build_git_commit":"preview"},
            "time":{"tls_ready":true,"source":"ntp"},"ssh":{"state":"Ready","bind":"10.42.0.1:22","sftp":true},
            "usb":{"file_transfer":{"schema":"org.y2linux.usb-transfer/v1","boot_id":"preview-boot","mode":"mtp","state":"Ready"},"udcs":[{"state":"configured"}],"dma":{"transfer":"dma","dma_errors":0}},
            "update":{"state":"Idle","download":{"state":"Checked","release_version":"1.0.0-reborn-product-ui-v2-candidate.1"}},
            "boot_history":{"previous_boot_id":"previous","last_stage":"application_ready"},
            "previous_boot_evidence":{"previous_orderly_shutdown":true},"kernel_taint":0},
        "readiness":{"storage":{"state":"Ready"},"audio":{"state":"Ready"}}})
}
fn caps() -> Value {
    json!({"schema":"org.y2linux.capabilities/v1","capabilities":{
        "ota":{"implemented":true,"enabled":true},"storage":{"implemented":true,"enabled":true},
        "audio":{"implemented":true,"enabled":true,"enabled_formats":["S16_LE"],"enabled_rates_hz":[44100,48000]},
        "bluetooth":{"implemented":true,"enabled":true,"optional_codecs":["AAC"],"codec_auto":{"eligible_codecs":[]}},
        "usb_host":{"implemented":true,"enabled":false,"experimental":true}}})
}

fn model() -> AppModel {
    let mut m = AppModel {
        playback: PlaybackState::Playing,
        ..Default::default()
    };
    m.replace_queue(tracks()[..6].to_vec(), 0).unwrap();
    m.position_ms = 137_000;
    m.library.tracks = tracks();
    m.library.playlists = playlists();
    m.settings.volume = 56;
    m.sources = vec![
        Source {
            id: "internal".into(),
            kind: MediaSource::Internal,
            root: "/data/music".into(),
            online: true,
            mount: "internal".into(),
            mount_id: Some(41),
        },
        Source {
            id: "uuid:CARD".into(),
            kind: MediaSource::SdCard("CARD".into()),
            root: "/media/sd".into(),
            online: true,
            mount: "sd".into(),
            mount_id: Some(52),
        },
    ];
    m.platform.snapshot = client::snapshot(&status(), &caps());
    m.platform.battery = BatteryState {
        percent: Some(72),
        estimated: true,
        charging: ChargingState::OnBattery,
        level: LowBattery::Normal,
    };
    m.platform.brightness_available = true;
    m.platform.audio_facts = vec![reborn_core::platform::Fact::new("PCM format", "S16_LE")];
    m
}

fn ui() -> Ui {
    let mut ui = Ui::default();
    ui.wifi = WifiView {
        available: true,
        powered: true,
        status: WifiStatus::Connected("Home Wi-Fi".into()),
        networks: vec![
            NetworkView {
                ssid: "Home Wi-Fi".into(),
                bars: 3,
                secured: Some(true),
                saved_id: Some(7),
                visible: true,
            },
            NetworkView {
                ssid: "Studio".into(),
                bars: 2,
                secured: Some(true),
                saved_id: Some(3),
                visible: true,
            },
            NetworkView {
                ssid: "A very long network name with spaces and more".into(),
                bars: 1,
                secured: Some(true),
                saved_id: None,
                visible: true,
            },
            NetworkView {
                ssid: "Café Gäste".into(),
                bars: 2,
                secured: Some(false),
                saved_id: None,
                visible: true,
            },
        ],
        ..Default::default()
    };
    ui.bluetooth = BluetoothView {
        available: true,
        powered: true,
        devices: vec![
            BluetoothDeviceView {
                name: "AirPods Pro".into(),
                path: "/bt/a".into(),
                address: "AA".into(),
                paired: true,
                bonded: true,
                connected: true,
                audio_ready: true,
                codec: Some("AAC".into()),
            },
            BluetoothDeviceView {
                name: "Living Room Speaker with an Unusually Long Name".into(),
                path: "/bt/b".into(),
                address: "BB".into(),
                paired: true,
                bonded: true,
                ..Default::default()
            },
            BluetoothDeviceView {
                name: "Kopfhörer Ü2".into(),
                path: "/bt/c".into(),
                address: "CC".into(),
                ..Default::default()
            },
        ],
        codec_choices: vec![],
        ..Default::default()
    };
    ui
}

/// Adjusts one preview state before it is drawn.
type Setup = Box<dyn Fn(&mut Ui, &mut AppModel)>;

fn at(screen: Screen, filter: &str) -> AppModel {
    let mut m = model();
    m.screen = screen;
    m.navigation.filter = filter.into();
    m
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let skin_dir = args
        .iter()
        .position(|a| a == "--skin")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let output = PathBuf::from(
        args.iter()
            .enumerate()
            .find(|(i, a)| !a.starts_with("--") && (*i == 0 || args[i - 1] != "--skin"))
            .map(|(_, a)| a.clone())
            .unwrap_or_else(|| "out/product-ui-v3-preview-quads".into()),
    );
    fs::create_dir_all(&output)?;
    // `--skin DIR`: every state in that skin; the atlas is written next to
    // the quads for render_previews.py.
    let skin = match &skin_dir {
        Some(dir) => {
            let id = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let skin = skins::load(dir, &id, &|path, w, h| {
                reborn_media::decode_image(path, w, h)
            })
            .map_err(|e| format!("skin {}: {e}", dir.display()))?;
            for w in &skin.warnings {
                eprintln!("skin warning: {w}");
            }
            fs::write(output.join("skin-atlas.rgba"), &skin.atlas)?;
            fs::write(
                output.join("skin-atlas.json"),
                serde_json::to_vec(&json!({"width":skin.atlas_width,"height":skin.atlas_height}))?,
            )?;
            Some(std::rc::Rc::new(skin))
        }
        None => None,
    };
    let themes = skins::scan(&[PathBuf::from("skins")]);
    let album = "album:Quiet Harbour\u{1f}Low Tide";
    let mut cases: Vec<(&str, AppModel, Setup)> = vec![];
    let none = || -> Setup { Box::new(|_, _| {}) };
    macro_rules! case {
        ($name:expr, $m:expr) => {
            cases.push(($name, $m, none()))
        };
        ($name:expr, $m:expr, $f:expr) => {
            cases.push(($name, $m, Box::new($f)))
        };
    }
    let orange = 0xFF965AFFu32;
    let teal = 0x5AE6CDFFu32;
    // Home, library
    case!("04-home-playing", at(Screen::Home, ""), move |ui, _| ui
        .accent =
        orange);
    case!("05-home-idle", at(Screen::Home, ""), move |ui, m| {
        ui.accent = teal;
        m.queue.clear();
        m.queue_entry_ids.clear();
        m.playback = PlaybackState::Stopped;
        m.navigation.focus = 1;
    });
    case!("06-artists", at(Screen::Artists, ""), move |ui, m| {
        ui.accent = teal;
        m.navigation.focus = 2;
    });
    case!(
        "07-artist",
        at(Screen::Artist, "artist:Northark"),
        move |ui, _| ui.accent = orange
    );
    case!("08-albums", at(Screen::Albums, ""), move |ui, m| {
        ui.accent = orange;
        m.navigation.focus = 5;
    });
    case!("08b-albums-grid", at(Screen::Albums, ""), move |ui, m| {
        ui.accent = orange;
        m.settings.album_view = reborn_core::AlbumView::Grid;
        m.navigation.focus = 5;
    });
    case!(
        "08c-cover-flow-first",
        at(Screen::Albums, ""),
        move |ui, m| {
            ui.accent = orange;
            m.navigation.focus = 0;
        }
    );
    case!("08d-genres", at(Screen::Genres, ""), move |ui, m| {
        ui.accent = teal;
        m.navigation.focus = 1;
    });
    case!(
        "08e-genre-albums",
        at(Screen::Albums, "genre:Jazz"),
        move |ui, m| {
            ui.accent = teal;
            m.navigation.focus = 1;
        }
    );
    case!("09-album", at(Screen::Album, album), move |ui, m| {
        ui.accent = teal;
        m.navigation.focus = 3;
    });
    case!("10-songs", at(Screen::Tracks, ""), move |ui, m| {
        ui.accent = teal;
        m.navigation.focus = 2;
    });
    case!(
        "11-songs-fast-scroll",
        at(Screen::Tracks, ""),
        move |ui, m| {
            ui.accent = teal;
            m.navigation.focus = 2;
            ui.fast_scroll_until =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(600));
        }
    );
    case!("11b-playlists", at(Screen::Playlists, ""), move |ui, m| {
        ui.accent = teal;
        m.navigation.focus = 1;
    });
    case!(
        "11c-playlist",
        at(Screen::Playlist, "playlist:2"),
        move |ui, m| {
            ui.accent = orange;
            m.navigation.focus = 2;
        }
    );
    case!("11d-playlists-empty", at(Screen::Playlists, ""), |_, m| {
        m.library.playlists.clear();
    });
    case!(
        "11e-playlist-menu",
        at(Screen::Playlists, ""),
        move |ui, m| {
            ui.accent = teal;
            m.navigation.focus = 1;
            ui.model_action(m, Action::ContextMenu);
        }
    );
    case!(
        "11f-playlist-not-found",
        at(Screen::Playlist, "playlist:3"),
        |_, m| m.library.playlists[2].track_ids = vec![404, 405]
    );
    case!("12-folders", at(Screen::Folders, ""));
    case!(
        "13-folder",
        at(Screen::Folders, "folder:/media/internal/music"),
        |_, m| m.navigation.focus = 1
    );
    // Playing
    case!(
        "14-now-playing",
        at(Screen::NowPlaying, ""),
        move |ui, m| {
            ui.accent = orange;
            m.settings.shuffle = true;
            m.settings.gapless_enabled = true;
            m.output = AudioOutput::Bluetooth("AA".into());
        }
    );
    case!(
        "14b-now-playing-all-options",
        at(Screen::NowPlaying, ""),
        move |ui, m| {
            ui.accent = orange;
            m.settings.shuffle = true;
            m.settings.repeat = reborn_core::RepeatMode::All;
            m.settings.eq_enabled = true;
            m.settings.replay_gain = reborn_core::ReplayGainMode::Track;
            m.output = AudioOutput::Bluetooth("AA".into());
        }
    );
    case!(
        "15-now-playing-paused",
        at(Screen::NowPlaying, ""),
        move |ui, m| {
            ui.accent = orange;
            m.playback = PlaybackState::Paused;
            m.settings.repeat = reborn_core::RepeatMode::Track;
        }
    );
    case!("16-volume", at(Screen::NowPlaying, ""), move |ui, _| {
        ui.accent = orange;
        ui.volume_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(600));
    });
    case!("17-up-next", at(Screen::Queue, ""), move |ui, m| {
        ui.accent = orange;
        m.navigation.focus = 2;
    });
    case!("18-song-menu", at(Screen::Tracks, ""), move |ui, m| {
        ui.accent = teal;
        m.navigation.focus = 2;
        ui.model_action(m, Action::ContextMenu);
    });
    case!(
        "18b-now-playing-menu",
        at(Screen::NowPlaying, ""),
        move |ui, m| {
            ui.accent = orange;
            ui.model_action(m, Action::Select);
        }
    );
    case!("19-play-error", at(Screen::NowPlaying, ""), move |ui, _| {
        ui.accent = orange;
        ui.flash("Can't play Intro.flac");
    });
    // Settings
    case!("20-settings", at(Screen::Settings, ""));
    case!(
        "21-bluetooth-pairing",
        at(Screen::Bluetooth, ""),
        |ui, _| {
            ui.pairing =
                Some("Kopfhörer Ü2 shows code 482 913. Pair only if the code matches.".into());
            ui.pairing_focus = 0;
        }
    );
    case!("23-bluetooth", at(Screen::Bluetooth, ""), |_, m| {
        m.output = AudioOutput::Bluetooth("AA".into());
        m.navigation.focus = 2;
    });
    case!("24-wifi-off", at(Screen::Wifi, ""), |ui, _| {
        ui.wifi.powered = false;
        ui.wifi.status = WifiStatus::Disconnected;
    });
    case!("25-wifi", at(Screen::Wifi, ""), |_, m| m.navigation.focus =
        2);
    case!("26-wifi-password", at(Screen::Wifi, ""), |ui, m| {
        m.navigation.focus = 3;
        ui.model_action(m, Action::Select);
        for _ in 0..3 {
            ui.model_action(m, Action::WheelClockwise(1));
        }
        ui.model_action(m, Action::Select);
        ui.model_action(m, Action::Select);
    });
    case!("27-sound", at(Screen::SettingsAudio, ""), |_, m| {
        m.output = AudioOutput::Bluetooth("AA".into());
        m.settings.eq_enabled = true;
        m.settings.eq_bands = reborn_core::flat_eq_bands();
        m.settings.eq_bands[0].gain_db = 6.;
        m.settings.eq_bands[1].gain_db = 4.;
    });
    case!("28-equalizer", at(Screen::Equalizer, ""), |_, m| {
        m.settings.eq_enabled = true;
        m.settings.eq_bands = reborn_core::flat_eq_bands();
        m.settings.eq_bands[0].gain_db = 6.;
        m.settings.eq_bands[1].gain_db = 4.;
        m.navigation.focus = 1;
    });
    case!("29-crossfade", at(Screen::Crossfade, ""), |_, m| {
        m.navigation.focus = 2;
    });
    case!("30-display", at(Screen::SettingsDisplay, ""));
    case!("31-screen-off", at(Screen::ScreenTimeout, ""), |_, m| {
        m.navigation.focus = 1;
        m.settings.screen_timeout_seconds = 30;
    });
    case!("32-storage", at(Screen::Storage, ""), |_, m| m
        .navigation
        .focus = 0);
    case!("33-about", at(Screen::Update, ""), |_, m| {
        m.platform.snapshot.update.phase = reborn_core::platform::UpdatePhase::Available {
            version: "1.1.0".into(),
        };
        m.platform.snapshot.update.can_download = true;
    });
    case!("35-about-up-to-date", at(Screen::Update, ""), |_, m| m
        .navigation
        .focus =
        1);
    case!("36-reset-confirm", at(Screen::Update, ""), |ui, m| {
        m.navigation.focus = 3;
        ui.model_action(m, Action::Select);
    });
    // System
    case!("37-power-hold", at(Screen::NowPlaying, ""), move |ui, _| {
        ui.accent = orange;
        ui.power_hold = Some(std::time::Instant::now() - std::time::Duration::from_millis(750));
    });
    case!("38-battery-low", at(Screen::Home, ""), move |ui, m| {
        ui.accent = orange;
        m.platform.battery = BatteryState {
            percent: Some(8),
            estimated: true,
            charging: ChargingState::OnBattery,
            level: LowBattery::Critical,
        };
    });
    case!("40-usb-connected", at(Screen::Home, ""), |ui, m| {
        m.platform.snapshot.usb = reborn_core::platform::UsbTransfer::Ready;
        ui.observe(m);
    });
    case!("41-usb-copying", at(Screen::Home, ""), |ui, m| {
        m.platform.snapshot.usb = reborn_core::platform::UsbTransfer::Copying;
        ui.observe(m);
    });
    case!("42-computer-page", at(Screen::PcTransfer, ""));
    case!(
        "21b-bluetooth-pairing-progress",
        at(Screen::Bluetooth, ""),
        |ui, m| {
            m.navigation.focus = 3;
            ui.model_action(m, Action::Select);
        }
    );
    case!("22-bluetooth-failed", at(Screen::Bluetooth, ""), |ui, _| {
        ui.alert = Some(reborn_ui::Alert::BluetoothFailed {
            path: "/bt/c".into(),
            name: "Kopfhörer Ü2".into(),
            operation: "pair".into(),
        });
    });
    case!("34-update-downloading", at(Screen::Update, ""), |_, m| {
        m.platform.snapshot.update.phase = reborn_core::platform::UpdatePhase::Downloading;
        m.platform.busy = Some(reborn_core::PlatformTask::UpdateStage);
    });
    case!("35-wifi-needs-firmware", at(Screen::Wifi, ""), |_, m| {
        m.platform.snapshot.radio_firmware = reborn_core::platform::RadioFirmware::Missing;
    });
    case!(
        "35b-bluetooth-firmware-failed",
        at(Screen::Bluetooth, ""),
        |_, m| {
            m.platform.snapshot.radio_firmware = reborn_core::platform::RadioFirmware::Failed;
        }
    );
    case!(
        "34b-update-download-progress",
        at(Screen::Update, ""),
        |_, m| {
            m.platform.busy = Some(reborn_core::PlatformTask::UpdateStage);
            m.platform.download = Some(reborn_core::platform::DownloadLive {
                verifying: false,
                bytes_done: 23_400_000,
                bytes_total: 58_700_000,
                speed_bytes_per_s: Some(3_250_000),
                seconds_left: Some(11),
                check_percent: None,
            });
        }
    );
    case!(
        "34c-update-download-check",
        at(Screen::Update, ""),
        |_, m| {
            m.platform.busy = Some(reborn_core::PlatformTask::UpdateStage);
            m.platform.download = Some(reborn_core::platform::DownloadLive {
                verifying: true,
                bytes_done: 58_700_000,
                bytes_total: 58_700_000,
                speed_bytes_per_s: None,
                seconds_left: None,
                check_percent: Some(63),
            });
        }
    );
    case!(
        "14c-now-playing-crossfade",
        at(Screen::NowPlaying, ""),
        move |ui, m| {
            ui.accent = orange;
            m.settings.crossfade_ms = 6000;
            m.settings.shuffle = true;
        }
    );
    case!(
        "38b-battery-low-dialog",
        at(Screen::Home, ""),
        move |ui, m| {
            ui.accent = orange;
            m.platform.battery = BatteryState {
                percent: Some(10),
                estimated: true,
                charging: ChargingState::OnBattery,
                level: LowBattery::Low,
            };
            ui.observe(m);
        }
    );
    case!("43-sd-removed", at(Screen::Albums, ""), |ui, _| ui
        .flash("SD card removed"));
    case!("44-songs-added", at(Screen::Tracks, ""), |ui, _| ui
        .flash("12 new songs added"));
    case!("45-empty-library", at(Screen::Albums, ""), |_, m| {
        m.library.tracks.clear();
        m.queue.clear();
        m.queue_entry_ids.clear();
        m.playback = PlaybackState::Stopped;
    });
    case!("03-scanning", at(Screen::Albums, ""), |_, m| {
        m.library.tracks.clear();
        m.library.scanning = true;
        m.queue.clear();
        m.queue_entry_ids.clear();
    });
    // Internet radio (Beta 4)
    let stations = || {
        vec![
            reborn_core::RadioStation {
                uuid: "a".into(),
                name: "Radio Swiss Jazz".into(),
                url: "http://a/".into(),
                country: "Switzerland".into(),
                codec: "MP3".into(),
                bitrate: 128,
                tags: "jazz".into(),
            },
            reborn_core::RadioStation {
                uuid: "b".into(),
                name: "FIP".into(),
                url: "http://b/".into(),
                country: "France".into(),
                codec: "AAC".into(),
                bitrate: 192,
                tags: "eclectic".into(),
            },
            reborn_core::RadioStation {
                uuid: "c".into(),
                name: "SomaFM Groove Salad".into(),
                url: "http://c/".into(),
                country: "USA".into(),
                codec: "MP3".into(),
                bitrate: 128,
                tags: "ambient,chillout".into(),
            },
            reborn_core::RadioStation {
                uuid: "d".into(),
                name: "BBC World Service".into(),
                url: "http://d/".into(),
                country: "UK".into(),
                codec: "MP3".into(),
                bitrate: 56,
                tags: "news".into(),
            },
        ]
    };
    case!("60-radio", at(Screen::Radio, ""), |ui, _| {
        ui.radio.online = true;
        ui.radio.favorites = vec!["a".into(), "b".into()];
    });
    case!("61-radio-no-wifi", at(Screen::Radio, ""), |ui, _| ui
        .radio
        .online =
        false);
    case!(
        "62-radio-stations",
        at(Screen::RadioStations, "Popular"),
        move |ui, m| {
            ui.radio.online = true;
            ui.radio.stations = stations();
            ui.radio.favorites = vec!["b".into()];
            m.navigation.focus = 1;
        }
    );
    case!(
        "63-radio-countries",
        at(Screen::RadioCategories, "Countries"),
        |ui, _| {
            ui.radio.online = true;
            ui.radio.categories = [
                ("DE", "Germany", 5124),
                ("US", "USA", 6301),
                ("FR", "France", 2048),
                ("AT", "Austria", 412),
            ]
            .into_iter()
            .map(|(id, name, n)| reborn_ui::RadioCategory {
                id: id.into(),
                name: name.into(),
                stations: n,
            })
            .collect();
        }
    );
    case!("64-radio-search", at(Screen::Radio, ""), |ui, m| {
        ui.radio.online = true;
        m.navigation.focus = 4;
        ui.model_action(m, Action::Select);
        // Type "jaz" on the wheel keyboard (a = 0, j = 9, z = 25).
        let mut at = 0i32;
        for target in [9i32, 0, 25] {
            while at < target {
                ui.model_action(m, Action::WheelClockwise(1));
                at += 1;
            }
            while at > target {
                ui.model_action(m, Action::WheelCounterClockwise(1));
                at -= 1;
            }
            ui.model_action(m, Action::Select);
        }
    });
    case!(
        "65-radio-playing",
        at(Screen::NowPlaying, ""),
        move |ui, m| {
            ui.accent = teal;
            let station = stations().remove(0);
            m.replace_queue(vec![reborn_core::Track::radio(&station)], 0)
                .unwrap();
            m.queue[0].artist = "Ahmad Jamal - Poinciana".into();
            m.position_ms = 754_000;
            m.playback = reborn_core::PlaybackState::Playing;
        }
    );
    case!("46-service-menu", at(Screen::SettingsSystem, ""));
    case!("47-diagnostics", at(Screen::Diagnostics, ""));
    case!(
        "48-sd-removed-now-playing",
        at(Screen::NowPlaying, ""),
        move |ui, m| {
            ui.accent = orange;
            m.replace_queue(tracks()[18..].to_vec(), 0).unwrap();
            m.sources.truncate(1);
            m.playback = PlaybackState::Paused;
        }
    );
    case!("49-long-names", at(Screen::NowPlaying, ""), move |ui, m| {
        ui.accent = teal;
        m.replace_queue(tracks()[11..12].to_vec(), 0).unwrap();
        m.position_ms = 20_000;
    });
    case!("50-song-info", at(Screen::TrackInfo, "0"));
    case!("51-display", at(Screen::SettingsDisplay, ""), |_, m| {
        m.navigation.focus = 3;
    });
    {
        let themes = themes.clone();
        case!("52-theme-picker", at(Screen::Theme, ""), move |ui, m| {
            ui.themes = themes.clone();
            m.navigation.focus = 1;
            m.settings.theme = themes.first().map(|t| t.id.clone()).unwrap_or_default();
        });
    }

    let mut manifest = vec![];
    let write = |name: &str, quads: &[reborn_graphics::Quad], focus: usize| {
        fs::write(
            output.join(format!("{name}.json")),
            serde_json::to_vec(
                &json!({"width":480,"height":360,"screen":name,"fixture":true,
                "focus_targets":focus,"quads":quads}),
            )
            .unwrap(),
        )
    };
    let keys: Vec<u64> = tracks().iter().map(reborn_ui::art_key).collect();
    let mut home = vec![];
    for (name, mut m, setup) in cases {
        let mut ui = ui();
        ui.skin = skin.clone();
        ui.spin_fixed = Some(0.45);
        ui.motion_fixed = true;
        setup(&mut ui, &mut m);
        ui.set_resident_thumbs(keys.clone());
        let tracks = std::mem::take(&mut m.library.tracks);
        ui.normalize(&mut m, &tracks);
        m.library.tracks = tracks;
        let draw = ui.draw(&m, &m.library.tracks, !m.queue.is_empty());
        let focus = reborn_ui::focus_target_count(&draw);
        assert!(focus <= 1, "{name}: {focus} focus targets");
        write(name, &draw, focus)?;
        if name == "04-home-playing" {
            home = draw;
        }
        manifest.push(name.to_owned());
    }
    // Boot. The splash draws the first and last screens and every fill between
    // them; Reborn's first frame is the complete boot screen over the UI.
    let phase = |token: &str| {
        reborn_ui::BOOT_PHASES
            .iter()
            .find(|p| p.token == token)
            .expect("known boot phase")
    };
    let boot_state = |token: &str| {
        let p = phase(token);
        reborn_ui::boot_screen(f32::from(p.fill_permille) / 1000., p.label)
    };
    write("01-boot-splash", &boot_state("start"), 0)?;
    write("01b-boot-25", &boot_state("rc_time"), 0)?;
    write("01c-boot-60", &boot_state("conn_wifi"), 0)?;
    write("01d-boot-final-phase", &boot_state("runtime_ready"), 0)?;
    write("02-boot-handoff", &boot_state("ready"), 0)?;
    for (name, remaining) in [
        ("03a-boot-fade-bar-out", 0.7),
        ("03-boot-fade", 0.5),
        ("03c-boot-fade-lift", 0.3),
    ] {
        write(
            name,
            &reborn_ui::boot_transition(home.clone(), remaining),
            0,
        )?;
    }
    write("03b-boot-failed", &reborn_ui::boot_failure_screen(), 0)?;
    // Shutdown sequence, one frame per stage.
    let plan = reborn_ui::shutdown_plan(false);
    let closing = reborn_ui::closing_label(false, false);
    let stages = [
        ("a-recede", plan.close_frame / 2),
        ("b-saving", plan.close_frame - 1),
        ("c-closing", plan.close_frame + 8),
        ("d-last-step", plan.close_frame + 26),
        ("e-bar-gone", plan.close_frame + 36),
        ("f-dimming", plan.frames - 12),
        ("g-last-light", plan.frames - 4),
    ];
    for (label, frame) in stages {
        write(
            &format!("90-shutdown-{label}"),
            &reborn_ui::shutdown_frame(&home, frame, closing, plan),
            0,
        )?;
        manifest.push(format!("90-shutdown-{label}"));
    }
    let low = reborn_ui::shutdown_plan(true);
    write(
        "93-shutdown-low-battery",
        &reborn_ui::shutdown_frame(
            &home,
            low.close_frame + 4,
            reborn_ui::closing_label(false, true),
            low,
        ),
        0,
    )?;
    write("94-shutdown-final-black", &reborn_ui::black_frame(), 0)?;
    for (name, turn, title, hint) in [
        ("39-charging-off", 0.42, 0, 0),
        ("39b-charged-off", 1.0, 1, 0),
        ("39c-charging-paused-off", 0.0, 2, 2),
    ] {
        write(name, &reborn_ui::charge::charge_frame(turn, title, hint), 0)?;
        manifest.push(name.into());
    }
    for name in [
        "01-boot-splash",
        "01b-boot-25",
        "01c-boot-60",
        "01d-boot-final-phase",
        "02-boot-handoff",
        "03a-boot-fade-bar-out",
        "03-boot-fade",
        "03c-boot-fade-lift",
        "03b-boot-failed",
        "93-shutdown-low-battery",
        "94-shutdown-final-black",
    ] {
        manifest.push(name.into());
    }
    // Everything the early splash needs to draw the same screens: the phase
    // table, the layout and the rendered wordmark and status lines. Consumed by
    // Y2Linux `tools/graphics/make-splash-mark.py`.
    let mut texts = reborn_ui::BOOT_PHASES
        .iter()
        .map(|p| (p.label, reborn_ui::LABEL_Y))
        .collect::<Vec<_>>();
    for (i, line) in reborn_ui::BOOT_FAILURE_LABELS.iter().enumerate() {
        texts.push((*line, reborn_ui::failure_line_y(i)));
    }
    let mut seen = vec![];
    texts.retain(|t| {
        let fresh = !seen.contains(&(t.0, t.1.to_bits()));
        seen.push((t.0, t.1.to_bits()));
        fresh
    });
    fs::write(
        output.join("boot-layout.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": "org.reborn.boot-layout/v1",
            "width": 480, "height": 360,
            "background": 0x06070Au32,
            "bar": {"x": reborn_ui::BAR_X, "y": reborn_ui::BAR_Y, "w": reborn_ui::BAR_W,
                    "h": reborn_ui::BAR_H, "track": reborn_ui::BAR_TRACK >> 8,
                    "fill": reborn_ui::BAR_FILL >> 8},
            "phases": reborn_ui::BOOT_PHASES.iter().map(|p| json!({
                "token": p.token, "label": p.label, "fill_permille": p.fill_permille,
            })).collect::<Vec<_>>(),
            "failure_tokens": reborn_ui::BOOT_FAILURE_TOKENS,
            "failure_lines": reborn_ui::BOOT_FAILURE_LABELS.iter().enumerate().map(|(i, text)| json!({
                "text": text,
                "y": reborn_ui::failure_line_y(i),
            })).collect::<Vec<_>>(),
            "mark": reborn_ui::boot_mark_frame(),
            "labels": texts.iter().map(|(text, y)| json!({
                "text": text, "y": y, "quads": reborn_ui::boot_label_frame(text, *y),
            })).collect::<Vec<_>>(),
        }))?,
    )?;
    // The charging screen drawn by the rescue charger: base image, text lines
    // and the ring geometry. Consumed by Y2Linux `tools/graphics/make-charge-art.py`.
    fs::write(
        output.join("charge-layout.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": "org.reborn.charge-layout/v1",
            "width": 480, "height": 360,
            "background": reborn_ui::charge::CHARGE_BG,
            "ring": {"cx": reborn_ui::charge::CHARGE_CX, "cy": reborn_ui::charge::CHARGE_CY,
                     "outer": reborn_ui::charge::CHARGE_RING_OUTER, "inner": reborn_ui::charge::CHARGE_RING_INNER,
                     "track": reborn_ui::charge::CHARGE_TRACK, "lit": reborn_ui::charge::CHARGE_LIT},
            "base": reborn_ui::charge::charge_base_frame(),
            "titles": reborn_ui::charge::CHARGE_TITLES.iter().map(|t| json!({
                "text": t, "quads": reborn_ui::charge::charge_label_frame(t, reborn_ui::charge::CHARGE_TITLE_Y)})).collect::<Vec<_>>(),
            "hints": reborn_ui::charge::CHARGE_HINTS.iter().map(|t| json!({
                "text": t, "quads": reborn_ui::charge::charge_label_frame(t, reborn_ui::charge::CHARGE_HINT_Y)})).collect::<Vec<_>>(),
        }))?,
    )?;
    manifest.sort();
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!(
        "Rendered {} production UI states at 480×360. Fixture data only.",
        manifest.len()
    );
    Ok(())
}
