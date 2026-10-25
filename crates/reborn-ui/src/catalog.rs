//! Cached ordered row identities, rebuilt on catalog/route change, not wheel ticks.
use crate::{album_filter, display_or_unknown, track_matches, Item};
use reborn_core::{
    sort::{sort_key, SortAs},
    AppModel, Screen, Track,
};
use std::collections::{BTreeMap, BTreeSet};
/// A genre's cover track, its albums, its song count and whether the cover
/// track has artwork.
type GenreStats<'a> = (usize, BTreeSet<(&'a str, &'a str)>, usize, bool);
pub enum Row {
    Track(usize),
    /// A track on its album's own page; the artist line is redundant there.
    AlbumTrack(usize),
    /// A song among search results: its album too, to tell versions apart.
    SearchSong(usize),
    /// An album among search results (a track of it).
    SearchAlbum(usize),
    Item(Item),
}

/// What a row of the search page is: the field, a section heading, Play All
/// or a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    Field,
    Heading,
    PlayAll,
    Result,
}

/// Keys of the search page's own rows.
pub const SEARCH_FIELD: &str = "search_edit";
pub const SEARCH_PLAY_ALL: &str = "search_play_all";
const HEADING: &str = "heading:";
impl Row {
    pub fn item(&self, tracks: &[Track]) -> Item {
        match self {
            Self::Track(i) => track_item(*i, &tracks[*i]),
            Self::AlbumTrack(i) => {
                let mut item = track_item(*i, &tracks[*i]);
                item.secondary.clear();
                item
            }
            Self::SearchSong(i) => {
                let t = &tracks[*i];
                let mut item = track_item(*i, t);
                if !t.album.is_empty() {
                    item.secondary = format!("{} · {}", item.secondary, t.album);
                }
                item
            }
            Self::SearchAlbum(i) => {
                let t = &tracks[*i];
                album_item(&t.album, &t.album_artist, *i, t)
            }
            Self::Item(i) => i.clone(),
        }
    }
    pub fn track(&self) -> Option<usize> {
        match self {
            Self::Track(i) | Self::AlbumTrack(i) | Self::SearchSong(i) => Some(*i),
            Self::SearchAlbum(_) | Self::Item(_) => None,
        }
    }
    /// The row's part of the search page, without making its item.
    pub fn search_kind(&self) -> SearchKind {
        match self {
            Self::Item(i) if i.key == SEARCH_FIELD => SearchKind::Field,
            Self::Item(i) if i.key == SEARCH_PLAY_ALL => SearchKind::PlayAll,
            Self::Item(i) if i.key.starts_with(HEADING) => SearchKind::Heading,
            _ => SearchKind::Result,
        }
    }
}
#[derive(Default)]
pub struct Catalog {
    stamp: Option<(usize, usize, Screen, String)>,
    pub rows: Vec<Row>,
    /// The search page: what it found ("2 artists · 9 songs", or "No
    /// matches") and how many results that is.
    pub summary: String,
    pub found: usize,
    /// Folded names for the library search (kept across routes).
    pub index: crate::search::Index,
}
impl Catalog {
    /// The search page (Beta 8.32): the field, then each kind of result
    /// under its heading — artists, albums, audiobooks, then songs with Play
    /// All. The keyboard's preview shows the first results of these rows.
    fn search_rows(&mut self, m: &AppModel, tracks: &[Track]) {
        let query = &m.navigation.filter;
        let results = self.index.search(tracks, query);
        // Audiobooks by title or author, matched as music is: every typed
        // word begins a word (they are not among the songs).
        let folded = reborn_core::sort::fold(query);
        let books: Vec<reborn_core::books::Book> = if folded.trim().is_empty() {
            vec![]
        } else {
            reborn_core::books::books(tracks)
                .into_iter()
                .filter(|b| {
                    let name = reborn_core::sort::fold(&format!("{} {}", b.title, b.author));
                    crate::search::words_match(&folded, &name)
                })
                .collect()
        };
        let parts: Vec<String> = [
            (results.artists.len(), "artist", "artists"),
            (results.albums.len(), "album", "albums"),
            (books.len(), "audiobook", "audiobooks"),
            (results.songs.len(), "song", "songs"),
        ]
        .into_iter()
        .filter(|(n, ..)| *n > 0)
        .map(|(n, one, many)| crate::components::count(n, one, many))
        .collect();
        self.found =
            results.artists.len() + results.albums.len() + books.len() + results.songs.len();
        self.summary = if parts.is_empty() {
            "No matches".into()
        } else {
            parts.join(" · ")
        };
        // Skins show this row as a list row: it names the search.
        self.push(
            Item::new("Edit Search", SEARCH_FIELD)
                .with_secondary(format!("“{query}” · {}", self.summary)),
        );
        let heading = |name: &str, n: usize| {
            let mut item = Item::new(name, format!("{HEADING}{}", name.to_lowercase()))
                .with_secondary(crate::screens::count_label(n));
            item.enabled = false;
            item
        };
        if !results.artists.is_empty() {
            self.push(heading("Artists", results.artists.len()));
        }
        for (artist, i, albums) in results.artists {
            let mut item = Item::new(display_or_unknown(&artist), format!("artist:{artist}"))
                .with_secondary(crate::components::count(albums, "album", "albums"));
            item.art = Some(i);
            self.push(item);
        }
        if !results.albums.is_empty() {
            self.push(heading("Albums", results.albums.len()));
        }
        self.rows
            .extend(results.albums.into_iter().map(Row::SearchAlbum));
        if !books.is_empty() {
            self.push(heading("Audiobooks", books.len()));
        }
        for book in books {
            let mut item = Item::new(book.title.clone(), format!("book:{}", book.key))
                .with_secondary(if book.author.is_empty() {
                    "Audiobook".to_owned()
                } else {
                    book.author.clone()
                });
            item.art = book.art;
            self.push(item);
        }
        if !results.songs.is_empty() {
            self.push(heading("Songs", results.songs.len()));
            if results.songs.len() > 1 {
                self.push(Item::new("Play All", SEARCH_PLAY_ALL).with_secondary(
                    crate::components::count(results.songs.len(), "song", "songs"),
                ));
            }
        }
        self.rows
            .extend(results.songs.into_iter().map(Row::SearchSong));
    }

    fn push(&mut self, item: Item) {
        self.rows.push(Row::Item(item));
    }
    pub fn clear(&mut self) {
        self.stamp = None;
        self.index = Default::default();
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
            Screen::Search => self.search_rows(m, tracks),
            Screen::Albums => {
                let mut albums = BTreeMap::new();
                for (i, t) in tracks.iter().enumerate().filter(|(_, t)| {
                    t.is_music()
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
                // A genre's album view opens with all of its songs, then
                // Shuffle (as on an album page; user request).
                if m.navigation.filter.starts_with("genre:") && !albums.is_empty() {
                    let songs = tracks
                        .iter()
                        .filter(|t| track_matches(t, &m.navigation.filter))
                        .count();
                    self.push(
                        Item::new("All Songs", "all_songs")
                            .with_secondary(crate::components::count(songs, "song", "songs")),
                    );
                    self.push(Item::new("Shuffle", "shuffle:collection"));
                }
                for ((album, artist), (i, t)) in albums {
                    self.push(album_item(album, artist, i, t));
                }
            }
            Screen::Genres => {
                let mut genres: BTreeMap<&str, GenreStats> = BTreeMap::new();
                // Each genre of a joined tag is a genre of its own (#40).
                for (i, t) in tracks.iter().enumerate().filter(|(_, t)| t.is_music()) {
                    for genre in t.genres() {
                        let entry =
                            genres
                                .entry(genre)
                                .or_insert((i, BTreeSet::new(), 0, t.artwork));
                        entry.1.insert((t.album.as_str(), t.album_artist.as_str()));
                        entry.2 += 1;
                        if !entry.3 && t.artwork {
                            entry.0 = i;
                            entry.3 = true;
                        }
                    }
                }
                // Untagged songs go last, under one honest name.
                let (mut named, untagged): (Vec<_>, Vec<_>) =
                    genres.into_iter().partition(|(genre, _)| !genre.is_empty());
                // A to Z as everywhere: case, accents and a leading article aside.
                named.sort_by_cached_key(|(genre, _)| title_key(genre));
                for (genre, (i, albums, songs, _)) in named.into_iter().chain(untagged) {
                    let label = if genre.is_empty() { "No Genre" } else { genre };
                    let albums = crate::components::count(albums.len(), "album", "albums");
                    let songs = crate::components::count(songs, "song", "songs");
                    let mut item = Item::new(label, format!("genre:{genre}"))
                        .with_secondary(format!("{albums} · {songs}"));
                    item.art = Some(i);
                    self.push(item);
                }
            }
            Screen::Artists => {
                let mut artists: BTreeMap<&str, (usize, BTreeSet<&str>, bool)> = BTreeMap::new();
                // Each name of a joined or "feat." tag is an artist of its
                // own, so a collaboration doesn't list its album again (#38).
                for (i, t) in tracks.iter().enumerate().filter(|(_, t)| t.is_music()) {
                    for artist in t.artists() {
                        let entry =
                            artists
                                .entry(artist)
                                .or_insert((i, BTreeSet::new(), t.artwork));
                        entry.1.insert(t.album.as_str());
                        if !entry.2 && t.artwork {
                            entry.0 = i;
                            entry.2 = true;
                        }
                    }
                }
                let mut artists: Vec<_> = artists.into_iter().collect();
                artists.sort_by_cached_key(|(artist, _)| artist_key(artist));
                for (artist, (i, albums, _)) in artists {
                    let mut item =
                        Item::new(display_or_unknown(artist), format!("artist:{artist}"))
                            .with_secondary(crate::components::count(
                                albums.len(),
                                "album",
                                "albums",
                            ));
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
                        for t in tracks.iter().filter(|t| t.is_music()) {
                            if let Some(p) = t.path.parent() {
                                children.insert(p.to_path_buf());
                            }
                        }
                    }
                } else {
                    for t in tracks.iter().filter(|t| t.is_music()) {
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
                        .filter(|(_, t)| t.is_music() && t.path.parent() == Some(parent))
                    {
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
                    .filter(|(_, t)| t.is_music() && track_matches(t, &m.navigation.filter))
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
                            "{} · {}",
                            crate::components::count(ids.len(), "song", "songs"),
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
fn album_item(album: &str, artist: &str, index: usize, t: &Track) -> Item {
    let mut item = Item::new(display_or_unknown(album), album_filter(t)).with_secondary(
        crate::names_or_unknown(if artist.is_empty() { &t.artist } else { artist }),
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
    .with_secondary(crate::names_or_unknown(&t.artist));
    item.art = Some(i);
    item
}
