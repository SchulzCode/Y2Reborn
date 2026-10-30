//! Volume, output and sound settings.
use crate::*;

impl Runtime {
    /// `Effect::SetCodecPreference`.
    pub(crate) fn on_set_codec_preference(
        &mut self,
        preference: reborn_core::CodecPreference,
    ) -> Result<(), String> {
        if !codec_change_allowed(self.model.playback, self.pending_reconfiguration.is_some()) {
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
        Ok(())
    }

    /// `Effect::AdjustVolume`.
    pub(crate) fn on_adjust_volume(&mut self, delta: i8) -> Result<(), String> {
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
        Ok(())
    }

    /// `Effect::SetEqBandGain`.
    pub(crate) fn on_set_eq_band_gain(&mut self, index: usize, gain_db: i8) -> Result<(), String> {
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
        Ok(())
    }

    /// `Effect::SetEqPreset`.
    pub(crate) fn on_set_eq_preset(&mut self, gains: [i8; 5]) -> Result<(), String> {
        let mut bands = reborn_core::flat_eq_bands();
        for (band, gain) in bands.iter_mut().zip(gains) {
            band.gain_db = f32::from(gain.clamp(-12, 12));
        }
        self.model.settings.eq_bands = bands;
        self.model.settings.eq_enabled = gains.iter().any(|g| *g != 0);
        self.model.settings.eq_profile_on = false;
        self.reload_dsp()?;
        Ok(())
    }

    /// `Effect::ImportEqProfile`.
    pub(crate) fn on_import_eq_profile(&mut self, path: PathBuf) -> Result<(), String> {
        match import_eq_profile(&path) {
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
        }
        Ok(())
    }

    /// `Effect::SetHpLevel`.
    pub(crate) fn on_set_hp_level(&mut self, level: reborn_core::HpLevel) -> Result<(), String> {
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
        Ok(())
    }

    /// `Effect::AllowLoudVolume`.
    pub(crate) fn on_allow_loud_volume(&mut self) -> Result<(), String> {
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
        Ok(())
    }

    /// `Effect::SetCrossfade`.
    pub(crate) fn on_set_crossfade(&mut self, milliseconds: u32) -> Result<(), String> {
        self.model.settings.crossfade_ms = milliseconds.min(30_000);
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.load()?;
        }
        self.checkpoint();
        Ok(())
    }

    /// `Effect::SetGapless`.
    pub(crate) fn on_set_gapless(&mut self, enabled: bool) -> Result<(), String> {
        self.model.settings.gapless_enabled = enabled;
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.load()?;
        }
        self.checkpoint();
        Ok(())
    }

    /// `Effect::SetBookSpeed`.
    pub(crate) fn on_set_book_speed(&mut self, percent: u16) -> Result<(), String> {
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
        Ok(())
    }

    /// `Effect::SetSpeaker`.
    pub(crate) fn on_set_speaker(&mut self, on: bool) -> Result<(), String> {
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
        Ok(())
    }
}
