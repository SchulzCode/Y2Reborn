use super::artwork_matches_current;
use reborn_core::{AppModel, Event, QueueEntryId, Track};

#[test]
fn duplicate_track_occurrences_accept_art_only_after_their_boundary() {
    let track = Track {
        id: 4,
        ..Default::default()
    };
    let mut model = AppModel {
        generation: 7,
        queue: vec![track.clone(), track],
        queue_entry_ids: vec![QueueEntryId(21), QueueEntryId(22)],
        ..Default::default()
    };

    assert!(artwork_matches_current(&model, 7, QueueEntryId(21), 4));
    assert!(!artwork_matches_current(&model, 7, QueueEntryId(22), 4));

    model.apply(Event::TrackBoundary {
        generation: 7,
        next_entry_id: Some(QueueEntryId(22)),
        next_track_id: Some(4),
        output_position_ms: 0,
    });

    assert_eq!(model.current_entry_id(), Some(QueueEntryId(22)));
    assert!(artwork_matches_current(&model, 7, QueueEntryId(22), 4));
    assert!(!artwork_matches_current(&model, 6, QueueEntryId(22), 4));
}
