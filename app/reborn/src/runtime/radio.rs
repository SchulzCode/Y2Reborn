//! Internet radio: the directory requests, playing a station, favourites and zapping.
use crate::*;

impl Runtime {
    /// Internet radio requests from the Radio screens and Now Playing.
    pub(crate) fn radio(&mut self, request: reborn_core::RadioRequest) -> Result<(), String> {
        use reborn_core::RadioRequest;
        use reborn_platform::radio::{own_stations, Directory};
        let local = |ui: &mut Ui, stations: Vec<reborn_core::RadioStation>| {
            ui.radio.set_stations(stations);
            ui.radio.loading = false;
            ui.radio.error = None;
        };
        match request {
            RadioRequest::Favorites => local(&mut self.ui, self.radio_favorites.clone()),
            RadioRequest::Own => {
                let stations = own_stations(&self.storage_roots());
                local(&mut self.ui, stations);
            }
            RadioRequest::Play(index) => {
                let station = self
                    .ui
                    .radio
                    .stations
                    .get(index)
                    .cloned()
                    .ok_or("station not found")?;
                self.play_radio(station)?;
            }
            RadioRequest::ToggleFavorite(index) => {
                let station = self
                    .ui
                    .radio
                    .stations
                    .get(index)
                    .cloned()
                    .ok_or("station not found")?;
                self.toggle_favorite(station)?;
            }
            RadioRequest::ToggleFavoriteCurrent => {
                let station = self.radio_current.clone().ok_or("no station is playing")?;
                self.toggle_favorite(station)?;
            }
            RadioRequest::PlayStation(station) => self.play_radio(*station)?,
            RadioRequest::ToggleFavoriteStation(station) => self.toggle_favorite(*station)?,
            network => {
                self.radio_sequence += 1;
                let sequence = self.radio_sequence;
                let reply = self.radio_replies.0.clone();
                let directory = self.radio_directory.clone();
                std::thread::Builder::new()
                    .name("radio-directory".into())
                    .spawn(move || {
                        // Mirror discovery (DNS) once, then reuse it.
                        let directory = {
                            let mut slot = directory.lock().unwrap_or_else(|e| e.into_inner());
                            slot.get_or_insert_with(|| Arc::new(Directory::discover()))
                                .clone()
                        };
                        let message = match network {
                            RadioRequest::Countries => {
                                RadioReply::Categories(sequence, directory.countries())
                            }
                            RadioRequest::Genres => {
                                RadioReply::Categories(sequence, directory.genres())
                            }
                            RadioRequest::Country { code, .. } => {
                                RadioReply::Stations(sequence, directory.by_country(&code))
                            }
                            RadioRequest::Genre(tag) => {
                                RadioReply::Stations(sequence, directory.by_genre(&tag))
                            }
                            RadioRequest::Search(name) => {
                                RadioReply::Stations(sequence, directory.by_name(&name))
                            }
                            _ => RadioReply::Stations(sequence, directory.popular()),
                        };
                        let _ = reply.send(message);
                    })
                    .map_err(|e| e.to_string())?;
            }
        }
        self.dirty.mark_render();
        Ok(())
    }
    /// A reply from the directory worker; replies to older requests are dropped.
    pub(crate) fn radio_reply(&mut self, reply: RadioReply) {
        let (sequence, error) = match reply {
            RadioReply::Stations(sequence, result) if sequence == self.radio_sequence => {
                match result {
                    Ok(stations) => {
                        self.refresh_favorites(&stations);
                        // A country's, a genre's and the popular list keep
                        // their ten most listened stations on top.
                        use reborn_core::RadioRequest::{Country, Genre, Popular};
                        match self.ui.radio.last_request {
                            Some(Country { .. } | Genre(_) | Popular) => {
                                self.ui.radio.set_stations_ranked(stations, TOP_STATIONS)
                            }
                            _ => self.ui.radio.set_stations(stations),
                        }
                        (sequence, None)
                    }
                    Err(e) => (sequence, Some(e)),
                }
            }
            RadioReply::Categories(sequence, result) if sequence == self.radio_sequence => {
                match result {
                    Ok(list) => {
                        self.ui.radio.set_categories(
                            list.into_iter()
                                .map(|c| reborn_ui::RadioCategory {
                                    id: c.id,
                                    name: c.name,
                                    stations: c.stations,
                                })
                                .collect(),
                        );
                        (sequence, None)
                    }
                    Err(e) => (sequence, Some(e)),
                }
            }
            _ => return,
        };
        let _ = sequence;
        self.ui.radio.loading = false;
        self.ui.radio.error = error;
        self.dirty.mark_render();
    }
    pub(crate) fn play_radio(&mut self, station: reborn_core::RadioStation) -> Result<(), String> {
        if self.ui.wifi.status == WifiStatus::Disconnected {
            return Err("Connect to Wi-Fi to listen to radio".into());
        }
        self.save_book();
        self.model.replace_queue(vec![Track::radio(&station)], 0)?;
        self.model.set_source("Radio", None);
        self.radio_live = None;
        // Previous/Next move through the list the station was chosen from.
        if self
            .ui
            .radio
            .stations
            .iter()
            .any(|s| s.key() == station.key())
        {
            self.radio_list = self.ui.radio.stations.clone();
        } else if !self.radio_list.iter().any(|s| s.key() == station.key()) {
            self.radio_list.clear();
        }
        self.ui.radio.now = Some(station.clone());
        self.ui.radio.set_song(None);
        self.radio_art = None;
        self.radio_art_pending = None;
        self.art = false;
        self.load()?;
        if !station.uuid.is_empty() {
            // The directory counts plays for its popularity lists; best effort.
            let directory = self.radio_directory.clone();
            let uuid = station.uuid.clone();
            let _ = std::thread::Builder::new()
                .name("radio-click".into())
                .spawn(move || {
                    let directory = {
                        let mut slot = directory.lock().unwrap_or_else(|e| e.into_inner());
                        slot.get_or_insert_with(|| {
                            Arc::new(reborn_platform::radio::Directory::discover())
                        })
                        .clone()
                    };
                    let _ = directory.play_url(&uuid);
                });
        }
        self.log.emit(Level::Info, "radio", "station_started", "Internet radio station started", None,
            json!({"codec":station.codec,"bitrate":station.bitrate,"directory":!station.uuid.is_empty()}));
        self.radio_current = Some(station);
        Ok(())
    }
    pub(crate) fn toggle_favorite(
        &mut self,
        station: reborn_core::RadioStation,
    ) -> Result<(), String> {
        let key = station.key().to_owned();
        if let Some(i) = self.radio_favorites.iter().position(|s| s.key() == key) {
            self.radio_favorites.remove(i);
            self.ui.flash("Removed from Favorites");
        } else {
            if self.radio_favorites.len() >= 200 {
                return Err("Favorites are full (200 stations)".into());
            }
            self.radio_favorites.push(station);
            self.ui.flash("Added to Favorites");
        }
        reborn_platform::radio::save_favorites(
            &self.root.join("state/radio.json"),
            &self.radio_favorites,
        )
        .map_err(|e| format!("Couldn't save favorites: {e}"))?;
        self.ui.radio.favorites = self
            .radio_favorites
            .iter()
            .map(|s| s.key().to_owned())
            .collect();
        // The Favorites list on screen follows at once.
        if self.ui.radio.last_request == Some(reborn_core::RadioRequest::Favorites) {
            self.ui.radio.set_stations(self.radio_favorites.clone());
        }
        Ok(())
    }
    /// Favorites saved before the directory sent logos and details get them
    /// when the station turns up in a list again.
    pub(crate) fn refresh_favorites(&mut self, fresh: &[reborn_core::RadioStation]) {
        let mut changed = false;
        for saved in &mut self.radio_favorites {
            if saved.uuid.is_empty() {
                continue;
            }
            if let Some(f) = fresh.iter().find(|f| f.uuid == saved.uuid) {
                if f != saved {
                    *saved = f.clone();
                    changed = true;
                }
            }
        }
        if changed {
            let _ = reborn_platform::radio::save_favorites(
                &self.root.join("state/radio.json"),
                &self.radio_favorites,
            );
        }
    }
    /// Previous/Next while a station plays: the neighbouring station of the
    /// list it was started from.
    pub(crate) fn zap(&mut self, delta: i32) -> Result<bool, String> {
        let Some(current) = self.radio_current.as_ref() else {
            return Ok(false);
        };
        let Some(at) = self
            .radio_list
            .iter()
            .position(|s| s.key() == current.key())
        else {
            return Ok(false);
        };
        let n = self.radio_list.len() as isize;
        if n < 2 {
            return Ok(false);
        }
        let next = self.radio_list[(at as isize + delta as isize).rem_euclid(n) as usize].clone();
        self.play_radio(next)?;
        Ok(true)
    }
    /// The live line under a playing station: its announced song, or how the
    /// connection is doing.
    pub(crate) fn radio_tick(&mut self) {
        let radio = self.model.current().is_some_and(Track::is_radio);
        if !radio {
            if self.ui.radio.now.take().is_some() {
                self.ui.radio.set_song(None);
                self.radio_art = None;
                self.dirty.mark_render();
            }
            return;
        }
        use reborn_platform::radio::StreamState;
        let song = match reborn_platform::radio::live() {
            Some((StreamState::Playing, Some(title))) => Some(title),
            _ => None,
        };
        if self.ui.radio.song.is_some() != song.is_some()
            || song
                .as_deref()
                .is_some_and(|t| self.radio_live.as_deref() != Some(t))
        {
            self.ui.radio.set_song(song.as_deref());
        }
        let line = match reborn_platform::radio::live() {
            Some((StreamState::Connecting, _)) => "Connecting…".to_owned(),
            Some((StreamState::Reconnecting { attempt }, _)) => {
                format!("Reconnecting… (try {attempt})")
            }
            Some((StreamState::Failed(reason), _)) => reason,
            Some((StreamState::Playing, Some(title))) => title,
            Some((StreamState::Playing, None)) => "Live".to_owned(),
            None => return,
        };
        if self.radio_live.as_deref() != Some(line.as_str()) {
            if let Some((StreamState::Failed(reason), _)) = reborn_platform::radio::live() {
                // Only the Now Playing line showed why a station failed (F19).
                let (status, content_type) = reborn_platform::radio::last_reply().unzip();
                self.log.emit(
                    Level::Warn,
                    "radio",
                    "station_failed",
                    &reason,
                    None,
                    json!({"http_status":status,"content_type":content_type,
                        "directory":self.radio_current.as_ref().map(|s| !s.uuid.is_empty())}),
                );
            }
            let position = self.model.queue_position;
            if let Some(track) = self.model.queue.get_mut(position) {
                track.artist = line.clone();
            }
            self.radio_live = Some(line);
            self.dirty.mark_render();
        }
    }
}
