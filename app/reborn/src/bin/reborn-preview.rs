#![forbid(unsafe_code)]

use reborn_core::{AppModel, AudioOutput, PlaybackState, Screen, Track};
use reborn_ui::{PowerView, PreviewScreen, Ui};
use serde_json::json;
use std::{fs, path::PathBuf};

fn track(id: i64, title: &str, album: &str, number: u32, duration_ms: u64) -> Track {
    Track {
        id,
        source_id: "internal".into(),
        path: PathBuf::from(format!(
            "/data/music/northark/{album}/{number:02}-{title}.flac"
        )),
        filename: format!("{title}.flac"),
        title: title.into(),
        artist: "Northark".into(),
        album: album.into(),
        album_artist: "Northark".into(),
        track: number,
        disc: 1,
        duration_ms,
        codec: "FLAC".into(),
        sample_rate: 96_000,
        channels: 2,
        bitrate: 2_400_000,
        artwork: true,
        online: true,
        ..Default::default()
    }
}

fn demo_tracks() -> Vec<Track> {
    vec![
        track(
            1,
            "A Brighter Silence",
            "Echoes of a Higher Place",
            1,
            318_000,
        ),
        track(
            2,
            "The Still Procession",
            "Echoes of a Higher Place",
            2,
            276_000,
        ),
        track(3, "A Distant Glow", "Echoes of a Higher Place", 3, 312_000),
        track(
            4,
            "Signals in the Dark",
            "Echoes of a Higher Place",
            4,
            261_000,
        ),
        track(5, "Higher Ground", "Towards Farther Shores", 1, 362_000),
        track(6, "Where We Remain", "The Still Between", 1, 318_000),
        track(7, "Letters to Nowhere", "The Still Between", 2, 296_000),
        track(8, "Lumière", "Lumière", 1, 284_000),
        track(9, "The Further We Go", "The Further We Go", 1, 301_000),
        track(10, "Still Here", "Still Here", 1, 272_000),
    ]
}

fn model(tracks: &[Track], screen: Screen, focus: usize) -> AppModel {
    let mut model = AppModel {
        playback: PlaybackState::Playing,
        queue: tracks.to_vec(),
        queue_position: 0,
        position_ms: 102_000,
        output: AudioOutput::Wired,
        screen,
        ..Default::default()
    };
    model.navigation.focus = focus;
    model.navigation.filter = if screen == Screen::Artist {
        "artist:Northark".into()
    } else {
        String::new()
    };
    model.settings.volume = 60;
    model.settings.gapless_enabled = true;
    model.sources = vec![reborn_core::Source {
        id: "internal".into(),
        kind: reborn_core::MediaSource::Internal,
        root: "/data/music".into(),
        online: true,
        mount: "internal".into(),
    }];
    model
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "out/reborn-ui-previews".into());
    let output = PathBuf::from(output);
    fs::create_dir_all(&output)?;
    let tracks = demo_tracks();
    let ui = Ui::default();
    let power = PowerView {
        battery_percent: Some(92),
        charging: false,
    };
    let previews = [
        (
            "now-playing",
            PreviewScreen::NowPlaying,
            model(&tracks, Screen::NowPlaying, 1),
        ),
        (
            "library",
            PreviewScreen::Library,
            model(&tracks, Screen::Albums, 0),
        ),
        (
            "artist",
            PreviewScreen::Artist,
            model(&tracks, Screen::Artist, 1),
        ),
        (
            "queue",
            PreviewScreen::Queue,
            model(&tracks, Screen::Queue, 2),
        ),
        (
            "settings",
            PreviewScreen::Settings,
            model(&tracks, Screen::SettingsAudio, 0),
        ),
        (
            "quick-settings",
            PreviewScreen::QuickSettings,
            model(&tracks, Screen::Connectivity, 0),
        ),
        (
            "lock",
            PreviewScreen::Lock,
            model(&tracks, Screen::NowPlaying, 1),
        ),
        ("boot", PreviewScreen::Boot, model(&tracks, Screen::Home, 0)),
    ];
    let mut manifest = Vec::new();
    for (name, screen, model) in previews {
        let quads = ui.draw_preview(model, &tracks, power, screen);
        let document = json!({"width":480,"height":360,"screen":name,"quads":quads});
        fs::write(
            output.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&document)?,
        )?;
        manifest.push(name);
    }
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!(
        "wrote {} deterministic 480x360 preview states to {}",
        manifest.len(),
        output.display()
    );
    Ok(())
}
