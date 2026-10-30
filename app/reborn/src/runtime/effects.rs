//! What the UI asks for (`Effect`) and the DSP reload.
use crate::*;

impl Runtime {
    pub(crate) fn effect(&mut self, e: Effect) -> Result<(), String> {
        // Holding Previous/Next seeks every 90 ms: steps are collected and
        // applied once, so they never meet a restart still in progress.
        if let Effect::Seek(direction) = e {
            if let Some(length) = self
                .model
                .current()
                .filter(|t| !t.is_radio())
                .map(|t| t.duration_ms)
            {
                let now = Instant::now();
                let start = match self.seek_run {
                    Some((start, last)) if now.duration_since(last) < SEEK_SETTLE => start,
                    _ => now,
                };
                self.seek_run = Some((start, now));
                let held = now.duration_since(start);
                let step = if self.model.book_part().is_some() {
                    books::seek_step(held)
                } else {
                    seek_step(held, length)
                } as i64;
                self.seek_by(direction.signum() * step);
                return Ok(());
            }
        }
        // While a song is still starting (about a second on Bluetooth),
        // Previous/Next move on at once and the restart follows when the
        // start in progress ends; Play/Pause pauses. Beta 6 refused these
        // ("Couldn't complete that").
        if self.pending_reconfiguration.is_some() {
            match e {
                Effect::NextTrack | Effect::PreviousTrack => {
                    let delta = if matches!(e, Effect::NextTrack) {
                        1
                    } else {
                        -1
                    };
                    let target = self.model.queue_position as i64 + delta;
                    let file = !self.model.current().is_some_and(Track::is_radio);
                    if file && (0..self.model.queue.len() as i64).contains(&target) {
                        self.model.step(delta as i32);
                        self.playback.hold(true);
                        self.seek_until = Some(Instant::now());
                        self.dirty.mark_both();
                    }
                    return Ok(());
                }
                Effect::TogglePlayback => {
                    // A song change keeps the chosen song, paused; an output
                    // or codec change still goes back to what worked.
                    let song_change = self.pending_reconfiguration.as_ref().is_some_and(|p| {
                        p.previous.output == self.model.output
                            && serde_json::to_value(&p.previous.settings).ok()
                                == serde_json::to_value(&self.model.settings).ok()
                    });
                    if song_change {
                        self.pending_reconfiguration = None;
                    }
                    self.pause();
                    return Ok(());
                }
                _ => {}
            }
        }
        if matches!(
            e,
            Effect::None
                | Effect::Platform(_)
                | Effect::WifiPower
                | Effect::WifiScan
                | Effect::WifiConnect { .. }
                | Effect::WifiSaved(_)
                | Effect::WifiForget(_)
                | Effect::WifiDisconnect
                | Effect::BluetoothPower
                | Effect::BluetoothScan
                | Effect::ConfirmPairing(_)
                | Effect::ScreenSleep
                | Effect::ScreenWake
                | Effect::SetScreenTimeout(_)
        ) {
            return self.effect_inner(e);
        }
        self.with_model_action(|runtime| runtime.effect_inner(e))
    }
    pub(crate) fn effect_inner(&mut self, e: Effect) -> Result<(), String> {
        if self.sleep_prepared && !matches!(e, Effect::None) {
            return Err("sleep_operation_in_progress".into());
        }
        self.dirty.mark_render();
        // Turning Bluetooth off, disconnecting or choosing an output by hand
        // decides where audio goes; nothing waits for lost headphones then.
        if matches!(
            e,
            Effect::BluetoothPower | Effect::BluetoothDevice { .. } | Effect::Output(_)
        ) {
            self.lost_peer = None;
        }
        match e {
            Effect::None => {}
            Effect::Platform(task) => {
                if task == reborn_core::PlatformTask::SleepRequest
                    && !matches!(
                        self.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    )
                {
                    // F48: Power turns the screen off at once, as on stock. It
                    // stayed lit ≈1.7 s until the platform prepared, people
                    // pressed again, and that press cancelled the sleep.
                    self.effect_inner(Effect::ScreenSleep)?;
                    self.sleep_requested_at = Some(Instant::now());
                }
                if self.model.platform.busy.is_some() {
                    self.queue_platform(task);
                    return Ok(());
                }
                if task == reborn_core::PlatformTask::InternalStorageSetup
                    && matches!(
                        self.model.platform.snapshot.usb,
                        reborn_core::platform::UsbTransfer::Ready
                            | reborn_core::platform::UsbTransfer::Copying
                            | reborn_core::platform::UsbTransfer::Starting
                    )
                {
                    return Err("Unplug from the computer first".into());
                }
                if task == reborn_core::PlatformTask::SdRepair {
                    // The card is unmounted for the repair: nothing may hold a
                    // song or its folder open on it.
                    let on_card = self.model.current().is_some_and(|t| {
                        self.model.sources.iter().any(|s| {
                            s.id == t.source_id
                                && matches!(s.kind, reborn_core::MediaSource::SdCard(_))
                        })
                    });
                    if on_card {
                        if self.model.playback == PlaybackState::Playing {
                            self.pause();
                        }
                        self.release_hold_with(true);
                        self.playback.stop(self.model.generation);
                    }
                }
                if task == reborn_core::PlatformTask::SleepRequest {
                    self.sleep_attempt = Some(Instant::now());
                    use reborn_core::platform::{SleepPhase, SleepProblem, SleepState};
                    if matches!(
                        self.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    ) {
                        self.model.platform.snapshot.sleep = SleepState {
                            phase: SleepPhase::SleepRefused,
                            problem: Some(SleepProblem::PlaybackActive),
                            ..Default::default()
                        };
                        return Ok(());
                    }
                    // A pause held in place keeps the sink, and its activity
                    // lease, open: give it back first so the platform's lock
                    // check passes (Play then restarts at the same place).
                    self.release_hold_with(true);
                    self.checkpoint();
                }
                let worker = self.platform.as_ref().ok_or("System service is starting")?;
                if worker.commands.try_send(task).is_err() {
                    // An observation runs and another waits: take the next turn.
                    if task == reborn_core::PlatformTask::Refresh {
                        return Err("busy: another operation is still running".into());
                    }
                    self.queue_platform(task);
                    return Ok(());
                }
                if matches!(
                    task,
                    reborn_core::PlatformTask::UpdateStage | reborn_core::PlatformTask::UpdateApply
                ) {
                    self.log.emit(
                        Level::Info,
                        "update",
                        "task_started",
                        "Update task sent to the platform",
                        None,
                        json!({"task":format!("{task:?}")}),
                    );
                }
                self.model.platform.busy = Some(task);
                if task == reborn_core::PlatformTask::SleepRequest {
                    self.model.platform.snapshot.sleep.phase =
                        reborn_core::platform::SleepPhase::SleepRequested;
                }
                if task != reborn_core::PlatformTask::Refresh {
                    self.model.platform.failure = None;
                    self.model.platform.result = None;
                }
                self.dirty.mark_render();
            }
            Effect::StopPlayback => {
                self.pause();
                self.model.playback = PlaybackState::Stopped;
            }
            Effect::CancelDownload => client::abort_download(),
            Effect::Radio(request) => self.radio(request)?,
            Effect::LastFm(request) => self.lastfm_request(request)?,
            Effect::SetScrobbleLog(on) => {
                self.model.settings.scrobble_log = on;
                self.checkpoint();
            }
            Effect::SetLastFmScrobble(on) => {
                self.model.settings.lastfm_scrobble = on;
                self.checkpoint();
            }
            Effect::SetLastFmNowPlaying(on) => {
                self.model.settings.lastfm_now_playing = on;
                self.checkpoint();
            }
            Effect::Playlist(request) => self.playlist(*request)?,
            Effect::SetCodecPreference(preference) => {
                if !codec_change_allowed(
                    self.model.playback,
                    self.pending_reconfiguration.is_some(),
                ) {
                    return Err("Pause music before changing codec".into());
                }
                let address = self
                    .bt_state
                    .devices
                    .iter()
                    .find(|d| d.connected && d.audio)
                    .map(|d| d.address.clone())
                    .ok_or("Connect Bluetooth headphones first")?;
                self.bluetooth
                    .as_ref()
                    .ok_or("Bluetooth unavailable")?
                    .commands
                    .try_send(bluetooth::Command::Codec {
                        address,
                        preference,
                    })
                    .map_err(|_| "Bluetooth busy")?;
                self.model.settings.codec_preference = preference;
                self.dirty.mark_both();
                self.ui
                    .flash("Codec preference saved. It applies when the device reconnects audio.");
            }
            Effect::WifiDisconnect => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Disconnect)
                .map_err(|e| e.to_string())?,
            Effect::Play(index) => self.play_index(index, ShuffleMode::Off)?,
            Effect::PlayShuffled(index) => self.play_index(index, ShuffleMode::Songs)?,
            Effect::PlayCollection {
                selected,
                members,
                shuffle,
                source,
                origin,
            } => self.play_members(selected, members, shuffle, source, origin)?,
            Effect::PlayQueue(index) => {
                if !self.model.jump_to(index) {
                    return Err("invalid queue entry".into());
                }
                self.load()?;
            }
            Effect::PlayNow(index) => {
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
            }
            Effect::PlayNext(index) => self.queue_songs(vec![index], true)?,
            Effect::PlayNextCollection(members) => self.queue_songs(members, true)?,
            Effect::AddToQueue(index) => self.queue_songs(vec![index], false)?,
            Effect::AddToQueueCollection(members) => self.queue_songs(members, false)?,
            Effect::TogglePlayback => self.toggle()?,
            Effect::NextTrack | Effect::PreviousTrack => {
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
                    && (self.model.position_ms > RESTART_SONG_AFTER_MS
                        || self.model.queue_position == 0)
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
            }
            // A live station has nothing to seek in.
            Effect::Seek(_) if self.model.current().is_some_and(Track::is_radio) => {}
            Effect::Seek(delta) => self.seek_by(delta),
            Effect::AdjustVolume(delta) => {
                let now = self.model.settings.volume;
                let mut volume = (now as i16 + delta as i16).clamp(0, 100) as u8;
                let limit = self.model.settings.wired_volume_limit();
                if self.wired_sink() && delta > 0 && volume > limit {
                    // Up to the safe level, then ask (never lower here).
                    volume = now.max(limit);
                    if self.model.navigation.modal.is_none() {
                        self.model.navigation.modal = Some(reborn_core::Modal::Confirm(
                            reborn_core::ConfirmAction::LoudVolume,
                        ));
                        self.model.navigation.modal_focus = 0;
                        self.dirty.mark_both();
                    }
                }
                self.model.settings.volume = volume;
                self.playback.set_volume(self.sink_volume());
                self.send_bt_volume();
                self.ui.show_volume();
                self.dirty.mark_render();
                // A hold may adjust every 90 ms. Keep the atomic audio target
                // immediate; persist on the existing bounded checkpoint and
                // shutdown boundaries instead of fsyncing each step.
                self.dirty.mark_persistence();
            }
            Effect::ScanLibrary => {
                if self.model.library.scanning {
                    return Ok(());
                }
                self.model.apply(Event::LibraryScanStarted);
                // Asked for: every folder is read again (a tag edit made on a
                // computer may leave the folder and the free space unchanged).
                if let Err(error) = self.scanner.scan_all(self.model.sources.clone()) {
                    self.model.apply(Event::LibraryScanFailed(error.clone()));
                    return Err(error);
                }
                self.ui.flash("Scanning for music");
            }
            Effect::WifiPower => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi service unavailable")?
                .commands
                .try_send(wifi::Command::Power(!self.wifi_state.enabled))
                .map_err(|_| "Wi-Fi busy")?,
            Effect::WifiScan => {
                if self.ui.wifi.scan.active() {
                    return Ok(());
                }
                self.wifi
                    .as_ref()
                    .ok_or("Wi-Fi unavailable")?
                    .commands
                    .try_send(wifi::Command::Scan)
                    .map_err(|_| "Wi-Fi busy")?;
                self.ui.wifi.scan = reborn_core::RadioScan::Starting;
                self.ui.wifi.problem = None;
                self.wifi_state.scan = reborn_core::RadioScan::Starting;
            }
            Effect::WifiConnect { ssid, password } => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Connect { ssid, password })
                .map_err(|_| "Wi-Fi busy")?,
            Effect::WifiSaved(id) => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Saved(id))
                .map_err(|_| "Wi-Fi busy")?,
            Effect::WifiForget(id) => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Forget(id))
                .map_err(|_| "Wi-Fi busy")?,
            Effect::BluetoothPower => self
                .bluetooth
                .as_ref()
                .ok_or("Bluetooth unavailable")?
                .commands
                .try_send(bluetooth::Command::Power(!self.bt_state.powered))
                .map_err(|_| "Bluetooth busy")?,
            Effect::BluetoothScan => {
                if self.ui.bluetooth.scan.active() {
                    return Ok(());
                }
                self.bluetooth
                    .as_ref()
                    .ok_or("Bluetooth unavailable")?
                    .commands
                    .try_send(bluetooth::Command::Scan(true))
                    .map_err(|_| "Bluetooth busy")?;
                self.ui.bluetooth.scan = reborn_core::RadioScan::Starting;
                self.ui.bluetooth.problem = None;
                self.bt_state.scan = reborn_core::RadioScan::Starting;
            }
            Effect::BluetoothDevice { path, operation } => {
                if operation == "output" {
                    let address = self
                        .bt_state
                        .devices
                        .iter()
                        .find(|d| d.path == path)
                        .ok_or("device not found")?
                        .address
                        .clone();
                    self.switch(AudioOutput::Bluetooth(address))?;
                } else {
                    let cmd = match operation.as_str() {
                        "pair" => bluetooth::Command::Pair(path),
                        "connect" => bluetooth::Command::Connect(path),
                        "disconnect" => bluetooth::Command::Disconnect(path),
                        "forget" => bluetooth::Command::Forget(path),
                        _ => return Err("unsupported Bluetooth action".into()),
                    };
                    self.bluetooth
                        .as_ref()
                        .ok_or("Bluetooth unavailable")?
                        .commands
                        .try_send(cmd)
                        .map_err(|_| "Bluetooth busy")?;
                }
            }
            Effect::Output(o) => self.switch(o)?,
            Effect::ConfirmPairing(ok) => self
                .bluetooth
                .as_ref()
                .ok_or("Bluetooth unavailable")?
                .commands
                .try_send(bluetooth::Command::Confirm(ok))
                .map_err(|_| "Bluetooth busy")?,
            Effect::ScreenSleep => {
                self.interaction_hints.clear();
                if self.model.screen_off {
                    return Ok(());
                }
                let mut dac_hold = None;
                if !self.headless {
                    power::blank(true)?;
                    if let Some(graphics) = self.graphics.as_mut() {
                        if let Err(error) = graphics.sleep(true) {
                            let _ = power::blank(false);
                            return Err(error);
                        }
                    }
                    dac_hold = hold_dac(&self.log, false);
                }
                self.model.apply(Event::ScreenSleep);
                self.dark_since = Some(Instant::now());
                self.wifi_idle_failed = false;
                self.log.emit(
                    Level::Info,
                    "power",
                    "screen_blank",
                    "Display put to sleep",
                    None,
                    json!({"playback":self.model.playback,"dac_hold":dac_hold}),
                );
            }
            Effect::ScreenWake => {
                if !self.model.screen_off && (self.headless || self.graphics.is_some()) {
                    return Ok(());
                }
                let mut dac_hold = None;
                if !self.headless {
                    self.graphics_recovery.wake();
                    if self.graphics.is_none() {
                        self.recreate_graphics()?;
                    }
                    if let Some(graphics) = self.graphics.as_mut() {
                        graphics.sleep(false)?;
                    }
                    if let Err(error) = power::blank(false) {
                        if let Some(graphics) = self.graphics.as_mut() {
                            let _ = graphics.sleep(true);
                        }
                        return Err(error);
                    }
                    dac_hold = hold_dac(&self.log, true);
                }
                self.model.apply(Event::ScreenWake);
                self.woke_at = Some(Instant::now());
                // Sleep turned Bluetooth off; the headphones return after the wake.
                if let Some(lost) = self.lost_peer.as_mut() {
                    lost.since = Instant::now();
                }
                // Awake again: a sleep still waiting for its turn is dropped.
                if self.queued_platform == Some(reborn_core::PlatformTask::SleepRequest) {
                    self.queued_platform = None;
                }
                self.last_activity = Instant::now();
                self.dark_since = None;
                self.wifi_idle_restore();
                self.log.emit(
                    Level::Info,
                    "power",
                    "screen_wake",
                    "Display woke",
                    None,
                    json!({"playback":self.model.playback,"dac_hold":dac_hold}),
                );
            }
            Effect::SetReplayGain(mode) => {
                self.model.settings.replay_gain = mode;
                self.reload_dsp()?;
            }
            Effect::SetEqEnabled(enabled) => {
                if self.model.settings.eq_bands.is_empty() {
                    self.model.settings.eq_bands = reborn_core::flat_eq_bands();
                }
                self.model.settings.eq_enabled = enabled;
                self.model.settings.eq_profile_on = false;
                self.reload_dsp()?;
            }
            Effect::SetEqBandGain { index, gain_db } => {
                if self.model.settings.eq_bands.is_empty() {
                    self.model.settings.eq_bands = reborn_core::flat_eq_bands();
                }
                let band = self
                    .model
                    .settings
                    .eq_bands
                    .get_mut(index)
                    .filter(|_| index < 8)
                    .ok_or("That equalizer band has changed")?;
                band.gain_db = f32::from(gain_db.clamp(-12, 12));
                // Editing the bands means listening to them, not the profile.
                if self.model.settings.eq_enabled || self.model.settings.eq_profile_on {
                    self.model.settings.eq_profile_on = false;
                    self.reload_dsp()?;
                } else {
                    self.checkpoint();
                }
            }
            Effect::SetEqPreset(gains) => {
                let mut bands = reborn_core::flat_eq_bands();
                for (band, gain) in bands.iter_mut().zip(gains) {
                    band.gain_db = f32::from(gain.clamp(-12, 12));
                }
                self.model.settings.eq_bands = bands;
                self.model.settings.eq_enabled = gains.iter().any(|g| *g != 0);
                self.model.settings.eq_profile_on = false;
                self.reload_dsp()?;
            }
            Effect::ResetEq => {
                self.model.settings.eq_bands = reborn_core::flat_eq_bands();
                self.reload_dsp()?;
            }
            Effect::ScanEqProfiles => {
                self.ui.eq_profiles = scan_eq_profiles(&shipped_eq_dirs(), &self.storage_roots());
                self.dirty.mark_render();
            }
            Effect::ImportEqProfile(path) => match import_eq_profile(&path) {
                Ok(profile) => {
                    let name = profile.name.clone();
                    let preamp = profile.preamp_db;
                    self.log.emit(
                        Level::Info,
                        "audio",
                        "eq_profile_imported",
                        "headphone profile imported",
                        None,
                        json!({"name":name,"filters":profile.bands.len(),"preamp_db":preamp}),
                    );
                    self.model.settings.eq_profile = Some(profile);
                    self.model.settings.eq_profile_on = true;
                    self.ui
                        .flash(format!("Playing {name} · Preamp {preamp:+.1} dB"));
                    self.reload_dsp()?;
                }
                Err(e) => self.ui.flash(e),
            },
            // The level in use again: nothing to restart.
            Effect::SetHpLevel(level) if level == self.model.settings.hp_level => {}
            Effect::SetHpLevel(level) => {
                let from = self.model.settings.hp_level;
                let raised = level.index() > from.index();
                self.model.settings.hp_level = level;
                // The wired volume moves so the output stays as loud (a
                // lower level may not reach it: then it plays quieter).
                // Bluetooth without its own volume shares `volume`: left alone.
                let wired_sink = self.wired_sink();
                let s = &mut self.model.settings;
                match s.wired_volume {
                    Some(wired) => s.wired_volume = Some(s.volume_for_level(wired, from)),
                    None if wired_sink => {
                        let before = s.volume;
                        s.volume = s.volume_for_level(before, from);
                        // Quieter at once. A higher number waits for the new
                        // stream at the lower level: the start sets it only
                        // after the old one is released.
                        if s.volume < before {
                            self.playback.set_volume(self.sink_volume());
                        }
                    }
                    None => {}
                }
                self.configure_wired();
                // The amplifier takes a new level only when it powers up:
                // restart wired playback (a higher level fades in); a held
                // pause gives its stream back, so Play starts a new one.
                if matches!(self.model.output, AudioOutput::Wired) {
                    self.reload_dsp()?;
                } else {
                    self.checkpoint();
                }
                if raised {
                    self.ui.flash("Higher level · Volume lowered to match");
                }
            }
            Effect::AllowLoudVolume => {
                self.model.settings.loud_ok = true;
                self.model.settings.loud_seconds = 0;
                self.log.emit(
                    Level::Info,
                    "audio",
                    "loud_volume_allowed",
                    "listener agreed to wired volumes above the safe level",
                    None,
                    json!({"hp_level":self.model.settings.hp_level,"volume":self.model.settings.volume}),
                );
                self.checkpoint();
                // The step that was held back.
                self.effect(Effect::AdjustVolume(2))?;
            }
            Effect::SetDacFilter(filter) => {
                self.model.settings.dac_filter = filter;
                self.configure_wired();
                self.checkpoint();
            }
            Effect::SetBrightness(percent) => {
                self.model.settings.brightness = percent.clamp(10, 100);
                if !self.headless {
                    power::set_brightness(self.model.settings.brightness)?;
                }
                self.checkpoint();
            }
            Effect::ResetSettings => {
                // The Last.fm sign-in is a setting too: forget it and its plays.
                self.lastfm_forget();
                // Volume and the music location are part of the session, not
                // preferences a reset should surprise the listener with.
                let defaults = reborn_core::Settings {
                    volume: self.model.settings.volume,
                    wired_volume: self.model.settings.wired_volume,
                    bluetooth_absolute_volume: self.model.settings.bluetooth_absolute_volume,
                    loud_ok: self.model.settings.loud_ok,
                    loud_seconds: self.model.settings.loud_seconds,
                    music_directory: self.model.settings.music_directory.clone(),
                    ..Default::default()
                };
                let reload = self.model.settings.gapless_enabled != defaults.gapless_enabled
                    || self.model.settings.eq_enabled
                    || self.model.settings.eq_profile_on
                    || self.model.settings.hp_level != defaults.hp_level
                    || self.model.settings.replay_gain != defaults.replay_gain
                    || self.model.settings.crossfade_ms != defaults.crossfade_ms
                    || self.model.settings.repeat != defaults.repeat;
                self.model.settings = defaults;
                self.configure_wired();
                if self.ui.skin.is_some() {
                    let _ = self.apply_theme("", false);
                }
                if !self.headless && self.model.platform.brightness_available {
                    let _ = power::set_brightness(self.model.settings.brightness);
                }
                if reload {
                    self.reload_dsp()?;
                } else {
                    self.checkpoint();
                }
                self.ui.flash("Settings reset");
            }
            Effect::ForgetAllWifi => {
                let worker = self.wifi.as_ref().ok_or("Wi-Fi unavailable")?;
                let ids: Vec<u32> = self
                    .wifi_state
                    .saved
                    .iter()
                    .filter_map(|n| n.saved_id)
                    .collect();
                let queued = ids
                    .iter()
                    .filter(|id| {
                        worker
                            .commands
                            .try_send(wifi::Command::Forget(**id))
                            .is_ok()
                    })
                    .count();
                self.ui.flash(if queued == ids.len() {
                    "Wi-Fi networks forgotten"
                } else {
                    "Some networks are still saved. Try again."
                });
            }
            Effect::ForgetAllBluetooth => {
                let worker = self.bluetooth.as_ref().ok_or("Bluetooth unavailable")?;
                let paths: Vec<String> = self
                    .bt_state
                    .devices
                    .iter()
                    .filter(|d| d.paired)
                    .map(|d| d.path.clone())
                    .collect();
                let queued = paths
                    .iter()
                    .filter(|path| {
                        worker
                            .commands
                            .try_send(bluetooth::Command::Forget((*path).clone()))
                            .is_ok()
                    })
                    .count();
                self.ui.flash(if queued == paths.len() {
                    "Bluetooth pairings removed"
                } else {
                    "Some pairings remain. Try again."
                });
            }
            Effect::ClearCache => {
                let removed = clear_cache(&self.root.join("cache"));
                self.art = false;
                self.dirty.mark_render();
                self.ui.flash(if removed.is_ok() {
                    "Cache cleared"
                } else {
                    "Couldn't clear everything. Try again."
                });
            }
            Effect::SetBalance(balance) => {
                self.model.settings.balance =
                    balance.clamp(-reborn_core::MAX_BALANCE, reborn_core::MAX_BALANCE);
                self.playback.set_balance(self.model.settings.balance);
                self.dirty.mark_both();
                self.checkpoint();
            }
            Effect::SetCrossfade(milliseconds) => {
                self.model.settings.crossfade_ms = milliseconds.min(30_000);
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::SetGapless(enabled) => {
                self.model.settings.gapless_enabled = enabled;
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::ScanThemes => self.scan_themes(),
            Effect::SetTheme(id) => match self.apply_theme(&id, true) {
                Ok(()) => {
                    self.model.settings.theme = id;
                    self.checkpoint();
                }
                Err(e) => self.ui.flash(e),
            },
            Effect::SetAlbumView(view) => {
                self.model.settings.album_view = view;
                self.checkpoint();
            }
            Effect::SetAlbumOrder(order) => {
                self.model.settings.album_order = order;
                self.checkpoint();
            }
            Effect::SetNowPlayingWheel(wheel) => {
                self.model.settings.now_playing_wheel = wheel;
                self.checkpoint();
            }
            Effect::SetArtistsList(list) => {
                self.model.settings.artists_list = list;
                self.checkpoint();
            }
            Effect::SetWrapAround(wrap) => {
                self.model.settings.wrap_around = wrap;
                self.checkpoint();
            }
            Effect::PlayBook(key, start) => self.start_book(&key, start)?,
            Effect::SetSleepTimer(choice) => self.set_sleep_timer(choice),
            Effect::SetBookSpeed(percent) => {
                let speed = f32::from(percent.clamp(50, 200)) / 100.;
                if let Some(book) = &mut self.model.book {
                    book.speed = speed;
                }
                // Applied at once, from where it is (the place is kept).
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                } else if self.playback.is_held() {
                    self.release_hold();
                }
                self.checkpoint();
            }
            Effect::SetAudiobooksOnHome(on) => {
                self.model.settings.audiobooks_on_home = on;
                self.checkpoint();
            }
            Effect::SetBookFinished(key, finished) => self.set_book_finished(&key, finished)?,
            Effect::ForgetBook(key) => self.forget_book(&key)?,
            Effect::LoadChapters(key) => {
                if let Some(book) = self.book_by_key(&key) {
                    self.request_chapters(&book);
                }
            }
            Effect::SetScreenOffButtons(mode) => {
                self.model.settings.screen_off_buttons = mode;
                self.checkpoint();
            }
            Effect::SetHaptics(strength) => {
                self.model.settings.haptics = strength;
                self.checkpoint();
            }
            Effect::SetSpeaker(on) => {
                self.model.settings.speaker = on;
                self.checkpoint();
                // Playing on the jack output: follow the new route at once.
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) && self.model.speaker_active != self.speaker_route()
                {
                    self.load()?;
                } else if self.model.playback == PlaybackState::Stopped {
                    self.model.speaker_active = self.speaker_route();
                }
                self.dirty.mark_both();
            }
            Effect::SetShuffle(mode) => {
                let was = self.model.settings.shuffle_mode();
                self.model.settings.set_shuffle_mode(mode);
                // What plays now follows at once: its songs to come shuffle,
                // as songs or albums, or go back to their own order (your
                // queue stays as it is).
                let music = self.model.book.is_none()
                    && self.model.current().is_some_and(|t| !t.is_radio());
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
            }
            Effect::SetPlaylistSort(path, sort) => {
                self.model.settings.set_playlist_sort(&path, sort);
                self.checkpoint();
            }
            Effect::SetRepeat(mode) => {
                self.model.settings.repeat = mode;
                self.upcoming_changed();
                self.checkpoint();
            }
            Effect::SetScreenTimeout(seconds) => {
                self.model.settings.screen_timeout_seconds = seconds;
                self.checkpoint();
            }
            Effect::SetTimeZone(zone) => {
                // A zone chosen by hand ends Automatic (Beta 7.1).
                self.model.settings.time_zone = zone;
                self.model.settings.time_zone_auto = Some(false);
                self.checkpoint();
            }
            Effect::SetTimeZoneAuto(on) => {
                self.model.settings.time_zone_auto = Some(on);
                self.zone_lookup_again = on;
                self.checkpoint();
            }
            Effect::SetHomePin(key, on) => {
                self.model.settings.pin_home(&key, on);
                self.checkpoint();
            }
            Effect::SetHomeShown(key, on) => {
                self.model.settings.show_on_home(&key, on);
                self.checkpoint();
            }
            Effect::QueueRemove(index) => {
                if index == self.model.queue_position {
                    self.ui.flash("The current track stays in the queue");
                } else if let Some(&id) = self.model.queue_entry_ids.get(index) {
                    if self.model.remove_entries(&[id]) > 0 {
                        self.upcoming_changed();
                        self.checkpoint();
                    }
                }
            }
            Effect::QueueMove { index, delta } => {
                let Some(&id) = self.model.queue_entry_ids.get(index) else {
                    return Ok(());
                };
                if self.model.move_entry(id, delta < 0) {
                    self.upcoming_changed();
                    self.checkpoint();
                }
            }
            Effect::QueueRemoveEntries(ids) => {
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
            }
            Effect::QueueEntriesNext(ids) => {
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
            }
            Effect::QueueEntriesLast(ids) => {
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
            }
            Effect::ClearQueued => {
                if self.model.clear_queued() {
                    self.upcoming_changed();
                    self.checkpoint();
                }
            }
            Effect::ClearQueue => {
                if self.model.clear_future_queue() {
                    self.upcoming_changed();
                    self.checkpoint();
                }
            }
            Effect::RebuildLibrary => {
                self.model.apply(Event::LibraryScanStarted);
                if let Err(error) = self.scanner.rebuild(self.model.sources.clone()) {
                    self.model.apply(Event::LibraryScanFailed(error.clone()));
                    return Err(error);
                }
                self.ui.flash("Rebuilding library");
            }
            Effect::PowerOff => {
                power::request_shutdown(false)?;
            }
            Effect::Reboot => {
                power::request_shutdown(true)?;
            }
            Effect::Checkpoint => self.checkpoint(),
        };
        Ok(())
    }
    /// Apply typed replies from the platform client.
    /// Play the changed sound settings: restart what plays; a held pause
    /// gives its stream back, or Play would continue with the old ones.
    pub(crate) fn reload_dsp(&mut self) -> Result<(), String> {
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.load()?;
        } else if self.playback.is_held() {
            self.release_hold();
        }
        self.checkpoint();
        Ok(())
    }
}
