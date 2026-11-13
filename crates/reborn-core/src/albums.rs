//! One album, one album artist (#45, #46): songs of an album where some lack
//! the Album Artist tag, or spell it with other capitals or spaces, are
//! listed as one album; an album whose songs name different album artists, or
//! none while different artists sing, is one "Various Artists" album; two
//! albums of the same title without the tag stay apart when their artists
//! differ; a song without an album title files under its own artist (a
//! Reddit user's 720 albums, 2026-11-13). Applied to the songs when the
//! library loads; the library file keeps every song's own tags.
use crate::Track;
use std::{collections::HashMap, path::Path};

/// The album artist of a compilation (the name players and taggers use).
pub const VARIOUS_ARTISTS: &str = "Various Artists";

/// Within one folder of one storage, the songs of one album title share one
/// album artist (`album_artist`). A song whose album artist changed keeps its
/// own in `own_album_artist` (Last.fm sends that one).
pub fn unify_album_artists(tracks: &mut [Track]) {
    let mut albums: HashMap<(&str, Option<&Path>, &str), Vec<usize>> = HashMap::new();
    let mut untitled = vec![];
    for (i, t) in tracks.iter().enumerate() {
        if t.is_radio() {
            continue;
        }
        if t.album.trim().is_empty() {
            untitled.push(i);
            continue;
        }
        albums
            .entry((t.source_id.as_str(), t.path.parent(), t.album.as_str()))
            .or_default()
            .push(i);
    }
    let mut changes: Vec<(Vec<usize>, String)> = albums
        .into_values()
        .filter_map(|songs| album_artist(tracks, &songs).map(|name| (songs, name)))
        .collect();
    // Without an album title: no album to keep together, but not one mix of everyone's songs
    // either: each under its own artist.
    for i in untitled {
        if tracks[i].album_artist.trim().is_empty() {
            if let Some(artist) = first_artist(&tracks[i]) {
                changes.push((vec![i], artist));
            }
        }
    }
    for (songs, name) in changes {
        for i in songs {
            let t = &mut tracks[i];
            if t.album_artist != name {
                let own = std::mem::replace(&mut t.album_artist, name.clone());
                t.own_album_artist.get_or_insert(own);
            }
        }
    }
}

/// The song's first artist, if it names one.
fn first_artist(t: &Track) -> Option<String> {
    t.artists()
        .first()
        .map(|a| a.trim().to_owned())
        .filter(|a| !a.is_empty())
}

/// The one album artist of these songs: the one they name, in its most common
/// spelling (the first seen on a tie); Various Artists when they name more
/// than one; when they name none, the one artist who sings them all, else
/// Various Artists. None when nothing is known.
fn album_artist(tracks: &[Track], songs: &[usize]) -> Option<String> {
    let mut folded: Option<String> = None;
    let mut spellings: Vec<(&str, usize)> = Vec::new();
    for &i in songs {
        let name = tracks[i].album_artist.trim();
        if name.is_empty() {
            continue;
        }
        let key = fold(name);
        match &folded {
            Some(seen) if *seen != key => return Some(VARIOUS_ARTISTS.into()),
            Some(_) => {}
            None => folded = Some(key),
        }
        match spellings.iter_mut().find(|(s, _)| *s == name) {
            Some((_, n)) => *n += 1,
            None => spellings.push((name, 1)),
        }
    }
    if spellings.is_empty() {
        let artists: Vec<String> = songs
            .iter()
            .filter_map(|&i| first_artist(&tracks[i]))
            .collect();
        let first = artists.first()?;
        return Some(if artists.iter().all(|a| fold(a) == fold(first)) {
            first.clone()
        } else {
            VARIOUS_ARTISTS.into()
        });
    }
    let mut best: Option<(&str, usize)> = None;
    for (name, n) in spellings {
        if best.is_none_or(|(_, most)| n > most) {
            best = Some((name, n));
        }
    }
    best.map(|(name, _)| name.to_owned())
}

/// Capitals and runs of spaces aside.
fn fold(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(folder: &str, title: &str, artist: &str, album: &str, album_artist: &str) -> Track {
        Track {
            source_id: "sd".into(),
            path: format!("/sd/{folder}/{title}.flac").into(),
            filename: format!("{title}.flac"),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            album_artist: album_artist.into(),
            online: true,
            ..Default::default()
        }
    }

    #[test]
    fn an_album_with_one_tagged_song_becomes_one_album() {
        // Y2Reborn #45: only "Too Long" carries Album Artist on Discovery.
        let titles = ["One More Time", "Aerodynamic", "Digital Love", "Too Long"];
        let mut tracks: Vec<Track> = titles
            .iter()
            .map(|t| {
                let tag = if *t == "Too Long" { "Daft Punk" } else { "" };
                song("Daft Punk/Discovery", t, "Daft Punk", "Discovery", tag)
            })
            .collect();
        unify_album_artists(&mut tracks);
        assert!(tracks.iter().all(|t| t.album_artist == "Daft Punk"));
        // Each song keeps its own tag for Last.fm.
        assert_eq!(tracks[0].own_album_artist.as_deref(), Some(""));
        assert_eq!(tracks[0].tagged_album_artist(), "");
        assert_eq!(tracks[3].own_album_artist, None);
        assert_eq!(tracks[3].tagged_album_artist(), "Daft Punk");
        // Once more (the next library publish) changes nothing.
        unify_album_artists(&mut tracks);
        assert_eq!(tracks[0].own_album_artist.as_deref(), Some(""));
    }

    #[test]
    fn a_compilation_without_album_artist_is_one_various_artists_album() {
        let mut tracks = vec![
            song("Now 50", "A", "Artist One", "Now 50", ""),
            song("Now 50", "B", "Artist Two", "Now 50", ""),
            song("Now 50", "C", "Artist Three", "Now 50", ""),
        ];
        unify_album_artists(&mut tracks);
        assert!(tracks.iter().all(|t| t.album_artist == VARIOUS_ARTISTS));
        assert_eq!(
            tracks[0].tagged_album_artist(),
            "",
            "Last.fm gets the song's own tag"
        );
    }

    #[test]
    fn songs_naming_different_album_artists_become_one_album() {
        // Downloaded songs often carry each song's artist as Album Artist.
        let mut tracks = vec![
            song("Mix", "A", "Artist One", "Summer Mix", "Artist One"),
            song("Mix", "B", "Artist Two", "Summer Mix", "Artist Two"),
            song("Mix", "C", "Artist Two", "Summer Mix", ""),
        ];
        unify_album_artists(&mut tracks);
        assert!(tracks.iter().all(|t| t.album_artist == VARIOUS_ARTISTS));
    }

    #[test]
    fn same_titled_albums_without_the_tag_stay_apart_and_untitled_songs_file_by_artist() {
        let mut tracks = vec![
            song("Queen/Hits", "A", "Queen", "Greatest Hits", ""),
            song("Queen/Hits", "B", "Queen", "Greatest Hits", ""),
            song("ABBA/Hits", "C", "ABBA", "Greatest Hits", ""),
            song("Loose", "D", "Artist One", "", ""),
            song("Loose", "E", "Artist Two feat. X", "", ""),
        ];
        unify_album_artists(&mut tracks);
        let artists: Vec<&str> = tracks.iter().map(|t| t.album_artist.as_str()).collect();
        assert_eq!(artists[..3], ["Queen", "Queen", "ABBA"]);
        assert_eq!(artists[3], "Artist One");
        assert_eq!(artists[4], tracks[4].artists()[0]);
        // Once more (the next library publish) changes nothing.
        let key = |t: &[Track]| -> Vec<(String, Option<String>)> {
            t.iter()
                .map(|t| (t.album_artist.clone(), t.own_album_artist.clone()))
                .collect()
        };
        let before = key(&tracks);
        unify_album_artists(&mut tracks);
        assert_eq!(key(&tracks), before);
    }

    #[test]
    fn capitals_and_spaces_join_the_most_common_spelling() {
        let mut tracks = vec![
            song("AC", "1", "AC/DC", "Back in Black", "AC/DC"),
            song("AC", "2", "AC/DC", "Back in Black", "ac/dc "),
            song("AC", "3", "AC/DC", "Back in Black", "AC/DC"),
            song("AC", "4", "AC/DC", "Back in Black", ""),
        ];
        unify_album_artists(&mut tracks);
        assert!(tracks.iter().all(|t| t.album_artist == "AC/DC"));
        assert_eq!(tracks[1].tagged_album_artist(), "ac/dc ");
    }

    #[test]
    fn two_album_artists_join_and_other_folders_and_loose_songs_stay_apart() {
        let mut tracks = vec![
            // Two different album artists on one record: one Various Artists album (it was two
            // albums before 9.3.30, and so a library of downloaded songs had hundreds).
            song("Split", "1", "X", "Split EP", "X"),
            song("Split", "2", "Y", "Split EP", "Y"),
            song("Split", "3", "Z", "Split EP", ""),
            // The same title in another folder is another album.
            song("Greatest/A", "1", "A", "Greatest Hits", "A"),
            song("Greatest/B", "1", "B", "Greatest Hits", ""),
            song("Greatest/B", "2", "B", "Greatest Hits", ""),
            // Songs without an album title: each under its own artist.
            song("Loose", "1", "L", "", "L"),
            song("Loose", "2", "M", "", ""),
        ];
        unify_album_artists(&mut tracks);
        let after: Vec<&str> = tracks.iter().map(|t| t.album_artist.as_str()).collect();
        let various = VARIOUS_ARTISTS;
        assert_eq!(after, [various, various, various, "A", "B", "B", "L", "M"]);
        assert_eq!(
            tracks[3].own_album_artist, None,
            "a tag that was right stays"
        );
        assert_eq!(tracks[4].own_album_artist.as_deref(), Some(""));
    }
}
