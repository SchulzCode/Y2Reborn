use super::main_loop_interval;
use std::time::Duration;

#[test]
fn screen_off_wakes_less_but_stays_responsive() {
    assert_eq!(
        main_loop_interval(false, false, false, false, Duration::ZERO),
        Duration::from_millis(250)
    );
    assert_eq!(
        main_loop_interval(true, true, false, false, Duration::ZERO),
        Duration::from_secs(1)
    );
    assert_eq!(
        main_loop_interval(false, true, false, false, Duration::from_millis(16)),
        Duration::from_millis(18)
    );
    assert_eq!(
        main_loop_interval(false, true, false, false, Duration::from_millis(40)),
        Duration::ZERO
    );
    // A glide keeps frames coming, even with nothing marked dirty yet
    // (Cover Flow measured ~10 fps without this), and the page flip paces
    // them rather than the calm 34-ms budget (27 fps measured on 24).
    assert_eq!(
        main_loop_interval(false, false, true, false, Duration::from_millis(6)),
        Duration::from_millis(6)
    );
    assert_eq!(
        main_loop_interval(false, true, true, true, Duration::from_millis(16)),
        Duration::ZERO
    );
    assert!(super::frame_gap(true) < Duration::from_micros(15_800));
    // Screen off never animates.
    assert_eq!(
        main_loop_interval(true, false, true, false, Duration::ZERO),
        Duration::from_secs(1)
    );
}
