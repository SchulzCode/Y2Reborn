//! Which library song each playlist entry names, without touching the disk.
//!
//! A path relative to the playlist's folder (what Reborn writes) or a path on
//! the player is looked up directly. A path from a computer
//! (`C:\Users\…\Music\Artist\Album\Song.flac`, `/Users/…`, `file:///…`) is
//! matched by its ending: the song whose path shares the most trailing folders
//! with it (case-insensitive, as exFAT and Windows are). Y2Player asked the
//! file system for every entry; here each entry is one or two hash lookups.

use reborn_core::{playlist::Playlist, Track};
use std::collections::{hash_map::Entry, HashMap};
use std::path::{Component, Path, PathBuf};

/// A playlist's file stamp and, per entry, its library track.
type Resolved = ((u64, i64), Vec<Option<usize>>);

#[derive(Default)]
pub struct Index {
    stamp: Option<(usize, usize)>,
    by_path: HashMap<String, u32>,
    /// Lower-case file name → tracks with that name.
    by_name: HashMap<String, Vec<u32>>,
    /// Resolved playlists: (path, file stamp) → entry → track.
    resolved: HashMap<PathBuf, Resolved>,
}

/// `a/b/../c` → `a/c`, without asking the file system.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // Bytes, not `&text[..]`: a `%` before `’` is no character boundary.
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
        // Songs on the player first: when the library holds a path twice
        // (a card recorded under an earlier source id as well), the copy that
        // can play is the one an entry finds, and the stale copy is no second
        // candidate for a name.
        for online in [true, false] {
            for (i, t) in tracks.iter().enumerate() {
                if t.is_live() || t.online != online {
                    continue;
                }
                let Entry::Vacant(slot) =
                    self.by_path.entry(t.path.to_string_lossy().to_lowercase())
                else {
                    continue;
                };
                slot.insert(i as u32);
                if let Some(name) = t.path.file_name() {
                    self.by_name
                        .entry(name.to_string_lossy().to_lowercase())
                        .or_default()
                        .push(i as u32);
                }
            }
        }
    }

    /// The library track an entry names, if it is in the library.
    pub fn resolve(&mut self, tracks: &[Track], dir: &Path, location: &str) -> Option<usize> {
        self.ensure(tracks);
        self.lookup(tracks, dir, location)
    }

    fn lookup(&self, tracks: &[Track], dir: &Path, location: &str) -> Option<usize> {
        let mut text = location.trim().to_owned();
        if text.len() > 7
            && text
                .get(..7)
                .is_some_and(|p| p.eq_ignore_ascii_case("file://"))
        {
            text = percent_decode(&text[7..]);
            text = text.strip_prefix("localhost").unwrap_or(&text).to_owned();
        } else if text.contains("://") {
            return None; // a web address, not a song on the player
        }
        let text = text.replace('\\', "/");
        let windows = text.len() > 2 && text.as_bytes()[1] == b':'
            || text.len() > 3 && text.starts_with('/') && text.as_bytes()[2] == b':';
        if !windows {
            let candidate = if text.starts_with('/') {
                PathBuf::from(&text)
            } else {
                lexical(&dir.join(&text))
            };
            let key = candidate.to_string_lossy().to_lowercase();
            if let Some(&i) = self.by_path.get(&key) {
                return Some(i as usize);
            }
        }
        // By the path's ending: the most trailing folders in common wins; a
        // name alone only when it is unique.
        let parts: Vec<String> = text
            .split('/')
            .filter(|p| !p.is_empty() && !p.ends_with(':'))
            .map(str::to_lowercase)
            .collect();
        let name = parts.last()?;
        let candidates = self.by_name.get(name)?;
        // Only the few songs with this file name are compared, so their
        // paths are lower-cased here rather than kept twice for every song.
        let score = |i: u32| -> usize {
            tracks[i as usize]
                .path
                .to_string_lossy()
                .to_lowercase()
                .split('/')
                .rev()
                .zip(parts.iter().rev())
                .take_while(|(a, b)| a == b)
                .count()
        };
        let mut best: Option<(usize, u32)> = None;
        let mut tie = false;
        for &i in candidates {
            let s = score(i);
            match best {
                Some((b, _)) if s < b => {}
                Some((b, _)) if s == b => tie = true,
                _ => {
                    best = Some((s, i));
                    tie = false;
                }
            }
        }
        let (s, i) = best?;
        (s >= 2 || (s == 1 && !tie && candidates.len() == 1)).then_some(i as usize)
    }

    /// Every entry of a playlist resolved, cached until the file or the
    /// library changes.
    pub fn resolve_all(&mut self, tracks: &[Track], playlist: &Playlist) -> &[Option<usize>] {
        self.ensure(tracks);
        let fresh = self
            .resolved
            .get(&playlist.path)
            .is_some_and(|(stamp, list)| {
                *stamp == playlist.stamp && list.len() == playlist.entries.len()
            });
        if !fresh {
            let dir = playlist.path.parent().unwrap_or(Path::new("/")).to_owned();
            let list = playlist
                .entries
                .iter()
                .map(|e| self.lookup(tracks, &dir, &e.location))
                .collect();
            self.resolved
                .insert(playlist.path.clone(), (playlist.stamp, list));
        }
        &self.resolved[&playlist.path].1
    }

    pub fn forget(&mut self, path: &Path) {
        self.resolved.remove(path);
    }
}

/// How a playlist file names a song: relative to the playlist's folder when
/// both are on the same storage (portable to a computer), else the full path.
pub fn location_for(track: &Path, playlist: &Path, roots: &[PathBuf]) -> String {
    let root_of = |p: &Path| roots.iter().find(|r| p.starts_with(r)).cloned();
    let dir = playlist.parent().unwrap_or(Path::new("/"));
    match (root_of(track), root_of(dir)) {
        (Some(a), Some(b)) if a == b => {
            let from: Vec<_> = dir.components().collect();
            let to: Vec<_> = track.components().collect();
            let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
            let mut parts: Vec<String> = vec!["..".into(); from.len() - common];
            parts.extend(
                to[common..]
                    .iter()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned()),
            );
            parts.join("/")
        }
        _ => track.to_string_lossy().into_owned(),
    }
}

/// The `#EXTINF` line Reborn writes for a song: seconds, "Artist - Title".
pub fn info_for(track: &Track) -> String {
    let title = crate::track_title(track);
    if track.artist.is_empty() {
        format!("#EXTINF:{},{title}", track.duration_ms / 1000)
    } else {
        format!(
            "#EXTINF:{},{} - {title}",
            track.duration_ms / 1000,
            track.artist
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(path: &str) -> Track {
        Track {
            path: path.into(),
            online: true,
            ..Default::default()
        }
    }

    fn library() -> Vec<Track> {
        vec![
            track("/media/internal/music/Hozier/Hozier/Take Me to Church.mp3"),
            track("/media/internal/music/Hozier/Wasteland/Like Real People Do.mp3"),
            track("/media/sd/Music/Bauhaus/Mask/The Passion of Lovers.flac"),
            track("/media/sd/Music/Other/Intro.mp3"),
            track("/media/sd/Music/Else/Intro.mp3"),
        ]
    }

    #[test]
    fn every_path_style_finds_its_song() {
        let tracks = library();
        let mut index = Index::default();
        let dir = Path::new("/media/internal/music/Playlists");
        let r = |index: &mut Index, loc: &str| index.resolve(&tracks, dir, loc);
        // Relative (what Reborn writes), case differences included.
        assert_eq!(
            r(&mut index, "../Hozier/Hozier/Take Me to Church.mp3"),
            Some(0)
        );
        assert_eq!(
            r(&mut index, "../hozier/HOZIER/take me to church.MP3"),
            Some(0)
        );
        // Absolute on the player.
        assert_eq!(
            r(
                &mut index,
                "/media/sd/Music/Bauhaus/Mask/The Passion of Lovers.flac"
            ),
            Some(2)
        );
        // A Windows export (backslashes, drive letter) and macOS / file URL.
        assert_eq!(
            r(
                &mut index,
                r"C:\Users\PC-R\Music\Hozier\Wasteland\Like Real People Do.mp3"
            ),
            Some(1)
        );
        assert_eq!(
            r(
                &mut index,
                "/Users/me/Music/Bauhaus/Mask/The Passion of Lovers.flac"
            ),
            Some(2)
        );
        assert_eq!(
            r(
                &mut index,
                "file:///C:/Users/me/Music/Bauhaus/Mask/The%20Passion%20of%20Lovers.flac"
            ),
            Some(2)
        );
        // Two "Intro.mp3": the folder decides; a bare ambiguous name finds nothing.
        assert_eq!(r(&mut index, r"D:\Music\Else\Intro.mp3"), Some(4));
        assert_eq!(r(&mut index, "Intro.mp3"), None);
        // Not in the library, and web addresses.
        assert_eq!(r(&mut index, r"C:\Music\Missing\Song.mp3"), None);
        assert_eq!(r(&mut index, "http://radio.example/stream"), None);
    }

    #[test]
    fn a_path_in_the_library_twice_finds_the_copy_on_the_player() {
        // The same card under an earlier source id (offline) and its current
        // one: every path twice, the stale copy first.
        let mut tracks = Vec::new();
        for online in [false, true] {
            for mut t in library() {
                t.online = online;
                tracks.push(t);
            }
        }
        let mut index = Index::default();
        let dir = Path::new("/media/internal/music/Playlists");
        let mut r = |loc: &str| index.resolve(&tracks, dir, loc);
        assert_eq!(r("../Hozier/Hozier/Take Me to Church.mp3"), Some(5));
        assert_eq!(
            r(r"C:\Users\PC-R\Music\Hozier\Wasteland\Like Real People Do.mp3"),
            Some(6)
        );
        assert_eq!(r("The Passion of Lovers.flac"), Some(7));
        assert_eq!(r(r"D:\Music\Else\Intro.mp3"), Some(9));
        // Only the stale copy: still found, so the page can say it is not available.
        let stale = vec![Track {
            online: false,
            ..track("/media/sd/Music/Bauhaus/Mask/The Passion of Lovers.flac")
        }];
        let mut index = Index::default();
        assert_eq!(
            index.resolve(
                &stale,
                dir,
                "/media/sd/Music/Bauhaus/Mask/The Passion of Lovers.flac"
            ),
            Some(0)
        );
    }

    #[test]
    fn locations_are_relative_on_the_same_storage_and_full_across() {
        let roots = [
            PathBuf::from("/media/internal/music"),
            PathBuf::from("/media/sd"),
        ];
        let list = Path::new("/media/internal/music/Playlists/Mix.m3u8");
        assert_eq!(
            location_for(
                Path::new("/media/internal/music/Hozier/Hozier/A.mp3"),
                list,
                &roots
            ),
            "../Hozier/Hozier/A.mp3"
        );
        assert_eq!(
            location_for(Path::new("/media/sd/Music/B.flac"), list, &roots),
            "/media/sd/Music/B.flac"
        );
        // And what is written is found again.
        let tracks = vec![track("/media/internal/music/Hozier/Hozier/A.mp3")];
        let mut index = Index::default();
        let location = location_for(&tracks[0].path, list, &roots);
        assert_eq!(
            index.resolve(&tracks, list.parent().unwrap(), &location),
            Some(0)
        );
    }

    #[test]
    fn non_ascii_entries_never_panic_and_still_resolve() {
        let tracks = vec![
            track("/media/sd/Music/Beyoncé - Halo.mp3"),
            track("/media/sd/Music/Café/Número 1º.flac"),
        ];
        let mut index = Index::default();
        let dir = Path::new("/media/sd/Music");
        let r = |index: &mut Index, loc: &str| index.resolve(&tracks, dir, loc);
        // `é` across byte 7, where `file://` would end.
        assert_eq!(r(&mut index, "Beyoncé - Halo.mp3"), Some(0));
        assert_eq!(
            r(
                &mut index,
                "file:///media/sd/Music/Caf%C3%A9/N%C3%BAmero%201%C2%BA.flac"
            ),
            Some(1)
        );
        // `%` before a multi-byte character or at the end stays as it is.
        for odd in [
            "%’x.mp3",
            "a%é.mp3",
            "file:///%’.mp3",
            "file:///%Aé.mp3",
            "x%",
            "%",
        ] {
            assert_eq!(r(&mut index, odd), None, "{odd}");
        }
        assert_eq!(percent_decode("%e2%80%99%zz%4"), "’%zz%4");
    }
}
