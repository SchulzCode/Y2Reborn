#![forbid(unsafe_code)]
//! Isolated host fixture renderer. This binary and its demonstration state are
//! never installed by the production package. It uses the production UI and
//! the production platform projection; only the input records are fixtures.
use reborn_core::{
    platform::{BatteryState, ChargingState, LowBattery},
    Action, AppModel, AudioOutput, MediaSource, PlaybackState, Screen, Source, Track,
};
use reborn_platform::client;
use reborn_ui::{BluetoothDeviceView, BluetoothView, NetworkView, Ui, WifiStatus, WifiView};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

fn tracks() -> Vec<Track> {
    let albums = [
        ("Northark", "Echoes of a Higher Place"),
        ("Émilie & the Northern Lights", "A Landscape in Sound"),
        ("Quiet Harbour", "Low Tide"),
    ];
    let titles = [
        "A Brighter Silence",
        "Fields of Tomorrow",
        "The Weighing Sky",
        "Halcyon",
        "Still Light",
        "Lumière à l'horizon",
        "Северный свет",
        "夜の静けさ",
        "A very long title that extends well beyond the edge of a small physical music player",
        "Morning Ferry",
        "Salt and Cedar",
        "Harbour Lights",
    ];
    titles
        .iter()
        .enumerate()
        .map(|(i, title)| {
            let (artist, album) = albums[(i / 5).min(2)];
            Track {
                id: i as i64 + 1,
                source_id: if i < 10 { "internal" } else { "uuid:CARD" }.into(),
                path: format!("/data/music/{artist}/{album}/{i:02}.flac").into(),
                filename: format!("{i:02} {title}.flac"),
                title: (*title).into(),
                artist: artist.into(),
                album: album.into(),
                album_artist: artist.into(),
                track: (i % 5) as u32 + 1,
                duration_ms: 198_000 + i as u64 * 17_000,
                codec: "flac".into(),
                sample_rate: if i % 2 == 0 { 96_000 } else { 44_100 },
                channels: 2,
                artwork: true,
                online: true,
                ..Default::default()
            }
        })
        .collect()
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
            "usb":{"udcs":[{"state":"configured"}],"dma":{"transfer":"dma","dma_errors":0}},
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
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "out/product-ui-v2-preview-quads".into()),
    );
    fs::create_dir_all(&output)?;
    let album = "album:Northark\u{1f}Echoes of a Higher Place";
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
    case!("04-home", at(Screen::Home, ""));
    case!("05-music", at(Screen::Music, ""));
    case!("06-albums", at(Screen::Albums, ""));
    case!("07-album", at(Screen::Album, album), |_, m| m
        .navigation
        .focus = 1);
    case!("08-artists", at(Screen::Artists, ""));
    case!("09-artist", at(Screen::Artist, "artist:Northark"));
    case!("10-songs", at(Screen::Tracks, ""), |_, m| m
        .navigation
        .focus = 2);
    case!("11-folders", at(Screen::Folders, ""));
    case!("12-now-playing", at(Screen::NowPlaying, ""));
    case!(
        "13-now-playing-options",
        at(Screen::NowPlaying, ""),
        |ui, m| {
            ui.model_action(m, Action::Select);
        }
    );
    case!("14-queue", at(Screen::Queue, ""), |_, m| m
        .navigation
        .focus = 2);
    case!("15-quick-settings", at(Screen::Home, ""), |ui, m| {
        ui.model_action(m, Action::PowerMenu);
    });
    case!("16-settings", at(Screen::Settings, ""));
    case!("17-wifi", at(Screen::Wifi, ""), |_, m| m.navigation.focus =
        1);
    case!("18-wifi-off", at(Screen::Wifi, ""), |ui, _| {
        ui.wifi.powered = false;
        ui.wifi.status = WifiStatus::Disconnected;
    });
    case!("19-wifi-error", at(Screen::Wifi, ""), |ui, m| {
        ui.wifi.status = WifiStatus::Connecting("Studio".into());
        ui.wifi.problem = Some(reborn_core::platform::WifiProblem::WrongPassword);
        m.navigation.focus = 2;
    });
    case!("20-wifi-password", at(Screen::Wifi, ""), |ui, m| {
        m.navigation.focus = 3;
        ui.model_action(m, Action::Select);
        for _ in 0..3 {
            ui.model_action(m, Action::Select);
        }
    });
    case!("21-bluetooth", at(Screen::Bluetooth, ""), |_, m| {
        m.output = AudioOutput::Bluetooth("AA".into());
        m.navigation.focus = 1;
    });
    case!("22-bluetooth-off", at(Screen::Bluetooth, ""), |ui, _| {
        ui.bluetooth.powered = false;
    });
    case!("23-bluetooth-device", at(Screen::Bluetooth, ""), |ui, m| {
        m.navigation.focus = 1;
        ui.model_action(m, Action::Select);
    });
    case!("24-pc-transfer", at(Screen::PcTransfer, ""));
    case!(
        "25-pc-transfer-disconnected",
        at(Screen::PcTransfer, ""),
        |_, m| {
            m.platform.snapshot.usb = reborn_core::platform::UsbTransfer::Disconnected;
        }
    );
    case!("26-audio", at(Screen::SettingsAudio, ""));
    case!("27-output", at(Screen::SettingsAudio, ""), |ui, m| {
        ui.model_action(m, Action::Select);
    });
    case!("28-playback", at(Screen::SettingsPlayback, ""));
    case!("29-library", at(Screen::SettingsLibrary, ""), |_, m| {
        m.library.last_scan = Some(reborn_core::ScanSummary {
            discovered: 1284,
            reused: 1280,
            elapsed_ms: 900,
        });
    });
    case!("30-display", at(Screen::SettingsDisplay, ""));
    case!("31-system", at(Screen::SettingsSystem, ""));
    case!("32-battery", at(Screen::Battery, ""), |_, m| {
        m.platform.battery.charging = ChargingState::Charging;
    });
    case!("33-battery-low", at(Screen::Battery, ""), |_, m| {
        m.platform.battery = BatteryState {
            percent: Some(8),
            estimated: true,
            charging: ChargingState::OnBattery,
            level: LowBattery::Low,
        };
    });
    case!("34-storage", at(Screen::Storage, ""));
    case!("35-storage-no-sd", at(Screen::Storage, ""), |_, m| {
        m.platform.snapshot.storage.sd = reborn_core::platform::SdCard::Absent;
    });
    case!("36-update", at(Screen::Update, ""));
    case!("37-update-available", at(Screen::Update, ""), |_, m| {
        m.platform.snapshot.update.phase = reborn_core::platform::UpdatePhase::Available {
            version: "1.1.0".into(),
        };
        m.platform.snapshot.update.can_download = true;
    });
    case!("38-update-needs-wifi", at(Screen::Update, ""), |_, m| {
        m.platform.snapshot.update.phase = reborn_core::platform::UpdatePhase::Failed(
            reborn_core::platform::UpdateProblem::NeedsNetwork,
        );
    });
    case!("39-about", at(Screen::About, ""));
    case!("40-maintenance", at(Screen::Maintenance, ""), |_, m| {
        m.navigation.focus = 3
    });
    case!(
        "41-confirm-rebuild",
        at(Screen::Maintenance, ""),
        |ui, m| {
            m.navigation.focus = 3;
            ui.model_action(m, Action::Select);
        }
    );
    case!("42-diagnostics", at(Screen::Diagnostics, ""));
    case!(
        "43-diagnostics-battery",
        at(Screen::DiagnosticSection, "battery")
    );
    case!("44-empty-library", at(Screen::Albums, ""), |_, m| {
        m.library.tracks.clear();
        m.queue.clear();
        m.queue_entry_ids.clear();
    });
    case!("45-no-sd", at(Screen::SettingsLibrary, ""), |_, m| {
        m.sources.truncate(1);
        m.platform.snapshot.storage.sd = reborn_core::platform::SdCard::Absent;
    });
    case!(
        "46-sd-removed-now-playing",
        at(Screen::NowPlaying, ""),
        |_, m| {
            m.replace_queue(tracks()[10..].to_vec(), 0).unwrap();
            m.sources.truncate(1);
            m.playback = PlaybackState::Paused;
        }
    );
    case!("47-long-names", at(Screen::NowPlaying, ""), |_, m| {
        m.replace_queue(tracks()[8..9].to_vec(), 0).unwrap();
        m.position_ms = 20_000;
    });
    case!("48-unicode-songs", at(Screen::Tracks, ""), |_, m| m
        .navigation
        .focus =
        9);
    case!("49-song-info", at(Screen::TrackInfo, "0"));
    case!("50-pairing", at(Screen::Bluetooth, ""), |ui, _| {
        ui.pairing = Some("Kopfhörer Ü2 shows code 482 913. Pair only if the code matches.".into());
        ui.pairing_focus = 0;
    });
    case!(
        "51-power-off-confirm",
        at(Screen::SettingsSystem, ""),
        |ui, m| {
            m.navigation.focus = ui
                .rows(m, &m.library.tracks)
                .iter()
                .position(|row| row.key == "confirm:power_off")
                .unwrap();
            ui.model_action(m, Action::Select);
        }
    );
    case!("52-empty-queue", at(Screen::Queue, ""), |_, m| {
        m.queue.clear();
        m.queue_entry_ids.clear();
        m.playback = PlaybackState::Stopped;
    });
    case!("53-nothing-playing", at(Screen::NowPlaying, ""), |_, m| {
        m.queue.clear();
        m.queue_entry_ids.clear();
        m.playback = PlaybackState::Stopped;
    });

    case!("54-equalizer", at(Screen::Equalizer, ""), |_, m| {
        m.settings.eq_enabled = true;
        m.settings.eq_bands = reborn_core::flat_eq_bands();
        m.settings.eq_bands[0].gain_db = 2.;
    });
    case!("55-equalizer-gain", at(Screen::Equalizer, ""), |_, m| {
        m.navigation.modal = Some(reborn_core::Modal::EqBand(0));
        m.navigation.modal_focus = 14;
    });
    case!("56-sleep-refused", at(Screen::Sleep, ""), |_, m| {
        m.platform.snapshot.sleep.phase = reborn_core::platform::SleepPhase::Refused;
        m.platform.snapshot.sleep.problem =
            Some(reborn_core::platform::SleepProblem::QualificationPending);
    });
    case!("57-sleep-playback", at(Screen::Sleep, ""), |_, m| {
        m.platform.snapshot.sleep.phase = reborn_core::platform::SleepPhase::Refused;
        m.platform.snapshot.sleep.problem =
            Some(reborn_core::platform::SleepProblem::PlaybackActive);
    });
    case!("58-sleep-restored", at(Screen::Sleep, ""), |_, m| {
        m.platform.snapshot.sleep.phase = reborn_core::platform::SleepPhase::Restored;
        m.platform.snapshot.sleep.same_boot = Some(true);
        m.platform.snapshot.sleep.wake = reborn_core::platform::WakeReason::Power;
    });
    case!("59-ldac-quality", at(Screen::Bluetooth, ""), |ui, m| {
        use reborn_core::platform::{BluetoothQuality, LdacQuality, SbcQuality};
        use reborn_core::CodecPreference;
        ui.bluetooth.codec_choices = vec![
            CodecPreference::Auto,
            CodecPreference::Sbc,
            CodecPreference::Ldac,
        ];
        m.platform.snapshot.bluetooth_quality = BluetoothQuality {
            sbc_supported: true,
            requested_sbc: Some(SbcQuality::High),
            effective_sbc: Some(SbcQuality::High),
            ldac_supported: true,
            abr_supported: true,
            requested_quality: Some(LdacQuality::High),
            effective_quality: Some(LdacQuality::Standard),
            requested_abr: Some(true),
            effective_abr: Some(false),
            pending_restart: true,
        };
        m.navigation.focus = ui
            .rows(m, &m.library.tracks)
            .iter()
            .position(|row| row.key == "ldac_quality")
            .unwrap();
    });

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
    let mut home = vec![];
    for (name, mut m, setup) in cases {
        let mut ui = ui();
        setup(&mut ui, &mut m);
        let tracks = std::mem::take(&mut m.library.tracks);
        ui.normalize(&mut m, &tracks);
        m.library.tracks = tracks;
        let draw = ui.draw(&m, &m.library.tracks, !m.queue.is_empty());
        let focus = reborn_ui::focus_target_count(&draw);
        let static_page = matches!(
            name,
            "32-battery" | "33-battery-low" | "34-storage" | "35-storage-no-sd" | "49-song-info"
        );
        assert!(focus <= 1, "{name}: {focus} focus targets");
        assert!(static_page || focus == 1, "{name}: no focus target");
        write(name, &draw, focus)?;
        if name == "04-home" {
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
    // Shutdown: UI into the "Saving" screen, the bar draining while closing,
    // the dimmed last frame, the final dark frame.
    let closing = reborn_ui::closing_label(false, false);
    let close = reborn_ui::SHUTDOWN_CLOSE_FRAME;
    for (label, frame, text) in [
        ("a-dissolve", 3, closing),
        ("b-saving", close - 1, closing),
        ("c-closing", close + 4, closing),
        ("d-dimming", reborn_ui::SHUTDOWN_FRAMES - 3, closing),
    ] {
        write(
            &format!("90-shutdown-{label}"),
            &reborn_ui::shutdown_frame(&home, frame, text),
            0,
        )?;
    }
    write(
        "93-shutdown-low-battery",
        &reborn_ui::shutdown_frame(&home, close + 2, reborn_ui::closing_label(false, true)),
        0,
    )?;
    write("94-shutdown-final-black", &reborn_ui::black_frame(), 0)?;
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
        "90-shutdown-a-dissolve",
        "90-shutdown-b-saving",
        "90-shutdown-c-closing",
        "90-shutdown-d-dimming",
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
        texts.push((
            *line,
            reborn_ui::LABEL_Y + reborn_ui::FAILURE_LINE_PITCH * i as f32,
        ));
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
            "background": 0x090B0Du32,
            "bar": {"x": reborn_ui::BAR_X, "y": reborn_ui::BAR_Y, "w": reborn_ui::BAR_W,
                    "h": reborn_ui::BAR_H, "track": reborn_ui::BAR_TRACK >> 8,
                    "fill": reborn_ui::BAR_FILL >> 8},
            "phases": reborn_ui::BOOT_PHASES.iter().map(|p| json!({
                "token": p.token, "label": p.label, "fill_permille": p.fill_permille,
            })).collect::<Vec<_>>(),
            "failure_tokens": reborn_ui::BOOT_FAILURE_TOKENS,
            "failure_lines": reborn_ui::BOOT_FAILURE_LABELS.iter().enumerate().map(|(i, text)| json!({
                "text": text,
                "y": reborn_ui::LABEL_Y + reborn_ui::FAILURE_LINE_PITCH * i as f32,
            })).collect::<Vec<_>>(),
            "mark": reborn_ui::boot_mark_frame(),
            "labels": texts.iter().map(|(text, y)| json!({
                "text": text, "y": y, "quads": reborn_ui::boot_label_frame(text, *y),
            })).collect::<Vec<_>>(),
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
