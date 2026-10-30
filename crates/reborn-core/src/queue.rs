//! The play queue (Beta 8.33). One list in play order: what played before,
//! the song playing, the songs you queued (Play Next, Add to Queue), then the
//! rest of what you are playing (its source: an album, a playlist, a search,
//! All Songs). Your queued songs stay one block right after the song
//! playing, whatever moves the position, and leave the list for good once a
//! Repeat All starts over. The source keeps its own order next to the play
//! order, so Shuffle can be turned off again. What played is also kept apart
//! as a short history (`HistoryEntry`).
use crate::{AppModel, QueueEntryId, RepeatMode, Track, MAX_QUEUE};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Songs kept in the history (Up Next, above the song playing).
pub const MAX_HISTORY: usize = 50;

/// A song that played, as the library knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub source_id: String,
    pub path: PathBuf,
}

/// Fisher-Yates over `items[from..]`, driven by splitmix64.
pub fn shuffle_from<T>(items: &mut [T], from: usize, mut seed: u64) {
    for i in (from + 1..items.len()).rev() {
        seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        let span = (i - from + 1) as u128;
        let j = from + ((u128::from(z) * span) >> 64) as usize;
        items.swap(i, j);
    }
}

/// Shuffle Albums (Beta 8.39): the songs as whole albums in random order,
/// each album in its own order (disc, track, file name). An album is a
/// folder (a `CD 2` or `Disc 1` folder folds into the one above it) and an
/// album tag: folders keep apart albums of the same name, the tag the
/// albums among a folder's loose songs. With `lead`, that song's album
/// comes first, from that song on; its songs before it play last. Returns
/// positions into `tracks`.
pub fn album_shuffle<'a>(tracks: &[&'a Track], lead: Option<usize>, seed: u64) -> Vec<usize> {
    // Each folder is looked at once, not once per song.
    let mut folders: HashMap<&'a Path, usize> = HashMap::new();
    let mut albums_folders: HashMap<&'a Path, usize> = HashMap::new();
    let mut index: HashMap<(usize, &'a str), usize> = HashMap::new();
    let mut groups: Vec<Vec<usize>> = vec![];
    for (i, &t) in tracks.iter().enumerate() {
        let parent = t.path.parent().unwrap_or(Path::new(""));
        let folder = match folders.get(parent) {
            Some(&folder) => folder,
            None => {
                let mut dir = parent;
                while dir.components().count() > 1
                    && dir
                        .file_name()
                        .is_some_and(|n| crate::books::subdivision(&n.to_string_lossy()))
                {
                    dir = dir.parent().unwrap_or(dir);
                }
                let next = albums_folders.len();
                let folder = *albums_folders.entry(dir).or_insert(next);
                folders.insert(parent, folder);
                folder
            }
        };
        let next = groups.len();
        let group = *index.entry((folder, t.album.as_str())).or_insert(next);
        if group == next {
            groups.push(vec![]);
        }
        groups[group].push(i);
    }
    // Disc and track first; file names only where those are equal.
    for group in &mut groups {
        group.sort_by(|&a, &b| {
            let (x, y) = (tracks[a], tracks[b]);
            (x.disc, x.track)
                .cmp(&(y.disc, y.track))
                .then_with(|| {
                    crate::sort::natural_key(&x.filename)
                        .cmp(&crate::sort::natural_key(&y.filename))
                })
                .then(a.cmp(&b))
        });
    }
    let first = lead.and_then(|l| groups.iter().position(|g| g.contains(&l)));
    let mut last = vec![];
    if let Some(at) = first {
        let mut group = groups.remove(at);
        let from = group.iter().position(|&i| Some(i) == lead).unwrap_or(0);
        last = group.drain(..from).collect();
        groups.insert(0, group);
    }
    shuffle_from(&mut groups, usize::from(first.is_some()), seed);
    groups.into_iter().flatten().chain(last).collect()
}

impl AppModel {
    /// Whether you queued this entry (Play Next, Add to Queue).
    pub fn is_queued(&self, id: QueueEntryId) -> bool {
        self.queue_queued.contains(&id)
    }

    /// Your queued songs waiting right after the song playing.
    pub fn queued_count(&self) -> usize {
        self.queue_entry_ids
            .iter()
            .skip(self.queue_position + 1)
            .take_while(|id| self.is_queued(**id))
            .count()
    }

    /// Where the source's songs to come begin (after your queued ones).
    pub fn source_start(&self) -> usize {
        (self.queue_position + 1 + self.queued_count()).min(self.queue.len())
    }

    pub fn entry_index(&self, id: QueueEntryId) -> Option<usize> {
        self.queue_entry_ids.iter().position(|entry| *entry == id)
    }

    /// Reorder the list: `order` names every index once, in its new place.
    fn permute(&mut self, order: &[usize]) {
        debug_assert_eq!(order.len(), self.queue.len());
        if order.iter().enumerate().all(|(at, i)| at == *i) {
            return;
        }
        let current = self.current_entry_id();
        let mut tracks: Vec<Option<Track>> = std::mem::take(&mut self.queue)
            .into_iter()
            .map(Some)
            .collect();
        self.queue = order.iter().filter_map(|&i| tracks[i].take()).collect();
        self.queue_entry_ids = order.iter().map(|&i| self.queue_entry_ids[i]).collect();
        if let Some(position) = current.and_then(|id| self.entry_index(id)) {
            self.queue_position = position;
        }
    }

    /// Your queued songs still to come move right after the song playing
    /// (after Previous, a jump back, or one moved below the source's songs).
    pub fn anchor_queued(&mut self) {
        let start = self.queue_position + 1;
        if start >= self.queue.len() {
            return;
        }
        let queued = |m: &Self, i: usize| m.is_queued(m.queue_entry_ids[i]);
        let mut order: Vec<usize> = (0..start).collect();
        order.extend((start..self.queue.len()).filter(|&i| queued(self, i)));
        order.extend((start..self.queue.len()).filter(|&i| !queued(self, i)));
        self.permute(&order);
    }

    /// Insert `tracks` at `at` as your queued songs. An empty queue becomes
    /// them, the first one current: `Ok(true)` asks the caller to start it.
    fn insert_queued(&mut self, at: usize, tracks: Vec<Track>) -> Result<bool, String> {
        if tracks.is_empty() {
            return Err("nothing to queue".into());
        }
        if self.queue.is_empty() {
            let count = tracks.len();
            self.replace_queue(tracks, 0)?;
            self.queue_source = String::new();
            // They are the queue now: no source to come back to.
            self.queue_order.clear();
            debug_assert_eq!(self.queue.len(), count);
            return Ok(true);
        }
        if self.queue.len().saturating_add(tracks.len()) > MAX_QUEUE {
            return Err("queue is full".into());
        }
        for (offset, track) in tracks.into_iter().enumerate() {
            let id = self.insert_queue_entry(at + offset, track)?;
            self.queue_queued.push(id);
        }
        Ok(false)
    }

    /// Play Next: `tracks` go to the top of your queue, in their order (the
    /// newest Play Next plays first).
    pub fn queue_next(&mut self, tracks: Vec<Track>) -> Result<bool, String> {
        let at = (self.queue_position + 1).min(self.queue.len());
        self.insert_queued(at, tracks)
    }

    /// Add to Queue: `tracks` go to the end of your queue, before the rest of
    /// what is playing.
    pub fn queue_last(&mut self, tracks: Vec<Track>) -> Result<bool, String> {
        let at = self.source_start();
        self.insert_queued(at, tracks)
    }

    /// Remove entries (never the song playing); how many went.
    pub fn remove_entries(&mut self, ids: &[QueueEntryId]) -> usize {
        let mut indices: Vec<usize> = ids
            .iter()
            .filter_map(|id| self.entry_index(*id))
            .filter(|&i| i != self.queue_position)
            .collect();
        indices.sort_unstable();
        indices.dedup();
        for &i in indices.iter().rev() {
            self.remove_queue_entry(i);
        }
        indices.len()
    }

    /// Move an entry one place in Up Next. The first step out of your queue
    /// (down) or into it (up) only moves it to the other part.
    pub fn move_entry(&mut self, id: QueueEntryId, up: bool) -> bool {
        let Some(i) = self.entry_index(id) else {
            return false;
        };
        if i <= self.queue_position {
            return false;
        }
        let source = self.source_start();
        let queued = self.is_queued(id);
        match (up, queued) {
            (true, true) if i == self.queue_position + 1 => false,
            (true, false) if i == source => {
                self.queue_queued.push(id);
                true
            }
            (false, true) if i + 1 == source => {
                self.queue_queued.retain(|q| *q != id);
                true
            }
            (false, _) if i + 1 >= self.queue.len() => false,
            (true, _) => self.move_queue_entry(i, i - 1),
            (false, _) => self.move_queue_entry(i, i + 1),
        }
    }

    /// The chosen entries still to come, in their Up Next order.
    fn chosen_entries(&self, ids: &[QueueEntryId]) -> Vec<usize> {
        let mut indices: Vec<usize> = ids
            .iter()
            .filter_map(|id| self.entry_index(*id))
            .filter(|&i| i > self.queue_position)
            .collect();
        indices.sort_unstable();
        indices.dedup();
        indices
    }

    /// The chosen entries become your queued songs, in `order`.
    fn queue_entries(&mut self, indices: &[usize], order: &[usize]) -> usize {
        for &i in indices {
            let id = self.queue_entry_ids[i];
            if !self.is_queued(id) {
                self.queue_queued.push(id);
            }
        }
        self.permute(order);
        indices.len()
    }

    /// Play Next in Up Next: the chosen entries (none of them playing) go to
    /// the top of your queue, together, in their Up Next order; how many moved.
    pub fn entries_next(&mut self, ids: &[QueueEntryId]) -> usize {
        let indices = self.chosen_entries(ids);
        let start = self.queue_position + 1;
        let mut order: Vec<usize> = (0..start).collect();
        order.extend(&indices);
        order.extend((start..self.queue.len()).filter(|i| !indices.contains(i)));
        self.queue_entries(&indices, &order)
    }

    /// Add to Queue in Up Next: the chosen entries go to the end of your
    /// queue, together, in their Up Next order (as Add to Queue does
    /// everywhere else); how many moved.
    pub fn entries_last(&mut self, ids: &[QueueEntryId]) -> usize {
        let indices = self.chosen_entries(ids);
        let start = self.queue_position + 1;
        let source = self.source_start();
        let mut order: Vec<usize> = (0..start).collect();
        order.extend((start..source).filter(|i| !indices.contains(i)));
        order.extend(&indices);
        order.extend((source..self.queue.len()).filter(|i| !indices.contains(i)));
        self.queue_entries(&indices, &order)
    }

    /// Clear your queue (the songs you queued that have not played).
    pub fn clear_queued(&mut self) -> bool {
        let start = self.queue_position + 1;
        let ids: Vec<QueueEntryId> = self.queue_entry_ids[start..self.source_start()].to_vec();
        self.remove_entries(&ids) > 0
    }

    /// Play the entry at `index` now. Your queued songs it passes over stay
    /// queued, right after it; the source's songs it passes count as played.
    pub fn jump_to(&mut self, index: usize) -> bool {
        if index >= self.queue.len() {
            return false;
        }
        if index > self.queue_position {
            let start = self.queue_position + 1;
            let passed = start..index;
            let queued: Vec<usize> = passed
                .clone()
                .filter(|&i| self.is_queued(self.queue_entry_ids[i]))
                .collect();
            let mut order: Vec<usize> = (0..start).collect();
            order.extend(passed.filter(|i| !queued.contains(i)));
            order.push(index);
            order.extend(&queued);
            order.extend(index + 1..self.queue.len());
            let id = self.queue_entry_ids[index];
            self.permute(&order);
            self.queue_position = self.entry_index(id).unwrap_or(index);
        } else {
            self.queue_position = index;
        }
        self.anchor_queued();
        self.position_ms = 0;
        self.invalidate();
        true
    }

    /// Repeat All starts over: the songs you queued have played and leave
    /// (unless they are all there is), the first song plays next.
    pub fn wrap(&mut self) -> bool {
        if self.queue.is_empty() {
            return false;
        }
        self.drop_played_queued(None);
        self.queue_position = 0;
        self.position_ms = 0;
        self.invalidate();
        true
    }

    /// The decoder went on from the last song to `id` near the start
    /// (gapless Repeat All).
    pub(crate) fn wrap_to(&mut self, id: QueueEntryId) {
        self.drop_played_queued(Some(id));
        if let Some(position) = self.entry_index(id) {
            self.queue_position = position;
        }
    }

    fn drop_played_queued(&mut self, keep: Option<QueueEntryId>) {
        let source_left = self
            .queue_entry_ids
            .iter()
            .any(|id| !self.is_queued(*id) || Some(*id) == keep);
        if !source_left {
            // Only queued songs: they are what repeats now.
            self.queue_queued.clear();
            return;
        }
        let gone: Vec<usize> = (0..self.queue.len())
            .filter(|&i| {
                let id = self.queue_entry_ids[i];
                self.is_queued(id) && Some(id) != keep
            })
            .collect();
        for &i in gone.iter().rev() {
            self.queue.remove(i);
            self.queue_entry_ids.remove(i);
            if i < self.queue_position {
                self.queue_position -= 1;
            }
        }
        self.queue_position = self.queue_position.min(self.queue.len().saturating_sub(1));
        self.queue_queued.clear();
    }

    /// The next `n` entries in play order, as the decoder pre-opens them.
    /// With Repeat All the start follows the end, without the queued songs
    /// that will have played by then. Nothing follows a station, and
    /// Repeat One repeats the song playing.
    pub fn upcoming(&self, n: usize) -> (Vec<Track>, Vec<QueueEntryId>) {
        if self.settings.repeat == RepeatMode::Track || self.current().is_some_and(Track::is_live) {
            return (vec![], vec![]);
        }
        let mut indices: Vec<usize> = (self.queue_position + 1..self.queue.len())
            .take(n)
            .collect();
        if self.settings.repeat == RepeatMode::All && indices.len() < n {
            let only_queued = self.queue_entry_ids.iter().all(|id| self.is_queued(*id));
            indices.extend(
                (0..=self.queue_position.min(self.queue.len().saturating_sub(1)))
                    .filter(|&i| only_queued || !self.is_queued(self.queue_entry_ids[i]))
                    .take(n - indices.len()),
            );
        }
        (
            indices.iter().map(|&i| self.queue[i].clone()).collect(),
            indices.iter().map(|&i| self.queue_entry_ids[i]).collect(),
        )
    }

    /// Name the source and its own order: `natural` lists queue indices in
    /// the order the source has them (a shuffled play keeps the album's).
    pub fn set_source(&mut self, label: impl Into<String>, natural: Option<&[usize]>) {
        self.queue_source = label.into();
        self.queue_order = match natural {
            Some(order) => order
                .iter()
                .filter_map(|&i| self.queue_entry_ids.get(i).copied())
                .collect(),
            None => self.queue_entry_ids.clone(),
        };
    }

    /// Shuffle the source's songs still to come (Shuffle turned on).
    pub fn shuffle_source(&mut self, seed: u64) -> bool {
        let start = self.source_start();
        if self.queue.len() < start + 2 {
            return false;
        }
        // A queue without its source's order (restored from a session of
        // Beta 8.32 or older): the order now is the one Shuffle off restores.
        if self.queue_order.is_empty() {
            self.queue_order = self.queue_entry_ids.clone();
        }
        let mut order: Vec<usize> = (0..self.queue.len()).collect();
        shuffle_from(&mut order, start, seed);
        self.permute(&order);
        true
    }

    /// Shuffle the source's songs still to come as whole albums (Shuffle
    /// Albums turned on): the album playing goes on first.
    pub fn shuffle_source_albums(&mut self, seed: u64) -> bool {
        let start = self.source_start();
        if self.queue.len() < start + 2 {
            return false;
        }
        if self.queue_order.is_empty() {
            self.queue_order = self.queue_entry_ids.clone();
        }
        let now = self.queue_position.min(self.queue.len() - 1);
        let mut rest = vec![now];
        rest.extend(start..self.queue.len());
        let picked = {
            let tracks: Vec<&Track> = rest.iter().map(|&i| &self.queue[i]).collect();
            album_shuffle(&tracks, Some(0), seed)
        };
        let mut order: Vec<usize> = (0..start).collect();
        order.extend(picked.into_iter().filter(|&p| p != 0).map(|p| rest[p]));
        self.permute(&order);
        true
    }

    /// Back to the source's own order (Shuffle turned off): its songs still
    /// to come continue after the one it is at, the ones before follow.
    pub fn unshuffle_source(&mut self) -> bool {
        if self.queue_order.is_empty() {
            return false;
        }
        let rank: HashMap<QueueEntryId, usize> = self
            .queue_order
            .iter()
            .enumerate()
            .map(|(rank, id)| (*id, rank))
            .collect();
        let at = (0..=self.queue_position.min(self.queue.len().saturating_sub(1)))
            .rev()
            .find_map(|i| rank.get(&self.queue_entry_ids[i]).copied());
        let start = self.source_start();
        let mut rest: Vec<usize> = (start..self.queue.len()).collect();
        rest.sort_by_key(|&i| match rank.get(&self.queue_entry_ids[i]) {
            Some(&r) if at.is_none_or(|a| r > a) => (0, r, i),
            Some(&r) => (1, r, i),
            None => (2, 0, i),
        });
        let mut order: Vec<usize> = (0..start).collect();
        order.extend(rest);
        self.permute(&order);
        true
    }

    /// `track` played: newest last, at most `MAX_HISTORY`; stations and
    /// audiobook parts have their own places.
    pub fn note_played(&mut self, track: &Track) {
        if track.is_live() || track.book || track.path.as_os_str().is_empty() {
            return;
        }
        let entry = HistoryEntry {
            source_id: track.source_id.clone(),
            path: track.path.clone(),
        };
        if self.history.last() == Some(&entry) {
            return;
        }
        self.history.retain(|e| *e != entry);
        self.history.push(entry);
        if self.history.len() > MAX_HISTORY {
            let extra = self.history.len() - MAX_HISTORY;
            self.history.drain(..extra);
        }
    }

    /// History entries as library indices, newest last (songs the library
    /// no longer has are left out).
    pub fn history_tracks(&self) -> Vec<usize> {
        let index: HashMap<(&str, &std::path::Path), usize> = self
            .library
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| ((t.source_id.as_str(), t.path.as_path()), i))
            .collect();
        self.history
            .iter()
            .filter_map(|e| {
                index
                    .get(&(e.source_id.as_str(), e.path.as_path()))
                    .copied()
            })
            .collect()
    }

    /// A restored queue names its library songs only (`stored`): take
    /// their details from the library once it is known.
    pub fn rehydrate_queue(&mut self) {
        if !self.queue.iter().any(is_stub) || self.library.tracks.is_empty() {
            return;
        }
        let index: HashMap<(&str, &std::path::Path), usize> = self
            .library
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| ((t.source_id.as_str(), t.path.as_path()), i))
            .collect();
        for track in &mut self.queue {
            if !is_stub(track) {
                continue;
            }
            if let Some(&i) = index.get(&(track.source_id.as_str(), track.path.as_path())) {
                *track = self.library.tracks[i].clone();
            }
        }
    }
}

/// A queue entry restored from a reference, not yet matched to the library.
fn is_stub(track: &Track) -> bool {
    track.size == 0 && !track.is_live() && !track.path.as_os_str().is_empty()
}

/// The queue as the session stores it: library songs by reference (source
/// and path, about a quarter of a full record; 5 000 songs were ≈4 MB per
/// song change), stations and anything else whole. Older sessions with
/// whole records read as before.
pub(crate) mod stored {
    use super::*;
    use serde::{ser::SerializeSeq, Deserializer, Serializer};

    #[derive(Serialize)]
    struct RefOut<'a> {
        r#ref: (&'a str, &'a std::path::Path, i64),
    }
    #[derive(Deserialize)]
    struct RefIn {
        r#ref: (String, PathBuf, i64),
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Ref(RefIn),
        Full(Box<Track>),
    }

    pub fn serialize<S: Serializer>(queue: &[Track], s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(queue.len()))?;
        for track in queue {
            if track.id > 0 && !track.is_live() {
                seq.serialize_element(&RefOut {
                    r#ref: (&track.source_id, &track.path, track.id),
                })?;
            } else {
                seq.serialize_element(track)?;
            }
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Track>, D::Error> {
        let stored: Vec<Stored> = Vec::deserialize(d)?;
        Ok(stored
            .into_iter()
            .map(|entry| match entry {
                Stored::Full(track) => *track,
                Stored::Ref(RefIn {
                    r#ref: (source_id, path, id),
                }) => Track {
                    id,
                    filename: path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    source_id,
                    path,
                    online: true,
                    ..Default::default()
                },
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: i64) -> Track {
        Track {
            id,
            title: format!("s{id}"),
            path: format!("/m/{id}.flac").into(),
            source_id: "internal".into(),
            size: 1,
            online: true,
            ..Default::default()
        }
    }
    /// An album of `n` songs playing from its first.
    fn album(n: i64) -> AppModel {
        let mut m = AppModel::default();
        m.replace_queue((1..=n).map(song).collect(), 0).unwrap();
        m.set_source("Album", None);
        m
    }
    fn titles(m: &AppModel) -> Vec<String> {
        m.queue.iter().map(|t| t.title.clone()).collect()
    }
    fn upcoming(m: &AppModel) -> Vec<String> {
        m.queue[m.queue_position + 1..]
            .iter()
            .map(|t| t.title.clone())
            .collect()
    }

    /// Song `id` of album `album` (its folder), track `n`.
    fn album_song(id: i64, album: &str, n: u32) -> Track {
        Track {
            title: format!("{album}{n}"),
            album: album.into(),
            track: n,
            path: format!("/m/{album}/{n:02} x.mp3").into(),
            filename: format!("{n:02} x.mp3"),
            ..song(id)
        }
    }
    fn runs(titles: &[String]) -> Vec<String> {
        let mut albums: Vec<String> = vec![];
        for t in titles {
            let album = t.trim_end_matches(char::is_numeric).to_string();
            if albums.last() != Some(&album) {
                albums.push(album);
            }
        }
        albums
    }

    #[test]
    fn shuffle_albums_keeps_each_album_whole_and_in_its_order() {
        // Three albums, their songs mixed up as a playlist might hold them.
        let songs: Vec<Track> = [
            ("a", 2),
            ("b", 1),
            ("a", 1),
            ("c", 1),
            ("b", 2),
            ("c", 2),
            ("a", 3),
        ]
        .iter()
        .enumerate()
        .map(|(i, (album, n))| album_song(i as i64, album, *n))
        .collect();
        let refs: Vec<&Track> = songs.iter().collect();
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..32 {
            let order = album_shuffle(&refs, None, seed);
            let titles: Vec<String> = order.iter().map(|&i| songs[i].title.clone()).collect();
            // Every song once; each album one run, in track order.
            assert_eq!(order.len(), songs.len());
            assert_eq!(runs(&titles).len(), 3, "{titles:?}");
            for album in ["a", "b", "c"] {
                let mine: Vec<&String> = titles.iter().filter(|t| t.starts_with(album)).collect();
                let mut sorted = mine.clone();
                sorted.sort();
                assert_eq!(mine, sorted);
            }
            seen.insert(runs(&titles).join(""));
        }
        assert!(
            seen.len() > 3,
            "the albums come in different orders: {seen:?}"
        );
        // A picked song: its album from that song on, the others after, its
        // songs before it last.
        let picked = album_shuffle(&refs, Some(0), 7);
        let titles: Vec<String> = picked.iter().map(|&i| songs[i].title.clone()).collect();
        assert_eq!(&titles[..2], ["a2", "a3"]);
        assert_eq!(titles.last().unwrap(), "a1");
    }

    #[test]
    fn disc_folders_and_loose_songs_make_albums_by_folder_and_tag() {
        let mut one = album_song(1, "x", 1);
        one.path = "/m/Best Of/CD1/01 a.mp3".into();
        let mut two = album_song(2, "x", 1);
        two.disc = 2;
        two.path = "/m/Best Of/CD 2/01 b.mp3".into();
        // Same album name in another folder: another album.
        let mut other = album_song(3, "x", 1);
        other.path = "/m/Other/01 c.mp3".into();
        let refs = [&one, &two, &other];
        for seed in 0..16 {
            let order = album_shuffle(&refs, None, seed);
            let at = |id: usize| order.iter().position(|&i| i == id).unwrap();
            assert_eq!(at(1), at(0) + 1, "CD 2 follows CD 1");
        }
    }

    #[test]
    fn shuffle_albums_on_keeps_the_album_playing_then_mixes_the_rest() {
        let songs: Vec<Track> = ["a", "b", "c", "d"]
            .iter()
            .flat_map(|album| (1..=3).map(move |n| (album, n)))
            .enumerate()
            .map(|(i, (album, n))| album_song(i as i64, album, n))
            .collect();
        let mut m = AppModel::default();
        m.replace_queue(songs, 0).unwrap();
        m.set_source("Folder", None);
        m.step(1); // a2 playing
        m.queue_last(vec![song(99)]).unwrap();
        assert!(m.shuffle_source_albums(5));
        let next = upcoming(&m);
        assert_eq!(
            &next[..2],
            ["s99", "a3"],
            "your queue, then the album playing"
        );
        assert_eq!(
            runs(&next[2..]).len(),
            3,
            "the other albums whole: {next:?}"
        );
        assert!(m.unshuffle_source());
        assert_eq!(upcoming(&m)[..3], ["s99", "a3", "b1"]);
    }

    #[test]
    fn your_queue_plays_before_the_rest_of_the_album() {
        let mut m = album(4);
        // Add to Queue goes after your queued songs, not to the album's end.
        m.queue_last(vec![song(10)]).unwrap();
        m.queue_last(vec![song(11)]).unwrap();
        assert_eq!(upcoming(&m), ["s10", "s11", "s2", "s3", "s4"]);
        // Play Next goes on top of them: the newest first.
        m.queue_next(vec![song(12)]).unwrap();
        m.queue_next(vec![song(13), song(14)]).unwrap();
        assert_eq!(
            upcoming(&m),
            ["s13", "s14", "s12", "s10", "s11", "s2", "s3", "s4"]
        );
        assert_eq!(m.queued_count(), 5);
        assert_eq!(m.source_start(), 6);
        // An empty queue becomes the songs, the first one playing.
        let mut empty = AppModel::default();
        assert_eq!(empty.queue_next(vec![song(1), song(2)]), Ok(true));
        assert_eq!(empty.current().unwrap().id, 1);
    }

    #[test]
    fn previous_and_jumps_keep_your_queue_right_after_the_song_playing() {
        let mut m = album(5);
        m.step(1);
        m.step(1); // s3 playing
        m.queue_last(vec![song(10), song(11)]).unwrap();
        assert_eq!(upcoming(&m), ["s10", "s11", "s4", "s5"]);
        // Previous: s2 plays, your songs still come next.
        m.step(-1);
        assert_eq!(m.current().unwrap().id, 2);
        assert_eq!(upcoming(&m), ["s10", "s11", "s3", "s4", "s5"]);
        // Jumping to s5 passes your songs: they stay queued after it, s3 and
        // s4 count as played.
        let at = m.queue.iter().position(|t| t.id == 5).unwrap();
        m.jump_to(at);
        assert_eq!(m.current().unwrap().id, 5);
        assert_eq!(upcoming(&m), ["s10", "s11"]);
        assert_eq!(m.queued_count(), 2);
    }

    #[test]
    fn moving_crosses_between_your_queue_and_the_album_one_step_at_a_time() {
        let mut m = album(3);
        m.queue_last(vec![song(10)]).unwrap();
        let s10 = m.queue_entry_ids[1];
        let s2 = m.queue_entry_ids[2];
        // Down: out of your queue first (same place), then past s2.
        assert!(m.move_entry(s10, false));
        assert!(!m.is_queued(s10));
        assert_eq!(upcoming(&m), ["s10", "s2", "s3"]);
        assert!(m.move_entry(s10, false));
        assert_eq!(upcoming(&m), ["s2", "s10", "s3"]);
        // s2 up: into your queue.
        assert!(m.move_entry(s2, true));
        assert!(m.is_queued(s2));
        assert!(
            !m.move_entry(s2, true),
            "the top of your queue stays below the song playing"
        );
        // The last one cannot go further down.
        let s3 = *m.queue_entry_ids.last().unwrap();
        assert!(!m.move_entry(s3, false));
        // The song playing never moves.
        assert!(!m.move_entry(m.queue_entry_ids[0], false));
    }

    #[test]
    fn chosen_songs_play_next_together_and_can_be_removed() {
        let mut m = album(6);
        let ids: Vec<QueueEntryId> = vec![m.queue_entry_ids[4], m.queue_entry_ids[2]];
        assert_eq!(m.entries_next(&ids), 2);
        assert_eq!(upcoming(&m), ["s3", "s5", "s2", "s4", "s6"]);
        assert_eq!(m.queued_count(), 2);
        // Remove two, the song playing never.
        let current = m.current_entry_id().unwrap();
        let s4 = m.queue_entry_ids[m.queue.iter().position(|t| t.id == 4).unwrap()];
        assert_eq!(m.remove_entries(&[current, s4]), 1);
        assert_eq!(upcoming(&m), ["s3", "s5", "s2", "s6"]);
        assert!(m.clear_queued());
        assert_eq!(upcoming(&m), ["s2", "s6"]);
    }

    #[test]
    fn add_to_queue_in_up_next_moves_songs_to_the_end_of_your_queue() {
        let mut m = album(6);
        assert_eq!(m.entries_next(&[m.queue_entry_ids[2]]), 1);
        assert_eq!(upcoming(&m), ["s3", "s2", "s4", "s5", "s6"]);
        // After your queued s3, before the rest of the album, in Up Next order.
        let ids = vec![m.queue_entry_ids[5], m.queue_entry_ids[3]];
        assert_eq!(m.entries_last(&ids), 2);
        assert_eq!(upcoming(&m), ["s3", "s4", "s6", "s2", "s5"]);
        assert_eq!(m.queued_count(), 3);
        // A queued song moves to the end of your queue; the song playing never.
        let s3 = m.queue_entry_ids[1];
        assert_eq!(m.entries_last(&[s3, m.current_entry_id().unwrap()]), 1);
        assert_eq!(upcoming(&m), ["s4", "s6", "s3", "s2", "s5"]);
    }

    #[test]
    fn repeat_all_starts_over_without_your_played_songs() {
        let mut m = album(3);
        m.settings.repeat = RepeatMode::All;
        m.queue_next(vec![song(10)]).unwrap();
        m.step(1);
        m.step(1);
        m.step(1); // s3, the last
        let (next, _) = m.upcoming(8);
        let next: Vec<i64> = next.iter().map(|t| t.id).collect();
        assert_eq!(next, [1, 2, 3], "the start follows, s10 does not");
        assert!(m.wrap());
        assert_eq!(titles(&m), ["s1", "s2", "s3"]);
        assert_eq!(m.queue_position, 0);
        // Repeat One and stations have nothing after them.
        m.settings.repeat = RepeatMode::Track;
        assert!(m.upcoming(8).0.is_empty());
    }

    #[test]
    fn gapless_wrap_lands_on_the_first_song() {
        let mut m = album(3);
        m.settings.repeat = RepeatMode::All;
        m.queue_next(vec![song(10)]).unwrap();
        for _ in 0..3 {
            m.step(1);
        }
        let first = m.queue_entry_ids[0];
        let generation = m.generation;
        m.apply(crate::Event::TrackBoundary {
            generation,
            next_entry_id: Some(first),
            next_track_id: Some(1),
            output_position_ms: 0,
        });
        assert_eq!(m.current().unwrap().id, 1);
        assert_eq!(titles(&m), ["s1", "s2", "s3"]);
    }

    #[test]
    fn shuffle_on_and_off_keeps_your_queue_and_restores_the_album() {
        let mut m = album(8);
        m.step(1); // s2
        m.queue_last(vec![song(10)]).unwrap();
        assert!(m.shuffle_source(42));
        assert_eq!(upcoming(&m)[0], "s10", "your queue is not shuffled");
        assert_ne!(upcoming(&m)[1..], ["s3", "s4", "s5", "s6", "s7", "s8"]);
        // Two shuffled songs play, then Shuffle goes off.
        m.step(1);
        m.step(1);
        let at = m.current().unwrap().id;
        assert!(m.unshuffle_source());
        let rest: Vec<i64> = m.queue[m.queue_position + 1..]
            .iter()
            .map(|t| t.id)
            .collect();
        let after: Vec<i64> = (at + 1..=8).filter(|id| rest.contains(id)).collect();
        assert_eq!(
            rest[..after.len()],
            after[..],
            "the album goes on after {at}"
        );
    }

    #[test]
    fn shuffle_off_restores_a_queue_restored_without_its_order() {
        // Found on the owner's player: a Beta 8.32 session has no order.
        let mut m = album(8);
        m.queue_order.clear();
        assert!(m.shuffle_source(7));
        assert_ne!(upcoming(&m), ["s2", "s3", "s4", "s5", "s6", "s7", "s8"]);
        assert!(m.unshuffle_source());
        assert_eq!(upcoming(&m), ["s2", "s3", "s4", "s5", "s6", "s7", "s8"]);
    }

    #[test]
    fn history_keeps_the_last_fifty_without_repeats() {
        let mut m = AppModel::default();
        for id in 1..=60 {
            m.note_played(&song(id));
        }
        m.note_played(&song(60));
        assert_eq!(m.history.len(), MAX_HISTORY);
        assert_eq!(m.history[0].path, PathBuf::from("/m/11.flac"));
        // Played again: it moves to the newest place.
        m.note_played(&song(30));
        assert_eq!(m.history.last().unwrap().path, PathBuf::from("/m/30.flac"));
        assert_eq!(m.history.len(), MAX_HISTORY);
        m.library.tracks = vec![song(30), song(59)];
        assert_eq!(m.history_tracks(), [1, 0], "newest last, library indices");
    }

    #[test]
    fn the_session_stores_library_songs_by_reference() {
        let mut m = album(3);
        m.queue.push(Track::radio(&crate::RadioStation {
            name: "Station".into(),
            url: "http://example.invalid/stream".into(),
            ..Default::default()
        }));
        m.queue_entry_ids.push(QueueEntryId(99));
        let bytes = serde_json::to_vec(&m).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            text.contains(r#"{"ref":["internal","/m/1.flac",1]}"#),
            "{text}"
        );
        let mut back: AppModel = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            back.queue[0].title, "",
            "a reference until the library is known"
        );
        assert_eq!(
            back.queue[3].title, m.queue[3].title,
            "stations are kept whole"
        );
        back.library.tracks = m.queue[..3].to_vec();
        back.rehydrate_queue();
        assert_eq!(titles(&back)[..3], ["s1", "s2", "s3"]);
        // A session with whole records (Beta 8.32 and older) reads as before.
        let old = serde_json::json!({"playback":"paused","queue":[m.queue[0]],"queue_position":0,
            "position_ms":0,"output":{"kind":"wired"},"settings":serde_json::to_value(&m.settings).unwrap()});
        let old: AppModel = serde_json::from_value(old).unwrap();
        assert_eq!(old.queue[0].title, "s1");
    }
}
