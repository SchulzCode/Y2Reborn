//! The screen, the library scan, the cache and a settings reset.
use crate::*;

impl Runtime {
    /// `Effect::ScanLibrary`.
    pub(crate) fn on_scan_library(&mut self) -> Result<(), String> {
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
        Ok(())
    }

    /// `Effect::ScreenSleep`.
    pub(crate) fn on_screen_sleep(&mut self) -> Result<(), String> {
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
        Ok(())
    }

    /// `Effect::ScreenWake`.
    pub(crate) fn on_screen_wake(&mut self) -> Result<(), String> {
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
        Ok(())
    }

    /// `Effect::ResetSettings`.
    pub(crate) fn on_reset_settings(&mut self) -> Result<(), String> {
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
        Ok(())
    }

    /// `Effect::ClearCache`.
    pub(crate) fn on_clear_cache(&mut self) -> Result<(), String> {
        let removed = clear_cache(&self.root.join("cache"));
        self.art = false;
        self.dirty.mark_render();
        self.ui.flash(if removed.is_ok() {
            "Cache cleared"
        } else {
            "Couldn't clear everything. Try again."
        });
        Ok(())
    }
}
