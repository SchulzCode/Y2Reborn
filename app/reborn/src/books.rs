//! Audiobooks in the player: where every book was left (`state/books.json`,
//! apart from the session so a library rebuild keeps it) and a background
//! reader for the chapter marks inside book files.
use crate::Runtime;
use reborn_core::{
    books::{self, Book, ChapterMark, Shelf},
    BookStart, PlaybackState, Track,
};
use reborn_media::{Cancel, MediaFile};
use reborn_observability::Level;
use serde_json::json;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc::{channel, Receiver, Sender},
    time::Duration,
};

/// A pause shorter than this continues in place, without a rewind.
const REWIND_AFTER: Duration = Duration::from_secs(30);

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The step of a held seek inside a book (one every 90 ms): ten seconds at
/// first, then half a minute, then a minute; an hour takes about a minute.
pub fn seek_step(held: Duration) -> u64 {
    if held < Duration::from_millis(1500) {
        10_000
    } else if held < Duration::from_secs(4) {
        30_000
    } else {
        60_000
    }
}

impl Runtime {
    pub(crate) fn book_by_key(&self, key: &str) -> Option<Book> {
        books::books(&self.model.library.tracks)
            .into_iter()
            .find(|b| b.key == key)
    }

    /// Remember where the playing book is (written with the next checkpoint).
    pub(crate) fn save_book(&mut self) {
        let Some((listening, _)) = self.model.book_part() else {
            return;
        };
        let (key, speed) = (listening.key.clone(), listening.speed);
        let Some(current) = self.model.current().map(|t| t.path.clone()) else {
            return;
        };
        let position = self.model.position_ms;
        let Some(book) = self.book_by_key(&key) else {
            return;
        };
        let library = &mut self.model.library;
        let Some(part) = book
            .parts
            .iter()
            .position(|&i| library.tracks[i].path == current)
        else {
            return;
        };
        library
            .shelf
            .record(&book, &library.tracks, part, position, speed, now());
    }

    /// Write `state/books.json` when the shelf changed.
    pub(crate) fn write_shelf(&mut self) {
        if self.model.library.shelf == self.shelf_written {
            return;
        }
        match save_shelf(
            &self.root.join("state/books.json"),
            &self.model.library.shelf,
        ) {
            Ok(()) => self.shelf_written = self.model.library.shelf.clone(),
            Err(error) => self.log.emit(
                Level::Warn,
                "books",
                "shelf_write_failed",
                "Audiobook progress could not be saved",
                None,
                json!({"error":error}),
            ),
        }
    }

    fn marks(&self, book: &Book) -> Vec<Vec<ChapterMark>> {
        let library = &self.model.library;
        book.parts
            .iter()
            .map(|&i| {
                library
                    .chapters
                    .get(&library.tracks[i].path)
                    .cloned()
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Play a book: where it was left, from the beginning or at a chapter.
    /// Shuffle, repeat and crossfade never apply to it; the music settings
    /// stay as they are.
    pub(crate) fn start_book(&mut self, key: &str, start: BookStart) -> Result<(), String> {
        self.save_book();
        let book = self.book_by_key(key).ok_or("This book is not available")?;
        let marks = self.marks(&book);
        let library = &self.model.library;
        let parts: Vec<&Track> = book.parts.iter().map(|&i| &library.tracks[i]).collect();
        let speed = library
            .shelf
            .find(&book, &library.tracks)
            .map_or(1.0, |(_, p)| p.speed);
        let mut listening = books::Listening::new(&book, &library.tracks, speed);
        listening.chapters = books::spans(&parts, &marks);
        let (part, position) = match start {
            BookStart::Resume => library.shelf.resume(&book, &library.tracks, now()),
            BookStart::Beginning => (0, 0),
            BookStart::Chapter(n) => listening
                .chapters
                .get(n)
                .map_or((0, 0), |c| (c.part, c.start_ms)),
        };
        let queue: Vec<Track> = parts.iter().map(|t| (*t).clone()).collect();
        self.model.replace_queue(queue, part)?;
        self.model.set_source(book.title.clone(), None);
        self.model.position_ms = position;
        self.model.book = Some(listening);
        self.paused_at = None;
        self.request_chapters(&book);
        self.load()
    }

    /// Ask the reader for the chapter marks of `book`'s files not read yet.
    pub(crate) fn request_chapters(&mut self, book: &Book) {
        let library = &self.model.library;
        let paths: Vec<PathBuf> = book
            .parts
            .iter()
            .map(|&i| library.tracks[i].path.clone())
            .filter(|p| !library.chapters.contains_key(p))
            .filter(|p| self.chapters_asked.insert(p.clone()))
            .collect();
        if let Some(reader) = &self.chapter_reader {
            reader.want(paths);
        }
    }

    /// Take the chapter marks read meanwhile; the playing book's chapters
    /// follow them.
    pub(crate) fn receive_chapters(&mut self) {
        let Some(reader) = &self.chapter_reader else {
            return;
        };
        let mut got = false;
        while let Ok((path, marks)) = reader.results.try_recv() {
            self.model.library.chapters.insert(path, marks);
            got = true;
        }
        if !got {
            return;
        }
        if let Some(listening) = &self.model.book {
            let parts: Vec<&Track> = listening
                .parts
                .iter()
                .filter_map(|p| self.model.queue.iter().find(|t| &t.path == p))
                .collect();
            if parts.len() == listening.parts.len() {
                let marks: Vec<_> = parts
                    .iter()
                    .map(|t| {
                        self.model
                            .library
                            .chapters
                            .get(&t.path)
                            .cloned()
                            .unwrap_or_default()
                    })
                    .collect();
                let chapters = books::spans(&parts, &marks);
                if let Some(listening) = &mut self.model.book {
                    listening.chapters = chapters;
                }
            }
        }
        self.dirty.mark_render();
    }

    /// Next and Previous inside a book move by chapter. `None` when the
    /// current song is not part of a book (the queue's own step applies).
    pub(crate) fn book_step(&mut self, delta: i32) -> Option<Result<(), String>> {
        let (listening, part) = self.model.book_part()?;
        let position = self.model.position_ms;
        let target = if delta > 0 {
            listening.next(part, position)
        } else {
            listening.previous(part, position)
        };
        let Some((to, ms)) = target else {
            // The last chapter: Next keeps the place (Y2Player lost it here).
            self.ui.flash("This is the last chapter");
            self.dirty.mark_render();
            return Some(Ok(()));
        };
        let path = listening.parts.get(to)?.clone();
        let index = self.model.queue.iter().position(|t| t.path == path)?;
        self.model.queue_position = index;
        self.model.position_ms = ms;
        self.model.invalidate();
        self.paused_at = None;
        Some(self.load())
    }

    /// The book's last part ended: it is finished and starts over next time.
    /// `false` when no book was playing.
    pub(crate) fn finish_book_part(&mut self) -> Option<Result<(), String>> {
        let (listening, part) = self.model.book_part()?;
        if part + 1 < listening.parts.len() {
            let next = listening.parts[part + 1].clone();
            let index = self.model.queue.iter().position(|t| t.path == next)?;
            self.model.queue_position = index;
            self.model.position_ms = 0;
            self.model.invalidate();
            return Some(self.load());
        }
        let key = listening.key.clone();
        let first = listening.parts.first().cloned();
        if let Some(book) = self.book_by_key(&key) {
            let library = &mut self.model.library;
            library
                .shelf
                .set_finished(&book, &library.tracks, true, now());
        }
        if let Some(index) = first.and_then(|p| self.model.queue.iter().position(|t| t.path == p)) {
            self.model.queue_position = index;
        }
        self.model.position_ms = 0;
        self.model.playback = PlaybackState::Stopped;
        self.model.invalidate();
        self.write_shelf();
        self.ui.flash("Book finished");
        self.dirty.mark_both();
        Some(Ok(()))
    }

    /// How far a book goes back when it continues after a pause: `None`
    /// for no book or a short pause (it continues in place).
    pub(crate) fn book_rewind(&self) -> Option<u64> {
        let (listening, _) = self.model.book_part()?;
        let paused = match self.paused_at {
            Some(at) => at.elapsed(),
            // After a restart: since the place was saved.
            None => {
                let saved = self
                    .model
                    .library
                    .shelf
                    .books
                    .get(&listening.key)
                    .map_or(now(), |p| p.updated);
                Duration::from_secs(now().saturating_sub(saved).max(0) as u64)
            }
        };
        (paused >= REWIND_AFTER).then(|| books::rewind_ms(paused.as_secs() as i64))
    }

    pub(crate) fn set_book_finished(&mut self, key: &str, finished: bool) -> Result<(), String> {
        let book = self.book_by_key(key).ok_or("This book is not available")?;
        let library = &mut self.model.library;
        library
            .shelf
            .set_finished(&book, &library.tracks, finished, now());
        self.write_shelf();
        Ok(())
    }

    pub(crate) fn forget_book(&mut self, key: &str) -> Result<(), String> {
        let book = self.book_by_key(key).ok_or("This book is not available")?;
        let library = &mut self.model.library;
        library.shelf.forget(&book, &library.tracks);
        self.write_shelf();
        Ok(())
    }
}

/// A shelf file larger than this is not read (thousands of books fit).
const SHELF_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The saved shelf, empty when there is none or it cannot be read.
pub fn load_shelf(path: &Path) -> Shelf {
    let mut bytes = vec![];
    std::fs::File::open(path)
        .and_then(|f| f.take(SHELF_MAX_BYTES + 1).read_to_end(&mut bytes))
        .ok()
        .filter(|_| bytes.len() as u64 <= SHELF_MAX_BYTES)
        .and_then(|_| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_shelf(path: &Path, shelf: &Shelf) -> Result<(), String> {
    let bytes = serde_json::to_vec(shelf).map_err(|e| e.to_string())?;
    reborn_core::atomic_write(path, &bytes).map_err(|e| e.to_string())
}

/// Reads chapter marks off the UI thread; a file that cannot be read has
/// none (and is not asked for again by the caller).
pub struct ChapterReader {
    requests: Sender<Vec<PathBuf>>,
    pub results: Receiver<(PathBuf, Vec<ChapterMark>)>,
}
impl ChapterReader {
    pub fn spawn() -> std::io::Result<Self> {
        let (requests, wanted) = channel::<Vec<PathBuf>>();
        let (sender, results) = channel();
        std::thread::Builder::new()
            .name("chapters".into())
            .spawn(move || {
                reborn_platform::lower_thread_priority();
                while let Ok(paths) = wanted.recv() {
                    for path in paths {
                        if sender.send((path.clone(), read(&path))).is_err() {
                            return;
                        }
                    }
                }
            })?;
        Ok(Self { requests, results })
    }
    pub fn want(&self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            let _ = self.requests.send(paths);
        }
    }
}

pub fn read(path: &Path) -> Vec<ChapterMark> {
    let _cpu_hint = reborn_platform::workload::CpuWorkloadLease::acquire_workload_hint(
        reborn_platform::workload::Class::ArtworkDecode,
    );
    Cancel::new()
        .and_then(|cancel| MediaFile::open(path, cancel))
        .and_then(|mut file| file.chapters())
        .map(|chapters| {
            chapters
                .into_iter()
                .map(|c| ChapterMark {
                    start_ms: c.start_ms,
                    end_ms: c.end_ms,
                    title: c.title,
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reborn_core::books::Progress;

    #[test]
    fn a_held_seek_in_a_book_takes_big_steps() {
        assert_eq!(seek_step(Duration::ZERO), 10_000);
        assert_eq!(seek_step(Duration::from_secs(2)), 30_000);
        assert_eq!(seek_step(Duration::from_secs(10)), 60_000);
    }

    #[test]
    fn the_shelf_survives_a_restart_and_a_damaged_file_is_an_empty_shelf() {
        let dir = std::env::temp_dir().join(format!("reborn-shelf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("books.json");
        assert_eq!(load_shelf(&path), Shelf::default(), "no file yet");
        let mut shelf = Shelf::default();
        shelf.books.insert(
            "internal|/m/Audiobooks/Momo".into(),
            Progress {
                part_path: "/m/Audiobooks/Momo/02.mp3".into(),
                part_name: "02.mp3".into(),
                part_size: 1234,
                position_ms: 61_000,
                updated: 1_792_000_000,
                finished: false,
                speed: 1.25,
            },
        );
        save_shelf(&path, &shelf).unwrap();
        assert_eq!(load_shelf(&path), shelf);
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load_shelf(&path), Shelf::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn chapter_marks_are_read_from_book_files() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
        let marks = read(&fixtures.join("book-chapters.m4b"));
        let titles: Vec<_> = marks.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Opening", "Chapter 1: The Start", "Kapitel 2: Über Umwege"]
        );
        assert!(read(&fixtures.join("tone.flac")).is_empty());
        assert!(read(&fixtures.join("missing.m4b")).is_empty());
        let reader = ChapterReader::spawn().unwrap();
        reader.want(vec![fixtures.join("book-chapters.mp3")]);
        let (path, marks) = reader
            .results
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert!(path.ends_with("book-chapters.mp3"));
        assert_eq!(marks.len(), 3);
    }
}
