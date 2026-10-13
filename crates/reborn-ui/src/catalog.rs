//! Cached ordered row identities, rebuilt on catalog/route change, not wheel ticks.
use crate::{album_filter, display_or_unknown, track_matches, Item};
use reborn_core::{
    sort::{sort_key, SortAs},
    AppModel, Playlist, Screen, Track,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
/// A genre's cover track, its albums, its song count and whether the cover
/// track has artwork.
type GenreStats<'a> = (usize, BTreeSet<(&'a str, &'a str)>, usize, bool);
pub enum Row {
    Track(usize),
    /// A track on its album's own page; the artist line is redundant there.
    AlbumTrack(usize),
    Item(Item),
}
impl Row {
    pub fn item(&self, tracks: &[Track]) -> Item {
        match self {
            Self::Track(i) => track_item(*i, &tracks[*i]),
            Self::AlbumTrack(i) => {
                let mut item = track_item(*i, &tracks[*i]);
                item.secondary.clear();
                item
            }
            Self::Item(i) => i.clone(),
        }
    }
    pub fn track(&self) -> Option<usize> {
        match self {
            Self::Track(i) | Self::AlbumTrack(i) => Some(*i),
            Self::Item(_) => None,
        }
    }
}
#[derive(Default)]
pub struct Catalog {
    stamp: Option<(usize, usize, Screen, String)>,
    pub rows: Vec<Row>,
}
impl Catalog {
    fn push(&mut self, item: Item) {
        self.rows.push(Row::Item(item));
    }
    pub fn clear(&mut self) {
        self.stamp = None;
    }
    pub fn ensure(&mut self, m: &AppModel, tracks: &[Track]) {
        let stamp = (
            tracks.as_ptr() as usize,
            tracks.len(),
            m.screen,
            m.navigation.filter.clone(),
        );
        if self.stamp.as_ref() == Some(&stamp) {
            return;
        }
        self.stamp = Some(stamp);
        self.rows.clear();
        match m.screen {
            Screen::Albums => {
                let mut albums = BTreeMap::new();
                for (i, t) in tracks.iter().enumerate().filter(|(_, t)| {
                    t.online
                        && (m.navigation.filter.is_empty()
                            || track_matches(t, &m.navigation.filter))
                }) {
                    albums
                        .entry((t.album.as_str(), t.album_artist.as_str()))
                        .or_insert((i, t));
                }
                // A to Z by title, then album artist, articles skipped.
                let mut albums: Vec<_> = albums.into_iter().collect();
                albums.sort_by_cached_key(|((album, artist), _)| {
                    (title_key(album), artist_key(artist))
                });
                // A genre's album view opens with all of its songs.
                if m.navigation.filter.starts_with("genre:") && !albums.is_empty() {
                    let songs = tracks
                        .iter()
                        .filter(|t| track_matches(t, &m.navigation.filter))
                        .count();
                    self.push(
                        Item::new("All Songs", "all_songs").with_secondary(match songs {
                            1 => "1 song".to_owned(),
                            n => format!("{n} songs"),
                        }),
                    );
                }
                for ((album, artist), (i, t)) in albums {
                    self.push(album_item(album, artist, i, t));
                }
            }
            Screen::Genres => {
                let mut genres: BTreeMap<&str, GenreStats> = BTreeMap::new();
                for (i, t) in tracks.iter().enumerate().filter(|(_, t)| t.online) {
                    let entry = genres.entry(t.genre.as_str()).or_insert((
                        i,
                        BTreeSet::new(),
                        0,
                        t.artwork,
                    ));
                    entry.1.insert((t.album.as_str(), t.album_artist.as_str()));
                    entry.2 += 1;
                    if !entry.3 && t.artwork {
                        entry.0 = i;
                        entry.3 = true;
                    }
                }
                // Untagged songs go last, under one honest name.
                let (mut named, untagged): (Vec<_>, Vec<_>) =
                    genres.into_iter().partition(|(genre, _)| !genre.is_empty());
                // A to Z as everywhere: case, accents and a leading article aside.
                named.sort_by_cached_key(|(genre, _)| title_key(genre));
                for (genre, (i, albums, songs, _)) in named.into_iter().chain(untagged) {
                    let label = if genre.is_empty() { "No Genre" } else { genre };
                    let albums = match albums.len() {
                        1 => "1 album".to_owned(),
                        n => format!("{n} albums"),
                    };
                    let songs = match songs {
                        1 => "1 song".to_owned(),
                        n => format!("{n} songs"),
                    };
                    let mut item = Item::new(label, format!("genre:{genre}"))
                        .with_secondary(format!("{albums} · {songs}"));
                    item.art = Some(i);
                    self.push(item);
                }
            }
            Screen::Artists => {
                let mut artists: BTreeMap<&str, (usize, BTreeSet<&str>, bool)> = BTreeMap::new();
                for (i, t) in tracks.iter().enumerate().filter(|(_, t)| t.online) {
                    let entry =
                        artists
                            .entry(t.artist.as_str())
                            .or_insert((i, BTreeSet::new(), t.artwork));
                    entry.1.insert(t.album.as_str());
                    if !entry.2 && t.artwork {
                        entry.0 = i;
                        entry.2 = true;
                    }
                }
                let mut artists: Vec<_> = artists.into_iter().collect();
                artists.sort_by_cached_key(|(artist, _)| artist_key(artist));
                for (artist, (i, albums, _)) in artists {
                    let mut item =
                        Item::new(display_or_unknown(artist), format!("artist:{artist}"))
                            .with_secondary(match albums.len() {
                                1 => "1 album".to_owned(),
                                n => format!("{n} albums"),
                            });
                    item.art = Some(i);
                    self.push(item);
                }
            }
            Screen::Folders => {
                let parent =
                    std::path::Path::new(m.navigation.filter.strip_prefix("folder:").unwrap_or(""));
                let mut children = BTreeSet::new();
                if parent.as_os_str().is_empty() {
                    for s in m.sources.iter().filter(|s| s.online) {
                        children.insert(s.root.clone());
                    }
                    if children.is_empty() {
                        for t in tracks.iter().filter(|t| t.online) {
                            if let Some(p) = t.path.parent() {
                                children.insert(p.to_path_buf());
                            }
                        }
                    }
                } else {
                    for t in tracks.iter().filter(|t| t.online) {
                        if let Ok(suffix) = t.path.strip_prefix(parent) {
                            if let Some(first) = suffix.components().next() {
                                let child = parent.join(first);
                                if child != t.path {
                                    children.insert(child);
                                }
                            }
                        }
                    }
                }
                let root = parent.as_os_str().is_empty();
                for child in children {
                    // Storage roots are named for what they are, not their path.
                    let source = m.sources.iter().find(|s| root && s.root == child);
                    let label = match source.map(|s| &s.kind) {
                        Some(reborn_core::MediaSource::Internal) => "Internal Storage".into(),
                        Some(reborn_core::MediaSource::SdCard(_)) => "SD Card".into(),
                        None => child
                            .file_name()
                            .map(|x| x.to_string_lossy().into_owned())
                            .unwrap_or_else(|| child.display().to_string()),
                    };
                    self.push(
                        Item::new(label, format!("folder:{}", child.display()))
                            .with_secondary(if source.is_some() { "" } else { "Folder" }),
                    );
                }
                if !parent.as_os_str().is_empty() {
                    for (i, _) in tracks
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| t.online && t.path.parent() == Some(parent))
                    {
                        self.rows.push(Row::Track(i));
                    }
                }
            }
            Screen::Playlists => {
                let index = id_index(tracks);
                // A to Z as everywhere: case, accents and a leading article aside.
                let mut playlists: Vec<_> =
                    m.library.playlists.iter().filter(|p| p.online).collect();
                playlists.sort_by_cached_key(|p| title_key(&p.name));
                for p in playlists {
                    let members = members_with(&index, tracks, p);
                    // Entries that matched no song, or a song since removed
                    // or on media that is not inserted.
                    let missing = p.unresolved as usize + (p.track_ids.len() - members.len());
                    let songs = match members.len() {
                        1 => "1 song".to_owned(),
                        n => format!("{n} songs"),
                    };
                    let mut item = Item::new(&p.name, format!("playlist:{}", p.id)).with_secondary(
                        if missing > 0 {
                            format!("{songs} · {missing} missing")
                        } else {
                            songs
                        },
                    );
                    item.art = members
                        .iter()
                        .copied()
                        .find(|i| tracks[*i].artwork)
                        .or(members.first().copied());
                    self.push(item);
                }
            }
            Screen::Playlist => {
                if let Some(p) = playlist(m) {
                    let index = id_index(tracks);
                    for i in members_with(&index, tracks, p) {
                        self.rows.push(Row::Track(i));
                    }
                }
            }
            Screen::Tracks | Screen::Album | Screen::Artist => {
                if matches!(m.screen, Screen::Album | Screen::Artist) {
                    self.push(Item::new("Play", "play:collection"));
                }
                if m.screen == Screen::Album {
                    self.push(Item::new("Shuffle", "shuffle:collection"));
                }
                let mut ids: Vec<_> = tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.online && track_matches(t, &m.navigation.filter))
                    .collect();
                if ids.is_empty() {
                    self.rows.clear();
                    return;
                }
                if m.screen == Screen::Tracks && m.navigation.filter.is_empty() {
                    ids.sort_by_cached_key(|(_, t)| {
                        title_key(if t.title.is_empty() {
                            &t.filename
                        } else {
                            &t.title
                        })
                    });
                }
                if m.screen == Screen::Album {
                    ids.sort_by_key(|(_, t)| (t.disc, t.track, t.filename.as_str()));
                }
                if matches!(m.screen, Screen::Album | Screen::Artist) {
                    if let Some(Row::Item(item)) = self.rows.first_mut() {
                        item.secondary = format!(
                            "{} songs · {}",
                            ids.len(),
                            crate::components::time(ids.iter().map(|(_, t)| t.duration_ms).sum())
                        );
                    }
                }
                if m.screen == Screen::Artist {
                    // An artist page lists the artist's albums, not their songs.
                    let mut albums = BTreeMap::new();
                    for (i, t) in &ids {
                        albums
                            .entry((t.album.as_str(), t.album_artist.as_str()))
                            .or_insert((*i, *t));
                    }
                    let mut albums: Vec<_> = albums.into_iter().collect();
                    albums.sort_by_cached_key(|((album, artist), _)| {
                        (title_key(album), artist_key(artist))
                    });
                    for ((album, artist), (i, t)) in albums {
                        self.push(album_item(album, artist, i, t));
                    }
                    return;
                }
                for (i, t) in ids {
                    // Album rows only name an artist when it differs.
                    if m.screen == Screen::Album
                        && (t.artist == t.album_artist || t.album_artist.is_empty())
                    {
                        self.rows.push(Row::AlbumTrack(i));
                    } else {
                        self.rows.push(Row::Track(i));
                    }
                }
            }
            _ => {}
        }
    }
}
/// A to Z keys (`reborn_core::sort::sort_key`): an artist skips a leading
/// "The ", a title "The", "A" or "An". The name itself breaks ties.
fn artist_key(name: &str) -> (String, String) {
    (sort_key(name, SortAs::Artist), name.to_owned())
}
fn title_key(name: &str) -> (String, String) {
    (sort_key(name, SortAs::Title), name.to_owned())
}
/// Library index of every track id; built when a playlist view is rebuilt,
/// never per wheel tick.
fn id_index(tracks: &[Track]) -> HashMap<i64, usize> {
    tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect()
}
fn members_with(index: &HashMap<i64, usize>, tracks: &[Track], p: &Playlist) -> Vec<usize> {
    p.track_ids
        .iter()
        .filter_map(|id| index.get(id).copied())
        .filter(|i| tracks[*i].online)
        .collect()
}
/// The playlist with id `id` (as written in a `playlist:<id>` key).
pub fn playlist_by_key<'a>(m: &'a AppModel, key: &str) -> Option<&'a Playlist> {
    let id = key.strip_prefix("playlist:")?.parse::<i64>().ok()?;
    m.library.playlists.iter().find(|p| p.id == id)
}
/// The playlist the Playlist screen shows.
pub fn playlist(m: &AppModel) -> Option<&Playlist> {
    playlist_by_key(m, &m.navigation.filter)
}
/// Library indices of a playlist's playable songs, in file order.
pub fn playlist_members(tracks: &[Track], p: &Playlist) -> Vec<usize> {
    members_with(&id_index(tracks), tracks, p)
}
fn album_item(album: &str, artist: &str, index: usize, t: &Track) -> Item {
    let mut item = Item::new(display_or_unknown(album), album_filter(t)).with_secondary(
        display_or_unknown(if artist.is_empty() { &t.artist } else { artist }),
    );
    item.art = Some(index);
    item
}
fn track_item(i: usize, t: &Track) -> Item {
    let mut item = Item::new(
        if t.title.is_empty() {
            &t.filename
        } else {
            &t.title
        },
        format!("track:{i}"),
    )
    .with_secondary(display_or_unknown(&t.artist));
    item.art = Some(i);
    item
}
