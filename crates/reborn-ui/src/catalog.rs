//! Cached ordered row identities, rebuilt on catalog/route change, not wheel ticks.
use crate::{album_filter, display_or_unknown, track_matches, Item};
use reborn_core::{
    sort::{sort_key, SortAs},
    AppModel, Screen, Track,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    /// A song in Folders, named by its file (Beta 8.39).
    FolderTrack(usize),
    Item(Item),
}

/// What a row of the search page is: the field, a section heading, Play All,
/// a section's See All or a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    Field,
    Heading,
    PlayAll,
    SeeAll,
    Result,
}

/// Keys of the search page's own rows.
pub const SEARCH_FIELD: &str = "search_edit";
pub const SEARCH_PLAY_ALL: &str = "search_play_all";
/// "See All 24 Songs": `search_all:songs` opens that section alone (#44).
pub const SEARCH_SEE_ALL: &str = "search_all:";
const HEADING: &str = "heading:";
/// Results each section shows on the whole search page; See All shows the rest.
pub const SECTION_PREVIEW: usize = 4;

/// A kind of search result, in the order the page lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Artists,
    Albums,
    Playlists,
    Audiobooks,
    Songs,
}
impl Section {
    pub fn key(self) -> &'static str {
        match self {
            Self::Artists => "artists",
            Self::Albums => "albums",
            Self::Playlists => "playlists",
            Self::Audiobooks => "audiobooks",
            Self::Songs => "songs",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Artists => "Artists",
            Self::Albums => "Albums",
            Self::Playlists => "Playlists",
            Self::Audiobooks => "Audiobooks",
            Self::Songs => "Songs",
        }
    }
    fn parse(key: &str) -> Option<Self> {
        [
            Self::Artists,
            Self::Albums,
            Self::Playlists,
            Self::Audiobooks,
            Self::Songs,
        ]
        .into_iter()
        .find(|s| s.key() == key)
    }
}

/// The search page's filter: what was typed and, after U+1F, the section
/// shown alone ("love\u{1f}songs", from See All).
pub fn search_parts(filter: &str) -> (&str, Option<Section>) {
    match filter.split_once('\u{1f}') {
        Some((query, section)) => (query, Section::parse(section)),
        None => (filter, None),
    }
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
            Self::FolderTrack(i) => file_item(*i, &tracks[*i]),
            Self::Item(i) => i.clone(),
        }
    }
    pub fn track(&self) -> Option<usize> {
        match self {
            Self::Track(i) | Self::AlbumTrack(i) | Self::SearchSong(i) | Self::FolderTrack(i) => {
                Some(*i)
            }
            Self::SearchAlbum(_) | Self::Item(_) => None,
        }
    }
    /// The row's part of the search page, without making its item.
    pub fn search_kind(&self) -> SearchKind {
        match self {
            Self::Item(i) if i.key == SEARCH_FIELD => SearchKind::Field,
            Self::Item(i) if i.key == SEARCH_PLAY_ALL => SearchKind::PlayAll,
            Self::Item(i) if i.key.starts_with(SEARCH_SEE_ALL) => SearchKind::SeeAll,
            Self::Item(i) if i.key.starts_with(HEADING) => SearchKind::Heading,
            _ => SearchKind::Result,
        }
    }
}
#[derive(Default)]
pub struct Catalog {
    stamp: Option<(usize, usize, Screen, String, reborn_core::ArtistsList)>,
    pub rows: Vec<Row>,
    /// The search page: what it found ("2 artists · 9 songs", or "No
    /// matches") and how many results that is.
    pub summary: String,
    pub found: usize,
    /// Folded names for the library search (kept across routes).
    pub index: crate::search::Index,
    /// Every song the search found, in its order: Play All and a song's
    /// queue take them all, not only the few the page shows.
    pub search_songs: Vec<usize>,
    /// The playlists as Playlists lists them, for the search (#44), and the
    /// playlists' generation they were made from.
    pub playlists: Vec<Item>,
    pub playlists_generation: Option<u64>,
}
impl Catalog {
    /// The search page (Beta 8.32): the field, then each kind of result
    /// under its heading — artists, albums, playlists, audiobooks, then songs
    /// with Play All. Each section shows its first results and See All; See
    /// All shows one section whole (#44). The keyboard's preview shows the
    /// first results of these rows.
    fn search_rows(&mut self, m: &AppModel, tracks: &[Track]) {
        let (query, only) = search_parts(&m.navigation.filter);
        let results = self.index.search(tracks, query);
        // Audiobooks and playlists by name, matched as music is: every typed
        // word begins a word (they are not among the songs).
        let folded = reborn_core::sort::fold(query);
        let named =
            |name: &str| crate::search::words_match(&folded, &reborn_core::sort::fold(name));
        let blank = folded.trim().is_empty();
        let books: Vec<reborn_core::books::Book> = if blank {
            vec![]
        } else {
            reborn_core::books::books(tracks)
                .into_iter()
                .filter(|b| named(&format!("{} {}", b.title, b.author)))
                .collect()
        };
        let playlists: Vec<Item> = if blank {
            vec![]
        } else {
            self.playlists
                .iter()
                .filter(|p| named(&p.label))
                .cloned()
                .collect()
        };
        let parts: Vec<String> = [
            (results.artists.len(), "artist", "artists"),
            (results.albums.len(), "album", "albums"),
            (playlists.len(), "playlist", "playlists"),
            (books.len(), "audiobook", "audiobooks"),
            (results.songs.len(), "song", "songs"),
        ]
        .into_iter()
        .filter(|(n, ..)| *n > 0)
        .map(|(n, one, many)| crate::components::count(n, one, many))
        .collect();
        self.found = results.artists.len()
            + results.albums.len()
            + playlists.len()
            + books.len()
            + results.songs.len();
        self.summary = if parts.is_empty() {
            "No matches".into()
        } else {
            parts.join(" · ")
        };
        self.search_songs = results.songs.clone();
        // One section alone has no field: Back returns to the whole page.
        if only.is_none() {
            // Skins show this row as a list row: it names the search.
            self.push(
                Item::new("Edit Search", SEARCH_FIELD)
                    .with_secondary(format!("“{query}” · {}", self.summary)),
            );
        }
        let artists = results
            .artists
            .into_iter()
            .map(|(artist, i, albums)| {
                let mut item = Item::new(display_or_unknown(&artist), format!("artist:{artist}"))
                    .with_secondary(crate::components::count(albums, "album", "albums"));
                item.art = Some(i);
                Row::Item(item)
            })
            .collect();
        self.section(only, Section::Artists, artists, None);
        let albums = results.albums.into_iter().map(Row::SearchAlbum).collect();
        self.section(only, Section::Albums, albums, None);
        self.section(
            only,
            Section::Playlists,
            playlists.into_iter().map(Row::Item).collect(),
            None,
        );
        let books = books
            .into_iter()
            .map(|book| {
                let mut item = Item::new(book.title.clone(), format!("book:{}", book.key))
                    .with_secondary(if book.author.is_empty() {
                        "Audiobook".to_owned()
                    } else {
                        book.author.clone()
                    });
                item.art = book.art;
                Row::Item(item)
            })
            .collect();
        self.section(only, Section::Audiobooks, books, None);
        let play_all = (results.songs.len() > 1).then(|| {
            Item::new("Play All", SEARCH_PLAY_ALL).with_secondary(crate::components::count(
                results.songs.len(),
                "song",
                "songs",
            ))
        });
        let songs = results.songs.into_iter().map(Row::SearchSong).collect();
        self.section(only, Section::Songs, songs, play_all);
    }

    /// One kind of result under its heading: on the whole page its first
    /// few and See All, alone all of it (`only`); another section alone
    /// leaves it out.
    fn section(
        &mut self,
        only: Option<Section>,
        section: Section,
        rows: Vec<Row>,
        lead: Option<Item>,
    ) {
        let total = rows.len();
        if total == 0 || only.is_some_and(|o| o != section) {
            return;
        }
        let mut heading = Item::new(section.name(), format!("{HEADING}{}", section.key()))
            .with_secondary(crate::screens::count_label(total));
        heading.enabled = false;
        self.push(heading);
        if let Some(lead) = lead {
            self.push(lead);
        }
        let whole = only.is_some() || total <= SECTION_PREVIEW;
        self.rows.extend(
            rows.into_iter()
                .take(if whole { total } else { SECTION_PREVIEW }),
        );
        if !whole {
            self.push(Item::new(
                format!("See All {total} {}", section.name()),
                format!("{SEARCH_SEE_ALL}{}", section.key()),
            ));
        }
    }

    fn push(&mut self, item: Item) {
        self.rows.push(Row::Item(item));
    }
    pub fn clear(&mut self) {
        self.stamp = None;
        self.index = Default::default();
    }
    /// The playlists the search looks through, as Playlists lists them.
    pub fn set_playlists(&mut self, generation: u64, items: Vec<Item>) {
        self.playlists = items;
        self.playlists_generation = Some(generation);
        self.stamp = None;
    }
    pub fn ensure(&mut self, m: &AppModel, tracks: &[Track]) {
        let stamp = (
            tracks.as_ptr() as usize,
            tracks.len(),
            m.screen,
            m.navigation.filter.clone(),
            m.settings.artists_list,
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
                // A genre's album opens on that genre's songs (#50).
                let genre = m.navigation.filter.strip_prefix("genre:");
                for ((album, artist), (i, t)) in albums {
                    let mut item = album_item(album, artist, i, t);
                    if let Some(genre) = genre {
                        item.key = format!("{}\u{1f}genre:{genre}", item.key);
                    }
                    self.push(item);
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
                // own, so a collaboration doesn't list its album again (#38);
                // or only album artists (Customize > Artists, #41).
                let album_artists =
                    m.settings.artists_list == reborn_core::ArtistsList::AlbumArtists;
                let prefix = if album_artists {
                    "albumartist"
                } else {
                    "artist"
                };
                for (i, t) in tracks.iter().enumerate().filter(|(_, t)| t.is_music()) {
                    let names = if album_artists {
                        vec![crate::album_artist_of(t)]
                    } else {
                        t.artists()
                    };
                    for artist in names {
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
                        Item::new(display_or_unknown(artist), format!("{prefix}:{artist}"))
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
                if parent.as_os_str().is_empty() {
                    self.folder_roots(m, tracks);
                    return;
                }
                // A folder as a file manager shows it (Beta 8.39): its
                // folders, then its songs, both by name with numbers in
                // number order; songs by file name, not by title or artist.
                let mut folders: HashMap<&std::ffi::OsStr, usize> = HashMap::new();
                let mut songs = vec![];
                for (i, t) in tracks.iter().enumerate().filter(|(_, t)| t.is_music()) {
                    let Ok(rest) = t.path.strip_prefix(parent) else {
                        continue;
                    };
                    let mut parts = rest.components();
                    let Some(first) = parts.next() else {
                        continue;
                    };
                    if parts.next().is_none() {
                        songs.push(i);
                    } else {
                        *folders.entry(first.as_os_str()).or_default() += 1;
                    }
                }
                let mut folders: Vec<_> = folders.into_iter().collect();
                folders.sort_by_cached_key(|(name, _)| name_key(&name.to_string_lossy()));
                for (name, songs) in folders {
                    let child = parent.join(name);
                    self.push(
                        Item::new(
                            name.to_string_lossy(),
                            format!("folder:{}", child.display()),
                        )
                        .with_secondary(crate::components::count(songs, "song", "songs")),
                    );
                }
                songs.sort_by_cached_key(|&i| name_key(&tracks[i].filename));
                self.rows.extend(songs.into_iter().map(Row::FolderTrack));
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
                // An album opened from a genre shows that genre's songs and
                // offers the whole album at the end (#50).
                let whole = crate::album_parts(&m.navigation.filter)
                    .filter(|(_, _, genre)| genre.is_some() && m.screen == Screen::Album)
                    .map(|(artist, album, genre)| {
                        let key = format!("album:{artist}\u{1f}{album}");
                        let total = tracks
                            .iter()
                            .filter(|t| t.is_music() && track_matches(t, &key))
                            .count();
                        (key, total, genre.unwrap_or_default())
                    })
                    .filter(|(_, total, _)| *total > ids.len());
                if matches!(m.screen, Screen::Album | Screen::Artist) {
                    if let Some(Row::Item(item)) = self.rows.first_mut() {
                        let songs = match &whole {
                            Some((_, total, genre)) => format!(
                                "{} · {} of {total} songs",
                                if genre.is_empty() { "No Genre" } else { genre },
                                ids.len()
                            ),
                            None => crate::components::count(ids.len(), "song", "songs"),
                        };
                        item.secondary = format!(
                            "{songs} · {}",
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
                if let Some((key, total, _)) = whole {
                    self.push(Item::new(format!("Show All {total} Songs"), key));
                }
            }
            _ => {}
        }
    }
}
impl Catalog {
    /// Folders' first page: the storages (Internal Storage, SD Card), or
    /// the songs' folders when none is online.
    fn folder_roots(&mut self, m: &AppModel, tracks: &[Track]) {
        let mut children = BTreeSet::new();
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
        for child in children {
            // Storage roots are named for what they are, not their path.
            let source = m.sources.iter().find(|s| s.root == child);
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
    }
}
/// A file or folder name's place in Folders (`natural_key`); the name
/// itself breaks ties.
fn name_key(name: &str) -> (String, String) {
    (reborn_core::sort::natural_key(name), name.to_owned())
}
/// The songs below `root` in the order Folders lists them: in each folder
/// its folders first, then its songs, both by name. Playing a folder plays
/// in this order. Each folder's names are sorted once (a whole card is
/// 120 000 songs in some 10 000 folders).
pub(crate) fn folder_play_order(
    root: &std::path::Path,
    tracks: &[Track],
    members: &[usize],
) -> Vec<usize> {
    use std::path::Path;
    let mut songs: HashMap<&Path, Vec<usize>> = HashMap::new();
    let mut folders: HashMap<&Path, Vec<&Path>> = HashMap::new();
    let mut linked: std::collections::HashSet<&Path> = Default::default();
    for &i in members {
        let Some(parent) = tracks[i].path.parent() else {
            continue;
        };
        songs.entry(parent).or_default().push(i);
        // Each folder joins the one above it once, up to `root`.
        let mut dir = parent;
        while dir != root && dir.starts_with(root) && linked.insert(dir) {
            let Some(up) = dir.parent() else {
                break;
            };
            folders.entry(up).or_default().push(dir);
            dir = up;
        }
    }
    let mut order = Vec::with_capacity(members.len());
    let mut stack = vec![root];
    let name = |p: &Path| {
        p.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    };
    while let Some(dir) = stack.pop() {
        if let Some(mut subfolders) = folders.remove(dir) {
            subfolders.sort_by_cached_key(|f| name_key(&name(f)));
            // Songs come after the subfolders: they wait on the stack.
            stack.push(dir);
            stack.extend(subfolders.into_iter().rev());
            continue;
        }
        if let Some(mut mine) = songs.remove(dir) {
            mine.sort_by_cached_key(|&i| name_key(&tracks[i].filename));
            order.extend(mine);
        }
    }
    order
}
/// A song in Folders: its file name without the extension.
fn file_item(i: usize, t: &Track) -> Item {
    let mut item = track_item(i, t);
    let stem = std::path::Path::new(&t.filename)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty());
    if let Some(stem) = stem {
        item.label = stem;
    }
    item
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
