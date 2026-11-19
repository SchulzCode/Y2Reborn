use super::*;
use reborn_core::Settings;

fn notes(n: usize) -> State {
    let mut lines = vec![Line::Heading("What's new".into())];
    lines.extend((0..n).map(|i| Line::Bullet(format!("Change number {i} in plain words."))));
    State::Ready(lines)
}

#[test]
fn the_page_fetches_once_and_again_only_after_a_failure() {
    let mut ui = Ui::default();
    let mut m = AppModel::default();
    assert_eq!(ui.open_whats_new(&mut m), Effect::FetchWhatsNew);
    assert_eq!(m.screen, Screen::WhatsNew);
    assert_eq!(ui.whats_new.state(), &State::Loading);
    // Still loading, or loaded: no second request.
    assert_eq!(ui.open_whats_new(&mut m), Effect::None);
    ui.whats_new.set(notes(3));
    assert_eq!(ui.open_whats_new(&mut m), Effect::None);
    ui.whats_new.set(State::Failed("offline".into()));
    assert_eq!(ui.open_whats_new(&mut m), Effect::FetchWhatsNew);
}

#[test]
fn the_wheel_reads_within_the_notes() {
    let mut ui = Ui::default();
    let mut m = AppModel::default();
    let _ = ui.open_whats_new(&mut m);
    ui.whats_new.set(notes(40));
    let room = ui.whats_new.total - (BOTTOM - TOP);
    assert!(room > 0.);
    assert_eq!(
        ui.whats_new_action(&m, Action::WheelCounterClockwise(1)),
        Some(Effect::None)
    );
    assert_eq!(ui.whats_new.offset.get(), 0., "not above the top");
    for _ in 0..200 {
        let _ = ui.whats_new_action(&m, Action::WheelClockwise(1));
    }
    assert_eq!(ui.whats_new.offset.get(), room, "not past the end");
    // Back and the other buttons keep their usual meaning; other screens are not touched.
    assert_eq!(ui.whats_new_action(&m, Action::Back), None);
    m.screen = Screen::Home;
    assert_eq!(ui.whats_new_action(&m, Action::WheelClockwise(1)), None);
}

#[test]
fn short_notes_do_not_scroll_and_new_notes_start_at_the_top() {
    let mut ui = Ui::default();
    let mut m = AppModel::default();
    let _ = ui.open_whats_new(&mut m);
    ui.whats_new.set(notes(1));
    let _ = ui.whats_new_action(&m, Action::WheelClockwise(5));
    assert_eq!(ui.whats_new.offset.get(), 0.);
    ui.whats_new.set(notes(40));
    let _ = ui.whats_new_action(&m, Action::WheelClockwise(3));
    ui.whats_new.set(notes(40));
    assert_eq!(ui.whats_new.offset.get(), 0.);
}

#[test]
fn showing_after_updates_starts_off() {
    let settings = Settings::default();
    assert!(!settings.whats_new_after_update);
    assert!(settings.whats_new_seen.is_empty());
}
