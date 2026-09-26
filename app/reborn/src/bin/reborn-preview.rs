#![forbid(unsafe_code)]
//! Isolated host fixture renderer. This binary and its demonstration state are
//! never installed by the production package. Uses the actual production UI.
use reborn_core::{
    AppModel, ConfirmAction, MediaSource, Modal, PlatformTask, PlaybackState, Screen, Source, Track,
};
use reborn_ui::{Item, PowerView, PreviewScreen, RadioView, Ui};
use serde_json::json;
use std::{fs, path::PathBuf};
fn tracks() -> Vec<Track> {
    [
        "A Brighter Silence",
        "Fields of Tomorrow",
        "The Weighing Sky",
        "Halcyon",
        "Still Light",
        "Lumière à l'horizon",
        "Северный свет",
        "夜の静けさ",
        "A very long title that extends well beyond the edge of a small physical music player",
    ]
    .iter()
    .enumerate()
    .map(|(i, title)| Track {
        id: i as i64 + 1,
        source_id: "internal".into(),
        path: format!("/data/music/Northark/Echoes/{i:02}.flac").into(),
        filename: format!("{i:02}.flac"),
        title: (*title).into(),
        artist: if i < 5 {
            "Northark"
        } else {
            "Émilie & the Northern Lights"
        }
        .into(),
        album: if i < 5 {
            "Echoes of a Higher Place"
        } else {
            "A Landscape in Sound"
        }
        .into(),
        album_artist: if i < 5 {
            "Northark"
        } else {
            "Émilie & the Northern Lights"
        }
        .into(),
        track: i as u32 + 1,
        duration_ms: 318_000 + i as u64 * 12_000,
        codec: "FLAC".into(),
        sample_rate: 96_000,
        channels: 2,
        artwork: true,
        online: true,
        ..Default::default()
    })
    .collect()
}
fn model() -> AppModel {
    let mut m = AppModel {
        playback: PlaybackState::Playing,
        position_ms: 137_000,
        ..Default::default()
    };
    m.replace_queue(tracks(), 0).unwrap();
    m.position_ms = 137_000;
    m.library.tracks = tracks();
    m.settings.volume = 56;
    m.sources = vec![Source {
        id: "internal".into(),
        kind: MediaSource::Internal,
        root: "/data/music".into(),
        online: true,
        mount: "internal".into(),
        mount_id: Some(41),
    }];
    m.platform.status = json!({"schema":"org.y2linux.status/v1","record":{"wall_timestamp":"2026-09-24T15:42:00+00:00","kernel":"6.18.0-y2linux-platform-v1-candidate-01","boot_id":"preview-boot"},"cpu":{"online":"0-3","load_average":"0.24 0.18 0.12","policies":[{"scaling_cur_freq":"598000","affected_cpus":"0 1 2 3","scaling_governor":"schedutil"}]},"memory":{"meminfo":{"MemTotal":954376,"MemAvailable":742112,"Cached":82412,"Slab":32900},"processes":[{"rss_kib":28500,"pss_kib":22400}]},"thermal":{"zones":[{"type":"CPU","temperature_millicelsius":46800},{"type":"PMIC","temperature_millicelsius":48200}],"cooling":[{"type":"cpufreq","state":0,"max_state":2}]},"power":{"supplies":[{"name":"Battery","type":"Battery","status":"Charging","voltage_now":"4050000"},{"name":"USB","type":"USB","status":null,"online":"1","usb_type":"SDP"}],"low_battery":{"state":"Disabled"}},"storage":{"volumes":[{"path":"/data","state":"Ready","space_state":"Normal","filesystem":"ext4","total_bytes":6442450944_u64,"available_bytes":3221225472_u64},{"path":"/media/sd","state":"Unavailable","space_state":"Unavailable","reason":"not_uniquely_mounted"},{"path":"/","state":"Ready","space_state":"Normal","filesystem":"ext4","total_bytes":536870912,"available_bytes":400000000}]},"wifi":{"state":"Online","reason":null,"ip_addresses":["192.0.2.42"],"default_route":[{"gateway":"192.0.2.1","dev":"wlan0"}],"dns_ready":true,"rssi_dbm":-51,"traffic_counters":{"rx_bytes":1120402,"tx_bytes":58424}},"bluetooth":{"selected_peer":{"trusted":true},"reconnect":{"state":"Connected"}},"system":{"versions":{"release_version":"1.0.0-candidate.1","rootfs_version":"2025.02.18-platform-v1.1","build_id":"Y2LINUX-PLATFORM-V1-CANDIDATE-01","reborn_source_commit":"preview-only","build_git_commit":"preview-only"},"time":{"tls_ready":true,"source":"ntp"},"ssh":{"state":"Ready","bind":"10.42.0.1:22","sftp":true},"usb":{"udcs":[{"state":"configured"}]},"update":{"state":"Queued","sequence":2,"download":{"state":"Verified"},"failure":null},"boot_history":{"previous_boot_id":"previous-preview-boot","last_stage":"application_ready"},"previous_boot_evidence":{"previous_orderly_shutdown":true},"kernel_taint":0,"reset_cause":null},"readiness":{"storage":{"state":"Ready"},"audio":{"state":"Ready"},"wifi":{"state":"Ready"},"bluetooth":{"state":"Ready"},"update":{"state":"Starting"}}});
    m.platform.capabilities = json!({"schema":"org.y2linux.capabilities/v1","capabilities":{"storage":{"implemented":true,"enabled":true,"qualified":false},"wifi":{"implemented":true,"enabled":true,"qualified":false},"bluetooth":{"implemented":true,"enabled":true,"qualified":false,"optional_codecs":[],"codec_auto":{"eligible_codecs":[]}},"audio":{"implemented":true,"enabled":true,"qualified":false,"enabled_rates_hz":[44100],"s32":false,"preserved_24bit":false},"ota":{"implemented":true,"enabled":true,"qualified":false},"deep_suspend":{"implemented":true,"enabled":false,"qualified":false},"usb_host":{"implemented":false,"enabled":false,"qualified":false},"automatic_bootimg_update":{"implemented":false,"enabled":false,"qualified":false}}});
    m.platform.health = json!({"state":"DEGRADED","checks":[{"name":"Storage","state":"OK","reason":"Normal"},{"name":"Audio","state":"OK","reason":"Observed"},{"name":"Wi-Fi","state":"OK","reason":"Online"},{"name":"Bluetooth","state":"OK","reason":"SBC transport"},{"name":"SD Card","state":"UNAVAILABLE","reason":"No card mounted"},{"name":"USB","state":"OK","reason":"Ready"}]});
    m.platform.audio = json!({"source":{"codec":"FLAC","sample_rate":96000,"source_bits":24},"decoder_format":"s32","internal_processing_format":"fltp","replay_gain":{"applied_gain_db":0},"eq":{"active":false},"alsa":{"rate":44100,"format":"S16_LE","channels":2}});
    m.platform.bluetooth = json!({"available":true,"powered":true,"devices":[{"name":"Studio Headphones","path":"/org/bluez/hci0/dev_PREVIEW","connected":true,"paired":true,"audio":true}],"pcms":[{"codec":"SBC","format":33296,"rate":44100,"channels":2,"running":true}]});
    m
}
fn ui() -> Ui {
    let mut ui = Ui::default();
    ui.wifi = RadioView {
        available: true,
        powered: true,
        connection: "Online · Studio".into(),
        ..Default::default()
    };
    ui.bluetooth = RadioView {
        available: true,
        powered: true,
        connection: "Connected · Studio Headphones".into(),
        ..Default::default()
    };
    ui.networks = vec![
        Item::new("A very long network name with spaces", "long-network")
            .with_secondary("-68 dBm · Secured"),
    ];
    ui.saved_networks = vec![Item::new("Studio", "saved:7").with_secondary("Saved")];
    ui.bluetooth_devices = vec![
        reborn_ui::BluetoothDeviceView {
            name: "Studio Headphones".into(), path: "/org/bluez/hci0/dev_PREVIEW".into(),
            paired: true, bonded: true, connected: true, audio_ready: true,
        },
        reborn_ui::BluetoothDeviceView {
            name: "Portable Speaker".into(), path: "/org/bluez/hci0/dev_OTHER".into(),
            ..Default::default()
        },
    ];
    ui
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "out/ui-v1-preview-quads".into()),
    );
    fs::create_dir_all(&output)?;
    let power = PowerView { charging: true };
    let mut cases = vec![];
    for (name, screen, filter) in [
        ("home", Screen::Home, ""),
        ("music", Screen::Music, ""),
        ("now-playing", Screen::NowPlaying, ""),
        ("albums", Screen::Albums, ""),
        (
            "album-detail",
            Screen::Album,
            "album:Northark\u{1f}Echoes of a Higher Place",
        ),
        ("artists", Screen::Artists, ""),
        ("artist", Screen::Artist, "artist:Northark"),
        ("songs", Screen::Tracks, ""),
        ("folders", Screen::Folders, "folder:/data/music"),
        ("queue", Screen::Queue, ""),
        ("quick-settings", Screen::QuickSettings, ""),
        ("connectivity", Screen::Connectivity, ""),
        ("wifi", Screen::Wifi, ""),
        ("bluetooth", Screen::Bluetooth, ""),
        ("settings", Screen::Settings, ""),
        ("audio-settings", Screen::SettingsAudio, ""),
        ("playback-settings", Screen::SettingsPlayback, ""),
        ("library-settings", Screen::SettingsLibrary, ""),
        ("display-settings", Screen::SettingsDisplay, ""),
        ("system", Screen::SettingsSystem, ""),
        ("diagnostics", Screen::Diagnostics, ""),
    ] {
        let mut m = model();
        m.screen = screen;
        m.navigation.filter = filter.into();
        cases.push((name.to_owned(), m));
    }
    for id in [
        "storage",
        "power",
        "clock",
        "about",
        "usb",
        "update",
        "update_diag",
        "versions",
        "health",
        "capabilities",
        "cpu",
        "thermal",
        "network",
        "bluetooth",
        "codec",
        "audio",
        "boot",
        "backup",
        "maintenance",
        "benchmarks",
    ] {
        let mut m = model();
        m.screen = Screen::Platform;
        m.navigation.filter = id.into();
        cases.push((format!("platform-{id}"), m));
    }
    let mut m = model();
    m.screen = Screen::NowPlaying;
    m.navigation.modal = Some(Modal::PowerMenu);
    m.navigation.modal_focus = 2;
    cases.push(("power-menu".into(), m));
    let mut m = model();
    m.screen = Screen::Platform;
    m.navigation.filter = "update".into();
    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::Platform(
        PlatformTask::UpdateApply,
    )));
    cases.push(("update-confirm".into(), m));
    let mut m = model();
    m.screen = Screen::Queue;
    m.navigation.modal = Some(Modal::ContextMenu);
    m.navigation.context_target = Some(1);
    m.navigation.context_key = Some(format!("queue:{}", m.queue_entry_ids[1].0));
    cases.push(("queue-context".into(), m));
    let mut m = model();
    m.screen = Screen::NowPlaying;
    m.queue_position = 8;
    cases.push(("long-metadata".into(), m));
    let mut m = model();
    m.screen = Screen::Tracks;
    m.navigation.focus = 6;
    m.navigation.scroll = 4;
    cases.push(("unicode-metadata".into(), m));
    let m = AppModel {
        screen: Screen::Tracks,
        ..Default::default()
    };
    cases.push(("empty-library".into(), m));
    let mut m = model();
    m.screen = Screen::Platform;
    m.navigation.filter = "storage".into();
    m.platform.status["storage"]["volumes"][0]["space_state"] = json!("CriticalSpace");
    m.platform.status["storage"]["volumes"][0]["available_bytes"] = json!(10485760);
    cases.push(("low-storage".into(), m));
    let mut m = model();
    m.screen = Screen::Platform;
    m.navigation.filter = "update".into();
    m.platform.status["system"]["update"]["state"] = json!("Failed");
    m.platform.status["system"]["update"]["failure"] =
        json!("Signature verification failed; installed root preserved");
    cases.push(("update-failed".into(), m));
    for (name, screen) in [
        ("pairing", Screen::Bluetooth),
        ("wifi-password", Screen::Wifi),
        ("letter-index", Screen::Tracks),
        ("track-info", Screen::TrackInfo),
        ("wifi-failed", Screen::Wifi),
        ("bluetooth-unavailable", Screen::Bluetooth),
        ("sd-removed", Screen::NowPlaying),
        ("audio-error", Screen::NowPlaying),
    ] {
        let mut m = model();
        m.screen = screen;
        if name == "sd-removed" {
            m.sources[0].online = false;
            m.playback = PlaybackState::Paused;
        }
        if name == "audio-error" {
            m.playback = PlaybackState::Error;
        }
        cases.push((name.into(), m));
    }
    for (name, page) in [
        ("clock-untrusted", "clock"),
        ("usb-disconnected", "usb"),
        ("update-unavailable", "update"),
        ("sd-mounted", "storage"),
    ] {
        let mut m = model();
        m.screen = Screen::Platform;
        m.navigation.filter = page.into();
        match name {
            "clock-untrusted" => m.platform.status["system"]["time"]["tls_ready"] = json!(false),
            "usb-disconnected" => {
                m.platform.status["system"]["ssh"]["state"] = json!("Unavailable");
                m.platform.status["system"]["usb"]["udcs"][0]["state"] = json!("not attached");
            }
            "update-unavailable" => {
                m.platform.capabilities = json!(null);
                m.platform.status["system"]["update"] = json!(null);
            }
            "sd-mounted" => {
                m.platform.status["storage"]["volumes"][1] = json!({"path":"/media/sd","state":"Ready","space_state":"Normal","filesystem":"exfat","total_bytes":64000000000_u64,"available_bytes":48000000000_u64});
                m.navigation.focus = 3;
                m.navigation.scroll = 3;
            }
            _ => {}
        }
        cases.push((name.into(), m));
    }
    for name in [
        "library-scan-interrupted",
        "update-working",
        "update-installing",
        "operation-error",
        "missing-metadata",
        "wifi-acquiring-ip",
        "wifi-off",
    ] {
        let mut m = model();
        match name {
            "library-scan-interrupted" => {
                m.screen = Screen::SettingsLibrary;
                m.navigation.focus = 4;
                m.library.error = Some("scan incomplete".into());
            }
            "update-working" => {
                m.screen = Screen::Platform;
                m.navigation.filter = "update".into();
                m.platform.busy = Some(PlatformTask::UpdateStage);
                m.platform.status["system"]["update"]["download"]["state"] = json!("Staging");
            }
            "update-installing" => {
                m.screen = Screen::Platform;
                m.navigation.filter = "update".into();
                m.platform.busy = Some(PlatformTask::UpdateApply);
            }
            "operation-error" => {
                m.screen = Screen::Platform;
                m.navigation.filter = "result".into();
                m.platform.failure =
                    Some("Operation timed out. Refresh its state before trying again.".into());
            }
            "missing-metadata" => {
                m.screen = Screen::NowPlaying;
                m.queue[0].title.clear();
                m.queue[0].artist.clear();
                m.queue[0].album.clear();
            }
            "wifi-acquiring-ip" | "wifi-off" => m.screen = Screen::Wifi,
            _ => {}
        }
        cases.push((name.into(), m));
    }
    let mut manifest = vec![];
    for (name, mut m) in cases {
        let mut ui = ui();
        match name.as_str() {
            "pairing" => {
                ui.pairing=Some("Studio Headphones · Compare this code: 123456. Confirm only if the device shows the same code.".into());
                ui.pairing_focus = 1;
            }
            "wifi-password" => {
                m.navigation.focus = 3;
                ui.model_action(&mut m, reborn_core::Action::Select);
            }
            "letter-index" => {
                ui.model_action(&mut m, reborn_core::Action::ContextMenu);
                m.navigation.modal_focus = 6;
                ui.model_action(&mut m, reborn_core::Action::Select);
            }
            "wifi-acquiring-ip" => ui.wifi.connection = "Acquiring IP address · Studio".into(),
            "wifi-off" => {
                ui.wifi.powered = false;
                ui.wifi.connection.clear();
                ui.networks.clear();
                ui.saved_networks.clear();
            }
            "wifi-failed" => ui
                .wifi
                .failed("Wrong password. Forget the network, then reconnect.".into()),
            "bluetooth-unavailable" => {
                ui.bluetooth = RadioView::default();
                ui.bluetooth_devices.clear();
            }
            _ => {}
        }
        let tracks = std::mem::take(&mut m.library.tracks);
        ui.normalize(&mut m, &tracks);
        m.library.tracks = tracks;
        let draw = ui.draw(&m, &m.library.tracks, "ok", !m.queue.is_empty(), power);
        let count = reborn_ui::focus_target_count(&draw);
        assert_eq!(count, usize::from(name != "update-installing"), "{name}");
        fs::write(
            output.join(format!("{name}.json")),
            serde_json::to_vec(
                &json!({"width":480,"height":360,"screen":name,"fixture":true,"focus_targets":count,"quads":draw}),
            )?,
        )?;
        manifest.push(name);
    }
    let draw = ui().draw_preview(model(), &tracks(), power, PreviewScreen::Boot);
    fs::write(
        output.join("boot.json"),
        serde_json::to_vec(
            &json!({"width":480,"height":360,"screen":"boot","fixture":true,"quads":draw}),
        )?,
    )?;
    manifest.push("boot".into());
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
