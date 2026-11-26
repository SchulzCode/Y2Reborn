//! Battery history: a small ledger of the battery level and what the player was doing, and the
//! two charts made from it (the last 24 hours, and the battery used per day). Pure: the file is
//! `reborn-platform`'s, and the clock and the time zone come in as arguments.
//!
//! A record is made only when something changed (the percent, the activity, the output) or the
//! last one is 15 minutes old, so the ledger needs no timer of its own: it rides on the battery
//! observation that happens anyway. Time asleep needs no sampling: the `Suspended` record before
//! the sleep and the first record after it bound it.
use serde::{Deserialize, Serialize};

/// How long records are kept.
pub const KEEP_S: i64 = 30 * 24 * 3600;
/// While awake, a record at least this often: after a crash, the last state lasts this long.
pub const HEARTBEAT_S: i64 = 15 * 60;
/// One bar of the 24-hour chart. Quarter hours were too thin on a 480 px wide screen.
pub const SLOT_S: i64 = 30 * 60;
/// Bars of the curve.
pub const SLOTS: usize = 48;
/// Days of the per-day chart.
pub const DAYS: usize = 30;

/// What the player was doing, the most expensive state that applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    /// A charger or a computer is connected.
    Charging,
    /// Sound plays with the screen off.
    Playing,
    /// The screen is on (with or without sound).
    ScreenOn,
    /// Awake with the screen off and no sound (a pause held for a headset button).
    Awake,
    /// Asleep (the kernel suspended).
    Suspended,
}

impl Activity {
    pub const ALL: [Self; 5] = [
        Self::Charging,
        Self::Playing,
        Self::ScreenOn,
        Self::Awake,
        Self::Suspended,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Charging => "Charging",
            Self::Playing => "Playing",
            Self::ScreenOn => "Screen on",
            Self::Awake => "Awake",
            Self::Suspended => "Asleep",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Charging => 0,
            Self::Playing => 1,
            Self::ScreenOn => 2,
            Self::Awake => 3,
            Self::Suspended => 4,
        }
    }
}

/// The facts the activity is made from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Situation {
    pub cable: bool,
    pub asleep: bool,
    pub screen_on: bool,
    pub playing: bool,
}

impl Situation {
    pub fn activity(self) -> Activity {
        if self.cable {
            Activity::Charging
        } else if self.asleep {
            Activity::Suspended
        } else if self.screen_on {
            Activity::ScreenOn
        } else if self.playing {
            Activity::Playing
        } else {
            Activity::Awake
        }
    }
}

/// One line of the history file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Record {
    /// From `t` (Unix seconds) the battery shows `pct` and the player is `activity`.
    Level {
        t: i64,
        pct: u8,
        activity: Activity,
        /// The sound goes to Bluetooth headphones.
        #[serde(default)]
        bt: bool,
        /// The percent is an estimate from the voltage.
        #[serde(default)]
        est: bool,
    },
    /// The player started at `t` (`id`: the kernel's boot id, so a restart of the app is no boot).
    Boot { t: i64, id: String },
    /// The player was shut down at `t`.
    Down { t: i64 },
}

impl Record {
    pub fn time(&self) -> i64 {
        match self {
            Self::Level { t, .. } | Self::Boot { t, .. } | Self::Down { t } => *t,
        }
    }
}

/// What the battery observation says now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    pub pct: Option<u8>,
    pub activity: Activity,
    pub bt: bool,
    pub estimated: bool,
}

/// The records in time order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    records: Vec<Record>,
}

impl Ledger {
    /// The records of a file: out of order ones (a clock that stepped back) are dropped.
    pub fn from_records(records: Vec<Record>) -> Self {
        let mut ledger = Self::default();
        for record in records {
            if record.time() >= ledger.last_time() {
                ledger.records.push(record);
            }
        }
        ledger
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    fn last_time(&self) -> i64 {
        self.records.last().map_or(i64::MIN, Record::time)
    }

    /// The record to write for this observation, if it changes anything. No percent (the
    /// platform has none yet) and a clock that is before the last record make no record.
    pub fn observe(&mut self, now: i64, seen: Observation) -> Option<Record> {
        let pct = seen.pct?.min(100);
        if now < self.last_time() {
            return None;
        }
        let due = match self.records.last() {
            Some(Record::Level {
                t,
                pct: last,
                activity,
                bt,
                ..
            }) => {
                *last != pct
                    || *activity != seen.activity
                    || *bt != seen.bt
                    || now - *t >= HEARTBEAT_S
            }
            _ => true,
        };
        due.then(|| {
            let record = Record::Level {
                t: now,
                pct,
                activity: seen.activity,
                bt: seen.bt,
                est: seen.estimated,
            };
            self.records.push(record.clone());
            record
        })
    }

    /// A boot record when `id` is not the boot of the last one.
    pub fn boot(&mut self, now: i64, id: &str) -> Option<Record> {
        if now < self.last_time() {
            return None;
        }
        let known = self.records.iter().rev().find_map(|r| match r {
            Record::Boot { id: last, .. } => Some(last == id),
            _ => None,
        });
        (known != Some(true)).then(|| {
            let record = Record::Boot {
                t: now,
                id: id.into(),
            };
            self.records.push(record.clone());
            record
        })
    }

    /// The shutdown record (none when the last record is one already).
    pub fn down(&mut self, now: i64) -> Option<Record> {
        if now < self.last_time() || matches!(self.records.last(), Some(Record::Down { .. })) {
            return None;
        }
        let record = Record::Down { t: now };
        self.records.push(record.clone());
        Some(record)
    }

    /// Drop what is older than [`KEEP_S`], but keep the last level before the cut: it is the
    /// state at the start of the period. Returns whether anything was dropped.
    pub fn prune(&mut self, now: i64) -> bool {
        let cut = now - KEEP_S;
        let first = self
            .records
            .iter()
            .position(|r| r.time() >= cut)
            .unwrap_or(self.records.len());
        let keep_from = match self.records[..first].last() {
            Some(Record::Level { .. }) => first - 1,
            _ => first,
        };
        self.records.drain(..keep_from);
        keep_from > 0
    }

    /// Keep the newest `max` records. Returns whether anything was dropped.
    pub fn cap(&mut self, max: usize) -> bool {
        let extra = self.records.len().saturating_sub(max);
        self.records.drain(..extra);
        extra > 0
    }

    /// The last charge: the level at the last unplug and when it was, or `None` before one.
    pub fn last_charge(&self) -> Option<LastCharge> {
        let mut found = None;
        let mut charging: Option<u8> = None;
        for record in &self.records {
            if let Record::Level {
                t, pct, activity, ..
            } = record
            {
                match (activity, charging) {
                    (Activity::Charging, _) => charging = Some(*pct),
                    (_, Some(level)) => {
                        found = Some(LastCharge {
                            pct: level.max(*pct),
                            t: *t,
                        });
                        charging = None;
                    }
                    _ => {}
                }
            }
        }
        found
    }

    /// A charger is connected now.
    pub fn charging_now(&self) -> bool {
        matches!(
            self.records.last(),
            Some(Record::Level {
                activity: Activity::Charging,
                ..
            })
        )
    }

    /// The last level and when it was seen.
    pub fn latest(&self) -> Option<(i64, u8)> {
        self.records.iter().rev().find_map(|r| match r {
            Record::Level { t, pct, .. } => Some((*t, *pct)),
            _ => None,
        })
    }
}

/// The battery was at `pct` when the charger was disconnected at `t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastCharge {
    pub pct: u8,
    pub t: i64,
}

/// A time when the player was on, in one state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: i64,
    pub end: i64,
    pub pct: u8,
    pub activity: Activity,
}

/// The times the player was on. A state lasts until the next record; a shutdown ends it; a boot
/// with no shutdown before it (a crash, a flat battery) ends it after [`HEARTBEAT_S`] at most.
pub fn spans(records: &[Record], now: i64) -> Vec<Span> {
    let mut out = Vec::new();
    for (i, record) in records.iter().enumerate() {
        let Record::Level {
            t, pct, activity, ..
        } = record
        else {
            continue;
        };
        let end = match records.get(i + 1) {
            None => now,
            Some(Record::Boot { t: boot, .. }) => (*boot).min(t + HEARTBEAT_S),
            Some(next) => next.time(),
        }
        .min(now);
        if end > *t {
            out.push(Span {
                start: *t,
                end,
                pct: *pct,
                activity: *activity,
            });
        }
    }
    out
}

/// Bars from `start`, `step` seconds each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: i64,
    pub step: i64,
    pub slots: usize,
}

impl Window {
    /// The 24 hours up to the end of the half hour that `now` is in.
    pub fn last_day(now: i64) -> Self {
        let end = (now.div_euclid(SLOT_S) + 1) * SLOT_S;
        Self {
            start: end - SLOTS as i64 * SLOT_S,
            step: SLOT_S,
            slots: SLOTS,
        }
    }

    pub fn end(&self) -> i64 {
        self.start + self.step * self.slots as i64
    }
}

/// The span that holds the instant `t`, if the player was on then. `spans` is in time order.
pub fn span_at(spans: &[Span], t: i64) -> Option<&Span> {
    let after = spans.partition_point(|span| span.start <= t);
    after
        .checked_sub(1)
        .map(|i| &spans[i])
        .filter(|span| span.end > t)
}

/// The battery level and the activity in each bar of a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Curve {
    pub window: Window,
    /// The last level in the bar; `None` where the player was off or there is no record.
    pub levels: Vec<Option<u8>>,
    /// What the player did most of the bar's time; `None` where it was off.
    pub activity: Vec<Option<Activity>>,
    /// Seconds in each activity in the bar, by [`Activity::ALL`].
    pub seconds: Vec<[i64; 5]>,
}

impl Curve {
    /// The activities of bar `slot`, the longest first, with their seconds (a minute or more).
    pub fn mix(&self, slot: usize) -> Vec<(Activity, i64)> {
        let mut mix: Vec<(Activity, i64)> = Activity::ALL
            .into_iter()
            .map(|a| (a, self.seconds[slot][a.index()]))
            .filter(|(_, seconds)| *seconds >= 60)
            .collect();
        mix.sort_by_key(|(_, seconds)| std::cmp::Reverse(*seconds));
        mix
    }
}

pub fn curve(spans: &[Span], window: Window) -> Curve {
    let mut levels = vec![None; window.slots];
    let mut times = vec![[0i64; 5]; window.slots];
    let mut start = window.start;
    for slot in 0..window.slots {
        let end = start + window.step;
        for span in spans {
            let overlap = span.end.min(end) - span.start.max(start);
            if overlap > 0 {
                levels[slot] = Some(span.pct);
                times[slot][span.activity.index()] += overlap;
            }
        }
        start = end;
    }
    let activity = times
        .iter()
        .map(|slot| {
            Activity::ALL
                .into_iter()
                .filter(|a| slot[a.index()] > 0)
                .max_by_key(|a| slot[a.index()])
        })
        .collect();
    Curve {
        window,
        levels,
        activity,
        seconds: times,
    }
}

/// One day of the per-day chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Day {
    /// Local midnight (Unix seconds).
    pub start: i64,
    /// Percent used while not charging (a fraction: a night's drop is shared by the days).
    pub used: f32,
    /// Seconds in each activity, by [`Activity::ALL`].
    pub seconds: [i64; 5],
}

impl Day {
    pub fn seconds_in(&self, activity: Activity) -> i64 {
        self.seconds[activity.index()]
    }

    /// The player was on at some time of the day.
    pub fn known(&self) -> bool {
        self.seconds.iter().any(|s| *s > 0)
    }
}

/// Local midnight at or before `t`; `offset(t)` is the zone's offset from UTC at `t` in seconds.
pub fn day_start(t: i64, offset: &impl Fn(i64) -> i64) -> i64 {
    let local = t + offset(t);
    local - local.rem_euclid(86_400) - offset(t)
}

/// The local midnights of the last `days` days, the oldest first; the last one is today's.
pub fn day_starts(now: i64, days: usize, offset: &impl Fn(i64) -> i64) -> Vec<i64> {
    let mut starts = vec![day_start(now, offset)];
    for _ in 1..days {
        let first = starts[0];
        starts.insert(0, day_start(first - 1, offset));
    }
    starts
}

/// The last `days` days, the last one is today.
pub fn daily(records: &[Record], now: i64, days: usize, offset: &impl Fn(i64) -> i64) -> Vec<Day> {
    let starts = day_starts(now, days, offset);
    // From each midnight to the next; 36 h after a midnight is always in the next day.
    let ends: Vec<i64> = starts
        .iter()
        .map(|s| day_start(s + 36 * 3600, offset))
        .collect();
    let mut out: Vec<Day> = starts
        .iter()
        .map(|&start| Day {
            start,
            used: 0.,
            seconds: [0; 5],
        })
        .collect();
    for span in spans(records, now) {
        for (day, end) in out.iter_mut().zip(&ends) {
            let overlap = span.end.min(*end) - span.start.max(day.start);
            if overlap > 0 {
                day.seconds[span.activity.index()] += overlap;
            }
        }
    }
    let mut previous: Option<(i64, u8)> = None;
    for record in records {
        let Record::Level {
            t, pct, activity, ..
        } = record
        else {
            // A shutdown or a boot: a drop while the player was off is not use.
            previous = None;
            continue;
        };
        if let Some((from, level)) = previous.filter(|(_, level)| level > pct) {
            if *activity != Activity::Charging {
                share_drop(&mut out, &ends, (from, *t), f32::from(level - pct));
            }
        }
        previous = Some((*t, *pct));
    }
    out
}

/// Share a drop of `drop` percent over the days of `(from, to)` by the time in each.
fn share_drop(days: &mut [Day], ends: &[i64], (from, to): (i64, i64), drop: f32) {
    let length = (to - from).max(1) as f32;
    for (day, end) in days.iter_mut().zip(ends) {
        let overlap = if to <= from {
            i64::from(from >= day.start && from < *end)
        } else {
            to.min(*end) - from.max(day.start)
        };
        if overlap > 0 {
            day.used += drop * overlap as f32 / if to <= from { 1. } else { length };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;
    const T0: i64 = 1_800_000_000 - 1_800_000_000 % DAY;

    fn seen(pct: u8, activity: Activity) -> Observation {
        Observation {
            pct: Some(pct),
            activity,
            bt: false,
            estimated: true,
        }
    }

    fn level(t: i64, pct: u8, activity: Activity) -> Record {
        Record::Level {
            t,
            pct,
            activity,
            bt: false,
            est: true,
        }
    }

    fn utc(_: i64) -> i64 {
        0
    }

    #[test]
    fn activity_is_the_most_expensive_state_that_applies() {
        let mut s = Situation::default();
        assert_eq!(s.activity(), Activity::Awake);
        s.playing = true;
        assert_eq!(s.activity(), Activity::Playing);
        s.screen_on = true;
        assert_eq!(s.activity(), Activity::ScreenOn);
        s.asleep = true;
        assert_eq!(s.activity(), Activity::Suspended);
        s.cable = true;
        assert_eq!(s.activity(), Activity::Charging);
    }

    #[test]
    fn records_only_a_change_or_a_heartbeat() {
        let mut ledger = Ledger::default();
        assert!(ledger.observe(T0, seen(80, Activity::Playing)).is_some());
        assert!(ledger
            .observe(T0 + 2, seen(80, Activity::Playing))
            .is_none());
        assert!(ledger
            .observe(T0 + 60, seen(79, Activity::Playing))
            .is_some());
        assert!(ledger
            .observe(T0 + 70, seen(79, Activity::ScreenOn))
            .is_some());
        let at = T0 + 70 + HEARTBEAT_S;
        assert!(ledger
            .observe(at - 1, seen(79, Activity::ScreenOn))
            .is_none());
        assert!(ledger.observe(at, seen(79, Activity::ScreenOn)).is_some());
        assert_eq!(ledger.records().len(), 4);
    }

    #[test]
    fn no_percent_and_a_clock_that_stepped_back_make_no_record() {
        let mut ledger = Ledger::default();
        let none = Observation {
            pct: None,
            ..seen(0, Activity::Awake)
        };
        assert!(ledger.observe(T0, none).is_none());
        assert!(ledger.observe(T0, seen(50, Activity::Awake)).is_some());
        assert!(ledger.observe(T0 - 5, seen(49, Activity::Awake)).is_none());
        assert!(ledger.down(T0 - 5).is_none());
        let ordered = Ledger::from_records(vec![
            level(T0 + 10, 50, Activity::Awake),
            level(T0, 49, Activity::Awake),
            level(T0 + 20, 48, Activity::Awake),
        ]);
        assert_eq!(ordered.records().len(), 2);
    }

    #[test]
    fn a_restart_of_the_app_is_not_a_boot() {
        let mut ledger = Ledger::default();
        assert!(ledger.boot(T0, "a").is_some());
        assert!(ledger.boot(T0 + 5, "a").is_none());
        assert!(ledger.boot(T0 + 9, "b").is_some());
        assert!(ledger.down(T0 + 10).is_some());
        assert!(ledger.down(T0 + 11).is_none());
    }

    #[test]
    fn a_state_ends_at_a_shutdown_and_after_a_crash_at_the_heartbeat() {
        let records = vec![
            level(T0, 90, Activity::ScreenOn),
            Record::Down { t: T0 + 600 },
            Record::Boot {
                t: T0 + 7200,
                id: "b".into(),
            },
            level(T0 + 7210, 88, Activity::Awake),
            Record::Boot {
                t: T0 + 20_000,
                id: "c".into(),
            },
        ];
        let all = spans(&records, T0 + 30_000);
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].start, all[0].end), (T0, T0 + 600));
        assert_eq!(
            (all[1].start, all[1].end),
            (T0 + 7210, T0 + 7210 + HEARTBEAT_S)
        );
    }

    #[test]
    fn suspend_is_a_span_between_its_two_records() {
        let records = vec![
            level(T0, 70, Activity::Suspended),
            level(T0 + 6 * 3600, 67, Activity::ScreenOn),
        ];
        let all = spans(&records, T0 + 6 * 3600 + 60);
        assert_eq!(all[0].activity, Activity::Suspended);
        assert_eq!(all[0].end - all[0].start, 6 * 3600);
        assert_eq!(all[1].end, T0 + 6 * 3600 + 60);
    }

    #[test]
    fn prune_keeps_the_state_at_the_start() {
        let mut ledger = Ledger::from_records(vec![
            level(T0, 90, Activity::Awake),
            level(T0 + DAY, 80, Activity::Awake),
            level(T0 + 31 * DAY, 20, Activity::Awake),
        ]);
        assert!(ledger.prune(T0 + 32 * DAY));
        assert_eq!(ledger.records().len(), 2);
        assert_eq!(ledger.records()[0].time(), T0 + DAY);
        assert!(!ledger.prune(T0 + 32 * DAY));
    }

    #[test]
    fn cap_keeps_the_newest() {
        let mut ledger = Ledger::from_records(
            (0..10)
                .map(|n| level(T0 + n, 50, Activity::Awake))
                .collect(),
        );
        assert!(ledger.cap(4));
        assert_eq!(ledger.records()[0].time(), T0 + 6);
        assert!(!ledger.cap(4));
    }

    #[test]
    fn last_charge_is_the_level_at_the_unplug() {
        let mut ledger = Ledger::from_records(vec![
            level(T0, 40, Activity::Charging),
            level(T0 + 3600, 100, Activity::Charging),
        ]);
        assert!(ledger.charging_now());
        assert_eq!(ledger.last_charge(), None);
        ledger.observe(T0 + 7200, seen(98, Activity::ScreenOn));
        assert!(!ledger.charging_now());
        assert_eq!(
            ledger.last_charge(),
            Some(LastCharge {
                pct: 100,
                t: T0 + 7200
            })
        );
        assert_eq!(ledger.latest(), Some((T0 + 7200, 98)));
    }

    #[test]
    fn the_day_window_ends_at_the_next_half_hour() {
        let w = Window::last_day(T0 + 3 * 3600 + 10 * 60);
        assert_eq!(w.end(), T0 + 3 * 3600 + 1800);
        assert_eq!(w.end() - w.start, DAY);
        assert_eq!(w.slots, 48);
    }

    #[test]
    fn curve_has_the_last_level_and_the_longest_activity_of_each_bar() {
        let records = vec![
            level(T0, 80, Activity::Playing),
            level(T0 + 300, 79, Activity::ScreenOn),
            level(T0 + 1500, 78, Activity::Playing),
            Record::Down {
                t: T0 + 1800 + 1800,
            },
        ];
        let window = Window {
            start: T0,
            step: SLOT_S,
            slots: 3,
        };
        let c = curve(&spans(&records, T0 + 3 * SLOT_S), window);
        assert_eq!(c.levels, vec![Some(78), Some(78), None]);
        assert_eq!(
            c.activity,
            // Bar 0: 300 s playing, 1200 s screen on, 300 s playing.
            vec![Some(Activity::ScreenOn), Some(Activity::Playing), None]
        );
    }

    #[test]
    fn the_instant_finds_its_span_and_a_gap_finds_none() {
        let records = vec![
            level(T0, 80, Activity::Playing),
            level(T0 + 600, 79, Activity::ScreenOn),
            Record::Down { t: T0 + 900 },
            level(T0 + 2000, 70, Activity::Awake),
        ];
        let all = spans(&records, T0 + 3000);
        assert_eq!(span_at(&all, T0 - 1), None);
        assert_eq!(span_at(&all, T0).map(|s| s.pct), Some(80));
        assert_eq!(span_at(&all, T0 + 599).map(|s| s.pct), Some(80));
        assert_eq!(span_at(&all, T0 + 600).map(|s| s.pct), Some(79));
        assert_eq!(span_at(&all, T0 + 900), None, "off after the shutdown");
        assert_eq!(span_at(&all, T0 + 2500).map(|s| s.pct), Some(70));
        assert_eq!(span_at(&all, T0 + 3000), None);
    }

    #[test]
    fn a_bar_keeps_the_mix_of_its_half_hour() {
        let records = vec![
            level(T0, 80, Activity::ScreenOn),
            level(T0 + 660, 79, Activity::Playing),
            level(T0 + 1780, 78, Activity::Awake),
        ];
        let window = Window {
            start: T0,
            step: SLOT_S,
            slots: 1,
        };
        let c = curve(&spans(&records, T0 + 2400), window);
        // 11 min screen, 18 min 40 s music, 20 s awake (under a minute: left out).
        assert_eq!(
            c.mix(0),
            vec![(Activity::Playing, 1120), (Activity::ScreenOn, 660)]
        );
    }

    #[test]
    fn a_day_adds_the_drops_and_not_the_charging() {
        let records = vec![
            level(T0 + 3600, 100, Activity::Charging),
            level(T0 + 7200, 96, Activity::Playing),
            level(T0 + 9000, 93, Activity::Playing),
            level(T0 + 9100, 94, Activity::Playing),
            level(T0 + 10_000, 50, Activity::Charging),
            level(T0 + 12_000, 90, Activity::Charging),
        ];
        let days = daily(&records, T0 + 13_000, 3, &utc);
        assert_eq!(days.len(), 3);
        assert_eq!(days[2].start, T0);
        assert!((days[2].used - 7.).abs() < 0.001, "{}", days[2].used);
        assert_eq!(days[2].seconds_in(Activity::Playing), 2800);
        assert!(!days[1].known());
    }

    #[test]
    fn a_night_asleep_is_shared_by_the_two_days() {
        let records = vec![
            level(T0 - 2 * 3600, 80, Activity::Suspended),
            level(T0 + 6 * 3600, 72, Activity::ScreenOn),
        ];
        let days = daily(&records, T0 + 7 * 3600, 2, &utc);
        assert!((days[0].used - 2.).abs() < 0.001, "{}", days[0].used);
        assert!((days[1].used - 6.).abs() < 0.001, "{}", days[1].used);
        assert_eq!(days[0].seconds_in(Activity::Suspended), 2 * 3600);
        assert_eq!(days[1].seconds_in(Activity::Suspended), 6 * 3600);
    }

    #[test]
    fn the_time_off_is_not_use() {
        let records = vec![
            level(T0, 80, Activity::Awake),
            Record::Down { t: T0 + 60 },
            Record::Boot {
                t: T0 + 3600,
                id: "b".into(),
            },
            level(T0 + 3700, 70, Activity::Awake),
        ];
        let days = daily(&records, T0 + 4000, 1, &utc);
        assert_eq!(days[0].used, 0.);
    }

    #[test]
    fn days_start_at_local_midnight() {
        let zone = |_: i64| 2 * 3600;
        assert_eq!(day_start(T0 + 3600, &zone), T0 - 2 * 3600);
        assert_eq!(day_start(T0 + 23 * 3600, &zone), T0 + 22 * 3600);
        let days = daily(&[], T0 + 3600, 3, &zone);
        assert_eq!(days[2].start, T0 - 2 * 3600);
        assert_eq!(days[1].start, T0 - 2 * 3600 - DAY);
    }

    #[test]
    fn the_file_lines_are_small_and_read_back() {
        let line = serde_json::to_string(&level(T0, 62, Activity::Playing)).unwrap();
        let expected = format!(
            "{{\"k\":\"level\",\"t\":{T0},\"pct\":62,\"activity\":\"playing\",{}",
            "\"bt\":false,\"est\":true}"
        );
        assert_eq!(line, expected);
        let old = format!("{{\"k\":\"level\",\"t\":{T0},\"pct\":62,\"activity\":\"awake\"}}");
        assert_eq!(
            serde_json::from_str::<Record>(&old).unwrap(),
            Record::Level {
                t: T0,
                pct: 62,
                activity: Activity::Awake,
                bt: false,
                est: false
            }
        );
        assert!(serde_json::from_str::<Record>("{\"k\":\"level\",\"t\":").is_err());
    }
}
