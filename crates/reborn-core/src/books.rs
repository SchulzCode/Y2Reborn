//! Audiobooks: which files are books, how they form books and in what order
//! their parts play.
//!
//! A file is a book when a folder named Audiobooks or Hörbücher (any case,
//! anywhere in its path) holds it, when it is an `.m4b`, or when its genre
//! says Audiobook or Hörbuch. Nothing here is stored: it follows from the
//! path and the tags, so a rule change needs no rescan.
//!
//! A book is the file's folder. Folders such as `CD 2` or `Part 1` fold into
//! the book above them, never past the first folder below the Audiobooks
//! folder (`Audiobooks/Part 1/` stays a book). `.m4b` files are whole books:
//! in one folder they are separate books unless they share an album tag
//! (`Book Part 1.m4b`, `Book Part 2.m4b`). A file lying directly in the
//! Audiobooks folder is a book of its own.
use crate::{sort::fold, Track};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::HashMap, path::Path};

/// Folder names (folded) that hold audiobooks.
const FOLDERS: [&str; 7] = [
    "audiobooks",
    "audiobook",
    "audio books",
    "horbucher",
    "horbuch",
    "hoerbuecher",
    "hoerbuch",
];
/// Genres (folded) that mark a file as a book.
const GENRES: [&str; 6] = [
    "audiobook",
    "audiobooks",
    "audio book",
    "horbuch",
    "horbucher",
    "hoerbuch",
];

fn is_m4b(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("m4b"))
}

/// Where the Audiobooks folder is among `path`'s folders (component index).
fn marker(path: &Path) -> Option<usize> {
    let parent = path.parent()?;
    parent
        .components()
        .position(|c| FOLDERS.contains(&fold(&c.as_os_str().to_string_lossy()).as_str()))
}

/// `Disc 2`, `CD1`, `Part 3`, `Vol. 1`, `Side A` and the like.
pub(crate) fn subdivision(name: &str) -> bool {
    let folded = fold(name);
    let mut words = folded.split(' ');
    let (Some(word), rest) = (words.next(), words.collect::<Vec<_>>()) else {
        return false;
    };
    const NAMES: [&str; 11] = [
        "disc", "disk", "cd", "part", "pt", "vol", "volume", "tape", "side", "teil", "tl",
    ];
    // "cd1" or "cd 1": a name, then a number (or a side letter).
    let (name, number) = match word.find(|c: char| c.is_ascii_digit()) {
        Some(at) if rest.is_empty() => (&word[..at], word[at..].to_string()),
        _ if rest.len() == 1 => (word, rest[0].to_string()),
        _ => return false,
    };
    NAMES.contains(&name)
        && (number.chars().all(|c| c.is_ascii_digit()) && !number.is_empty()
            || (name == "side" && number.len() == 1))
}

/// Whether `track` is a book (computed once when the library loads).
pub fn is_book(track: &Track) -> bool {
    !track.is_live()
        && (marker(&track.path).is_some()
            || is_m4b(&track.path)
            || GENRES.contains(&fold(&track.genre).as_str()))
}

/// The book a file belongs to: its stable key, the folder it is named after
/// and whether the file alone is the book (then its title names it).
fn identity(track: &Track) -> (String, std::path::PathBuf, bool) {
    let parent = track.path.parent().unwrap_or(Path::new("")).to_path_buf();
    let mark = marker(&track.path);
    let depth = parent.components().count();
    // Directly in the Audiobooks folder: a book of its own.
    if mark.is_some_and(|m| m + 1 == depth) && !is_m4b(&track.path) {
        return (
            format!("{}|{}", track.source_id, track.path.display()),
            parent,
            true,
        );
    }
    // Fold disc folders into the book, never above the first folder below
    // the Audiobooks folder.
    let floor = mark.map_or(1, |m| m + 2);
    let mut dir = parent;
    while dir.components().count() > floor
        && dir
            .file_name()
            .is_some_and(|n| subdivision(&n.to_string_lossy()))
    {
        dir.pop();
    }
    let mut key = format!("{}|{}", track.source_id, dir.display());
    let mut alone = false;
    if is_m4b(&track.path) {
        let album = fold(&track.album);
        if album.is_empty() {
            key.push('|');
            key.push_str(&track.filename);
            alone = true;
        } else {
            key.push_str("|album:");
            key.push_str(&album);
        }
    }
    (key, dir, alone)
}

/// One audiobook: its parts in play order, as indexes into the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Book {
    /// Stable identity (source, folder, and for `.m4b` the album or file).
    pub key: String,
    pub title: String,
    /// The album artist or artist all parts share, else an author folder.
    pub author: String,
    pub parts: Vec<usize>,
    /// Sum of the parts; 0 when any part's length is unknown.
    pub duration_ms: u64,
    /// A part with a cover, for the book's picture.
    pub art: Option<usize>,
}

/// The value every item shares, if there is one and it is not empty.
fn shared<'a>(mut values: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let first = values.next()?.trim();
    (!first.is_empty() && values.all(|v| v.trim() == first)).then_some(first)
}

/// Every book in `tracks` (only online books), A to Z by title.
pub fn books(tracks: &[Track]) -> Vec<Book> {
    let mut groups: HashMap<String, (std::path::PathBuf, bool, Vec<usize>)> = HashMap::new();
    for (i, t) in tracks.iter().enumerate() {
        if t.book && t.online {
            let (key, dir, alone) = identity(t);
            groups
                .entry(key)
                .or_insert_with(|| (dir, alone, vec![]))
                .2
                .push(i);
        }
    }
    let mut books: Vec<Book> = groups
        .into_iter()
        .map(|(key, (dir, alone, mut parts))| {
            order(tracks, &mut parts);
            let all = || parts.iter().map(|&i| &tracks[i]);
            let title = shared(all().map(|t| t.album.as_str()))
                .map(str::to_owned)
                .or_else(|| alone.then(|| tracks[parts[0]].title.clone()))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| {
                    dir.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
            let author = shared(all().map(|t| t.album_artist.as_str()))
                .or_else(|| shared(all().map(|t| t.artist.as_str())))
                .map(str::to_owned)
                .unwrap_or_else(|| author_folder(&tracks[parts[0]].path, &dir));
            let duration_ms = if all().any(|t| t.duration_ms == 0) {
                0
            } else {
                all().map(|t| t.duration_ms).sum()
            };
            let art = parts.iter().copied().find(|&i| tracks[i].artwork);
            Book {
                key,
                title,
                author,
                parts,
                duration_ms,
                art,
            }
        })
        .collect();
    books.sort_by_cached_key(|b| (fold(&b.title), fold(&b.author), b.key.clone()));
    books
}

/// `Audiobooks/<Author>/<Book>`: the folder between, when there is one.
fn author_folder(path: &Path, book: &Path) -> String {
    let Some(m) = marker(path) else {
        return String::new();
    };
    let names: Vec<_> = book.components().collect();
    if names.len() == m + 3 {
        names[m + 1].as_os_str().to_string_lossy().into_owned()
    } else {
        String::new()
    }
}

/// Play order: disc, folder, track number (only when every part has one),
/// then file name with numbers compared as numbers.
pub fn order(tracks: &[Track], parts: &mut [usize]) {
    let numbered = parts.iter().all(|&i| tracks[i].track > 0);
    parts.sort_by(|&a, &b| {
        let (x, y) = (&tracks[a], &tracks[b]);
        x.disc
            .cmp(&y.disc)
            .then_with(|| {
                natural(
                    &x.path.parent().unwrap_or(Path::new("")).to_string_lossy(),
                    &y.path.parent().unwrap_or(Path::new("")).to_string_lossy(),
                )
            })
            .then_with(|| {
                if numbered {
                    x.track.cmp(&y.track)
                } else {
                    Ordering::Equal
                }
            })
            .then_with(|| natural(&x.filename, &y.filename))
            .then_with(|| x.id.cmp(&y.id))
    });
}

/// Compare with runs of digits as numbers ("2" before "10"), ignoring case.
pub fn natural(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |s: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut n = String::new();
                    while let Some(c) = s.peek().copied().filter(char::is_ascii_digit) {
                        n.push(c);
                        s.next();
                    }
                    n
                };
                let (n, m) = (take(&mut a), take(&mut b));
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let ord = n.len().cmp(&m.len()).then_with(|| n.cmp(m));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

// ------------------------------------------------------------- listening

/// A chapter mark inside a file, as the media layer reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterMark {
    pub start_ms: u64,
    pub end_ms: u64,
    pub title: String,
}

/// One chapter of a book: which part (file) holds it and where in that part.
/// `end_ms` 0 means "to the part's end".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub part: usize,
    pub start_ms: u64,
    pub end_ms: u64,
    pub title: String,
}

/// The chapters of a book: a part's own chapter marks when it has any, else
/// the part itself. `marks[i]` belongs to `parts[i]`.
pub fn spans(parts: &[&Track], marks: &[Vec<ChapterMark>]) -> Vec<Span> {
    let mut out = vec![];
    for (i, track) in parts.iter().enumerate() {
        let number = out.len();
        match marks.get(i).filter(|m| !m.is_empty()) {
            Some(marks) => out.extend(marks.iter().enumerate().map(|(n, m)| Span {
                part: i,
                start_ms: if n == 0 { 0 } else { m.start_ms },
                end_ms: if n + 1 == marks.len() { 0 } else { m.end_ms },
                title: if m.title.trim().is_empty() {
                    format!("Chapter {}", number + n + 1)
                } else {
                    m.title.trim().to_owned()
                },
            })),
            None => out.push(Span {
                part: i,
                start_ms: 0,
                end_ms: 0,
                title: track.title.clone(),
            }),
        }
    }
    out
}

/// The speeds a book can play at, in percent; one press on Speed steps on.
pub const SPEEDS: [u16; 7] = [75, 100, 110, 125, 150, 175, 200];
/// The speed after `speed` (as a factor) in `SPEEDS`, round to 0.75.
pub fn next_speed(speed: f32) -> u16 {
    let now = (speed * 100.).round() as u16;
    SPEEDS
        .iter()
        .copied()
        .find(|&s| s > now)
        .unwrap_or(SPEEDS[0])
}

/// Previous restarts the chapter after this long in it, else goes back one.
pub const RESTART_AFTER_MS: u64 = 5_000;

/// The book being listened to: its parts (by path, so songs added to the
/// queue never confuse it) and its chapters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Listening {
    pub key: String,
    pub title: String,
    pub author: String,
    pub parts: Vec<std::path::PathBuf>,
    /// Lengths of the parts (0 unknown), for book time and percent.
    pub part_ms: Vec<u64>,
    pub chapters: Vec<Span>,
    /// Playback speed of this book (1.0 normal).
    #[serde(default = "normal_speed")]
    pub speed: f32,
}
fn normal_speed() -> f32 {
    1.0
}
impl Listening {
    pub fn new(book: &Book, tracks: &[Track], speed: f32) -> Self {
        let parts: Vec<&Track> = book.parts.iter().map(|&i| &tracks[i]).collect();
        Self {
            key: book.key.clone(),
            title: book.title.clone(),
            author: book.author.clone(),
            parts: parts.iter().map(|t| t.path.clone()).collect(),
            part_ms: parts.iter().map(|t| t.duration_ms).collect(),
            chapters: spans(&parts, &[]),
            speed,
        }
    }
    /// Which part `track` is, if it belongs to this book.
    pub fn part_of(&self, track: &Track) -> Option<usize> {
        self.parts.iter().position(|p| *p == track.path)
    }
    fn end_of(&self, span: &Span) -> u64 {
        if span.end_ms > 0 {
            span.end_ms
        } else {
            self.part_ms.get(span.part).copied().unwrap_or(0)
        }
    }
    /// The chapter playing at `position_ms` of `part`.
    pub fn chapter_at(&self, part: usize, position_ms: u64) -> Option<usize> {
        self.chapters
            .iter()
            .rposition(|c| c.part == part && c.start_ms <= position_ms)
            .or_else(|| self.chapters.iter().position(|c| c.part == part))
    }
    /// Chapter `index`'s start, its length and how far `position_ms` is in it.
    pub fn chapter_span(&self, index: usize) -> Option<(u64, u64)> {
        let c = self.chapters.get(index)?;
        Some((c.start_ms, self.end_of(c).saturating_sub(c.start_ms)))
    }
    /// Where Next goes: the start of the following chapter, `None` at the end.
    pub fn next(&self, part: usize, position_ms: u64) -> Option<(usize, u64)> {
        let at = self.chapter_at(part, position_ms)?;
        self.chapters.get(at + 1).map(|c| (c.part, c.start_ms))
    }
    /// Where Previous goes: this chapter's start after `RESTART_AFTER_MS` in
    /// it (or in the first chapter), else the previous chapter's start.
    pub fn previous(&self, part: usize, position_ms: u64) -> Option<(usize, u64)> {
        let at = self.chapter_at(part, position_ms)?;
        let c = &self.chapters[at];
        if at == 0 || position_ms.saturating_sub(c.start_ms) > RESTART_AFTER_MS {
            return Some((c.part, c.start_ms));
        }
        let p = &self.chapters[at - 1];
        Some((p.part, p.start_ms))
    }
    /// Milliseconds of the book before `part` plus `position_ms`; with the
    /// whole length, `None` when a part's length is unknown.
    pub fn progress(&self, part: usize, position_ms: u64) -> Option<(u64, u64)> {
        if self.part_ms.contains(&0) {
            return None;
        }
        let before: u64 = self.part_ms.iter().take(part).sum();
        Some((before + position_ms, self.part_ms.iter().sum()))
    }
}

// --------------------------------------------------------------- progress

/// Where a book was left. The part is kept by path and by name + size, so
/// a renamed or moved folder finds its place again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub part_path: std::path::PathBuf,
    pub part_name: String,
    pub part_size: u64,
    pub position_ms: u64,
    /// Unix seconds of the last listening.
    pub updated: i64,
    #[serde(default)]
    pub finished: bool,
    #[serde(default = "normal_speed")]
    pub speed: f32,
}

/// Every book's progress, by book key (`state/books.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Shelf {
    #[serde(default)]
    pub books: std::collections::BTreeMap<String, Progress>,
}

/// Rewind on resume: a little after a short pause, more after a long one.
pub fn rewind_ms(paused_seconds: i64) -> u64 {
    match paused_seconds {
        ..600 => 5_000,
        600..86_400 => 15_000,
        _ => 30_000,
    }
}

impl Shelf {
    /// This book's progress and the key it is stored under: its own key, or
    /// a record whose part is one of the book's files (by name and size).
    pub fn find<'a>(&'a self, book: &Book, tracks: &[Track]) -> Option<(&'a str, &'a Progress)> {
        if let Some((key, p)) = self.books.get_key_value(&book.key) {
            return Some((key.as_str(), p));
        }
        self.books.iter().find_map(|(key, p)| {
            book.parts
                .iter()
                .any(|&i| tracks[i].filename == p.part_name && tracks[i].size == p.part_size)
                .then_some((key.as_str(), p))
        })
    }
    /// Remember `book` at `part` (an index into its parts), `position_ms`.
    /// The first seconds of a book are not worth a place; a renamed book's
    /// old record moves to its new key.
    pub fn record(
        &mut self,
        book: &Book,
        tracks: &[Track],
        part: usize,
        position_ms: u64,
        speed: f32,
        now: i64,
    ) {
        let old = self.find(book, tracks).map(|(k, _)| k.to_owned());
        let started = part > 0 || position_ms >= 10_000;
        // Saving a place that did not move (a pause, a shutdown) keeps the
        // time it was last listened at: the rewind and the order follow it.
        let kept_time = old
            .as_ref()
            .and_then(|k| self.books.get(k))
            .filter(|p| {
                book.parts
                    .get(part)
                    .is_some_and(|&i| tracks[i].path == p.part_path)
                    && p.position_ms == position_ms
                    && (p.speed - speed).abs() < 0.001
            })
            .map(|p| p.updated);
        let finished = old
            .as_ref()
            .and_then(|k| self.books.get(k))
            .is_some_and(|p| p.finished && part == 0 && position_ms < 10_000);
        if let Some(old) = old {
            self.books.remove(&old);
        }
        if !started && !finished {
            return;
        }
        let Some(&index) = book.parts.get(part) else {
            return;
        };
        let t = &tracks[index];
        self.books.insert(
            book.key.clone(),
            Progress {
                part_path: t.path.clone(),
                part_name: t.filename.clone(),
                part_size: t.size,
                position_ms,
                updated: kept_time.unwrap_or(now),
                finished,
                speed,
            },
        );
    }
    /// Mark `book` finished (or not): finished books start over.
    pub fn set_finished(&mut self, book: &Book, tracks: &[Track], finished: bool, now: i64) {
        let speed = self.find(book, tracks).map_or(1.0, |(_, p)| p.speed);
        self.forget(book, tracks);
        if finished {
            let t = &tracks[book.parts[0]];
            self.books.insert(
                book.key.clone(),
                Progress {
                    part_path: t.path.clone(),
                    part_name: t.filename.clone(),
                    part_size: t.size,
                    position_ms: 0,
                    updated: now,
                    finished: true,
                    speed,
                },
            );
        }
    }
    pub fn forget(&mut self, book: &Book, tracks: &[Track]) {
        if let Some(key) = self.find(book, tracks).map(|(k, _)| k.to_owned()) {
            self.books.remove(&key);
        }
    }
    /// Where `book` continues: (part index, position) with the rewind, the
    /// start for a new or finished book. A place within 5 s of a part's end
    /// continues at the next part.
    pub fn resume(&self, book: &Book, tracks: &[Track], now: i64) -> (usize, u64) {
        let Some((_, p)) = self.find(book, tracks).filter(|(_, p)| !p.finished) else {
            return (0, 0);
        };
        let Some(part) = book.parts.iter().position(|&i| {
            tracks[i].path == p.part_path
                || (tracks[i].filename == p.part_name && tracks[i].size == p.part_size)
        }) else {
            return (0, 0);
        };
        let length = tracks[book.parts[part]].duration_ms;
        if length > 0 && p.position_ms + 5_000 >= length && part + 1 < book.parts.len() {
            return (part + 1, 0);
        }
        let back = rewind_ms(now - p.updated);
        (part, p.position_ms.saturating_sub(back))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> Track {
        let mut t = Track {
            id: path.len() as i64,
            source_id: "internal".into(),
            path: path.into(),
            filename: Path::new(path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            title: Path::new(path)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            duration_ms: 60_000,
            online: true,
            ..Default::default()
        };
        t.book = is_book(&t);
        t
    }
    fn keys(tracks: &[Track]) -> Vec<(String, Vec<String>)> {
        books(tracks)
            .into_iter()
            .map(|b| {
                (
                    b.title,
                    b.parts
                        .iter()
                        .map(|&i| tracks[i].filename.clone())
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn folders_m4b_and_genre_make_books_and_music_stays_music() {
        assert!(file("/m/Audiobooks/Dune/01.mp3").book);
        assert!(
            file("/m/Media/HÖRBÜCHER/Momo/01.mp3").book,
            "any case, umlauts"
        );
        assert!(file("/m/Hoerbuecher/Momo/01.mp3").book);
        assert!(file("/m/Music/Book.M4B").book);
        assert!(!file("/m/Music/Song.m4a").book);
        assert!(
            !file("/m/MyAudiobooks/x.mp3").book,
            "a word inside a name is not the folder"
        );
        assert!(
            !file("/m/Music/Audiobooks.mp3").book,
            "a file name is not the folder"
        );
        let mut tagged = file("/m/Music/Talk/01.mp3");
        tagged.genre = "Hörbuch".into();
        assert!(is_book(&tagged));
        let mut radio = Track::radio(&Default::default());
        radio.path = "/Audiobooks/stream".into();
        assert!(!is_book(&radio));
    }

    #[test]
    fn disc_folders_fold_into_the_book_and_author_folders_stay() {
        let tracks = [
            file("/m/Audiobooks/Frank Herbert/Dune/CD 2/01.mp3"),
            file("/m/Audiobooks/Frank Herbert/Dune/CD 1/02.mp3"),
            file("/m/Audiobooks/Frank Herbert/Dune/CD 1/01.mp3"),
            file("/m/Audiobooks/Frank Herbert/Dune/CD 10/01.mp3"),
            file("/m/Audiobooks/Part 1/a.mp3"),
            file("/m/Audiobooks/Loose.mp3"),
        ];
        let books = books(&tracks);
        let dune = books.iter().find(|b| b.title == "Dune").unwrap();
        assert_eq!(
            dune.author, "Frank Herbert",
            "the author folder names the author"
        );
        let order: Vec<_> = dune.parts.iter().map(|&i| tracks[i].path.clone()).collect();
        assert_eq!(
            order,
            [
                "/m/Audiobooks/Frank Herbert/Dune/CD 1/01.mp3",
                "/m/Audiobooks/Frank Herbert/Dune/CD 1/02.mp3",
                "/m/Audiobooks/Frank Herbert/Dune/CD 2/01.mp3",
                "/m/Audiobooks/Frank Herbert/Dune/CD 10/01.mp3",
            ]
            .map(std::path::PathBuf::from)
        );
        assert_eq!(dune.duration_ms, 4 * 60_000);
        assert!(
            books.iter().any(|b| b.title == "Part 1"),
            "a book named like a part"
        );
        assert!(
            books.iter().any(|b| b.title == "Loose"),
            "a loose file is a book"
        );
        assert_eq!(books.len(), 3);
    }

    #[test]
    fn m4b_files_are_whole_books_unless_they_share_an_album() {
        let mut tracks = vec![
            file("/m/Audiobooks/Tolkien/Hobbit.m4b"),
            file("/m/Audiobooks/Tolkien/Return of the King.m4b"),
            file("/m/Music/Book Part 2.m4b"),
            file("/m/Music/Book Part 1.m4b"),
        ];
        tracks[2].album = "The Book".into();
        tracks[3].album = "The Book".into();
        assert_eq!(
            keys(&tracks),
            [
                ("Hobbit".to_string(), vec!["Hobbit.m4b".to_string()]),
                (
                    "Return of the King".to_string(),
                    vec!["Return of the King.m4b".to_string()]
                ),
                (
                    "The Book".to_string(),
                    vec!["Book Part 1.m4b".to_string(), "Book Part 2.m4b".to_string()]
                ),
            ]
        );
    }

    #[test]
    fn tags_name_the_book_and_order_follows_disc_then_track_then_name() {
        let mut tracks = vec![
            file("/m/Audiobooks/x/b.mp3"),
            file("/m/Audiobooks/x/a.mp3"),
            file("/m/Audiobooks/x/c.mp3"),
        ];
        for (t, (n, d)) in tracks.iter_mut().zip([(1, 2), (2, 1), (1, 1)]) {
            t.album = "Momo".into();
            t.album_artist = "Michael Ende".into();
            t.track = n;
            t.disc = d;
        }
        let book = &books(&tracks)[0];
        assert_eq!(
            (book.title.as_str(), book.author.as_str()),
            ("Momo", "Michael Ende")
        );
        let names: Vec<_> = book
            .parts
            .iter()
            .map(|&i| tracks[i].filename.as_str())
            .collect();
        assert_eq!(names, ["c.mp3", "a.mp3", "b.mp3"]);
        // One part without a number: names decide, naturally.
        tracks[0].track = 0;
        tracks[0].disc = 0;
        tracks[1].disc = 0;
        tracks[2].disc = 0;
        let mut parts = vec![0, 1, 2];
        order(&tracks, &mut parts);
        assert_eq!(parts, [1, 0, 2]);
        // An unknown length leaves the book's length unknown.
        tracks[1].duration_ms = 0;
        assert_eq!(books(&tracks)[0].duration_ms, 0);
    }

    fn mark(start_ms: u64, end_ms: u64, title: &str) -> ChapterMark {
        ChapterMark {
            start_ms,
            end_ms,
            title: title.into(),
        }
    }

    #[test]
    fn chapters_come_from_marks_or_from_the_parts() {
        let a = file("/m/Audiobooks/B/1.mp3");
        let b = file("/m/Audiobooks/B/2.m4b");
        let spans = spans(
            &[&a, &b],
            &[
                vec![],
                vec![mark(0, 20_000, "Intro"), mark(20_000, 60_000, " ")],
            ],
        );
        let shown: Vec<_> = spans
            .iter()
            .map(|s| (s.part, s.start_ms, s.end_ms, s.title.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (0, 0, 0, "1"),
                (1, 0, 20_000, "Intro"),
                (1, 20_000, 0, "Chapter 3")
            ],
            "a part without marks is one chapter; the last mark runs to the end"
        );
    }

    #[test]
    fn next_and_previous_move_by_chapter_and_previous_restarts_after_five_seconds() {
        let tracks = vec![file("/m/Audiobooks/B/1.mp3"), file("/m/Audiobooks/B/2.mp3")];
        let book = &books(&tracks)[0];
        let mut l = Listening::new(book, &tracks, 1.0);
        let parts: Vec<&Track> = book.parts.iter().map(|&i| &tracks[i]).collect();
        l.chapters = spans(
            &parts,
            &[
                vec![mark(0, 30_000, "One"), mark(30_000, 60_000, "Two")],
                vec![],
            ],
        );
        assert_eq!(l.chapter_at(0, 31_000), Some(1));
        assert_eq!(l.next(0, 1_000), Some((0, 30_000)), "same file: a seek");
        assert_eq!(l.next(0, 31_000), Some((1, 0)), "into the next file");
        assert_eq!(l.next(1, 5_000), None, "the end stays where it is");
        assert_eq!(
            l.previous(0, 40_000),
            Some((0, 30_000)),
            "restart after 5 s"
        );
        assert_eq!(
            l.previous(0, 33_000),
            Some((0, 0)),
            "early: the chapter before"
        );
        assert_eq!(
            l.previous(0, 2_000),
            Some((0, 0)),
            "the first chapter restarts"
        );
        assert_eq!(l.previous(1, 1_000), Some((0, 30_000)));
        assert_eq!(l.chapter_span(1), Some((30_000, 30_000)));
        assert_eq!(l.progress(1, 30_000), Some((90_000, 120_000)));
    }

    #[test]
    fn progress_is_kept_rewound_and_found_again_after_a_rename() {
        let mut tracks = vec![file("/m/Audiobooks/B/1.mp3"), file("/m/Audiobooks/B/2.mp3")];
        for (t, size) in tracks.iter_mut().zip([100, 200]) {
            t.size = size;
        }
        let book = books(&tracks).remove(0);
        let mut shelf = Shelf::default();
        shelf.record(&book, &tracks, 0, 4_000, 1.0, 1_000);
        assert!(shelf.books.is_empty(), "the first seconds are no place");
        shelf.record(&book, &tracks, 1, 40_000, 1.25, 1_000);
        // Saved again unmoved (a shutdown while paused): the time stays.
        shelf.record(&book, &tracks, 1, 40_000, 1.25, 1_900);
        assert_eq!(shelf.books[&book.key].updated, 1_000);
        assert_eq!(shelf.resume(&book, &tracks, 1_060), (1, 35_000), "5 s back");
        assert_eq!(shelf.resume(&book, &tracks, 1_000 + 3_600), (1, 25_000));
        assert_eq!(shelf.resume(&book, &tracks, 1_000 + 200_000), (1, 10_000));
        // A place at a part's very end continues with the next part.
        shelf.record(&book, &tracks, 0, 58_000, 1.0, 1_000);
        assert_eq!(shelf.resume(&book, &tracks, 1_001), (1, 0));
        // The folder is renamed: same files, new key and paths.
        let mut moved = tracks.clone();
        for t in &mut moved {
            t.path = std::path::PathBuf::from(
                t.path.to_string_lossy().replace("/B/", "/Book B/").as_ref() as &str,
            );
        }
        let renamed = books(&moved).remove(0);
        assert_ne!(renamed.key, book.key);
        assert_eq!(shelf.resume(&renamed, &moved, 1_001), (1, 0));
        shelf.record(&renamed, &moved, 1, 20_000, 1.0, 2_000);
        assert_eq!(shelf.books.len(), 1, "the record moved to the new key");
        assert!(shelf.books.contains_key(&renamed.key));
        // Finished books start over; a few seconds in, still finished.
        shelf.set_finished(&renamed, &moved, true, 3_000);
        assert_eq!(shelf.resume(&renamed, &moved, 3_001), (0, 0));
        shelf.record(&renamed, &moved, 0, 3_000, 1.0, 3_010);
        assert!(shelf.books[&renamed.key].finished);
        shelf.record(&renamed, &moved, 0, 30_000, 1.0, 3_100);
        assert!(
            !shelf.books[&renamed.key].finished,
            "listening again unfinishes it"
        );
        shelf.forget(&renamed, &moved);
        assert!(shelf.books.is_empty());
    }

    #[test]
    fn speed_steps_round_from_three_quarters_to_double() {
        assert_eq!(next_speed(1.0), 110);
        assert_eq!(next_speed(1.25), 150);
        assert_eq!(next_speed(2.0), 75, "round to the slowest");
        assert_eq!(next_speed(0.75), 100);
    }

    #[test]
    fn natural_order_reads_numbers_as_numbers() {
        assert_eq!(natural("Chapter 2", "chapter 10"), Ordering::Less);
        assert_eq!(natural("01", "1"), Ordering::Equal);
        assert_eq!(natural("b", "A"), Ordering::Greater);
        assert!(subdivision("CD1") && subdivision("Disc 02") && subdivision("Part 3"));
        assert!(subdivision("Side A") && subdivision("Teil 2"));
        assert!(!subdivision("Discovery") && !subdivision("Part of Me") && !subdivision("CD"));
    }
}
