//! The sleep timer: playback pauses after the chosen minutes (the volume
//! fading out over the last seconds) or when the chapter or song playing ends.
//! It lives only in memory: a restart ends it.
use crate::Runtime;
use reborn_core::{PlaybackState, SleepChoice, SleepTimer};
use std::time::{Duration, Instant};

/// The last stretch of a timed sleep fades the volume out.
const FADE: Duration = Duration::from_secs(10);

/// The volume `left` before a timed pause: full until the fade, then less.
pub fn faded(volume: u8, left: Duration) -> u8 {
    if left >= FADE {
        return volume;
    }
    (f32::from(volume) * left.as_secs_f32() / FADE.as_secs_f32()).round() as u8
}

impl Runtime {
    pub(crate) fn set_sleep_timer(&mut self, choice: SleepChoice) {
        self.end_sleep_fade();
        let item = self.model.sleep_item();
        self.model.sleep_timer = match choice {
            SleepChoice::Off => None,
            SleepChoice::Minutes(m) => Some(SleepTimer {
                choice,
                until: Some(Instant::now() + Duration::from_secs(u64::from(m) * 60)),
                item,
            }),
            SleepChoice::EndOfItem => Some(SleepTimer {
                choice,
                until: None,
                item,
            }),
        };
        self.dirty.mark_render();
    }

    /// The listener's own volume again after a fade.
    fn end_sleep_fade(&mut self) {
        if std::mem::take(&mut self.sleep_fading) {
            self.playback.set_volume(self.model.settings.volume);
        }
    }

    /// Every pass of the main loop: fade, pause when due.
    pub(crate) fn sleep_timer_tick(&mut self) {
        let Some(timer) = self.model.sleep_timer else {
            self.sleep_minutes_shown = None;
            return;
        };
        // Now Playing shows the minutes left: draw again when they change,
        // also while paused (#29).
        let minutes = timer.until.map(|until| {
            until
                .saturating_duration_since(Instant::now())
                .as_secs()
                .div_ceil(60)
        });
        if minutes != self.sleep_minutes_shown {
            self.sleep_minutes_shown = minutes;
            self.dirty.mark_render();
        }
        let playing = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        if let Some(until) = timer.until {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.model.sleep_timer = None;
                if playing {
                    self.pause();
                    self.ui.flash("Sleep timer paused playback");
                }
                self.end_sleep_fade();
                self.dirty.mark_both();
            } else if playing && left < FADE {
                self.playback
                    .set_volume(faded(self.model.settings.volume, left));
                self.sleep_fading = true;
            }
            return;
        }
        if self.model.playback == PlaybackState::Stopped {
            self.model.sleep_timer = None;
            self.dirty.mark_render();
            return;
        }
        if !playing || self.model.sleep_item() == timer.item {
            return;
        }
        // The chapter or song ended: pause, ready to start the next one from
        // its beginning.
        self.model.sleep_timer = None;
        let start = self
            .model
            .book_part()
            .and_then(|(book, part)| {
                let at = book.chapter_at(part, self.model.position_ms)?;
                book.chapter_span(at).map(|(start, _)| start)
            })
            .unwrap_or(0);
        self.pause();
        self.release_hold();
        self.model.position_ms = start;
        self.ui.flash("Sleep timer paused playback");
        self.dirty.mark_both();
        self.checkpoint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timed_sleep_fades_over_its_last_ten_seconds() {
        assert_eq!(faded(60, Duration::from_secs(60)), 60);
        assert_eq!(faded(60, Duration::from_secs(10)), 60);
        assert_eq!(faded(60, Duration::from_secs(5)), 30);
        assert_eq!(faded(60, Duration::from_millis(500)), 3);
        assert_eq!(faded(60, Duration::ZERO), 0);
    }
}
