//! The battery history on the player: each battery observation that changes something becomes a
//! record, kept in the model for the history page and written to disk in batches.
//!
//! No timer, no wake of its own (power plan 2026-11: the dark loop's deadlines are measured and
//! stay as they are): records are made where the battery is observed anyway, a batch is written by
//! the periodic pass that is running anyway, and the sleep and the shutdown write what is left.
use crate::*;
use reborn_core::battery_history::{Activity, Ledger, Observation, Record, Situation, KEEP_S};
use reborn_platform::battery_history::{self as file, Store};
use std::sync::Arc;

/// A batch of records waits this long before it is written (the periodic pass writes it).
const BATCH: Duration = Duration::from_secs(10 * 60);
/// The file is rewritten without the old records at most this often.
const PRUNE_EVERY: i64 = 24 * 3600;

/// What waits to be written, and when the file was last cleaned.
pub(crate) struct Writer {
    store: Store,
    pending: Vec<Record>,
    since: Option<Instant>,
    pruned_at: i64,
    booted: bool,
}

impl Writer {
    /// The history of the last run, ready to use.
    pub(crate) fn open(root: &Path) -> (Self, Ledger) {
        let store = Store::new(root.join("state").join(file::FILE));
        let ledger = Ledger::from_records(store.load());
        (
            Self {
                store,
                pending: Vec::new(),
                since: None,
                pruned_at: 0,
                booted: false,
            },
            ledger,
        )
    }

    fn push(&mut self, record: Record) {
        self.since.get_or_insert_with(Instant::now);
        self.pending.push(record);
    }

    /// A batch is waiting long enough to be written.
    pub(crate) fn due(&self) -> bool {
        self.since.is_some_and(|since| since.elapsed() >= BATCH)
    }

    /// Write what waits, or (once a day) the whole cleaned history. A failed write keeps the
    /// records for the next try. `durable` syncs them to flash (the sleep and the shutdown).
    fn flush(&mut self, ledger: &mut Ledger, now: i64, durable: bool) -> bool {
        if self.pending.is_empty() {
            return true;
        }
        let clean = now - self.pruned_at >= PRUNE_EVERY;
        let written = if clean {
            ledger.prune(now);
            ledger.cap(file::RECORDS_MAX);
            self.store.rewrite(ledger.records())
        } else {
            self.store.append(&self.pending, durable)
        };
        if written.is_err() {
            return false;
        }
        if clean {
            self.pruned_at = now;
        }
        self.pending.clear();
        self.since = None;
        true
    }
}

impl Runtime {
    /// The battery was observed: record what changed since the last record.
    pub(crate) fn record_battery(&mut self) {
        let situation = Situation {
            cable: self.model.platform.battery.cable,
            asleep: false,
            screen_on: !self.model.screen_off,
            playing: matches!(
                self.model.playback,
                PlaybackState::Playing | PlaybackState::Buffering
            ),
        };
        self.record_battery_as(situation.activity());
    }

    /// The player is about to sleep: the state until the first record after the wake.
    pub(crate) fn record_battery_sleep(&mut self) {
        let asleep = Situation {
            cable: self.model.platform.battery.cable,
            asleep: true,
            ..Situation::default()
        };
        self.record_battery_as(asleep.activity());
        self.write_battery_history(true);
    }

    /// The player shuts down: the time after this record is off, not use.
    pub(crate) fn record_battery_down(&mut self) {
        let Some(now) = self.trusted_unix() else {
            return;
        };
        let ledger = Arc::make_mut(&mut self.model.battery_history);
        if let Some(record) = ledger.down(now) {
            self.battery_writer.push(record);
        }
        self.write_battery_history(true);
    }

    /// The periodic pass: write a batch that has waited long enough.
    pub(crate) fn battery_history_tick(&mut self) {
        if self.battery_writer.due() {
            self.write_battery_history(false);
        }
    }

    fn record_battery_as(&mut self, activity: Activity) {
        // Before the clock is known there is no time to put on a record (checked every 30 s).
        let Some(now) = self.trusted_unix() else {
            return;
        };
        let battery = self.model.platform.battery;
        let seen = Observation {
            pct: battery.percent,
            activity,
            bt: matches!(self.model.output, AudioOutput::Bluetooth(_)),
            estimated: battery.estimated,
        };
        let ledger = Arc::make_mut(&mut self.model.battery_history);
        if !self.battery_writer.booted {
            self.battery_writer.booted = true;
            let id = file::boot_id();
            if let Some(record) = (!id.is_empty()).then(|| ledger.boot(now, &id)).flatten() {
                self.battery_writer.push(record);
            }
        }
        if let Some(record) = ledger.observe(now, seen) {
            self.battery_writer.push(record);
            if self.model.screen == Screen::BatteryHistory {
                self.dirty.mark_render();
            }
        }
    }

    fn write_battery_history(&mut self, durable: bool) {
        let Some(now) = self.trusted_unix() else {
            return;
        };
        let ledger = Arc::make_mut(&mut self.model.battery_history);
        if !self.battery_writer.flush(ledger, now, durable) {
            self.log.emit(
                Level::Warn,
                "power",
                "battery_history_write_failed",
                "The battery history could not be written",
                None,
                json!({"waiting":self.battery_writer.pending.len(),"keep_days":KEEP_S / 86_400}),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("reborn-bathist-rt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn level(t: i64, pct: u8) -> Record {
        Record::Level {
            t,
            pct,
            activity: Activity::Awake,
            bt: false,
            est: false,
        }
    }

    #[test]
    fn a_batch_is_written_once_and_again_only_with_news() {
        let root = dir("batch");
        let (mut writer, mut ledger) = Writer::open(&root);
        assert!(ledger.records().is_empty() && !writer.due());
        writer.pruned_at = 1_800_000_000;
        writer.push(level(1_800_000_001, 80));
        assert!(!writer.due(), "a batch waits to fill");
        assert!(writer.flush(&mut ledger, 1_800_000_002, false));
        assert!(writer.pending.is_empty() && writer.since.is_none());
        // Nothing waits: no write, no failure.
        assert!(writer.flush(&mut ledger, 1_800_000_003, true));
        let (_, again) = Writer::open(&root);
        assert_eq!(again.records(), &[level(1_800_000_001, 80)]);
    }

    #[test]
    fn once_a_day_the_file_is_rewritten_without_the_old_records() {
        let root = dir("prune");
        let now = 1_800_000_000;
        let (mut writer, _) = Writer::open(&root);
        let ancient = level(now - 50 * 86_400, 95);
        let before = level(now - 40 * 86_400, 90);
        let keep = level(now - 86_400, 70);
        let latest = level(now, 60);
        let mut ledger = Ledger::from_records(vec![
            ancient.clone(),
            before.clone(),
            keep.clone(),
            latest.clone(),
        ]);
        writer.store.append(ledger.records(), false).unwrap();
        writer.push(latest.clone());
        assert!(writer.flush(&mut ledger, now, false));
        let (_, stored) = Writer::open(&root);
        // The last state before the cut stays (it is the state at the start of the month).
        assert_eq!(stored.records(), &[before, keep, latest]);
        assert_eq!(writer.pruned_at, now);
    }

    #[test]
    fn a_failed_write_keeps_the_records() {
        let root = dir("fail");
        // A directory where the file should be: every write fails.
        std::fs::create_dir_all(root.join("state").join(file::FILE)).unwrap();
        let (mut writer, mut ledger) = Writer::open(&root);
        writer.pruned_at = 1_800_000_000;
        writer.push(level(1_800_000_001, 80));
        assert!(!writer.flush(&mut ledger, 1_800_000_002, false));
        assert_eq!(writer.pending.len(), 1);
        assert!(writer.since.is_some());
    }
}
