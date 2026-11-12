//! Click-wheel pages and transient state for the Audiobookshelf service.
use crate::{
    keyboard,
    pages::{self, Page},
    Item, RowKind, Ui,
};
use reborn_core::{AppModel, AudiobookshelfRequest as Request, Effect, Screen};

const BOOK_LAYOUT_NAME: &str = "audiobookshelf_book";
pub(crate) static BOOK_LAYOUTS: [&str; 1] = [BOOK_LAYOUT_NAME];

pub(crate) fn book_screen(c: &mut crate::canvas::Canvas, ui: &Ui, m: &AppModel, interactive: bool) {
    use crate::{components, text::Face, theme::color};
    components::status_bar(c, ui, m, "Audiobookshelf", true);
    let page = page(ui, Screen::AudiobookshelfBook);
    if let Some(book) = &ui.audiobookshelf.selected {
        if !ui.audiobookshelf.loading && ui.audiobookshelf.error.is_none() {
            let key = ui.audiobookshelf.cover_key.unwrap_or(0);
            if key != 0 && ui.thumb_resident(key) {
                c.draw.push(reborn_graphics::Quad {
                    tex: reborn_graphics::Tex::Thumb(key),
                    ..reborn_graphics::Quad::rect(24., 48., 104., 104., color::TEXT)
                });
            } else {
                components::placeholder(c, 24., 48., 104., 9., key);
            }
            c.text_mid(144., 76., 310., &book.title, Face::S17, color::TEXT);
            c.text_mid(144., 104., 310., &book.author, Face::R15, color::MUTED);
            if ui
                .skin
                .as_ref()
                .is_some_and(|skin| skin.layout(BOOK_LAYOUT_NAME).is_some())
            {
                return;
            }
            let focus = m.navigation.focus.min(page.rows.len().saturating_sub(1));
            crate::views_settings::group_page(c, ui, m, &page, focus, interactive, 164.);
            return;
        }
    }
    let focus = m.navigation.focus.min(page.rows.len().saturating_sub(1));
    crate::views_settings::group_page(c, ui, m, &page, focus, interactive, 38.);
}

#[derive(Clone, Default)]
pub struct Library {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Default)]
pub struct Book {
    pub id: String,
    pub title: String,
    pub author: String,
}

#[derive(Default)]
pub struct View {
    pub sequence: u64,
    pub reset_forget_request: Option<u64>,
    pub scan_pending: bool,
    pub download_scan_active: bool,
    pub connected: bool,
    pub username: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    pub libraries: Vec<Library>,
    pub library: Option<Library>,
    pub books: Vec<Book>,
    pub cover_keys: std::collections::HashMap<String, u64>,
    pub cover_requested: std::collections::HashSet<String>,
    pub cover_in_flight: bool,
    pub cover_decode_pending: Option<(String, u64)>,
    pub selected: Option<Book>,
    pub cover_key: Option<u64>,
    pub page: u32,
    pub total: u64,
    pub searching: bool,
    pub downloaded: Option<bool>,
    pub progress: Option<(u64, u64)>,
}

fn row(label: &str, key: &str, detail: impl Into<String>) -> Item {
    pages::row(label, key, detail)
}

pub(crate) fn page(ui: &Ui, screen: Screen) -> Page {
    let view = &ui.audiobookshelf;
    if view.loading {
        return Page::hero("Loading…", "Contacting Audiobookshelf.");
    }
    if let Some(error) = &view.error {
        return Page::hero("Audiobookshelf", error).with_rows(vec![row("Back", "abs_back", "")]);
    }
    if screen == Screen::Audiobookshelf && !view.connected {
        return Page::rows(vec![row(
            "Sign In",
            "abs_sign_in",
            "Server, username and password",
        )
        .with_kind(RowKind::Chevron)]);
    }
    if screen == Screen::Audiobookshelf {
        return Page::rows(vec![
            row(
                "Browse Libraries",
                "abs_libraries",
                view.username.as_deref().unwrap_or("Connected"),
            )
            .with_kind(RowKind::Chevron),
            row("Forget Account", "abs_forget", ""),
        ]);
    }
    if screen == Screen::AudiobookshelfLibraries {
        return if view.libraries.is_empty() {
            Page::hero(
                "No Book Libraries",
                "This account has no accessible book libraries.",
            )
        } else {
            Page::rows(
                view.libraries
                    .iter()
                    .enumerate()
                    .map(|(i, library)| {
                        row(&library.name, &format!("abs_library:{i}"), "")
                            .with_kind(RowKind::Chevron)
                    })
                    .collect(),
            )
        };
    }
    if screen == Screen::AudiobookshelfBooks {
        let mut rows =
            vec![row("Search", "abs_search", "Title or author").with_kind(RowKind::Chevron)];
        if view.searching {
            rows.push(row("All Books", "abs_all", ""));
        }
        rows.extend(view.books.iter().enumerate().map(|(i, book)| {
            let mut item = row(&book.title, &format!("abs_book:{i}"), &book.author)
                .with_kind(RowKind::Chevron);
            item.remote_art = view.cover_keys.get(&book.id).copied();
            item
        }));
        if !view.searching && view.page > 0 {
            rows.push(row("Previous Page", "abs_previous", ""));
        }
        if !view.searching && (u64::from(view.page) + 1) * 50 < view.total {
            rows.push(row("Next Page", "abs_next", ""));
        }
        return Page::rows(rows);
    }
    if screen == Screen::AudiobookshelfBook {
        if view.selected.is_none() {
            return Page::hero("Book Unavailable", "Go back and choose a book.");
        }
        let (label, detail) = match view.progress {
            Some((_, 0)) => ("Downloading…", "Starting…".to_owned()),
            Some((done, total)) => (
                "Downloading…",
                format!("{}%", done.saturating_mul(100) / total),
            ),
            None if view.downloaded == Some(true) => {
                ("Downloaded", "Available in Audiobooks".to_owned())
            }
            None => ("Download", "Download for offline listening".to_owned()),
        };
        let mut action = row(label, "abs_download", detail);
        action.enabled = view.progress.is_none() && view.downloaded != Some(true);
        return Page::rows(vec![action]);
    }
    Page::default()
}

impl Ui {
    pub(crate) fn audiobookshelf_select(&mut self, m: &mut AppModel, key: &str) -> Effect {
        let view = &mut self.audiobookshelf;
        match key {
            "abs_back" => {
                view.error = None;
                Self::back(m);
                Effect::None
            }
            "abs_sign_in" => {
                self.keyboard = Some(keyboard::Keyboard::new(keyboard::Purpose::AbsServer, ""));
                Effect::None
            }
            "abs_forget" => {
                view.loading = true;
                Effect::Audiobookshelf(Request::Forget)
            }
            "abs_libraries" => {
                view.loading = true;
                view.error = None;
                Self::go(m, Screen::AudiobookshelfLibraries, "");
                Effect::Audiobookshelf(Request::Libraries)
            }
            "abs_search" => {
                self.keyboard = Some(keyboard::Keyboard::new(keyboard::Purpose::AbsSearch, ""));
                Effect::None
            }
            "abs_all" => {
                let Some(library) = &view.library else {
                    return Effect::None;
                };
                view.page = 0;
                view.loading = true;
                Effect::Audiobookshelf(Request::Books {
                    library: library.id.clone(),
                    page: 0,
                })
            }
            "abs_previous" | "abs_next" => {
                let Some(library) = &view.library else {
                    return Effect::None;
                };
                view.page = if key == "abs_next" {
                    view.page.saturating_add(1)
                } else {
                    view.page.saturating_sub(1)
                };
                view.loading = true;
                Effect::Audiobookshelf(Request::Books {
                    library: library.id.clone(),
                    page: view.page,
                })
            }
            "abs_download" if view.downloaded != Some(true) && view.progress.is_none() => {
                let Some(book) = &view.selected else {
                    return Effect::None;
                };
                view.progress = Some((0, 0));
                Effect::Audiobookshelf(Request::Download {
                    item: book.id.clone(),
                })
            }
            _ => {
                if let Some(library) = key
                    .strip_prefix("abs_library:")
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| view.libraries.get(i))
                    .cloned()
                {
                    view.library = Some(library.clone());
                    view.page = 0;
                    view.searching = false;
                    view.loading = true;
                    Self::go(m, Screen::AudiobookshelfBooks, &library.name);
                    return Effect::Audiobookshelf(Request::Books {
                        library: library.id,
                        page: 0,
                    });
                }
                if let Some(book) = key
                    .strip_prefix("abs_book:")
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| view.books.get(i))
                    .cloned()
                {
                    view.selected = Some(book.clone());
                    view.cover_key = None;
                    view.downloaded = None;
                    view.progress = None;
                    Self::go(m, Screen::AudiobookshelfBook, "");
                    return Effect::Audiobookshelf(Request::OpenBook { item: book.id });
                }
                Effect::None
            }
        }
    }
}

impl Ui {
    pub(crate) fn audiobookshelf_keyboard(
        &mut self,
        purpose: keyboard::Purpose,
        text: String,
        raw: String,
    ) -> Effect {
        use keyboard::Purpose;
        match purpose {
            Purpose::AbsServer => {
                if text.is_empty() {
                    self.flash("Enter the server address");
                    return Effect::None;
                }
                self.keyboard = Some(keyboard::Keyboard::new(
                    Purpose::AbsUsername { server: text },
                    "",
                ));
                Effect::None
            }
            Purpose::AbsUsername { server } => {
                if text.is_empty() {
                    self.flash("Enter your username");
                    return Effect::None;
                }
                self.keyboard = Some(keyboard::Keyboard::new(
                    Purpose::AbsPassword {
                        server,
                        username: text,
                    },
                    "",
                ));
                Effect::None
            }
            Purpose::AbsPassword { server, username } => {
                if raw.is_empty() {
                    self.flash("Enter your password");
                    return Effect::None;
                }
                self.keyboard = None;
                self.audiobookshelf.loading = true;
                self.audiobookshelf.error = None;
                Effect::Audiobookshelf(Request::SignIn {
                    server,
                    username,
                    password: raw,
                })
            }
            Purpose::AbsSearch => {
                let Some(library) = &self.audiobookshelf.library else {
                    return Effect::None;
                };
                if text.is_empty() {
                    self.flash("Enter a title or author");
                    return Effect::None;
                }
                let library = library.id.clone();
                self.keyboard = None;
                self.audiobookshelf.loading = true;
                self.audiobookshelf.searching = true;
                Effect::Audiobookshelf(Request::Search {
                    library,
                    query: text,
                })
            }
            Purpose::WifiPassword { .. }
            | Purpose::WifiName
            | Purpose::RadioSearch
            | Purpose::LibrarySearch
            | Purpose::PlaylistNew
            | Purpose::PlaylistRename
            | Purpose::LastFmUser
            | Purpose::LastFmPassword { .. } => Effect::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_page_shows_the_saved_username() {
        let mut ui = Ui::default();
        ui.audiobookshelf.connected = true;
        ui.audiobookshelf.username = Some("reader".into());
        let page = page(&ui, Screen::Audiobookshelf);
        assert!(page
            .rows
            .iter()
            .any(|row| row.key == "abs_libraries" && row.secondary == "reader"));
    }

    #[test]
    fn browse_rows_use_the_matching_service_cover() {
        let mut ui = Ui::default();
        ui.audiobookshelf.books.push(Book {
            id: "book".into(),
            title: "Title".into(),
            author: "Author".into(),
        });
        ui.audiobookshelf.cover_keys.insert("book".into(), 42);
        ui.set_resident_thumbs([42]);
        let model = AppModel {
            screen: Screen::AudiobookshelfBooks,
            ..Default::default()
        };
        assert_eq!(page(&ui, model.screen).rows[1].remote_art, Some(42));
        assert!(ui
            .draw(&model, &[], false)
            .iter()
            .any(|quad| quad.tex == reborn_graphics::Tex::Thumb(42)));
    }

    #[test]
    fn selected_book_displays_its_resident_cover() {
        let mut ui = Ui::default();
        ui.audiobookshelf.selected = Some(Book {
            id: "book".into(),
            title: "Title".into(),
            author: "Author".into(),
        });
        ui.audiobookshelf.cover_key = Some(42);
        ui.set_resident_thumbs([42]);
        let model = AppModel {
            screen: Screen::AudiobookshelfBook,
            ..Default::default()
        };
        assert!(ui
            .draw(&model, &[], false)
            .iter()
            .any(|quad| quad.tex == reborn_graphics::Tex::Thumb(42)));
    }

    #[test]
    fn downloaded_or_downloading_books_do_not_offer_another_download() {
        let mut ui = Ui::default();
        ui.audiobookshelf.selected = Some(Book::default());
        ui.audiobookshelf.progress = Some((42, 100));
        let row = &page(&ui, Screen::AudiobookshelfBook).rows[0];
        assert_eq!(row.label, "Downloading…");
        assert!(!row.enabled);
        ui.audiobookshelf.progress = None;
        ui.audiobookshelf.downloaded = Some(true);
        let row = &page(&ui, Screen::AudiobookshelfBook).rows[0];
        assert_eq!(row.label, "Downloaded");
        assert!(!row.enabled);
    }

    #[test]
    fn browse_search_and_download_follow_the_selected_book() {
        let mut ui = Ui::default();
        let mut model = AppModel::default();
        ui.audiobookshelf.libraries.push(Library {
            id: "library".into(),
            name: "Books".into(),
        });
        assert!(
            matches!(ui.audiobookshelf_select(&mut model, "abs_library:0"),
            Effect::Audiobookshelf(Request::Books { library, page: 0 }) if library == "library")
        );
        ui.audiobookshelf.loading = false;
        ui.audiobookshelf.searching = true;
        assert!(matches!(ui.audiobookshelf_select(&mut model, "abs_all"),
            Effect::Audiobookshelf(Request::Books { library, page: 0 }) if library == "library"));
        ui.audiobookshelf.books.push(Book {
            id: "book".into(),
            title: "Title".into(),
            author: "Author".into(),
        });
        assert!(matches!(ui.audiobookshelf_select(&mut model, "abs_book:0"),
            Effect::Audiobookshelf(Request::OpenBook { item }) if item == "book"));
        assert!(
            matches!(ui.audiobookshelf_select(&mut model, "abs_download"),
            Effect::Audiobookshelf(Request::Download { item }) if item == "book")
        );
        assert!(matches!(
            ui.audiobookshelf_select(&mut model, "abs_download"),
            Effect::None
        ));
    }

    #[test]
    fn error_back_clears_the_error() {
        let mut ui = Ui::default();
        let mut model = AppModel::default();
        ui.audiobookshelf.error = Some("Network unavailable".into());
        ui.audiobookshelf_select(&mut model, "abs_back");
        assert!(ui.audiobookshelf.error.is_none());
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;

    #[test]
    fn password_keyboard_debug_redacts_input_and_context() {
        let keyboard = keyboard::Keyboard::new(
            keyboard::Purpose::AbsPassword {
                server: "private-server".into(),
                username: "private-user".into(),
            },
            "private-password",
        );
        assert!(!format!("{keyboard:?}").contains("private-"));
    }
}
