//! Read-only M3U/M3U8 playlist import.
//!
//! Playlist files are discovered by the scanner and parsed here. Every entry
//! is mapped to a library track through a [`LibraryIndex`], built once per
//! synchronisation from the library's paths: hashed lowercase full paths and
//! hashed lowercase trailing components (`file`, `Album/file`,
//! `Artist/Album/file`). Each entry costs a handful of hash lookups, never a
//! walk over the library. Reborn never writes to a playlist file.
use std::{
    borrow::Cow,
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    path::Path,
};

/// Larger files are not playlists a person made; they are skipped.
pub const MAX_PLAYLIST_BYTES: u64 = 4 * 1024 * 1024;
/// Entries beyond the queue limit could never be played together.
pub const MAX_PLAYLIST_ENTRIES: usize = reborn_core::MAX_QUEUE;
/// Playlists considered per scan, across all sources.
pub const MAX_PLAYLISTS: usize = 1_000;
/// Folder depth below a source root at which playlists are still found.
pub const MAX_PLAYLIST_DEPTH: usize = 8;

/// Whether `path` names an M3U or M3U8 playlist.
pub fn is_playlist(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        matches!(
            e.to_string_lossy().to_ascii_lowercase().as_str(),
            "m3u" | "m3u8"
        )
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The location exactly as written (trimmed).
    pub location: String,
    /// The `#EXTINF` title, when one preceded the entry.
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    /// A `#PLAYLIST:` name, when the file carries one.
    pub name: Option<String>,
    pub entries: Vec<Entry>,
}

fn strip_prefix_ci<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    line.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &line[prefix.len()..])
}

/// Parse an extended or plain M3U/M3U8 playlist. UTF-8 is expected (with or
/// without a byte-order mark); a file that is not valid UTF-8 is read as
/// Latin-1, as legacy `.m3u` files usually are. CR, LF and CRLF all end a line.
pub fn parse(bytes: &[u8]) -> Parsed {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => Cow::Owned(bytes.iter().map(|&b| char::from(b)).collect()),
    };
    let mut parsed = Parsed::default();
    let mut title = None;
    for line in text.split(['\n', '\r']) {
        let line = line.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            if let Some(info) = strip_prefix_ci(line, "#EXTINF:") {
                title = info
                    .split_once(',')
                    .map(|(_, t)| t.trim().to_owned())
                    .filter(|t| !t.is_empty());
            } else if let Some(name) = strip_prefix_ci(line, "#PLAYLIST:") {
                let name = name.trim();
                if !name.is_empty() {
                    parsed.name = Some(name.to_owned());
                }
            }
            continue;
        }
        if parsed.entries.len() >= MAX_PLAYLIST_ENTRIES {
            break;
        }
        parsed.entries.push(Entry {
            location: line.to_owned(),
            title: title.take(),
        });
    }
    parsed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    /// Relative to the playlist's folder.
    Relative,
    /// A path on this player.
    Absolute,
    /// A Windows drive or network path: only its trailing components help.
    Foreign,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Location {
    anchor: Anchor,
    /// Path components, `..` kept, `.` and empty ones dropped.
    parts: Vec<String>,
}

fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = char::from(bytes[i + 1]).to_digit(16);
            let lo = char::from(bytes[i + 2]).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok().filter(|decoded| decoded != s)
}

fn has_drive(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'/')
}

fn location(raw: &str) -> Option<Location> {
    let raw = raw.trim();
    let (body, url) = match strip_prefix_ci(raw, "file:") {
        Some(rest) => {
            let rest = rest.strip_prefix("//").unwrap_or(rest);
            let rest = strip_prefix_ci(rest, "localhost").unwrap_or(rest);
            (
                percent_decode(rest).unwrap_or_else(|| rest.to_owned()),
                true,
            )
        }
        // Streams and other URLs cannot be songs in the library.
        None if raw.contains("://") => return None,
        None => (raw.to_owned(), false),
    };
    let body = body.replace('\\', "/");
    let body = match body.strip_prefix('/') {
        Some(rest) if url && has_drive(rest) => rest,
        _ => body.as_str(),
    };
    let (anchor, body) = if has_drive(body) {
        (Anchor::Foreign, &body[2..])
    } else if body.starts_with("//") {
        (Anchor::Foreign, body)
    } else if body.starts_with('/') {
        (Anchor::Absolute, body)
    } else {
        (Anchor::Relative, body)
    };
    let parts: Vec<String> = body
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .map(str::to_owned)
        .collect();
    (!parts.is_empty()).then_some(Location { anchor, parts })
}

/// The ways an entry may be spelled: as written and, when it carries
/// percent escapes, decoded.
fn spellings(raw: &str) -> Vec<Location> {
    let mut out: Vec<Location> = location(raw).into_iter().collect();
    if !raw.trim_start().to_ascii_lowercase().starts_with("file:") {
        if let Some(decoded) = percent_decode(raw).and_then(|d| location(&d)) {
            if !out.contains(&decoded) {
                out.push(decoded);
            }
        }
    }
    out
}

/// `base` joined with `parts`, `..` applied lexically and never above `/`.
fn join(base: &Path, parts: &[String]) -> String {
    let base = base.to_string_lossy();
    let mut stack: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for part in parts {
        if part == ".." {
            stack.pop();
        } else {
            stack.push(part);
        }
    }
    let mut out = String::with_capacity(base.len() + 64);
    for part in stack {
        out.push('/');
        out.push_str(part);
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

fn key(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

#[derive(Debug, Clone)]
enum Hits {
    One(u32),
    Many(Vec<u32>),
}
impl Hits {
    fn add(&mut self, index: u32) {
        match self {
            Self::One(first) => *self = Self::Many(vec![*first, index]),
            Self::Many(all) => all.push(index),
        }
    }
    fn as_slice(&self) -> &[u32] {
        match self {
            Self::One(one) => std::slice::from_ref(one),
            Self::Many(all) => all,
        }
    }
}

/// Library paths indexed for playlist resolution.
pub struct LibraryIndex {
    paths: Vec<(i64, String)>,
    full: HashMap<u64, Hits>,
    /// Trailing 1, 2 and 3 components.
    suffix: [HashMap<u64, Hits>; 3],
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    pub track_ids: Vec<i64>,
    pub unresolved: u32,
}

impl LibraryIndex {
    /// Index `(track id, path)` pairs of the library.
    pub fn new(paths: Vec<(i64, String)>) -> Self {
        let mut full: HashMap<u64, Hits> = HashMap::with_capacity(paths.len());
        let mut suffix: [HashMap<u64, Hits>; 3] = Default::default();
        for map in &mut suffix {
            map.reserve(paths.len());
        }
        fn insert(map: &mut HashMap<u64, Hits>, k: u64, index: u32) {
            map.entry(k)
                .and_modify(|hits| hits.add(index))
                .or_insert(Hits::One(index));
        }
        for (index, (_, path)) in paths.iter().enumerate() {
            let index = index as u32;
            let lower = path.to_lowercase();
            insert(&mut full, key(&lower), index);
            // Byte offsets of the last three separators give the suffixes
            // without building component vectors.
            let mut end = lower.len();
            let mut taken = 0;
            while taken < 3 {
                let Some(slash) = lower[..end].rfind('/') else {
                    break;
                };
                let tail = &lower[slash + 1..];
                if !tail.is_empty() && slash + 1 < lower.len() {
                    insert(&mut suffix[taken], key(tail), index);
                    taken += 1;
                }
                end = slash;
            }
        }
        Self {
            paths,
            full,
            suffix,
        }
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// One candidate among `hits` that passes `verify`; on a tie, the one
    /// on the playlist's own media.
    fn pick(&self, hits: &Hits, root: &Path, verify: impl Fn(&str) -> bool) -> Option<i64> {
        let matching: Vec<&(i64, String)> = hits
            .as_slice()
            .iter()
            .map(|i| &self.paths[*i as usize])
            .filter(|(_, path)| verify(path))
            .collect();
        match matching.as_slice() {
            [] => None,
            [(id, _)] => Some(*id),
            many => {
                let mut local = many
                    .iter()
                    .filter(|(_, path)| Path::new(path).starts_with(root));
                match (local.next(), local.next()) {
                    (Some((id, _)), None) => Some(*id),
                    _ => None,
                }
            }
        }
    }

    fn full_path(&self, path: &str, exact: bool, root: &Path) -> Option<i64> {
        let lower = path.to_lowercase();
        let hits = self.full.get(&key(&lower))?;
        self.pick(hits, root, |candidate| {
            if exact {
                candidate == path
            } else {
                candidate.to_lowercase() == lower
            }
        })
    }

    fn trailing(&self, parts: &[&String], n: usize, root: &Path) -> Option<i64> {
        if parts.len() < n {
            return None;
        }
        let tail = parts[parts.len() - n..]
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join("/")
            .to_lowercase();
        let hits = self.suffix[n - 1].get(&key(&tail))?;
        let needle = format!("/{tail}");
        self.pick(hits, root, |candidate| {
            let lower = candidate.to_lowercase();
            lower.ends_with(&needle) || lower == tail
        })
    }

    /// The library track for one playlist entry. Tried in order: the path
    /// relative to the playlist's folder, the path as an absolute path (or
    /// relative to the media root), the same case-insensitively, then the
    /// trailing `Artist/Album/file`, `Album/file` and `file` components
    /// ("portable" relocation), accepted only when unambiguous.
    pub fn resolve_entry(&self, playlist: &Path, root: &Path, raw: &str) -> Option<i64> {
        let folder = playlist.parent().unwrap_or(root);
        let spellings = spellings(raw);
        let mut paths = Vec::with_capacity(spellings.len() * 2);
        for location in &spellings {
            match location.anchor {
                Anchor::Relative => {
                    paths.push(join(folder, &location.parts));
                    paths.push(join(root, &location.parts));
                }
                Anchor::Absolute => paths.push(join(Path::new("/"), &location.parts)),
                Anchor::Foreign => {}
            }
        }
        for exact in [true, false] {
            if let Some(id) = paths.iter().find_map(|p| self.full_path(p, exact, root)) {
                return Some(id);
            }
        }
        for n in (1..=3).rev() {
            for location in &spellings {
                let parts: Vec<&String> = location
                    .parts
                    .iter()
                    .filter(|p| p.as_str() != "..")
                    .collect();
                if let Some(id) = self.trailing(&parts, n, root) {
                    return Some(id);
                }
            }
        }
        None
    }

    /// Resolve every entry of a playlist at `playlist` on media rooted at
    /// `root`. Unresolved entries are counted, never silently dropped.
    pub fn resolve<'a>(
        &self,
        playlist: &Path,
        root: &Path,
        locations: impl IntoIterator<Item = &'a str>,
    ) -> Resolved {
        let mut resolved = Resolved::default();
        for raw in locations {
            match self.resolve_entry(playlist, root, raw) {
                Some(id) => resolved.track_ids.push(id),
                None => resolved.unresolved = resolved.unresolved.saturating_add(1),
            }
        }
        resolved
    }
}

/// The name a playlist file shows under: its `#PLAYLIST:` name, else the
/// file name without extension.
pub fn display_name(path: &Path, parsed: &Parsed) -> String {
    parsed.name.clone().unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Playlist".into())
    })
}

/// Track ids stored as consecutive little-endian i64 values.
pub fn encode_ids(ids: &[i64]) -> Vec<u8> {
    ids.iter().flat_map(|id| id.to_le_bytes()).collect()
}

pub fn decode_ids(bytes: &[u8]) -> Vec<i64> {
    bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| i64::from_le_bytes(*c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locations(parsed: &Parsed) -> Vec<&str> {
        parsed.entries.iter().map(|e| e.location.as_str()).collect()
    }

    #[test]
    fn plain_m3u_lists_paths_and_skips_comments_and_blank_lines() {
        let parsed =
            parse(b"# a comment\n\nMusic/One.mp3\n  Music/Two.flac  \n#another\nThree.ogg");
        assert_eq!(
            locations(&parsed),
            ["Music/One.mp3", "Music/Two.flac", "Three.ogg"]
        );
        assert!(parsed.entries.iter().all(|e| e.title.is_none()));
        assert_eq!(parsed.name, None);
    }

    #[test]
    fn extended_m3u8_with_bom_crlf_and_extinf_titles() {
        let bytes = "\u{feff}#EXTM3U\r\n#PLAYLIST:Road Trip\r\n#EXTINF:215,Émilie - Lumière, Part 2\r\n../Music/Émilie/Lumière/01 Lumière.flac\r\n#EXTINF:-1,\r\n../Music/Other/02.flac\r\n\r\n";
        let parsed = parse(bytes.as_bytes());
        assert_eq!(parsed.name.as_deref(), Some("Road Trip"));
        assert_eq!(
            locations(&parsed),
            [
                "../Music/Émilie/Lumière/01 Lumière.flac",
                "../Music/Other/02.flac"
            ]
        );
        assert_eq!(
            parsed.entries[0].title.as_deref(),
            Some("Émilie - Lumière, Part 2")
        );
        assert_eq!(parsed.entries[1].title, None);
    }

    #[test]
    fn old_mac_line_endings_and_latin1_files_are_read() {
        let parsed = parse(b"#EXTM3U\rA.mp3\rB.mp3\r");
        assert_eq!(locations(&parsed), ["A.mp3", "B.mp3"]);
        // "Café.mp3" in Latin-1.
        let parsed = parse(b"Caf\xe9.mp3\n");
        assert_eq!(locations(&parsed), ["Café.mp3"]);
    }

    #[test]
    fn extinf_title_attaches_only_to_the_next_entry() {
        let parsed = parse(b"#EXTINF:10,First\n#EXTVLCOPT:x\nA.mp3\nB.mp3\n");
        assert_eq!(parsed.entries[0].title.as_deref(), Some("First"));
        assert_eq!(parsed.entries[1].title, None);
    }

    #[test]
    fn locations_understand_relative_absolute_backslash_windows_and_urls() {
        let l = location("..\\Music\\A\\B\\01.flac").unwrap();
        assert_eq!(l.anchor, Anchor::Relative);
        assert_eq!(l.parts, ["..", "Music", "A", "B", "01.flac"]);
        let l = location("/media/sd/Music/A/01.flac").unwrap();
        assert_eq!(l.anchor, Anchor::Absolute);
        let l = location("D:\\Users\\Owner\\Music\\A\\B\\01.flac").unwrap();
        assert_eq!(l.anchor, Anchor::Foreign);
        assert_eq!(l.parts.last().unwrap(), "01.flac");
        let l = location("\\\\NAS\\share\\Music\\A\\01.flac").unwrap();
        assert_eq!(l.anchor, Anchor::Foreign);
        let l = location("file:///C:/Music/My%20Band/Album/01%20Song.flac").unwrap();
        assert_eq!(l.anchor, Anchor::Foreign);
        assert_eq!(l.parts, ["Music", "My Band", "Album", "01 Song.flac"]);
        let l = location("file:///media/sd/Music/%C3%89milie/01.flac").unwrap();
        assert_eq!(l.anchor, Anchor::Absolute);
        assert_eq!(l.parts[3], "Émilie");
        assert!(location("http://radio.example/stream.mp3").is_none());
        assert_eq!(location("./x/./y.mp3").unwrap().parts, ["x", "y.mp3"]);
    }

    #[test]
    fn percent_escapes_are_decoded_only_when_complete_and_utf8() {
        assert_eq!(percent_decode("a%20b").as_deref(), Some("a b"));
        assert_eq!(percent_decode("100%"), None);
        assert_eq!(percent_decode("50%zz"), None);
        assert_eq!(percent_decode("%ff"), None);
        assert_eq!(spellings("My%20Band/01.flac").len(), 2);
    }

    fn index() -> LibraryIndex {
        LibraryIndex::new(vec![
            (1, "/media/sd/Music/Artist/Album/01 Track.flac".into()),
            (2, "/media/sd/Music/Artist/Album/02 Other.flac".into()),
            (3, "/media/sd/Music/My Band/Live/01 Intro.flac".into()),
            (4, "/media/sd/Music/Else/Best Of/01 Intro.flac".into()),
            (5, "/media/internal/music/Solo/Only/Unique Song.mp3".into()),
            (6, "/media/sd/Music/Dup/Same/Twin.flac".into()),
            (7, "/media/internal/music/Dup/Same/Twin.flac".into()),
            (8, "/media/sd/Music/Case/Exact.flac".into()),
            (9, "/media/sd/Music/case/exact.flac".into()),
        ])
    }

    fn sd(entry: &str) -> Option<i64> {
        index().resolve_entry(
            Path::new("/media/sd/Playlists/Mix.m3u8"),
            Path::new("/media/sd"),
            entry,
        )
    }

    #[test]
    fn relative_and_absolute_paths_resolve_exactly() {
        assert_eq!(sd("../Music/Artist/Album/01 Track.flac"), Some(1));
        assert_eq!(sd("..\\Music\\Artist\\Album\\02 Other.flac"), Some(2));
        assert_eq!(sd("/media/sd/Music/Artist/Album/02 Other.flac"), Some(2));
        assert_eq!(
            sd("/media/internal/music/Solo/Only/Unique Song.mp3"),
            Some(5)
        );
        // Relative to the media root, as some exporters write.
        assert_eq!(sd("Music/Artist/Album/01 Track.flac"), Some(1));
        assert_eq!(sd("../Music/My%20Band/Live/01%20Intro.flac"), Some(3));
    }

    #[test]
    fn case_differences_resolve_but_an_exact_spelling_wins() {
        assert_eq!(sd("../MUSIC/artist/ALBUM/01 track.FLAC"), Some(1));
        assert_eq!(sd("../Music/Case/Exact.flac"), Some(8));
        assert_eq!(sd("../Music/case/exact.flac"), Some(9));
        // Two case-insensitive matches and no exact one: ambiguous.
        assert_eq!(sd("../Music/CASE/EXACT.flac"), None);
    }

    #[test]
    fn portable_suffix_relocates_foreign_and_moved_paths() {
        assert_eq!(
            sd("D:\\Users\\Owner\\Music\\Artist\\Album\\01 Track.flac"),
            Some(1)
        );
        assert_eq!(sd("/mnt/sdcard/Music/Artist/Album/02 Other.flac"), Some(2));
        assert_eq!(sd("file:///C:/Old/Album/02%20Other.flac"), Some(2));
        // File name alone, when it is unique in the library.
        assert_eq!(sd("E:\\Unique Song.mp3"), Some(5));
        // A shared file name is ambiguous; the folder settles it.
        assert_eq!(sd("X:\\01 Intro.flac"), None);
        assert_eq!(sd("X:\\Live\\01 Intro.flac"), Some(3));
        // The same Artist/Album/file on both media: the playlist's media wins.
        assert_eq!(sd("Z:\\Dup\\Same\\Twin.flac"), Some(6));
        assert_eq!(
            index().resolve_entry(
                Path::new("/media/internal/music/list.m3u"),
                Path::new("/media/internal/music"),
                "Z:\\Dup\\Same\\Twin.flac"
            ),
            Some(7)
        );
    }

    #[test]
    fn missing_files_and_streams_are_counted_not_dropped() {
        let index = index();
        let resolved = index.resolve(
            Path::new("/media/sd/Playlists/Mix.m3u8"),
            Path::new("/media/sd"),
            [
                "../Music/Artist/Album/01 Track.flac",
                "../Music/Nobody/Nothing/99.flac",
                "http://radio.example/live",
                "../Music/Artist/Album/01 Track.flac",
            ],
        );
        assert_eq!(resolved.track_ids, [1, 1]);
        assert_eq!(resolved.unresolved, 2);
    }

    #[test]
    fn index_is_built_once_and_answers_large_playlists_by_lookup() {
        let paths: Vec<(i64, String)> = (0..12_000)
            .map(|i| {
                (
                    i,
                    format!(
                        "/media/sd/Music/Artist {}/Album {}/{i:05} Song.flac",
                        i / 100,
                        i / 10
                    ),
                )
            })
            .collect();
        let index = LibraryIndex::new(paths);
        assert_eq!(index.len(), 12_000);
        let entries: Vec<String> = (0..12_000)
            .rev()
            .map(|i| {
                format!(
                    "..\\music\\ARTIST {}\\album {}\\{i:05} song.flac",
                    i / 100,
                    i / 10
                )
            })
            .collect();
        let started = std::time::Instant::now();
        let resolved = index.resolve(
            Path::new("/media/sd/Playlists/All.m3u8"),
            Path::new("/media/sd"),
            entries.iter().map(String::as_str),
        );
        assert_eq!(resolved.unresolved, 0);
        assert_eq!(resolved.track_ids.first(), Some(&11_999));
        // Generous bound even for debug builds; a library walk per entry
        // (144 million comparisons) would not come close.
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn ids_round_trip_through_their_stored_form() {
        let ids = [1, -2, i64::MAX, 7, 7];
        assert_eq!(decode_ids(&encode_ids(&ids)), ids);
    }

    #[test]
    fn playlist_files_are_recognised_by_extension() {
        assert!(is_playlist(Path::new("/x/Road Trip.m3u8")));
        assert!(is_playlist(Path::new("/x/OLD.M3U")));
        assert!(!is_playlist(Path::new("/x/song.mp3")));
        let parsed = parse(b"a.mp3");
        assert_eq!(
            display_name(Path::new("/x/Road Trip.m3u8"), &parsed),
            "Road Trip"
        );
    }
}
