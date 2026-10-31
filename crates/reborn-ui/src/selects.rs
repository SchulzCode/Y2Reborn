//! Selecting on the radio, settings, Bluetooth and Wi-Fi screens, and playing what is chosen.
use crate::*;

/// A network the user typed a password for: its keyboard reopens with it after a wrong password.
pub(crate) struct TypedJoin {
    pub ssid: String,
    pub password: String,
    pub hidden: bool,
}

impl Ui {
    /// The Brightness card while it is adjusted: the wheel moves it in 5 % steps, Select or Back
    /// ends (Y2Linux #74: Select only cycled 20 % steps). `None` leaves the action to the screen.
    pub(crate) fn brightness_input(&mut self, m: &AppModel, action: Action) -> Option<Effect> {
        if !self.adjusting_brightness {
            return None;
        }
        if m.screen != Screen::SettingsDisplay || m.navigation.modal.is_some() {
            self.adjusting_brightness = false;
            return None;
        }
        let step = i16::from(reborn_core::BRIGHTNESS_STEP);
        let delta = match action {
            Action::WheelClockwise(_) | Action::NavigateDown => step,
            Action::WheelCounterClockwise(_) | Action::NavigateUp => -step,
            Action::Select | Action::Back => {
                self.adjusting_brightness = false;
                return Some(Effect::None);
            }
            _ => {
                self.adjusting_brightness = false;
                return None;
            }
        };
        let level = (i16::from(m.settings.brightness) + delta)
            .clamp(i16::from(reborn_core::BRIGHTNESS_MIN), 100);
        Some(Effect::SetBrightness(level as u8))
    }

    pub(crate) fn radio_select(&mut self, m: &mut AppModel, key: &str) -> Effect {
        let list = |m: &mut AppModel, ui: &mut Self, title: &str, request: RadioRequest| {
            ui.radio.title = title.into();
            ui.radio.stations.clear();
            ui.radio.loading = true;
            ui.radio.error = None;
            ui.radio.last_request = Some(request.clone());
            Self::go(m, Screen::RadioStations, title);
            Effect::Radio(request)
        };
        let categories = |m: &mut AppModel, ui: &mut Self, genres: bool| {
            ui.radio.categories.clear();
            ui.radio.genres = genres;
            ui.radio.loading = true;
            ui.radio.error = None;
            let request = if genres {
                RadioRequest::Genres
            } else {
                RadioRequest::Countries
            };
            ui.radio.last_request = Some(request.clone());
            Self::go(
                m,
                Screen::RadioCategories,
                if genres { "Genres" } else { "Countries" },
            );
            Effect::Radio(request)
        };
        match key {
            "radio_favorites" => list(m, self, "Favorites", RadioRequest::Favorites),
            "radio_own" => list(m, self, "My Stations", RadioRequest::Own),
            "radio_popular" => list(m, self, "Popular", RadioRequest::Popular),
            "radio_countries" => categories(m, self, false),
            "radio_genres" => categories(m, self, true),
            "radio_search" => {
                self.keyboard = Some(keyboard::Keyboard::new(
                    keyboard::Purpose::RadioSearch,
                    self.radio_query.clone(),
                ));
                Effect::None
            }
            "radio_retry" => match self.radio.last_request.clone() {
                Some(request) => {
                    self.radio.loading = true;
                    self.radio.error = None;
                    Effect::Radio(request)
                }
                None => Effect::None,
            },
            "radio_wifi" => {
                Self::go(m, Screen::Wifi, "");
                Effect::None
            }
            "radio_now" => {
                Self::go(m, Screen::NowPlaying, "");
                Effect::None
            }
            "radio_info_play" => match self.radio.info.clone() {
                Some(station) => {
                    Self::go(m, Screen::NowPlaying, "");
                    Effect::Radio(RadioRequest::PlayStation(Box::new(station)))
                }
                None => Effect::None,
            },
            "radio_info_favorite" => match self.radio.info.clone() {
                Some(station) => {
                    Effect::Radio(RadioRequest::ToggleFavoriteStation(Box::new(station)))
                }
                None => Effect::None,
            },
            "radio_back" => {
                Self::back(m);
                Effect::None
            }
            k => {
                if let Some(i) = k
                    .strip_prefix("station:")
                    .and_then(|i| i.parse::<usize>().ok())
                {
                    if i < self.radio.stations.len() {
                        Self::go(m, Screen::NowPlaying, "");
                        return Effect::Radio(RadioRequest::Play(i));
                    }
                }
                if let Some(category) = k
                    .strip_prefix("category:")
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| self.radio.categories.get(i).cloned())
                {
                    let request = if self.radio.genres {
                        RadioRequest::Genre(category.id.clone())
                    } else {
                        RadioRequest::Country {
                            code: category.id.clone(),
                            name: category.name.clone(),
                        }
                    };
                    return list(m, self, &category.name, request);
                }
                Effect::None
            }
        }
    }

    /// Settings rows that change a value in place: one Select, one step.
    pub(crate) fn setting(&mut self, m: &mut AppModel, key: &str) -> Option<Effect> {
        Some(match key {
            "output" => {
                Self::open_modal(m, Modal::OutputPicker);
                Effect::None
            }
            "codec_preference" => {
                Self::open_modal(m, Modal::CodecPicker);
                Effect::None
            }
            "ldac_quality" => {
                let q = m.platform.snapshot.bluetooth_quality;
                if !q.ldac_supported {
                    return None;
                }
                Effect::Platform(PlatformTask::LdacQuality(q.requested_quality?.next()))
            }
            "sbc_quality" => {
                let q = m.platform.snapshot.bluetooth_quality;
                if !q.sbc_supported {
                    return None;
                }
                Effect::Platform(PlatformTask::SbcQuality(q.requested_sbc?.next()))
            }
            "aac_quality" => {
                let q = m.platform.snapshot.bluetooth_quality;
                if !q.aac_supported {
                    return None;
                }
                Effect::Platform(PlatformTask::AacQuality(q.requested_aac?.next()))
            }
            "aac_abr" => {
                let q = m.platform.snapshot.bluetooth_quality;
                if !q.aac_abr_supported {
                    return None;
                }
                Effect::Platform(PlatformTask::AacAbr(!q.requested_aac_abr?))
            }
            "ldac_abr" => {
                let q = m.platform.snapshot.bluetooth_quality;
                if !q.abr_supported {
                    return None;
                }
                Effect::Platform(PlatformTask::LdacAbr(!q.requested_abr?))
            }
            "screen_off" => Effect::ScreenSleep,
            "eq_enabled" => Effect::SetEqEnabled(!m.settings.eq_enabled),
            "eq_reset" => Effect::ResetEq,
            "replay_gain" => Effect::SetReplayGain(match m.settings.replay_gain {
                reborn_core::ReplayGainMode::Off => reborn_core::ReplayGainMode::Track,
                _ => reborn_core::ReplayGainMode::Off,
            }),
            k if k.starts_with("crossfade_set:") => {
                Effect::SetCrossfade(k["crossfade_set:".len()..].parse().ok()?)
            }
            k if k.starts_with("balance_set:") => {
                Effect::SetBalance(k["balance_set:".len()..].parse().ok()?)
            }
            k if k.starts_with("theme_set:") => {
                Effect::SetTheme(k["theme_set:".len()..].to_owned())
            }
            k if k.starts_with("timeout_set:") => {
                Effect::SetScreenTimeout(k["timeout_set:".len()..].parse().ok()?)
            }
            k if k.starts_with("tz_set:") => Effect::SetTimeZone(k["tz_set:".len()..].into()),
            "tz_auto" => Effect::SetTimeZoneAuto(!m.settings.zone_auto()),
            k if k.starts_with("home_pin:") => {
                let key = &k["home_pin:".len()..];
                let pinned = m.settings.home_pins.iter().any(|p| p == key);
                Effect::SetHomePin(key.into(), !pinned)
            }
            k if k.starts_with("home_show:") => {
                let key = &k["home_show:".len()..];
                Effect::SetHomeShown(key.into(), !m.settings.home_shows(key))
            }
            "output_set:wired" => Effect::Output(AudioOutput::Wired),
            k if k.starts_with("output_set:") => Effect::BluetoothDevice {
                path: k["output_set:".len()..].into(),
                operation: "output".into(),
            },
            k if k.starts_with("hp_level_set:") => {
                Effect::SetHpLevel(reborn_core::HpLevel::from_key(&k["hp_level_set:".len()..])?)
            }
            k if k.starts_with("dac_filter_set:") => Effect::SetDacFilter(
                reborn_core::DacFilter::from_key(&k["dac_filter_set:".len()..])?,
            ),
            k if k.starts_with("eq_preset:") => {
                let name = &k["eq_preset:".len()..];
                let (_, _, gains) = pages::EQ_PRESETS.iter().find(|(_, key, _)| *key == name)?;
                Effect::SetEqPreset(*gains)
            }
            "shuffle" => Effect::SetShuffle(m.settings.shuffle_mode().next()),
            "repeat" => Effect::SetRepeat(next_repeat(m.settings.repeat)),
            "gapless" => Effect::SetGapless(!m.settings.gapless_enabled),
            "scrobble_log" => Effect::SetScrobbleLog(!m.settings.scrobble_log),
            "lastfm_scrobble" => Effect::SetLastFmScrobble(!m.settings.lastfm_scrobble),
            "lastfm_now_playing" => Effect::SetLastFmNowPlaying(!m.settings.lastfm_now_playing),
            "lastfm_connect" => Effect::LastFm(reborn_core::LastFmRequest::Connect),
            "lastfm_cancel" => Effect::LastFm(reborn_core::LastFmRequest::Cancel),
            "lastfm_send" => Effect::LastFm(reborn_core::LastFmRequest::SendNow),
            "lastfm_disconnect" => Effect::LastFm(reborn_core::LastFmRequest::Disconnect),
            "lastfm_password" => {
                self.keyboard = Some(keyboard::Keyboard::new(keyboard::Purpose::LastFmUser, ""));
                Effect::None
            }
            "wrap_around" => Effect::SetWrapAround(m.settings.wrap_around.next()),
            "home_audiobooks" => Effect::SetAudiobooksOnHome(!m.settings.audiobooks_on_home),
            "screen_off_buttons" => {
                Effect::SetScreenOffButtons(m.settings.screen_off_buttons.next())
            }
            "haptics" => Effect::SetHaptics(m.settings.haptics.next()),
            "speaker" => Effect::SetSpeaker(!m.settings.speaker),
            "album_view" => Effect::SetAlbumView(m.settings.album_view.next()),
            "album_order" => Effect::SetAlbumOrder(m.settings.album_order.next()),
            "artists_list" => Effect::SetArtistsList(m.settings.artists_list.next()),
            "now_playing_wheel" => Effect::SetNowPlayingWheel(m.settings.now_playing_wheel.next()),
            "crossfade" => Effect::SetCrossfade(match m.settings.crossfade_ms {
                0 => 3_000,
                3_000 => 6_000,
                6_000 => 10_000,
                _ => 0,
            }),
            "brightness" => {
                self.adjusting_brightness = true;
                Effect::None
            }
            "timeout" => Effect::SetScreenTimeout(match m.settings.screen_timeout_seconds {
                15 => 30,
                30 => 60,
                60 => 120,
                120 => 300,
                300 => 0,
                _ => 15,
            }),
            "scan_library" => Effect::ScanLibrary,
            "wifi_power" => Effect::WifiPower,
            "wifi_scan" => Effect::WifiScan,
            "wifi_other" => {
                self.keyboard = Some(keyboard::Keyboard::new(keyboard::Purpose::WifiName, ""));
                Effect::None
            }
            "bt_power" => Effect::BluetoothPower,
            "bt_scan" => Effect::BluetoothScan,
            _ => return None,
        })
    }

    pub(crate) fn play_track(&mut self, m: &mut AppModel, tracks: &[Track], key: &str) -> Effect {
        let Some(index) = key
            .strip_prefix("track:")
            .and_then(|value| value.parse().ok())
        else {
            return Effect::None;
        };
        if tracks.get(index).is_none() {
            return Effect::None;
        }
        let effect = self.play_effect(m, tracks, index);
        Self::go(m, Screen::NowPlaying, "");
        effect
    }

    pub(crate) fn play_filtered(
        &mut self,
        m: &mut AppModel,
        tracks: &[Track],
        shuffle: ShuffleMode,
    ) -> Effect {
        let members = self.filtered_indices(m, tracks);
        let Some(&index) = members.first() else {
            return Effect::None;
        };
        let effect = Effect::PlayCollection {
            selected: index,
            members,
            shuffle,
            source: source_label(m),
            origin: page_origin(m),
        };
        Self::go(m, Screen::NowPlaying, "");
        effect
    }
    pub(crate) fn filtered_indices(&self, m: &AppModel, tracks: &[Track]) -> Vec<usize> {
        if matches!(
            m.screen,
            Screen::Album | Screen::Search | Screen::Tracks | Screen::Folders
        ) {
            // The page's own order (album, search results, Songs A to Z, a
            // folder's own songs by file name), not library order: Next
            // plays the row below (Y2Reborn #21).
            // The search: every song found, also those See All would show.
            let mut c = self.catalog.borrow_mut();
            self.ensure_catalog(&mut c, m, tracks);
            if m.screen == Screen::Search {
                return c.search_songs.clone();
            }
            return c.rows.iter().filter_map(catalog::Row::track).collect();
        }
        tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track_matches(track, &m.navigation.filter))
            .map(|(index, _)| index)
            .collect()
    }
    pub(crate) fn play_effect(&self, m: &AppModel, tracks: &[Track], selected: usize) -> Effect {
        let members = self.filtered_indices(m, tracks);
        if members.len() > 1 || !m.navigation.filter.is_empty() {
            Effect::PlayCollection {
                selected,
                members,
                shuffle: ShuffleMode::Off,
                source: source_label(m),
                origin: page_origin(m),
            }
        } else {
            Effect::Play(selected)
        }
    }

    pub(crate) fn bluetooth_select(&mut self, m: &mut AppModel, key: &str) -> Effect {
        let Some(device) = self.device(Some(key)) else {
            return Effect::None;
        };
        if device.connected && device.saved_pairing() {
            // Connected devices offer their actions instead of disconnecting
            // on a single press.
            m.navigation.context_target = Some(m.navigation.focus);
            m.navigation.context_key = Some(key.into());
            Self::open_modal(m, Modal::ContextMenu);
            return Effect::None;
        }
        let (path, name, operation) = (
            device.path.clone(),
            device.name.clone(),
            device.operation().to_owned(),
        );
        if matches!(operation.as_str(), "pair" | "connect") {
            self.bt_busy = Some((path.clone(), name, operation.clone(), Instant::now()));
        }
        Effect::BluetoothDevice { path, operation }
    }

    pub(crate) fn wifi_select(&mut self, m: &mut AppModel, key: &str) -> Effect {
        if let Some(id) = key.strip_prefix("saved:") {
            let joined = self.wifi.networks.iter().any(|n| {
                n.saved_id.map(|v| v.to_string()).as_deref() == Some(id)
                    && self.wifi.status.network() == Some(n.ssid.as_str())
            });
            if joined {
                m.navigation.context_target = Some(m.navigation.focus);
                m.navigation.context_key = Some(key.into());
                Self::open_modal(m, Modal::ContextMenu);
                return Effect::None;
            }
            // Its password failed: ask for the new one instead of trying it again.
            if let Some(ssid) = self.wifi_failed_ssid(id) {
                self.open_wifi_keyboard(ssid, "", false);
                return Effect::None;
            }
            return id.parse().map(Effect::WifiSaved).unwrap_or(Effect::None);
        }
        let Some(ssid) = key.strip_prefix("ssid:") else {
            return Effect::None;
        };
        match self
            .wifi
            .networks
            .iter()
            .find(|n| n.ssid == ssid)
            .and_then(|n| n.secured)
        {
            Some(false) => Effect::WifiConnect {
                ssid: ssid.into(),
                password: String::new(),
                hidden: false,
            },
            Some(true) => {
                self.open_wifi_keyboard(ssid.to_owned(), "", false);
                Effect::None
            }
            None => Effect::None,
        }
    }

    pub(crate) fn open_wifi_keyboard(&mut self, ssid: String, text: &str, hidden: bool) {
        self.keyboard = Some(keyboard::Keyboard::new(
            keyboard::Purpose::WifiPassword { ssid, hidden },
            text,
        ));
    }

    /// The name of saved network `id` when its password failed.
    pub(crate) fn wifi_failed_ssid(&self, id: &str) -> Option<String> {
        let ssid = &self
            .wifi
            .networks
            .iter()
            .find(|n| n.saved_id.map(|v| v.to_string()).as_deref() == Some(id))?
            .ssid;
        let failed =
            self.wifi.problem == Some(WifiProblem::WrongPassword)
                && self.wifi.problem_network.as_deref() == Some(ssid.as_str())
                || self.wifi.join.as_ref().is_some_and(|j| {
                    j.ssid == *ssid && j.outcome == WifiJoinOutcome::WrongPassword
                });
        failed.then(|| ssid.clone())
    }

    /// Say how a join ended, once (called after each Wi-Fi observation). A
    /// wrong typed password reopens the keyboard with it and says so.
    pub fn wifi_join_changed(&mut self) {
        let Some(join) = self.wifi.join.clone() else {
            return;
        };
        if join.outcome == WifiJoinOutcome::Checking || join.attempt <= self.wifi_join_seen {
            return;
        }
        self.wifi_join_seen = join.attempt;
        let typed = self.wifi_typed.take().filter(|t| t.ssid == join.ssid);
        match join.outcome {
            WifiJoinOutcome::Joined => self.flash(format!("Connected to {}", join.ssid)),
            WifiJoinOutcome::WrongPassword => match typed {
                Some(typed) => {
                    self.open_wifi_keyboard(join.ssid.clone(), &typed.password, typed.hidden);
                    if let Some(k) = self.keyboard.as_mut() {
                        k.note = "Wrong password".into();
                    }
                }
                None => self.flash(format!(
                    "Wrong password for {}. Select it to enter the new one",
                    join.ssid
                )),
            },
            WifiJoinOutcome::NotFound => self.flash(format!(
                "{} is not in range. Move closer and try again",
                join.ssid
            )),
            WifiJoinOutcome::Failed | WifiJoinOutcome::Checking => {
                self.flash(format!("Couldn't connect to {}. Try again", join.ssid))
            }
        }
    }
}
