use super::*;

fn model(entries: &[(u64, i64)], position: usize) -> AppModel {
    AppModel {
        queue: entries
            .iter()
            .map(|&(_, id)| Track {
                id,
                ..Default::default()
            })
            .collect(),
        queue_entry_ids: entries
            .iter()
            .map(|&(entry, _)| QueueEntryId(entry))
            .collect(),
        queue_position: position,
        ..Default::default()
    }
}

#[test]
fn the_owner_is_the_current_entry_and_its_track() {
    let queue = model(&[(21, 4), (22, 9)], 1);
    assert_eq!(art_owner_of(&queue), Some((QueueEntryId(22), 9)));
    assert_eq!(art_owner_of(&AppModel::default()), None, "no song");
}

#[test]
fn a_seek_or_resume_of_the_same_song_keeps_its_cover() {
    let owner = art_owner_of(&model(&[(21, 4), (22, 9)], 0));
    assert!(keeps_art(
        owner,
        art_owner_of(&model(&[(21, 4), (22, 9)], 0))
    ));
}

#[test]
fn another_song_does_not_inherit_the_cover() {
    let shown = art_owner_of(&model(&[(21, 4), (22, 9)], 0));
    assert!(!keeps_art(
        shown,
        art_owner_of(&model(&[(21, 4), (22, 9)], 1))
    ));
    // The same track queued twice is two occurrences: Next to the copy loads a new song.
    let twice = [(21, 4), (22, 4)];
    assert!(!keeps_art(
        art_owner_of(&model(&twice, 0)),
        art_owner_of(&model(&twice, 1))
    ));
    // The same entry now holds another track.
    assert!(!keeps_art(shown, art_owner_of(&model(&[(21, 5)], 0))));
}

#[test]
fn a_logo_or_nothing_shown_is_never_kept() {
    let next = art_owner_of(&model(&[(21, 4)], 0));
    assert!(!keeps_art(None, next), "a station's logo");
    assert!(!keeps_art(None, None));
    assert!(!keeps_art(next, None), "nothing to open");
}
