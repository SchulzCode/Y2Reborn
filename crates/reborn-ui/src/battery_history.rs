//! Settings > System > Battery > History: the battery level over a day, and the battery used each
//! day. No rows and no buttons: the wheel moves a cursor over the bars (half hours, or days) and a
//! line above the chart says what is under it; Select zooms between the two views (a day opens its
//! 24 hours, a day's 24 hours go back to the days). The records come from
//! `AppModel::battery_history`.
use crate::{
    canvas::Canvas,
    pages::Page,
    text::Face,
    theme::{color, mix, WIDTH},
    time_zone, Effect, Ui,
};
use reborn_core::{
    battery_history::{self as history, Activity, Curve, Day, Window},
    clock::{self, Local, Rule},
    Action, AppModel, Screen,
};

/// Days shown at a time by the days view.
const VISIBLE_DAYS: usize = 10;
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Chart {
    /// One day, in half hours: the level and what the player did.
    #[default]
    Day,
    /// The days of the month, each with the battery it used.
    Days,
}

/// Where the cursor is: kept while the player runs, not saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ChartView {
    chart: Chart,
    /// The day under the cursor, counted back from today (0).
    day: usize,
    /// The newest day the days view shows, counted back from today.
    newest: usize,
    /// The half hour under the cursor in the day view; `None` is the latest with a record.
    slot: Option<usize>,
}

impl ChartView {
    fn zoom(&mut self) {
        match self.chart {
            Chart::Day => {
                self.chart = Chart::Days;
                self.keep_day_in_view();
            }
            Chart::Days => {
                self.chart = Chart::Day;
                self.slot = None;
            }
        }
    }

    /// Move the cursor of the days view by `step` days (positive: later).
    fn step_days(&mut self, step: i32) {
        let back = self.day as i32 - step;
        self.day = back.clamp(0, history::DAYS as i32 - 1) as usize;
        self.keep_day_in_view();
    }

    fn keep_day_in_view(&mut self) {
        if self.day < self.newest {
            self.newest = self.day;
        } else if self.day >= self.newest + VISIBLE_DAYS {
            self.newest = self.day + 1 - VISIBLE_DAYS;
        }
    }
}

impl Ui {
    /// The wheel and the pad move the cursor; Select zooms. Back and the rest are the page's.
    pub(crate) fn battery_history_input(
        &mut self,
        m: &mut AppModel,
        action: Action,
    ) -> Option<Effect> {
        if m.screen != Screen::BatteryHistory || m.navigation.modal.is_some() {
            return None;
        }
        let step = match action {
            Action::WheelClockwise(n) => i32::from(n.max(1)),
            Action::WheelCounterClockwise(n) => -i32::from(n.max(1)),
            Action::NavigateDown | Action::NavigateRight => 1,
            Action::NavigateUp | Action::NavigateLeft => -1,
            Action::Select => {
                self.battery_chart.zoom();
                return Some(Effect::None);
            }
            _ => return None,
        };
        let view = self.battery_chart;
        match view.chart {
            Chart::Days => self.battery_chart.step_days(step),
            Chart::Day => {
                let now = time_zone::now(self);
                let curve = day_curve(m, view, now, &time_zone::rule(m));
                let last = newest_slot(&curve);
                let at = view.slot.unwrap_or(last) as i32;
                self.battery_chart.slot = Some((at + step).clamp(0, last as i32) as usize);
            }
        }
        Some(Effect::None)
    }
}

/// The page has no rows: the chart is the page.
pub(crate) fn page() -> Page {
    Page::default()
}

pub(crate) fn draw(c: &mut Canvas, ui: &Ui, m: &AppModel) {
    let now = time_zone::now(ui);
    let rule = time_zone::rule(m);
    let view = ui.battery_chart;
    tabs(c, view.chart);
    if m.battery_history.latest().is_none() {
        c.text_center(WIDTH / 2., 170., "No history yet", Face::S15, color::TEXT);
        c.text_center(
            WIDTH / 2.,
            194.,
            "It fills in as the player is used.",
            Face::R13,
            color::MUTED,
        );
        return;
    }
    let twelve = clock::twelve_hour(&m.settings.time_zone);
    match view.chart {
        Chart::Day => day_view(c, m, view, now, &rule, twelve),
        Chart::Days => days_view(c, m, view, now, &rule),
    }
}

const LEFT: f32 = 44.;
const RIGHT: f32 = 14.;
const TOP: f32 = 124.;
const HEIGHT: f32 = 104.;
/// Width of a column of the day's area chart: about three and a half minutes.
const COLUMN: f32 = 1.;

/// Which view this is, as two labels: the current one lit. Not a control (the hint says what
/// Select does), so it never takes the highlight.
fn tabs(c: &mut Canvas, chart: Chart) {
    for (i, (label, this)) in [("24 hours", Chart::Day), ("Days", Chart::Days)]
        .into_iter()
        .enumerate()
    {
        let x = WIDTH / 2. - 106. + i as f32 * 108.;
        let on = chart == this;
        if on {
            c.rrect(x, 46., 104., 24., 12., color::GLASS_STRONG);
        }
        let tint = if on { color::TEXT } else { color::DIM };
        c.text_mid_center(x + 52., 58., label, Face::R13, tint);
    }
}

// ------------------------------------------------------------------ one day

/// The 24 hours shown: the last 24 hours for today, the calendar day for an earlier one.
fn day_window(view: ChartView, now: i64, rule: &Rule) -> Window {
    if view.day == 0 {
        return Window::last_day(now);
    }
    let offset = |t: i64| i64::from(rule.offset_at(t));
    let starts = history::day_starts(now, history::DAYS, &offset);
    Window {
        start: starts[history::DAYS - 1 - view.day.min(history::DAYS - 1)],
        step: history::SLOT_S,
        slots: history::SLOTS,
    }
}

fn day_curve(m: &AppModel, view: ChartView, now: i64, rule: &Rule) -> Curve {
    let spans = history::spans(m.battery_history.records(), now);
    history::curve(&spans, day_window(view, now, rule))
}

/// The last half hour with a level: where the cursor starts and the latest it can go.
fn newest_slot(curve: &Curve) -> usize {
    curve
        .levels
        .iter()
        .rposition(Option::is_some)
        .unwrap_or(curve.window.slots - 1)
}

fn day_view(c: &mut Canvas, m: &AppModel, view: ChartView, now: i64, rule: &Rule, twelve: bool) {
    let spans = history::spans(m.battery_history.records(), now);
    let window = day_window(view, now, rule);
    let curve = history::curve(&spans, window);
    let cursor = view
        .slot
        .unwrap_or_else(|| newest_slot(&curve))
        .min(window.slots - 1);
    let t = window.start + cursor as i64 * window.step;
    // What is under the cursor: the time and level, and the mix of the half hour.
    let clock = clock::clock_text(&rule.local(t), twelve);
    let when = format!("{} {clock}", day_name(t, now, rule));
    c.text_mid(LEFT, 90., 250., &when, Face::S15, color::TEXT);
    match curve.levels[cursor] {
        Some(pct) => {
            let text = format!("{pct}%");
            c.text_mid_right(WIDTH - RIGHT, 90., &text, Face::S15, color::TEXT);
        }
        None => c.text_mid_right(WIDTH - RIGHT, 90., "No record", Face::R13, color::MUTED),
    }
    let mix = curve.mix(cursor);
    if let Some((first, _)) = mix.first() {
        c.rrect(LEFT, 106., 8., 8., 2., activity_color(*first));
        let parts: Vec<String> = mix
            .iter()
            .map(|(a, seconds)| format!("{} {}", a.label(), duration(*seconds)))
            .collect();
        let line = parts.join(" · ");
        c.text_mid(
            LEFT + 14.,
            110.,
            WIDTH - LEFT - RIGHT - 14.,
            &line,
            Face::R13,
            color::MUTED,
        );
    }
    grid(c, 100.);
    let slot = (WIDTH - LEFT - RIGHT) / window.slots as f32;
    cursor_column(c, LEFT + cursor as f32 * slot, slot, 16.);
    area(c, &spans, window, cursor);
    hour_axis(c, &curve, rule, twelve);
    legend(c);
    footer(c, m, now, rule, twelve, "Turn to browse · Select for days");
}

/// The level as a filled area, a column every two pixels (about seven minutes), each coloured by
/// what the player was doing then: the finest the records go, not a bar per half hour.
fn area(c: &mut Canvas, spans: &[history::Span], window: Window, cursor: usize) {
    let width = WIDTH - LEFT - RIGHT;
    let columns = (width / COLUMN) as usize;
    let total = window.step * window.slots as i64;
    let slot = width / window.slots as f32;
    for column in 0..columns {
        let t = window.start + total * (2 * column as i64 + 1) / (2 * columns as i64);
        let Some(span) = history::span_at(spans, t) else {
            continue;
        };
        let x = LEFT + column as f32 * COLUMN;
        let h = (HEIGHT * f32::from(span.pct) / 100.).max(2.);
        let mut tint = activity_color(span.activity);
        if (x - LEFT + COLUMN / 2.) / slot < cursor as f32 + 1.
            && (x - LEFT + COLUMN / 2.) / slot >= cursor as f32
        {
            // The half hour under the cursor is lighter, so it stands out of its column.
            tint = mix(tint, color::TEXT, 0.45);
        }
        c.rect(x, TOP + HEIGHT - h, COLUMN, h, tint);
        if span.pct <= 20 && span.activity != Activity::Charging {
            c.rect(x, TOP + HEIGHT - h, COLUMN, 3., color::DANGER);
        }
    }
}

/// The highlight of the bar under the cursor: the screen's one focus target.
fn cursor_column(c: &mut Canvas, x: f32, width: f32, below: f32) {
    let start = c.draw.len();
    c.rrect(
        x - 1.,
        TOP - 6.,
        width + 2.,
        HEIGHT + below,
        3.,
        color::GLASS_STRONG,
    );
    c.mark_focus(start);
}

fn grid(c: &mut Canvas, scale: f32) {
    let mut pct = 0.;
    while pct <= scale {
        let y = TOP + HEIGHT - HEIGHT * pct / scale;
        c.rect(LEFT, y, WIDTH - LEFT - RIGHT, 1., color::RULE);
        c.text_mid_right(LEFT - 6., y, &format!("{pct:.0}%"), Face::R11, color::DIM);
        pct += scale / 2.;
    }
}

/// Labels under the bands, every third hour.
fn hour_axis(c: &mut Canvas, curve: &Curve, rule: &Rule, twelve: bool) {
    let window = curve.window;
    let slot = (WIDTH - LEFT - RIGHT) / window.slots as f32;
    for i in 0..window.slots {
        let local = rule.local(window.start + i as i64 * window.step);
        if local.minute == 0 && local.hour.is_multiple_of(3) {
            let text = hour_label(local.hour, twelve);
            c.text_center(LEFT + i as f32 * slot, 248., &text, Face::R11, color::MUTED);
        }
    }
}

fn hour_label(hour: u8, twelve: bool) -> String {
    match (twelve, hour) {
        (false, h) => format!("{h:02}"),
        (true, 0) => "12 AM".into(),
        (true, 12) => "12 PM".into(),
        (true, h) if h < 12 => format!("{h} AM"),
        (true, h) => format!("{} PM", h - 12),
    }
}

fn legend(c: &mut Canvas) {
    let mut x = LEFT;
    for activity in [
        Activity::Playing,
        Activity::ScreenOn,
        Activity::Awake,
        Activity::Suspended,
        Activity::Charging,
    ] {
        c.rrect(x, 272., 8., 8., 2., activity_color(activity));
        let w = c.text(x + 13., 280., activity.label(), Face::R11, color::MUTED);
        x += 13. + w + 16.;
    }
}

fn activity_color(activity: Activity) -> u32 {
    match activity {
        Activity::Charging => color::SUCCESS,
        Activity::Playing => color::ACCENT,
        Activity::ScreenOn => color::WARNING,
        Activity::Awake => color::DIM,
        Activity::Suspended => mix(color::DIM, color::BG, 0.7),
    }
}

/// The last charge and the caveat about the percent, then what the buttons do.
fn footer(c: &mut Canvas, m: &AppModel, now: i64, rule: &Rule, twelve: bool, hint: &str) {
    let ledger = &m.battery_history;
    let note = if ledger.charging_now() {
        "Charging".to_owned()
    } else {
        ledger.last_charge().map_or_else(String::new, |charge| {
            format!(
                "Last charged to {}%, {}",
                charge.pct,
                when(charge.t, now, rule, twelve)
            )
        })
    };
    let estimate = ledger.records().iter().any(is_estimate);
    let line = match (note.is_empty(), estimate) {
        (false, true) => format!("{note} · Percent is estimated"),
        (false, false) => note,
        (true, true) => "Percent is estimated from the battery voltage".to_owned(),
        (true, false) => String::new(),
    };
    c.text_center(WIDTH / 2., 318., &line, Face::R11, color::MUTED);
    c.text_center(WIDTH / 2., 342., hint, Face::R11, color::DIM);
}

fn is_estimate(record: &history::Record) -> bool {
    matches!(record, history::Record::Level { est: true, .. })
}

// ------------------------------------------------------------------ the days

fn days_view(c: &mut Canvas, m: &AppModel, view: ChartView, now: i64, rule: &Rule) {
    let offset = |t: i64| i64::from(rule.offset_at(t));
    let days = history::daily(m.battery_history.records(), now, history::DAYS, &offset);
    let end = days.len() - view.newest.min(days.len() - 1);
    let shown = &days[end.saturating_sub(VISIBLE_DAYS)..end];
    let chosen = days
        .len()
        .checked_sub(view.day + 1)
        .and_then(|i| days.get(i));
    // A day that used more than all of it (charged in between) widens the scale.
    let top = shown.iter().map(|d| d.used).fold(100f32, f32::max);
    let scale = (top / 100.).ceil() * 100.;
    if let Some(day) = chosen {
        detail(c, day, rule);
    }
    grid(c, scale);
    let slot = (WIDTH - LEFT - RIGHT) / VISIBLE_DAYS as f32;
    let gap = VISIBLE_DAYS - shown.len();
    for (i, day) in shown.iter().enumerate() {
        let x = LEFT + (i + gap) as f32 * slot;
        let picked = chosen.is_some_and(|d| d.start == day.start);
        if picked {
            cursor_column(c, x, slot, 28.);
        }
        if day.known() {
            let h = (HEIGHT * day.used / scale).max(2.);
            let tint = if picked {
                color::ACCENT
            } else {
                mix(color::ACCENT, color::BG, 0.5)
            };
            c.rect(x + 6., TOP + HEIGHT - h, slot - 12., h, tint);
            let used = format!("{}", day.used.round() as i32);
            c.text_center(
                x + slot / 2.,
                TOP + HEIGHT - h - 6.,
                &used,
                Face::R11,
                color::MUTED,
            );
        }
        let local = rule.local(day.start + 12 * 3600);
        let face = if picked { color::TEXT } else { color::MUTED };
        let cx = x + slot / 2.;
        c.text_center(
            cx,
            TOP + HEIGHT + 20.,
            &weekday(&local)[..1],
            Face::R11,
            face,
        );
        c.text_center(
            cx,
            TOP + HEIGHT + 36.,
            &local.day.to_string(),
            Face::R11,
            face,
        );
    }
    let twelve = clock::twelve_hour(&m.settings.time_zone);
    let hint = "Turn to browse · Select for this day's 24 hours";
    footer(c, m, now, rule, twelve, hint);
}

/// What is under the cursor of the days view.
fn detail(c: &mut Canvas, day: &Day, rule: &Rule) {
    let local = rule.local(day.start + 12 * 3600);
    let used = if day.known() {
        format!("Used {}%", day.used.round() as i32)
    } else {
        "No record".to_owned()
    };
    let name = format!("{} {} {}", weekday(&local), local.day, month(&local));
    c.text_mid(LEFT, 90., 250., &name, Face::S15, color::TEXT);
    c.text_mid_right(WIDTH - RIGHT, 90., &used, Face::S15, color::TEXT);
    let times: Vec<String> = [
        Activity::Playing,
        Activity::ScreenOn,
        Activity::Awake,
        Activity::Suspended,
        Activity::Charging,
    ]
    .into_iter()
    .filter(|a| day.seconds_in(*a) >= 60)
    .map(|a| format!("{} {}", a.label(), duration(day.seconds_in(a))))
    .collect();
    let line = times.join(" · ");
    c.text_mid(
        LEFT,
        110.,
        WIDTH - LEFT - RIGHT,
        &line,
        Face::R11,
        color::MUTED,
    );
}

// ------------------------------------------------------------------ words

fn duration(seconds: i64) -> String {
    let minutes = (seconds + 30) / 60;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} m"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} m"),
    }
}

/// 0 is Sunday: 1970-01-01 was a Thursday.
fn weekday(t: &Local) -> &'static str {
    let days = days_from_civil(t.year, t.month, t.day);
    WEEKDAYS[(days + 4).rem_euclid(7) as usize]
}

fn month(t: &Local) -> &'static str {
    MONTHS[usize::from(t.month.clamp(1, 12)) - 1]
}

/// Days since 1970-01-01 of a calendar date.
fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "Today", "Yesterday" or "Mon 23 Nov".
fn day_name(t: i64, now: i64, rule: &Rule) -> String {
    let (then, today) = (rule.local(t), rule.local(now));
    let ago = days_from_civil(today.year, today.month, today.day)
        - days_from_civil(then.year, then.month, then.day);
    match ago {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => format!("{} {} {}", weekday(&then), then.day, month(&then)),
    }
}

/// "today 19:50", "yesterday 22:40" or "Mon 23 Nov 07:15".
fn when(t: i64, now: i64, rule: &Rule, twelve: bool) -> String {
    let name = day_name(t, now, rule);
    let time = clock::clock_text(&rule.local(t), twelve);
    match name.as_str() {
        "Today" => format!("today {time}"),
        "Yesterday" => format!("yesterday {time}"),
        _ => format!("{name} {time}"),
    }
}

#[cfg(test)]
#[path = "battery_history_tests.rs"]
mod tests;
