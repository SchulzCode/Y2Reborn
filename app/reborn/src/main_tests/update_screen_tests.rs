use super::update_screen_waits;
use reborn_core::PlatformTask::{Refresh, ReportSend, UpdateApply, UpdateStage};

#[test]
fn the_update_screen_never_stands_without_an_update_task() {
    // Update Now while a status observation ran: the stage task was
    // dropped and the screen stood at step 1 until a restart.
    assert!(update_screen_waits(true, false, None, None));
    assert!(update_screen_waits(true, false, Some(Refresh), None));
    assert!(update_screen_waits(true, false, Some(ReportSend), None));
    // Queued behind another task, running, or restarting to install: fine.
    assert!(!update_screen_waits(
        true,
        false,
        Some(Refresh),
        Some(UpdateStage)
    ));
    assert!(!update_screen_waits(true, false, Some(UpdateStage), None));
    assert!(!update_screen_waits(true, false, Some(UpdateApply), None));
    assert!(!update_screen_waits(true, true, None, None));
    assert!(!update_screen_waits(false, false, None, None));
}
