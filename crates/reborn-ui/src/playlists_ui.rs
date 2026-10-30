//! Playlists as their own section: the list of playlists, one playlist's
//! songs, Add to Playlist from anywhere, Favorites with a heart, and moving a
//! song with the wheel. Files are read and written by the player
//! (`Effect::Playlist`); this module only decides and draws from what it has.

use crate::{keyboard, names_or_unknown, playlist_index, track_title, Item, Ui};
use reborn_core::{
    playlist::{Playlist, PlaylistEdit, PlaylistEntry},
    sort::{fold, natural_key, sort_key, SortAs},
    Action, AppModel, ConfirmAction, Effect, Modal, PlaylistRequest, PlaylistSort, Screen,
    ShuffleMode, Track,
};
use std::path::{Path, PathBuf};

/// The Playlists folder on Internal Storage and the Favorites file in it.
pub const FOLDER: &str = "Playlists";
pub const FAVORITES: &str = "Favorites";

#[derive(Default)]
pub struct PlaylistsView {
    /// Every playlist read, Favorites first, then A to Z.
    pub lists: Vec<Playlist>,
    pub loaded: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// Songs waiting for the playlist picker or a new playlist's name.
    pub pending: Vec<usize>,
    /// Playlists added to lately, the latest first (the picker's order).
    pub recent: Vec<PathBuf>,
    /// A song being moved: its place in the file and where it is now.
    pub moving: Option<(usize, usize)>,
    /// The playlist a confirmation or a rename is about.
    pub target: Option<PathBuf>,
    /// Bumped whenever `lists` changes (the rows cache's key).
    generation: u64,
}

/// What a playlist holds in the library, worked out once per change of the
/// file or the library (views read it every frame).
pub(crate) struct Facts {
    /// Per entry, its library track.
    pub resolved: Vec<Option<usize>>,
    /// Library songs that can play now, in order.
    pub playable: Vec<usize>,
    /// Up to four tracks of different albums (the mosaic).
    pub mosaic: Vec<usize>,
    /// Length of the songs found, and entries not found.
    pub ms: u64,
    pub missing: usize,
    pub members: std::collections::HashSet<usize>,
}

/// A playlist's file stamp, entry count and the library it was matched in.
pub(crate) type FactsKey = ((u64, i64), usize, (usize, usize));

/// What the rows of a Playlist screen depend on.
pub(crate) type RowsKey = (
    Screen,
    String,
    u64,
    usize,
    usize,
    Option<(usize, usize)>,
    PlaylistSort,
);

impl PlaylistsView {
    /// Bumped whenever the lists change.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub fn set_lists(&mut self, mut lists: Vec<Playlist>) {
        lists.sort_by_cached_key(|p| (!is_favorites(&p.path), sort_key(&p.name, SortAs::Title)));
        self.lists = lists;
        self.loaded = true;
        self.loading = false;
        self.generation += 1;
    }
    /// A playlist read or written again replaces its old copy.
    pub fn replace(&mut self, playlist: Playlist) {
        let mut lists = std::mem::take(&mut self.lists);
        lists.retain(|p| p.path != playlist.path);
        lists.push(playlist);
        self.set_lists(lists);
    }
    pub fn remove(&mut self, path: &Path) {
        self.generation += 1;
        self.lists.retain(|p| p.path != path);
        self.recent.retain(|p| p != path);
    }
    pub fn get(&self, path: &Path) -> Option<&Playlist> {
        self.lists.iter().find(|p| p.path == path)
    }
    fn used(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_owned());
        self.recent.truncate(16);
    }
}

pub fn favorites_path(music: &Path) -> PathBuf {
    music.join(FOLDER).join(format!("{FAVORITES}.m3u8"))
}

pub fn is_favorites(path: &Path) -> bool {
    path.file_stem()
        .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(FAVORITES))
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(FOLDER))
}

/// Storage roots for relative locations: Internal Storage and the cards.
fn roots(m: &AppModel) -> Vec<PathBuf> {
    let mut roots = vec![m.settings.music_directory.clone()];
    for s in &m.sources {
        if !roots.contains(&s.root) {
            roots.push(s.root.clone());
        }
    }
    roots
}

/// The Sort row of a playlist's menus: each press steps the order on.
fn sort_row(key: &str, sort: PlaylistSort) -> Item {
    Item::new("Sort", key).with_secondary(sort.label())
}

/// "24 songs · 1 h 32 min".
pub fn summary(count: usize, ms: u64) -> String {
    let songs = crate::components::count(count, "song", "songs");
    let minutes = ms / 60_000;
    let length = if minutes >= 60 {
        format!("{} h {} min", minutes / 60, minutes % 60)
    } else {
        format!("{minutes} min")
    };
    if ms == 0 {
        songs
    } else {
        format!("{songs} · {length}")
    }
}

impl Ui {
    /// Entries for these library songs, as `playlist`'s file names them.
    fn entries_for(
        &self,
        m: &AppModel,
        tracks: &[Track],
        indices: &[usize],
        playlist: &Path,
    ) -> Vec<PlaylistEntry> {
        let roots = roots(m);
        indices
            .iter()
            .filter_map(|&i| tracks.get(i))
            .filter(|t| !t.is_live())
            .map(|t| PlaylistEntry {
                location: playlist_index::location_for(&t.path, playlist, &roots),
                info: Some(playlist_index::info_for(t)),
                extra: vec![],
            })
            .collect()
    }

    /// What a playlist holds in the library, cached until it or the
    /// library changes.
    pub(crate) fn facts(&self, tracks: &[Track], playlist: &Playlist) -> std::rc::Rc<Facts> {
        let key: FactsKey = (
            playlist.stamp,
            playlist.entries.len(),
            crate::library_stamp(tracks),
        );
        if let Some((k, facts)) = self.playlist_facts.borrow().get(&playlist.path) {
            if *k == key {
                return facts.clone();
            }
        }
        let resolved = self
            .playlist_index
            .borrow_mut()
            .resolve_all(tracks, playlist)
            .to_vec();
        let online = |&i: &usize| tracks.get(i).is_some_and(|t| t.online);
        let playable: Vec<usize> = resolved.iter().flatten().copied().filter(online).collect();
        let mut mosaic: Vec<usize> = vec![];
        for &t in resolved.iter().flatten() {
            if mosaic.len() == 4 {
                break;
            }
            let track = &tracks[t];
            if !mosaic.iter().any(|&o| {
                tracks[o].album == track.album && tracks[o].album_artist == track.album_artist
            }) {
                mosaic.push(t);
            }
        }
        let facts = std::rc::Rc::new(Facts {
            ms: resolved
                .iter()
                .flatten()
                .map(|&t| tracks[t].duration_ms)
                .sum(),
            missing: resolved.iter().filter(|r| r.is_none()).count(),
            members: resolved.iter().flatten().copied().collect(),
            playable,
            mosaic,
            resolved,
        });
        self.playlist_facts
            .borrow_mut()
            .insert(playlist.path.clone(), (key, facts.clone()));
        facts
    }

    /// Each entry of a playlist as a library song (None: not on the player).
    pub(crate) fn resolved(&self, tracks: &[Track], playlist: &Playlist) -> Vec<Option<usize>> {
        self.facts(tracks, playlist).resolved.clone()
    }

    /// A playlist's entries in the order its page shows them (Sort, Beta
    /// 8.39): the file's order, or sorted with entries not on the player
    /// last.
    pub(crate) fn shown_order(
        &self,
        m: &AppModel,
        tracks: &[Track],
        playlist: &Playlist,
    ) -> Vec<usize> {
        let mut order: Vec<usize> = (0..playlist.entries.len()).collect();
        let sort = m.settings.playlist_sort(&playlist.path);
        if sort == PlaylistSort::Playlist {
            return order;
        }
        let facts = self.facts(tracks, playlist);
        let song = |i: usize| facts.resolved[i].and_then(|t| tracks.get(t));
        type Key = (bool, String, String, u32, u32, String, usize);
        order.sort_by_cached_key(|&i| -> Key {
            let Some(t) = song(i) else {
                return (true, String::new(), String::new(), 0, 0, String::new(), i);
            };
            let file = natural_key(&t.filename);
            match sort {
                PlaylistSort::Album => (
                    false,
                    sort_key(crate::album_artist_of(t), SortAs::Artist),
                    sort_key(&t.album, SortAs::Title),
                    t.disc,
                    t.track,
                    file,
                    i,
                ),
                PlaylistSort::Artist => (
                    false,
                    sort_key(&t.artist, SortAs::Artist),
                    sort_key(&t.album, SortAs::Title),
                    t.disc,
                    t.track,
                    file,
                    i,
                ),
                PlaylistSort::Title => (
                    false,
                    sort_key(track_title(t), SortAs::Title),
                    fold(&t.artist),
                    0,
                    0,
                    file,
                    i,
                ),
                PlaylistSort::FileName | PlaylistSort::Playlist => {
                    (false, file, String::new(), 0, 0, String::new(), i)
                }
            }
        });
        order
    }

    /// The songs that can play now in the order the page shows them.
    fn playable_shown(&self, m: &AppModel, tracks: &[Track], playlist: &Playlist) -> Vec<usize> {
        let facts = self.facts(tracks, playlist);
        self.shown_order(m, tracks, playlist)
            .into_iter()
            .filter_map(|i| facts.resolved[i])
            .filter(|&t| tracks.get(t).is_some_and(|t| t.online))
            .collect()
    }

    /// The playlist a menu is about: a playlist row, or on a playlist's
    /// page its Play and Shuffle buttons.
    fn menu_playlist(&self, m: &AppModel, key: &str) -> Option<&Playlist> {
        if let Some(i) = key.strip_prefix("playlist:") {
            return i
                .parse::<usize>()
                .ok()
                .and_then(|i| self.playlists.lists.get(i));
        }
        (m.screen == Screen::Playlist && matches!(key, "pl_play" | "pl_shuffle"))
            .then(|| self.playlists.get(Path::new(&m.navigation.filter)))
            .flatten()
    }

    /// Track indices in Favorites (for the heart).
    pub fn favorite_tracks(&self, m: &AppModel, tracks: &[Track]) -> Vec<usize> {
        self.playlists
            .get(&favorites_path(&m.settings.music_directory))
            .map(|f| self.facts(tracks, f).playable.clone())
            .unwrap_or_default()
    }

    pub fn is_favorite_track(&self, m: &AppModel, tracks: &[Track], index: usize) -> bool {
        self.playlists
            .get(&favorites_path(&m.settings.music_directory))
            .is_some_and(|f| self.facts(tracks, f).members.contains(&index))
    }

    /// The heart: add the song to Favorites, or take every copy out.
    pub(crate) fn toggle_favorite_track(
        &mut self,
        m: &AppModel,
        tracks: &[Track],
        index: usize,
    ) -> Effect {
        let path = favorites_path(&m.settings.music_directory);
        let favorite = self.playlists.get(&path).cloned();
        let present: Vec<String> = favorite
            .as_ref()
            .map(|f| {
                let resolved = self.resolved(tracks, f);
                f.entries
                    .iter()
                    .zip(resolved)
                    .filter(|(_, r)| *r == Some(index))
                    .map(|(e, _)| e.location.clone())
                    .collect()
            })
            .unwrap_or_default();
        let edit = if present.is_empty() {
            self.flash("Added to Favorites");
            PlaylistEdit::Append(self.entries_for(m, tracks, &[index], &path))
        } else {
            self.flash("Removed from Favorites");
            PlaylistEdit::RemoveLocations(present)
        };
        Effect::Playlist(Box::new(PlaylistRequest::Edit {
            path,
            edit,
            create: true,
        }))
    }

    /// Rows of the Playlists section and of one playlist, built once per
    /// change and lent out: a long playlist is neither rebuilt nor copied
    /// for every frame.
    pub(crate) fn with_playlist_rows<R>(
        &self,
        m: &AppModel,
        tracks: &[Track],
        read: impl FnOnce(&[Item]) -> R,
    ) -> R {
        let key: RowsKey = (
            m.screen,
            m.navigation.filter.clone(),
            self.playlists.generation,
            tracks.as_ptr() as usize,
            tracks.len(),
            self.playlists.moving,
            m.settings.playlist_sort(Path::new(&m.navigation.filter)),
        );
        let fresh = matches!(&*self.playlist_rows_cache.borrow(), Some((k, _)) if *k == key);
        if !fresh {
            let rows = self.build_playlist_rows(m, tracks);
            *self.playlist_rows_cache.borrow_mut() = Some((key, rows));
        }
        let cache = self.playlist_rows_cache.borrow();
        read(cache.as_ref().map_or(&[][..], |(_, rows)| rows.as_slice()))
    }

    pub(crate) fn playlist_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        self.with_playlist_rows(m, tracks, <[Item]>::to_vec)
    }

    fn build_playlist_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        match m.screen {
            Screen::Playlists => {
                let mut rows = vec![Item::new("New Playlist", "pl_new")
                    .with_secondary("Name it, then add songs from anywhere")];
                for (i, p) in self.playlists.lists.iter().enumerate() {
                    let facts = self.facts(tracks, p);
                    let mut item = Item::new(&p.name, format!("playlist:{i}"))
                        .with_secondary(summary(p.entries.len(), facts.ms));
                    item.art = facts.mosaic.first().copied();
                    rows.push(item);
                }
                rows
            }
            Screen::Playlist => {
                let Some(p) = self.playlists.get(Path::new(&m.navigation.filter)) else {
                    return vec![];
                };
                let facts = self.facts(tracks, p);
                let resolved = &facts.resolved;
                let mut rows = vec![];
                if !facts.playable.is_empty() {
                    rows.push(Item::new("Play", "pl_play"));
                    rows.push(Item::new("Shuffle", "pl_shuffle"));
                }
                // While a song is being moved it is drawn at its new place.
                let mut order = self.shown_order(m, tracks, p);
                if let Some((from, to)) = self.playlists.moving {
                    if from < order.len() {
                        let entry = order.remove(from);
                        order.insert(to.min(order.len()), entry);
                    }
                }
                for i in order {
                    let entry = &p.entries[i];
                    let song = resolved[i].and_then(|t| tracks.get(t).map(|track| (t, track)));
                    let mut item = match song {
                        Some((t, track)) => {
                            let mut item = Item::new(track_title(track), format!("pl_entry:{i}"))
                                .with_secondary(names_or_unknown(&track.artist));
                            item.art = Some(t);
                            if !track.online {
                                item.secondary = "Not available · SD card removed".into();
                            }
                            item
                        }
                        None => Item::new(missing_label(entry), format!("pl_entry:{i}"))
                            .with_secondary("Not on the player"),
                    };
                    if self.playlists.moving.is_some_and(|(from, _)| from == i) {
                        item.secondary = "Moving".into();
                    }
                    rows.push(item);
                }
                rows
            }
            _ => vec![],
        }
    }

    pub(crate) fn playlist_select(
        &mut self,
        m: &mut AppModel,
        tracks: &[Track],
        key: &str,
    ) -> Effect {
        if key == "pl_new" {
            self.playlists.pending.clear();
            self.open_name_keyboard(None);
            return Effect::None;
        }
        if let Some(i) = key
            .strip_prefix("playlist:")
            .and_then(|i| i.parse::<usize>().ok())
        {
            if let Some(p) = self.playlists.lists.get(i) {
                let path = p.path.to_string_lossy().into_owned();
                Self::go(m, Screen::Playlist, path);
            }
            return Effect::None;
        }
        let Some(p) = self.playlists.get(Path::new(&m.navigation.filter)).cloned() else {
            return Effect::None;
        };
        let members = self.playable_shown(m, tracks, &p);
        let source = crate::source_label(m);
        let origin = crate::page_origin(m);
        match key {
            "pl_play" | "pl_shuffle" if !members.is_empty() => {
                Self::go(m, Screen::NowPlaying, "");
                Effect::PlayCollection {
                    selected: members[0],
                    members,
                    shuffle: if key == "pl_shuffle" {
                        ShuffleMode::Songs
                    } else {
                        ShuffleMode::Off
                    },
                    source,
                    origin,
                }
            }
            k => {
                let Some(i) = k
                    .strip_prefix("pl_entry:")
                    .and_then(|i| i.parse::<usize>().ok())
                else {
                    return Effect::None;
                };
                match self.resolved(tracks, &p).get(i).copied().flatten() {
                    Some(t) if tracks.get(t).is_some_and(|t| t.online) => {
                        Self::go(m, Screen::NowPlaying, "");
                        Effect::PlayCollection {
                            selected: t,
                            members,
                            shuffle: ShuffleMode::Off,
                            source,
                            origin,
                        }
                    }
                    Some(_) => {
                        self.flash("Insert the SD card to play this song");
                        Effect::None
                    }
                    None => {
                        self.flash("This song isn't on the player");
                        Effect::None
                    }
                }
            }
        }
    }

    /// Hold Select on a playlist, or on a song in one.
    pub(crate) fn playlist_context_rows(&self, m: &AppModel, key: &str) -> Vec<Item> {
        if let Some(p) = self.menu_playlist(m, key) {
            // As an album's: Play and Shuffle first (8.36).
            let mut rows = vec![
                Item::new("Play", "pl_play"),
                Item::new("Shuffle", "pl_shuffle"),
                Item::new("Shuffle Albums", "pl_shuffle_albums"),
                Item::new("Play Next", "pl_next"),
                Item::new("Add to Queue", "pl_queue"),
                sort_row("pl_sort", m.settings.playlist_sort(&p.path)),
            ];
            if !is_favorites(&p.path) && !p.read_only {
                rows.push(Item::new("Rename", "pl_rename"));
            }
            rows.push(Item::new("Duplicate", "pl_duplicate"));
            if !is_favorites(&p.path) {
                rows.push(Item::new("Delete Playlist", "pl_delete"));
            }
            return rows;
        }
        if key.starts_with("pl_entry:") {
            let playlist = self.playlists.get(Path::new(&m.navigation.filter));
            let read_only = playlist.is_none_or(|p| p.read_only);
            let sort = playlist.map_or(PlaylistSort::Playlist, |p| {
                m.settings.playlist_sort(&p.path)
            });
            let mut rows = vec![
                Item::new("Play Next", "ple_next"),
                Item::new("Add to Queue", "ple_queue"),
            ];
            if !read_only {
                // Moving a song only means something in the file's order.
                if sort == PlaylistSort::Playlist {
                    rows.push(Item::new("Move", "ple_move"));
                }
                rows.push(Item::new("Remove from Playlist", "ple_remove"));
            }
            rows.extend([
                Item::new("Add to Playlist…", "ple_add"),
                Item::new("Go to Album", "ple_album"),
                Item::new("Go to Artist", "ple_artist"),
                sort_row("ple_sort", sort),
            ]);
            return rows;
        }
        vec![]
    }

    pub(crate) fn playlist_context_action(
        &mut self,
        m: &mut AppModel,
        tracks: &[Track],
        source: &str,
        menu: &str,
    ) -> Option<Effect> {
        if let Some(p) = self.menu_playlist(m, source).cloned() {
            let members = self.playable_shown(m, tracks, &p);
            return Some(match menu {
                // The menu stays open: each press steps the order on.
                "pl_sort" => Effect::SetPlaylistSort(
                    p.path.clone(),
                    m.settings.playlist_sort(&p.path).next(),
                ),
                "pl_play" | "pl_shuffle" | "pl_shuffle_albums" if !members.is_empty() => {
                    Self::go(m, Screen::NowPlaying, "");
                    Effect::PlayCollection {
                        selected: members[0],
                        members,
                        shuffle: match menu {
                            "pl_shuffle" => ShuffleMode::Songs,
                            "pl_shuffle_albums" => ShuffleMode::Albums,
                            _ => ShuffleMode::Off,
                        },
                        source: p.name.clone(),
                        origin: Some(reborn_core::QueueOrigin {
                            screen: Screen::Playlist,
                            filter: p.path.to_string_lossy().into_owned(),
                        }),
                    }
                }
                "pl_next" if !members.is_empty() => {
                    Self::close_modal(m);
                    Effect::PlayNextCollection(members)
                }
                "pl_queue" if !members.is_empty() => {
                    Self::close_modal(m);
                    Effect::AddToQueueCollection(members)
                }
                "pl_rename" => {
                    Self::close_modal(m);
                    self.open_name_keyboard(Some(&p));
                    Effect::None
                }
                "pl_duplicate" => {
                    Self::close_modal(m);
                    Effect::Playlist(Box::new(PlaylistRequest::Duplicate(p.path)))
                }
                "pl_delete" => {
                    self.playlists.target = Some(p.path);
                    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::DeletePlaylist));
                    m.navigation.modal_focus = 0;
                    Effect::None
                }
                _ => {
                    Self::close_modal(m);
                    Effect::None
                }
            });
        }
        let index = source
            .strip_prefix("pl_entry:")
            .and_then(|i| i.parse::<usize>().ok())?;
        let p = self
            .playlists
            .get(Path::new(&m.navigation.filter))
            .cloned()?;
        let entry = p.entries.get(index)?.clone();
        let track = self.resolved(tracks, &p).get(index).copied().flatten();
        if menu == "ple_sort" {
            let next = m.settings.playlist_sort(&p.path).next();
            return Some(Effect::SetPlaylistSort(p.path, next));
        }
        Self::close_modal(m);
        Some(match (menu, track) {
            ("ple_next", Some(t)) => Effect::PlayNext(t),
            ("ple_queue", Some(t)) => Effect::AddToQueue(t),
            ("ple_add", Some(t)) => {
                self.open_picker(m, vec![t]);
                Effect::None
            }
            ("ple_album", Some(t)) => {
                if let Some(track) = tracks.get(t) {
                    Self::go(m, Screen::Album, crate::album_filter(track));
                }
                Effect::None
            }
            ("ple_artist", Some(t)) => {
                if let Some(track) = tracks.get(t) {
                    Self::go(m, Screen::Artist, crate::artist_filter(track));
                }
                Effect::None
            }
            ("ple_move", _) => {
                self.playlists.moving = Some((index, index));
                Effect::None
            }
            ("ple_remove", _) => Effect::Playlist(Box::new(PlaylistRequest::Edit {
                path: p.path,
                edit: PlaylistEdit::Remove {
                    index,
                    location: entry.location,
                },
                create: false,
            })),
            (_, None) => {
                self.flash("This song isn't on the player");
                Effect::None
            }
            _ => Effect::None,
        })
    }

    /// While a song is being moved, the wheel (or Up/Down) carries it, Select
    /// drops it and Back puts it back.
    pub(crate) fn moving_action(
        &mut self,
        m: &mut AppModel,
        tracks: &[Track],
        action: Action,
    ) -> Option<Effect> {
        let (from, to) = self.playlists.moving?;
        let p = self.playlists.get(Path::new(&m.navigation.filter))?.clone();
        let last = p.entries.len().saturating_sub(1);
        // Play and Shuffle come first when anything can play.
        let lead = if self.facts(tracks, &p).playable.is_empty() {
            0
        } else {
            2
        };
        let step =
            |to: usize, delta: i32| (to as i64 + delta as i64).clamp(0, last as i64) as usize;
        let effect = match action {
            Action::WheelClockwise(_)
            | Action::NavigateDown
            | Action::WheelCounterClockwise(_)
            | Action::NavigateUp => {
                let delta = match action {
                    Action::WheelClockwise(n) => n as i32,
                    Action::WheelCounterClockwise(n) => -(n as i32),
                    Action::NavigateDown => 1,
                    _ => -1,
                };
                self.playlists.moving = Some((from, step(to, delta)));
                Effect::None
            }
            Action::Select => {
                self.playlists.moving = None;
                if to == from {
                    Effect::None
                } else {
                    Effect::Playlist(Box::new(PlaylistRequest::Edit {
                        path: p.path.clone(),
                        edit: PlaylistEdit::Move {
                            from,
                            to,
                            location: p.entries[from].location.clone(),
                        },
                        create: false,
                    }))
                }
            }
            Action::Back | Action::Home => {
                self.playlists.moving = None;
                Effect::None
            }
            _ => return Some(Effect::None),
        };
        // The moving song stays in focus.
        let (_, now) = self.playlists.moving.unwrap_or((from, to));
        m.navigation.focus = lead + now;
        Some(effect)
    }

    /// Open a playlist's page (a playlist just made).
    pub fn show_playlist(&mut self, m: &mut AppModel, path: &Path) {
        Self::go(m, Screen::Playlist, path.to_string_lossy().into_owned());
    }
    /// A playlist on screen was renamed or deleted elsewhere.
    pub fn playlist_moved(&mut self, m: &mut AppModel, old: &Path, new: Option<&Path>) {
        if m.screen != Screen::Playlist || Path::new(&m.navigation.filter) != old {
            return;
        }
        self.playlists.moving = None;
        match new {
            Some(new) => m.navigation.filter = new.to_string_lossy().into_owned(),
            None => Self::back(m),
        }
    }

    /// The playlist picker for these songs.
    pub fn open_picker(&mut self, m: &mut AppModel, songs: Vec<usize>) {
        if songs.is_empty() {
            return;
        }
        self.playlists.pending = songs;
        m.navigation.modal = Some(Modal::PlaylistPicker);
        m.navigation.modal_focus = 0;
    }

    /// New, Favorites, the playlists added to lately, then the rest A to Z.
    pub(crate) fn picker_rows(&self, m: &AppModel) -> Vec<Item> {
        let mut rows = vec![Item::new("New Playlist…", "pick_new")];
        let favorites = favorites_path(&m.settings.music_directory);
        let mut order: Vec<&Playlist> = vec![];
        for path in &self.playlists.recent {
            if let Some(p) = self.playlists.get(path) {
                order.push(p);
            }
        }
        for p in &self.playlists.lists {
            if !order.iter().any(|o| o.path == p.path) {
                order.push(p);
            }
        }
        if !order.iter().any(|p| p.path == favorites) {
            rows.push(Item::new(FAVORITES, "pick_favorites"));
        }
        for p in order.into_iter().filter(|p| !p.read_only) {
            rows.push(
                Item::new(&p.name, format!("pick:{}", p.path.to_string_lossy()))
                    .with_secondary(summary(p.entries.len(), 0)),
            );
        }
        rows
    }

    pub(crate) fn picker_action(
        &mut self,
        m: &mut AppModel,
        tracks: &[Track],
        key: &str,
    ) -> Effect {
        Self::close_modal(m);
        let songs = std::mem::take(&mut self.playlists.pending);
        if key == "pick_new" {
            self.playlists.pending = songs;
            self.open_name_keyboard(None);
            return Effect::None;
        }
        let path = if key == "pick_favorites" {
            favorites_path(&m.settings.music_directory)
        } else {
            match key.strip_prefix("pick:") {
                Some(p) => PathBuf::from(p),
                None => return Effect::None,
            }
        };
        let name = self
            .playlists
            .get(&path)
            .map_or_else(|| FAVORITES.to_owned(), |p| p.name.clone());
        self.playlists.used(&path);
        let entries = self.entries_for(m, tracks, &songs, &path);
        self.flash(match entries.len() {
            1 => format!("Added to {name}"),
            n => format!("{n} songs added to {name}"),
        });
        Effect::Playlist(Box::new(PlaylistRequest::Edit {
            path,
            edit: PlaylistEdit::Append(entries),
            create: true,
        }))
    }

    /// The keyboard for a new playlist's name, or to rename one.
    fn open_name_keyboard(&mut self, rename: Option<&Playlist>) {
        let purpose = match rename {
            Some(p) => {
                self.playlists.target = Some(p.path.clone());
                keyboard::Purpose::PlaylistRename
            }
            None => keyboard::Purpose::PlaylistNew,
        };
        let text = rename.map(|p| p.name.clone()).unwrap_or_default();
        self.keyboard = Some(keyboard::Keyboard::new(purpose, text));
    }

    /// The name keyboard is done.
    pub(crate) fn playlist_named(&mut self, purpose: &keyboard::Purpose, name: String) -> Effect {
        match purpose {
            keyboard::Purpose::PlaylistNew => {
                let tracks = std::mem::take(&mut self.playlists.pending);
                Effect::Playlist(Box::new(PlaylistRequest::Create { name, tracks }))
            }
            _ => match self.playlists.target.take() {
                Some(path) => Effect::Playlist(Box::new(PlaylistRequest::Rename { path, name })),
                None => Effect::None,
            },
        }
    }
}

/// A song the player does not have: its file name from the entry.
fn missing_label(entry: &PlaylistEntry) -> String {
    let name = entry
        .location
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&entry.location);
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    if stem.is_empty() {
        entry.location.clone()
    } else {
        stem.to_owned()
    }
}
