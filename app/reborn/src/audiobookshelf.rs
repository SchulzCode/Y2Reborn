//! Player-owned lifecycle for Audiobookshelf's background worker.
use reborn_platform::audiobookshelf::worker::Worker;
use std::path::Path;

pub(crate) fn start(root: &Path) -> Option<Worker> {
    Worker::start(
        root.join("state/audiobookshelf.json"),
        reborn_library::supported,
    )
    .ok()
}

use crate::Runtime;
use reborn_core::{AudiobookshelfRequest as Request, MediaSource};
use reborn_platform::audiobookshelf::worker::{Command, Event};
use reborn_ui::audiobookshelf_ui::{Book, Library};

fn book(value: reborn_platform::audiobookshelf::Book) -> Book {
    Book {
        id: value.id,
        title: value.media.metadata.title,
        author: value.media.metadata.author_name.unwrap_or_default(),
    }
}

impl Runtime {
    /// Queue sign-out before resetting settings, without racing active work.
    pub(crate) fn audiobookshelf_reset(&mut self) -> Result<(), String> {
        let worker = self
            .audiobookshelf
            .as_ref()
            .ok_or("Audiobookshelf is unavailable")?;
        if worker.busy() {
            return Err("Wait for Audiobookshelf to finish before resetting settings".into());
        }
        let request = self.ui.audiobookshelf.sequence.wrapping_add(1);
        worker
            .submit(Command::Forget { request })
            .map_err(str::to_owned)?;
        self.ui.audiobookshelf.sequence = request;
        self.ui.audiobookshelf.reset_forget_request = Some(request);
        Ok(())
    }

    pub(crate) fn audiobookshelf_request(&mut self, request: Request) {
        if matches!(request, Request::SignIn { .. }) && !self.online() {
            self.audiobookshelf_error("Connect to Wi-Fi first".into());
            return;
        }
        if matches!(request, Request::SignIn { .. }) && self.trusted_unix().is_none() {
            self.audiobookshelf_error("The clock isn't set yet; try again in a moment".into());
            return;
        }
        self.ui.audiobookshelf.sequence = self.ui.audiobookshelf.sequence.wrapping_add(1);
        self.ui.audiobookshelf.cover_in_flight = false;
        self.ui.audiobookshelf.cover_decode_pending = None;
        let sequence = self.ui.audiobookshelf.sequence;
        self.ui.audiobookshelf.error = None;
        if let Err(error) = self.submit_audiobookshelf(request, sequence) {
            self.audiobookshelf_error(error);
        }
    }

    fn submit_audiobookshelf(&self, request: Request, sequence: u64) -> Result<(), String> {
        let worker = self
            .audiobookshelf
            .as_ref()
            .ok_or("Audiobookshelf is unavailable")?;
        let source = || {
            self.model
                .sources
                .iter()
                .find(|s| s.online && matches!(s.kind, MediaSource::Internal))
                .ok_or("Internal storage is unavailable".to_owned())
        };
        let command = match request {
            Request::Status => Command::Status { request: sequence },
            Request::SignIn {
                server,
                username,
                password,
            } => Command::SignIn {
                request: sequence,
                server,
                username,
                password,
            },
            Request::Libraries => Command::Libraries { request: sequence },
            Request::Books { library, page } => Command::Books {
                request: sequence,
                library,
                page,
            },
            Request::Search { library, query } => Command::Search {
                request: sequence,
                library,
                query,
            },
            Request::OpenBook { item } => {
                let root = source()?.root.clone();
                worker.submit(Command::Downloaded {
                    request: sequence,
                    root,
                    item: item.clone(),
                })?;
                worker.submit(Command::Cover {
                    request: sequence,
                    item: item.clone(),
                })?;
                Command::Details {
                    request: sequence,
                    item,
                }
            }
            Request::Download { item } => {
                if self.model.platform.busy.is_some() {
                    return Err("Wait for the current system task to finish".into());
                }
                if matches!(
                    self.model.platform.snapshot.usb,
                    reborn_core::platform::UsbTransfer::Ready
                        | reborn_core::platform::UsbTransfer::Copying
                        | reborn_core::platform::UsbTransfer::Starting
                ) {
                    return Err("Unplug from the computer first".into());
                }
                let source = source()?;
                Command::Download {
                    request: sequence,
                    root: source.root.clone(),
                    mount_id: source.mount_id.ok_or("Storage identity is unavailable")?,
                    item,
                }
            }
            Request::Forget => Command::Forget { request: sequence },
        };
        worker.submit(command).map_err(str::to_owned)
    }

    fn audiobookshelf_error(&mut self, error: String) {
        let view = &mut self.ui.audiobookshelf;
        view.loading = false;
        view.progress = None;
        view.error = Some(error);
        self.dirty.mark_render();
    }

    pub(crate) fn remote_audio_tick(&mut self) {
        self.radio_tick();
        loop {
            let event = match self.audiobookshelf.as_ref().map(Worker::poll) {
                Some(Ok(Some(event))) => event,
                Some(Err(error)) => {
                    self.audiobookshelf = None;
                    if self.ui.audiobookshelf.reset_forget_request.take().is_some() {
                        self.ui
                            .flash("Settings reset; couldn't forget Audiobookshelf");
                    }
                    self.audiobookshelf_error(error.into());
                    break;
                }
                _ => break,
            };
            self.audiobookshelf_event(event);
        }
        self.audiobookshelf_cover_tick();
        if self.ui.audiobookshelf.scan_pending
            && !self.model.library.scanning
            && !self.sleep_prepared
            && self.on_scan_library().is_ok()
        {
            self.ui.audiobookshelf.scan_pending = false;
            self.ui.audiobookshelf.download_scan_active = true;
            self.ui.flash("Adding audiobook to library");
        }
    }

    /// Fetch one visible cover at a time, so a large library cannot flood the worker.
    fn audiobookshelf_cover_tick(&mut self) {
        let view = &mut self.ui.audiobookshelf;
        if self.model.screen != reborn_core::Screen::AudiobookshelfBooks
            || self.model.screen_off
            || self.sleep_prepared
            || view.loading
            || view.cover_in_flight
            || self.graphics.is_none()
            || self.thumbs.is_none()
        {
            return;
        }
        if let Some((item, key)) = view.cover_decode_pending.as_ref() {
            let (failed, pending) = self
                .thumbs
                .as_ref()
                .map_or((false, false), |worker| worker.status(*key));
            let done = self
                .graphics
                .as_ref()
                .is_some_and(|g| g.thumb_resident(*key))
                || failed;
            if !done && pending {
                return;
            }
            if !done {
                view.cover_requested.remove(item);
                view.cover_keys.remove(item);
            }
            view.cover_decode_pending = None;
        }
        let Some(worker) = &self.audiobookshelf else {
            return;
        };
        if worker.busy() {
            return;
        }
        let first_book_row = 1 + usize::from(view.searching);
        let first = self.model.navigation.scroll.saturating_sub(first_book_row);
        let Some(book) = view
            .books
            .iter()
            .skip(first)
            .take(10)
            .find(|book| !view.cover_requested.contains(&book.id))
        else {
            return;
        };
        let item = book.id.clone();
        if worker
            .submit(Command::ListCover {
                request: view.sequence,
                item: item.clone(),
            })
            .is_ok()
        {
            view.cover_requested.insert(item);
            view.cover_in_flight = true;
        }
    }

    fn audiobookshelf_list_cover(
        &mut self,
        request: u64,
        item: String,
        result: Result<Vec<u8>, String>,
    ) {
        self.ui.audiobookshelf.cover_in_flight = false;
        match result {
            Ok(bytes) => {
                if let Some(worker) = self.thumbs.as_mut() {
                    let key = reborn_core::logo_key(&format!("abs-list-{request}-{item}"));
                    self.ui.audiobookshelf.cover_keys.insert(item.clone(), key);
                    self.ui.audiobookshelf.cover_decode_pending = Some((item, key));
                    worker.want(vec![crate::artwork::Job {
                        thumb: key,
                        art: crate::artwork::Art::Memory(bytes),
                        cache: self.root.join("cache"),
                    }]);
                    self.dirty.mark_render();
                }
            }
            Err(error) if error == "Sign in to Audiobookshelf again" => {
                self.ui.audiobookshelf.connected = false;
                self.ui.audiobookshelf.username = None;
                self.audiobookshelf_error(error);
            }
            Err(_) => {}
        }
    }

    fn audiobookshelf_book_cover(&mut self, request: u64, result: Result<Vec<u8>, String>) {
        if let (Ok(bytes), Some(worker)) = (result, self.thumbs.as_mut()) {
            let key = reborn_core::logo_key(&format!("audiobookshelf-cover-{request}"));
            self.ui.audiobookshelf.cover_key = Some(key);
            worker.want(vec![crate::artwork::Job {
                thumb: key,
                art: crate::artwork::Art::Memory(bytes),
                cache: self.root.join("cache"),
            }]);
            self.dirty.mark_render();
        }
    }

    fn audiobookshelf_reset_reply(&mut self, request: u64, event: &Event) {
        if self.ui.audiobookshelf.reset_forget_request != Some(request) {
            return;
        }
        if let Event::Forget { result, .. } = event {
            self.ui.audiobookshelf.reset_forget_request = None;
            if result.is_ok() {
                self.ui.audiobookshelf.connected = false;
                self.ui.audiobookshelf.username = None;
                self.ui.flash(if matches!(result, Ok(false)) {
                    "Signed out here; server session may remain"
                } else {
                    "Settings reset"
                });
            } else {
                self.ui
                    .flash("Settings reset; couldn't forget Audiobookshelf");
            }
        }
    }

    fn audiobookshelf_event(&mut self, event: Event) {
        if matches!(&event, Event::Download { result: Ok(()), .. }) {
            self.ui.audiobookshelf.scan_pending = true;
        }
        if matches!(
            &event,
            Event::Forget {
                result: Ok(false),
                ..
            }
        ) {
            self.ui.flash("Signed out here; server session may remain");
        }
        if matches!(&event, Event::Forget { result: Err(_), .. }) {
            self.ui.flash("Couldn't remove the Audiobookshelf session");
        }
        let request = match &event {
            Event::Status { request, .. }
            | Event::SignIn { request, .. }
            | Event::Libraries { request, .. }
            | Event::Books { request, .. }
            | Event::Search { request, .. }
            | Event::Details { request, .. }
            | Event::Cover { request, .. }
            | Event::ListCover { request, .. }
            | Event::Downloaded { request, .. }
            | Event::Progress { request, .. }
            | Event::Download { request, .. }
            | Event::Forget { request, .. } => *request,
        };
        self.audiobookshelf_reset_reply(request, &event);
        if request != self.ui.audiobookshelf.sequence {
            return;
        }
        let event = match event {
            Event::ListCover { item, result, .. } => {
                self.audiobookshelf_list_cover(request, item, result);
                return;
            }
            Event::Cover { result, .. } => {
                self.audiobookshelf_book_cover(request, result);
                return;
            }
            event => event,
        };
        let view = &mut self.ui.audiobookshelf;
        let mut remote_logout_failed = false;
        let result: Result<(), String> = (|| {
            match event {
                Event::Status {
                    connected,
                    username,
                    ..
                } => {
                    view.connected = connected;
                    view.username = username;
                    view.loading = false;
                }
                Event::SignIn {
                    result, username, ..
                } => {
                    result?;
                    view.connected = true;
                    view.username = username;
                    view.loading = false;
                }
                Event::Libraries { result, .. } => {
                    view.libraries = result?
                        .into_iter()
                        .filter(|l| l.media_type == "book")
                        .map(|l| Library {
                            id: l.id,
                            name: l.name,
                        })
                        .collect();
                    view.loading = false;
                }
                Event::Books { result, .. } => {
                    let page = result?;
                    view.cover_keys.clear();
                    view.cover_requested.clear();
                    view.books = page.results.into_iter().map(book).collect();
                    view.total = page.total;
                    view.searching = false;
                    view.loading = false;
                }
                Event::Search { result, .. } => {
                    view.cover_keys.clear();
                    view.cover_requested.clear();
                    view.books = result?
                        .book
                        .into_iter()
                        .map(|b| book(b.library_item))
                        .collect();
                    view.searching = true;
                    view.loading = false;
                }
                Event::Details { result, .. } => {
                    view.selected = Some(book(result?));
                }
                Event::Downloaded { result, .. } => {
                    view.downloaded = Some(result?);
                }
                Event::Progress { bytes, total, .. } => {
                    view.progress = Some((bytes, total));
                }
                Event::Download { result, .. } => {
                    result?;
                    view.progress = None;
                    view.downloaded = Some(true);
                }
                Event::Forget { result, .. } => {
                    let revoked = result?;
                    view.connected = false;
                    view.username = None;
                    view.loading = false;
                    remote_logout_failed = !revoked;
                }
                Event::Cover { .. } => {}
                Event::ListCover { .. } => {}
            }
            Ok(())
        })();
        if remote_logout_failed {
            self.ui.flash("Signed out here; server session may remain");
        }
        if let Err(error) = result {
            if error == "Sign in to Audiobookshelf again" {
                self.ui.audiobookshelf.connected = false;
                self.ui.audiobookshelf.username = None;
            }
            self.audiobookshelf_error(error);
        }
        self.dirty.mark_render();
    }
}

/// Describe a library scan without calling audiobook downloads songs.
pub(crate) fn scan_message(download: bool, songs: u64, rescanned: u64) -> String {
    match songs {
        _ if download => "Audiobook downloaded".to_owned(),
        _ if rescanned > 0 && songs > 1 => format!("{} new songs added", rescanned.min(songs)),
        0 => "Scan finished. No music found.".to_owned(),
        1 => "Scan finished. 1 song in your library.".to_owned(),
        n => format!("Scan finished. {n} songs in your library."),
    }
}

#[cfg(test)]
mod scan_message_tests {
    use super::scan_message;

    #[test]
    fn download_scan_does_not_call_book_parts_songs() {
        assert_eq!(scan_message(true, 17, 17), "Audiobook downloaded");
        assert_eq!(scan_message(false, 17, 17), "17 new songs added");
        assert_eq!(scan_message(false, 0, 0), "Scan finished. No music found.");
        assert_eq!(
            scan_message(false, 1, 0),
            "Scan finished. 1 song in your library."
        );
        assert_eq!(
            scan_message(false, 17, 0),
            "Scan finished. 17 songs in your library."
        );
    }
}
