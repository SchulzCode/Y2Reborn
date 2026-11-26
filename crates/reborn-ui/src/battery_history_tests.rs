use super::*;
use crate::{tests::drawn_text, Action, Effect};
use reborn_core::{
    battery_history::{Ledger, Record},
    Screen,
};
use std::sync::Arc;

/// Monday 2026-10-12 09:41 UTC (the clock the other UI tests freeze).
const NOW: i64 = 1_791_798_060;

fn level(hours_ago: i64, pct: u8, activity: Activity) -> Record {
    Record::Level {
        t: NOW - hours_ago * 3600,
        pct,
        activity,
        bt: false,
        est: true,
    }
}

/// A charge to 100% yesterday evening, then music, the screen, and a night asleep.
fn model() -> AppModel {
    let mut m = AppModel {
        screen: Screen::BatteryHistory,
        ..Default::default()
    };
    m.battery_history = Arc::new(Ledger::from_records(vec![
        level(40, 20, Activity::Charging),
        level(36, 100, Activity::Charging),
        level(35, 98, Activity::Playing),
        level(30, 80, Activity::ScreenOn),
        level(29, 77, Activity::Playing),
        level(22, 60, Activity::Suspended),
        level(8, 55, Activity::ScreenOn),
        level(7, 52, Activity::Playing),
        level(1, 40, Activity::Awake),
    ]));
    m
}

fn ui() -> Ui {
    Ui {
        clock_fixed: Some(NOW),
        ..Default::default()
    }
}

fn text(ui: &Ui, m: &AppModel) -> String {
    drawn_text(&ui.draw(m, &m.library.tracks, false))
}

#[test]
fn the_battery_page_opens_the_history_and_back_returns() {
    let mut ui = ui();
    let mut m = model();
    m.screen = Screen::Battery;
    ui.normalize(&mut m, &[]);
    assert!(text(&ui, &m).contains("History"));
    assert_eq!(ui.select_row(&mut m, &[], "battery_history"), Effect::None);
    assert_eq!(m.screen, Screen::BatteryHistory);
    assert_eq!(crate::screens::screen_title(&m), "Battery History");
    ui.model_action(&mut m, Action::Back);
    assert_eq!(m.screen, Screen::Battery);
}

fn turn(ui: &mut Ui, m: &mut AppModel, steps: i32) {
    for _ in 0..steps.abs() {
        let action = if steps > 0 {
            Action::WheelClockwise(1)
        } else {
            Action::WheelCounterClockwise(1)
        };
        ui.model_action(m, action);
    }
}

#[test]
fn without_records_it_says_so_and_shows_no_controls() {
    let ui = ui();
    let mut m = model();
    m.battery_history = Arc::default();
    let shown = text(&ui, &m);
    assert!(shown.contains("No history yet"), "{shown}");
    assert!(
        !shown.contains("Range") && !shown.contains("View"),
        "{shown}"
    );
}

#[test]
fn the_day_starts_at_the_latest_half_hour_and_names_it() {
    let ui = ui();
    let shown = text(&ui, &model());
    assert!(
        shown.contains("24 hours") && shown.contains("Days"),
        "{shown}"
    );
    assert!(
        shown.contains("Today 09:30") && shown.contains("40%"),
        "{shown}"
    );
    assert!(
        shown.contains("Awake") && shown.contains("Playing"),
        "{shown}"
    );
    assert!(
        shown.contains("Turn to browse · Select for days"),
        "{shown}"
    );
    // Unplugged 35 h ago at 100%: Saturday 22:41 UTC.
    assert!(
        shown.contains("Last charged to 100%, Sat 10 Oct 22:41"),
        "{shown}"
    );
    assert!(shown.contains("Percent is estimated"));
    // Every third hour is labelled; nothing on the page looks like a button.
    assert!(
        shown.contains("09") && shown.contains("12") && shown.contains("21"),
        "{shown}"
    );
    assert!(
        !shown.contains("Range") && !shown.contains("View"),
        "{shown}"
    );
}

#[test]
fn the_wheel_moves_over_the_half_hours_and_stops_at_the_ends() {
    let mut ui = ui();
    let mut m = model();
    turn(&mut ui, &mut m, -3);
    let shown = text(&ui, &m);
    // 08:00 to 08:30 was music at 52%.
    assert!(
        shown.contains("Today 08:00") && shown.contains("52%"),
        "{shown}"
    );
    turn(&mut ui, &mut m, 20);
    assert!(
        text(&ui, &m).contains("Today 09:30"),
        "no later than the latest record"
    );
    turn(&mut ui, &mut m, -100);
    let shown = text(&ui, &m);
    assert!(
        shown.contains("Yesterday 10:00"),
        "the first half hour of the window: {shown}"
    );
}

#[test]
fn select_zooms_out_to_the_days_and_the_wheel_chooses_a_day() {
    let mut ui = ui();
    let mut m = model();
    ui.model_action(&mut m, Action::Select);
    let shown = text(&ui, &m);
    assert!(
        shown.contains("Mon 12 Oct") && shown.contains("Used"),
        "{shown}"
    );
    assert!(shown.contains("Select for this day's 24 hours"), "{shown}");
    turn(&mut ui, &mut m, -1);
    let shown = text(&ui, &m);
    assert!(
        shown.contains("Sun 11 Oct") && shown.contains("Asleep"),
        "{shown}"
    );
    turn(&mut ui, &mut m, 5);
    assert!(
        text(&ui, &m).contains("Mon 12 Oct"),
        "today is the newest day"
    );
    // Twelve days back is past the ten shown: the window follows the cursor.
    turn(&mut ui, &mut m, -12);
    let shown = text(&ui, &m);
    assert!(
        shown.contains("Wed 30 Sep") && shown.contains("No record"),
        "{shown}"
    );
}

#[test]
fn select_on_a_day_opens_its_24_hours_and_select_goes_back_to_that_day() {
    let mut ui = ui();
    let mut m = model();
    ui.model_action(&mut m, Action::Select);
    turn(&mut ui, &mut m, -1);
    ui.model_action(&mut m, Action::Select);
    let shown = text(&ui, &m);
    assert!(shown.contains("Yesterday 23:30"), "{shown}");
    assert!(
        shown.contains("Turn to browse · Select for days"),
        "{shown}"
    );
    turn(&mut ui, &mut m, -1);
    assert!(text(&ui, &m).contains("Yesterday 23:00"));
    ui.model_action(&mut m, Action::Select);
    assert!(text(&ui, &m).contains("Sun 11 Oct"));
    // Back leaves the page from either view.
    ui.model_action(&mut m, Action::Back);
    assert_ne!(m.screen, Screen::BatteryHistory);
}

#[test]
fn the_cursor_is_the_one_focus_in_both_views_and_everything_stays_on_screen() {
    for days in [false, true] {
        let mut ui = ui();
        let mut m = model();
        if days {
            ui.model_action(&mut m, Action::Select);
        }
        ui.normalize(&mut m, &[]);
        let quads = ui.draw(&m, &[], false);
        assert_eq!(crate::focus_target_count(&quads), 1, "days: {days}");
        for q in &quads {
            assert!(
                q.x >= -1. && q.y >= -1. && q.x + q.w <= 481. && q.y + q.h <= 361.,
                "{q:?}"
            );
        }
    }
}

#[test]
fn settings_system_leads_to_the_battery_and_its_history() {
    let mut ui = ui();
    let mut m = model();
    m.screen = Screen::Update;
    ui.normalize(&mut m, &[]);
    let shown = text(&ui, &m);
    assert!(shown.contains("Battery"), "{shown}");
    assert_eq!(ui.select_row(&mut m, &[], "battery"), Effect::None);
    assert_eq!(m.screen, Screen::Battery);
    assert_eq!(ui.select_row(&mut m, &[], "battery_history"), Effect::None);
    assert_eq!(m.screen, Screen::BatteryHistory);
}

#[test]
fn the_cursor_names_the_mix_of_its_half_hour() {
    let mut ui = ui();
    let mut m = model();
    // 02:30 to 03:00: the screen until 02:41, then music.
    turn(&mut ui, &mut m, -14);
    let shown = text(&ui, &m);
    assert!(shown.contains("Today 02:30"), "{shown}");
    assert!(shown.contains("Playing 19 m · Screen on 11 m"), "{shown}");
}

#[test]
fn the_area_is_drawn_at_a_finer_grain_than_the_half_hours() {
    let ui = ui();
    let m = model();
    let quads = ui.draw(&m, &[], false);
    // The window starts at 10:00 yesterday: 02:30 today is the 33rd half hour.
    let slot = (WIDTH - LEFT - RIGHT) / history::SLOTS as f32;
    let (from, to) = (LEFT + 33. * slot, LEFT + 34. * slot);
    let mut columns: Vec<(f32, u32)> = quads
        .iter()
        .filter(|q| q.w == COLUMN && (q.y + q.h - (TOP + HEIGHT)).abs() < 0.01)
        .filter(|q| q.x >= from - 0.01 && q.x < to)
        .map(|q| (q.x, q.color))
        .collect();
    columns.sort_by(|a, b| a.0.total_cmp(&b.0));
    // The screen until 02:41 (about a third of the half hour), then music.
    let colours: std::collections::BTreeSet<u32> = columns.iter().map(|c| c.1).collect();
    assert!(columns.len() >= 6 && colours.len() == 2, "{columns:?}");
    let first = columns.first().map(|c| c.1);
    assert_ne!(first, columns.last().map(|c| c.1));
}
