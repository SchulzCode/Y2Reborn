//! What the UI asks for (`Effect`) and the DSP reload.
use crate::*;

mod platform;
mod queue;
mod radios;
mod sound;
mod system;

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
            Effect::Platform(task) => self.on_platform(task)?,
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
            Effect::SetCodecPreference(preference) => self.on_set_codec_preference(preference)?,
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
            Effect::PlayNow(index) => self.on_play_now(index)?,
            Effect::PlayNext(index) => self.queue_songs(vec![index], true)?,
            Effect::PlayNextCollection(members) => self.queue_songs(members, true)?,
            Effect::AddToQueue(index) => self.queue_songs(vec![index], false)?,
            Effect::AddToQueueCollection(members) => self.queue_songs(members, false)?,
            Effect::TogglePlayback => self.toggle()?,
            Effect::NextTrack | Effect::PreviousTrack => self.on_next_or_previous_track(&e)?,
            // A live station has nothing to seek in.
            Effect::Seek(_) if self.model.current().is_some_and(Track::is_radio) => {}
            Effect::Seek(delta) => self.seek_by(delta),
            Effect::AdjustVolume(delta) => self.on_adjust_volume(delta)?,
            Effect::ScanLibrary => self.on_scan_library()?,
            Effect::WifiPower => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi service unavailable")?
                .commands
                .try_send(wifi::Command::Power(!self.wifi_state.enabled))
                .map_err(|_| "Wi-Fi busy")?,
            Effect::WifiScan => self.on_wifi_scan()?,
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
            Effect::BluetoothScan => self.on_bluetooth_scan()?,
            Effect::BluetoothDevice { path, operation } => {
                self.on_bluetooth_device(path, operation)?
            }
            Effect::Output(o) => self.switch(o)?,
            Effect::ConfirmPairing(ok) => self
                .bluetooth
                .as_ref()
                .ok_or("Bluetooth unavailable")?
                .commands
                .try_send(bluetooth::Command::Confirm(ok))
                .map_err(|_| "Bluetooth busy")?,
            Effect::ScreenSleep => self.on_screen_sleep()?,
            Effect::ScreenWake => self.on_screen_wake()?,
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
            Effect::SetEqBandGain { index, gain_db } => self.on_set_eq_band_gain(index, gain_db)?,
            Effect::SetEqPreset(gains) => self.on_set_eq_preset(gains)?,
            Effect::ResetEq => {
                self.model.settings.eq_bands = reborn_core::flat_eq_bands();
                self.reload_dsp()?;
            }
            Effect::ScanEqProfiles => {
                self.ui.eq_profiles = scan_eq_profiles(&shipped_eq_dirs(), &self.storage_roots());
                self.dirty.mark_render();
            }
            Effect::ImportEqProfile(path) => self.on_import_eq_profile(path)?,
            // The level in use again: nothing to restart.
            Effect::SetHpLevel(level) if level == self.model.settings.hp_level => {}
            Effect::SetHpLevel(level) => self.on_set_hp_level(level)?,
            Effect::AllowLoudVolume => self.on_allow_loud_volume()?,
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
            Effect::ResetSettings => self.on_reset_settings()?,
            Effect::ForgetAllWifi => self.on_forget_all_wifi()?,
            Effect::ForgetAllBluetooth => self.on_forget_all_bluetooth()?,
            Effect::ClearCache => self.on_clear_cache()?,
            Effect::SetBalance(balance) => {
                self.model.settings.balance =
                    balance.clamp(-reborn_core::MAX_BALANCE, reborn_core::MAX_BALANCE);
                self.playback.set_balance(self.model.settings.balance);
                self.dirty.mark_both();
                self.checkpoint();
            }
            Effect::SetCrossfade(milliseconds) => self.on_set_crossfade(milliseconds)?,
            Effect::SetGapless(enabled) => self.on_set_gapless(enabled)?,
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
            Effect::SetBookSpeed(percent) => self.on_set_book_speed(percent)?,
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
            Effect::SetSpeaker(on) => self.on_set_speaker(on)?,
            Effect::SetShuffle(mode) => self.on_set_shuffle(mode)?,
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
            Effect::QueueRemove(index) => self.on_queue_remove(index)?,
            Effect::QueueMove { index, delta } => {
                let Some(&id) = self.model.queue_entry_ids.get(index) else {
                    return Ok(());
                };
                if self.model.move_entry(id, delta < 0) {
                    self.upcoming_changed();
                    self.checkpoint();
                }
            }
            Effect::QueueRemoveEntries(ids) => self.on_queue_remove_entries(ids)?,
            Effect::QueueEntriesNext(ids) => self.on_queue_entries_next(ids)?,
            Effect::QueueEntriesLast(ids) => self.on_queue_entries_last(ids)?,
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
