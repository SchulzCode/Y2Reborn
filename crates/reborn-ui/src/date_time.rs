//! Settings > Display > Date & Time (#53): the date and time set by hand,
//! for a player that never reaches Wi-Fi. Each field opens a picker on the
//! draft; Set hands the instant to the platform. Network time replaces it
//! whenever Wi-Fi connects.
use crate::{time_zone, Item, RowKind, Ui};
use reborn_core::{
    clock::{self, Local},
    AppModel, DateField,
};

pub(crate) const FIELDS: [DateField; 5] = [
    DateField::Year,
    DateField::Month,
    DateField::Day,
    DateField::Hour,
    DateField::Minute,
];

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// The platform's range around its build date (`timekeeping.plausible`).
const BEFORE_BUILD_S: i64 = 86_400;
const AFTER_BUILD_S: i64 = 5 * 366 * 86_400;

/// The clock now, in the chosen zone.
fn current(ui: &Ui, m: &AppModel) -> Local {
    time_zone::rule(m).local(time_zone::now(ui))
}

/// What the page shows: the draft being set, else the clock.
fn draft(ui: &Ui, m: &AppModel) -> Local {
    ui.date_draft.unwrap_or_else(|| current(ui, m))
}

fn twelve_hour(m: &AppModel) -> bool {
    clock::twelve_hour(&m.settings.time_zone)
}

fn hour_label(hour: u8, twelve: bool) -> String {
    if twelve {
        let h = match hour % 12 {
            0 => 12,
            h => h,
        };
        format!("{h} {}", if hour < 12 { "AM" } else { "PM" })
    } else {
        format!("{hour:02}")
    }
}

fn month_name(month: u8) -> &'static str {
    MONTHS[usize::from(month.clamp(1, 12)) - 1]
}

/// "22 Oct 2026, 14:05", or "Oct 22, 2026, 2:05 PM" with a 12-hour clock.
fn text(t: &Local, twelve: bool) -> String {
    let (month, time) = (&month_name(t.month)[..3], clock::clock_text(t, twelve));
    if twelve {
        format!("{month} {}, {}, {time}", t.day, t.year)
    } else {
        format!("{} {month} {}, {time}", t.day, t.year)
    }
}

/// The Display row's value: the clock as it reads now.
pub(crate) fn summary(ui: &Ui, m: &AppModel) -> String {
    text(&current(ui, m), twelve_hour(m))
}

fn label(field: DateField) -> &'static str {
    match field {
        DateField::Year => "Year",
        DateField::Month => "Month",
        DateField::Day => "Day",
        DateField::Hour => "Hour",
        DateField::Minute => "Minute",
    }
}

fn key(field: DateField) -> &'static str {
    match field {
        DateField::Year => "year",
        DateField::Month => "month",
        DateField::Day => "day",
        DateField::Hour => "hour",
        DateField::Minute => "minute",
    }
}

/// The field a row key (`date_field:<name>`) names.
pub(crate) fn field(name: &str) -> Option<DateField> {
    FIELDS.into_iter().find(|f| key(*f) == name)
}

fn value(field: DateField, t: &Local) -> i32 {
    match field {
        DateField::Year => t.year,
        DateField::Month => i32::from(t.month),
        DateField::Day => i32::from(t.day),
        DateField::Hour => i32::from(t.hour),
        DateField::Minute => i32::from(t.minute),
    }
}

fn value_label(field: DateField, n: i32, twelve: bool) -> String {
    match field {
        DateField::Month => month_name(n as u8).into(),
        DateField::Hour => hour_label(n as u8, twelve),
        DateField::Minute => format!("{n:02}"),
        DateField::Year | DateField::Day => n.to_string(),
    }
}

/// The build date's range in years, else a few around the draft's year.
fn years(ui: &Ui, m: &AppModel) -> std::ops::RangeInclusive<i32> {
    let year = draft(ui, m).year;
    let (first, last) = match m.platform.snapshot.info.build_epoch {
        Some(build) => (
            clock::civil(build - BEFORE_BUILD_S).year,
            clock::civil(build + AFTER_BUILD_S).year,
        ),
        None => (year - 1, year + 5),
    };
    first.min(year)..=last.max(year)
}

fn choices(ui: &Ui, m: &AppModel, field: DateField) -> std::ops::RangeInclusive<i32> {
    let t = draft(ui, m);
    match field {
        DateField::Year => years(ui, m),
        DateField::Month => 1..=12,
        DateField::Day => 1..=i32::from(clock::days_in_month(t.year, t.month)),
        DateField::Hour => 0..=23,
        DateField::Minute => 0..=59,
    }
}

/// The clock came from the network this boot: it is right, and a running
/// time server would not take a hand-set time back for minutes while calling
/// it network time (8.22 test build). Only shown then.
pub(crate) fn network_time(m: &AppModel) -> bool {
    m.platform.snapshot.info.clock_source.as_deref() == Some("ntp")
}

/// The page: year, month, day and time (hour, then minute), then Set; five
/// rows, so Set is on screen without scrolling. With network time only the
/// clock itself.
pub(crate) fn rows(ui: &Ui, m: &AppModel) -> Vec<Item> {
    if network_time(m) {
        return vec![Item::new("Date & Time", "").with_secondary(summary(ui, m))];
    }
    let (t, twelve) = (draft(ui, m), twelve_hour(m));
    let mut rows: Vec<Item> = FIELDS[..3]
        .iter()
        .map(|&f| {
            Item::new(label(f), format!("date_field:{}", key(f))).with_secondary(value_label(
                f,
                value(f, &t),
                twelve,
            ))
        })
        .collect();
    rows.push(Item::new("Time", "date_field:hour").with_secondary(clock::clock_text(&t, twelve)));
    rows.push(Item::new("Set Date & Time", "date_set").with_kind(RowKind::Chevron));
    rows
}

/// The picker that follows a field's: the minute after the hour.
pub(crate) fn then(field: DateField) -> Option<DateField> {
    (field == DateField::Hour).then_some(DateField::Minute)
}

/// The line under the page.
pub(crate) fn hint(m: &AppModel) -> &'static str {
    if network_time(m) {
        "The time comes from the internet while Wi-Fi is connected."
    } else {
        "With Wi-Fi the player sets the time by itself."
    }
}

/// A field's picker: its values and the one to focus (the draft's).
pub(crate) fn picker(ui: &Ui, m: &AppModel, field: DateField) -> (Vec<Item>, usize) {
    let (now, twelve) = (value(field, &draft(ui, m)), twelve_hour(m));
    let range = choices(ui, m, field);
    let focus = (now - range.start()).max(0) as usize;
    let rows = range
        .map(|n| Item::new(value_label(field, n, twelve), format!("date_value:{n}")))
        .collect();
    (rows, focus)
}

pub(crate) fn picker_copy(field: DateField) -> (String, String) {
    (
        label(field).into(),
        "Turn the wheel to choose. Select keeps it; Back cancels.".into(),
    )
}

/// Keep a picked value in the draft; a day past the month's end becomes
/// its last day (31 March, then February: 28 or 29).
pub(crate) fn choose(ui: &mut Ui, m: &AppModel, field: DateField, n: i32) {
    let mut t = draft(ui, m);
    match field {
        DateField::Year => t.year = n,
        DateField::Month => t.month = n.clamp(1, 12) as u8,
        DateField::Day => t.day = n.clamp(1, 31) as u8,
        DateField::Hour => t.hour = n.clamp(0, 23) as u8,
        DateField::Minute => t.minute = n.clamp(0, 59) as u8,
    }
    t.day = t.day.min(clock::days_in_month(t.year, t.month));
    ui.date_draft = Some(t);
}

/// Start from the clock as it reads now.
pub(crate) fn begin(ui: &mut Ui, m: &AppModel) {
    ui.date_draft = Some(current(ui, m));
}

/// The draft as an instant (unix seconds), or why the platform would refuse it.
pub(crate) fn instant(ui: &Ui, m: &AppModel) -> Result<i64, &'static str> {
    let utc = time_zone::rule(m).utc_of(&draft(ui, m));
    match m.platform.snapshot.info.build_epoch {
        Some(build) if utc < build - BEFORE_BUILD_S => Err("Choose a later date."),
        Some(build) if utc > build + AFTER_BUILD_S => Err("Choose an earlier date."),
        _ => Ok(utc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_shows_the_clock_and_a_picked_day_fits_its_month() {
        // 2026-10-12 03:31 UTC.
        let mut ui = Ui {
            clock_fixed: Some(1_791_775_896),
            ..Default::default()
        };
        let mut m = AppModel::default();
        m.settings.time_zone = "Europe/Berlin".into();
        assert_eq!(summary(&ui, &m), "12 Oct 2026, 05:31");
        m.settings.time_zone = "America/New_York".into();
        assert_eq!(summary(&ui, &m), "Oct 11, 2026, 11:31 PM");
        begin(&mut ui, &m);
        let rows = rows(&ui, &m);
        let values: Vec<_> = rows.iter().map(|r| r.secondary.as_str()).collect();
        assert_eq!(values, ["2026", "October", "11", "11:31 PM", ""]);
        assert_eq!(rows[4].key, "date_set");
        let (hours, at) = picker(&ui, &m, DateField::Hour);
        assert_eq!(
            (hours.len(), at, hours[0].label.as_str()),
            (24, 23, "12 AM")
        );
        let (days, at) = picker(&ui, &m, DateField::Day);
        assert_eq!((days.len(), days[at].key.as_str()), (31, "date_value:11"));
        choose(&mut ui, &m, DateField::Day, 31);
        choose(&mut ui, &m, DateField::Month, 2);
        assert_eq!(ui.date_draft.map(|t| t.day), Some(28));
        choose(&mut ui, &m, DateField::Year, 2028);
        choose(&mut ui, &m, DateField::Day, 29);
        assert_eq!(picker(&ui, &m, DateField::Day).0.len(), 29);
        // Without the build date: a year back, five ahead.
        let (years, at) = picker(&ui, &m, DateField::Year);
        assert_eq!(years.first().map(|r| r.label.as_str()), Some("2027"));
        assert_eq!(years[at].label, "2028");
        assert_eq!(field("minute"), Some(DateField::Minute));
    }

    #[test]
    fn the_instant_is_the_zones_and_stays_in_the_platforms_range() {
        let mut ui = Ui {
            clock_fixed: Some(1_791_775_896),
            ..Default::default()
        };
        let mut m = AppModel::default();
        m.settings.time_zone = "Europe/Berlin".into();
        m.platform.snapshot.info.build_epoch = Some(1_791_000_000);
        begin(&mut ui, &m);
        // The clock's own minute (seconds dropped).
        assert_eq!(instant(&ui, &m), Ok(1_791_775_860));
        choose(&mut ui, &m, DateField::Hour, 14);
        choose(&mut ui, &m, DateField::Minute, 5);
        // 14:05 in Berlin (summer time, UTC+2) is 12:05 UTC.
        assert_eq!(instant(&ui, &m), Ok(1_791_763_200 + 12 * 3600 + 5 * 60));
        let (years, _) = picker(&ui, &m, DateField::Year);
        assert_eq!(years.first().map(|r| r.label.as_str()), Some("2026"));
        assert_eq!(years.last().map(|r| r.label.as_str()), Some("2031"));
        choose(&mut ui, &m, DateField::Month, 1);
        assert_eq!(instant(&ui, &m), Err("Choose a later date."));
        choose(&mut ui, &m, DateField::Year, 2031);
        choose(&mut ui, &m, DateField::Month, 12);
        assert_eq!(instant(&ui, &m), Err("Choose an earlier date."));
        let offline = hint(&m);
        assert_eq!(rows(&ui, &m).len(), 5);
        m.platform.snapshot.info.clock_source = Some("ntp".into());
        assert_ne!(hint(&m), offline);
        // Network time: the clock only, nothing to set.
        let shown = rows(&ui, &m);
        assert_eq!(shown.len(), 1);
        assert!(!shown[0].key.starts_with("date_"));
    }
}
