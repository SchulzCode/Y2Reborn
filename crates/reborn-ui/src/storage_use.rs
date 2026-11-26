//! What fills a volume: the library's files by kind (music, audiobooks, films), summed per source
//! from the tracks already loaded, so Storage needs no scan of its own. What the volume holds
//! beyond the library (playlists, saved artwork, other files) is "Other" (`other_bytes`).
use crate::views_books::length;
use reborn_core::{books::Book, platform::VolumeSpace, MediaSource, Source, Track};
use std::collections::HashMap;

/// Files of one kind: how many (books count as books, not parts), their size and play time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Share {
    pub count: usize,
    pub bytes: u64,
    pub ms: u64,
}

/// The library's three kinds of file on one volume.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Kinds {
    pub music: Share,
    pub books: Share,
    pub videos: Share,
}

impl Kinds {
    pub fn bytes(&self) -> u64 {
        self.music.bytes + self.books.bytes + self.videos.bytes
    }
    fn add(&mut self, other: &Kinds) {
        for (mine, theirs) in [
            (&mut self.music, other.music),
            (&mut self.books, other.books),
            (&mut self.videos, other.videos),
        ] {
            mine.count += theirs.count;
            mine.bytes += theirs.bytes;
            mine.ms += theirs.ms;
        }
    }
}

/// Every source's files by kind, keyed by source id. Offline tracks are included here and left
/// out by `volume` (a removed card's source is offline), so the sums stay valid until the library
/// changes.
pub(crate) fn by_source(tracks: &[Track], books: &[Book]) -> HashMap<String, Kinds> {
    let mut map: HashMap<String, Kinds> = HashMap::new();
    for t in tracks {
        let kinds = map.entry(t.source_id.clone()).or_default();
        // The order `Track::is_music` uses: a book part is a book even when it holds a film's codecs.
        let (share, counted) = if t.book {
            (&mut kinds.books, false)
        } else if t.is_video() {
            (&mut kinds.videos, true)
        } else {
            (&mut kinds.music, true)
        };
        share.count += usize::from(counted);
        share.bytes += t.size;
        share.ms += t.duration_ms;
    }
    for book in books {
        if let Some(first) = book.parts.first().and_then(|&i| tracks.get(i)) {
            map.entry(first.source_id.clone()).or_default().books.count += 1;
        }
    }
    map
}

/// The online sources of one volume (the internal storage, or the SD card), added up.
pub(crate) fn volume(map: &HashMap<String, Kinds>, sources: &[Source], internal: bool) -> Kinds {
    let mut sum = Kinds::default();
    for source in sources {
        if source.online && matches!(source.kind, MediaSource::Internal) == internal {
            if let Some(kinds) = map.get(&source.id) {
                sum.add(kinds);
            }
        }
    }
    sum
}

/// What the volume holds beyond the library: used bytes minus the library's. None when the volume
/// does not say how much is used.
pub(crate) fn other_bytes(space: &VolumeSpace, library: u64) -> Option<u64> {
    let used = space.total_bytes?.saturating_sub(space.free_bytes?);
    Some(used.saturating_sub(library))
}

/// A play time for a legend: "9 h 41 min", from a day on "14 d 2 h".
pub(crate) fn span(ms: u64) -> String {
    let hours = ms / 3_600_000;
    match (hours / 24, hours % 24) {
        (0, _) => length(ms),
        (days, 0) => format!("{days} d"),
        (days, hours) => format!("{days} d {hours} h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(source: &str, size: u64, ms: u64, codec: &str, book: bool) -> Track {
        Track {
            source_id: source.into(),
            size,
            duration_ms: ms,
            codec: codec.into(),
            book,
            online: true,
            ..Track::default()
        }
    }
    fn source(id: &str, kind: MediaSource, online: bool) -> Source {
        Source {
            id: id.into(),
            kind,
            root: format!("/media/{id}").into(),
            online,
            mount: String::new(),
            mount_id: None,
        }
    }
    fn book(parts: &[usize]) -> Book {
        Book {
            key: String::new(),
            title: String::new(),
            author: String::new(),
            parts: parts.to_vec(),
            duration_ms: 0,
            art: None,
            series: None,
        }
    }

    #[test]
    fn files_are_sorted_into_music_books_and_films_per_source() {
        let tracks = [
            track("sd", 5_000, 200_000, "mp3", false),
            track("sd", 7_000, 300_000, "flac", false),
            track("sd", 40_000, 3_600_000, "mp3", true),
            track("sd", 30_000, 3_600_000, "mp3", true),
            track("sd", 900_000, 5_000_000, "h264/aac", false),
            track("int", 1_000, 100_000, "aac", false),
        ];
        let map = by_source(&tracks, &[book(&[2, 3])]);
        let sd = map["sd"];
        assert_eq!(
            sd.music,
            Share {
                count: 2,
                bytes: 12_000,
                ms: 500_000
            }
        );
        // Two parts, one book.
        assert_eq!(
            sd.books,
            Share {
                count: 1,
                bytes: 70_000,
                ms: 7_200_000
            }
        );
        assert_eq!(sd.videos.count, 1);
        assert_eq!(sd.videos.bytes, 900_000);
        assert_eq!(sd.bytes(), 982_000);
        assert_eq!(map["int"].music.bytes, 1_000);
        assert_eq!(map["int"].books, Share::default());
    }

    #[test]
    fn a_volume_adds_its_online_sources_and_a_removed_card_counts_for_nothing() {
        let tracks = [
            track("old", 100, 1, "mp3", false),
            track("new", 200, 1, "mp3", false),
            track("int", 400, 1, "mp3", false),
        ];
        let map = by_source(&tracks, &[]);
        let sources = [
            source("old", MediaSource::SdCard("a".into()), false),
            source("new", MediaSource::SdCard("b".into()), true),
            source("int", MediaSource::Internal, true),
        ];
        assert_eq!(volume(&map, &sources, false).music.bytes, 200);
        assert_eq!(volume(&map, &sources, true).music.bytes, 400);
        assert_eq!(volume(&map, &[], false), Kinds::default());
    }

    #[test]
    fn other_is_what_is_used_beyond_the_library_and_never_negative() {
        let space = |free, total| VolumeSpace {
            free_bytes: free,
            total_bytes: total,
            ..VolumeSpace::default()
        };
        assert_eq!(
            other_bytes(&space(Some(40), Some(100)), 55),
            Some(5),
            "100 total, 40 free, 55 of it music"
        );
        assert_eq!(other_bytes(&space(Some(40), Some(100)), 90), Some(0));
        assert_eq!(other_bytes(&space(None, Some(100)), 10), None);
        assert_eq!(other_bytes(&space(Some(40), None), 10), None);
        assert_eq!(other_bytes(&space(Some(120), Some(100)), 0), Some(0));
    }

    #[test]
    fn play_time_reads_in_days_from_a_day_on() {
        assert_eq!(span(41 * 60_000), "41 min");
        assert_eq!(span(9 * 3_600_000 + 41 * 60_000), "9 h 41 min");
        assert_eq!(span(23 * 3_600_000 + 59 * 60_000), "23 h 59 min");
        assert_eq!(span(24 * 3_600_000), "1 d");
        assert_eq!(span((14 * 24 + 2) * 3_600_000 + 5 * 60_000), "14 d 2 h");
    }
}
