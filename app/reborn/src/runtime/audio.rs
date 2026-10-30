//! Model actions, the sink reconfiguration and the volume (wired and Bluetooth).
use crate::*;

impl Runtime {
    pub(crate) fn with_model_action(
        &mut self,
        action: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<(), String> {
        let outermost = self.action_depth == 0;
        if outermost {
            if self.pending_reconfiguration.is_some() {
                self.ui.flash("Audio output is still starting");
                self.dirty.mark_render();
                return Err("audio reconfiguration is still pending".into());
            }
            // The library, book progress and chapter marks are not rolled
            // back (and not copied for every action).
            let tracks = std::mem::take(&mut self.model.library.tracks);
            let shelf = std::mem::take(&mut self.model.library.shelf);
            let chapters = std::mem::take(&mut self.model.library.chapters);
            let platform = std::mem::take(&mut self.model.platform);
            self.action_before = Some(self.model.clone());
            self.model.library.tracks = tracks;
            self.model.library.shelf = shelf;
            self.model.library.chapters = chapters;
            self.model.platform = platform;
            self.action_dirty_before = Some(self.dirty);
            self.reconfiguration_attempted = false;
            self.sink_release_started = false;
            self.transition_generation = None;
        }
        self.action_depth += 1;
        let result = action(self);
        self.action_depth -= 1;
        if outermost {
            let before = self.action_before.take().expect("outer action snapshot");
            let dirty_before = self
                .action_dirty_before
                .take()
                .expect("outer action dirty snapshot");
            let attempted = self.reconfiguration_attempted;
            let release_started = self.sink_release_started;
            let generation = self.transition_generation;
            let start_pending = self.playback.has_pending_start();
            self.reconfiguration_attempted = false;
            self.sink_release_started = false;
            self.transition_generation = None;
            if result.is_ok() && attempted && start_pending {
                self.pending_reconfiguration = Some(PendingReconfiguration {
                    previous: before,
                    dirty_before,
                    generation: generation.unwrap_or(self.model.generation),
                });
            } else if let Err(error) = &result {
                if attempted {
                    self.restore_failed_reconfiguration(
                        before,
                        dirty_before,
                        error,
                        release_started,
                        generation,
                    );
                }
            }
        }
        result
    }

    pub(crate) fn restore_failed_reconfiguration(
        &mut self,
        mut previous: AppModel,
        dirty_before: DirtyState,
        error: &str,
        sink_release_started: bool,
        transition_generation: Option<u64>,
    ) {
        previous.library.tracks = std::mem::take(&mut self.model.library.tracks);
        previous.library.shelf = std::mem::take(&mut self.model.library.shelf);
        previous.library.chapters = std::mem::take(&mut self.model.library.chapters);
        previous.platform = std::mem::take(&mut self.model.platform);
        self.keep_held_volume(&mut previous);
        if !sink_release_started {
            self.model = previous;
            self.dirty = dirty_before;
            return;
        }
        self.model = failed_reconfiguration_model(previous, error, true, transition_generation);
        self.playback.stop(self.model.generation);
        self.active_transport_generation = None;
        self.art = false;
        self.dirty.mark_both();
        self.checkpoint();
    }
    pub(crate) fn poll_pending_reconfiguration(&mut self) {
        if self.pending_reconfiguration.is_none() {
            return;
        }
        let epoch_state = transport_epoch_state(
            &self.model.output,
            self.model.playback,
            self.active_transport_generation,
            &self.bt_state,
        );
        if epoch_state != TransportEpochState::Current {
            let pending = self.pending_reconfiguration.take().unwrap();
            let error = match epoch_state {
                TransportEpochState::Changed => "Bluetooth transport changed during sink open",
                TransportEpochState::Unavailable => {
                    "Bluetooth transport disappeared during sink open"
                }
                TransportEpochState::Current => unreachable!(),
            };
            self.restore_failed_reconfiguration(
                pending.previous,
                pending.dirty_before,
                error,
                true,
                Some(pending.generation),
            );
            self.ui.flash(error);
            self.dirty.mark_render();
            return;
        }
        let Some(result) = self.playback.poll_start() else {
            return;
        };
        let Some(pending) = self.pending_reconfiguration.take() else {
            if let Err(error) = result {
                self.fail("playback", error);
            }
            return;
        };
        match result {
            Ok(()) => {
                if self.dirty.checkpoint_due() {
                    self.checkpoint();
                }
            }
            Err(error) => {
                self.restore_failed_reconfiguration(
                    pending.previous,
                    pending.dirty_before,
                    &error,
                    true,
                    Some(pending.generation),
                );
                self.ui.flash(format!("Playback could not start: {error}"));
                self.dirty.mark_render();
            }
        }
    }
    pub(crate) fn abandon_pending_reconfiguration(&mut self) {
        if let Some(mut pending) = self.pending_reconfiguration.take() {
            self.keep_held_volume(&mut pending.previous);
            self.model = pending.previous;
            self.dirty = pending.dirty_before;
        }
    }
    /// A rolled-back model keeps the level headphones hold: it is theirs,
    /// set on them already, not part of the change that failed.
    pub(crate) fn keep_held_volume(&self, previous: &mut AppModel) {
        if self.bt_volume.device.is_some() {
            let now = &self.model.settings;
            previous.settings.volume = now.volume;
            previous.settings.wired_volume = now.wired_volume;
            previous.settings.bluetooth_absolute_volume = now.bluetooth_absolute_volume;
        }
    }

    /// Music plays on the headphone jack (where the safe level applies).
    pub(crate) fn wired_sink(&self) -> bool {
        matches!(self.model.output, AudioOutput::Wired) && self.bt_volume.device.is_none()
    }
    /// Back on the jack: a volume above the safe level starts at it, since
    /// other headphones may be plugged in now.
    pub(crate) fn cap_wired_volume(&mut self) {
        let safe = self.model.settings.hp_level.safe_volume();
        if self.wired_sink() && self.model.settings.volume > safe {
            self.model.settings.volume = safe;
            self.playback.set_volume(self.sink_volume());
            self.dirty.mark_both();
            self.dirty.mark_persistence();
        }
    }
    /// Counts wired listening above the safe level (every few seconds);
    /// after `LOUD_LISTENING_LIMIT_S` the agreement lapses and the volume
    /// returns to the safe level.
    pub(crate) fn loud_listening_tick(&mut self, elapsed: Duration) {
        let s = &self.model.settings;
        let loud = s.loud_ok
            && self.wired_sink()
            && self.model.playback == PlaybackState::Playing
            && s.volume > s.hp_level.safe_volume();
        if !loud {
            return;
        }
        // Instant does not advance in suspend; a stalled loop counts once.
        let add = elapsed.as_secs().min(10) as u32;
        let s = &mut self.model.settings;
        s.loud_seconds = s.loud_seconds.saturating_add(add);
        if s.loud_seconds < reborn_core::LOUD_LISTENING_LIMIT_S {
            if s.loud_seconds % 60 < add {
                self.dirty.mark_persistence();
            }
            return;
        }
        s.loud_ok = false;
        s.loud_seconds = 0;
        self.log.emit(
            Level::Info,
            "audio",
            "loud_volume_lapsed",
            "20 hours above the safe level: volume back to it",
            None,
            json!({"hp_level":self.model.settings.hp_level,"volume":self.model.settings.volume}),
        );
        self.cap_wired_volume();
        self.ui
            .flash("Volume lowered after 20 hours of loud listening");
        self.checkpoint();
    }
    /// The software level: full for headphones that hold the volume.
    pub(crate) fn sink_volume(&self) -> u8 {
        if self.bt_volume.device.is_some() {
            100
        } else {
            self.model.settings.volume
        }
    }
    /// The playing Bluetooth headphones and their level, when they report one.
    pub(crate) fn absolute_volume_peer(&self) -> Option<(String, u8)> {
        let AudioOutput::Bluetooth(address) = &self.model.output else {
            return None;
        };
        self.bt_state
            .devices
            .iter()
            .find(|d| d.address.eq_ignore_ascii_case(address) && d.connected)
            .and_then(|d| Some((d.path.clone(), d.volume?)))
    }
    pub(crate) fn send_bt_volume(&mut self) {
        let (Some(device), Some(service)) = (self.bt_volume.device.clone(), &self.bluetooth) else {
            return;
        };
        let volume = percent_to_avrcp(self.model.settings.volume);
        self.bt_volume.unsent = service
            .commands
            .try_send(bluetooth::Command::Volume { device, volume })
            .is_err();
        self.bt_volume.sent = Some(Instant::now());
    }
    /// Follows headphones that hold their own volume: taking it over when
    /// they become the output, adopting changes made on them, and giving
    /// the wired volume back when they stop being the output.
    pub(crate) fn sync_bt_volume(&mut self) {
        let peer = self.absolute_volume_peer();
        if self.bt_volume.device.is_some()
            && peer.as_ref().map(|(device, _)| device) != self.bt_volume.device.as_ref()
        {
            if let Some(wired) = self.model.settings.wired_volume.take() {
                self.model.settings.volume = wired;
            }
            self.bt_volume = BtVolume::default();
            self.cap_wired_volume();
            self.playback.set_volume(self.model.settings.volume);
            self.dirty.mark_both();
            self.dirty.mark_persistence();
        }
        let Some((device, reported)) = peer else {
            return;
        };
        if self.bt_volume.device.is_none() {
            let wired = self.model.settings.volume;
            // The loudness so far stays: the software level (before any
            // headphones held the volume) moves into the headphones' level.
            let software = if self.model.settings.bluetooth_absolute_volume {
                100
            } else {
                wired
            };
            let level = takeover_level(reported, software);
            self.model.settings.wired_volume = Some(wired);
            self.model.settings.volume = avrcp_to_percent(level);
            self.model.settings.bluetooth_absolute_volume = true;
            self.bt_volume = BtVolume {
                device: Some(device),
                reported: Some(reported),
                sent: None,
                unsent: level != reported,
            };
            self.playback.set_volume(100);
            self.log.emit(
                Level::Info,
                "audio",
                "bluetooth_volume",
                "headphones hold the volume (absolute volume)",
                None,
                json!({"reported":reported,"volume":self.model.settings.volume,"wired":wired}),
            );
            self.dirty.mark_both();
            self.dirty.mark_persistence();
        } else if self.bt_volume.reported != Some(reported) {
            self.bt_volume.reported = Some(reported);
            let echo = self
                .bt_volume
                .sent
                .is_some_and(|sent| sent.elapsed() < BT_VOLUME_ECHO);
            if !echo && !self.bt_volume.unsent {
                // Changed on the headphones.
                self.model.settings.volume = avrcp_to_percent(reported);
                self.ui.show_volume();
                self.dirty.mark_both();
                self.dirty.mark_persistence();
            }
        }
        if self.bt_volume.unsent {
            self.send_bt_volume();
        }
    }
    pub(crate) fn play_index(&mut self, index: usize, shuffle: ShuffleMode) -> Result<(), String> {
        // The music library around the song; audiobook parts stay out of
        // it (Y2Linux #58), they play through Effect::PlayBook.
        let tracks = &self.model.library.tracks;
        let members = (0..tracks.len())
            .filter(|&i| i == index || tracks[i].is_music())
            .collect();
        self.play_members(index, members, shuffle, "Songs".into(), None)
    }
}
