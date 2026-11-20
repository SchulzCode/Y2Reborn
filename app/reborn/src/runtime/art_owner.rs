//! Whose cover Now Playing shows, so that opening the same song again (a seek, a resume after the
//! pause released its sink) does not blank the picture until the decoder publishes it anew.
use crate::*;

/// A song in the queue as the decoder tags its picture (`PlaybackEvent::Artwork`): the queue entry
/// and the library id. The entry tells two occurrences of one song apart.
pub(crate) type ArtOwner = (QueueEntryId, i64);

/// The song a `load` is about to open.
pub(crate) fn art_owner_of(model: &AppModel) -> Option<ArtOwner> {
    Some((model.current_entry_id()?, model.current()?.id))
}

/// Whether the picture shown (`shown`: whose it is, `None` for a station's logo) still belongs
/// to the song about to open (`next`).
pub(crate) fn keeps_art(shown: Option<ArtOwner>, next: Option<ArtOwner>) -> bool {
    shown.is_some() && shown == next
}

#[cfg(test)]
#[path = "../main_tests/art_owner_tests.rs"]
mod tests;
