use super::{input_ticks, jack_move, wheel_clicks, JackMove, HEADPHONES_LOST};
use reborn_core::{Action, NormalizedInput, PhysicalControl, WheelClick};

/// #62: unplugged headphones that were playing pause, with a message; a
/// playing speaker moves to plugged headphones; anything else stays.
#[test]
fn unplugged_headphones_pause_and_say_so() {
    assert_eq!(
        jack_move(true, true, false, Some(true), false),
        JackMove::Pause
    );
    assert_eq!(HEADPHONES_LOST, "Headphones unplugged. Playback paused.");
    // Not playing, on Bluetooth, the speaker playing or the first reading:
    // nothing paused, so no message.
    assert_eq!(
        jack_move(true, false, false, Some(true), false),
        JackMove::Nothing
    );
    assert_eq!(
        jack_move(false, true, false, Some(true), false),
        JackMove::Nothing
    );
    assert_eq!(
        jack_move(true, true, false, Some(true), true),
        JackMove::Nothing
    );
    assert_eq!(jack_move(true, true, false, None, false), JackMove::Nothing);
    assert_eq!(
        jack_move(true, true, true, Some(false), true),
        JackMove::ToHeadphones
    );
}

/// #28: a tick per press and per seek step of a held Previous/Next on a
/// lit screen; none for the wheel (also its Now Playing seek), a hold
/// that seeks nothing (a station) or in the dark.
#[test]
fn haptics_tick_for_presses_and_held_seek_steps_only() {
    let next = PhysicalControl::Next;
    assert!(input_ticks(true, NormalizedInput::Press(next), &[], false));
    assert!(!input_ticks(
        false,
        NormalizedInput::Press(next),
        &[],
        false
    ));
    for held in [
        NormalizedInput::LongPress(next),
        NormalizedInput::Repeat(next),
    ] {
        assert!(input_ticks(true, held, &[Action::SeekForward], true));
        assert!(!input_ticks(true, held, &[Action::SeekForward], false));
        assert!(!input_ticks(false, held, &[Action::SeekForward], true));
        assert!(!input_ticks(true, held, &[], false));
    }
    let back = NormalizedInput::Repeat(PhysicalControl::Previous);
    assert!(input_ticks(true, back, &[Action::SeekBackward], true));
    let wheel = NormalizedInput::WheelClockwise(1);
    assert!(!input_ticks(
        true,
        wheel,
        &[Action::WheelClockwise(1)],
        true
    ));
    assert!(!input_ticks(
        true,
        NormalizedInput::Release(next),
        &[Action::NextTrack],
        false
    ));
}

/// #67: wheel steps click on a lit screen as the setting says; presses and
/// the dark never do.
#[test]
fn wheel_steps_click_as_the_setting_says() {
    let step = NormalizedInput::WheelCounterClockwise(1);
    assert!(wheel_clicks(true, step, WheelClick::Always, true));
    assert!(wheel_clicks(true, step, WheelClick::WhenPaused, false));
    assert!(!wheel_clicks(true, step, WheelClick::WhenPaused, true));
    assert!(!wheel_clicks(true, step, WheelClick::Off, false));
    assert!(!wheel_clicks(false, step, WheelClick::Always, false));
    let press = NormalizedInput::Press(PhysicalControl::Select);
    assert!(!wheel_clicks(true, press, WheelClick::Always, false));
}
