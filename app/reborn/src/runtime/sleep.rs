//! Sleep and wake, a lost Bluetooth peer, the wired route and the report question.
use crate::*;

impl Runtime {
    /// The selected Bluetooth peer is gone: back to the jack, paused.
    pub(crate) fn bluetooth_peer_lost(&mut self, recovery: &str) {
        self.abandon_pending_reconfiguration();
        if let AudioOutput::Bluetooth(address) = self.model.output.clone() {
            self.lost_peer = Some(LostPeer::new(address.clone(), Instant::now()));
            self.model.apply(Event::BluetoothDisconnected(address));
        }
        self.pause();
        self.ui.flash("Bluetooth disconnected. Playback paused.");
        self.log.emit(
            Level::Warn,
            "bluetooth",
            "transport_lost",
            "Selected audio peer disconnected",
            None,
            json!({"recovery":recovery}),
        );
        self.dirty.mark_render();
    }
    pub(crate) fn prepare_sleep(&mut self) -> Result<Value, String> {
        if self.sleep_prepared {
            return Err("sleep_operation_in_progress".into());
        }
        // Power turned the screen off with the request; lit again now means a
        // second press woke it. Stay awake instead of blanking it once more.
        let requested = self
            .sleep_requested_at
            .take()
            .is_some_and(|at| at.elapsed() < Duration::from_secs(30));
        if requested && !self.model.screen_off {
            return Err("wake_event_in_progress".into());
        }
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            return Err("playback_active".into());
        }
        if self.playback.has_pending_start()
            || self.pending_reconfiguration.is_some()
            || self.model.library.scanning
            || self.query.is_some()
        {
            return Err("sleep_preparation_busy".into());
        }
        if !self.background.quiesce() {
            self.background.update(
                &self.model.settings.music_directory,
                &self.model.sources,
                false,
            );
            return Err("sleep_preparation_busy".into());
        }
        let scan_ready = self.scanner.quiesce();
        let art_ready = self.thumbs.as_mut().is_none_or(|worker| worker.quiesce());
        if !scan_ready || !art_ready {
            self.scanner.resume();
            self.background.update(
                &self.model.settings.music_directory,
                &self.model.sources,
                false,
            );
            return Err("sleep_preparation_busy".into());
        }
        let prepared = (|| {
            self.db.prepare_sleep(Duration::from_secs(2))?;
            self.model.platform.snapshot.sleep.phase =
                reborn_core::platform::SleepPhase::SleepPreparing;
            self.model
                .checkpoint(&self.root.join("state/session.json"))
                .map_err(|e| e.to_string())?;
            self.held_since = None;
            self.seek_until = None;
            self.playback.stop_and_wait(self.model.generation)?;
            self.effect_inner(Effect::ScreenSleep)?;
            Ok::<(), String>(())
        })();
        if let Err(error) = prepared {
            self.scanner.resume();
            self.background.update(
                &self.model.settings.music_directory,
                &self.model.sources,
                false,
            );
            return Err(error);
        }
        self.sleep_prepared = true;
        self.sleep_prepared_since = Some(Instant::now());
        Ok(json!({"sleep_ready":true,"screen_off":self.model.screen_off}))
    }

    pub(crate) fn restore_sleep(&mut self, dark: bool) -> Result<Value, String> {
        // A failed display restore must also discard the wake gesture before
        // input is accepted again, so its release cannot request another sleep.
        self.input_resume = true;
        self.sleep_prepared = false;
        self.sleep_prepared_since = None;
        self.scanner.resume();
        self.background.update(
            &self.model.settings.music_directory,
            &self.model.sources,
            false,
        );
        self.model.platform.snapshot.sleep.phase = reborn_core::platform::SleepPhase::Waking;
        if dark {
            // F47: the sleep was refused with nobody waking the player; the
            // screen stays off (it lit every two minutes for hours on a player
            // whose kernel refused every sleep) and the next try waits longer.
            self.sleep_refusals = self.sleep_refusals.saturating_add(1);
            self.sleep_attempt = Some(Instant::now());
            self.log.emit(
                Level::Info,
                "power",
                "sleep_refused_dark",
                "Sleep refused after preparation; the screen stays off",
                None,
                json!({"refusals":self.sleep_refusals,
                    "retry_s":reborn_core::platform::sleep_retry_seconds(self.sleep_refusals)}),
            );
            return Ok(json!({"sleep_ready":true,"screen_off":self.model.screen_off}));
        }
        self.sleep_refusals = 0;
        self.effect_inner(Effect::ScreenWake)?;
        if let Some(graphics) = self.graphics.as_mut() {
            let presented = graphics.render(&self.ui.draw(
                &self.model,
                &self.model.library.tracks,
                self.art,
            ));
            if let Err(error) = presented {
                // Not retried here: a screen off/on can block for seconds
                // (flip_done timeouts) and the platform's sleep_restore call
                // gives up after 5 s. The main loop's next frame tries it.
                self.dirty.mark_render();
                return Err(error);
            }
            self.graphics_recovery.presented();
        } else if !self.headless {
            return Err("display_restoration_failed".into());
        }
        self.playback_hints.clear();
        self.last_activity = Instant::now();
        self.dirty.mark_render();
        Ok(json!({"sleep_ready":true,"screen_off":self.model.screen_off}))
    }

    /// The first frames after a wake can fail when the display link came back
    /// lagging (report 51f3b06d: vblank timeouts, presentation -16, a dark
    /// screen); the next screen off/on cleared it there. That cycle is tried
    /// once per wake before the frame counts as failed.
    pub(crate) fn retry_after_wake(&mut self, error: &str) -> bool {
        let Some(woke) = self.woke_at.take() else {
            return false;
        };
        if woke.elapsed() > Duration::from_secs(30) {
            return false;
        }
        let draw = self
            .ui
            .draw(&self.model, &self.model.library.tracks, self.art);
        let Some(graphics) = self.graphics.as_mut() else {
            return false;
        };
        let result = graphics
            .sleep(true)
            .and_then(|()| graphics.sleep(false))
            .and_then(|()| graphics.render(&draw));
        self.log.emit(
            if result.is_ok() {
                Level::Info
            } else {
                Level::Warn
            },
            "graphics",
            "wake_presentation_retry",
            "Screen off/on after a failed frame following a wake",
            None,
            json!({"error":error,"recovery_attempted":true,"recovered":result.is_ok(),
                "retry_error":result.as_ref().err()}),
        );
        if result.is_ok() {
            self.graphics_recovery.presented();
            self.dirty.mark_render();
        }
        result.is_ok()
    }

    pub(crate) fn recreate_graphics(&mut self) -> Result<(), String> {
        self.art = false;
        // A new renderer starts without Cover Flow's full-resolution cover.
        self.ui.collection_art.set(None);
        if let Some(worker) = &mut self.thumbs {
            worker.forget_failures();
        }
        self.graphics = Some(Renderer::open(&reborn_ui::atlases(), self.log.clone())?);
        if let Some(skin) = self.ui.skin.clone() {
            self.upload_skin(&skin);
        }
        self.dirty.mark_render();
        Ok(())
    }

    /// Send the wired level and DAC filter to the audio layer (kept for every
    /// wired stream; applied now where the amplifier is down).
    pub(crate) fn configure_wired(&self) {
        if self.headless {
            return;
        }
        let s = &self.model.settings;
        match reborn_audio::wired_configure(s.hp_level, s.dac_filter) {
            Ok(levels) => self.log.emit(
                Level::Info,
                "audio",
                "wired_configured",
                "wired headphone level and DAC filter",
                None,
                json!({"hp_level":s.hp_level,"dac_filter":s.dac_filter,"level_control":levels}),
            ),
            Err(e) => self.log.emit(
                Level::Warn,
                "audio",
                "wired_configure_failed",
                &e,
                None,
                json!({"hp_level":s.hp_level,"dac_filter":s.dac_filter}),
            ),
        }
    }

    /// Error reports (ERROR-REPORTS-PLAN): asked once, after the update that
    /// brought them; "No Thanks" is an answer too. Only on a lit screen, with
    /// nothing else open and no button pressed for a while, so a scroll or a
    /// press already under way can't answer it.
    pub(crate) fn ask_about_reports(&mut self) {
        if self.asked_reports
            || self.model.screen_off
            || self.model.navigation.modal.is_some()
            || self.last_activity.elapsed() < Duration::from_secs(5)
            || !self
                .model
                .platform
                .snapshot
                .reports
                .as_ref()
                .is_some_and(|r| r.upload && r.enabled.is_none())
            || matches!(
                self.model.platform.snapshot.internal_setup,
                reborn_core::platform::InternalSetup::Waiting { .. }
            )
        {
            return;
        }
        self.asked_reports = true;
        self.model.navigation.modal = Some(reborn_core::Modal::Confirm(
            reborn_core::ConfirmAction::ReportsQuestion,
        ));
        self.model.navigation.modal_focus = 0;
        self.dirty.mark_both();
    }
}
