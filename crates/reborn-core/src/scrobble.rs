//! Scrobbling rules (Last.fm): which plays count, and when.
//!
//! A library track longer than 30 s counts once it has been *heard* for half
//! its length or 4 minutes, whichever comes first. Heard time is measured as
//! small forward steps of the playback position that are no larger than the
//! time that actually passed: paused time, seeks and stalls add nothing. A
//! play is one queue occurrence; returning to its start (repeat, Previous)
//! begins a new play. Internet radio, FM and untagged files never count.
//! Pure logic: the player feeds observations, nothing here does I/O.
use crate::Track;
use serde::{Deserialize, Serialize};

/// Shortest track that can count (Last.fm).
pub const MIN_TRACK_MS: u64 = 30_000;
/// Heard time that always counts, however long the track.
pub const CAP_MS: u64 = 240_000;
/// Position steps up to this much larger than the elapsed time still count
/// (position reports and the clock are not sampled at the same instant).
const SLACK_MS: u64 = 1_500;
/// A position this close to the start, after the play had moved on, is a
/// new play of the same occurrence.
const RESTART_MS: u64 = 2_000;

/// What Last.fm needs about one play.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Play {
    pub artist: String,
    pub title: String,
    #[serde(default)]
    pub album: String,
    /// Only when it differs from the artist.
    #[serde(default)]
    pub album_artist: String,
    pub duration_s: u32,
    #[serde(default)]
    pub track_number: u32,
}

/// A counted play with its start time (UTC seconds).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scrobble {
    #[serde(flatten)]
    pub play: Play,
    pub timestamp: i64,
}

/// A counted play whose start time may still be unknown (no trustworthy
/// clock yet). `started_mono_ms` lets the player stamp it later in the same
/// boot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counted {
    pub play: Play,
    pub started_unix: Option<i64>,
    pub started_mono_ms: u64,
}
impl Counted {
    /// The scrobble, once the clock is known (`now_unix` at `now_mono_ms`).
    pub fn stamp(&self, now_mono_ms: u64, now_unix: Option<i64>) -> Option<Scrobble> {
        let timestamp = match self.started_unix {
            Some(t) => t,
            None => {
                let ago = now_mono_ms.saturating_sub(self.started_mono_ms) / 1000;
                now_unix? - ago as i64
            }
        };
        Some(Scrobble {
            play: self.play.clone(),
            timestamp,
        })
    }
}

/// The play a track would be, or `None` when it can never count.
pub fn play_of(track: &Track) -> Option<Play> {
    let artist = track.artist.trim();
    let title = track.title.trim();
    // Audiobooks are not scrobbled: they are not music listening.
    if track.is_live()
        || track.book
        || artist.is_empty()
        || title.is_empty()
        || title == track.filename
        || track.duration_ms < MIN_TRACK_MS
    {
        return None;
    }
    let album_artist = track.tagged_album_artist().trim();
    Some(Play {
        artist: artist.to_owned(),
        title: title.to_owned(),
        album: track.album.trim().to_owned(),
        album_artist: if album_artist.eq_ignore_ascii_case(artist) {
            String::new()
        } else {
            album_artist.to_owned()
        },
        duration_s: (track.duration_ms / 1000) as u32,
        track_number: track.track,
    })
}

/// Heard time a play needs: half its length, at most four minutes.
pub fn threshold_ms(duration_ms: u64) -> u64 {
    (duration_ms / 2).min(CAP_MS)
}

#[derive(Clone, Debug)]
struct Current {
    entry: u64,
    play: Option<Play>,
    duration_ms: u64,
    started_unix: Option<i64>,
    started_mono_ms: u64,
    last_position_ms: u64,
    last_mono_ms: u64,
    heard_ms: u64,
    counted: bool,
}

/// Follows the occurrence playing now and says when it counts.
#[derive(Clone, Debug, Default)]
pub struct Tracker {
    current: Option<Current>,
    plays: u64,
}
impl Tracker {
    /// Changes whenever a new play begins (another occurrence, or the same
    /// one from its start again).
    pub fn play_number(&self) -> u64 {
        self.plays
    }
    /// One observation: the queue occurrence and track playing (if any),
    /// whether audio is actually playing, its position, a monotonic clock and
    /// the wall clock when it is trustworthy. Returns a play the moment it
    /// counts (once per play).
    pub fn observe(
        &mut self,
        now: Option<(u64, &Track)>,
        playing: bool,
        position_ms: u64,
        now_mono_ms: u64,
        now_unix: Option<i64>,
    ) -> Option<Counted> {
        let Some((entry, track)) = now else {
            self.current = None;
            return None;
        };
        let restart = self.current.as_ref().is_some_and(|c| {
            c.entry == entry
                && position_ms < RESTART_MS
                && c.last_position_ms >= RESTART_MS + SLACK_MS
                && position_ms < c.last_position_ms
        });
        if restart || self.current.as_ref().is_none_or(|c| c.entry != entry) {
            self.plays = self.plays.wrapping_add(1);
            self.current = Some(Current {
                entry,
                play: play_of(track),
                duration_ms: track.duration_ms,
                started_unix: now_unix.map(|t| t - (position_ms / 1000) as i64),
                started_mono_ms: now_mono_ms.saturating_sub(position_ms),
                last_position_ms: position_ms,
                last_mono_ms: now_mono_ms,
                heard_ms: 0,
                counted: false,
            });
            return None;
        }
        let c = self.current.as_mut()?;
        let elapsed = now_mono_ms.saturating_sub(c.last_mono_ms);
        if playing && position_ms > c.last_position_ms {
            let step = position_ms - c.last_position_ms;
            if step <= elapsed + SLACK_MS {
                c.heard_ms += step;
            }
        }
        c.last_position_ms = position_ms;
        c.last_mono_ms = now_mono_ms;
        if c.counted || c.heard_ms < threshold_ms(c.duration_ms) {
            return None;
        }
        let play = c.play.clone()?;
        c.counted = true;
        Some(Counted {
            play,
            started_unix: c.started_unix,
            started_mono_ms: c.started_mono_ms,
        })
    }
}

/// One line of a Rockbox AUDIOSCROBBLER/1.1 `.scrobbler.log` (timestamps UTC).
pub fn log_line(s: &Scrobble) -> String {
    let clean = |v: &str| v.replace(['\t', '\n', '\r'], " ");
    format!(
        "{}\t{}\t{}\t{}\t{}\tL\t{}\t\n",
        clean(&s.play.artist),
        clean(&s.play.album),
        clean(&s.play.title),
        if s.play.track_number > 0 {
            s.play.track_number.to_string()
        } else {
            String::new()
        },
        s.play.duration_s,
        s.timestamp
    )
}
/// The header a new `.scrobbler.log` starts with.
pub fn log_header(client: &str) -> String {
    format!("#AUDIOSCROBBLER/1.1\n#TZ/UTC\n#CLIENT/{client}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(ms: u64) -> Track {
        Track {
            id: 7,
            source_id: "internal".into(),
            filename: "07 Song.flac".into(),
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            album_artist: "Artist".into(),
            track: 7,
            duration_ms: ms,
            ..Default::default()
        }
    }
    /// Plays `t` from `from` to `to` ms in 1 s steps, real time.
    fn run(
        tr: &mut Tracker,
        entry: u64,
        t: &Track,
        clock: &mut u64,
        from: u64,
        to: u64,
    ) -> Vec<Counted> {
        let mut out = vec![];
        let mut p = from;
        while p <= to {
            if let Some(c) = tr.observe(
                Some((entry, t)),
                true,
                p,
                *clock,
                Some(1_700_000_000 + (*clock / 1000) as i64),
            ) {
                out.push(c);
            }
            p += 1000;
            *clock += 1000;
        }
        out
    }

    #[test]
    fn half_the_length_or_four_minutes_counts_once() {
        let t = track(200_000);
        let mut tr = Tracker::default();
        let mut clock = 10_000;
        let got = run(&mut tr, 1, &t, &mut clock, 0, 200_000);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].play.title, "Song");
        assert_eq!(got[0].play.album_artist, "", "same as the artist");
        assert_eq!(got[0].started_unix, Some(1_700_000_010));
        assert_eq!(threshold_ms(200_000), 100_000);
        assert_eq!(threshold_ms(3_600_000), CAP_MS);
        // A one-hour track counts after four minutes.
        let long = track(3_600_000);
        let got = run(&mut tr, 2, &long, &mut clock, 0, 239_000);
        assert!(got.is_empty());
        let got = run(&mut tr, 2, &long, &mut clock, 240_000, 241_000);
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn seeks_pauses_and_skips_add_nothing() {
        let t = track(200_000);
        let mut tr = Tracker::default();
        let mut clock = 0;
        assert!(run(&mut tr, 1, &t, &mut clock, 0, 40_000).is_empty());
        // Seek to 180 s: the jump is not heard time.
        assert!(run(&mut tr, 1, &t, &mut clock, 180_000, 200_000).is_empty());
        // Paused: position stands still, then the clock races on.
        let mut tr = Tracker::default();
        let mut clock = 0;
        run(&mut tr, 3, &t, &mut clock, 0, 50_000);
        assert_eq!(
            tr.observe(Some((3, &t)), false, 50_000, clock + 600_000, None),
            None
        );
        clock += 600_000;
        // Position reported while paused (a seek in pause) does not count.
        assert_eq!(
            tr.observe(Some((3, &t)), false, 99_000, clock + 1000, None),
            None
        );
        clock += 1000;
        assert_eq!(run(&mut tr, 3, &t, &mut clock, 99_000, 147_000).len(), 0);
        assert_eq!(run(&mut tr, 3, &t, &mut clock, 148_000, 151_000).len(), 1);
        // Skipped after 30 s: nothing.
        let mut tr = Tracker::default();
        assert!(run(&mut tr, 4, &t, &mut clock, 0, 30_000).is_empty());
        assert!(run(&mut tr, 5, &t, &mut clock, 0, 1_000).is_empty());
    }

    #[test]
    fn repeat_and_previous_start_a_new_play_gapless_moves_on() {
        let t = track(60_000);
        let mut tr = Tracker::default();
        let mut clock = 0;
        assert_eq!(run(&mut tr, 1, &t, &mut clock, 0, 60_000).len(), 1);
        let first = tr.play_number();
        // Repeat one: the same occurrence from the start again.
        assert_eq!(run(&mut tr, 1, &t, &mut clock, 0, 60_000).len(), 1);
        let repeated = tr.play_number();
        assert_ne!(
            repeated, first,
            "a repeat is a new play (Now Playing again)"
        );
        // Gapless boundary: the next occurrence begins mid-period.
        assert_eq!(run(&mut tr, 2, &t, &mut clock, 400, 31_400).len(), 1);
        assert_ne!(tr.play_number(), repeated);
        // No queue entry (stopped): forgotten.
        assert_eq!(tr.observe(None, false, 0, clock, None), None);
        assert!(tr.current.is_none());
    }

    #[test]
    fn short_untagged_and_radio_never_count() {
        let mut short = track(29_000);
        assert!(play_of(&short).is_none());
        short.duration_ms = 30_000;
        assert!(play_of(&short).is_some());
        let mut untagged = track(100_000);
        untagged.artist.clear();
        assert!(play_of(&untagged).is_none());
        let mut fallback = track(100_000);
        fallback.title = fallback.filename.clone();
        assert!(play_of(&fallback).is_none());
        let mut radio = track(100_000);
        radio.source_id = crate::RADIO_SOURCE.into();
        assert!(play_of(&radio).is_none());
        let mut tr = Tracker::default();
        let mut clock = 0;
        assert!(run(&mut tr, 1, &radio, &mut clock, 0, 100_000).is_empty());
        let mut va = track(100_000);
        va.album_artist = "Various Artists".into();
        assert_eq!(play_of(&va).unwrap().album_artist, "Various Artists");
        // The song's own tag, not the album's the lists use (#45).
        va.own_album_artist = Some(String::new());
        assert_eq!(play_of(&va).unwrap().album_artist, "");
    }

    #[test]
    fn unknown_clock_is_stamped_later_or_never() {
        let t = track(60_000);
        let mut tr = Tracker::default();
        let mut got = None;
        for s in 0..=31u64 {
            if let Some(c) = tr.observe(Some((1, &t)), true, s * 1000, 50_000 + s * 1000, None) {
                got = Some(c);
            }
        }
        let c = got.expect("counted");
        assert_eq!(c.started_unix, None);
        assert_eq!(c.started_mono_ms, 50_000);
        assert_eq!(c.stamp(110_000, None), None);
        assert_eq!(
            c.stamp(110_000, Some(1_800_000_000)).unwrap().timestamp,
            1_800_000_000 - 60
        );
    }

    #[test]
    fn rockbox_log_lines() {
        let s = Scrobble {
            play: Play {
                artist: "A\tB".into(),
                title: "T".into(),
                album: "Al".into(),
                album_artist: String::new(),
                duration_s: 215,
                track_number: 3,
            },
            timestamp: 1_700_000_000,
        };
        assert_eq!(log_line(&s), "A B\tAl\tT\t3\t215\tL\t1700000000\t\n");
        assert!(log_header("Y2Reborn 1.0").starts_with("#AUDIOSCROBBLER/1.1\n#TZ/UTC\n"));
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Scrobble>(&json).unwrap(), s);
    }
}
