//! Up Next (Beta 8.33): the history above the song playing (the list opens
//! on the song playing, the history is a scroll up), then your queue, then
//! the rest of what plays, each part under a heading. Rows are made one at a
//! time from the queue (it holds up to 5 000 songs).
use crate::{display_or_unknown, track_title, Effect, Item, Selection, Ui};
use reborn_core::{Action, AppModel, QueueEntryId, Screen, ShuffleMode, Track};
use std::{collections::HashMap, ops::Range};

/// A row of Up Next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    HistoryHeading,
    /// A library index.
    History(usize),
    Now,
    QueueHeading,
    /// A queue index.
    Queued(usize),
    SourceHeading,
    Source(usize),
}

impl Row {
    pub(crate) fn heading(self) -> bool {
        matches!(
            self,
            Self::HistoryHeading | Self::QueueHeading | Self::SourceHeading
        )
    }
}

/// Where each part of Up Next is.
pub(crate) struct Layout {
    pub history: Vec<usize>,
    pub queued: Range<usize>,
    pub source: Range<usize>,
    empty: bool,
}

impl Layout {
    fn new(m: &AppModel, history: Vec<usize>) -> Self {
        let source_start = m.source_start();
        Self {
            history,
            queued: (m.queue_position + 1).min(source_start)..source_start,
            source: source_start..m.queue.len(),
            empty: m.queue.is_empty(),
        }
    }
    fn history_rows(&self) -> usize {
        if self.history.is_empty() {
            0
        } else {
            self.history.len() + 1
        }
    }
    fn part_rows(range: &Range<usize>) -> usize {
        if range.is_empty() {
            0
        } else {
            range.len() + 1
        }
    }
    pub(crate) fn len(&self) -> usize {
        if self.empty {
            return 0;
        }
        self.history_rows() + 1 + Self::part_rows(&self.queued) + Self::part_rows(&self.source)
    }
    /// The song playing's row: where Up Next opens.
    pub(crate) fn now_row(&self) -> usize {
        self.history_rows()
    }
    pub(crate) fn row(&self, i: usize) -> Option<Row> {
        if i >= self.len() {
            return None;
        }
        let history = self.history_rows();
        if i < history {
            return Some(if i == 0 {
                Row::HistoryHeading
            } else {
                Row::History(self.history[i - 1])
            });
        }
        let mut at = i - history;
        if at == 0 {
            return Some(Row::Now);
        }
        at -= 1;
        let queued = Self::part_rows(&self.queued);
        if at < queued {
            return Some(if at == 0 {
                Row::QueueHeading
            } else {
                Row::Queued(self.queued.start + at - 1)
            });
        }
        at -= queued;
        Some(if at == 0 {
            Row::SourceHeading
        } else {
            Row::Source(self.source.start + at - 1)
        })
    }
    /// The row showing queue index `index`.
    pub(crate) fn row_of(&self, index: usize) -> Option<usize> {
        let base = self.history_rows();
        if self.queued.contains(&index) {
            Some(base + 2 + index - self.queued.start)
        } else if self.source.contains(&index) {
            Some(base + 1 + Self::part_rows(&self.queued) + 1 + index - self.source.start)
        } else if !self.empty && index < self.queued.start {
            // The song playing (a song before it is not shown).
            Some(base)
        } else {
            None
        }
    }
}

/// Library stamp, the history as read, its rows (library indices).
type HistoryRows = (
    (usize, usize),
    Vec<reborn_core::queue::HistoryEntry>,
    Vec<usize>,
);
/// Library stamp and each song's library index by (id, source).
type LibraryIds = ((usize, usize), HashMap<(i64, String), usize>);

/// History rows and library indices by id, kept until they change.
#[derive(Default)]
pub(crate) struct Cache {
    history: Option<HistoryRows>,
    ids: Option<LibraryIds>,
}

impl Ui {
    pub(crate) fn up_next(&self, m: &AppModel, tracks: &[Track]) -> Layout {
        let stamp = crate::library_stamp(tracks);
        let mut cache = self.up_next_cache.borrow_mut();
        let fresh = cache
            .history
            .as_ref()
            .is_some_and(|(s, h, _)| *s == stamp && *h == m.history);
        if !fresh {
            let index: HashMap<(&str, &std::path::Path), usize> = tracks
                .iter()
                .enumerate()
                .map(|(i, t)| ((t.source_id.as_str(), t.path.as_path()), i))
                .collect();
            let rows = m
                .history
                .iter()
                .filter_map(|e| {
                    index
                        .get(&(e.source_id.as_str(), e.path.as_path()))
                        .copied()
                })
                .filter(|&i| tracks[i].online)
                .collect();
            cache.history = Some((stamp, m.history.clone(), rows));
        }
        let history = cache
            .history
            .as_ref()
            .map(|(_, _, r)| r.clone())
            .unwrap_or_default();
        Layout::new(m, history)
    }

    /// The library index of a queued song.
    pub(crate) fn library_index(&self, tracks: &[Track], track: &Track) -> Option<usize> {
        let stamp = crate::library_stamp(tracks);
        let mut cache = self.up_next_cache.borrow_mut();
        if cache.ids.as_ref().is_none_or(|(s, _)| *s != stamp) {
            let ids = tracks
                .iter()
                .enumerate()
                .map(|(i, t)| ((t.id, t.source_id.clone()), i))
                .collect();
            cache.ids = Some((stamp, ids));
        }
        cache
            .ids
            .as_ref()
            .and_then(|(_, ids)| ids.get(&(track.id, track.source_id.clone())).copied())
    }

    /// Up Next's row `i` as a list item.
    pub(crate) fn up_next_item(
        &self,
        m: &AppModel,
        tracks: &[Track],
        layout: &Layout,
        i: usize,
    ) -> Option<Item> {
        let heading = |label: String, key: &str, count: usize| {
            let mut item = Item::new(label, key).with_secondary(crate::screens::count_label(count));
            item.enabled = false;
            item
        };
        let song = |index: usize| {
            let t = &m.queue[index];
            let mut item = Item::new(track_title(t), Self::entry_key(m, index))
                .with_secondary(crate::names_or_unknown(&t.artist));
            item.art = self.library_index(tracks, t);
            item
        };
        Some(match layout.row(i)? {
            Row::HistoryHeading => {
                heading("History".into(), "heading:history", layout.history.len())
            }
            Row::History(lib) => {
                let t = &tracks[lib];
                let mut item = Item::new(track_title(t), format!("hist:{lib}"))
                    .with_secondary(crate::names_or_unknown(&t.artist));
                item.art = Some(lib);
                item
            }
            Row::Now => song(m.queue_position),
            Row::QueueHeading => heading("Queue".into(), "heading:queue", layout.queued.len()),
            Row::Queued(index) | Row::Source(index) => song(index),
            Row::SourceHeading => heading(
                if m.queue_source.is_empty() {
                    "Next".into()
                } else {
                    format!("Next from {}", m.queue_source)
                },
                "heading:source",
                layout.source.len(),
            ),
        })
    }

    /// The Up Next key of queue index `index`.
    pub(crate) fn entry_key(m: &AppModel, index: usize) -> String {
        let id = m.queue_entry_ids.get(index).map_or(index as u64, |v| v.0);
        format!("queue:{id}")
    }

    /// The queue entry an Up Next key names.
    pub(crate) fn entry_of(m: &AppModel, key: &str) -> Option<QueueEntryId> {
        let id = key.strip_prefix("queue:")?.parse::<u64>().ok()?;
        m.queue_entry_ids
            .iter()
            .copied()
            .find(|entry| entry.0 == id)
    }
}

impl Ui {
    /// Up Next opens on the song playing; the history is a scroll up.
    pub(crate) fn open_up_next(&self, m: &mut AppModel, tracks: &[Track]) {
        let now = self.up_next(m, tracks).now_row();
        m.navigation.focus = now;
        m.navigation.scroll = now;
        self.up_next_focus.replace(None);
    }

    /// The focus stays on its song when the rows move under it (#32): Play
    /// Next, a song ending or a removal adds and moves rows and headings, and
    /// the old index could land on a heading, which sent the focus to the top.
    /// A focus the wheel moved is left alone.
    pub(crate) fn keep_up_next_focus(&self, m: &mut AppModel, tracks: &[Track]) {
        let Some((row, key)) = self.up_next_focus.borrow().clone() else {
            return;
        };
        if row != m.navigation.focus {
            return;
        }
        let layout = self.up_next(m, tracks);
        let found = if let Some(id) = Self::entry_of(m, &key) {
            m.entry_index(id).and_then(|index| layout.row_of(index))
        } else if let Some(lib) = key
            .strip_prefix("hist:")
            .and_then(|v| v.parse::<usize>().ok())
        {
            layout
                .history
                .iter()
                .rposition(|&i| i == lib)
                .map(|at| at + 1)
        } else {
            None
        };
        if let Some(found) = found.filter(|&found| found != row) {
            m.navigation.focus = found;
            let heights = self.search_heights(m, tracks);
            m.navigation.scroll = crate::views_search::window(&heights, found, m.navigation.scroll);
        }
    }

    /// The menu of an Up Next row (hold Select).
    pub(crate) fn up_next_menu(&self, m: &AppModel, key: Option<&str>) -> Vec<Item> {
        let Some(key) = key else {
            return vec![];
        };
        if key.starts_with("hist:") {
            return vec![
                Item::new("Play Next", "play_next"),
                Item::new("Add to Queue", "add_queue"),
                Item::new("Add to Playlist…", "add_playlist"),
                Item::new("Go to Album", "go_album"),
                Item::new("Go to Artist", "go_artist"),
            ];
        }
        let Some(index) = Self::entry_of(m, key).and_then(|id| m.entry_index(id)) else {
            return vec![];
        };
        // The song playing holds what acts on the whole list.
        if index == m.queue_position {
            let mut rows = vec![
                Item::new("Add to Playlist…", "add_playlist"),
                Item::new("Go to Album", "go_album"),
                Item::new("Go to Artist", "go_artist"),
                Item::new("Save Up Next as Playlist…", "save_queue"),
            ];
            if m.queued_count() > 0 {
                rows.push(Item::new("Clear Queue", "clear_queued"));
            }
            if m.queue_position + 1 < m.queue.len() {
                rows.push(Item::new("Clear Up Next", "clear_queue"));
            }
            return rows;
        }
        // A song to come: the order every song menu has (8.36). Play Next or
        // Add to Queue is left out where it would not move the song.
        let queued = m.is_queued(m.queue_entry_ids[index]);
        let mut rows = vec![];
        if !(queued && index == m.queue_position + 1) {
            rows.push(Item::new("Play Next", "queue_next"));
        }
        if !(queued && index + 1 == m.source_start()) {
            rows.push(Item::new("Add to Queue", "queue_last"));
        }
        rows.extend([
            Item::new("Move", "queue_move"),
            Item::new("Remove from Queue", "queue_remove"),
            Item::new("Add to Playlist…", "add_playlist"),
            Item::new("Go to Album", "go_album"),
            Item::new("Go to Artist", "go_artist"),
            Item::new("Select Songs…", "select_songs"),
        ]);
        rows
    }

    /// Select Songs on the screen showing.
    pub(crate) fn selecting(&self, m: &AppModel) -> Option<&Selection> {
        self.selection
            .as_ref()
            .filter(|s| s.screen == m.screen && s.filter == m.navigation.filter)
    }

    /// A list's title, or how many songs are chosen.
    pub(crate) fn list_title(&self, m: &AppModel) -> String {
        if let Some(selection) = self.selecting(m) {
            return format!("{} Selected", selection.keys.len());
        }
        if m.screen == Screen::Queue && self.moving().is_some() {
            return "Moving".into();
        }
        crate::screens::screen_title(m)
    }

    /// Lists that offer Select Songs.
    pub(crate) fn selects(screen: Screen) -> bool {
        matches!(
            screen,
            Screen::Tracks | Screen::Album | Screen::Search | Screen::Queue
        )
    }

    fn selectable(m: &AppModel, key: &str) -> bool {
        key.starts_with("track:")
            || Self::entry_of(m, key).is_some_and(|id| Some(id) != m.current_entry_id())
    }

    /// While choosing: whether this row is chosen (`None`: no mark).
    pub(crate) fn selection_mark(&self, m: &AppModel, key: &str) -> Option<bool> {
        let selection = self.selecting(m)?;
        Self::selectable(m, key).then(|| selection.keys.iter().any(|k| k == key))
    }

    pub(crate) fn start_selection(&mut self, m: &AppModel, key: &str) {
        self.selection = Some(Selection {
            screen: m.screen,
            filter: m.navigation.filter.clone(),
            keys: if Self::selectable(m, key) {
                vec![key.to_owned()]
            } else {
                vec![]
            },
        });
    }

    /// What can be done with the chosen songs (hold Select while choosing).
    pub(crate) fn selection_menu(&self, m: &AppModel) -> Vec<Item> {
        let chosen = self.selecting(m).map_or(0, |s| s.keys.len());
        let mut rows = if m.screen == Screen::Queue {
            vec![
                Item::new("Play Next", "sel_next"),
                Item::new("Add to Queue", "sel_queue"),
                Item::new("Remove from Queue", "sel_remove"),
                Item::new("Add to Playlist…", "sel_playlist"),
            ]
        } else {
            vec![
                Item::new("Play Next", "sel_next"),
                Item::new("Add to Queue", "sel_queue"),
                Item::new("Add to Playlist…", "sel_playlist"),
            ]
        };
        for row in &mut rows {
            row.enabled = chosen > 0;
        }
        rows.push(Item::new("Done", "sel_done"));
        rows
    }

    /// The chosen songs as library indices, in the list's order.
    fn chosen_songs(&self, m: &AppModel, tracks: &[Track]) -> Vec<usize> {
        let Some(selection) = self.selecting(m) else {
            return vec![];
        };
        if m.screen == Screen::Queue {
            return self
                .chosen_entries(m)
                .into_iter()
                .filter_map(|id| m.entry_index(id))
                .filter_map(|i| self.library_index(tracks, &m.queue[i]))
                .collect();
        }
        let chosen: std::collections::HashSet<usize> = selection
            .keys
            .iter()
            .filter_map(|k| k.strip_prefix("track:")?.parse().ok())
            .collect();
        self.filtered_indices(m, tracks)
            .into_iter()
            .filter(|i| chosen.contains(i))
            .collect()
    }

    /// The chosen Up Next entries, in Up Next's order.
    fn chosen_entries(&self, m: &AppModel) -> Vec<QueueEntryId> {
        let mut ids: Vec<QueueEntryId> = self
            .selecting(m)
            .map(|s| s.keys.iter().filter_map(|k| Self::entry_of(m, k)).collect())
            .unwrap_or_default();
        ids.sort_by_key(|id| m.entry_index(*id));
        ids
    }

    /// A Select Songs action; the choosing ends.
    pub(crate) fn selection_effect(
        &mut self,
        m: &mut AppModel,
        tracks: &[Track],
        menu_key: &str,
    ) -> Effect {
        Self::close_modal(m);
        let effect = match menu_key {
            "sel_next" if m.screen == Screen::Queue => {
                Effect::QueueEntriesNext(self.chosen_entries(m))
            }
            "sel_queue" if m.screen == Screen::Queue => {
                Effect::QueueEntriesLast(self.chosen_entries(m))
            }
            "sel_remove" => Effect::QueueRemoveEntries(self.chosen_entries(m)),
            "sel_next" => Effect::PlayNextCollection(self.chosen_songs(m, tracks)),
            "sel_queue" => Effect::AddToQueueCollection(self.chosen_songs(m, tracks)),
            "sel_playlist" => {
                let songs = self.chosen_songs(m, tracks);
                self.selection = None;
                self.open_picker(m, songs);
                return Effect::None;
            }
            _ => Effect::None,
        };
        self.selection = None;
        effect
    }

    /// Buttons while an Up Next song moves with the wheel, or while songs
    /// are chosen; `None` leaves the action to the list.
    pub(crate) fn list_mode_input(
        &mut self,
        m: &mut AppModel,
        tracks: &[Track],
        action: Action,
    ) -> Option<Effect> {
        if self.selection.is_some() && self.selecting(m).is_none() {
            self.selection = None;
        }
        if m.screen != Screen::Queue {
            self.queue_moving.set(None);
        }
        if m.navigation.modal.is_some() {
            return None;
        }
        if let Some(id) = self.queue_moving.get() {
            let Some(index) = m.entry_index(id).filter(|&i| i > m.queue_position) else {
                self.queue_moving.set(None);
                return None;
            };
            return match action {
                Action::WheelClockwise(_) | Action::NavigateDown => {
                    Some(Effect::QueueMove { index, delta: 1 })
                }
                Action::WheelCounterClockwise(_) | Action::NavigateUp => {
                    Some(Effect::QueueMove { index, delta: -1 })
                }
                Action::Select | Action::Back | Action::Home | Action::ContextMenu => {
                    self.queue_moving.set(None);
                    Some(Effect::None)
                }
                _ => None,
            };
        }
        self.selecting(m)?;
        match action {
            Action::Select => {
                let key = self.row_at(m, tracks, m.navigation.focus)?.key;
                if Self::selectable(m, &key) {
                    let keys = &mut self.selection.as_mut()?.keys;
                    match keys.iter().position(|k| *k == key) {
                        Some(at) => {
                            keys.remove(at);
                        }
                        None => keys.push(key),
                    }
                }
                Some(Effect::None)
            }
            Action::Back | Action::Home => {
                self.selection = None;
                Some(Effect::None)
            }
            Action::ContextMenu => {
                m.navigation.context_target = Some(m.navigation.focus);
                m.navigation.context_key = Some("selection".into());
                Self::open_modal(m, reborn_core::Modal::ContextMenu);
                Some(Effect::None)
            }
            _ => None,
        }
    }

    /// The moved song stays focused as it moves.
    pub(crate) fn follow_moving(&self, m: &mut AppModel, tracks: &[Track]) {
        let Some(id) = self.queue_moving.get() else {
            return;
        };
        let Some(index) = m.entry_index(id).filter(|&i| i > m.queue_position) else {
            self.queue_moving.set(None);
            return;
        };
        if let Some(row) = self.up_next(m, tracks).row_of(index) {
            m.navigation.focus = row;
            let heights = self.search_heights(m, tracks);
            m.navigation.scroll = crate::views_search::window(&heights, row, m.navigation.scroll);
        }
    }

    pub(crate) fn moving(&self) -> Option<QueueEntryId> {
        self.queue_moving.get()
    }

    pub(crate) fn start_moving(&self, id: QueueEntryId) {
        self.queue_moving.set(Some(id));
    }

    /// A whole album, artist, genre, folder or the search's songs: its rows
    /// get Play, Shuffle, Play Next, Add to Queue and Add to Playlist.
    pub(crate) fn group_key(key: &str) -> bool {
        ["album:", "artist:", "albumartist:", "genre:", "folder:"]
            .iter()
            .any(|p| key.starts_with(p))
            || key == crate::catalog::SEARCH_PLAY_ALL
    }

    pub(crate) fn group_menu(key: &str) -> Vec<Item> {
        let mut rows = vec![
            Item::new("Play", "group_play"),
            Item::new("Shuffle", "group_shuffle"),
            Item::new("Play Next", "group_next"),
            Item::new("Add to Queue", "group_queue"),
            Item::new("Add to Playlist…", "group_playlist"),
        ];
        // Many albums: each whole, in random order (one album has nothing
        // to mix).
        if !key.starts_with("album:") {
            rows.insert(2, Item::new("Shuffle Albums", "group_shuffle_albums"));
        }
        rows
    }

    /// A group's songs in its own order (an album by disc and track).
    pub(crate) fn group_members(&self, m: &AppModel, tracks: &[Track], key: &str) -> Vec<usize> {
        if key == crate::catalog::SEARCH_PLAY_ALL {
            return self.filtered_indices(m, tracks);
        }
        if let Some(folder) = key.strip_prefix("folder:") {
            // As Folders shows them: in each folder its folders first, then
            // its songs, both by name (Beta 8.39).
            let mut c = self.catalog.borrow_mut();
            c.folders.ensure(tracks);
            return c.folders.play_order(tracks, std::path::Path::new(folder));
        }
        let mut members: Vec<usize> = tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| crate::track_matches(t, key))
            .map(|(i, _)| i)
            .collect();
        {
            members.sort_by_cached_key(|&i| {
                let t = &tracks[i];
                (
                    reborn_core::sort::fold(&t.album_artist),
                    reborn_core::sort::fold(&t.album),
                    t.disc,
                    t.track,
                    t.path.clone(),
                )
            });
        }
        members
    }

    /// What a group is called (the menu's title, Up Next's source).
    pub(crate) fn group_label(m: &AppModel, key: &str) -> String {
        if key == crate::catalog::SEARCH_PLAY_ALL {
            return format!("Search “{}”", m.navigation.filter.trim());
        }
        if let Some(plain) = key.strip_prefix("album:") {
            let album = crate::album_parts(key).map_or(plain, |(_, a, _)| a);
            return display_or_unknown(album).to_owned();
        }
        if let Some(folder) = key.strip_prefix("folder:") {
            return crate::folder_name(m, folder);
        }
        let name = key.split_once(':').map_or(key, |(_, n)| n);
        if key.starts_with("genre:") && name.is_empty() {
            return "No Genre".into();
        }
        display_or_unknown(name).to_owned()
    }

    pub(crate) fn group_effect(
        &mut self,
        m: &mut AppModel,
        tracks: &[Track],
        key: &str,
        menu_key: &str,
    ) -> Effect {
        let members = self.group_members(m, tracks, key);
        let Some(&first) = members.first() else {
            return Effect::None;
        };
        let source = Self::group_label(m, key);
        Self::close_modal(m);
        match menu_key {
            "group_play" | "group_shuffle" | "group_shuffle_albums" => {
                Self::go(m, Screen::NowPlaying, "");
                Effect::PlayCollection {
                    selected: first,
                    members,
                    shuffle: match menu_key {
                        "group_shuffle" => ShuffleMode::Songs,
                        "group_shuffle_albums" => ShuffleMode::Albums,
                        _ => ShuffleMode::Off,
                    },
                    source,
                }
            }
            "group_next" => Effect::PlayNextCollection(members),
            "group_queue" => Effect::AddToQueueCollection(members),
            "group_playlist" => {
                self.open_picker(m, members);
                Effect::None
            }
            _ => Effect::None,
        }
    }

    /// The song or group a menu is about: its title and line under it.
    pub(crate) fn menu_subject(
        &self,
        m: &AppModel,
        tracks: &[Track],
        key: &str,
    ) -> Option<(String, String)> {
        if key == "selection" {
            let n = self.selecting(m).map_or(0, |s| s.keys.len());
            return Some((crate::components::count(n, "Song", "Songs"), String::new()));
        }
        if Self::group_key(key) {
            // Counted once per menu, not on every frame it is drawn.
            let library = crate::library_stamp(tracks);
            let cached = self
                .group_count
                .borrow()
                .as_ref()
                .filter(|(l, k, _)| *l == library && k == key)
                .map(|(_, _, n)| *n);
            let n = match cached {
                Some(n) => n,
                None => {
                    let n = if let Some(folder) = key.strip_prefix("folder:") {
                        let mut c = self.catalog.borrow_mut();
                        c.folders.ensure(tracks);
                        c.folders.total(std::path::Path::new(folder))
                    } else {
                        self.group_members(m, tracks, key).len()
                    };
                    *self.group_count.borrow_mut() = Some((library, key.to_owned(), n));
                    n
                }
            };
            return Some((
                Self::group_label(m, key),
                crate::components::count(n, "song", "songs"),
            ));
        }
        let song = if let Some(lib) = key.strip_prefix("hist:") {
            tracks.get(lib.parse::<usize>().ok()?)?
        } else {
            let index = m.entry_index(Self::entry_of(m, key)?)?;
            &m.queue[index]
        };
        Some((
            track_title(song).to_owned(),
            crate::names_or_unknown(&song.artist).into_owned(),
        ))
    }

    /// The library song a menu row names (a song, a queue entry, history).
    pub(crate) fn menu_song(&self, m: &AppModel, tracks: &[Track], key: &str) -> Option<usize> {
        if let Some(i) = key
            .strip_prefix("track:")
            .or_else(|| key.strip_prefix("hist:"))
        {
            return i.parse().ok();
        }
        let index = m.entry_index(Self::entry_of(m, key)?)?;
        self.library_index(tracks, &m.queue[index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(n: i64, queued: usize, position: usize) -> AppModel {
        let mut m = AppModel::default();
        let songs = (1..=n)
            .map(|id| Track {
                id,
                size: 1,
                ..Default::default()
            })
            .collect();
        m.replace_queue(songs, position).unwrap();
        for _ in 0..queued {
            m.queue_last(vec![Track {
                id: 100,
                size: 1,
                ..Default::default()
            }])
            .unwrap();
        }
        m
    }

    #[test]
    fn rows_run_history_now_queue_then_the_source() {
        let m = model(5, 2, 1);
        let layout = Layout::new(&m, vec![7, 8]);
        // History(2) + heading, now, queue heading + 2, source heading + 3.
        assert_eq!(layout.len(), 3 + 1 + 3 + 4);
        let rows: Vec<Row> = (0..layout.len()).filter_map(|i| layout.row(i)).collect();
        assert_eq!(
            rows,
            [
                Row::HistoryHeading,
                Row::History(7),
                Row::History(8),
                Row::Now,
                Row::QueueHeading,
                Row::Queued(2),
                Row::Queued(3),
                Row::SourceHeading,
                Row::Source(4),
                Row::Source(5),
                Row::Source(6),
            ]
        );
        assert_eq!(layout.now_row(), 3);
        for (row, index) in [(5, 2), (6, 3), (8, 4), (10, 6), (3, 1)] {
            assert_eq!(layout.row_of(index), Some(row), "queue index {index}");
        }
        // Nothing queued, no history: the song playing, then the source.
        let m = model(3, 0, 0);
        let layout = Layout::new(&m, vec![]);
        assert_eq!(layout.len(), 1 + 3);
        assert_eq!(layout.row(1), Some(Row::SourceHeading));
        assert_eq!(Layout::new(&AppModel::default(), vec![1]).len(), 0);
    }
}
