//! Library search (Beta 5): artists, albums and songs whose words begin with
//! every typed word. Case, accents and punctuation are ignored as in A to Z
//! order (`reborn_core::sort::fold`): "beat" finds The Beatles, "sigur ros"
//! finds Sigur Rós, "beatles abbey" finds Abbey Road.
use reborn_core::{
    sort::{fold, sort_key_folded, SortAs},
    Track,
};
use std::collections::BTreeMap;

/// Earlier searches kept for narrowing and for Delete (one per keystroke).
const CACHED_SEARCHES: usize = 48;

struct Artist {
    name: String,
    folded: String,
    /// A track standing for the artist (its cover) and the album count.
    art: usize,
    albums: usize,
    /// The artist's tracks, to leave out an artist whose songs are all offline.
    tracks: Vec<u32>,
}

/// Folded names of the library's artists, albums and songs, each list in A
/// to Z order, rebuilt only when the library changes. A keystroke searches
/// only what the search before it found when the new text extends it, and
/// Delete finds the earlier search again.
#[derive(Default)]
pub struct Index {
    stamp: Option<(usize, usize)>,
    artists: Vec<Artist>,
    /// (track standing for the album, album and album artist folded).
    albums: Vec<(usize, String)>,
    /// (track, title, artist and album folded).
    songs: Vec<(usize, String)>,
    cache: Vec<(String, Matches)>,
}

/// Positions in the index's lists.
#[derive(Clone, Default)]
struct Matches {
    artists: Vec<u32>,
    albums: Vec<u32>,
    songs: Vec<u32>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Results {
    /// Artist name, a track standing for it (its cover), its album count.
    pub artists: Vec<(String, usize, usize)>,
    /// A track of each album (album and album artist are taken from it).
    pub albums: Vec<usize>,
    pub songs: Vec<usize>,
}

#[cfg(test)]
impl Results {
    /// "2 artists · 1 album · 14 songs", or "No matches" (the search page
    /// counts audiobooks too: `catalog`).
    pub fn summary(&self) -> String {
        let part = |n: usize, one: &str, many: &str| {
            (n > 0).then(|| crate::components::count(n, one, many))
        };
        let parts: Vec<String> = [
            part(self.artists.len(), "artist", "artists"),
            part(self.albums.len(), "album", "albums"),
            part(self.songs.len(), "song", "songs"),
        ]
        .into_iter()
        .flatten()
        .collect();
        if parts.is_empty() {
            "No matches".into()
        } else {
            parts.join(" · ")
        }
    }
}

/// Every query word begins a word of `name` (words are single-space
/// separated, as `fold` writes them). `inner` holds " word" per word.
fn matches(words: &[&str], inner: &[String], name: &str) -> bool {
    words
        .iter()
        .zip(inner)
        .all(|(w, spaced)| name.starts_with(w) || name.contains(spaced.as_str()))
}

/// Every word of the folded `query` begins a word of the folded `name`
/// (what the index finds, for names outside it: audiobooks).
pub fn words_match(query: &str, name: &str) -> bool {
    let words: Vec<&str> = query.split(' ').filter(|w| !w.is_empty()).collect();
    let inner: Vec<String> = words.iter().map(|w| format!(" {w}")).collect();
    !words.is_empty() && matches(&words, &inner, name)
}

impl Index {
    fn ensure(&mut self, tracks: &[Track]) {
        let stamp = crate::library_stamp(tracks);
        if self.stamp == Some(stamp) {
            return;
        }
        *self = Self {
            stamp: Some(stamp),
            ..Default::default()
        };
        let folded: Vec<[String; 3]> = tracks
            .iter()
            .map(|t| [fold(crate::track_title(t)), fold(&t.artist), fold(&t.album)])
            .collect();
        // Artists: name -> (cover track, albums).
        type Entry<'a> = (usize, std::collections::BTreeSet<&'a str>, Vec<u32>);
        let mut artists: BTreeMap<&str, Entry> = BTreeMap::new();
        let mut albums: BTreeMap<(&str, &str), usize> = BTreeMap::new();
        for (i, t) in tracks.iter().enumerate() {
            // One artist per name of a joined or "feat." tag (#38).
            for artist in t.artists().into_iter().filter(|a| !a.is_empty()) {
                let entry = artists
                    .entry(artist)
                    .or_insert((i, Default::default(), vec![]));
                if !tracks[entry.0].artwork && t.artwork {
                    entry.0 = i;
                }
                entry.1.insert(t.album.as_str());
                entry.2.push(i as u32);
            }
            if !t.album.is_empty() {
                albums
                    .entry((t.album.as_str(), t.album_artist.as_str()))
                    .or_insert(i);
            }
        }
        self.artists = artists
            .into_iter()
            .map(|(name, (art, albums, tracks))| Artist {
                name: name.to_owned(),
                folded: fold(name),
                art,
                albums: albums.len(),
                tracks,
            })
            .collect();
        self.artists
            .sort_by_cached_key(|a| sort_key_folded(a.folded.clone(), SortAs::Artist));
        let mut albums: Vec<(usize, String)> = albums
            .into_values()
            .map(|i| {
                let t = &tracks[i];
                let owner = if t.album_artist.is_empty() {
                    folded[i][1].clone()
                } else {
                    fold(&t.album_artist)
                };
                (i, format!("{} {owner}", folded[i][2]))
            })
            .collect();
        albums.sort_by_cached_key(|(i, _)| sort_key_folded(folded[*i][2].clone(), SortAs::Title));
        self.albums = albums;
        let mut songs: Vec<(usize, String)> = folded
            .iter()
            .enumerate()
            .map(|(i, [title, artist, album])| (i, format!("{title} {artist} {album}")))
            .collect();
        songs.sort_by_cached_key(|(i, _)| sort_key_folded(folded[*i][0].clone(), SortAs::Title));
        self.songs = songs;
    }

    pub fn search(&mut self, tracks: &[Track], query: &str) -> Results {
        self.ensure(tracks);
        let folded = fold(query);
        let found = self.matches(&folded);
        // Offline tracks (an SD card taken out) and audiobook parts are left
        // out at reading time.
        let online = |i: usize| tracks.get(i).is_some_and(|t| t.is_music());
        Results {
            artists: found
                .artists
                .iter()
                .map(|&a| &self.artists[a as usize])
                .filter(|a| online(a.art) || a.tracks.iter().any(|&i| online(i as usize)))
                .map(|a| (a.name.clone(), a.art, a.albums))
                .collect(),
            albums: found
                .albums
                .iter()
                .map(|&a| self.albums[a as usize].0)
                .filter(|&i| online(i))
                .collect(),
            songs: found
                .songs
                .iter()
                .map(|&s| self.songs[s as usize].0)
                .filter(|&i| online(i))
                .collect(),
        }
    }

    fn matches(&mut self, folded: &str) -> Matches {
        let words: Vec<&str> = folded.split(' ').filter(|w| !w.is_empty()).collect();
        if words.is_empty() {
            return Matches::default();
        }
        if let Some((_, found)) = self.cache.iter().find(|(q, _)| q == folded) {
            return found.clone();
        }
        let inner: Vec<String> = words.iter().map(|w| format!(" {w}")).collect();
        // A longer text finds a subset of what its beginning found.
        let base = self
            .cache
            .iter()
            .filter(|(q, _)| folded.starts_with(q.as_str()))
            .max_by_key(|(q, _)| q.len())
            .map(|(_, found)| found);
        let pick = |names: &mut dyn Iterator<Item = (u32, &str)>| -> Vec<u32> {
            names
                .filter(|(_, name)| matches(&words, &inner, name))
                .map(|(i, _)| i)
                .collect()
        };
        let found = match base {
            Some(base) => Matches {
                artists: pick(
                    &mut base
                        .artists
                        .iter()
                        .map(|&i| (i, self.artists[i as usize].folded.as_str())),
                ),
                albums: pick(
                    &mut base
                        .albums
                        .iter()
                        .map(|&i| (i, self.albums[i as usize].1.as_str())),
                ),
                songs: pick(
                    &mut base
                        .songs
                        .iter()
                        .map(|&i| (i, self.songs[i as usize].1.as_str())),
                ),
            },
            None => Matches {
                artists: pick(
                    &mut self
                        .artists
                        .iter()
                        .enumerate()
                        .map(|(i, a)| (i as u32, a.folded.as_str())),
                ),
                albums: pick(
                    &mut self
                        .albums
                        .iter()
                        .enumerate()
                        .map(|(i, a)| (i as u32, a.1.as_str())),
                ),
                songs: pick(
                    &mut self
                        .songs
                        .iter()
                        .enumerate()
                        .map(|(i, s)| (i as u32, s.1.as_str())),
                ),
            },
        };
        // Only beginnings of this text can serve again (typing on, Delete):
        // results of other searches go, so the cache stays small.
        self.cache.retain(|(q, _)| folded.starts_with(q.as_str()));
        if self.cache.len() >= CACHED_SEARCHES {
            self.cache.remove(0);
        }
        self.cache.push((folded.to_owned(), found.clone()));
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: &str, album: &str) -> Track {
        Track {
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            album_artist: artist.into(),
            online: true,
            ..Default::default()
        }
    }

    #[test]
    fn word_beginnings_ignore_case_accents_and_articles() {
        let tracks = vec![
            track("Come Together", "The Beatles", "Abbey Road"),
            track("Something", "The Beatles", "Abbey Road"),
            track("Hoppípolla", "Sigur Rós", "Takk..."),
            track("Beat It", "Michael Jackson", "Thriller"),
        ];
        let mut index = Index::default();
        let r = index.search(&tracks, "beat");
        assert_eq!(r.artists, vec![("The Beatles".to_owned(), 0, 1)]);
        assert_eq!(
            r.songs,
            vec![3, 0, 1],
            "Beat It, then the Beatles songs by title"
        );
        // An album is found by its artist's name too.
        assert_eq!(r.albums, vec![0]);
        assert_eq!(r.summary(), "1 artist · 1 album · 3 songs");
        let r = index.search(&tracks, "SIGUR ros");
        assert_eq!(r.artists.len(), 1);
        assert_eq!(r.songs, vec![2]);
        let r = index.search(&tracks, "beatles abbey");
        assert_eq!(r.albums, vec![0]);
        assert_eq!(r.songs.len(), 2);
        // Inside a word is not a beginning.
        assert_eq!(index.search(&tracks, "eatles").summary(), "No matches");
        assert_eq!(index.search(&tracks, "   "), Results::default());
    }

    #[test]
    fn offline_tracks_are_left_out() {
        let mut tracks = vec![track("Yesterday", "The Beatles", "Help!")];
        tracks[0].online = false;
        assert_eq!(
            Index::default().search(&tracks, "yes").summary(),
            "No matches"
        );
    }
}

#[cfg(test)]
pub(crate) mod speed {
    use super::*;
    use std::time::Instant;

    /// A large library from a fixed word list: 20 000 songs, ~2 000
    /// albums, ~400 artists, names of one to four words.
    pub fn large_library() -> Vec<Track> {
        const WORDS: &[&str] = &[
            "the",
            "night",
            "light",
            "heart",
            "love",
            "blue",
            "river",
            "dream",
            "fire",
            "stone",
            "moon",
            "sun",
            "rain",
            "wild",
            "golden",
            "silver",
            "electric",
            "summer",
            "winter",
            "ghost",
            "city",
            "road",
            "home",
            "star",
            "black",
            "white",
            "red",
            "ocean",
            "sky",
            "song",
            "dance",
            "time",
            "world",
            "life",
            "sweet",
            "dark",
            "young",
            "lost",
            "free",
            "beautiful",
            "broken",
            "Beatles",
            "Björk",
            "Sigur",
            "Rós",
            "Café",
            "Élodie",
            "north",
            "south",
            "garden",
            "machine",
            "paper",
            "glass",
            "velvet",
            "thunder",
            "echo",
            "shadow",
        ];
        let mut seed = 0x2545F4914F6CDD1Du64;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let name = |words: usize, next: &mut dyn FnMut(usize) -> usize| {
            (0..words)
                .map(|_| WORDS[next(WORDS.len())])
                .collect::<Vec<_>>()
                .join(" ")
        };
        let artists: Vec<String> = (0..400).map(|_| name(1 + next(3), &mut next)).collect();
        let mut tracks = Vec::with_capacity(20_000);
        for album in 0..2_000 {
            let artist = artists[next(artists.len())].clone();
            let title = name(1 + next(3), &mut next);
            for _ in 0..10 {
                tracks.push(Track {
                    title: name(1 + next(4), &mut next),
                    artist: artist.clone(),
                    album: format!("{title} {album}"),
                    album_artist: artist.clone(),
                    online: true,
                    ..Default::default()
                });
            }
        }
        tracks
    }

    /// Timings for typing "the beat" letter by letter, then deleting it
    /// (`cargo test -p reborn-ui --release -- --ignored --nocapture speed`).
    #[test]
    #[ignore]
    fn speed_on_a_large_library() {
        let tracks = large_library();
        let mut index = Index::default();
        let started = Instant::now();
        index.search(&tracks, "x");
        eprintln!("index build + first search: {:?}", started.elapsed());
        let typed = "the light s";
        let mut worst = std::time::Duration::ZERO;
        let mut total = std::time::Duration::ZERO;
        let steps: Vec<&str> = (1..=typed.len())
            .map(|n| &typed[..n])
            .chain((1..typed.len()).rev().map(|n| &typed[..n]))
            .collect();
        for query in &steps {
            let started = Instant::now();
            let r = index.search(&tracks, query);
            let took = started.elapsed();
            worst = worst.max(took);
            total += took;
            eprintln!("{query:>12}: {took:>10?}  {}", r.summary());
        }
        eprintln!(
            "per keystroke: mean {:?}, worst {worst:?}",
            total / steps.len() as u32
        );
    }
}
