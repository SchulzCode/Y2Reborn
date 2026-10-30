//! Playlists and the queue: playing and queueing members, the history.
use crate::*;

impl Runtime {
    /// Playlist requests from the Playlist screens and song menus.
    pub(crate) fn playlist(&mut self, request: reborn_core::PlaylistRequest) -> Result<(), String> {
        use reborn_core::PlaylistRequest as R;
        let worker = self
            .playlist_worker
            .as_ref()
            .ok_or("playlists unavailable")?;
        let job = match request {
            R::Refresh => {
                self.ui.playlists.loading = true;
                playlists::Job::Refresh(self.storage_roots())
            }
            R::Create { name, tracks } => {
                let dir = self
                    .model
                    .settings
                    .music_directory
                    .join(reborn_platform::playlists::FOLDER);
                let file = reborn_platform::playlists::file_name_for(&name)
                    .ok_or("Type a name for the playlist")?;
                let path = dir.join(format!("{file}.m3u8"));
                let roots = self.storage_roots();
                let entries = tracks
                    .iter()
                    .filter_map(|&i| self.model.library.tracks.get(i))
                    .filter(|t| !t.is_radio())
                    .map(|t| reborn_core::playlist::PlaylistEntry {
                        location: reborn_ui::playlist_index::location_for(&t.path, &path, &roots),
                        info: Some(reborn_ui::playlist_index::info_for(t)),
                        extra: vec![],
                    })
                    .collect();
                playlists::Job::Create { dir, name, entries }
            }
            R::Edit { path, edit, create } => playlists::Job::Edit { path, edit, create },
            R::Rename { path, name } => playlists::Job::Rename { path, name },
            R::Duplicate(path) => playlists::Job::Duplicate(path),
            R::Delete(path) => playlists::Job::Delete(path),
        };
        worker.send(job);
        self.dirty.mark_render();
        Ok(())
    }
    pub(crate) fn playlist_reply(&mut self, reply: playlists::Reply) {
        match reply {
            playlists::Reply::Listed(lists) => self.ui.playlists.set_lists(lists),
            playlists::Reply::Saved { playlist, created } => {
                let path = playlist.path.clone();
                let name = playlist.name.clone();
                self.ui.playlists.replace(playlist);
                if created && !reborn_ui::playlists_is_favorites(&path) {
                    self.ui.flash(format!("Created “{name}”"));
                    if self.model.screen == Screen::Playlists {
                        self.ui.show_playlist(&mut self.model, &path);
                    }
                }
            }
            playlists::Reply::Renamed { old, playlist } => {
                let new = playlist.path.clone();
                self.ui.playlists.remove(&old);
                self.ui.playlists.replace(playlist);
                self.ui.playlist_moved(&mut self.model, &old, Some(&new));
            }
            playlists::Reply::Deleted(path) => {
                self.ui.playlists.remove(&path);
                self.ui.playlist_moved(&mut self.model, &path, None);
                self.ui.flash("Playlist deleted");
            }
            playlists::Reply::Failed(error) => {
                self.ui.playlists.loading = false;
                self.ui.flash(error);
            }
        }
        self.dirty.mark_render();
    }
    /// Internal Storage and online SD cards, where Themes, Radio and EQ live.
    pub(crate) fn storage_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.model.settings.music_directory.clone()];
        roots.extend(
            self.model
                .sources
                .iter()
                .filter(|s| s.online && matches!(s.kind, reborn_core::MediaSource::SdCard(_)))
                .map(|s| s.root.clone()),
        );
        roots
    }
    pub(crate) fn play_members(
        &mut self,
        index: usize,
        members: Vec<usize>,
        shuffle: ShuffleMode,
        source: String,
        origin: Option<reborn_core::QueueOrigin>,
    ) -> Result<(), String> {
        let online = self
            .model
            .library
            .tracks
            .get(index)
            .map(|track| track.online)
            .ok_or("invalid selection")?;
        if !online {
            return Err("media source offline".into());
        }
        // Positions first, songs last: a 28 600-song collection is cut to
        // MAX_QUEUE before any song is copied.
        let tracks = &self.model.library.tracks;
        let mut members = members
            .into_iter()
            .filter(|&member| tracks.get(member).is_some_and(|track| track.online))
            .collect::<Vec<_>>();
        let selected_position = members
            .iter()
            .position(|&member| member == index)
            .ok_or("selected track is not in the playback collection")?;
        // A Shuffle row says how; a song or Play follows the setting.
        let force_shuffle = shuffle != ShuffleMode::Off;
        let mode = if force_shuffle {
            shuffle
        } else {
            self.model.settings.shuffle_mode()
        };
        // The source's own order, for Shuffle off later (by library index:
        // a whole card is 120 000 songs).
        let mut rank = vec![usize::MAX; tracks.len()];
        for (at, &member) in members.iter().enumerate() {
            rank[member] = at;
        }
        let (window, start) = if mode == ShuffleMode::Albums {
            // Whole albums in random order; a picked song's album first,
            // from that song on (Beta 8.39).
            let picked = {
                let refs: Vec<&Track> = members.iter().map(|&m| &tracks[m]).collect();
                reborn_core::queue::album_shuffle(
                    &refs,
                    (!force_shuffle).then_some(selected_position),
                    shuffle_seed(),
                )
            };
            members = picked.into_iter().map(|p| members[p]).collect();
            (0..members.len().min(reborn_core::MAX_QUEUE), 0)
        } else if mode == ShuffleMode::Songs {
            if force_shuffle {
                // A Shuffle row: any song may come first.
                shuffle_from(&mut members, 0, shuffle_seed());
            } else {
                // A song picked with Shuffle on plays first, the rest follow.
                let selected = members.remove(selected_position);
                members.insert(0, selected);
                shuffle_from(&mut members, 1, shuffle_seed());
            }
            (0..members.len().min(reborn_core::MAX_QUEUE), 0)
        } else {
            let window =
                reborn_core::queue_window(members.len(), selected_position, reborn_core::MAX_QUEUE);
            let start = selected_position - window.start;
            (window, start)
        };
        let chosen = &members[window];
        let queue = chosen
            .iter()
            .filter_map(|&member| tracks.get(member).cloned())
            .collect::<Vec<_>>();
        let mut natural: Vec<usize> = (0..chosen.len()).collect();
        natural.sort_by_key(|&i| rank.get(chosen[i]).copied().unwrap_or(usize::MAX));
        self.save_book();
        self.model.replace_queue(queue, start)?;
        self.model.set_source(source, Some(&natural));
        self.model.queue_origin = origin;
        self.load()
    }

    /// A new song plays: the one before goes into the history.
    pub(crate) fn note_history(&mut self) {
        let now = self
            .model
            .current()
            .map(|t| (self.model.current_entry_id(), t.id));
        if now == self.history_seen {
            return;
        }
        if let Some(previous) = self.history_track.take() {
            self.model.note_played(&previous);
            self.dirty.mark_persistence();
        }
        self.history_seen = now;
        self.history_track = self.model.current().cloned();
    }

    /// The queue changed after the song playing: the decoder learns the
    /// songs to come without the song starting over.
    pub(crate) fn upcoming_changed(&mut self) {
        let (tracks, ids) = self.model.upcoming(UPCOMING);
        self.playback
            .set_upcoming(self.model.generation, tracks, ids);
    }

    /// Play Next (`next`) or Add to Queue. An empty queue becomes the songs;
    /// Play Next then starts them.
    pub(crate) fn queue_songs(&mut self, members: Vec<usize>, next: bool) -> Result<(), String> {
        let tracks = members
            .into_iter()
            .filter_map(|index| self.model.library.tracks.get(index).cloned())
            .filter(|track| track.online)
            .collect::<Vec<_>>();
        let count = tracks.len();
        if count == 0 {
            return Err("media source offline".into());
        }
        let fresh = if next {
            self.model.queue_next(tracks)?
        } else {
            self.model.queue_last(tracks)?
        };
        if fresh && next {
            self.load()?;
        } else if fresh {
            self.model.playback = PlaybackState::Paused;
        } else {
            self.upcoming_changed();
        }
        self.ui.flash(match (next, count) {
            (true, 1) => "Plays next".to_owned(),
            (true, n) => format!("{n} songs play next"),
            (false, 1) => "Added to queue".to_owned(),
            (false, n) => format!("{n} songs added to queue"),
        });
        self.checkpoint();
        Ok(())
    }

    pub(crate) fn fail(&mut self, sub: &str, error: String) {
        let peer_gone = selected_peer_gone(&self.model.output, &self.bt_state, &error);
        self.ui.flash(
            refused_card_error(&error, self.model.platform.snapshot.storage.sd)
                .map_or_else(|| friendly_error(&error), str::to_owned),
        );
        self.log.emit(
            Level::Error,
            sub,
            "operation_failed",
            &error,
            None,
            json!({"state":self.model.playback,"recovery_attempted":peer_gone}),
        );
        if peer_gone {
            // The disconnect itself was never seen: recover as if it had been.
            self.bluetooth_peer_lost("paused_after_failed_start");
        }
        self.dirty.mark_render();
    }
}
