//! Original LRC demonstration rendered through the production native UI and skin fallback.
#[path = "../skins.rs"]
mod skins;
use reborn_core::{
    lyrics::{self, Key},
    AppModel, Modal, PlaybackState, Screen, Track,
};
use reborn_ui::{
    lyrics::{Prepared, State},
    Ui,
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

fn write(
    ui: &Ui,
    model: &AppModel,
    output: &Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(
        output.join(format!("{name}.json")),
        serde_json::to_vec(&json!({
            "width":480,"height":360,"fixture":true,
            "quads":ui.draw(model, &model.library.tracks, false)
        }))?,
    )?;
    Ok(())
}

fn install_skin(ui: &mut Ui, dir: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let skin = skins::load(dir, "lyrics-preview", &|path, w, h| {
        reborn_media::decode_image(path, w, h)
    })?;
    fs::write(output.join("skin-atlas.rgba"), &skin.atlas)?;
    fs::write(
        output.join("skin-atlas.json"),
        serde_json::to_vec(&json!({
            "width":skin.atlas_width,"height":skin.atlas_height,"square_covers":skin.square_covers
        }))?,
    )?;
    ui.skin = Some(Rc::new(skin));
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let output = PathBuf::from(
        args.first()
            .ok_or("usage: reborn-lyrics-preview OUT [SKIN_DIR]")?,
    );
    fs::create_dir_all(&output)?;
    let mut ui = Ui::default();
    ui.motion_fixed = true;
    ui.clock_fixed = Some(1_794_374_460);
    ui.themes = skins::scan(&[Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skins")]);
    if let Some(dir) = args.get(1) {
        install_skin(&mut ui, Path::new(dir), &output)?;
    }
    let mut model = AppModel {
        screen: Screen::Lyrics,
        playback: PlaybackState::Playing,
        position_ms: 13_000,
        ..AppModel::default()
    };
    model.queue = vec![Track {
        id: 1,
        source_id: "internal".into(),
        title: "Homeward".into(),
        artist: "Reborn original examples".into(),
        path: "/music/Homeward.mp3".into(),
        duration_ms: 45_000,
        online: true,
        ..Track::default()
    }];
    let key = Key::new(model.current().ok_or("missing fixture track")?, &[]);
    let document = lyrics::parse(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/fixtures/lyrics-original.lrc"
    )))
    .map_err(|error| error.message())?;
    ui.lyrics.publish(
        Some(key.clone()),
        State::Ready(Arc::new(Prepared::new(document))),
    );
    write(&ui, &model, &output, "lyrics")?;
    model.playback = PlaybackState::Paused;
    write(&ui, &model, &output, "lyrics-paused")?;
    ui.lyrics.publish(Some(key), State::Missing);
    write(&ui, &model, &output, "lyrics-missing")?;
    model.screen = Screen::NowPlaying;
    model.navigation.modal = Some(Modal::ContextMenu);
    model.navigation.modal_focus = 1;
    write(&ui, &model, &output, "lyrics-menu")?;
    Ok(())
}
