//! Idle work: Wi-Fi idle, automatic sleep, the dark timers, the battery and the status record.
use crate::*;

impl Runtime {
    /// Block Wi-Fi at runtime once the player has been dark and idle on battery
    /// long enough. The saved preference is untouched; wake restores the radio.
    pub(crate) fn wifi_idle_tick(&mut self) {
        let Some(since) = self.dark_since else {
            return;
        };
        if self.headless || self.wifi_idle_off || self.wifi_idle_failed {
            return;
        }
        let playing = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        let busy = self.model.platform.busy.is_some() || self.ui.wifi.scan.active();
        let on_battery =
            self.model.platform.battery.charging == reborn_core::platform::ChargingState::OnBattery;
        if !reborn_core::platform::wifi_idle_power_down(
            since.elapsed().as_secs(),
            self.wifi_state.enabled,
            playing,
            on_battery,
            busy,
        ) {
            return;
        }
        match wifi::radio(false, false) {
            Ok(()) => {
                self.wifi_idle_off = true;
                self.log.emit(
                    Level::Info,
                    "wifi",
                    "idle_power_down",
                    "Wi-Fi blocked while the player is dark and idle",
                    None,
                    json!({"dark_seconds":since.elapsed().as_secs()}),
                );
            }
            Err(error) => {
                // Do not retry every tick; the next screen-off tries again.
                self.wifi_idle_failed = true;
                self.log.emit(
                    Level::Warn,
                    "wifi",
                    "idle_power_down_failed",
                    "Wi-Fi idle power-down failed",
                    None,
                    json!({"error":error}),
                );
            }
        }
    }

    pub(crate) fn automatic_sleep_tick(&mut self) {
        // A crashed helper must not leave a stale preparation flag swallowing
        // input forever. Time asleep does not advance Instant. The actual flock
        // (not a PID file) distinguishes a live owner from an abandoned request.
        if self.sleep_prepared
            && self
                .sleep_prepared_since
                .is_some_and(|since| since.elapsed().as_secs() >= 15)
            && matches!(power::sleep_owner_present(), Ok(false))
        {
            let result = self.restore_sleep(false);
            self.model.platform.snapshot.sleep.phase =
                reborn_core::platform::SleepPhase::RestoreFailed;
            self.model.platform.snapshot.sleep.problem =
                Some(reborn_core::platform::SleepProblem::RestorationFailed);
            self.log.emit(
                Level::Error,
                "power",
                "sleep_owner_lost",
                "Sleep owner stopped before restoration",
                None,
                json!({"application_restore":result}),
            );
        }
        if self.headless || self.sleep_prepared {
            return;
        }
        let playing = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        let busy = self.model.platform.busy.is_some()
            || self.playback.has_pending_start()
            || self.pending_reconfiguration.is_some()
            || self.model.library.scanning
            || self.query.is_some();
        if playing || busy || !self.model.screen_off {
            self.sleep_attempt = Some(Instant::now());
            return;
        }
        // Headphones connected: a pause stays awake a while, so their own
        // play button can continue (asleep, the radio is off).
        let headset = self.model.playback == PlaybackState::Paused
            && matches!(&self.model.output, AudioOutput::Bluetooth(address)
            if self.bt_state.devices.iter().any(|d| {
                d.connected && d.address.eq_ignore_ascii_case(address)
            }));
        if !headset {
            self.headset_paused = None;
        } else if self.headset_paused.is_none() {
            self.headset_paused = Some(Instant::now());
        }
        if reborn_core::platform::headset_pause_awake(
            self.headset_paused.map(|since| since.elapsed().as_secs()),
        ) {
            return;
        }
        if reborn_core::platform::charger_holds_sleep(
            self.sleep_refused_for_charger,
            self.model.platform.battery.cable,
        ) {
            return;
        }
        self.sleep_refused_for_charger = false;
        if reborn_core::platform::automatic_sleep_due(
            self.model.screen_off,
            self.dark_since.map(|since| since.elapsed().as_secs()),
            self.last_activity.elapsed().as_secs(),
            self.sleep_attempt.map(|since| since.elapsed().as_secs()),
            playing,
            busy,
        ) && self.sleep_attempt.is_none_or(|at| {
            at.elapsed().as_secs()
                >= reborn_core::platform::sleep_retry_seconds(self.sleep_refusals)
        }) {
            // Platform owns fresh USB/battery/workload checks and the operation
            // locks. Preparation drains the DB and closes PCM before entry.
            let _ = self.effect(Effect::Platform(reborn_core::PlatformTask::SleepRequest));
        }
    }

    pub(crate) fn wifi_idle_restore(&mut self) {
        if !self.wifi_idle_off {
            return;
        }
        self.wifi_idle_off = false;
        if let Err(error) = wifi::radio(true, false) {
            self.log.emit(
                Level::Warn,
                "wifi",
                "idle_restore_failed",
                "Wi-Fi restore after idle failed",
                None,
                json!({"error":error}),
            );
        }
    }

    /// The screen is dark and nothing goes on: no sound, no platform task,
    /// no start, scan, reconfiguration, seek or wait for headphones in
    /// flight. The main loop and the watchdog then wait longer (`dark`).
    pub(crate) fn dark_and_quiet(&self) -> bool {
        !self.headless
            && self.model.screen_off
            && !self.sleep_prepared
            && !matches!(
                self.model.playback,
                PlaybackState::Playing | PlaybackState::Buffering
            )
            && !self.model.library.scanning
            && self.query.is_none()
            && self.pending_reconfiguration.is_none()
            && self.model.platform.busy.is_none()
            && self.queued_platform.is_none()
            && !self.playback.has_pending_start()
            && self.lost_peer.is_none()
            && self.jack_pending.is_none()
            && self.seek_until.is_none()
            && self.boot_fade == 0
    }

    /// The moments a dark, quiet loop owes a pass: the automatic sleep,
    /// Wi-Fi's idle power-down, the end of a held pause and the sleep timer.
    pub(crate) fn dark_timers(&self) -> impl Iterator<Item = Instant> {
        let sleep = self.dark_since.map(|dark| {
            dark::automatic_sleep_at(
                dark,
                self.last_activity,
                self.sleep_attempt,
                self.sleep_refusals,
                self.headset_paused,
            )
        });
        let wifi = self
            .dark_since
            .filter(|_| {
                !self.wifi_idle_off
                    && !self.wifi_idle_failed
                    && self.wifi_state.enabled
                    && self.model.platform.battery.charging
                        == reborn_core::platform::ChargingState::OnBattery
            })
            .map(|dark| dark + Duration::from_secs(reborn_core::platform::WIFI_IDLE_OFF_SECS));
        let hold = self.held_since.map(|since| since + HOLD_LIMIT);
        let timer = self.model.sleep_timer.and_then(|timer| timer.until);
        [sleep, wifi, hold, timer].into_iter().flatten()
    }

    /// Present the battery and announce a worsening low-battery level once.
    pub(crate) fn present_battery(&mut self, battery: reborn_core::platform::BatteryState) {
        let before = self.model.platform.battery;
        if before == battery {
            return;
        }
        match battery.level {
            LowBattery::Low if before.level == LowBattery::Normal => {
                self.ui.flash("Low battery. Connect a charger soon.")
            }
            LowBattery::Critical if before.level != LowBattery::Critical => self
                .ui
                .flash("Battery critically low. Connect a charger now."),
            _ => {}
        }
        self.model.platform.battery = battery;
        self.dirty.mark_render();
    }

    pub(crate) fn status(&self) -> Value {
        let (healthy, reasons) = update_health(
            self.headless,
            self.graphics.is_some(),
            self.log.uptime(),
            &self.log.health(),
        );
        json!({"healthy":healthy,"health_reasons":reasons,"version":reborn_core::BUILD_LABEL,"build_id":option_env!("REBORN_BUILD_ID").unwrap_or("development"),"session":self.log.session(),"uptime_seconds":self.log.uptime(),"current_screen":self.model.screen,"screen_off":self.model.screen_off,"playback":{"state":self.model.playback,"track_id":self.model.current().map(|t|t.id),"position_ms":self.model.position_ms,"duration_ms":self.model.current().map(|t|t.duration_ms),"queue_length":self.model.queue.len(),"queue_position":self.model.queue_position,"generation":self.model.generation},"output":self.model.output,"audio":self.playback.audio_state(),"library":{"tracks_loaded":self.model.library.tracks.len(),"schema":reborn_library::SCHEMA_VERSION,"scanning":self.model.library.scanning,"last_scan":self.model.library.last_scan,"error":self.model.library.error},"wifi":self.wifi_state,"bluetooth":self.bt_state,"storage":self.model.sources,"power":self.power,"graphics":{"available":self.graphics.is_some(),"renderer":self.graphics.as_ref().map(|g|&g.info),"headless":self.headless},"decoder":{"ffmpeg":reborn_media::version(),"runtime":reborn_media::runtime_components_if_loaded()},"buffers":{"frames":self.log.metric("audio_buffer_frames"),"milliseconds":self.log.metric("audio_buffer_ms")}})
    }
    pub(crate) fn snapshot(&self) -> Value {
        self.snapshot_with(self.status())
    }
    /// The snapshot around a `status()` the caller made (and uses again).
    pub(crate) fn snapshot_with(&self, status: Value) -> Value {
        json!({"status":status,"health":self.log.health(),"metrics":self.log.metrics(),"recent_errors":self.log.events(20,None,Some(Level::Warn),None),"resource_usage":fs::read_to_string("/proc/self/status").unwrap_or_default(),"kernel_events":self.kernel_events.lock().map(|v|v.clone()).unwrap_or_default()})
    }
    pub(crate) fn play_control(&mut self, a: PlaybackAction) -> Result<(), String> {
        self.with_model_action(|runtime| runtime.play_control_inner(a))
    }
    pub(crate) fn play_control_inner(&mut self, a: PlaybackAction) -> Result<(), String> {
        self.dirty.mark_render();
        match a {
            PlaybackAction::Play(id) => {
                let index = self
                    .model
                    .library
                    .tracks
                    .iter()
                    .position(|t| t.id == id)
                    .ok_or("track id not in library")?;
                self.effect(Effect::Play(index))
            }
            PlaybackAction::Pause => {
                self.pause();
                Ok(())
            }
            PlaybackAction::Resume => {
                if self.resume_in_place() {
                    Ok(())
                } else {
                    self.load()
                }
            }
            PlaybackAction::Stop => {
                self.model.invalidate();
                self.playback.stop(self.model.generation);
                self.model.playback = PlaybackState::Stopped;
                self.model.position_ms = 0;
                self.dirty.mark_both();
                self.checkpoint();
                Ok(())
            }
            PlaybackAction::Next => self.effect(Effect::NextTrack),
            PlaybackAction::Previous => self.effect(Effect::PreviousTrack),
            PlaybackAction::Seek(ms) => {
                self.model.position_ms = ms.min(
                    self.model
                        .current()
                        .map(|t| t.duration_ms.saturating_sub(1))
                        .unwrap_or(0),
                );
                self.dirty.mark_both();
                if self.model.playback == PlaybackState::Playing {
                    self.load()
                } else {
                    self.release_hold();
                    Ok(())
                }
            }
            PlaybackAction::Wired => self.switch(AudioOutput::Wired),
            PlaybackAction::Bluetooth(s) => self.switch(AudioOutput::Bluetooth(s)),
            PlaybackAction::Volume(v) => {
                // No one to ask here: never above the agreed wired level.
                let limit = if self.wired_sink() {
                    self.model.settings.wired_volume_limit()
                } else {
                    100
                };
                self.model.settings.volume = v.min(limit.max(self.model.settings.volume)).min(100);
                self.dirty.mark_both();
                self.playback.set_volume(self.sink_volume());
                self.send_bt_volume();
                Ok(())
            }
        }
    }
}
