//! The main loop and the watchdog while the screen is dark and nothing goes
//! on ("quiet": no sound, no task, no start, scan or wait in flight). Nobody
//! sees a frame then, so both wait for events and long timers instead of
//! passing every second (test build 172, screen off on the charger: the main
//! thread 24.6 s and the watchdog 14.4 s CPU per hour).
//!
//! Still at once: input (Power, buttons with the screen off), the control
//! socket (the platform's sleep RPCs, rebornctl), AVRCP commands, Bluetooth
//! status (both write the control wake). Between full passes a
//! one-second check reads only what lights the screen or starts the shutdown
//! screen: the platform's shutdown intent and the USB state. The timers a
//! pass owes (the automatic sleep, Wi-Fi's idle power-down, a held pause, the
//! sleep timer, a due checkpoint) end the wait when they are due.
use reborn_core::platform::{sleep_retry_seconds, HEADSET_PAUSE_AWAKE_SECS};
use std::time::{Duration, Instant};

/// The longest wait between two full passes while dark and quiet (1 s before).
pub const PASS: Duration = Duration::from_secs(5);
/// The shutdown intent and the USB state are read this often in between.
pub const TICK: Duration = Duration::from_secs(1);
/// Battery and media sources (the watchdog's observation) while dark and
/// quiet; every 2 s otherwise, and once at once when the screen lights.
pub const OBSERVATION: Duration = Duration::from_secs(10);
/// The diagnostic snapshot and the panic summary while dark and quiet (once
/// when the quiet begins, then this often); every 2 s otherwise.
pub const SNAPSHOT: Duration = Duration::from_secs(30);
/// The watchdog's own pass (kernel log, health verdict) while dark and
/// quiet; every second otherwise. Worker deadlines are 10-30 s.
pub const WATCHDOG: Duration = Duration::from_secs(5);

/// How long the dark loop waits for the next full pass: until the earliest
/// timer it owes, but not before the periodic block (which runs the automatic
/// sleep) can run again, and at most `PASS`.
pub fn pass_wait(
    now: Instant,
    timers: impl IntoIterator<Item = Instant>,
    periodic_due: Instant,
) -> Duration {
    timers
        .into_iter()
        .min()
        .map_or(PASS, |next| {
            next.max(periodic_due).saturating_duration_since(now)
        })
        .min(PASS)
}

/// The earliest moment `reborn_core::platform::automatic_sleep_due` (with its
/// retry wait and the paused-headset hold) can ask to sleep: 60 s dark, 60 s
/// without input, the retry wait after the last attempt, and the end of a
/// pause with headphones connected.
pub fn automatic_sleep_at(
    dark_since: Instant,
    last_activity: Instant,
    attempt: Option<Instant>,
    refusals: u32,
    headset_paused: Option<Instant>,
) -> Instant {
    let minute = Duration::from_secs(60);
    let mut at = (dark_since + minute).max(last_activity + minute);
    if let Some(attempt) = attempt {
        at = at.max(attempt + Duration::from_secs(sleep_retry_seconds(refusals).max(60)));
    }
    if let Some(paused) = headset_paused {
        at = at.max(paused + Duration::from_secs(HEADSET_PAUSE_AWAKE_SECS));
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;
    use reborn_core::platform::{automatic_sleep_due, headset_pause_awake};

    /// The tick's own verdict `seconds` after `base` (as `automatic_sleep_tick`
    /// reads its instants: whole seconds elapsed).
    fn due_after(
        base: Instant,
        seconds: u64,
        dark: Instant,
        activity: Instant,
        attempt: Option<Instant>,
        refusals: u32,
        headset: Option<Instant>,
    ) -> bool {
        let now = base + Duration::from_secs(seconds);
        let secs = |t: Instant| now.saturating_duration_since(t).as_secs();
        !headset_pause_awake(headset.map(secs))
            && automatic_sleep_due(
                true,
                Some(secs(dark)),
                secs(activity),
                attempt.map(secs),
                false,
                false,
            )
            && attempt.is_none_or(|a| secs(a) >= sleep_retry_seconds(refusals))
    }

    #[test]
    fn the_sleep_deadline_is_the_ticks_first_due_second() {
        let base = Instant::now();
        let at = |s: u64| base + Duration::from_secs(s);
        for (dark, activity, attempt, refusals, headset) in [
            (0, 0, None, 0, None),
            (0, 30, None, 0, None),
            (45, 10, Some(20), 0, None),
            (0, 0, Some(5), 3, None),
            (0, 0, Some(5), 9, None),
            (0, 0, None, 0, Some(40)),
            (100, 100, Some(90), 1, Some(95)),
        ] {
            let (dark, activity) = (at(dark), at(activity));
            let (attempt, headset) = (attempt.map(at), headset.map(at));
            let due = automatic_sleep_at(dark, activity, attempt, refusals, headset);
            let seconds = due.duration_since(base).as_secs();
            assert_eq!(due, at(seconds), "whole seconds");
            let verdict = |s| due_after(base, s, dark, activity, attempt, refusals, headset);
            assert!(verdict(seconds), "due at {seconds}");
            assert!(!verdict(seconds - 1), "not before {seconds}");
        }
    }

    #[test]
    fn the_wait_ends_at_the_first_timer_after_the_periodic_block() {
        let now = Instant::now();
        let s = Duration::from_secs;
        assert_eq!(pass_wait(now, [], now + s(2)), PASS);
        assert_eq!(pass_wait(now, [now + s(3), now + s(60)], now), s(3));
        assert_eq!(pass_wait(now, [now + s(600)], now), PASS);
        // A timer already due waits for the periodic block that serves it.
        assert_eq!(pass_wait(now, [now], now + s(2)), s(2));
        assert_eq!(pass_wait(now, [now + s(1)], now - s(1)), s(1));
        // A due timer with the periodic block due too: the pass runs now.
        assert_eq!(pass_wait(now + s(1), [now], now), Duration::ZERO);
    }
}
