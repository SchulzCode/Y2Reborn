//! Fictional Audiobookshelf states for the host preview renderer.
use super::{model, Setup};
use reborn_core::{AppModel, Screen};
use reborn_ui::audiobookshelf_ui::{Book, Library};

pub(super) fn at(screen: Screen, filter: &str) -> AppModel {
    let mut model = model();
    model.screen = screen;
    model.navigation.filter = filter.into();
    model
}

pub(super) fn cases() -> Vec<(&'static str, AppModel, Setup)> {
    let mut cases: Vec<(&str, AppModel, Setup)> = Vec::new();
    cases.push((
        "abs-01-sign-in",
        at(Screen::Audiobookshelf, ""),
        Box::new(|_, _| {}),
    ));
    cases.push((
        "abs-02-account",
        at(Screen::Audiobookshelf, ""),
        Box::new(|ui, _| {
            ui.audiobookshelf.connected = true;
            ui.audiobookshelf.username = Some("reader".into());
        }),
    ));
    cases.push((
        "abs-03-libraries",
        at(Screen::AudiobookshelfLibraries, ""),
        Box::new(|ui, _| {
            ui.audiobookshelf.libraries = ["Audiobooks", "Family Books"]
                .into_iter()
                .map(|name| Library {
                    id: name.into(),
                    name: name.into(),
                })
                .collect();
        }),
    ));
    cases.push((
        "abs-04-books",
        at(Screen::AudiobookshelfBooks, ""),
        Box::new(|ui, model| {
            ui.audiobookshelf.books = [
                ("The Paper Garden", "Mira Lane"),
                ("The Long Way Home", "Alex Morgan"),
                ("A Quiet Harbour", "Jordan Bell"),
            ]
            .into_iter()
            .map(|(title, author)| Book {
                id: title.into(),
                title: title.into(),
                author: author.into(),
            })
            .collect();
            for (book, track) in ui.audiobookshelf.books.iter().zip(&model.library.tracks) {
                ui.audiobookshelf
                    .cover_keys
                    .insert(book.id.clone(), reborn_ui::art_key(track));
            }
            ui.audiobookshelf.total = ui.audiobookshelf.books.len() as u64;
        }),
    ));
    for (name, progress, downloaded) in [
        ("abs-05-book", None, false),
        ("abs-06-downloading", Some((42, 100)), false),
        ("abs-07-downloaded", None, true),
    ] {
        cases.push((
            name,
            at(Screen::AudiobookshelfBook, ""),
            Box::new(move |ui, model| {
                ui.audiobookshelf.selected = Some(Book {
                    id: "sample-book".into(),
                    title: "The Paper Garden".into(),
                    author: "Mira Lane".into(),
                });
                ui.audiobookshelf.cover_key = model.library.tracks.first().map(reborn_ui::art_key);
                ui.audiobookshelf.progress = progress;
                ui.audiobookshelf.downloaded = Some(downloaded);
            }),
        ));
    }
    cases.push((
        "abs-08-network-error",
        at(Screen::Audiobookshelf, ""),
        Box::new(|ui, _| {
            ui.audiobookshelf.error = Some("Couldn't reach Audiobookshelf".into());
        }),
    ));
    cases
}
