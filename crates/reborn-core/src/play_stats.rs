//! What the listener played, for Music's Recently Played and Most Played (Y2Reborn #158), and
//! what arrived last, for Recently Added (#109).
//!
//! A play counts by the scrobbling rule (`scrobble::Tracker`: heard for half the song or four
//! minutes; seeks, pauses and skips add nothing), with or without Last.fm and whatever the
//! song's tags say. Each song keeps its play count and when it was last played, as a sequence
//! number rather than a time (the clock may not be set). The record is keyed by the song's
//! path, so a rescan or a rebuilt library keeps it. Pure logic: the player loads and saves it.
use crate::{scrobble::MIN_TRACK_MS, Track};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// How many songs each of the three lists shows.
pub const LIST_LEN: usize = 100;
/// Songs remembered at most; the longest unplayed go first.
const KEEP: usize = 5_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Stat {
    plays: u32,
    /// `PlayStats::seq` when it last counted.
    last: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayStats {
    seq: u64,
    /// By the song's path.
    songs: HashMap<String, Stat>,
    /// Changes with every counted play (the lists are built again).
    #[serde(skip)]
    generation: u64,
}

/// Whether a play of `track` can count: a song long enough, not a station, book or film.
pub fn counts(track: &Track) -> bool {
    track.is_music() && !track.is_radio() && track.duration_ms >= MIN_TRACK_MS
}

impl PlayStats {
    /// The record at `path`; none (or an unreadable one) starts empty.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        crate::atomic_write(path, &bytes)
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// One counted play of `track`.
    pub fn record(&mut self, track: &Track) {
        if !counts(track) {
            return;
        }
        self.seq += 1;
        self.generation = self.generation.wrapping_add(1);
        let stat = self
            .songs
            .entry(track.path.to_string_lossy().into_owned())
            .or_default();
        stat.plays = stat.plays.saturating_add(1);
        stat.last = self.seq;
        if self.songs.len() > KEEP {
            let mut lasts: Vec<u64> = self.songs.values().map(|s| s.last).collect();
            lasts.sort_unstable();
            let cut = lasts[self.songs.len() - KEEP];
            self.songs.retain(|_, s| s.last >= cut);
        }
    }
    /// How often `track` was played.
    pub fn plays(&self, track: &Track) -> u32 {
        self.stat(track).map_or(0, |s| s.plays)
    }
    fn stat(&self, track: &Track) -> Option<Stat> {
        self.songs.get(track.path.to_str()?).copied()
    }
    /// The songs in `tracks` (indices) played most recently, newest first.
    pub fn recently_played(&self, tracks: &[Track]) -> Vec<usize> {
        self.ranked(tracks, |s| (s.last, 0))
    }
    /// The songs played most often; the more recent first among equals.
    pub fn most_played(&self, tracks: &[Track]) -> Vec<usize> {
        self.ranked(tracks, |s| (u64::from(s.plays), s.last))
    }
    fn ranked(&self, tracks: &[Track], rank: impl Fn(Stat) -> (u64, u64)) -> Vec<usize> {
        if self.songs.is_empty() {
            return Vec::new();
        }
        let mut found: Vec<(usize, (u64, u64))> = tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| counts(t))
            .filter_map(|(i, t)| Some((i, rank(self.stat(t)?))))
            .collect();
        found.sort_by_key(|a| std::cmp::Reverse(a.1));
        found.into_iter().take(LIST_LEN).map(|(i, _)| i).collect()
    }
}

/// The songs that arrived last, newest file first (the library's own order among equals).
pub fn recently_added(tracks: &[Track]) -> Vec<usize> {
    let mut found: Vec<usize> = (0..tracks.len())
        .filter(|&i| tracks[i].is_music())
        .collect();
    found.sort_by(|&a, &b| tracks[b].mtime.cmp(&tracks[a].mtime).then(b.cmp(&a)));
    found.truncate(LIST_LEN);
    found
}

#[cfg(test)]
#[path = "play_stats_tests.rs"]
mod tests;
