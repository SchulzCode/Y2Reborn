//! FM radio: the player's own tuner under Radio.
//!
//! The tuner sits in the radio chip (`reborn_platform::fm`); its sound arrives as a capture stream
//! through the decoder like any station, so the queue, the outputs and Now Playing treat an FM
//! station as a radio station whose address is `fm:<kHz>`. This file is what the Radio screens need
//! around it: whether FM shows at all, the saved stations, the search over the band and the
//! Stereo/Mono line of Now Playing. Its state lives beside the runtime (as the film player's does)
//! so that a player without a tuner pays nothing for it.
use crate::*;
use reborn_platform::fm::{self, Availability};
use reborn_ui::FmView;
use std::cell::RefCell;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

/// How often the tuner is asked (its signal line is a command to the chip).
const EVERY: Duration = Duration::from_secs(2);

#[derive(Default)]
struct State {
    paths: fm::Paths,
    /// The last search's stations (`state/fm.json`), read when first needed.
    saved: Option<fm::Saved>,
    /// A search running in its worker, and where its answer arrives.
    search: Option<Receiver<Result<Vec<u32>, String>>>,
    checked: Option<Instant>,
}

thread_local! {
    static FM: RefCell<State> = RefCell::new(State::default());
}

impl Runtime {
    fn fm_file(&self) -> PathBuf {
        self.root.join("state/fm.json")
    }

    /// FM as the Radio screens show it.
    pub(crate) fn fm_view(&self) -> FmView {
        if !self.model.settings.fm_radio {
            return FmView::Hidden;
        }
        let file = self.fm_file();
        FM.with_borrow_mut(|state| {
            if state.search.is_some() {
                return FmView::Scanning {
                    progress: state.paths.progress().unwrap_or(0),
                };
            }
            let saved = state.saved.get_or_insert_with(|| fm::load(&file));
            match state.paths.availability() {
                Availability::NoTuner => FmView::Hidden,
                Availability::NeedsFirmware => FmView::NeedsFirmware,
                Availability::Ready if !saved.scanned => FmView::NotScanned,
                Availability::Ready => FmView::Ready {
                    stations: saved.stations.len(),
                },
            }
        })
    }

    fn fm_stations(&self) -> Vec<reborn_core::RadioStation> {
        FM.with_borrow(|state| {
            let stations = state.saved.iter().flat_map(|saved| &saved.stations);
            stations.map(|&khz| fm::station(khz)).collect()
        })
    }

    /// The FM requests of the Radio screens.
    pub(crate) fn fm_request(&mut self, request: reborn_core::RadioRequest) -> Result<(), String> {
        use reborn_core::RadioRequest;
        match request {
            RadioRequest::FmScan => self.fm_search()?,
            RadioRequest::FmShow(on) => self.set_fm_radio(on),
            RadioRequest::FmCancel => {
                let _ = FM.with_borrow(|state| state.paths.cancel_scan());
            }
            _ => {
                self.ui.radio.fm = self.fm_view();
                let stations = self.fm_stations();
                self.ui.radio.set_stations_in_order(stations);
                self.ui.radio.loading = false;
                self.ui.radio.error = None;
            }
        }
        Ok(())
    }

    /// Settings > Customize > Browsing > FM Radio: whether Radio shows FM.
    fn set_fm_radio(&mut self, on: bool) {
        self.model.settings.fm_radio = on;
        self.ui.radio.fm = self.fm_view();
        self.checkpoint();
    }

    /// Search the band in a worker. FM playing stops first: the search turns the tuner's sound off
    /// while it runs.
    fn fm_search(&mut self) -> Result<(), String> {
        if FM.with_borrow(|state| state.search.is_some()) {
            return Ok(());
        }
        if FM.with_borrow(|state| state.paths.availability()) != Availability::Ready {
            self.ui.radio.fm = self.fm_view();
            return Err("FM needs the tuner firmware".into());
        }
        let playing = self.model.current();
        if playing.is_some_and(|t| t.is_radio() && fm::is_fm(&t.path.to_string_lossy())) {
            self.pause();
            self.model.playback = PlaybackState::Stopped;
        }
        let (answer, done) = channel();
        let paths = FM.with_borrow(|state| state.paths.clone());
        std::thread::Builder::new()
            .name("fm-search".into())
            .spawn(move || {
                let result = paths.scan().map_err(|e| match e.raw_os_error() {
                    Some(125) => "Search stopped".to_owned(), // ECANCELED
                    _ => format!("FM search failed: {e}"),
                });
                let _ = answer.send(result);
            })
            .map_err(|e| e.to_string())?;
        FM.with_borrow_mut(|state| state.search = Some(done));
        self.ui.radio.fm = self.fm_view();
        let fields = json!({});
        self.log.emit(
            Level::Info,
            "radio",
            "fm_scan_started",
            "FM search started",
            None,
            fields,
        );
        Ok(())
    }

    fn fm_search_done(&mut self, result: Result<Vec<u32>, String>) {
        match result {
            Ok(stations) => {
                let fields = json!({"stations": stations.len()});
                self.log.emit(
                    Level::Info,
                    "radio",
                    "fm_scan_done",
                    "FM search finished",
                    None,
                    fields,
                );
                if stations.is_empty() {
                    self.ui.flash("No FM stations found here");
                }
                let saved = fm::Saved {
                    scanned: true,
                    stations,
                };
                if let Err(e) = fm::save(&self.fm_file(), &saved) {
                    self.ui.flash(format!("Couldn't save FM stations: {e}"));
                }
                FM.with_borrow_mut(|state| state.saved = Some(saved));
            }
            Err(error) => {
                let fields = json!({"error": error});
                self.log.emit(
                    Level::Warn,
                    "radio",
                    "fm_scan_failed",
                    "FM search failed",
                    None,
                    fields,
                );
                self.ui.flash(error);
            }
        }
        self.ui.radio.fm = self.fm_view();
        if self.ui.radio.last_request == Some(reborn_core::RadioRequest::Fm) {
            let stations = self.fm_stations();
            self.ui.radio.set_stations_in_order(stations);
        }
        self.dirty.mark_render();
    }

    /// Once a tick: a finished search, the Radio row, and (true) the Stereo/Mono line of an FM
    /// station that is playing, which has no song titles to look for.
    pub(crate) fn fm_tick(&mut self) -> bool {
        self.share_tick();
        let finished = FM.with_borrow_mut(|state| {
            let answer = state.search.as_ref().map(Receiver::try_recv);
            match answer {
                Some(Ok(result)) => Some(result),
                Some(Err(TryRecvError::Disconnected)) => Some(Err("FM search failed".into())),
                _ => None,
            }
        });
        if let Some(result) = finished {
            FM.with_borrow_mut(|state| state.search = None);
            self.fm_search_done(result);
        }
        let due = FM.with_borrow_mut(|state| {
            let due = state.checked.is_none_or(|at| at.elapsed() >= EVERY);
            if due {
                state.checked = Some(Instant::now());
            }
            due
        });
        if due {
            let view = self.fm_view();
            if view != self.ui.radio.fm {
                self.ui.radio.fm = view;
                self.dirty.mark_render();
            }
        }
        let playing = self.model.current().filter(|t| t.is_radio());
        let Some(khz) = playing.and_then(|t| fm::station_khz(&t.path.to_string_lossy())) else {
            return false;
        };
        if self.ui.radio.now.is_none() {
            self.ui.radio.now = Some(fm::station(khz));
        }
        if !due && self.radio_live.is_some() {
            return true;
        }
        let signal = FM.with_borrow(|state| state.paths.signal());
        let line = match (self.model.playback, signal) {
            (PlaybackState::Playing, Some((_, true))) => "Stereo",
            (PlaybackState::Playing, Some((_, false))) => "Mono",
            _ => "FM",
        };
        if self.radio_live.as_deref() != Some(line) {
            let position = self.model.queue_position;
            if let Some(track) = self.model.queue.get_mut(position) {
                track.artist = line.to_owned();
            }
            self.radio_live = Some(line.to_owned());
            self.dirty.mark_render();
        }
        true
    }
}
