use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    root: PathBuf,
    track: Track,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "reborn-lyrics-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("Original.mp3");
        fs::write(&path, b"local media fixture").unwrap();
        let meta = fs::metadata(&path).unwrap();
        let modified = meta
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        let track = Track {
            id: 1,
            source_id: "internal".into(),
            path,
            online: true,
            size: meta.len(),
            mtime: modified,
            ..Track::default()
        };
        Self { root, track }
    }
    fn model(&self) -> AppModel {
        let mut model = AppModel {
            screen: Screen::Lyrics,
            ..AppModel::default()
        };
        model.settings.music_directory = self.root.clone();
        model.queue = vec![self.track.clone()];
        model
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn wait(controller: &mut Controller, model: &AppModel, view: &mut reborn_ui::lyrics::View) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        controller.tick(model, view);
        if !matches!(view.state, State::Loading) {
            return;
        }
        assert!(Instant::now() < deadline, "lyrics worker did not finish");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn actual_sidecar_load_is_cached_tracks_the_audio_position_and_quiesces() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("Original.lrc"),
        b"[00:01]A new day\n[00:03]We travel on",
    )
    .unwrap();
    let (mut controller, mut view) = (Controller::default(), reborn_ui::lyrics::View::default());
    let mut model = fixture.model();
    wait(&mut controller, &model, &mut view);
    assert!(matches!(view.state, State::Ready(_)), "{:?}", view.state);
    model.position_ms = 3200;
    assert!(controller.tick(&model, &mut view));
    assert_eq!(view.active(&model), Some(1));
    assert!(
        !controller.tick(&model, &mut view),
        "unchanged lines do not request redraws"
    );
    model.position_ms = 1500;
    controller.tick(&model, &mut view);
    assert_eq!(view.active(&model), Some(0));
    assert!(controller.quiesce());
}

#[test]
fn missing_invalid_and_oversized_files_do_not_change_playback() {
    let fixture = Fixture::new();
    for contents in [
        None,
        Some(vec![0xff]),
        Some(vec![b'x'; reborn_core::lyrics::MAX_BYTES + 1]),
    ] {
        let sidecar = fixture.root.join("Original.lrc");
        if let Some(bytes) = contents.as_ref() {
            fs::write(&sidecar, bytes).unwrap();
        }
        let mut model = fixture.model();
        model.playback = PlaybackState::Playing;
        model.position_ms = 42_000;
        let (mut controller, mut view) =
            (Controller::default(), reborn_ui::lyrics::View::default());
        wait(&mut controller, &model, &mut view);
        assert_eq!(model.position_ms, 42_000);
        assert_eq!(model.playback, PlaybackState::Playing);
        if contents.is_none() {
            assert!(matches!(view.state, State::Missing));
        } else {
            assert!(matches!(view.state, State::Failed(_)), "{:?}", view.state);
        }
    }
}

#[test]
fn track_change_and_storage_loss_clear_the_previous_document() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("Original.LRC"), b"[00:00]Only this song").unwrap();
    let mut model = fixture.model();
    let (mut controller, mut view) = (Controller::default(), reborn_ui::lyrics::View::default());
    wait(&mut controller, &model, &mut view);
    assert!(matches!(view.state, State::Ready(_)));
    model.queue[0].path = fixture.root.join("Absent.mp3");
    controller.tick(&model, &mut view);
    assert!(matches!(view.state, State::Loading));
    assert_eq!(view.active(&model), None);
    model.queue[0].online = false;
    controller.tick(&model, &mut view);
    assert!(matches!(view.state, State::Missing));
    assert!(view.key.is_none());
}

#[test]
fn closing_lyrics_cancels_and_normal_playback_never_starts_a_reader() {
    let fixture = Fixture::new();
    let mut model = fixture.model();
    model.screen = Screen::NowPlaying;
    let (mut controller, mut view) = (Controller::default(), reborn_ui::lyrics::View::default());
    assert!(!controller.tick(&model, &mut view));
    assert!(controller.worker.is_none());
    model.screen = Screen::Lyrics;
    wait(&mut controller, &model, &mut view);
    model.screen = Screen::NowPlaying;
    controller.tick(&model, &mut view);
    assert!(controller.key.is_none());
    assert!(view.key.is_none());
}
