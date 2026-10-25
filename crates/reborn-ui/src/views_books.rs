//! Audiobooks: the list of books, a book's page with its chapters, and what
//! Now Playing shows while a book plays (chapter, book, author, time left).
use crate::{
    canvas::{Canvas, Moving},
    components::{self, cover_rounded},
    text::{self, Face},
    theme::{color, WIDTH},
    views_library::pill_row,
    Item, Ui,
};
use reborn_core::{
    books::{self, Book, Listening},
    AppModel, Track,
};

/// The books of a library, with the library they were made from (its
/// address and length).
pub(crate) type BookCache = ((usize, usize), Vec<Book>);

/// A length for people: "9 h 41 min", "41 min", "40 s".
pub fn length(ms: u64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        0 => format!("{} s", ms / 1000),
        1..60 => format!("{minutes} min"),
        _ => match minutes % 60 {
            0 => format!("{} h", minutes / 60),
            rest => format!("{} h {rest} min", minutes / 60),
        },
    }
}

impl Ui {
    /// Every book in the library (cached until the library changes).
    pub(crate) fn books(&self, tracks: &[Track]) -> Vec<Book> {
        let stamp = (tracks.as_ptr() as usize, tracks.len());
        let mut cache = self.books.borrow_mut();
        match &*cache {
            Some((seen, books)) if *seen == stamp => books.clone(),
            _ => {
                let books = books::books(tracks);
                *cache = Some((stamp, books.clone()));
                books
            }
        }
    }
    pub(crate) fn book(&self, tracks: &[Track], key: &str) -> Option<Book> {
        self.books(tracks).into_iter().find(|b| b.key == key)
    }
}

/// A book with its chapters as far as they are known (the playing book's
/// own, else from the marks read so far, else one per file).
pub(crate) fn listening(m: &AppModel, tracks: &[Track], book: &Book) -> Listening {
    if let Some(playing) = m.book.as_ref().filter(|l| l.key == book.key) {
        return playing.clone();
    }
    let parts: Vec<&Track> = book.parts.iter().map(|&i| &tracks[i]).collect();
    let marks: Vec<_> = parts
        .iter()
        .map(|t| m.library.chapters.get(&t.path).cloned().unwrap_or_default())
        .collect();
    let mut l = Listening::new(book, tracks, 1.0);
    l.chapters = books::spans(&parts, &marks);
    l
}

/// Where the listener is in `book`: (part index, position), live for the
/// playing book, else from the shelf; `None` for a new or finished book.
fn place(m: &AppModel, tracks: &[Track], book: &Book) -> Option<(usize, u64)> {
    if let Some((l, part)) = m.book_part().filter(|(l, _)| l.key == book.key) {
        return (!l.parts.is_empty()).then_some((part, m.position_ms));
    }
    let (_, p) = m.library.shelf.find(book, tracks)?;
    if p.finished {
        return None;
    }
    let part = book.parts.iter().position(|&i| {
        tracks[i].path == p.part_path
            || (tracks[i].filename == p.part_name && tracks[i].size == p.part_size)
    })?;
    Some((part, p.position_ms))
}

fn finished(m: &AppModel, tracks: &[Track], book: &Book) -> bool {
    m.library
        .shelf
        .find(book, tracks)
        .is_some_and(|(_, p)| p.finished)
}

/// "Chapter 3 of 12 · 45 %", "Finished", or the author and the length.
pub(crate) fn status(m: &AppModel, tracks: &[Track], book: &Book) -> String {
    if finished(m, tracks, book) {
        return "Finished".into();
    }
    let l = listening(m, tracks, book);
    match place(m, tracks, book) {
        Some((part, position)) => {
            let chapter = l.chapter_at(part, position).map_or(1, |c| c + 1);
            let mut s = format!("Chapter {chapter} of {}", l.chapters.len());
            if let Some((done, total)) = l.progress(part, position).filter(|(_, t)| *t > 0) {
                s += &format!(" · {} %", done * 100 / total);
            }
            s
        }
        None => [
            book.author.clone(),
            if book.duration_ms > 0 {
                length(book.duration_ms)
            } else {
                components::count(l.chapters.len(), "chapter", "chapters")
            },
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · "),
    }
}

/// Audiobooks: books in progress first (the last listened first), then the
/// new ones A to Z, finished books last.
pub(crate) fn book_rows(ui: &Ui, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
    let mut books: Vec<(u8, i64, Book)> = ui
        .books(tracks)
        .into_iter()
        .map(|b| {
            let saved = m.library.shelf.find(&b, tracks).map(|(_, p)| p.clone());
            let playing = m.book.as_ref().is_some_and(|l| l.key == b.key);
            let (group, recent) = match saved {
                _ if playing => (0, i64::MAX),
                Some(p) if p.finished => (2, 0),
                Some(p) => (0, p.updated),
                None => (1, 0),
            };
            (group, -recent, b)
        })
        .collect();
    // Stable: within a group the A to Z order of `books` stays.
    books.sort_by_key(|(group, recent, _)| (*group, *recent));
    books
        .into_iter()
        .map(|(_, _, b)| {
            let mut item = Item::new(b.title.clone(), format!("book:{}", b.key))
                .with_secondary(status(m, tracks, &b));
            item.art = b.art;
            item
        })
        .collect()
}

/// A book's page: Continue (or Play) and Start Over, then its chapters.
pub(crate) fn page_rows(ui: &Ui, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
    let Some(book) = ui.book(tracks, &m.navigation.filter) else {
        return vec![];
    };
    let started = place(m, tracks, &book).is_some();
    let mut rows = vec![if started {
        Item::new("Continue", "book_resume")
    } else {
        Item::new("Play", "book_resume")
    }];
    if started || finished(m, tracks, &book) {
        rows.push(Item::new("Start Over", "book_restart"));
    }
    let l = listening(m, tracks, &book);
    rows.extend(l.chapters.iter().enumerate().map(|(n, c)| {
        let (_, ms) = l.chapter_span(n).unwrap_or((0, 0));
        Item::new(c.title.clone(), format!("chapter:{n}")).with_secondary(if ms > 0 {
            components::time(ms)
        } else {
            String::new()
        })
    }));
    rows
}

/// The book page, drawn like an album page: cover, title, author, where
/// the listener is, the buttons, then the chapters (the current one in the
/// accent colour).
pub(crate) fn book_page(
    c: &mut Canvas,
    ui: &Ui,
    m: &AppModel,
    tracks: &[Track],
    interactive: bool,
) {
    let accent = ui.accent();
    components::status_bar(c, ui, m, "Audiobook", true);
    let Some(book) = ui.book(tracks, &m.navigation.filter) else {
        c.text_center(
            240.,
            180.,
            "This book is not available",
            Face::R15,
            color::MUTED,
        );
        return;
    };
    let rows = page_rows(ui, m, tracks);
    let pills = rows
        .iter()
        .take_while(|r| r.key.starts_with("book_"))
        .count();
    let count = rows.len();
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    // The header is the list's first row and scrolls away with it (8.37).
    let first = components::header_page_first(focus, pills, count, m.navigation.scroll);
    let offset = components::header_page_offset(first);
    let l = listening(m, tracks, &book);
    let current = place(m, tracks, &book).and_then(|(part, pos)| l.chapter_at(part, pos));
    let page_at = c.draw.len();
    if components::header_page_shows_header(ui, first) {
        let art = book.art.and_then(|i| tracks.get(i).map(|t| (i, t)));
        cover_rounded(c, ui, art, 24., 44., 104., 9.);
        c.text_mid(144., 72., 316., &book.title, Face::S25, color::TEXT);
        c.text_mid(144., 98., 310., &book.author, Face::R15, color::TEXT);
        // The author stands above: a new book's line is its length alone.
        let mut line = match place(m, tracks, &book) {
            None if !finished(m, tracks, &book) => {
                let parts = components::count(l.chapters.len(), "chapter", "chapters");
                if book.duration_ms > 0 {
                    format!("{} · {parts}", length(book.duration_ms))
                } else {
                    parts
                }
            }
            _ => status(m, tracks, &book),
        };
        if let Some((done, total)) = place(m, tracks, &book)
            .and_then(|(part, pos)| l.progress(part, pos))
            .filter(|(d, t)| *t > *d)
        {
            line += &format!(" · {} left", length(total - done));
        }
        c.text_mid(144., 119., 310., &line, Face::R11, color::MUTED);
        let labels: Vec<(&str, bool)> = rows[..pills]
            .iter()
            .enumerate()
            .map(|(i, r)| (r.label.as_str(), focus == i))
            .collect();
        pill_row(c, 144., 134., &labels, interactive);
    }
    let range = components::header_page_rows(ui, first, pills, count);
    for (n, row) in rows.iter().skip(range.start).take(range.len()).enumerate() {
        let index = range.start + n;
        let chapter = index - pills;
        let y = components::header_page_y(index, pills, 0.);
        let selected = interactive && index == focus;
        if selected {
            components::focus_row(c, 10., y + 1., WIDTH - 20., 50.);
        }
        c.text_mid_center(
            34.,
            y + 26.,
            &(chapter + 1).to_string(),
            Face::R11,
            if selected { color::TEXT } else { color::DIM },
        );
        let face = if selected { Face::S15 } else { Face::R15 };
        let tint = if current == Some(chapter) {
            accent
        } else {
            color::TEXT
        };
        let tail = text::width(Face::R11, &row.secondary) + 20.;
        c.text_mid(58., y + 26., 360. - tail, &row.label, face, tint);
        c.text_mid_right(
            WIDTH - 28.,
            y + 26.,
            &row.secondary,
            Face::R11,
            if selected { color::TEXT } else { color::DIM },
        );
    }
    components::shift_from(c, page_at, -offset);
    c.mark_moving(Moving::Scroll {
        start: page_at,
        end: c.draw.len(),
        top: crate::views_search::TOP,
        bottom: crate::theme::HEIGHT,
        offset,
        row_h: components::PAGE_ROW,
    });
    let total = components::PAGE_HEADER + (count - pills) as f32 * components::PAGE_ROW;
    components::page_rail(c, offset, total, accent);
}

/// What Now Playing shows for a book: chapter title, book, author, the
/// chapter's start and length, and the book's place and length.
pub(crate) struct BookNow {
    pub chapter: String,
    pub book: String,
    pub author: String,
    pub chapter_start: u64,
    pub chapter_ms: u64,
    pub done_ms: u64,
    pub total_ms: u64,
    pub speed: f32,
    /// The chapter's number (from 0) and how many the book has.
    pub index: usize,
    pub count: usize,
}
impl BookNow {
    /// Where in the chapter `position_ms` (of the part) is.
    pub fn elapsed(&self, position_ms: u64) -> u64 {
        position_ms
            .saturating_sub(self.chapter_start)
            .min(self.chapter_ms)
    }
    /// "23 % · 16 h 7 min left" (empty while the book's length is unknown).
    pub fn left(&self) -> String {
        (self.done_ms * 100)
            .checked_div(self.total_ms)
            .map(|percent| {
                format!(
                    "{percent} % · {} left",
                    length(self.total_ms.saturating_sub(self.done_ms))
                )
            })
            .unwrap_or_default()
    }
}
pub(crate) fn now(m: &AppModel) -> Option<BookNow> {
    let (l, part) = m.book_part()?;
    let at = l.chapter_at(part, m.position_ms)?;
    let (start, ms) = l.chapter_span(at)?;
    let (done, total) = l.progress(part, m.position_ms).unwrap_or((0, 0));
    Some(BookNow {
        chapter: l.chapters[at].title.clone(),
        book: l.title.clone(),
        author: l.author.clone(),
        chapter_start: start,
        chapter_ms: ms,
        done_ms: done,
        total_ms: total,
        speed: l.speed,
        index: at,
        count: l.chapters.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_read_as_hours_and_minutes() {
        assert_eq!(length(40_000), "40 s");
        assert_eq!(length(41 * 60_000), "41 min");
        assert_eq!(length(9 * 3_600_000 + 41 * 60_000), "9 h 41 min");
        assert_eq!(length(2 * 3_600_000), "2 h");
    }
}
