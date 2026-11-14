use super::*;
use reborn_core::{lyrics, Track};

fn fixture() -> (Ui, AppModel) {
    let mut model = AppModel {
        screen: Screen::Lyrics,
        playback: PlaybackState::Playing,
        ..AppModel::default()
    };
    model.queue = vec![Track {
        id: 1,
        source_id: "internal".into(),
        path: "/music/Homeward.mp3".into(),
        title: "Homeward".into(),
        online: true,
        ..Track::default()
    }];
    let mut ui = Ui::default();
    let text = (0..20)
        .map(|n| format!("[00:{n:02}]Original line {n}\n"))
        .collect::<String>();
    let prepared = Arc::new(Prepared::new(lyrics::parse(text.as_bytes()).unwrap()));
    ui.lyrics.publish(
        Some(Key::new(model.current().unwrap(), &[])),
        State::Ready(prepared),
    );
    (ui, model)
}

#[test]
fn menu_select_opens_lyrics_and_back_returns_to_now_playing() {
    let (mut ui, mut model) = fixture();
    model.screen = Screen::NowPlaying;
    assert_eq!(
        ui.action(&mut model, &[], Action::ContextMenu),
        Effect::None
    );
    model.navigation.modal_focus = ui
        .context_rows(&model, &[])
        .iter()
        .position(|row| row.key == "lyrics")
        .expect("Now Playing contains Lyrics");
    assert_eq!(ui.action(&mut model, &[], Action::Select), Effect::None);
    assert_eq!(model.screen, Screen::Lyrics);
    assert!(model.navigation.modal.is_none());
    ui.action(&mut model, &[], Action::Back);
    assert_eq!(model.screen, Screen::NowPlaying);
}

#[test]
fn playback_buttons_and_browsing_keep_separate_semantics() {
    let (mut ui, mut model) = fixture();
    model.position_ms = 10_000;
    ui.draw(&model, &[], false);
    assert_eq!(
        ui.action(&mut model, &[], Action::PlayPause),
        Effect::TogglePlayback
    );
    assert_eq!(
        ui.action(&mut model, &[], Action::NextTrack),
        Effect::NextTrack
    );
    assert_eq!(
        ui.action(&mut model, &[], Action::SeekBackward),
        Effect::Seek(-1)
    );
    assert_eq!(
        ui.action(&mut model, &[], Action::WheelClockwise(3)),
        Effect::None
    );
    assert!(ui.lyrics.manual.get().is_some());
    assert_eq!(model.position_ms, 10_000);
    ui.action(&mut model, &[], Action::Select);
    assert!(ui.lyrics.manual.get().is_none());
    model.screen = Screen::NowPlaying;
    assert!(matches!(
        ui.action(&mut model, &[], Action::WheelClockwise(1)),
        Effect::AdjustVolume(_) | Effect::Seek(_)
    ));
}

#[test]
fn active_line_is_pure_audio_position_and_stale_tracks_never_render() {
    let (ui, mut model) = fixture();
    model.position_ms = 9000;
    assert_eq!(ui.lyrics.active(&model), Some(9));
    model.playback = PlaybackState::Paused;
    assert_eq!(ui.lyrics.active(&model), Some(9));
    model.position_ms = 2000;
    assert_eq!(ui.lyrics.active(&model), Some(2));
    let shown = crate::tests::drawn_text(&ui.draw(&model, &[], false));
    assert!(shown.contains("Original line"));
    model.queue[0].path = "/music/Another.mp3".into();
    let shown = crate::tests::drawn_text(&ui.draw(&model, &[], false));
    assert!(shown.contains("No lyrics found"));
    assert!(!shown.contains("Original line"));
}

#[test]
fn reduced_motion_centers_immediately_and_long_lines_wrap_before_drawing() {
    let (mut ui, mut model) = fixture();
    ui.ui_motion = false;
    model.position_ms = 10_000;
    let prepared = ui.lyrics.prepared(&model).unwrap();
    let target = prepared.target(Some(10));
    ui.draw(&model, &[], false);
    assert_eq!(ui.lyrics.shown.get(), target);
    assert!(!ui.motion.settling());
    let text =
        "One original sentence travelling gently across the small screen without losing its words.";
    let doc = lyrics::parse(format!("[00:01]{text}").as_bytes()).unwrap();
    let prepared = Prepared::new(doc);
    assert!(prepared.rows.len() > 1);
    assert_eq!(
        prepared
            .rows
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        text
    );
    assert!(prepared
        .rows
        .iter()
        .all(|row| text::width(Face::S25, &row.text) <= 424.));
}

struct NoFiles;
impl crate::skin::Files for NoFiles {
    fn read(&self, _: &str, _: u64) -> Result<Vec<u8>, String> {
        Err("unused".into())
    }
    fn picture(&self, _: &str, _: u32, _: u32) -> Result<(u32, u32, Vec<u8>), String> {
        Err("unused".into())
    }
}
#[test]
fn custom_skin_fallback_uses_its_palette_without_a_lyrics_variable() {
    let (mut ui, mut model) = fixture();
    model.position_ms = 5000;
    let skin = crate::skin::compile(
        "test",
        r##"{"format":1,"name":"Light",
        "palette":{"bg":"#FFFFFF","text":"#000000","muted":"#555555","accent":"#003399"}}
        "##,
        &NoFiles,
    )
    .unwrap();
    ui.skin = Some(std::rc::Rc::new(skin));
    let frame = ui.draw(&model, &[], false);
    assert_eq!(frame[0].color, 0xffffffff);
    assert!(frame.iter().any(|q| q.color == 0x003399ff));
    assert!(crate::tests::drawn_text(&frame).contains("Original line"));
    assert!(frame.iter().all(|q| q.x.is_finite() && q.y.is_finite()));
}
