//! One album, one album artist (#45, #46): songs of an album where some lack
//! the Album Artist tag, or spell it with other capitals or spaces, are
//! listed as one album. Applied to the songs when the library loads; the
//! library file keeps every song's own tags.
use crate::Track;
use std::{collections::HashMap, path::Path};

/// Within one folder of one storage, the songs of one album title share its
/// album artist: an empty tag takes the one the others carry, and spellings
/// that differ only in capitals or spaces become the most common one. Only
/// when the album's songs name exactly one album artist; a compilation
/// without the tag, or an album naming two, stays as it is (falling back to
/// the song's artist would split a compilation into one album per song).
/// A song whose album artist changed keeps its own in `own_album_artist`.
pub fn unify_album_artists(tracks: &mut [Track]) {
    let mut albums: HashMap<(&str, Option<&Path>, &str), Vec<usize>> = HashMap::new();
    for (i, t) in tracks.iter().enumerate() {
        // Songs without an album title are no album to keep together.
        if t.album.trim().is_empty() || t.is_live() {
            continue;
        }
        albums
            .entry((t.source_id.as_str(), t.path.parent(), t.album.as_str()))
            .or_default()
            .push(i);
    }
    let albums: Vec<Vec<usize>> = albums.into_values().filter(|s| s.len() > 1).collect();
    for songs in albums {
        let Some(name) = album_artist(tracks, &songs) else {
            continue;
        };
        for i in songs {
            let t = &mut tracks[i];
            if t.album_artist != name {
                let own = std::mem::replace(&mut t.album_artist, name.clone());
                t.own_album_artist.get_or_insert(own);
            }
        }
    }
}

/// The one album artist these songs name, in its most common spelling (the
/// first seen on a tie), or None when they name none or more than one.
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
            Some(seen) if *seen != key => return None,
            Some(_) => {}
            None => folded = Some(key),
        }
        match spellings.iter_mut().find(|(s, _)| *s == name) {
            Some((_, n)) => *n += 1,
            None => spellings.push((name, 1)),
        }
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
    fn a_compilation_without_album_artist_stays_as_it_is() {
        let mut tracks = vec![
            song("Now 50", "A", "Artist One", "Now 50", ""),
            song("Now 50", "B", "Artist Two", "Now 50", ""),
            song("Now 50", "C", "Artist Three", "Now 50", ""),
        ];
        unify_album_artists(&mut tracks);
        assert!(tracks
            .iter()
            .all(|t| t.album_artist.is_empty() && t.own_album_artist.is_none()));
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
    fn two_album_artists_other_folders_and_loose_songs_stay_apart() {
        let mut tracks = vec![
            // Two different album artists: left alone.
            song("Split", "1", "X", "Split EP", "X"),
            song("Split", "2", "Y", "Split EP", "Y"),
            song("Split", "3", "Z", "Split EP", ""),
            // The same title in another folder is another album.
            song("Greatest/A", "1", "A", "Greatest Hits", "A"),
            song("Greatest/B", "1", "B", "Greatest Hits", ""),
            song("Greatest/B", "2", "B", "Greatest Hits", ""),
            // Songs without an album title.
            song("Loose", "1", "L", "", "L"),
            song("Loose", "2", "M", "", ""),
        ];
        let before: Vec<String> = tracks.iter().map(|t| t.album_artist.clone()).collect();
        unify_album_artists(&mut tracks);
        let after: Vec<String> = tracks.iter().map(|t| t.album_artist.clone()).collect();
        assert_eq!(before, after);
        assert!(tracks.iter().all(|t| t.own_album_artist.is_none()));
    }
}
