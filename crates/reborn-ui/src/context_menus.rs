//! The modal and context menus and the Now Playing entry points.
use crate::*;

impl Ui {
    pub(crate) fn modal_action(&mut self, m: &mut AppModel, tracks: &[Track]) -> Effect {
        let rows = self.modal_rows(m, tracks);
        let Some(row) = rows.get(m.navigation.modal_focus).filter(|r| r.enabled) else {
            return Effect::None;
        };
        let key = row.key.clone();
        match m.navigation.modal.clone() {
            Some(Modal::OutputPicker) => {
                Self::close_modal(m);
                match key.strip_prefix("output:") {
                    Some("wired") => Effect::Output(AudioOutput::Wired),
                    Some(path) => Effect::BluetoothDevice {
                        path: path.into(),
                        operation: "output".into(),
                    },
                    None => Effect::None,
                }
            }
            Some(Modal::CodecPicker) => {
                Self::close_modal(m);
                let preference = self
                    .bluetooth
                    .codec_choices
                    .iter()
                    .copied()
                    .find(|c| key == format!("codec:{}", c.label()));
                preference
                    .map(Effect::SetCodecPreference)
                    .unwrap_or(Effect::None)
            }
            Some(Modal::EqBand(index)) => {
                Self::close_modal(m);
                key.strip_prefix("eq_gain:")
                    .and_then(|s| s.parse::<i8>().ok())
                    .map(|gain_db| Effect::SetEqBandGain { index, gain_db })
                    .unwrap_or(Effect::None)
            }
            Some(Modal::SleepTimer) => {
                Self::close_modal(m);
                match key.strip_prefix("sleep_set:") {
                    Some("off") => Effect::SetSleepTimer(reborn_core::SleepChoice::Off),
                    Some("end") => Effect::SetSleepTimer(reborn_core::SleepChoice::EndOfItem),
                    Some(n) => n
                        .parse()
                        .map(|n| Effect::SetSleepTimer(reborn_core::SleepChoice::Minutes(n)))
                        .unwrap_or(Effect::None),
                    None => Effect::None,
                }
            }
            Some(Modal::DateField(field)) => {
                Self::close_modal(m);
                if let Some(n) = key.strip_prefix("date_value:").and_then(|s| s.parse().ok()) {
                    date_time::choose(self, m, field, n);
                    // Time: the hour, then the minute.
                    if let Some(next) = date_time::then(field) {
                        let focus = date_time::picker(self, m, next).1;
                        Self::open_modal(m, Modal::DateField(next));
                        m.navigation.modal_focus = focus;
                    }
                }
                Effect::None
            }
            Some(Modal::Confirm(confirm)) => {
                if key == "cancel" {
                    Self::close_modal(m);
                    return Effect::None;
                }
                let context = m.navigation.context_key.clone();
                Self::close_modal(m);
                match confirm {
                    ConfirmAction::ForgetBluetooth => context
                        .as_deref()
                        .and_then(|s| s.strip_prefix("bt_device:"))
                        .filter(|path| self.bluetooth.devices.iter().any(|d| d.path == *path))
                        .map(|path| Effect::BluetoothDevice {
                            path: path.into(),
                            operation: "forget".into(),
                        })
                        .unwrap_or(Effect::None),
                    ConfirmAction::ForgetWifi => context
                        .as_deref()
                        .and_then(|s| s.strip_prefix("saved:"))
                        .and_then(|s| s.parse().ok())
                        .map(Effect::WifiForget)
                        .unwrap_or(Effect::None),
                    ConfirmAction::ClearQueue => Effect::ClearQueue,
                    ConfirmAction::RebuildLibrary => Effect::RebuildLibrary,
                    ConfirmAction::ResetSettings => Effect::ResetSettings,
                    ConfirmAction::ForgetAllWifi => Effect::ForgetAllWifi,
                    ConfirmAction::ForgetAllBluetooth => Effect::ForgetAllBluetooth,
                    ConfirmAction::ClearCache => Effect::ClearCache,
                    ConfirmAction::DeletePlaylist => match self.playlists.target.take() {
                        Some(path) => {
                            Effect::Playlist(Box::new(reborn_core::PlaylistRequest::Delete(path)))
                        }
                        None => Effect::None,
                    },
                    ConfirmAction::PowerOff => Effect::PowerOff,
                    ConfirmAction::Reboot => Effect::Reboot,
                    ConfirmAction::Platform(task) => Effect::Platform(task),
                    ConfirmAction::ReportsQuestion => {
                        Effect::Platform(PlatformTask::ReportEnable(key == "confirm"))
                    }
                    ConfirmAction::LoudVolume => Effect::AllowLoudVolume,
                }
            }
            Some(Modal::ContextMenu) => self.context_action(m, tracks, &key),
            Some(Modal::PlaylistPicker) => self.picker_action(m, tracks, &key),
            None => Effect::None,
        }
    }

    /// A book's hold menu: Continue (or Play), Start Over, Mark as Finished
    /// (or Not Finished) and Forget Progress.
    pub(crate) fn book_context_rows(&self, m: &AppModel, tracks: &[Track], key: &str) -> Vec<Item> {
        let Some(book) = self.book(tracks, key) else {
            return vec![];
        };
        let saved = m.library.shelf.find(&book, tracks).map(|(_, p)| p.finished);
        let mut rows = vec![
            Item::new(
                if saved == Some(false) {
                    "Continue"
                } else {
                    "Play"
                },
                "book_menu_resume",
            ),
            Item::new("Start Over", "book_menu_restart"),
            Item::new(
                if saved == Some(true) {
                    "Mark as Not Finished"
                } else {
                    "Mark as Finished"
                },
                "book_menu_finish",
            ),
        ];
        if saved == Some(false) {
            rows.push(Item::new("Forget Progress", "book_menu_forget"));
        }
        rows
    }

    pub(crate) fn context_action(
        &mut self,
        m: &mut AppModel,
        tracks: &[Track],
        menu_key: &str,
    ) -> Effect {
        let target = m.navigation.context_target.unwrap_or(0);
        let source_key = m
            .navigation
            .context_key
            .clone()
            .or_else(|| self.row_at(m, tracks, target).map(|r| r.key))
            .unwrap_or_default();
        if menu_key == "sleep_timer" {
            // A picker (#29): the wheel chooses, Select starts it, Back leaves
            // the timer as it was. It opens on the timer set, else 30 min.
            let now = m
                .sleep_timer
                .map_or(reborn_core::SleepChoice::Minutes(30), |t| t.choice);
            let focus = reborn_core::SleepChoice::all()
                .iter()
                .position(|c| *c == now)
                .unwrap_or(0);
            Self::open_modal(m, Modal::SleepTimer);
            m.navigation.modal_focus = focus;
            return Effect::None;
        }
        if menu_key.starts_with("sel_") {
            return self.selection_effect(m, tracks, menu_key);
        }
        if menu_key.starts_with("group_") {
            return self.group_effect(m, tracks, &source_key, menu_key);
        }
        if menu_key == "book_speed" {
            // The menu stays open: each press steps the speed on.
            return m.book_part().map_or(Effect::None, |(book, _)| {
                Effect::SetBookSpeed(reborn_core::books::next_speed(book.speed))
            });
        }
        if menu_key == "book_chapters" {
            Self::close_modal(m);
            if let Some(book) = m.book.as_ref().map(|b| b.key.clone()) {
                Self::go(m, Screen::Book, book.clone());
                return Effect::LoadChapters(book);
            }
            return Effect::None;
        }
        if let Some(action) = menu_key.strip_prefix("book_menu_") {
            let book = source_key
                .strip_prefix("book:")
                .map(str::to_owned)
                .unwrap_or_else(|| m.navigation.filter.clone());
            Self::close_modal(m);
            let finished = self
                .book(tracks, &book)
                .and_then(|b| m.library.shelf.find(&b, tracks).map(|(_, p)| p.finished))
                .unwrap_or(false);
            return match action {
                "resume" | "restart" => {
                    Self::go(m, Screen::NowPlaying, "");
                    Effect::PlayBook(
                        book,
                        if action == "resume" {
                            reborn_core::BookStart::Resume
                        } else {
                            reborn_core::BookStart::Beginning
                        },
                    )
                }
                "finish" => Effect::SetBookFinished(book, !finished),
                "forget" => Effect::ForgetBook(book),
                _ => Effect::None,
            };
        }
        let queue_target = self.queue_target(m, Some(&source_key));
        let track_index = if m.screen == Screen::NowPlaying {
            m.current()
                .and_then(|current| tracks.iter().position(|track| track.id == current.id))
        } else {
            self.menu_song(m, tracks, &source_key)
        };
        let station = source_key
            .strip_prefix("station:")
            .and_then(|value| value.parse::<usize>().ok());
        if matches!(m.screen, Screen::Playlists | Screen::Playlist)
            || source_key.starts_with("playlist:")
        {
            if let Some(effect) = self.playlist_context_action(m, tracks, &source_key, menu_key) {
                return effect;
            }
        }
        match menu_key {
            "add_playlist" => {
                Self::close_modal(m);
                if let Some(t) = track_index {
                    self.open_picker(m, vec![t]);
                }
                return Effect::None;
            }
            "collection_playlist" => {
                Self::close_modal(m);
                let members = self.filtered_indices(m, tracks);
                self.open_picker(m, members);
                return Effect::None;
            }
            "fav_toggle" => {
                Self::close_modal(m);
                return match track_index {
                    Some(t) => self.toggle_favorite_track(m, tracks, t),
                    None => Effect::None,
                };
            }
            "save_queue" => {
                Self::close_modal(m);
                self.playlists.pending = m
                    .queue
                    .iter()
                    .filter_map(|q| {
                        tracks
                            .iter()
                            .position(|t| t.id == q.id && t.source_id == q.source_id)
                    })
                    .collect();
                if self.playlists.pending.is_empty() {
                    self.flash("The queue has no songs to save");
                } else {
                    self.keyboard =
                        Some(keyboard::Keyboard::new(keyboard::Purpose::PlaylistNew, ""));
                }
                return Effect::None;
            }
            _ => {}
        }
        match menu_key {
            "radio_play" if station.is_some() => {
                Self::close_modal(m);
                Self::go(m, Screen::NowPlaying, "");
                return Effect::Radio(RadioRequest::Play(station.unwrap_or(0)));
            }
            "radio_favorite" if station.is_some() => {
                Self::close_modal(m);
                return Effect::Radio(RadioRequest::ToggleFavorite(station.unwrap_or(0)));
            }
            "radio_favorite_current" => {
                Self::close_modal(m);
                return Effect::Radio(RadioRequest::ToggleFavoriteCurrent);
            }
            "radio_info" | "radio_info_current" => {
                let shown = if menu_key == "radio_info" {
                    station.and_then(|i| self.radio.stations.get(i)).cloned()
                } else {
                    self.radio.now.clone()
                };
                Self::close_modal(m);
                if let Some(shown) = shown {
                    self.radio.info = Some(shown);
                    Self::go(m, Screen::RadioInfo, "");
                }
                return Effect::None;
            }
            "radio_stop" => {
                Self::close_modal(m);
                return Effect::StopPlayback;
            }
            "bt_forget" => {
                m.navigation.modal = Some(Modal::Confirm(ConfirmAction::ForgetBluetooth));
                m.navigation.modal_focus = 0;
                return Effect::None;
            }
            "wifi_change_password" => {
                let ssid = m
                    .navigation
                    .context_key
                    .as_deref()
                    .and_then(|k| k.strip_prefix("saved:"))
                    .and_then(|id| {
                        self.wifi
                            .networks
                            .iter()
                            .find(|n| n.saved_id.map(|v| v.to_string()).as_deref() == Some(id))
                    })
                    .map(|n| n.ssid.clone());
                Self::close_modal(m);
                if let Some(ssid) = ssid {
                    // Joined like a new network: kept only if it connects,
                    // and then it replaces the old entry.
                    self.open_wifi_keyboard(ssid, "", false);
                }
                return Effect::None;
            }
            "wifi_forget" => {
                m.navigation.modal = Some(Modal::Confirm(ConfirmAction::ForgetWifi));
                m.navigation.modal_focus = 0;
                return Effect::None;
            }
            "clear_queue" => {
                m.navigation.modal = Some(Modal::Confirm(ConfirmAction::ClearQueue));
                m.navigation.modal_focus = 0;
                return Effect::None;
            }
            "bt_codec" => {
                Self::open_modal(m, Modal::CodecPicker);
                return Effect::None;
            }
            _ => {}
        }
        Self::close_modal(m);
        match menu_key {
            "bt_connect" | "bt_output" => self
                .device(Some(&source_key))
                .filter(|device| menu_key != "bt_output" || device.usable_audio())
                .map(|device| Effect::BluetoothDevice {
                    path: device.path.clone(),
                    operation: if menu_key == "bt_output" {
                        "output"
                    } else {
                        device.operation()
                    }
                    .into(),
                })
                .unwrap_or(Effect::None),
            "wifi_connect" => source_key
                .strip_prefix("saved:")
                .and_then(|s| s.parse().ok())
                .map(Effect::WifiSaved)
                .unwrap_or(Effect::None),
            "wifi_disconnect" => Effect::WifiDisconnect,
            "play" => track_index.map(Effect::Play).unwrap_or(Effect::None),
            "play_next" => track_index.map(Effect::PlayNext).unwrap_or(Effect::None),
            "add_queue" => track_index.map(Effect::AddToQueue).unwrap_or(Effect::None),
            "collection_play" => self.play_filtered(m, tracks, ShuffleMode::Off),
            "collection_shuffle" => self.play_filtered(m, tracks, ShuffleMode::Songs),
            "collection_shuffle_albums" => self.play_filtered(m, tracks, ShuffleMode::Albums),
            "collection_next" | "collection_queue" => {
                let members = self.filtered_indices(m, tracks);
                if members.is_empty() {
                    Effect::None
                } else if menu_key == "collection_next" {
                    Effect::PlayNextCollection(members)
                } else {
                    Effect::AddToQueueCollection(members)
                }
            }
            "show_queue" => {
                Self::go(m, Screen::Queue, "");
                self.open_up_next(m, tracks);
                Effect::None
            }
            "toggle_shuffle" => Effect::SetShuffle(m.settings.shuffle_mode().next()),
            "cycle_repeat" => Effect::SetRepeat(next_repeat(m.settings.repeat)),
            "go_album" => {
                if let Some(track) = track_index.and_then(|i| tracks.get(i)) {
                    Self::go(m, Screen::Album, album_filter(track));
                }
                Effect::None
            }
            "go_artist" => {
                if let Some(track) = track_index.and_then(|i| tracks.get(i)) {
                    Self::go(m, Screen::Artist, artist_filter(track));
                }
                Effect::None
            }
            "go_source" => self.go_to_origin(m, tracks),
            "track_info" => {
                Self::go(
                    m,
                    Screen::TrackInfo,
                    track_index.map(|i| i.to_string()).unwrap_or_default(),
                );
                Effect::None
            }
            "queue_play" => queue_target.map(Effect::PlayQueue).unwrap_or(Effect::None),
            "queue_remove" => queue_target
                .map(Effect::QueueRemove)
                .unwrap_or(Effect::None),
            "queue_up" | "queue_down" => queue_target
                .map(|index| Effect::QueueMove {
                    index,
                    delta: if menu_key == "queue_up" { -1 } else { 1 },
                })
                .unwrap_or(Effect::None),
            "queue_next" => Self::entry_of(m, &source_key)
                .map(|id| Effect::QueueEntriesNext(vec![id]))
                .unwrap_or(Effect::None),
            "queue_last" => Self::entry_of(m, &source_key)
                .map(|id| Effect::QueueEntriesLast(vec![id]))
                .unwrap_or(Effect::None),
            "queue_move" => {
                if let Some(id) = Self::entry_of(m, &source_key) {
                    self.start_moving(id);
                }
                Effect::None
            }
            "select_songs" => {
                self.start_selection(m, &source_key);
                Effect::None
            }
            "clear_queued" => Effect::ClearQueued,
            _ => Effect::None,
        }
    }

    /// The keyboard has the buttons: see `keyboard` and the router's keyboard
    /// mode. Playback buttons never reach here while it is open.
    pub(crate) fn entry(&mut self, m: &mut AppModel, tracks: &[Track], action: Action) -> Effect {
        use keyboard::{Outcome, Purpose};
        let Some(k) = self.keyboard.as_mut() else {
            return Effect::None;
        };
        match k.action(action) {
            Outcome::Stay => Effect::None,
            Outcome::Edited => {
                if k.purpose == Purpose::LibrarySearch {
                    self.search_typed(m, tracks);
                }
                Effect::None
            }
            Outcome::Browse => {
                self.fold_search(m, tracks);
                Effect::None
            }
            Outcome::Cancel => {
                // Cancel or a held Back in the search leaves it, typed text
                // or not, to the screen it was opened from (#51).
                if self.keyboard.take().is_some_and(|k| k.live()) {
                    Self::leave_search(m);
                }
                Effect::None
            }
            Outcome::Done => {
                let text = k.text.trim().to_owned();
                match k.purpose.clone() {
                    purpose @ (Purpose::PlaylistNew | Purpose::PlaylistRename) => {
                        if text.is_empty() {
                            self.flash("Type a name for the playlist");
                            return Effect::None;
                        }
                        self.keyboard = None;
                        self.playlist_named(&purpose, text)
                    }
                    Purpose::WifiName => {
                        if text.is_empty() || text.len() > 32 {
                            self.flash("Type the network's name (up to 32 characters)");
                            return Effect::None;
                        }
                        self.open_wifi_keyboard(text, "", true);
                        Effect::None
                    }
                    Purpose::WifiPassword { ssid, hidden } => {
                        // WPA passphrases keep their spaces. A hidden network may be open.
                        if k.text.len() < 8 && !(hidden && k.text.is_empty()) {
                            self.flash("Wi-Fi passwords have at least 8 characters");
                            return Effect::None;
                        }
                        let password = std::mem::take(&mut k.text);
                        self.keyboard = None;
                        self.wifi_typed = Some(crate::selects::TypedJoin {
                            ssid: ssid.clone(),
                            password: password.clone(),
                            hidden,
                        });
                        Effect::WifiConnect {
                            ssid,
                            password,
                            hidden,
                        }
                    }
                    Purpose::LastFmUser => {
                        if text.is_empty() {
                            self.flash("Type your Last.fm username");
                            return Effect::None;
                        }
                        self.keyboard = Some(keyboard::Keyboard::new(
                            Purpose::LastFmPassword { username: text },
                            "",
                        ));
                        Effect::None
                    }
                    Purpose::LastFmPassword { username } => {
                        if k.text.is_empty() {
                            self.flash("Type your Last.fm password");
                            return Effect::None;
                        }
                        let password = std::mem::take(&mut k.text);
                        self.keyboard = None;
                        Effect::LastFm(reborn_core::LastFmRequest::Login { username, password })
                    }
                    Purpose::RadioSearch => {
                        if text.chars().count() < 2 {
                            self.flash("Type at least two letters");
                            return Effect::None;
                        }
                        self.keyboard = None;
                        self.radio_query = text.clone();
                        let title = format!("“{text}”");
                        self.radio.title = title.clone();
                        self.radio.stations.clear();
                        self.radio.loading = true;
                        self.radio.error = None;
                        self.radio.last_request = Some(RadioRequest::Search(text.clone()));
                        Self::go(m, Screen::RadioStations, &title);
                        Effect::Radio(RadioRequest::Search(text))
                    }
                    Purpose::LibrarySearch => {
                        if text.is_empty() {
                            self.flash("Type a name to search for");
                            return Effect::None;
                        }
                        self.fold_search(m, tracks);
                        Effect::None
                    }
                }
            }
        }
    }

    /// Now Playing's Go to row for the place the music was played from
    /// (#25, #60), while the song playing came from it (not one you queued)
    /// and that place is still there.
    pub(crate) fn origin_label(&self, m: &AppModel) -> Option<&'static str> {
        let origin = m.queue_origin.as_ref()?;
        if m.current_entry_id().is_none_or(|id| m.is_queued(id)) {
            return None;
        }
        Some(match origin.screen {
            Screen::Album => "Go to Album",
            Screen::Artist => "Go to Artist",
            Screen::Playlist
                if !self.playlists.loaded
                    || self
                        .playlists
                        .get(std::path::Path::new(&origin.filter))
                        .is_some() =>
            {
                "Go to Playlist"
            }
            Screen::Folders => "Go to Folder",
            Screen::Search => "Go to Search",
            Screen::Tracks if origin.filter.starts_with("genre:") => "Go to Genre",
            Screen::Tracks if origin.filter.is_empty() => "Go to Songs",
            _ => return None,
        })
    }

    /// Open the page the music was played from, the song playing focused.
    pub(crate) fn go_to_origin(&mut self, m: &mut AppModel, tracks: &[Track]) -> Effect {
        let Some(origin) = m.queue_origin.clone() else {
            return Effect::None;
        };
        let current = self.current_index(m, tracks);
        Self::go(m, origin.screen, origin.filter);
        if m.screen == Screen::Playlist && !self.playlists.loaded {
            // After a restart: the playlist shows once they are read.
            self.playlists.loading = true;
            return Effect::Playlist(Box::new(reborn_core::PlaylistRequest::Refresh));
        }
        let Some(current) = current else {
            return Effect::None;
        };
        let row = if m.screen == Screen::Playlist {
            let resolved = self
                .playlists
                .get(std::path::Path::new(&m.navigation.filter))
                .map(|p| self.resolved(tracks, p))
                .unwrap_or_default();
            self.with_playlist_rows(m, tracks, |rows| {
                rows.iter().position(|r| {
                    r.key
                        .strip_prefix("pl_entry:")
                        .and_then(|i| i.parse::<usize>().ok())
                        .is_some_and(|i| resolved.get(i).copied().flatten() == Some(current))
                })
            })
        } else if Self::is_catalog(m.screen) {
            let mut c = self.catalog.borrow_mut();
            self.ensure_catalog(&mut c, m, tracks);
            c.rows.iter().position(|r| r.track() == Some(current))
        } else {
            None
        };
        if let Some(row) = row {
            self.move_focus(m, tracks, row as i32);
        }
        Effect::None
    }

    /// The library index of the song playing.
    pub(crate) fn current_index(&self, m: &AppModel, tracks: &[Track]) -> Option<usize> {
        let current = m.current()?;
        tracks
            .iter()
            .position(|t| t.id == current.id && t.source_id == current.source_id)
    }

    /// The heart in a song's menu.
    pub(crate) fn favorite_row(
        &self,
        m: &AppModel,
        tracks: &[Track],
        index: Option<usize>,
    ) -> Item {
        let favorite = index.is_some_and(|i| self.is_favorite_track(m, tracks, i));
        Item::new(
            if favorite {
                "Remove from Favorites"
            } else {
                "Add to Favorites"
            },
            "fav_toggle",
        )
    }
}
