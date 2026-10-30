//! Next and previous, playing now, shuffle and the queue.
use crate::*;

impl Runtime {
    /// `Effect::PlayNow`.
    pub(crate) fn on_play_now(&mut self, index: usize) -> Result<(), String> {
        let track = self
            .model
            .library
            .tracks
            .get(index)
            .cloned()
            .ok_or("invalid selection")?;
        if !track.online {
            return Err("media source offline".into());
        }
        if !self.model.queue_next(vec![track])? {
            self.model.jump_to(self.model.queue_position + 1);
        }
        self.load()?;
        self.checkpoint();
        Ok(())
    }

    /// `Effect::NextTrack` and `Effect::PreviousTrack`.
    pub(crate) fn on_next_or_previous_track(&mut self, e: &Effect) -> Result<(), String> {
        let delta = if matches!(e, Effect::NextTrack) {
            1
        } else {
            -1
        };
        // A book moves by chapter.
        if let Some(result) = self.book_step(delta) {
            result?;
        // A station has no next track: the next station of its list.
        } else if self.model.current().is_some_and(Track::is_radio) {
            self.zap(delta)?;
        // Previous more than 3 s into a song starts it again; a
        // second press goes back a song (Y2Reborn #21).
        } else if delta < 0
            && self.model.current().is_some()
            && (self.model.position_ms > RESTART_SONG_AFTER_MS || self.model.queue_position == 0)
        {
            // The first song has nothing before it: it starts over.
            self.seek_by(-(self.model.position_ms as i64));
        // Next on the last song: Repeat All starts over.
        } else if (delta > 0
            && self.model.queue_position + 1 >= self.model.queue.len()
            && self.model.settings.repeat == RepeatMode::All
            && self.model.wrap())
            || self.model.step(delta)
        {
            self.load()?
        } else {
            self.model.invalidate();
            self.playback.stop(self.model.generation);
        }
        Ok(())
    }

    /// `Effect::SetShuffle`.
    pub(crate) fn on_set_shuffle(&mut self, mode: ShuffleMode) -> Result<(), String> {
        let was = self.model.settings.shuffle_mode();
        self.model.settings.set_shuffle_mode(mode);
        // What plays now follows at once: its songs to come shuffle,
        // as songs or albums, or go back to their own order (your
        // queue stays as it is).
        let music =
            self.model.book.is_none() && self.model.current().is_some_and(|t| !t.is_radio());
        if music && mode != was {
            let mut changed = was != ShuffleMode::Off && self.model.unshuffle_source();
            changed |= match mode {
                ShuffleMode::Off => false,
                ShuffleMode::Songs => self.model.shuffle_source(shuffle_seed()),
                ShuffleMode::Albums => self.model.shuffle_source_albums(shuffle_seed()),
            };
            if changed {
                self.upcoming_changed();
            }
        }
        self.checkpoint();
        Ok(())
    }

    /// `Effect::QueueRemove`.
    pub(crate) fn on_queue_remove(&mut self, index: usize) -> Result<(), String> {
        if index == self.model.queue_position {
            self.ui.flash("The current track stays in the queue");
        } else if let Some(&id) = self.model.queue_entry_ids.get(index) {
            if self.model.remove_entries(&[id]) > 0 {
                self.upcoming_changed();
                self.checkpoint();
            }
        }
        Ok(())
    }

    /// `Effect::QueueRemoveEntries`.
    pub(crate) fn on_queue_remove_entries(
        &mut self,
        ids: Vec<reborn_core::QueueEntryId>,
    ) -> Result<(), String> {
        let removed = self.model.remove_entries(&ids);
        if removed > 0 {
            self.upcoming_changed();
            self.checkpoint();
            self.ui.flash(if removed == 1 {
                "1 song removed".to_owned()
            } else {
                format!("{removed} songs removed")
            });
        }
        Ok(())
    }

    /// `Effect::QueueEntriesNext`.
    pub(crate) fn on_queue_entries_next(
        &mut self,
        ids: Vec<reborn_core::QueueEntryId>,
    ) -> Result<(), String> {
        let moved = self.model.entries_next(&ids);
        if moved > 0 {
            self.upcoming_changed();
            self.checkpoint();
            self.ui.flash(if moved == 1 {
                "Plays next".to_owned()
            } else {
                format!("{moved} songs play next")
            });
        }
        Ok(())
    }

    /// `Effect::QueueEntriesLast`.
    pub(crate) fn on_queue_entries_last(
        &mut self,
        ids: Vec<reborn_core::QueueEntryId>,
    ) -> Result<(), String> {
        let moved = self.model.entries_last(&ids);
        if moved > 0 {
            self.upcoming_changed();
            self.checkpoint();
            self.ui.flash(if moved == 1 {
                "Added to queue".to_owned()
            } else {
                format!("{moved} songs added to queue")
            });
        }
        Ok(())
    }
}
