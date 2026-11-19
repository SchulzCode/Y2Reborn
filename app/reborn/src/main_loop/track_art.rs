//! What the cover worker is asked to decode for a song: a list thumbnail, Cover Flow's centre cover
//! and Now Playing's picture for the song a restart brings back.
use super::*;

/// The worker's job for `track`'s picture, filed under the renderer's `thumb` key.
pub(super) fn job(model: &AppModel, track: &Track, thumb: u64, cache: PathBuf) -> artwork::Job {
    let mount = model
        .sources
        .iter()
        .find(|s| s.id == track.source_id)
        .and_then(|s| s.mount_id);
    artwork::Job {
        thumb,
        art: artwork::Key::new(track, mount, &model.settings.music_directory).into(),
        cache,
    }
}

/// Tells the restored song's request from Cover Flow's, which is filed under the album's key
/// (`art_key`) too: a reply is told apart by its key alone.
const RESTORED: u64 = 1 << 62;

pub(super) fn restored_key(track: &Track) -> u64 {
    reborn_ui::art_key(track) ^ RESTORED
}

/// The job for Now Playing's picture of the song a restart brings back, once. The player starts
/// paused with no decoder open, and the decoder is what publishes a song's picture, so it only
/// showed after Play. `asked` keeps the key, so a song without a cover is not asked for again.
pub(super) fn restored_job(
    model: &AppModel,
    art: bool,
    asked: &mut Option<u64>,
    cache: &Path,
) -> Option<artwork::Job> {
    let track = model.current().filter(|t| !t.is_radio())?;
    if art || asked.is_some() || model.generation != 0 || model.playback != PlaybackState::Paused {
        return None;
    }
    let key = restored_key(track);
    *asked = Some(key);
    Some(job(model, track, key, cache.to_path_buf()))
}

/// The restored song's picture when it still belongs on screen: playback has not started (Play
/// moves the generation on and the decoder publishes its own), nothing else was shown, and it is
/// still the current song.
pub(super) fn restored_picture(
    model: &AppModel,
    art: bool,
    thumb: u64,
    rgba: Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let current = model.current()?;
    (!art && model.generation == 0 && restored_key(current) == thumb)
        .then_some(rgba)
        .flatten()
}

#[cfg(test)]
#[path = "../main_tests/track_art_tests.rs"]
mod tests;
