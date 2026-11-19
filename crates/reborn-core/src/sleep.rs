//! The sleep timer: its choices, the item it waits for and how it follows the listener.
use crate::{AppModel, QueueEntryId};

/// The sleep timer's choices, picked with the wheel from Now Playing's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepChoice {
    Off,
    Minutes(u16),
    /// The end of the chapter (a book) or the song playing.
    EndOfItem,
}
/// The timed lengths in minutes (#29): short steps first, up to three hours.
pub const SLEEP_MINUTES: [u16; 12] = [5, 10, 15, 20, 30, 45, 60, 75, 90, 120, 150, 180];
impl SleepChoice {
    /// The picker's rows: Off, the end of this song or chapter, the lengths.
    pub fn all() -> Vec<Self> {
        let mut all = vec![Self::Off, Self::EndOfItem];
        all.extend(SLEEP_MINUTES.iter().map(|&m| Self::Minutes(m)));
        all
    }
}
/// What the sleep timer tells apart: the queue entry playing (its place in the
/// queue moves when songs are added or removed, the entry does not) and, in a
/// book, the chapter.
pub type SleepItem = (Option<QueueEntryId>, Option<usize>);

/// A running sleep timer: playback pauses at `until` (after a fade) or when
/// the item playing when it was set (queue entry, chapter) ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SleepTimer {
    pub choice: SleepChoice,
    pub until: Option<std::time::Instant>,
    pub item: SleepItem,
}
impl SleepTimer {
    /// The timer after the listener moved from the item `before` to `now`
    /// (a skip, a song picked from a list, a new queue); `None`: it ends.
    /// Only "end of the song or chapter" follows items; the listener's own
    /// choice is not the item ending, so it never pauses what they chose.
    /// While playing, the timer goes on with the new item (skipping on the way
    /// to sleep still ends in sleep); one left over from a pause or a stop
    /// belongs to a listening that is over (#137: it paused the next
    /// morning's songs the moment they started).
    pub fn after_listener_moved(
        self,
        before: SleepItem,
        now: SleepItem,
        was_playing: bool,
    ) -> Option<Self> {
        if self.until.is_some() || self.item != before || now == before {
            return Some(self);
        }
        was_playing.then_some(Self { item: now, ..self })
    }
}
impl AppModel {
    /// What plays now, as the sleep timer tells items apart.
    pub fn sleep_item(&self) -> SleepItem {
        (
            self.queue_entry_ids.get(self.queue_position).copied(),
            self.book_part()
                .and_then(|(book, part)| book.chapter_at(part, self.position_ms)),
        )
    }
}

#[cfg(test)]
mod sleep_timer_tests {
    use super::*;
    #[test]
    fn the_sleep_timer_offers_off_the_end_of_the_item_and_lengths_to_three_hours() {
        let all = SleepChoice::all();
        assert_eq!(all.len(), 14);
        assert_eq!(
            all[..3],
            [
                SleepChoice::Off,
                SleepChoice::EndOfItem,
                SleepChoice::Minutes(5)
            ]
        );
        assert_eq!(all.last(), Some(&SleepChoice::Minutes(180)));
        assert!(SLEEP_MINUTES.windows(2).all(|w| w[0] < w[1]));
    }

    fn timer(item: SleepItem) -> SleepTimer {
        SleepTimer {
            choice: SleepChoice::EndOfItem,
            until: None,
            item,
        }
    }
    fn song(id: u64) -> SleepItem {
        (Some(QueueEntryId(id)), None)
    }
    #[test]
    fn a_song_picked_while_playing_carries_the_end_of_song_timer_along() {
        let moved = timer(song(1)).after_listener_moved(song(1), song(2), true);
        assert_eq!(moved, Some(timer(song(2))));
    }
    #[test]
    fn a_song_picked_after_a_pause_or_stop_ends_the_end_of_song_timer() {
        assert_eq!(
            timer(song(1)).after_listener_moved(song(1), song(2), false),
            None
        );
    }
    #[test]
    fn the_timer_is_left_alone_when_the_listener_did_not_change_the_song() {
        let t = timer(song(1));
        assert_eq!(t.after_listener_moved(song(1), song(1), true), Some(t));
        // The song it was set on ended meanwhile: the tick pauses, not this.
        assert_eq!(t.after_listener_moved(song(2), song(3), true), Some(t));
        let timed = SleepTimer {
            choice: SleepChoice::Minutes(5),
            until: Some(std::time::Instant::now()),
            item: song(1),
        };
        assert_eq!(
            timed.after_listener_moved(song(1), song(2), false),
            Some(timed)
        );
    }
    #[test]
    fn a_new_chapter_in_a_book_counts_as_a_new_item() {
        let (a, b) = (
            (Some(QueueEntryId(1)), Some(0)),
            (Some(QueueEntryId(1)), Some(1)),
        );
        assert_eq!(timer(a).after_listener_moved(a, b, true), Some(timer(b)));
    }
    #[test]
    fn removing_songs_before_the_playing_one_keeps_its_sleep_item() {
        let entry = |at: usize, ids: Vec<QueueEntryId>| AppModel {
            queue_entry_ids: ids,
            queue_position: at,
            ..Default::default()
        };
        let ids = vec![QueueEntryId(7), QueueEntryId(8), QueueEntryId(9)];
        let before = entry(2, ids.clone()).sleep_item();
        let after = entry(1, vec![ids[1], ids[2]]).sleep_item();
        assert_eq!(before, after);
    }
}
