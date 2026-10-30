use super::DirtyState;

#[test]
fn stopped_setting_save_keeps_its_redraw_request() {
    let mut dirty = DirtyState::default();
    dirty.mark_both(); // SetScreenTimeout while playback is stopped.

    dirty.checkpoint_succeeded();

    assert!(!dirty.checkpoint_due());
    assert!(dirty.render);
}

#[test]
fn rendering_before_checkpoint_keeps_persistence_pending() {
    let mut dirty = DirtyState::default();
    dirty.mark_both();

    dirty.rendered();

    assert!(dirty.checkpoint_due());
    dirty.checkpoint_succeeded();
    assert!(!dirty.checkpoint_due());
}

#[test]
fn checkpoint_before_render_keeps_redraw_pending() {
    let mut dirty = DirtyState::default();
    dirty.mark_both();

    dirty.checkpoint_succeeded();

    assert!(dirty.render);
    dirty.rendered();
    assert!(!dirty.render);
}

#[test]
fn bluetooth_loss_notice_can_be_saved_and_still_rendered() {
    let mut dirty = DirtyState::default();
    dirty.mark_both(); // Lost transport changes output/playback and sets a notice.

    dirty.checkpoint_succeeded();

    assert!(!dirty.checkpoint_due());
    assert!(dirty.render);
}

#[test]
fn rendered_position_change_remains_due_for_bounded_checkpoint() {
    let mut dirty = DirtyState::default();
    dirty.mark_persistence(); // Position events are saved on the 15-second cadence.
    dirty.mark_render();
    dirty.rendered();

    assert!(dirty.checkpoint_due());
    dirty.checkpoint_succeeded();
    assert!(!dirty.checkpoint_due());
}

#[test]
fn unchanged_state_does_not_schedule_a_save_loop() {
    let mut dirty = DirtyState::default();
    assert!(!dirty.checkpoint_due());

    dirty.mark_render();
    dirty.rendered();

    assert!(!dirty.checkpoint_due());
}

#[test]
fn failed_checkpoint_retains_persistence_intent() {
    let mut dirty = DirtyState::default();
    dirty.mark_persistence();

    dirty.checkpoint_failed();

    assert!(dirty.checkpoint_due());
}
