//! The transport: loading a track, seeking, pause and resume, the next track, the jack and outputs.
use crate::*;

impl Runtime {
    pub(crate) fn checkpoint(&mut self) {
        // Where the playing audiobook is, with every session checkpoint.
        self.save_book();
        self.write_shelf();
        self.dirty.mark_persistence();
        if self.playback.has_pending_start() || self.pending_reconfiguration.is_some() {
            return;
        }
        if let Err(e) = self.model.checkpoint(&self.root.join("state/session.json")) {
            self.dirty.checkpoint_failed();
            self.log.emit(
                Level::Warn,
                "core",
                "checkpoint_failed",
                &e.to_string(),
                None,
                json!({}),
            );
        } else {
            self.dirty.checkpoint_succeeded();
        }
    }
    pub(crate) fn load(&mut self) -> Result<(), String> {
        if self.wait_for_lost_peer() {
            return Ok(());
        }
        // The output may just have become headphones with their own volume.
        self.sync_bt_volume();
        self.playback.set_balance(self.model.settings.balance);
        // Restore coordinator-owned cores before decoder/device startup.
        self.playback_hints
            .publish(reborn_platform::workload::playback_class(
                true,
                self.model.settings.playback_heavy(),
                false,
            ));
        self.reconfiguration_attempted = true;
        let track = self.model.current().cloned().ok_or("queue is empty")?;
        let radio = track.is_radio();
        if !radio && !track.path.is_file() {
            return Err("track source unavailable".into());
        }
        let _source = if radio {
            None
        } else {
            Some(storage::pin_track(
                &track,
                &self.model.settings.music_directory,
            )?)
        };
        let music_root = self.model.settings.music_directory.clone();
        let output = self.model.output.clone();
        // #28: the jack output plays on the speaker while nothing is in it.
        let speaker = self.speaker_route();
        self.model.speaker_active = speaker;
        let bluetooth_pcm = match &output {
            AudioOutput::Bluetooth(address) => Some(self.bt_state.playback_pcm(address)?),
            AudioOutput::Wired => None,
        };
        let generation = self.model.generation.wrapping_add(1);
        self.transition_generation = Some(generation);
        let entry_id = self
            .model
            .current_entry_id()
            .unwrap_or(QueueEntryId(self.model.queue_position as u64 + 1));
        // A station is never pre-opened for gapless playback: that would start
        // a second network transfer.
        // (`upcoming` leaves stations and Repeat One without a next song.)
        let (queue, queue_entry_ids) = self.model.upcoming(UPCOMING);
        let eq = self.model.settings.active_eq();
        let loaded = (self.model.settings.replay_gain, eq.clone());
        let fade_in = dsp_rise_fades(self.loaded_dsp.as_ref(), &loaded);
        let dsp = reborn_media::DspConfig {
            volume: self.sink_volume(),
            replay_gain: self.model.settings.replay_gain,
            eq_enabled: eq.is_some(),
            preamp_db: eq.as_ref().map_or(0.0, |(preamp, _)| *preamp),
            eq_bands: eq.map(|(_, bands)| bands).unwrap_or_default(),
            // A book's parts follow each other without a crossfade.
            crossfade_ms: if self.model.book_part().is_some() {
                0
            } else {
                self.model.settings.crossfade_ms
            },
            // A book's own speed; music always plays at its own.
            speed: self.model.book_part().map_or(1.0, |(book, _)| book.speed),
        };
        let request_parts = (
            track,
            entry_id,
            queue,
            queue_entry_ids,
            self.model.position_ms,
            generation,
            dsp,
            self.model.settings.gapless_enabled,
            self.log.correlation(),
            fade_in,
        );
        let log = self.log.clone();
        let correlation = self.log.correlation();
        let plan_output = output.clone();
        self.sink_release_started = true;
        let playback = &mut self.playback;
        run_sink_reconfiguration(
            playback,
            generation,
            |kept| {
                (keeps_wired_sink(kept, &output) && kept.speaker == speaker).then(|| kept.clone())
            },
            || {
                match bluetooth_pcm.as_ref() {
                    Some(pcm) => reborn_audio::AlsaSink::plan_bluetooth(
                        &plan_output,
                        pcm,
                        log.clone(),
                        correlation,
                    ),
                    None => reborn_audio::AlsaSink::plan(&plan_output, log.clone(), correlation),
                }
                .map(|(mut spec, _planned)| {
                    spec.speaker = speaker;
                    spec
                })
            },
            move |playback, spec| {
                let (
                    track,
                    entry_id,
                    queue,
                    queue_entry_ids,
                    position,
                    generation,
                    dsp,
                    gapless_enabled,
                    id,
                    fade_in,
                ) = request_parts;
                playback.begin_start_with_gapless(playback::LoadRequest {
                    track,
                    music_root,
                    entry_id,
                    queue,
                    queue_entry_ids,
                    position,
                    spec,
                    generation,
                    dsp,
                    gapless_enabled,
                    id,
                    fade_in,
                })
            },
        )?;
        self.loaded_dsp = Some(loaded);
        self.model.generation = generation;
        self.model.playback = PlaybackState::Buffering;
        self.active_transport_generation =
            bluetooth_pcm.as_ref().map(|pcm| pcm.transport_generation);
        self.art = false;
        self.dirty.mark_both();
        Ok(())
    }
    /// Move the position by `delta` ms. While playing, the sound holds and
    /// playback restarts once at the target after the last step
    /// (`apply_seek`); while paused, the next Play starts there.
    pub(crate) fn seek_by(&mut self, delta: i64) {
        let Some(length) = self.model.current().map(|t| t.duration_ms) else {
            return;
        };
        self.model.position_ms = self
            .model
            .position_ms
            .saturating_add_signed(delta)
            .min(length.saturating_sub(1));
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.playback.hold(true);
            self.seek_until = Some(Instant::now() + SEEK_SETTLE);
        } else if self.playback.is_held() {
            // A held pause would continue from the old place.
            self.release_hold();
        }
        self.dirty.mark_both();
    }
    /// The collected seek: restart at the target once the steps stopped and
    /// no restart is in progress.
    pub(crate) fn apply_seek(&mut self) {
        if self
            .seek_until
            .is_none_or(|until| Instant::now() < until || self.pending_reconfiguration.is_some())
        {
            return;
        }
        self.seek_until = None;
        if !matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            return;
        }
        if let Err(error) = self.with_model_action(|runtime| runtime.load()) {
            self.fail("playback", error);
        }
    }
    /// Give the held pipeline back (after a long pause, before sleep).
    pub(crate) fn release_hold(&mut self) {
        self.release_hold_with(false);
    }
    /// `wait`: until the sink is closed. An open sink holds the platform's
    /// activity lease, which refuses sleep before Reborn is asked to prepare
    /// (Beta 6: Power while paused said "finishing another task").
    pub(crate) fn release_hold_with(&mut self, wait: bool) {
        self.held_since = None;
        if !self.playback.is_held() {
            return;
        }
        self.log.emit(
            Level::Info,
            "playback",
            "pause_released",
            "Paused playback released its sink",
            None,
            json!({"position_ms":self.model.position_ms,"for_sleep":wait}),
        );
        self.model.invalidate();
        if wait {
            if let Err(error) = self.playback.stop_and_wait(self.model.generation) {
                self.log.emit(
                    Level::Warn,
                    "playback",
                    "pause_release_slow",
                    "Paused sink did not close in time",
                    None,
                    json!({"error":error}),
                );
            }
        } else {
            self.playback.stop(self.model.generation);
        }
        self.active_transport_generation = None;
    }
    /// Continue a pause held in place: no decoder, file or device restart.
    pub(crate) fn resume_in_place(&mut self) -> bool {
        // A held stream on the other route (headphones went in or out while
        // paused; the kernel turned a held speaker off) opens anew instead.
        if self.model.playback != PlaybackState::Paused
            || !self.playback.is_held()
            || self.pending_reconfiguration.is_some()
            || self.model.speaker_active != self.speaker_route()
            || self.awaits_lost_peer()
        {
            return false;
        }
        self.playback_hints
            .publish(reborn_platform::workload::playback_class(
                true,
                self.model.settings.playback_heavy(),
                false,
            ));
        self.playback.hold(false);
        self.held_since = None;
        self.model.playback = PlaybackState::Playing;
        self.log.emit(
            Level::Info,
            "playback",
            "resumed",
            "Playback resumed in place",
            None,
            json!({"position_ms":self.model.position_ms}),
        );
        self.dirty.mark_both();
        self.checkpoint();
        true
    }
    pub(crate) fn pause(&mut self) {
        self.paused_at = Some(Instant::now());
        self.playback_hints.clear();
        // A file paused while it plays holds in place: resume is instant. A
        // station (live), a start in progress or a pending seek release.
        // So does Bluetooth (F43, AirPods Pro 2 and others): a stream drained
        // and prepared again inside BlueALSA stayed silent in the headphones,
        // while a new stream (a skip) always played. Releasing the sink ends
        // the stream as a phone's pause does, and Play opens a new one where
        // the song was (about half a second).
        let bluetooth = matches!(self.model.output, AudioOutput::Bluetooth(_));
        if self.model.playback == PlaybackState::Playing
            && self.pending_reconfiguration.is_none()
            && self.seek_until.is_none()
            && self.model.current().is_some_and(|t| !t.is_radio())
            && !bluetooth
        {
            self.playback.hold(true);
            self.held_since = Some(Instant::now());
            self.model.playback = PlaybackState::Paused;
            self.log.emit(
                Level::Info,
                "playback",
                "paused",
                "Playback paused in place",
                None,
                json!({"position_ms":self.model.position_ms,"in_place":true}),
            );
            self.dirty.mark_both();
            self.checkpoint();
            return;
        }
        self.seek_until = None;
        self.held_since = None;
        self.abandon_pending_reconfiguration();
        self.log.emit(
            Level::Info,
            "playback",
            "paused",
            "Playback paused and sink released",
            None,
            json!({"position_ms":self.model.position_ms}),
        );
        self.model.invalidate();
        self.playback.stop(self.model.generation);
        self.active_transport_generation = None;
        self.model.playback = PlaybackState::Paused;
        self.dirty.mark_both();
        self.checkpoint();
    }
    pub(crate) fn fail_active_transport(&mut self, error: &str) {
        self.abandon_pending_reconfiguration();
        self.model.invalidate();
        self.playback.stop(self.model.generation);
        self.active_transport_generation = None;
        self.model.playback = PlaybackState::Error;
        self.model.last_error = Some(error.to_owned());
        self.art = false;
        self.dirty.mark_both();
        self.log.emit(
            Level::Error,
            "bluetooth",
            "transport_invalidated",
            error,
            None,
            json!({"recovery":"stopped_without_sink"}),
        );
        self.checkpoint();
    }
    pub(crate) fn toggle(&mut self) -> Result<(), String> {
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.pause();
            Ok(())
        } else if let Some(back) = self.book_rewind() {
            // A book continues a little before where it was left.
            self.release_hold();
            self.model.position_ms = self.model.position_ms.saturating_sub(back);
            self.paused_at = None;
            self.load()
        } else if self.resume_in_place() {
            Ok(())
        } else {
            self.load()
        }
    }

    pub(crate) fn finish_track(&mut self) -> Result<(), String> {
        self.with_model_action(Self::finish_track_inner)
    }
    pub(crate) fn finish_track_inner(&mut self) -> Result<(), String> {
        // A book plays its parts in order once, whatever Repeat says.
        if let Some(result) = self.finish_book_part() {
            return result;
        }
        match self.model.settings.repeat {
            reborn_core::RepeatMode::Track => {
                self.model.position_ms = 0;
                self.load()
            }
            reborn_core::RepeatMode::All => {
                if self.model.queue.is_empty() {
                    return Ok(());
                }
                if self.model.queue_position + 1 < self.model.queue.len() {
                    self.model.step(1);
                } else {
                    // Your queued songs that played leave; the start follows.
                    self.model.wrap();
                }
                self.load()
            }
            reborn_core::RepeatMode::Off => {
                if self.model.queue_position + 1 < self.model.queue.len() {
                    self.model.step(1);
                    self.load()
                } else {
                    Ok(())
                }
            }
        }
    }
    /// F51 (reports 666edc3c, 46be199c): a sleep turns Bluetooth off and a
    /// link drops now and then; the output then falls back to the jack, and a
    /// Play in the seconds before the headphones returned played there (since
    /// #28 out loud on the speaker). With nothing in the jack, Play waits for
    /// them instead; the output follows them when they are back.
    pub(crate) fn awaits_lost_peer(&self) -> bool {
        self.lost_peer
            .as_ref()
            .is_some_and(|lost| lost.applies(&self.model.output, self.headphones, Instant::now()))
    }
    pub(crate) fn wait_for_lost_peer(&mut self) -> bool {
        if !self.awaits_lost_peer() {
            return false;
        }
        let Some(lost) = self.lost_peer.as_mut() else {
            return false;
        };
        lost.play.get_or_insert_with(Instant::now);
        let address = lost.address.clone();
        let name = self
            .bt_state
            .devices
            .iter()
            .find(|d| d.address.eq_ignore_ascii_case(&address))
            .map(|d| d.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Bluetooth headphones".into());
        self.ui.flash(format!("Waiting for {name}…"));
        self.log.emit(
            Level::Info,
            "audio",
            "play_waits_for_bluetooth",
            "Play waits for the Bluetooth headphones just lost",
            None,
            json!({}),
        );
        self.dirty.mark_both();
        true
    }
    /// A Play that waited too long gives up; afterwards the jack (or the
    /// speaker) plays again like before.
    pub(crate) fn lost_peer_tick(&mut self) {
        let now = Instant::now();
        let Some(lost) = self.lost_peer.as_ref() else {
            return;
        };
        if lost.gave_up(now) {
            self.lost_peer = None;
            self.ui
                .flash("Bluetooth headphones didn't come back. Press Play to use the player.");
            self.log.emit(
                Level::Info,
                "audio",
                "bluetooth_wait_ended",
                "The Bluetooth headphones did not return in time",
                None,
                json!({}),
            );
            self.dirty.mark_render();
        } else if lost.expired(now) {
            self.lost_peer = None;
        }
    }
    /// A wheel click while no music plays (#67, #77): in the paused stream when it is on the
    /// output the jack calls for, else in a headphone or speaker sink the sink thread keeps (or
    /// opens) for clicks. Bluetooth never clicks; neither does an empty jack with the speaker off.
    pub(crate) fn wheel_click(&mut self) {
        let speaker = self.speaker_route();
        if !matches!(self.model.output, AudioOutput::Wired)
            || !(speaker || self.headphones == Some(true))
        {
            return;
        }
        if self.playback.is_held() && self.model.speaker_active != speaker {
            // Paused on the other output: Play opens anew anyway.
            self.release_hold();
        }
        self.playback.set_volume(self.sink_volume());
        if self.playback.is_held() {
            self.playback.click(None);
        } else if let Some(spec) = self.click_spec(speaker) {
            self.playback.click(Some(spec));
        }
    }
    /// The wired sink a click opens: planned once (a probe of the headphone output, as a start
    /// makes), on the speaker or not.
    fn click_spec(&mut self, speaker: bool) -> Option<reborn_audio::SinkSpec> {
        if self.click_spec.is_none() {
            let id = self.log.correlation();
            match reborn_audio::AlsaSink::plan(&AudioOutput::Wired, self.log.clone(), id) {
                Ok((spec, _)) => self.click_spec = Some(spec),
                Err(e) => {
                    self.log.emit(
                        Level::Warn,
                        "audio",
                        "click_plan_failed",
                        &e,
                        Some(id),
                        json!({}),
                    );
                    return None;
                }
            }
        }
        self.click_spec
            .clone()
            .map(|spec| reborn_audio::SinkSpec { speaker, ..spec })
    }
    /// The jack output plays on the internal speaker: wired, Speaker on and
    /// the kernel has seen the jack empty (unknown counts as headphones; the
    /// kernel also refuses the speaker while it reads a plug).
    pub(crate) fn speaker_route(&self) -> bool {
        matches!(self.model.output, AudioOutput::Wired)
            && self.model.settings.speaker
            && self.headphones == Some(false)
    }
    /// The jack changed (#28). As stock: headphones in move a playing
    /// speaker to them; headphones out pause what played in them.
    pub(crate) fn jack_changed(&mut self, plugged: bool) -> Result<(), String> {
        let before = self.headphones.replace(plugged);
        if before == Some(plugged) {
            return Ok(());
        }
        let active = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        let wired = matches!(self.model.output, AudioOutput::Wired);
        self.log.emit(
            Level::Info,
            "audio",
            "jack",
            if plugged {
                "Headphones plugged in"
            } else {
                "Headphones unplugged"
            },
            None,
            json!({"plugged":plugged,"playing":active,"speaker_active":self.model.speaker_active}),
        );
        match jack_move(wired, active, plugged, before, self.model.speaker_active) {
            JackMove::ToHeadphones => self.load()?,
            JackMove::Pause => {
                self.pause();
                // #62: say why the music stopped, as a lost Bluetooth peer does.
                self.ui.flash(HEADPHONES_LOST);
            }
            JackMove::Nothing if !active && self.model.playback != PlaybackState::Paused => {
                self.model.speaker_active = self.speaker_route();
            }
            JackMove::Nothing => {}
        }
        self.dirty.mark_both();
        Ok(())
    }
    pub(crate) fn switch(&mut self, out: AudioOutput) -> Result<(), String> {
        if let AudioOutput::Bluetooth(address) = &out {
            self.bt_state.playback_rate(address)?;
        }
        // The headphones a Play waited for are back: it plays there now.
        let waited = self
            .lost_peer
            .take()
            .is_some_and(|lost| lost.play.is_some() && lost.is(&out));
        let active = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        if !active {
            self.active_transport_generation = None;
        }
        let to_jack =
            matches!(out, AudioOutput::Wired) && !matches!(self.model.output, AudioOutput::Wired);
        self.model.output = out;
        if to_jack {
            self.cap_wired_volume();
        }
        self.dirty.mark_both();
        if active || waited {
            self.load()?
        }
        self.log.emit(
            Level::Info,
            "audio",
            "output_switch",
            "Output preference changed",
            Some(self.log.correlation()),
            json!({"output":self.model.output}),
        );
        self.checkpoint();
        self.dirty.mark_render();
        Ok(())
    }
}
