//! The rows a screen lists: catalog, queue, context and modal menus.
use crate::*;

impl Ui {
    pub fn rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        match m.screen {
            Screen::Artists
            | Screen::Albums
            | Screen::Genres
            | Screen::Folders
            | Screen::Artist
            | Screen::Album
            | Screen::Tracks
            | Screen::Search => self.catalog_rows(m, tracks),
            Screen::Queue => self.queue_rows(m, tracks),
            Screen::Playlists | Screen::Playlist => self.playlist_rows(m, tracks),
            Screen::Audiobooks => views_books::book_rows(self, m, tracks),
            Screen::Book => views_books::page_rows(self, m, tracks),
            Screen::Diagnostics => diagnostics::root(m),
            Screen::DiagnosticSection => diagnostics::section_rows(m, &m.navigation.filter),
            Screen::ValueDetail => vec![Item::new("Back", "back")],
            Screen::TextEntry | Screen::Pairing => vec![],
            _ => pages::page(self, m, tracks).rows,
        }
    }

    pub fn collection_track<'a>(&self, m: &AppModel, tracks: &'a [Track]) -> Option<&'a Track> {
        let mut catalog = self.catalog.borrow_mut();
        self.ensure_catalog(&mut catalog, m, tracks);
        catalog
            .rows
            .iter()
            .find_map(|row| row.track().and_then(|i| tracks.get(i)))
    }
    pub fn invalidate_catalog(&self) {
        self.catalog.borrow_mut().clear();
    }
    /// The catalog for this screen, with the playlists the search looks
    /// through as Playlists lists them (refreshed when they change).
    pub(crate) fn ensure_catalog(&self, c: &mut catalog::Catalog, m: &AppModel, tracks: &[Track]) {
        let generation = self.playlists.generation();
        if m.screen == Screen::Search && c.playlists_generation != Some(generation) {
            let items = self
                .playlists
                .lists
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let facts = self.facts(tracks, p);
                    let mut item = Item::new(&p.name, format!("playlist:{i}"))
                        .with_secondary(playlists_ui::summary(p.entries.len(), facts.ms));
                    item.art = facts.mosaic.first().copied();
                    item
                })
                .collect();
            c.set_playlists(generation, items);
        }
        c.ensure(m, tracks);
    }
    pub(crate) fn catalog_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        let mut c = self.catalog.borrow_mut();
        self.ensure_catalog(&mut c, m, tracks);
        c.rows.iter().map(|r| r.item(tracks)).collect()
    }
    pub(crate) fn track_rows_page(
        &self,
        m: &AppModel,
        tracks: &[Track],
        offset: usize,
        limit: usize,
    ) -> Vec<Item> {
        let mut c = self.catalog.borrow_mut();
        self.ensure_catalog(&mut c, m, tracks);
        c.rows
            .iter()
            .skip(offset)
            .take(limit)
            .map(|r| r.item(tracks))
            .collect()
    }
    pub(crate) fn row_count(&self, m: &AppModel, tracks: &[Track]) -> usize {
        if Self::is_catalog(m.screen) {
            let mut c = self.catalog.borrow_mut();
            self.ensure_catalog(&mut c, m, tracks);
            c.rows.len()
        } else if m.screen == Screen::Queue {
            self.up_next(m, tracks).len()
        } else if matches!(m.screen, Screen::Playlists | Screen::Playlist) {
            self.with_playlist_rows(m, tracks, <[Item]>::len)
        } else {
            self.rows(m, tracks).len()
        }
    }
    /// Lists that wrap around only when Wrap Around is Everywhere: the
    /// library, the queue, playlists, stations and the time zones.
    pub(crate) fn is_long_list(s: Screen) -> bool {
        Self::is_catalog(s)
            || matches!(
                s,
                Screen::Queue
                    | Screen::Playlists
                    | Screen::Playlist
                    | Screen::RadioStations
                    | Screen::TimeZone
                    | Screen::Audiobooks
                    | Screen::Book
            )
    }

    pub(crate) fn is_catalog(s: Screen) -> bool {
        matches!(
            s,
            Screen::Albums
                | Screen::Artists
                | Screen::Genres
                | Screen::Folders
                | Screen::Album
                | Screen::Artist
                | Screen::Tracks
                | Screen::Search
        )
    }
    pub(crate) fn row_at(&self, m: &AppModel, tracks: &[Track], index: usize) -> Option<Item> {
        let mut item = self.row_at_plain(m, tracks, index)?;
        // Select Songs: chosen rows carry a check (skins draw it as one).
        if let Some(on) = self.selection_mark(m, &item.key) {
            item.kind = RowKind::Check(on);
        }
        Some(item)
    }
    pub(crate) fn row_at_plain(
        &self,
        m: &AppModel,
        tracks: &[Track],
        index: usize,
    ) -> Option<Item> {
        if Self::is_catalog(m.screen) {
            self.track_rows_page(m, tracks, index, 1).pop()
        } else if m.screen == Screen::Queue {
            self.up_next_item(m, tracks, &self.up_next(m, tracks), index)
        } else if matches!(m.screen, Screen::Playlists | Screen::Playlist) {
            self.with_playlist_rows(m, tracks, |rows| rows.get(index).cloned())
        } else {
            self.rows(m, tracks).get(index).cloned()
        }
    }
    pub(crate) fn visible_rows(
        &self,
        m: &AppModel,
        tracks: &[Track],
        offset: usize,
        count: usize,
    ) -> Vec<Item> {
        if Self::is_catalog(m.screen) || m.screen == Screen::Queue {
            (offset..offset + count)
                .filter_map(|i| self.row_at(m, tracks, i))
                .collect()
        } else if matches!(m.screen, Screen::Playlists | Screen::Playlist) {
            self.with_playlist_rows(m, tracks, |rows| {
                rows.iter().skip(offset).take(count).cloned().collect()
            })
        } else {
            self.rows(m, tracks)
                .into_iter()
                .skip(offset)
                .take(count)
                .collect()
        }
    }
    pub(crate) fn queue_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        let layout = self.up_next(m, tracks);
        (0..layout.len())
            .filter_map(|i| self.up_next_item(m, tracks, &layout, i))
            .collect()
    }

    pub(crate) fn bluetooth_summary(&self) -> String {
        if !self.bluetooth.available {
            String::new()
        } else if !self.bluetooth.powered {
            "Off".into()
        } else if let Some(device) = self.bluetooth.devices.iter().find(|d| d.usable_audio()) {
            device.name.clone()
        } else {
            "On".into()
        }
    }
    pub(crate) fn wifi_summary(&self) -> String {
        if !self.wifi.available {
            String::new()
        } else if !self.wifi.powered {
            "Off".into()
        } else {
            match &self.wifi.status {
                WifiStatus::Connected(n) => n.clone(),
                WifiStatus::NoInternet(n) => format!("{n} · No internet"),
                WifiStatus::Connecting(_) | WifiStatus::GettingAddress(_) => "Connecting…".into(),
                WifiStatus::Disconnected => "Not connected".into(),
            }
        }
    }

    pub(crate) fn device(&self, key: Option<&str>) -> Option<&BluetoothDeviceView> {
        let path = key?.strip_prefix("bt_device:")?;
        self.bluetooth.devices.iter().find(|d| d.path == path)
    }

    pub(crate) fn context_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        if m.screen == Screen::NowPlaying {
            let Some(current) = m.current() else {
                return vec![];
            };
            if current.is_radio() {
                let key = if current.filename.is_empty() {
                    current.path.to_string_lossy().into_owned()
                } else {
                    current.filename.clone()
                };
                let saved = self.radio.favorites.contains(&key);
                let mut rows = vec![
                    Item::new(
                        if saved {
                            "Remove from Favorites"
                        } else {
                            "Add to Favorites"
                        },
                        "radio_favorite_current",
                    ),
                    Item::new("Stop Radio", "radio_stop"),
                ];
                if self.radio.now.is_some() {
                    rows.insert(1, Item::new("Station Info", "radio_info_current"));
                }
                return rows;
            }
            // A book: its chapters; shuffle and repeat do not apply to it.
            if let Some((book, _)) = m.book_part() {
                return vec![
                    Item::new("Chapters", "book_chapters"),
                    Item::new("Speed", "book_speed")
                        .with_secondary(format!("{}×", speed_label(book.speed))),
                    Item::new("Sleep Timer", "sleep_timer").with_secondary(sleep_label(m, true)),
                    Item::new("Up Next", "show_queue"),
                ];
            }
            // The song after this one, named where Up Next opens.
            let next = m
                .queue
                .get(m.queue_position + 1)
                .filter(|_| m.settings.repeat != RepeatMode::Track)
                .map(|t| track_title(t).to_owned())
                .unwrap_or_default();
            return vec![
                Item::new("Up Next", "show_queue").with_secondary(next),
                Item::new("Sleep Timer", "sleep_timer").with_secondary(sleep_label(m, false)),
                Item::new("Shuffle", "toggle_shuffle")
                    .with_secondary(m.settings.shuffle_mode().label()),
                Item::new("Repeat", "cycle_repeat").with_secondary(repeat_label(m.settings.repeat)),
                self.favorite_row(m, tracks, self.current_index(m, tracks)),
                Item::new("Add to Playlist…", "add_playlist"),
                // Back to where the music came from (#25, #60): the Go to
                // row of that place, Go to Album's for a playlist or folder.
                match self.origin_label(m) {
                    Some(label) if label != "Go to Artist" => Item::new(label, "go_source"),
                    _ => Item::new("Go to Album", "go_album"),
                },
                match self.origin_label(m) {
                    Some(label @ "Go to Artist") => Item::new(label, "go_source"),
                    _ => Item::new("Go to Artist", "go_artist"),
                },
            ];
        }
        let selected_key = m.navigation.context_key.clone().or_else(|| {
            m.navigation
                .context_target
                .and_then(|target| self.row_at(m, tracks, target).map(|item| item.key))
        });
        let key = selected_key.as_deref();
        if key == Some("selection") {
            return self.selection_menu(m);
        }
        // A playlist, also among search results (#44).
        if matches!(m.screen, Screen::Playlists | Screen::Playlist)
            || key.is_some_and(|k| k.starts_with("playlist:"))
        {
            return self.playlist_context_rows(m, key.unwrap_or(""));
        }
        if let Some(book) = key
            .and_then(|k| k.strip_prefix("book:"))
            .map(str::to_owned)
            .or_else(|| (m.screen == Screen::Book).then(|| m.navigation.filter.clone()))
        {
            return self.book_context_rows(m, tracks, &book);
        }
        if let Some(index) = key
            .and_then(|key| key.strip_prefix("track:"))
            .and_then(|i| i.parse::<usize>().ok())
        {
            // Every song menu reads the same (8.36): Play Next, Add to Queue,
            // what this list can change, keep it, go to it, then many songs.
            let mut rows = vec![
                Item::new("Play Next", "play_next"),
                Item::new("Add to Queue", "add_queue"),
                self.favorite_row(m, tracks, Some(index)),
                Item::new("Add to Playlist…", "add_playlist"),
                Item::new("Go to Album", "go_album"),
                Item::new("Go to Artist", "go_artist"),
            ];
            if Self::selects(m.screen) {
                rows.push(Item::new("Select Songs…", "select_songs"));
            }
            return rows;
        }
        // An album, artist, genre or folder row, the search's Play All.
        if let Some(key) = key.filter(|k| Self::group_key(k)) {
            return Self::group_menu(key);
        }
        if matches!(m.screen, Screen::Album | Screen::Artist) && key == Some("play:collection") {
            let mut rows = vec![
                Item::new("Play", "collection_play"),
                Item::new("Shuffle", "collection_shuffle"),
                Item::new("Play Next", "collection_next"),
                Item::new("Add to Queue", "collection_queue"),
                Item::new("Add to Playlist…", "collection_playlist"),
            ];
            // An artist's albums, each whole, in random order.
            if m.screen == Screen::Artist {
                rows.insert(2, Item::new("Shuffle Albums", "collection_shuffle_albums"));
            }
            return rows;
        }
        match m.screen {
            Screen::Albums | Screen::Artists | Screen::Genres | Screen::Search => vec![],
            Screen::RadioStations => {
                let Some(station) = key
                    .and_then(|k| k.strip_prefix("station:"))
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| self.radio.stations.get(i))
                else {
                    return vec![];
                };
                vec![
                    Item::new("Play", "radio_play"),
                    Item::new(
                        if self.radio.is_favorite(station) {
                            "Remove from Favorites"
                        } else {
                            "Add to Favorites"
                        },
                        "radio_favorite",
                    ),
                    Item::new("Station Info", "radio_info"),
                ]
            }
            Screen::Queue => self.up_next_menu(m, key),
            Screen::Bluetooth => {
                let Some(device) = self.device(key) else {
                    return vec![];
                };
                let mut rows = vec![Item::new(
                    match device.operation() {
                        "pair" => "Pair",
                        "disconnect" => "Disconnect",
                        _ => "Connect",
                    },
                    "bt_connect",
                )];
                let current = matches!(&m.output, AudioOutput::Bluetooth(a) if a.eq_ignore_ascii_case(&device.address));
                if device.usable_audio() && !current {
                    rows.push(Item::new("Use for Audio", "bt_output"));
                }
                if device.usable_audio() && !self.bluetooth.codec_choices.is_empty() {
                    rows.push(
                        Item::new("Codec", "bt_codec")
                            .with_secondary(m.settings.codec_preference.label()),
                    );
                }
                if device.saved_pairing() {
                    rows.push(Item::new("Forget Device", "bt_forget"));
                }
                rows
            }
            Screen::Wifi => {
                let Some(id) = key.and_then(|k| k.strip_prefix("saved:")) else {
                    return vec![];
                };
                let joined = self
                    .wifi
                    .networks
                    .iter()
                    .find(|n| n.saved_id.map(|v| v.to_string()).as_deref() == Some(id))
                    .is_some_and(|n| self.wifi.status.network() == Some(n.ssid.as_str()));
                let secured = self
                    .wifi
                    .networks
                    .iter()
                    .find(|n| n.saved_id.map(|v| v.to_string()).as_deref() == Some(id))
                    .is_some_and(|n| n.secured == Some(true));
                let mut items = vec![if joined {
                    Item::new("Disconnect", "wifi_disconnect")
                } else {
                    Item::new("Connect", "wifi_connect")
                }];
                if secured {
                    items.push(Item::new("Change Password", "wifi_change_password"));
                }
                items.push(Item::new("Forget Network", "wifi_forget"));
                items
            }
            _ => vec![],
        }
    }

    pub(crate) fn queue_target(&self, m: &AppModel, key: Option<&str>) -> Option<usize> {
        key.and_then(|s| s.strip_prefix("queue:"))
            .and_then(|s| s.parse::<u64>().ok())
            .and_then(|id| {
                m.queue_entry_ids
                    .iter()
                    .position(|v| v.0 == id)
                    .or_else(|| m.queue_entry_ids.is_empty().then_some(id as usize))
            })
            .filter(|i| *i < m.queue.len())
    }

    pub(crate) fn usable_outputs(&self) -> impl Iterator<Item = &BluetoothDeviceView> {
        self.bluetooth.devices.iter().filter(|d| d.usable_audio())
    }

    pub(crate) fn modal_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        match m.navigation.modal {
            Some(Modal::ContextMenu) => self.context_rows(m, tracks),
            Some(Modal::PlaylistPicker) => self.picker_rows(m),
            Some(Modal::OutputPicker) => {
                let mut rows = vec![Item::new("Headphone Jack", "output:wired").with_secondary(
                    if m.output == AudioOutput::Wired {
                        "Current"
                    } else {
                        ""
                    },
                )];
                rows.extend(self.usable_outputs().map(|d| {
                    let current = matches!(&m.output, AudioOutput::Bluetooth(a) if a.eq_ignore_ascii_case(&d.address));
                    Item::new(&d.name, format!("output:{}", d.path))
                        .with_secondary(if current { "Current" } else { "Bluetooth" })
                }));
                rows
            }
            Some(Modal::CodecPicker) => self
                .bluetooth
                .codec_choices
                .iter()
                .map(|c| {
                    Item::new(
                        if *c == CodecPreference::Auto {
                            "Automatic"
                        } else {
                            c.label()
                        },
                        format!("codec:{}", c.label()),
                    )
                    .with_secondary(if *c == m.settings.codec_preference {
                        "Preferred"
                    } else {
                        ""
                    })
                })
                .collect(),
            Some(Modal::EqBand(_)) => (-12..=12)
                .map(|gain| Item::new(format!("{gain:+} dB"), format!("eq_gain:{gain}")))
                .collect(),
            Some(Modal::DateField(field)) => date_time::picker(self, m, field).0,
            Some(Modal::SleepTimer) => {
                use reborn_core::SleepChoice;
                let set = m.sleep_timer.map(|t| t.choice);
                let book = m.book_part().is_some();
                SleepChoice::all()
                    .into_iter()
                    .map(|c| {
                        let (key, running) = match c {
                            SleepChoice::Off => ("sleep_set:off".to_owned(), String::new()),
                            SleepChoice::EndOfItem => ("sleep_set:end".to_owned(), "On".to_owned()),
                            SleepChoice::Minutes(n) => (
                                format!("sleep_set:{n}"),
                                sleep_minutes_left(m).map_or_else(String::new, |l| {
                                    format!("{} left", minutes_label(l))
                                }),
                            ),
                        };
                        // The running choice says so, a timed one how long it has left.
                        let detail = if set == Some(c) {
                            running
                        } else {
                            String::new()
                        };
                        Item::new(sleep_choice_label(c, book), key).with_secondary(detail)
                    })
                    .collect()
            }
            Some(Modal::Confirm(action)) => {
                let mut rows = vec![
                    Item::new("Cancel", "cancel"),
                    Item::new(confirm_label(action), "confirm"),
                ];
                if action == ConfirmAction::ReportsQuestion {
                    // A real answer either way, so the question is not asked again.
                    rows[0] = Item::new("No Thanks", "decline");
                }
                if let ConfirmAction::Platform(task) = action {
                    let u = &m.platform.snapshot.update;
                    let idle = m.platform.busy.is_none();
                    rows[1].enabled = idle
                        && match task {
                            PlatformTask::UpdateApply => u.can_install,
                            PlatformTask::UpdateCancel => u.can_cancel,
                            PlatformTask::UpdateRollback => u.can_rollback,
                            // Only the old storage the platform still reports waiting.
                            PlatformTask::InternalStorageSetup => matches!(
                                m.platform.snapshot.internal_setup,
                                reborn_core::platform::InternalSetup::Waiting { .. }
                            ),
                            // Only a card the platform marked (read-only or refused).
                            PlatformTask::SdRepair => m.platform.snapshot.storage.sd.repairable(),
                            _ => true,
                        };
                }
                rows
            }
            None => vec![],
        }
    }

    /// Focus the open menu's row with `key` (previews pick rows by name, so
    /// a menu's order can change under them); false when it has none.
    pub fn focus_menu_row(&self, m: &mut AppModel, tracks: &[Track], key: &str) -> bool {
        let at = self.modal_rows(m, tracks).iter().position(|r| r.key == key);
        if let Some(at) = at {
            m.navigation.modal_focus = at;
        }
        at.is_some()
    }

    pub(crate) fn modal_rows_public(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        self.modal_rows(m, tracks)
    }

    pub(crate) fn modal_copy(&self, m: &AppModel) -> (String, String) {
        let (title, body) = match m.navigation.modal {
            Some(Modal::OutputPicker) => ("Output", "Choose where music plays."),
            Some(Modal::CodecPicker) => (
                "Bluetooth Codec",
                "Your headphones use the closest codec they support.",
            ),
            Some(Modal::EqBand(index)) => {
                let defaults = reborn_core::flat_eq_bands();
                let bands = if m.settings.eq_bands.is_empty() {
                    &defaults
                } else {
                    &m.settings.eq_bands
                };
                let title = bands
                    .get(index)
                    .map(|b| pages::eq_frequency(b.frequency_hz))
                    .unwrap_or_else(|| "Equalizer".into());
                return (
                    title,
                    "Turn the wheel to choose gain. Select applies; Back cancels.".into(),
                );
            }
            Some(Modal::DateField(field)) => return date_time::picker_copy(field),
            Some(Modal::SleepTimer) => (
                "Sleep Timer",
                "Turn the wheel to choose. Select starts it; Back cancels.",
            ),
            Some(Modal::Confirm(action)) => confirm_copy(action),
            Some(Modal::PlaylistPicker) => ("Add to Playlist", "Songs are added at the end."),
            Some(Modal::ContextMenu) => {
                let key = m.navigation.context_key.as_deref();
                let named = match m.screen {
                    Screen::NowPlaying => Some("Now Playing".to_owned()),
                    Screen::Bluetooth => self.device(key).map(|d| d.name.clone()),
                    Screen::Wifi => key.and_then(|k| k.strip_prefix("saved:")).and_then(|id| {
                        self.wifi
                            .networks
                            .iter()
                            .find(|n| n.saved_id.map(|v| v.to_string()).as_deref() == Some(id))
                            .map(|n| n.ssid.clone())
                    }),
                    Screen::Queue => Some("Queue".to_owned()),
                    Screen::Playlists => key
                        .and_then(|k| k.strip_prefix("playlist:"))
                        .and_then(|i| i.parse::<usize>().ok())
                        .and_then(|i| self.playlists.lists.get(i))
                        .map(|p| p.name.clone()),
                    Screen::RadioStations => key
                        .and_then(|k| k.strip_prefix("station:"))
                        .and_then(|i| i.parse::<usize>().ok())
                        .and_then(|i| self.radio.stations.get(i))
                        .map(|s| s.name.clone()),
                    _ => None,
                };
                return (named.unwrap_or_else(|| "Options".into()), String::new());
            }
            None => ("", ""),
        };
        (title.into(), body.into())
    }
}
