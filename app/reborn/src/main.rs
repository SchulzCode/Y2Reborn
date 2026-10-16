#![forbid(unsafe_code)]
mod artwork;
mod background;
mod diagnostics;
mod graphics_recovery;
mod performance;
mod playback;
mod playlists;
mod skins;
mod volume;
use reborn_control::{Command, PlaybackAction, Response};
use reborn_core::platform::{Fact, LowBattery};
use reborn_core::{
    AppModel, AudioOutput, Effect, Event, PlaybackState, QueueEntryId, RepeatMode, Screen, Source,
    Track,
};
use reborn_graphics::Renderer;
use reborn_library::{Database, Scanner};
use reborn_observability::{HealthState, Level, Observer};
use reborn_platform::client;
use reborn_platform::{avrcp, bluetooth, input, kernel_events, power, storage, wifi};
use reborn_ui::{BluetoothDeviceView, BluetoothView, NetworkView, Ui, WifiStatus, WifiView};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        mpsc::{Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
/// A pause held in place gives its sink back after this long (Bluetooth
/// headphones and the wired codec may then rest).
const HOLD_LIMIT: Duration = Duration::from_secs(180);
/// Seek steps closer together than this are one seek.
const SEEK_SETTLE: Duration = Duration::from_millis(350);

/// Stations kept on top of a country's, a genre's or the popular list.
const TOP_STATIONS: usize = 10;

/// Station logos: one background download at a time, each logo once (a
/// failure is remembered on disk for a few days). The player asks for the
/// logos a frame wanted; finished ones are then decoded like covers. A logo
/// that could not be fetched or decoded is not asked for again this session:
/// asking every frame kept the player redrawing without end (Beta 5.1–6).
struct LogoFetcher {
    requests: std::sync::mpsc::Sender<(u64, String)>,
    /// (key, a logo file is on disk now).
    done: std::sync::mpsc::Receiver<(u64, bool)>,
    pending: std::collections::HashSet<u64>,
    settled: std::collections::HashSet<u64>,
}
impl LogoFetcher {
    fn spawn(cache: PathBuf) -> Option<Self> {
        let (requests, queue) = std::sync::mpsc::channel::<(u64, String)>();
        let (finished, done) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("radio-logos".into())
            .spawn(move || {
                while let Ok((key, url)) = queue.recv() {
                    if !reborn_platform::radio::logo_settled(&cache, key) {
                        let _ = reborn_platform::radio::fetch_logo(&url, &cache, key);
                    }
                    let saved = reborn_platform::radio::logo_file(&cache, key).exists();
                    if finished.send((key, saved)).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        Some(Self {
            requests,
            done,
            pending: Default::default(),
            settled: Default::default(),
        })
    }
    fn want(&mut self, key: u64, url: &str) {
        if !self.settled.contains(&key) && self.pending.len() < 64 && self.pending.insert(key) {
            let _ = self.requests.send((key, url.to_owned()));
        }
    }
    /// No usable logo behind this key (none to fetch, or it did not decode).
    fn settle(&mut self, key: u64) {
        self.settled.insert(key);
    }
    fn is_settled(&self, key: u64) -> bool {
        self.settled.contains(&key)
    }
    /// Logos saved since the last call (only those need a new frame).
    fn finished(&mut self) -> usize {
        let mut saved = 0;
        while let Ok((key, ok)) = self.done.try_recv() {
            self.pending.remove(&key);
            if ok {
                saved += 1;
            } else {
                self.settled.insert(key);
            }
        }
        saved
    }
}

/// A finished station-directory request; `u64` matches `Runtime::radio_sequence`.
enum RadioReply {
    Stations(u64, Result<Vec<reborn_core::RadioStation>, String>),
    Categories(u64, Result<Vec<reborn_platform::radio::Category>, String>),
    /// An FM search finished: stations in kHz, or why it did not.
    FmScan(Result<Vec<u32>, String>),
}

struct Runtime {
    /// Look the time zone up again (Automatic was just turned on).
    zone_lookup_again: bool,
    playback_hints: reborn_platform::workload::Hints,
    interaction_hints: reborn_platform::workload::Hints,
    model: AppModel,
    ui: Ui,
    /// The single owner of platform observation and platform operations.
    platform: Option<client::Client>,
    /// A refresh (not a user operation) is in flight.
    platform_refresh_pending: bool,
    platform_observed: bool,
    /// The release the daily check last announced, so the toast shows once.
    announced_update: Option<String>,
    /// Remaining frames of the first-frame fade from the boot mark into the UI.
    boot_fade: u8,
    thumbs: Option<artwork::Worker>,
    playback: playback::Playback,
    log: Observer,
    db: Database,
    scanner: Scanner,
    wifi: Option<wifi::Wifi>,
    bluetooth: Option<bluetooth::Bluetooth>,
    wifi_state: wifi::Status,
    bt_state: bluetooth::Status,
    graphics: Option<Renderer>,
    graphics_recovery: graphics_recovery::Recovery,
    power: Value,
    kernel_events: Arc<Mutex<Vec<String>>>,
    background: background::Gate,
    art: bool,
    dirty: DirtyState,
    root: PathBuf,
    /// Internet radio: saved favorites, the station playing, directory replies.
    radio_favorites: Vec<reborn_core::RadioStation>,
    radio_current: Option<reborn_core::RadioStation>,
    radio_sequence: u64,
    radio_replies: (
        std::sync::mpsc::Sender<RadioReply>,
        std::sync::mpsc::Receiver<RadioReply>,
    ),
    radio_directory: Arc<Mutex<Option<Arc<reborn_platform::radio::Directory>>>>,
    /// The live line last shown for the station (title or connection state).
    radio_live: Option<String>,
    /// The list a station was started from, for Previous/Next between stations.
    radio_list: Vec<reborn_core::RadioStation>,
    /// FM: the tuner's interface, its saved stations, a search running and
    /// when the signal line was last read.
    fm: reborn_platform::fm::Paths,
    fm_saved: reborn_platform::fm::Saved,
    fm_scanning: bool,
    /// A search ran in this session: FM stays visible even when it found
    /// nothing, so the listener can plug in headphones and search again.
    fm_searched_now: bool,
    fm_checked: Option<Instant>,
    /// Station logos: downloads (once per logo) and the logo shown as Now
    /// Playing's picture.
    radio_logos: Option<LogoFetcher>,
    /// The logo accepted as Now Playing's picture, and one being decoded
    /// (kept after a failed decode, so a broken logo is not retried).
    radio_art: Option<u64>,
    radio_art_pending: Option<u64>,
    /// Playlist files are read and written on their own thread.
    playlist_worker: Option<playlists::Worker>,
    headless: bool,
    last_activity: Instant,
    query: Option<Receiver<Result<Vec<Track>, String>>>,
    action_depth: usize,
    action_before: Option<AppModel>,
    action_dirty_before: Option<DirtyState>,
    pending_reconfiguration: Option<PendingReconfiguration>,
    /// Paused in place since (the open pipeline is held, see `Playback::hold`).
    held_since: Option<Instant>,
    /// Playback restarts once at the model's song and place at this moment
    /// (and only when no start is in progress): seeking with held
    /// Previous/Next, skipping while a song is still starting.
    seek_until: Option<Instant>,
    reconfiguration_attempted: bool,
    sink_release_started: bool,
    transition_generation: Option<u64>,
    active_transport_generation: Option<u64>,
    input_resume: bool,
    sleep_prepared: bool,
    sleep_prepared_since: Option<Instant>,
    /// When the screen last went dark, for the idle radio policy.
    dark_since: Option<Instant>,
    /// Last request or active work; bounds retries and the pause-to-sleep delay.
    sleep_attempt: Option<Instant>,
    /// Wi-Fi was blocked at runtime by that policy and must be restored on wake.
    wifi_idle_off: bool,
    wifi_idle_failed: bool,
}

fn sleep_control_allowed(command: &Command) -> bool {
    matches!(
        command,
        Command::Status
            | Command::Audio
            | Command::Health
            | Command::Metrics
            | Command::Snapshot
            | Command::Events { .. }
            | Command::Logs { .. }
            | Command::SleepRestore
    )
}

/// Read-only periodic I/O runs on the existing watchdog. The UI remains the
/// owner of battery presentation, media transitions and scanner requests.
struct Observation {
    request: background::Request,
    power: Value,
    media: Option<(bool, Vec<Source>)>,
    elapsed_ms: f64,
}

struct PendingReconfiguration {
    previous: AppModel,
    dirty_before: DirtyState,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransportEpochState {
    Current,
    Changed,
    Unavailable,
}

fn transport_epoch_state(
    output: &AudioOutput,
    playback: PlaybackState,
    active_generation: Option<u64>,
    status: &bluetooth::Status,
) -> TransportEpochState {
    let AudioOutput::Bluetooth(address) = output else {
        return TransportEpochState::Current;
    };
    if !matches!(playback, PlaybackState::Playing | PlaybackState::Buffering) {
        return TransportEpochState::Current;
    }
    let Some(active_generation) = active_generation else {
        return TransportEpochState::Current;
    };
    match status.playback_pcm(address) {
        Ok(pcm) if pcm.transport_generation == active_generation => TransportEpochState::Current,
        Ok(pcm) if pcm.transport_generation != 0 => TransportEpochState::Changed,
        _ => TransportEpochState::Unavailable,
    }
}

#[derive(Default, Clone, Copy)]
struct DirtyState {
    render: bool,
    persistence: bool,
}

impl DirtyState {
    fn mark_render(&mut self) {
        self.render = true;
    }

    fn mark_persistence(&mut self) {
        self.persistence = true;
    }

    fn mark_both(&mut self) {
        self.mark_render();
        self.mark_persistence();
    }

    fn rendered(&mut self) {
        self.render = false;
    }

    fn checkpoint_succeeded(&mut self) {
        self.persistence = false;
    }

    fn checkpoint_failed(&mut self) {
        self.persistence = true;
    }

    fn checkpoint_due(&self) -> bool {
        self.persistence
    }
}

fn run_sink_reconfiguration<P>(
    playback: &mut playback::Playback,
    generation: u64,
    plan: impl FnOnce() -> Result<P, String>,
    start: impl FnOnce(&mut playback::Playback, P) -> Result<(), String>,
) -> Result<(), String> {
    playback.stop_and_wait(generation)?;
    let plan = plan()?;
    start(playback, plan)
}

fn failed_reconfiguration_model(
    mut previous: AppModel,
    error: &str,
    sink_release_started: bool,
    failed_generation: Option<u64>,
) -> AppModel {
    if !sink_release_started {
        return previous;
    }
    let was_active = matches!(
        previous.playback,
        PlaybackState::Playing | PlaybackState::Buffering
    );
    let had_current = previous.current().is_some();
    previous.playback = if was_active && had_current {
        PlaybackState::Error
    } else if !had_current {
        PlaybackState::Stopped
    } else {
        previous.playback
    };
    previous.last_error = Some(error.to_owned());
    previous.generation = failed_generation
        .map(|generation| generation.wrapping_add(1))
        .unwrap_or_else(|| previous.generation.wrapping_add(1));
    previous
}

fn artwork_matches_current(
    model: &AppModel,
    generation: u64,
    entry_id: QueueEntryId,
    track_id: i64,
) -> bool {
    generation == model.generation
        && model.current_entry_id() == Some(entry_id)
        && model.current().is_some_and(|track| track.id == track_id)
}

impl Runtime {
    fn with_model_action(
        &mut self,
        action: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<(), String> {
        let outermost = self.action_depth == 0;
        if outermost {
            if self.pending_reconfiguration.is_some() {
                self.ui.flash("Audio output is still starting");
                self.dirty.mark_render();
                return Err("audio reconfiguration is still pending".into());
            }
            let tracks = std::mem::take(&mut self.model.library.tracks);
            let platform = std::mem::take(&mut self.model.platform);
            self.action_before = Some(self.model.clone());
            self.model.library.tracks = tracks;
            self.model.platform = platform;
            self.action_dirty_before = Some(self.dirty);
            self.reconfiguration_attempted = false;
            self.sink_release_started = false;
            self.transition_generation = None;
        }
        self.action_depth += 1;
        let result = action(self);
        self.action_depth -= 1;
        if outermost {
            let before = self.action_before.take().expect("outer action snapshot");
            let dirty_before = self
                .action_dirty_before
                .take()
                .expect("outer action dirty snapshot");
            let attempted = self.reconfiguration_attempted;
            let release_started = self.sink_release_started;
            let generation = self.transition_generation;
            let start_pending = self.playback.has_pending_start();
            self.reconfiguration_attempted = false;
            self.sink_release_started = false;
            self.transition_generation = None;
            if result.is_ok() && attempted && start_pending {
                self.pending_reconfiguration = Some(PendingReconfiguration {
                    previous: before,
                    dirty_before,
                    generation: generation.unwrap_or(self.model.generation),
                });
            } else if let Err(error) = &result {
                if attempted {
                    self.restore_failed_reconfiguration(
                        before,
                        dirty_before,
                        error,
                        release_started,
                        generation,
                    );
                }
            }
        }
        result
    }

    fn restore_failed_reconfiguration(
        &mut self,
        mut previous: AppModel,
        dirty_before: DirtyState,
        error: &str,
        sink_release_started: bool,
        transition_generation: Option<u64>,
    ) {
        previous.library.tracks = std::mem::take(&mut self.model.library.tracks);
        previous.platform = std::mem::take(&mut self.model.platform);
        if !sink_release_started {
            self.model = previous;
            self.dirty = dirty_before;
            return;
        }
        self.model = failed_reconfiguration_model(previous, error, true, transition_generation);
        self.playback.stop(self.model.generation);
        self.active_transport_generation = None;
        self.art = false;
        self.dirty.mark_both();
        self.checkpoint();
    }
    fn poll_pending_reconfiguration(&mut self) {
        if self.pending_reconfiguration.is_none() {
            return;
        }
        let epoch_state = transport_epoch_state(
            &self.model.output,
            self.model.playback,
            self.active_transport_generation,
            &self.bt_state,
        );
        if epoch_state != TransportEpochState::Current {
            let pending = self.pending_reconfiguration.take().unwrap();
            let error = match epoch_state {
                TransportEpochState::Changed => "Bluetooth transport changed during sink open",
                TransportEpochState::Unavailable => {
                    "Bluetooth transport disappeared during sink open"
                }
                TransportEpochState::Current => unreachable!(),
            };
            self.restore_failed_reconfiguration(
                pending.previous,
                pending.dirty_before,
                error,
                true,
                Some(pending.generation),
            );
            self.ui.flash(error);
            self.dirty.mark_render();
            return;
        }
        let Some(result) = self.playback.poll_start() else {
            return;
        };
        let Some(pending) = self.pending_reconfiguration.take() else {
            if let Err(error) = result {
                self.fail("playback", error);
            }
            return;
        };
        match result {
            Ok(()) => {
                if self.dirty.checkpoint_due() {
                    self.checkpoint();
                }
            }
            Err(error) => {
                self.restore_failed_reconfiguration(
                    pending.previous,
                    pending.dirty_before,
                    &error,
                    true,
                    Some(pending.generation),
                );
                self.ui.flash(format!("Playback could not start: {error}"));
                self.dirty.mark_render();
            }
        }
    }
    fn abandon_pending_reconfiguration(&mut self) {
        if let Some(pending) = self.pending_reconfiguration.take() {
            self.model = pending.previous;
            self.dirty = pending.dirty_before;
        }
    }

    fn play_index(&mut self, index: usize, force_shuffle: bool) -> Result<(), String> {
        self.play_members(
            index,
            (0..self.model.library.tracks.len()).collect(),
            force_shuffle,
        )
    }

    /// Internet radio requests from the Radio screens and Now Playing.
    fn radio(&mut self, request: reborn_core::RadioRequest) -> Result<(), String> {
        use reborn_core::RadioRequest;
        use reborn_platform::radio::{own_stations, Directory};
        let local = |ui: &mut Ui, stations: Vec<reborn_core::RadioStation>| {
            ui.radio.set_stations(stations);
            ui.radio.loading = false;
            ui.radio.error = None;
        };
        match request {
            RadioRequest::Favorites => local(&mut self.ui, self.radio_favorites.clone()),
            RadioRequest::Own => {
                let stations = own_stations(&self.storage_roots());
                local(&mut self.ui, stations);
            }
            RadioRequest::Play(index) => {
                let station = self
                    .ui
                    .radio
                    .stations
                    .get(index)
                    .cloned()
                    .ok_or("station not found")?;
                self.play_radio(station)?;
            }
            RadioRequest::ToggleFavorite(index) => {
                let station = self
                    .ui
                    .radio
                    .stations
                    .get(index)
                    .cloned()
                    .ok_or("station not found")?;
                self.toggle_favorite(station)?;
            }
            RadioRequest::ToggleFavoriteCurrent => {
                let station = self.radio_current.clone().ok_or("no station is playing")?;
                self.toggle_favorite(station)?;
            }
            RadioRequest::PlayStation(station) => self.play_radio(*station)?,
            RadioRequest::ToggleFavoriteStation(station) => self.toggle_favorite(*station)?,
            RadioRequest::Fm => {
                self.ui.radio.fm = self.fm_view();
                let stations = self.fm_stations();
                self.ui.radio.set_stations_in_order(stations);
                self.ui.radio.loading = false;
                self.ui.radio.error = None;
            }
            RadioRequest::FmScan => self.fm_scan()?,
            RadioRequest::FmCancel => {
                let _ = self.fm.cancel_scan();
            }
            network => {
                self.radio_sequence += 1;
                let sequence = self.radio_sequence;
                let reply = self.radio_replies.0.clone();
                let directory = self.radio_directory.clone();
                std::thread::Builder::new()
                    .name("radio-directory".into())
                    .spawn(move || {
                        // Mirror discovery (DNS) once, then reuse it.
                        let directory = {
                            let mut slot = directory.lock().unwrap_or_else(|e| e.into_inner());
                            slot.get_or_insert_with(|| Arc::new(Directory::discover()))
                                .clone()
                        };
                        let message = match network {
                            RadioRequest::Countries => {
                                RadioReply::Categories(sequence, directory.countries())
                            }
                            RadioRequest::Genres => {
                                RadioReply::Categories(sequence, directory.genres())
                            }
                            RadioRequest::Country { code, .. } => {
                                RadioReply::Stations(sequence, directory.by_country(&code))
                            }
                            RadioRequest::Genre(tag) => {
                                RadioReply::Stations(sequence, directory.by_genre(&tag))
                            }
                            RadioRequest::Search(name) => {
                                RadioReply::Stations(sequence, directory.by_name(&name))
                            }
                            _ => RadioReply::Stations(sequence, directory.popular()),
                        };
                        let _ = reply.send(message);
                    })
                    .map_err(|e| e.to_string())?;
            }
        }
        self.dirty.mark_render();
        Ok(())
    }
    /// A reply from the directory worker; replies to older requests are dropped.
    fn radio_reply(&mut self, reply: RadioReply) {
        let (sequence, error) = match reply {
            RadioReply::Stations(sequence, result) if sequence == self.radio_sequence => {
                match result {
                    Ok(stations) => {
                        self.refresh_favorites(&stations);
                        // A country's, a genre's and the popular list keep
                        // their ten most listened stations on top.
                        use reborn_core::RadioRequest::{Country, Genre, Popular};
                        match self.ui.radio.last_request {
                            Some(Country { .. } | Genre(_) | Popular) => {
                                self.ui.radio.set_stations_ranked(stations, TOP_STATIONS)
                            }
                            _ => self.ui.radio.set_stations(stations),
                        }
                        (sequence, None)
                    }
                    Err(e) => (sequence, Some(e)),
                }
            }
            RadioReply::Categories(sequence, result) if sequence == self.radio_sequence => {
                match result {
                    Ok(list) => {
                        self.ui.radio.set_categories(
                            list.into_iter()
                                .map(|c| reborn_ui::RadioCategory {
                                    id: c.id,
                                    name: c.name,
                                    stations: c.stations,
                                })
                                .collect(),
                        );
                        (sequence, None)
                    }
                    Err(e) => (sequence, Some(e)),
                }
            }
            RadioReply::FmScan(result) => {
                self.fm_scan_done(result);
                return;
            }
            _ => return,
        };
        let _ = sequence;
        self.ui.radio.loading = false;
        self.ui.radio.error = error;
        self.dirty.mark_render();
    }
    /// FM as the Radio menu shows it, from the tuner and the saved search.
    fn fm_view(&self) -> reborn_ui::FmView {
        use reborn_platform::fm::Availability;
        use reborn_ui::FmView;
        if self.fm_scanning {
            return FmView::Scanning {
                progress: self.fm.progress().unwrap_or(0),
            };
        }
        match self.fm.availability() {
            Availability::NoTuner => FmView::Hidden,
            Availability::NeedsFirmware => FmView::NeedsFirmware,
            Availability::Ready if !self.fm_saved.scanned => FmView::NotScanned,
            // A completed search found nothing: no reception on this player
            // (hidden from the next start; this session may search again).
            Availability::Ready if self.fm_saved.stations.is_empty() && !self.fm_searched_now => {
                FmView::Hidden
            }
            Availability::Ready => FmView::Ready {
                stations: self.fm_saved.stations.len(),
            },
        }
    }
    fn fm_stations(&self) -> Vec<reborn_core::RadioStation> {
        self.fm_saved
            .stations
            .iter()
            .map(|&k| reborn_platform::fm::station(k))
            .collect()
    }
    /// Search the band in a worker. FM playing stops first: the search
    /// turns the tuner's sound off while it runs.
    fn fm_scan(&mut self) -> Result<(), String> {
        if self.fm_scanning {
            return Ok(());
        }
        if self.fm.availability() != reborn_platform::fm::Availability::Ready {
            self.ui.radio.fm = self.fm_view();
            return Err("FM needs the tuner firmware".into());
        }
        if self
            .model
            .current()
            .is_some_and(|t| t.is_radio() && reborn_platform::fm::is_fm(&t.path.to_string_lossy()))
        {
            self.pause();
            self.model.playback = PlaybackState::Stopped;
        }
        self.fm_scanning = true;
        self.ui.radio.fm = self.fm_view();
        let reply = self.radio_replies.0.clone();
        let fm = self.fm.clone();
        std::thread::Builder::new()
            .name("fm-scan".into())
            .spawn(move || {
                let result = fm.scan().map_err(|e| match e.raw_os_error() {
                    Some(125) => "Search stopped".to_owned(), // ECANCELED
                    _ => format!("FM search failed: {e}"),
                });
                let _ = reply.send(RadioReply::FmScan(result));
            })
            .map_err(|e| e.to_string())?;
        self.log.emit(
            Level::Info,
            "fm",
            "scan_started",
            "FM search started",
            None,
            json!({}),
        );
        Ok(())
    }
    fn fm_scan_done(&mut self, result: Result<Vec<u32>, String>) {
        self.fm_scanning = false;
        self.fm_searched_now = true;
        match result {
            Ok(stations) => {
                self.log.emit(
                    Level::Info,
                    "fm",
                    "scan_done",
                    "FM search finished",
                    None,
                    json!({"stations": stations.len()}),
                );
                self.fm_saved = reborn_platform::fm::Saved {
                    scanned: true,
                    stations,
                };
                if let Err(e) =
                    reborn_platform::fm::save(&self.root.join("state/fm.json"), &self.fm_saved)
                {
                    self.ui.flash(format!("Couldn't save FM stations: {e}"));
                }
                if self.fm_saved.stations.is_empty() {
                    self.ui.flash("No FM stations found here");
                }
            }
            Err(error) => {
                self.log.emit(
                    Level::Warn,
                    "fm",
                    "scan_failed",
                    "FM search failed",
                    None,
                    json!({"error": error}),
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
    fn play_radio(&mut self, station: reborn_core::RadioStation) -> Result<(), String> {
        if !station.is_fm() && self.ui.wifi.status == WifiStatus::Disconnected {
            return Err("Connect to Wi-Fi to listen to radio".into());
        }
        self.model.replace_queue(vec![Track::radio(&station)], 0)?;
        self.radio_live = None;
        // Previous/Next move through the list the station was chosen from.
        if self
            .ui
            .radio
            .stations
            .iter()
            .any(|s| s.key() == station.key())
        {
            self.radio_list = self.ui.radio.stations.clone();
        } else if !self.radio_list.iter().any(|s| s.key() == station.key()) {
            self.radio_list.clear();
        }
        self.ui.radio.now = Some(station.clone());
        self.ui.radio.set_song(None);
        self.radio_art = None;
        self.radio_art_pending = None;
        self.art = false;
        self.load()?;
        if !station.uuid.is_empty() {
            // The directory counts plays for its popularity lists; best effort.
            let directory = self.radio_directory.clone();
            let uuid = station.uuid.clone();
            let _ = std::thread::Builder::new()
                .name("radio-click".into())
                .spawn(move || {
                    let directory = {
                        let mut slot = directory.lock().unwrap_or_else(|e| e.into_inner());
                        slot.get_or_insert_with(|| {
                            Arc::new(reborn_platform::radio::Directory::discover())
                        })
                        .clone()
                    };
                    let _ = directory.play_url(&uuid);
                });
        }
        self.log.emit(Level::Info, "radio", "station_started", "Radio station started", None,
            json!({"codec":station.codec,"bitrate":station.bitrate,"directory":!station.uuid.is_empty(),
                   "fm":station.is_fm()}));
        self.radio_current = Some(station);
        Ok(())
    }
    fn toggle_favorite(&mut self, station: reborn_core::RadioStation) -> Result<(), String> {
        let key = station.key().to_owned();
        if let Some(i) = self.radio_favorites.iter().position(|s| s.key() == key) {
            self.radio_favorites.remove(i);
            self.ui.flash("Removed from Favorites");
        } else {
            if self.radio_favorites.len() >= 200 {
                return Err("Favorites are full (200 stations)".into());
            }
            self.radio_favorites.push(station);
            self.ui.flash("Added to Favorites");
        }
        reborn_platform::radio::save_favorites(
            &self.root.join("state/radio.json"),
            &self.radio_favorites,
        )
        .map_err(|e| format!("Couldn't save favorites: {e}"))?;
        self.ui.radio.favorites = self
            .radio_favorites
            .iter()
            .map(|s| s.key().to_owned())
            .collect();
        // The Favorites list on screen follows at once.
        if self.ui.radio.last_request == Some(reborn_core::RadioRequest::Favorites) {
            self.ui.radio.set_stations(self.radio_favorites.clone());
        }
        Ok(())
    }
    /// Favorites saved before the directory sent logos and details get them
    /// when the station turns up in a list again.
    fn refresh_favorites(&mut self, fresh: &[reborn_core::RadioStation]) {
        let mut changed = false;
        for saved in &mut self.radio_favorites {
            if saved.uuid.is_empty() {
                continue;
            }
            if let Some(f) = fresh.iter().find(|f| f.uuid == saved.uuid) {
                if f != saved {
                    *saved = f.clone();
                    changed = true;
                }
            }
        }
        if changed {
            let _ = reborn_platform::radio::save_favorites(
                &self.root.join("state/radio.json"),
                &self.radio_favorites,
            );
        }
    }
    /// Previous/Next while a station plays: the neighbouring station of the
    /// list it was started from.
    fn zap(&mut self, delta: i32) -> Result<bool, String> {
        let Some(current) = self.radio_current.as_ref() else {
            return Ok(false);
        };
        let Some(at) = self
            .radio_list
            .iter()
            .position(|s| s.key() == current.key())
        else {
            return Ok(false);
        };
        let n = self.radio_list.len() as isize;
        if n < 2 {
            return Ok(false);
        }
        let next = self.radio_list[(at as isize + delta as isize).rem_euclid(n) as usize].clone();
        self.play_radio(next)?;
        Ok(true)
    }
    /// The live line under a playing station: its announced song, or how the
    /// connection is doing.
    fn radio_tick(&mut self) {
        // FM: scan progress and the menu row follow the tuner (cheap reads,
        // at most every two seconds; the signal line is a chip command).
        let due = self
            .fm_checked
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(2));
        if due {
            self.fm_checked = Some(Instant::now());
            let view = self.fm_view();
            if view != self.ui.radio.fm {
                self.ui.radio.fm = view;
                self.dirty.mark_render();
            }
        }
        let fm_khz = self
            .model
            .current()
            .filter(|t| t.is_radio())
            .and_then(|t| reborn_platform::fm::station_khz(&t.path.to_string_lossy()));
        if let Some(khz) = fm_khz {
            if self.ui.radio.now.is_none() {
                self.ui.radio.now = Some(reborn_platform::fm::station(khz));
            }
            if !due && self.radio_live.is_some() {
                return;
            }
            let line = match (self.model.playback, self.fm.signal()) {
                (PlaybackState::Playing, Some((_, true))) => "Stereo".to_owned(),
                (PlaybackState::Playing, Some((_, false))) => "Mono".to_owned(),
                _ => "FM".to_owned(),
            };
            if self.radio_live.as_deref() != Some(line.as_str()) {
                let position = self.model.queue_position;
                if let Some(track) = self.model.queue.get_mut(position) {
                    track.artist = line.clone();
                }
                self.radio_live = Some(line);
                self.dirty.mark_render();
            }
            return;
        }
        let radio = self.model.current().is_some_and(Track::is_radio);
        if !radio {
            if self.ui.radio.now.take().is_some() {
                self.ui.radio.set_song(None);
                self.radio_art = None;
                self.dirty.mark_render();
            }
            return;
        }
        use reborn_platform::radio::StreamState;
        let song = match reborn_platform::radio::live() {
            Some((StreamState::Playing, Some(title))) => Some(title),
            _ => None,
        };
        if self.ui.radio.song.is_some() != song.is_some()
            || song
                .as_deref()
                .is_some_and(|t| self.radio_live.as_deref() != Some(t))
        {
            self.ui.radio.set_song(song.as_deref());
        }
        let line = match reborn_platform::radio::live() {
            Some((StreamState::Connecting, _)) => "Connecting…".to_owned(),
            Some((StreamState::Reconnecting { attempt }, _)) => {
                format!("Reconnecting… (try {attempt})")
            }
            Some((StreamState::Failed(reason), _)) => reason,
            Some((StreamState::Playing, Some(title))) => title,
            Some((StreamState::Playing, None)) => "Live".to_owned(),
            None => return,
        };
        if self.radio_live.as_deref() != Some(line.as_str()) {
            let position = self.model.queue_position;
            if let Some(track) = self.model.queue.get_mut(position) {
                track.artist = line.clone();
            }
            self.radio_live = Some(line);
            self.dirty.mark_render();
        }
    }
    /// Playlist requests from the Playlist screens and song menus.
    fn playlist(&mut self, request: reborn_core::PlaylistRequest) -> Result<(), String> {
        use reborn_core::PlaylistRequest as R;
        let worker = self
            .playlist_worker
            .as_ref()
            .ok_or("playlists unavailable")?;
        let job = match request {
            R::Refresh => {
                self.ui.playlists.loading = true;
                playlists::Job::Refresh(self.storage_roots())
            }
            R::Create { name, tracks } => {
                let dir = self
                    .model
                    .settings
                    .music_directory
                    .join(reborn_platform::playlists::FOLDER);
                let file = reborn_platform::playlists::file_name_for(&name)
                    .ok_or("Type a name for the playlist")?;
                let path = dir.join(format!("{file}.m3u8"));
                let roots = self.storage_roots();
                let entries = tracks
                    .iter()
                    .filter_map(|&i| self.model.library.tracks.get(i))
                    .filter(|t| !t.is_radio())
                    .map(|t| reborn_core::playlist::PlaylistEntry {
                        location: reborn_ui::playlist_index::location_for(&t.path, &path, &roots),
                        info: Some(reborn_ui::playlist_index::info_for(t)),
                        extra: vec![],
                    })
                    .collect();
                playlists::Job::Create { dir, name, entries }
            }
            R::Edit { path, edit, create } => playlists::Job::Edit { path, edit, create },
            R::Rename { path, name } => playlists::Job::Rename { path, name },
            R::Duplicate(path) => playlists::Job::Duplicate(path),
            R::Delete(path) => playlists::Job::Delete(path),
        };
        worker.send(job);
        self.dirty.mark_render();
        Ok(())
    }
    fn playlist_reply(&mut self, reply: playlists::Reply) {
        match reply {
            playlists::Reply::Listed(lists) => self.ui.playlists.set_lists(lists),
            playlists::Reply::Saved { playlist, created } => {
                let path = playlist.path.clone();
                let name = playlist.name.clone();
                self.ui.playlists.replace(playlist);
                if created && !reborn_ui::playlists_is_favorites(&path) {
                    self.ui.flash(format!("Created “{name}”"));
                    if self.model.screen == Screen::Playlists {
                        self.ui.show_playlist(&mut self.model, &path);
                    }
                }
            }
            playlists::Reply::Renamed { old, playlist } => {
                let new = playlist.path.clone();
                self.ui.playlists.remove(&old);
                self.ui.playlists.replace(playlist);
                self.ui.playlist_moved(&mut self.model, &old, Some(&new));
            }
            playlists::Reply::Deleted(path) => {
                self.ui.playlists.remove(&path);
                self.ui.playlist_moved(&mut self.model, &path, None);
                self.ui.flash("Playlist deleted");
            }
            playlists::Reply::Failed(error) => {
                self.ui.playlists.loading = false;
                self.ui.flash(error);
            }
        }
        self.dirty.mark_render();
    }
    /// Internal Storage and online SD cards, where Themes and Radio live.
    fn storage_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.model.settings.music_directory.clone()];
        roots.extend(
            self.model
                .sources
                .iter()
                .filter(|s| s.online && matches!(s.kind, reborn_core::MediaSource::SdCard(_)))
                .map(|s| s.root.clone()),
        );
        roots
    }
    fn play_members(
        &mut self,
        index: usize,
        members: Vec<usize>,
        force_shuffle: bool,
    ) -> Result<(), String> {
        let selected = self
            .model
            .library
            .tracks
            .get(index)
            .cloned()
            .ok_or("invalid selection")?;
        if !selected.online {
            return Err("media source offline".into());
        }
        let mut queue = members
            .into_iter()
            .filter_map(|member| self.model.library.tracks.get(member).cloned())
            .filter(|track| track.online)
            .collect::<Vec<_>>();
        let selected_position = queue
            .iter()
            .position(|track| track.id == selected.id && track.path == selected.path)
            .ok_or("selected track is not in the playback collection")?;
        let use_shuffle = force_shuffle || self.model.settings.shuffle;
        if use_shuffle {
            let selected = queue.remove(selected_position);
            let mut seed = selected.id.unsigned_abs().wrapping_add(index as u64 + 1);
            for i in (1..queue.len()).rev() {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let j = (seed as usize % (i + 1)).min(i);
                queue.swap(i, j);
            }
            queue.insert(0, selected);
            self.model.replace_queue(queue, 0)?;
        } else {
            let selected_index = selected_position;
            self.model.replace_queue(queue, selected_index)?;
        }
        self.load()
    }

    fn fail(&mut self, sub: &str, error: String) {
        self.ui.flash(friendly_error(&error));
        self.log.emit(
            Level::Error,
            sub,
            "operation_failed",
            &error,
            None,
            json!({"state":self.model.playback,"recovery_attempted":false}),
        );
        self.dirty.mark_render();
    }
    fn prepare_sleep(&mut self) -> Result<Value, String> {
        if self.sleep_prepared {
            return Err("sleep_operation_in_progress".into());
        }
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            return Err("playback_active".into());
        }
        if self.playback.has_pending_start()
            || self.pending_reconfiguration.is_some()
            || self.model.library.scanning
            || self.query.is_some()
        {
            return Err("sleep_preparation_busy".into());
        }
        if !self.background.quiesce() {
            self.background.update(
                &self.model.settings.music_directory,
                &self.model.sources,
                false,
            );
            return Err("sleep_preparation_busy".into());
        }
        let scan_ready = self.scanner.quiesce();
        let art_ready = self.thumbs.as_mut().is_none_or(|worker| worker.quiesce());
        if !scan_ready || !art_ready {
            self.scanner.resume();
            self.background.update(
                &self.model.settings.music_directory,
                &self.model.sources,
                false,
            );
            return Err("sleep_preparation_busy".into());
        }
        let prepared = (|| {
            self.db.prepare_sleep(Duration::from_secs(2))?;
            self.model.platform.snapshot.sleep.phase =
                reborn_core::platform::SleepPhase::SleepPreparing;
            self.model
                .checkpoint(&self.root.join("state/session.json"))
                .map_err(|e| e.to_string())?;
            self.held_since = None;
            self.seek_until = None;
            self.playback.stop_and_wait(self.model.generation)?;
            self.effect_inner(Effect::ScreenSleep)?;
            Ok::<(), String>(())
        })();
        if let Err(error) = prepared {
            self.scanner.resume();
            self.background.update(
                &self.model.settings.music_directory,
                &self.model.sources,
                false,
            );
            return Err(error);
        }
        self.sleep_prepared = true;
        self.sleep_prepared_since = Some(Instant::now());
        Ok(json!({"sleep_ready":true,"screen_off":self.model.screen_off}))
    }

    fn restore_sleep(&mut self) -> Result<Value, String> {
        // A failed display restore must also discard the wake gesture before
        // input is accepted again, so its release cannot request another sleep.
        self.input_resume = true;
        self.sleep_prepared = false;
        self.sleep_prepared_since = None;
        self.scanner.resume();
        self.background.update(
            &self.model.settings.music_directory,
            &self.model.sources,
            false,
        );
        self.model.platform.snapshot.sleep.phase = reborn_core::platform::SleepPhase::Waking;
        self.effect_inner(Effect::ScreenWake)?;
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.render(
                &self
                    .ui
                    .draw(&self.model, &self.model.library.tracks, self.art),
            )?;
            self.graphics_recovery.presented();
        } else if !self.headless {
            return Err("display_restoration_failed".into());
        }
        self.playback_hints.clear();
        self.last_activity = Instant::now();
        self.dirty.mark_render();
        Ok(json!({"sleep_ready":true,"screen_off":self.model.screen_off}))
    }

    fn recreate_graphics(&mut self) -> Result<(), String> {
        self.art = false;
        // A new renderer starts without Cover Flow's full-resolution cover.
        self.ui.collection_art.set(None);
        if let Some(worker) = &mut self.thumbs {
            worker.forget_failures();
        }
        self.graphics = Some(Renderer::open(&reborn_ui::atlases(), self.log.clone())?);
        if let Some(skin) = self.ui.skin.clone() {
            self.upload_skin(&skin);
        }
        self.dirty.mark_render();
        Ok(())
    }

    /// Folders skins are installed in: the shipped ones first, then Skins (or
    /// Themes, its name before Beta 7.5) on Internal Storage and on the SD
    /// card (any letter case), as seen over MTP.
    fn skin_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![];
        if let Ok(dev) = std::env::var("REBORN_SKINS") {
            roots.push(PathBuf::from(dev));
        }
        roots.push(PathBuf::from("/usr/share/reborn/skins"));
        let mut storage = vec![self.model.settings.music_directory.clone()];
        storage.extend(
            self.model
                .sources
                .iter()
                .filter(|s| s.online && matches!(s.kind, reborn_core::MediaSource::SdCard(_)))
                .map(|s| s.root.clone()),
        );
        for dir in storage {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            roots.extend(
                entries
                    .flatten()
                    .filter(|e| {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        name.eq_ignore_ascii_case("skins") || name.eq_ignore_ascii_case("themes")
                    })
                    .map(|e| e.path()),
            );
        }
        roots
    }

    fn scan_themes(&mut self) {
        let mut themes = skins::scan(&self.skin_roots());
        // Keep the reason a skin was refused this session.
        for t in &mut themes {
            if let Some(old) = self
                .ui
                .themes
                .iter()
                .find(|o| o.id == t.id && o.problem.is_some())
            {
                t.problem = t.problem.take().or_else(|| old.problem.clone());
            }
        }
        self.ui.themes = themes;
        self.dirty.mark_render();
    }

    fn upload_skin(&mut self, skin: &reborn_ui::skin::Skin) -> bool {
        let Some(renderer) = self.graphics.as_mut() else {
            return true;
        };
        match renderer.skin_atlas(Some((&skin.atlas, skin.atlas_width, skin.atlas_height))) {
            Ok(()) => true,
            Err(e) => {
                self.log.emit(
                    Level::Warn,
                    "ui",
                    "skin_upload_failed",
                    &e,
                    None,
                    json!({"skin":skin.id}),
                );
                false
            }
        }
    }

    /// Switch to the skin `id` ("" = Reborn's own look). A skin that does not
    /// load leaves the current look in place and says why.
    fn apply_theme(&mut self, id: &str, announce: bool) -> Result<(), String> {
        if id.is_empty() {
            self.ui.skin = None;
            if let Some(renderer) = self.graphics.as_mut() {
                let _ = renderer.skin_atlas(None);
            }
            self.dirty.mark_render();
            return Ok(());
        }
        if !self.ui.themes.iter().any(|t| t.id == id) {
            self.scan_themes();
        }
        let info = self
            .ui
            .themes
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .ok_or_else(|| "That theme is no longer installed".to_owned())?;
        let started = Instant::now();
        let loaded = skins::load(&info.dir, &info.id, &|path, w, h| {
            reborn_media::decode_image(path, w, h)
        });
        let skin = match loaded {
            Ok(skin) => skin,
            Err(e) => {
                self.log.emit(
                    Level::Warn,
                    "ui",
                    "skin_refused",
                    &e,
                    None,
                    json!({"skin":info.id,"dir":info.dir}),
                );
                if let Some(t) = self.ui.themes.iter_mut().find(|t| t.id == id) {
                    t.problem = Some(e.clone());
                }
                return Err(format!("Couldn't use {}: {e}", info.name));
            }
        };
        let skin = std::rc::Rc::new(skin);
        if !self.upload_skin(&skin) {
            return Err(format!(
                "Couldn't use {}: not enough video memory",
                info.name
            ));
        }
        self.log.emit(
            Level::Info,
            "ui",
            "skin_loaded",
            &info.name,
            None,
            json!({"skin":info.id,"dir":info.dir,"load_ms":started.elapsed().as_secs_f64()*1000.,
                "atlas":[skin.atlas_width,skin.atlas_height],"layouts":skin.layout_names(),"warnings":skin.warnings}),
        );
        if announce {
            if let Some(w) = skin.warnings.first() {
                self.ui.flash(format!("{}: {w}", info.name));
            }
        }
        self.ui.skin = Some(skin);
        self.dirty.mark_render();
        Ok(())
    }

    fn checkpoint(&mut self) {
        self.dirty.mark_persistence();
        if self.playback.has_pending_start() || self.pending_reconfiguration.is_some() {
            return;
        }
        if let Err(e) = self.model.checkpoint(&self.root.join("state/session.json")) {
            self.dirty.checkpoint_failed();
            self.log.emit(
                Level::Warn,
                "core",
                "checkpoint_failed",
                &e.to_string(),
                None,
                json!({}),
            );
        } else {
            self.dirty.checkpoint_succeeded();
        }
    }
    fn load(&mut self) -> Result<(), String> {
        // Restore coordinator-owned cores before decoder/device startup.
        self.playback_hints
            .publish(reborn_platform::workload::playback_class(
                true,
                self.model.settings.crossfade_ms > 0 || self.model.settings.eq_enabled,
                false,
            ));
        self.reconfiguration_attempted = true;
        let track = self.model.current().cloned().ok_or("queue is empty")?;
        let radio = track.is_radio();
        if !radio && !track.path.is_file() {
            return Err("track source unavailable".into());
        }
        let _source = if radio {
            None
        } else {
            Some(storage::pin_track(
                &track,
                &self.model.settings.music_directory,
            )?)
        };
        let music_root = self.model.settings.music_directory.clone();
        let output = self.model.output.clone();
        let bluetooth_pcm = match &output {
            AudioOutput::Bluetooth(address) => Some(self.bt_state.playback_pcm(address)?),
            AudioOutput::Wired => None,
        };
        let requested_rate = match bluetooth_pcm.as_ref() {
            Some(pcm) => pcm.negotiated_rate()?,
            None if track.sample_rate >= 8_000 => track.sample_rate,
            None => self.output_rate()?,
        };
        let generation = self.model.generation.wrapping_add(1);
        self.transition_generation = Some(generation);
        let entry_id = self
            .model
            .current_entry_id()
            .unwrap_or(QueueEntryId(self.model.queue_position as u64 + 1));
        // A station is never pre-opened for gapless playback: that would start
        // a second network transfer.
        let (queue, queue_entry_ids) = if radio || self.model.settings.repeat == RepeatMode::Track {
            (Vec::new(), Vec::new())
        } else {
            (
                self.model
                    .queue
                    .iter()
                    .skip(self.model.queue_position + 1)
                    .cloned()
                    .collect(),
                self.model
                    .queue_entry_ids
                    .iter()
                    .skip(self.model.queue_position + 1)
                    .copied()
                    .collect(),
            )
        };
        let dsp = reborn_media::DspConfig {
            volume: self.model.settings.volume,
            replay_gain: self.model.settings.replay_gain,
            eq_enabled: self.model.settings.eq_enabled,
            eq_bands: self.model.settings.eq_bands.clone(),
            crossfade_ms: self.model.settings.crossfade_ms,
        };
        let request_parts = (
            track,
            entry_id,
            queue,
            queue_entry_ids,
            self.model.position_ms,
            generation,
            dsp,
            self.model.settings.gapless_enabled,
            self.log.correlation(),
        );
        let log = self.log.clone();
        let correlation = self.log.correlation();
        let plan_output = output.clone();
        self.sink_release_started = true;
        let playback = &mut self.playback;
        run_sink_reconfiguration(
            playback,
            generation,
            || {
                let plan = |rate| match bluetooth_pcm.as_ref() {
                    Some(pcm) => reborn_audio::AlsaSink::plan_bluetooth(
                        &plan_output,
                        pcm,
                        log.clone(),
                        correlation,
                    ),
                    None => {
                        reborn_audio::AlsaSink::plan(&plan_output, rate, log.clone(), correlation)
                    }
                };
                match plan(requested_rate) {
                    Ok((spec, _planned)) => Ok(spec),
                    Err(error)
                        if matches!(&plan_output, AudioOutput::Wired)
                            && requested_rate != 44_100 =>
                    {
                        log.emit(
                            Level::Warn,
                            "audio",
                            "enabled_rate_fallback",
                            "Native source rate unavailable; converting within its rate family",
                            Some(correlation),
                            json!({"source_rate":requested_rate,"selected_rate":reborn_audio::fallback_rate(requested_rate),"reason":error}),
                        );
                        plan(reborn_audio::fallback_rate(requested_rate))
                            .or_else(|_| plan(44_100))
                            .map(|(spec, _planned)| spec)
                    }
                    Err(error) => Err(error),
                }
            },
            move |playback, spec| {
                let (
                    track,
                    entry_id,
                    queue,
                    queue_entry_ids,
                    position,
                    generation,
                    dsp,
                    gapless_enabled,
                    id,
                ) = request_parts;
                playback.begin_start_with_gapless(playback::LoadRequest {
                    track,
                    music_root,
                    entry_id,
                    queue,
                    queue_entry_ids,
                    position,
                    spec,
                    generation,
                    dsp,
                    gapless_enabled,
                    id,
                })
            },
        )?;
        self.model.generation = generation;
        self.model.playback = PlaybackState::Buffering;
        self.active_transport_generation =
            bluetooth_pcm.as_ref().map(|pcm| pcm.transport_generation);
        self.art = false;
        self.dirty.mark_both();
        Ok(())
    }
    fn output_rate(&self) -> Result<u32, String> {
        match &self.model.output {
            AudioOutput::Wired => Ok(44100),
            AudioOutput::Bluetooth(address) => self.bt_state.playback_rate(address),
        }
    }
    /// Move the position by `delta` ms. While playing, the sound holds and
    /// playback restarts once at the target after the last step
    /// (`apply_seek`); while paused, the next Play starts there.
    fn seek_by(&mut self, delta: i64) {
        let Some(length) = self.model.current().map(|t| t.duration_ms) else {
            return;
        };
        self.model.position_ms = self
            .model
            .position_ms
            .saturating_add_signed(delta)
            .min(length.saturating_sub(1));
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.playback.hold(true);
            self.seek_until = Some(Instant::now() + SEEK_SETTLE);
        } else if self.playback.is_held() {
            // A held pause would continue from the old place.
            self.release_hold();
        }
        self.dirty.mark_both();
    }
    /// The collected seek: restart at the target once the steps stopped and
    /// no restart is in progress.
    fn apply_seek(&mut self) {
        if self
            .seek_until
            .is_none_or(|until| Instant::now() < until || self.pending_reconfiguration.is_some())
        {
            return;
        }
        self.seek_until = None;
        if !matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            return;
        }
        if let Err(error) = self.with_model_action(|runtime| runtime.load()) {
            self.fail("playback", error);
        }
    }
    /// Give the held pipeline back (after a long pause, before sleep).
    fn release_hold(&mut self) {
        self.release_hold_with(false);
    }
    /// `wait`: until the sink is closed. An open sink holds the platform's
    /// activity lease, which refuses sleep before Reborn is asked to prepare
    /// (Beta 6: Power while paused said "finishing another task").
    fn release_hold_with(&mut self, wait: bool) {
        self.held_since = None;
        if !self.playback.is_held() {
            return;
        }
        self.log.emit(
            Level::Info,
            "playback",
            "pause_released",
            "Paused playback released its sink",
            None,
            json!({"position_ms":self.model.position_ms,"for_sleep":wait}),
        );
        self.model.invalidate();
        if wait {
            if let Err(error) = self.playback.stop_and_wait(self.model.generation) {
                self.log.emit(
                    Level::Warn,
                    "playback",
                    "pause_release_slow",
                    "Paused sink did not close in time",
                    None,
                    json!({"error":error}),
                );
            }
        } else {
            self.playback.stop(self.model.generation);
        }
        self.active_transport_generation = None;
    }
    /// Continue a pause held in place: no decoder, file or device restart.
    fn resume_in_place(&mut self) -> bool {
        if self.model.playback != PlaybackState::Paused
            || !self.playback.is_held()
            || self.pending_reconfiguration.is_some()
        {
            return false;
        }
        self.playback_hints
            .publish(reborn_platform::workload::playback_class(
                true,
                self.model.settings.crossfade_ms > 0 || self.model.settings.eq_enabled,
                false,
            ));
        self.playback.hold(false);
        self.held_since = None;
        self.model.playback = PlaybackState::Playing;
        self.log.emit(
            Level::Info,
            "playback",
            "resumed",
            "Playback resumed in place",
            None,
            json!({"position_ms":self.model.position_ms}),
        );
        self.dirty.mark_both();
        self.checkpoint();
        true
    }
    fn pause(&mut self) {
        self.playback_hints.clear();
        // A file paused while it plays holds in place: resume is instant. A
        // station (live), a start in progress or a pending seek release.
        if self.model.playback == PlaybackState::Playing
            && self.pending_reconfiguration.is_none()
            && self.seek_until.is_none()
            && self.model.current().is_some_and(|t| !t.is_radio())
        {
            self.playback.hold(true);
            self.held_since = Some(Instant::now());
            self.model.playback = PlaybackState::Paused;
            self.log.emit(
                Level::Info,
                "playback",
                "paused",
                "Playback paused in place",
                None,
                json!({"position_ms":self.model.position_ms,"in_place":true}),
            );
            self.dirty.mark_both();
            self.checkpoint();
            return;
        }
        self.seek_until = None;
        self.held_since = None;
        self.abandon_pending_reconfiguration();
        self.log.emit(
            Level::Info,
            "playback",
            "paused",
            "Playback paused and sink released",
            None,
            json!({"position_ms":self.model.position_ms}),
        );
        self.model.invalidate();
        self.playback.stop(self.model.generation);
        self.active_transport_generation = None;
        self.model.playback = PlaybackState::Paused;
        self.dirty.mark_both();
        self.checkpoint();
    }
    fn fail_active_transport(&mut self, error: &str) {
        self.abandon_pending_reconfiguration();
        self.model.invalidate();
        self.playback.stop(self.model.generation);
        self.active_transport_generation = None;
        self.model.playback = PlaybackState::Error;
        self.model.last_error = Some(error.to_owned());
        self.art = false;
        self.dirty.mark_both();
        self.log.emit(
            Level::Error,
            "bluetooth",
            "transport_invalidated",
            error,
            None,
            json!({"recovery":"stopped_without_sink"}),
        );
        self.checkpoint();
    }
    fn toggle(&mut self) -> Result<(), String> {
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.pause();
            Ok(())
        } else if self.resume_in_place() {
            Ok(())
        } else {
            self.load()
        }
    }

    fn finish_track(&mut self) -> Result<(), String> {
        self.with_model_action(Self::finish_track_inner)
    }
    fn finish_track_inner(&mut self) -> Result<(), String> {
        match self.model.settings.repeat {
            reborn_core::RepeatMode::Track => {
                self.model.position_ms = 0;
                self.load()
            }
            reborn_core::RepeatMode::All => {
                if self.model.queue.is_empty() {
                    return Ok(());
                }
                if self.model.queue_position + 1 < self.model.queue.len() {
                    self.model.step(1);
                } else {
                    self.model.queue_position = 0;
                    self.model.position_ms = 0;
                    self.model.invalidate();
                }
                self.load()
            }
            reborn_core::RepeatMode::Off => {
                if self.model.queue_position + 1 < self.model.queue.len() {
                    self.model.step(1);
                    self.load()
                } else {
                    Ok(())
                }
            }
        }
    }
    fn switch(&mut self, out: AudioOutput) -> Result<(), String> {
        if let AudioOutput::Bluetooth(address) = &out {
            self.bt_state.playback_rate(address)?;
        }
        let active = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        if !active {
            self.active_transport_generation = None;
        }
        self.model.output = out;
        self.dirty.mark_both();
        if active {
            self.load()?
        }
        self.log.emit(
            Level::Info,
            "audio",
            "output_switch",
            "Output preference changed",
            Some(self.log.correlation()),
            json!({"output":self.model.output}),
        );
        self.checkpoint();
        self.dirty.mark_render();
        Ok(())
    }
    fn effect(&mut self, e: Effect) -> Result<(), String> {
        // Holding Previous/Next seeks every 90 ms: steps are collected and
        // applied once, so they never meet a restart still in progress.
        if let Effect::Seek(delta) = e {
            if !self.model.current().is_some_and(Track::is_radio) {
                self.seek_by(delta);
                return Ok(());
            }
        }
        // While a song is still starting (about a second on Bluetooth),
        // Previous/Next move on at once and the restart follows when the
        // start in progress ends; Play/Pause pauses. Beta 6 refused these
        // ("Couldn't complete that").
        if self.pending_reconfiguration.is_some() {
            match e {
                Effect::NextTrack | Effect::PreviousTrack => {
                    let delta = if matches!(e, Effect::NextTrack) {
                        1
                    } else {
                        -1
                    };
                    let target = self.model.queue_position as i64 + delta;
                    let file = !self.model.current().is_some_and(Track::is_radio);
                    if file && (0..self.model.queue.len() as i64).contains(&target) {
                        self.model.step(delta as i32);
                        self.playback.hold(true);
                        self.seek_until = Some(Instant::now());
                        self.dirty.mark_both();
                    }
                    return Ok(());
                }
                Effect::TogglePlayback => {
                    // A song change keeps the chosen song, paused; an output
                    // or codec change still goes back to what worked.
                    let song_change = self.pending_reconfiguration.as_ref().is_some_and(|p| {
                        p.previous.output == self.model.output
                            && serde_json::to_value(&p.previous.settings).ok()
                                == serde_json::to_value(&self.model.settings).ok()
                    });
                    if song_change {
                        self.pending_reconfiguration = None;
                    }
                    self.pause();
                    return Ok(());
                }
                _ => {}
            }
        }
        if matches!(
            e,
            Effect::None
                | Effect::Platform(_)
                | Effect::WifiPower
                | Effect::WifiScan
                | Effect::WifiConnect { .. }
                | Effect::WifiSaved(_)
                | Effect::WifiForget(_)
                | Effect::WifiDisconnect
                | Effect::BluetoothPower
                | Effect::BluetoothScan
                | Effect::ConfirmPairing(_)
                | Effect::ScreenSleep
                | Effect::ScreenWake
                | Effect::SetScreenTimeout(_)
        ) {
            return self.effect_inner(e);
        }
        self.with_model_action(|runtime| runtime.effect_inner(e))
    }
    fn effect_inner(&mut self, e: Effect) -> Result<(), String> {
        if self.sleep_prepared && !matches!(e, Effect::None) {
            return Err("sleep_operation_in_progress".into());
        }
        self.dirty.mark_render();
        match e {
            Effect::None => {}
            Effect::Platform(task) => {
                if self.model.platform.busy.is_some() {
                    return Ok(());
                }
                if task == reborn_core::PlatformTask::SleepRequest {
                    self.sleep_attempt = Some(Instant::now());
                    use reborn_core::platform::{SleepPhase, SleepProblem, SleepState};
                    if matches!(
                        self.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    ) {
                        self.model.platform.snapshot.sleep = SleepState {
                            phase: SleepPhase::SleepRefused,
                            problem: Some(SleepProblem::PlaybackActive),
                            ..Default::default()
                        };
                        return Ok(());
                    }
                    // A pause held in place keeps the sink, and its activity
                    // lease, open: give it back first so the platform's lock
                    // check passes (Play then restarts at the same place).
                    self.release_hold_with(true);
                    self.checkpoint();
                }
                let worker = self.platform.as_ref().ok_or("System service is starting")?;
                worker
                    .commands
                    .try_send(task)
                    .map_err(|_| "busy: another operation is still running")?;
                self.model.platform.busy = Some(task);
                if task == reborn_core::PlatformTask::SleepRequest {
                    self.model.platform.snapshot.sleep.phase =
                        reborn_core::platform::SleepPhase::SleepRequested;
                }
                if task != reborn_core::PlatformTask::Refresh {
                    self.model.platform.failure = None;
                    self.model.platform.result = None;
                }
                self.dirty.mark_render();
            }
            Effect::StopPlayback => {
                self.pause();
                self.model.playback = PlaybackState::Stopped;
            }
            Effect::CancelDownload => client::abort_download(),
            Effect::Radio(request) => self.radio(request)?,
            Effect::Playlist(request) => self.playlist(*request)?,
            Effect::SetCodecPreference(preference) => {
                if !codec_change_allowed(
                    self.model.playback,
                    self.pending_reconfiguration.is_some(),
                ) {
                    return Err("Pause music before changing codec".into());
                }
                let address = self
                    .bt_state
                    .devices
                    .iter()
                    .find(|d| d.connected && d.audio)
                    .map(|d| d.address.clone())
                    .ok_or("Connect Bluetooth headphones first")?;
                self.bluetooth
                    .as_ref()
                    .ok_or("Bluetooth unavailable")?
                    .commands
                    .try_send(bluetooth::Command::Codec {
                        address,
                        preference,
                    })
                    .map_err(|_| "Bluetooth busy")?;
                self.model.settings.codec_preference = preference;
                self.dirty.mark_both();
                self.ui
                    .flash("Codec preference saved. It applies when the device reconnects audio.");
            }
            Effect::WifiDisconnect => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Disconnect)
                .map_err(|e| e.to_string())?,
            Effect::Play(index) => self.play_index(index, false)?,
            Effect::PlayShuffled(index) => self.play_index(index, true)?,
            Effect::PlayCollection {
                selected,
                members,
                shuffle,
            } => self.play_members(selected, members, shuffle)?,
            Effect::PlayQueue(index) => {
                if index >= self.model.queue.len() {
                    return Err("invalid queue entry".into());
                }
                self.model.queue_position = index;
                self.model.position_ms = 0;
                self.load()?;
            }
            Effect::PlayNext(index) => {
                let track = self
                    .model
                    .library
                    .tracks
                    .get(index)
                    .cloned()
                    .ok_or("invalid selection")?;
                if !track.online {
                    return Err("media source offline".into());
                }
                if self.model.queue.len() >= reborn_core::MAX_QUEUE {
                    return Err("queue is full".into());
                }
                let was_empty = self.model.queue.is_empty();
                let insert_at = if was_empty {
                    0
                } else {
                    (self.model.queue_position + 1).min(self.model.queue.len())
                };
                self.model.insert_queue_entry(insert_at, track)?;
                if was_empty {
                    self.model.queue_position = 0;
                    self.load()?;
                } else if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::PlayNextCollection(members) => {
                let tracks = members
                    .into_iter()
                    .filter_map(|index| self.model.library.tracks.get(index).cloned())
                    .filter(|track| track.online)
                    .collect::<Vec<_>>();
                if tracks.is_empty() {
                    return Err("collection has no online tracks".into());
                }
                let was_empty = self.model.queue.is_empty();
                if self.model.queue.len().saturating_add(tracks.len()) > reborn_core::MAX_QUEUE {
                    return Err("queue is full".into());
                }
                let start = if was_empty {
                    0
                } else {
                    (self.model.queue_position + 1).min(self.model.queue.len())
                };
                for (insert_at, track) in (start..).zip(tracks) {
                    self.model.insert_queue_entry(insert_at, track)?;
                }
                self.model.queue_position = if was_empty {
                    0
                } else {
                    self.model.queue_position
                };
                if was_empty
                    || matches!(
                        self.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    )
                {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::AddToQueue(index) => {
                let track = self
                    .model
                    .library
                    .tracks
                    .get(index)
                    .cloned()
                    .ok_or("invalid selection")?;
                if self.model.queue.len() >= reborn_core::MAX_QUEUE {
                    return Err("queue is full".into());
                }
                let was_empty = self.model.queue.is_empty();
                let insert_at = self.model.queue.len();
                self.model.insert_queue_entry(insert_at, track)?;
                if was_empty {
                    self.model.queue_position = 0;
                } else if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::AddToQueueCollection(members) => {
                let tracks = members
                    .into_iter()
                    .filter_map(|index| self.model.library.tracks.get(index).cloned())
                    .filter(|track| track.online)
                    .collect::<Vec<_>>();
                if tracks.is_empty() {
                    return Err("collection has no online tracks".into());
                }
                if self.model.queue.len().saturating_add(tracks.len()) > reborn_core::MAX_QUEUE {
                    return Err("queue is full".into());
                }
                let was_empty = self.model.queue.is_empty();
                let start = self.model.queue.len();
                for (insert_at, track) in (start..).zip(tracks) {
                    self.model.insert_queue_entry(insert_at, track)?;
                }
                if was_empty {
                    self.model.queue_position = 0;
                } else if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::TogglePlayback => self.toggle()?,
            Effect::NextTrack | Effect::PreviousTrack => {
                let delta = if matches!(e, Effect::NextTrack) {
                    1
                } else {
                    -1
                };
                // A station has no next track: the next station of its list.
                if self.model.current().is_some_and(Track::is_radio) {
                    self.zap(delta)?;
                } else if self.model.step(delta) {
                    self.load()?
                } else {
                    self.model.invalidate();
                    self.playback.stop(self.model.generation);
                }
            }
            // A live station has nothing to seek in.
            Effect::Seek(_) if self.model.current().is_some_and(Track::is_radio) => {}
            Effect::Seek(delta) => self.seek_by(delta),
            Effect::AdjustVolume(delta) => {
                self.model.settings.volume =
                    (self.model.settings.volume as i16 + delta as i16).clamp(0, 100) as u8;
                self.playback.set_volume(self.model.settings.volume);
                self.ui.show_volume();
                self.dirty.mark_render();
                // A hold may adjust every 90 ms. Keep the atomic audio target
                // immediate; persist on the existing bounded checkpoint and
                // shutdown boundaries instead of fsyncing each step.
                self.dirty.mark_persistence();
            }
            Effect::ScanLibrary => {
                if self.model.library.scanning {
                    return Ok(());
                }
                self.model.apply(Event::LibraryScanStarted);
                if let Err(error) = self.scanner.scan(self.model.sources.clone()) {
                    self.model.apply(Event::LibraryScanFailed(error.clone()));
                    return Err(error);
                }
                self.ui.flash("Scanning for music");
            }
            Effect::WifiPower => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi service unavailable")?
                .commands
                .try_send(wifi::Command::Power(!self.wifi_state.enabled))
                .map_err(|_| "Wi-Fi busy")?,
            Effect::WifiScan => {
                if self.ui.wifi.scan.active() {
                    return Ok(());
                }
                self.wifi
                    .as_ref()
                    .ok_or("Wi-Fi unavailable")?
                    .commands
                    .try_send(wifi::Command::Scan)
                    .map_err(|_| "Wi-Fi busy")?;
                self.ui.wifi.scan = reborn_core::RadioScan::Starting;
                self.ui.wifi.problem = None;
                self.wifi_state.scan = reborn_core::RadioScan::Starting;
            }
            Effect::WifiConnect { ssid, password } => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Connect { ssid, password })
                .map_err(|_| "Wi-Fi busy")?,
            Effect::WifiSaved(id) => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Saved(id))
                .map_err(|_| "Wi-Fi busy")?,
            Effect::WifiForget(id) => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Forget(id))
                .map_err(|_| "Wi-Fi busy")?,
            Effect::BluetoothPower => self
                .bluetooth
                .as_ref()
                .ok_or("Bluetooth unavailable")?
                .commands
                .try_send(bluetooth::Command::Power(!self.bt_state.powered))
                .map_err(|_| "Bluetooth busy")?,
            Effect::BluetoothScan => {
                if self.ui.bluetooth.scan.active() {
                    return Ok(());
                }
                self.bluetooth
                    .as_ref()
                    .ok_or("Bluetooth unavailable")?
                    .commands
                    .try_send(bluetooth::Command::Scan(true))
                    .map_err(|_| "Bluetooth busy")?;
                self.ui.bluetooth.scan = reborn_core::RadioScan::Starting;
                self.ui.bluetooth.problem = None;
                self.bt_state.scan = reborn_core::RadioScan::Starting;
            }
            Effect::BluetoothDevice { path, operation } => {
                if operation == "output" {
                    let address = self
                        .bt_state
                        .devices
                        .iter()
                        .find(|d| d.path == path)
                        .ok_or("device not found")?
                        .address
                        .clone();
                    self.switch(AudioOutput::Bluetooth(address))?;
                } else {
                    let cmd = match operation.as_str() {
                        "pair" => bluetooth::Command::Pair(path),
                        "connect" => bluetooth::Command::Connect(path),
                        "disconnect" => bluetooth::Command::Disconnect(path),
                        "forget" => bluetooth::Command::Forget(path),
                        _ => return Err("unsupported Bluetooth action".into()),
                    };
                    self.bluetooth
                        .as_ref()
                        .ok_or("Bluetooth unavailable")?
                        .commands
                        .try_send(cmd)
                        .map_err(|_| "Bluetooth busy")?;
                }
            }
            Effect::Output(o) => self.switch(o)?,
            Effect::ConfirmPairing(ok) => self
                .bluetooth
                .as_ref()
                .ok_or("Bluetooth unavailable")?
                .commands
                .try_send(bluetooth::Command::Confirm(ok))
                .map_err(|_| "Bluetooth busy")?,
            Effect::ScreenSleep => {
                self.interaction_hints.clear();
                if self.model.screen_off {
                    return Ok(());
                }
                if !self.headless {
                    power::blank(true)?;
                    if let Some(graphics) = self.graphics.as_mut() {
                        if let Err(error) = graphics.sleep(true) {
                            let _ = power::blank(false);
                            return Err(error);
                        }
                    }
                }
                self.model.apply(Event::ScreenSleep);
                self.dark_since = Some(Instant::now());
                self.wifi_idle_failed = false;
                self.log.emit(
                    Level::Info,
                    "power",
                    "screen_blank",
                    "Display put to sleep",
                    None,
                    json!({"playback":self.model.playback}),
                );
            }
            Effect::ScreenWake => {
                if !self.model.screen_off && (self.headless || self.graphics.is_some()) {
                    return Ok(());
                }
                if !self.headless {
                    self.graphics_recovery.wake();
                    if self.graphics.is_none() {
                        self.recreate_graphics()?;
                    }
                    if let Some(graphics) = self.graphics.as_mut() {
                        graphics.sleep(false)?;
                    }
                    if let Err(error) = power::blank(false) {
                        if let Some(graphics) = self.graphics.as_mut() {
                            let _ = graphics.sleep(true);
                        }
                        return Err(error);
                    }
                }
                self.model.apply(Event::ScreenWake);
                self.last_activity = Instant::now();
                self.dark_since = None;
                self.wifi_idle_restore();
                self.log.emit(
                    Level::Info,
                    "power",
                    "screen_wake",
                    "Display woke",
                    None,
                    json!({"playback":self.model.playback}),
                );
            }
            Effect::SetReplayGain(mode) => {
                self.model.settings.replay_gain = mode;
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::SetEqEnabled(enabled) => {
                if self.model.settings.eq_bands.is_empty() {
                    self.model.settings.eq_bands = reborn_core::flat_eq_bands();
                }
                self.model.settings.eq_enabled = enabled;
                self.reload_dsp()?;
            }
            Effect::SetEqBandGain { index, gain_db } => {
                if self.model.settings.eq_bands.is_empty() {
                    self.model.settings.eq_bands = reborn_core::flat_eq_bands();
                }
                let band = self
                    .model
                    .settings
                    .eq_bands
                    .get_mut(index)
                    .filter(|_| index < 8)
                    .ok_or("That equalizer band has changed")?;
                band.gain_db = f32::from(gain_db.clamp(-12, 12));
                if self.model.settings.eq_enabled {
                    self.reload_dsp()?;
                } else {
                    self.checkpoint();
                }
            }
            Effect::SetEqPreset(gains) => {
                let mut bands = reborn_core::flat_eq_bands();
                for (band, gain) in bands.iter_mut().zip(gains) {
                    band.gain_db = f32::from(gain.clamp(-12, 12));
                }
                self.model.settings.eq_bands = bands;
                self.model.settings.eq_enabled = gains.iter().any(|g| *g != 0);
                self.reload_dsp()?;
            }
            Effect::ResetEq => {
                self.model.settings.eq_bands = reborn_core::flat_eq_bands();
                self.reload_dsp()?;
            }
            Effect::SetBrightness(percent) => {
                self.model.settings.brightness = percent.clamp(10, 100);
                if !self.headless {
                    power::set_brightness(self.model.settings.brightness)?;
                }
                self.checkpoint();
            }
            Effect::ResetSettings => {
                // Volume and the music location are part of the session, not
                // preferences a reset should surprise the listener with.
                let defaults = reborn_core::Settings {
                    volume: self.model.settings.volume,
                    music_directory: self.model.settings.music_directory.clone(),
                    ..Default::default()
                };
                let reload = self.model.settings.gapless_enabled != defaults.gapless_enabled
                    || self.model.settings.eq_enabled
                    || self.model.settings.replay_gain != defaults.replay_gain
                    || self.model.settings.crossfade_ms != defaults.crossfade_ms
                    || self.model.settings.repeat != defaults.repeat;
                self.model.settings = defaults;
                if self.ui.skin.is_some() {
                    let _ = self.apply_theme("", false);
                }
                if !self.headless && self.model.platform.brightness_available {
                    let _ = power::set_brightness(self.model.settings.brightness);
                }
                if reload
                    && matches!(
                        self.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    )
                {
                    self.load()?;
                }
                self.checkpoint();
                self.ui.flash("Settings reset");
            }
            Effect::ForgetAllWifi => {
                let worker = self.wifi.as_ref().ok_or("Wi-Fi unavailable")?;
                let ids: Vec<u32> = self
                    .wifi_state
                    .saved
                    .iter()
                    .filter_map(|n| n.saved_id)
                    .collect();
                let queued = ids
                    .iter()
                    .filter(|id| {
                        worker
                            .commands
                            .try_send(wifi::Command::Forget(**id))
                            .is_ok()
                    })
                    .count();
                self.ui.flash(if queued == ids.len() {
                    "Wi-Fi networks forgotten"
                } else {
                    "Some networks are still saved. Try again."
                });
            }
            Effect::ForgetAllBluetooth => {
                let worker = self.bluetooth.as_ref().ok_or("Bluetooth unavailable")?;
                let paths: Vec<String> = self
                    .bt_state
                    .devices
                    .iter()
                    .filter(|d| d.paired)
                    .map(|d| d.path.clone())
                    .collect();
                let queued = paths
                    .iter()
                    .filter(|path| {
                        worker
                            .commands
                            .try_send(bluetooth::Command::Forget((*path).clone()))
                            .is_ok()
                    })
                    .count();
                self.ui.flash(if queued == paths.len() {
                    "Bluetooth pairings removed"
                } else {
                    "Some pairings remain. Try again."
                });
            }
            Effect::ClearCache => {
                let removed = clear_cache(&self.root.join("cache"));
                self.art = false;
                self.dirty.mark_render();
                self.ui.flash(if removed.is_ok() {
                    "Cache cleared"
                } else {
                    "Couldn't clear everything. Try again."
                });
            }
            Effect::SetCrossfade(milliseconds) => {
                self.model.settings.crossfade_ms = milliseconds.min(30_000);
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::SetGapless(enabled) => {
                self.model.settings.gapless_enabled = enabled;
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::ScanThemes => self.scan_themes(),
            Effect::SetTheme(id) => match self.apply_theme(&id, true) {
                Ok(()) => {
                    self.model.settings.theme = id;
                    self.checkpoint();
                }
                Err(e) => self.ui.flash(e),
            },
            Effect::SetAlbumView(view) => {
                self.model.settings.album_view = view;
                self.checkpoint();
            }
            Effect::SetShuffle(enabled) => {
                self.model.settings.shuffle = enabled;
                self.checkpoint();
            }
            Effect::SetRepeat(mode) => {
                self.model.settings.repeat = mode;
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::SetScreenTimeout(seconds) => {
                self.model.settings.screen_timeout_seconds = seconds;
                self.checkpoint();
            }
            Effect::SetTimeZone(zone) => {
                // A zone chosen by hand ends Automatic (Beta 7.1).
                self.model.settings.time_zone = zone;
                self.model.settings.time_zone_auto = Some(false);
                self.checkpoint();
            }
            Effect::SetTimeZoneAuto(on) => {
                self.model.settings.time_zone_auto = Some(on);
                self.zone_lookup_again = on;
                self.checkpoint();
            }
            Effect::SetHomePin(key, on) => {
                self.model.settings.pin_home(&key, on);
                self.checkpoint();
            }
            Effect::QueueRemove(index) => {
                if index == self.model.queue_position {
                    self.ui.flash("The current track stays in the queue");
                } else if index < self.model.queue.len()
                    && self.model.remove_queue_entry(index).is_some()
                {
                    let active = matches!(
                        self.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    );
                    if active {
                        self.load()?;
                    }
                    self.checkpoint();
                }
            }
            Effect::QueueMove { index, delta } => {
                let Some(next) = index.checked_add_signed(delta as isize) else {
                    return Ok(());
                };
                if index > self.model.queue_position
                    && next > self.model.queue_position
                    && next < self.model.queue.len()
                {
                    self.model.move_queue_entry(index, next);
                    if matches!(
                        self.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    ) {
                        self.load()?;
                    }
                    self.checkpoint();
                }
            }
            Effect::ClearQueue => {
                if self.model.clear_future_queue() {
                    if matches!(
                        self.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    ) {
                        self.load()?;
                    }
                    self.checkpoint();
                }
            }
            Effect::RebuildLibrary => {
                self.model.apply(Event::LibraryScanStarted);
                if let Err(error) = self.scanner.rebuild(self.model.sources.clone()) {
                    self.model.apply(Event::LibraryScanFailed(error.clone()));
                    return Err(error);
                }
                self.ui.flash("Rebuilding library");
            }
            Effect::PowerOff => {
                power::request_shutdown(false)?;
            }
            Effect::Reboot => {
                power::request_shutdown(true)?;
            }
            Effect::Checkpoint => self.checkpoint(),
        };
        Ok(())
    }
    /// Apply typed replies from the platform client.
    fn reload_dsp(&mut self) -> Result<(), String> {
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.load()?;
        }
        self.checkpoint();
        Ok(())
    }

    fn poll_platform(&mut self) {
        let Some(worker) = &self.platform else {
            return;
        };
        let Ok(reply) = worker.replies.try_recv() else {
            return;
        };
        let mut refresh = false;
        let mut blank_screen = false;
        // The one-step update installs right after its download (Beta 7.1).
        let mut install_next = false;
        let p = &mut self.model.platform;
        match reply {
            client::Reply::Sleep(result) => {
                p.busy = None;
                match result {
                    Ok(state) => {
                        // Full sleep is gated (not yet qualified) or refused while
                        // charging. Power must still save display power; the page keeps saying the player
                        // stayed awake, so this never implies deep sleep.
                        blank_screen = state.refused_but_screen_can_blank();
                        if state.phase == reborn_core::platform::SleepPhase::SleepRefused {
                            self.log.emit(
                                Level::Info,
                                "power",
                                "sleep_refused",
                                "Platform refused sleep",
                                None,
                                json!({"problem":format!("{:?}", state.problem)}),
                            );
                        }
                        p.snapshot.sleep = state;
                        p.failure = None;
                    }
                    Err(error) => {
                        p.snapshot.sleep.phase = reborn_core::platform::SleepPhase::SleepRefused;
                        p.snapshot.sleep.problem = Some(reborn_core::platform::SleepProblem::Other);
                        p.failure = Some(error.clone());
                        self.ui.flash(error);
                    }
                }
            }
            client::Reply::Snapshot(snapshot) => {
                self.platform_observed = true;
                self.platform_refresh_pending = false;
                let health = p.snapshot.health;
                let health_section = p.snapshot.section("health").cloned();
                let firmware_before = p.snapshot.radio_firmware;
                p.snapshot = *snapshot;
                // The status call took ≈1.6 s: the live cable state is newer.
                let usb = client::usb_now();
                if usb != reborn_core::platform::UsbTransfer::Unknown {
                    p.snapshot.usb = usb;
                }
                p.snapshot.health = health;
                {
                    use reborn_core::platform::RadioFirmware::*;
                    if matches!(firmware_before, Missing | Installing | Failed)
                        && p.snapshot.radio_firmware == Ready
                    {
                        self.ui.flash("Wi-Fi and Bluetooth are ready");
                    }
                }
                p.snapshot.diagnostics.extend(health_section);
                if let reborn_core::platform::UpdatePhase::Available { version } =
                    &p.snapshot.update.phase
                {
                    if self.announced_update.as_ref() != Some(version) {
                        self.announced_update = Some(version.clone());
                        self.ui.flash("Software update available");
                        self.dirty.mark_render();
                    }
                }
                if p.busy == Some(reborn_core::PlatformTask::Refresh) {
                    p.busy = None;
                }
            }
            client::Reply::Health(level, section) => {
                p.busy = None;
                p.snapshot.health = level;
                p.snapshot.diagnostics.retain(|s| s.id != "health");
                p.snapshot.diagnostics.push(section);
            }
            client::Reply::Operation(task, result) => {
                use reborn_core::PlatformTask::*;
                p.busy = None;
                refresh = true;
                match result {
                    Ok(result) => {
                        if task == UpdateStage {
                            p.download = None;
                        }
                        if !result.succeeded {
                            p.failure =
                                Some("The operation did not finish. See Latest Result.".into());
                            p.result = Some(result);
                            self.ui
                                .flash("The operation did not finish. See Latest Result.");
                            self.dirty.mark_render();
                            return;
                        }
                        p.failure = None;
                        p.result = Some(result);
                        match task {
                            UpdateStage => {
                                // Ready at once, wherever the owner is; the
                                // refresh that follows confirms the journal.
                                let u = &mut p.snapshot.update;
                                u.phase = reborn_core::platform::UpdatePhase::ReadyToInstall;
                                u.can_install = true;
                                u.can_cancel = true;
                                u.can_download = false;
                                if self.ui.update_run {
                                    install_next = true;
                                } else {
                                    self.ui.flash("Update downloaded. Ready to install.");
                                }
                            }
                            UpdateApply if self.ui.update_run => {
                                // The platform restarts into the installer.
                                self.ui.update_restarting = true;
                            }
                            UpdateCheck => {
                                // The check's own answer, at once: no wait for
                                // the status refresh behind it (Beta 7.1).
                                use reborn_core::platform::UpdatePhase;
                                let fact = |label: &str| {
                                    p.result.as_ref().and_then(|r| {
                                        r.facts
                                            .iter()
                                            .find(|f| f.label == label)
                                            .map(|f| f.value.clone())
                                    })
                                };
                                let (state, release) = (fact("State"), fact("Release"));
                                let u = &mut p.snapshot.update;
                                if state.as_deref() == Some("UpToDate") {
                                    u.phase = UpdatePhase::UpToDate;
                                    u.can_download = false;
                                } else if let Some(version) = release {
                                    u.phase = UpdatePhase::Available { version };
                                    u.can_download = true;
                                }
                            }
                            UpdateCancel => {}
                            Export => self.ui.flash("Player data exported"),
                            LdacQuality(_) | LdacAbr(_) | SbcQuality(_) | AacQuality(_)
                            | AacAbr(_) => {
                                self.ui.flash("Quality saved. Restart the player to apply.")
                            }
                            DiagnosticsExport => {
                                self.ui.flash("Diagnostic report saved. See Latest Result.");
                                self.ui.show_operation_result(&mut self.model);
                            }
                            _ => self.ui.flash("Check finished. See Latest Result."),
                        }
                    }
                    Err(error) => {
                        // The update page presents its own journaled reason.
                        if matches!(task, UpdateStage | UpdateApply) {
                            self.ui.update_run = false;
                            self.ui.update_restarting = false;
                        }
                        if matches!(task, UpdateApply | UpdateRollback) {
                            self.ui.flash(install_refusal(&error));
                        } else if task == UpdateStage && error.contains("download_cancelled") {
                            p.download = None;
                            self.ui.flash("Update cancelled. Nothing was changed.");
                        } else if task == UpdateStage {
                            p.download = None;
                            self.ui
                                .flash("The download didn't finish. Nothing was changed.");
                        } else if !matches!(task, UpdateCheck) {
                            self.ui.flash(error.clone());
                        }
                        p.failure = Some(error);
                    }
                }
            }
            client::Reply::Failed(task, error) => {
                if task == reborn_core::PlatformTask::UpdateStage {
                    p.download = None;
                }
                if task == reborn_core::PlatformTask::Refresh {
                    self.platform_refresh_pending = false;
                    // A stale observation cannot keep a control enabled.
                    p.snapshot.observed = false;
                    p.snapshot.update = Default::default();
                } else {
                    self.ui.flash(error.clone());
                }
                if p.busy == Some(task) {
                    p.busy = None;
                }
                p.failure = Some(error);
            }
        }
        if blank_screen && !self.model.screen_off {
            if let Err(error) = self.effect(Effect::ScreenSleep) {
                self.ui.flash(error);
            }
        }
        if install_next {
            if let Err(error) =
                self.effect(Effect::Platform(reborn_core::PlatformTask::UpdateApply))
            {
                self.ui.update_run = false;
                self.ui.flash(error);
            }
            self.dirty.mark_render();
            return;
        }
        // Operations change platform state; observe it again.
        if refresh {
            if let Some(worker) = &self.platform {
                if worker
                    .commands
                    .try_send(reborn_core::PlatformTask::Refresh)
                    .is_ok()
                {
                    self.platform_refresh_pending = true;
                }
            }
        }
        self.dirty.mark_render();
    }

    /// Block Wi-Fi at runtime once the player has been dark and idle on battery
    /// long enough. The saved preference is untouched; wake restores the radio.
    fn wifi_idle_tick(&mut self) {
        let Some(since) = self.dark_since else {
            return;
        };
        if self.headless || self.wifi_idle_off || self.wifi_idle_failed {
            return;
        }
        let playing = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        let busy = self.model.platform.busy.is_some() || self.ui.wifi.scan.active();
        let on_battery =
            self.model.platform.battery.charging == reborn_core::platform::ChargingState::OnBattery;
        if !reborn_core::platform::wifi_idle_power_down(
            since.elapsed().as_secs(),
            self.wifi_state.enabled,
            playing,
            on_battery,
            busy,
        ) {
            return;
        }
        match wifi::radio(false, false) {
            Ok(()) => {
                self.wifi_idle_off = true;
                self.log.emit(
                    Level::Info,
                    "wifi",
                    "idle_power_down",
                    "Wi-Fi blocked while the player is dark and idle",
                    None,
                    json!({"dark_seconds":since.elapsed().as_secs()}),
                );
            }
            Err(error) => {
                // Do not retry every tick; the next screen-off tries again.
                self.wifi_idle_failed = true;
                self.log.emit(
                    Level::Warn,
                    "wifi",
                    "idle_power_down_failed",
                    "Wi-Fi idle power-down failed",
                    None,
                    json!({"error":error}),
                );
            }
        }
    }

    fn automatic_sleep_tick(&mut self) {
        // A crashed helper must not leave a stale preparation flag swallowing
        // input forever. Time asleep does not advance Instant. The actual flock
        // (not a PID file) distinguishes a live owner from an abandoned request.
        if self.sleep_prepared
            && self
                .sleep_prepared_since
                .is_some_and(|since| since.elapsed().as_secs() >= 15)
            && matches!(power::sleep_owner_present(), Ok(false))
        {
            let result = self.restore_sleep();
            self.model.platform.snapshot.sleep.phase =
                reborn_core::platform::SleepPhase::RestoreFailed;
            self.model.platform.snapshot.sleep.problem =
                Some(reborn_core::platform::SleepProblem::RestorationFailed);
            self.log.emit(
                Level::Error,
                "power",
                "sleep_owner_lost",
                "Sleep owner stopped before restoration",
                None,
                json!({"application_restore":result}),
            );
        }
        if self.headless || self.sleep_prepared {
            return;
        }
        let playing = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        let busy = self.model.platform.busy.is_some()
            || self.playback.has_pending_start()
            || self.pending_reconfiguration.is_some()
            || self.model.library.scanning
            || self.query.is_some();
        if playing || busy || !self.model.screen_off {
            self.sleep_attempt = Some(Instant::now());
            return;
        }
        if reborn_core::platform::automatic_sleep_due(
            self.model.screen_off,
            self.dark_since.map(|since| since.elapsed().as_secs()),
            self.last_activity.elapsed().as_secs(),
            self.sleep_attempt.map(|since| since.elapsed().as_secs()),
            playing,
            busy,
        ) {
            // Platform owns fresh USB/battery/workload checks and the operation
            // locks. Preparation drains the DB and closes PCM before entry.
            let _ = self.effect(Effect::Platform(reborn_core::PlatformTask::SleepRequest));
        }
    }

    fn wifi_idle_restore(&mut self) {
        if !self.wifi_idle_off {
            return;
        }
        self.wifi_idle_off = false;
        if let Err(error) = wifi::radio(true, false) {
            self.log.emit(
                Level::Warn,
                "wifi",
                "idle_restore_failed",
                "Wi-Fi restore after idle failed",
                None,
                json!({"error":error}),
            );
        }
    }

    /// Present the battery and announce a worsening low-battery level once.
    fn present_battery(&mut self, battery: reborn_core::platform::BatteryState) {
        let before = self.model.platform.battery;
        if before == battery {
            return;
        }
        match battery.level {
            LowBattery::Low if before.level == LowBattery::Normal => {
                self.ui.flash("Low battery. Connect a charger soon.")
            }
            LowBattery::Critical if before.level != LowBattery::Critical => self
                .ui
                .flash("Battery critically low. Connect a charger now."),
            _ => {}
        }
        self.model.platform.battery = battery;
        self.dirty.mark_render();
    }

    fn status(&self) -> Value {
        json!({"version":reborn_core::BUILD_LABEL,"build_id":option_env!("REBORN_BUILD_ID").unwrap_or("development"),"session":self.log.session(),"uptime_seconds":self.log.uptime(),"current_screen":self.model.screen,"screen_off":self.model.screen_off,"playback":{"state":self.model.playback,"track_id":self.model.current().map(|t|t.id),"position_ms":self.model.position_ms,"duration_ms":self.model.current().map(|t|t.duration_ms),"queue_length":self.model.queue.len(),"queue_position":self.model.queue_position,"generation":self.model.generation},"output":self.model.output,"audio":self.playback.audio_state(),"library":{"tracks_loaded":self.model.library.tracks.len(),"schema":reborn_library::SCHEMA_VERSION,"scanning":self.model.library.scanning,"last_scan":self.model.library.last_scan,"error":self.model.library.error},"wifi":self.wifi_state,"bluetooth":self.bt_state,"storage":self.model.sources,"power":self.power,"graphics":{"available":self.graphics.is_some(),"renderer":self.graphics.as_ref().map(|g|&g.info),"headless":self.headless},"decoder":{"ffmpeg":reborn_media::version(),"runtime":reborn_media::runtime_components_if_loaded()},"buffers":{"frames":self.log.metrics()["audio_buffer_frames"],"milliseconds":self.log.metrics()["audio_buffer_ms"]}})
    }
    fn snapshot(&self) -> Value {
        json!({"status":self.status(),"health":self.log.health(),"metrics":self.log.metrics(),"recent_errors":self.log.events(20,None,Some(Level::Warn),None),"resource_usage":fs::read_to_string("/proc/self/status").unwrap_or_default(),"kernel_events":self.kernel_events.lock().map(|v|v.clone()).unwrap_or_default()})
    }
    fn play_control(&mut self, a: PlaybackAction) -> Result<(), String> {
        self.with_model_action(|runtime| runtime.play_control_inner(a))
    }
    fn play_control_inner(&mut self, a: PlaybackAction) -> Result<(), String> {
        self.dirty.mark_render();
        match a {
            PlaybackAction::Play(id) => {
                let index = self
                    .model
                    .library
                    .tracks
                    .iter()
                    .position(|t| t.id == id)
                    .ok_or("track id not in library")?;
                self.effect(Effect::Play(index))
            }
            PlaybackAction::Pause => {
                self.pause();
                Ok(())
            }
            PlaybackAction::Resume => {
                if self.resume_in_place() {
                    Ok(())
                } else {
                    self.load()
                }
            }
            PlaybackAction::Stop => {
                self.model.invalidate();
                self.playback.stop(self.model.generation);
                self.model.playback = PlaybackState::Stopped;
                self.model.position_ms = 0;
                self.dirty.mark_both();
                self.checkpoint();
                Ok(())
            }
            PlaybackAction::Next => self.effect(Effect::NextTrack),
            PlaybackAction::Previous => self.effect(Effect::PreviousTrack),
            PlaybackAction::Seek(ms) => {
                self.model.position_ms = ms.min(
                    self.model
                        .current()
                        .map(|t| t.duration_ms.saturating_sub(1))
                        .unwrap_or(0),
                );
                self.dirty.mark_both();
                if self.model.playback == PlaybackState::Playing {
                    self.load()
                } else {
                    self.release_hold();
                    Ok(())
                }
            }
            PlaybackAction::Wired => self.switch(AudioOutput::Wired),
            PlaybackAction::Bluetooth(s) => self.switch(AudioOutput::Bluetooth(s)),
            PlaybackAction::Volume(v) => {
                self.model.settings.volume = v.min(100);
                self.dirty.mark_both();
                self.playback.set_volume(self.model.settings.volume);
                Ok(())
            }
        }
    }
}
/// The shortest start-to-start gap between two frames. Presentation waits
/// for the page flip, so while Cover Flow glides (or the cover flies) the
/// panel's vblank (~63 Hz) paces the frames; the 12-ms floor only guards
/// against a flip that returns early. Everything else stays at ~30 fps.
fn frame_gap(gliding: bool) -> Duration {
    if gliding {
        Duration::from_millis(12)
    } else {
        Duration::from_millis(34)
    }
}

/// Input readiness wakes immediately; the bounded timeout services asynchronous
/// workers. Dirty frames use a start-to-start budget rather than adding the
/// entire GPU/presentation cost to a fresh delay.
fn main_loop_interval(
    screen_off: bool,
    dirty: bool,
    gliding: bool,
    playing: bool,
    since_frame: Duration,
) -> Duration {
    if (dirty || gliding) && !screen_off {
        frame_gap(gliding).saturating_sub(since_frame)
    } else if playing {
        Duration::from_millis(50)
    } else if screen_off {
        Duration::from_secs(1)
    } else {
        Duration::from_millis(250)
    }
}

/// Present one Wi-Fi observation in product terms.
fn wifi_view(s: &wifi::Status) -> WifiView {
    let status = if s.ssid.is_empty() {
        WifiStatus::Disconnected
    } else {
        let name = s.ssid.clone();
        match (s.readiness.as_str(), s.state.as_str()) {
            ("Online", _) => WifiStatus::Connected(name),
            ("Authenticated", _)
                if s.readiness_reason
                    .as_deref()
                    .is_some_and(|r| r.contains("dns")) =>
            {
                WifiStatus::NoInternet(name)
            }
            ("AcquiringIP", _) => WifiStatus::GettingAddress(name),
            ("Associating" | "Authenticated", _) => WifiStatus::Connecting(name),
            ("Failed" | "Off" | "Unavailable", _) => WifiStatus::Disconnected,
            // Without a platform readiness record, association alone is not
            // a working connection: require at least an address.
            (_, "COMPLETED") if !s.ip.is_empty() => WifiStatus::Connected(name),
            (_, "COMPLETED") => WifiStatus::GettingAddress(name),
            (_, "ASSOCIATING" | "ASSOCIATED" | "4WAY_HANDSHAKE" | "GROUP_HANDSHAKE") => {
                WifiStatus::Connecting(name)
            }
            _ => WifiStatus::Disconnected,
        }
    };
    let bars = |signal: i32| match signal {
        s if s >= -60 => 3,
        s if s >= -70 => 2,
        s if s >= -80 => 1,
        _ => 0,
    };
    let joined = status.network().map(str::to_owned);
    let mut networks: Vec<NetworkView> = s
        .saved
        .iter()
        .filter_map(|n| {
            let visible = s.networks.iter().find(|v| v.ssid == n.ssid);
            Some(NetworkView {
                ssid: n.ssid.clone(),
                bars: visible.map(|v| bars(v.signal)).unwrap_or(0),
                secured: visible.and_then(|v| v.password_required()).or(Some(true)),
                saved_id: Some(n.saved_id?),
                visible: visible.is_some(),
            })
        })
        .collect();
    networks.extend(
        s.networks
            .iter()
            .filter(|n| !s.saved.iter().any(|saved| saved.ssid == n.ssid))
            .map(|n| NetworkView {
                ssid: n.ssid.clone(),
                bars: bars(n.signal),
                secured: n.password_required(),
                saved_id: None,
                visible: true,
            }),
    );
    networks.sort_by_key(|n| {
        (
            joined.as_deref() != Some(n.ssid.as_str()),
            n.saved_id.is_none(),
            !n.visible,
            std::cmp::Reverse(n.bars),
        )
    });
    WifiView {
        available: s.available,
        powered: s.enabled,
        scan: s.scan.clone(),
        problem: wifi::problem(s),
        status,
        networks,
    }
}

/// Present one Bluetooth observation in product terms.
fn bluetooth_view(s: &bluetooth::Status) -> BluetoothView {
    BluetoothView {
        available: s.available,
        powered: s.powered,
        scan: s.scan.clone(),
        devices: s
            .devices
            .iter()
            .map(|d| BluetoothDeviceView {
                path: d.path.clone(),
                address: d.address.clone(),
                name: if d.name.is_empty() {
                    "Bluetooth device".into()
                } else {
                    d.name.clone()
                },
                paired: d.paired,
                bonded: d.bonded,
                connected: d.connected,
                audio_ready: s.pcms.iter().any(|pcm| pcm.is_a2dp_playback_for(&d.path)),
                codec: s.active_codec(&d.path),
            })
            .collect(),
        codec_choices: s.codec_choices(),
        sbc_quality_available: s.codec_policy.as_ref().is_some_and(|p| {
            p.eligibility
                .get("SBC")
                .is_some_and(|e| e.compiled_locally && e.runtime_enabled && e.mutually_usable)
        }),
        problem: s
            .error
            .as_deref()
            .map(|e| format!("Couldn't complete that. {}", bluetooth_hint(e))),
    }
}

fn bluetooth_hint(error: &str) -> &'static str {
    let e = error.to_ascii_lowercase();
    if e.contains("auth") || e.contains("reject") || e.contains("canceled") {
        "Put the device in pairing mode and try again."
    } else if e.contains("timeout") || e.contains("host is down") || e.contains("page") {
        "Make sure the device is on and nearby."
    } else {
        "Try again."
    }
}

/// Application-owned audio pipeline facts for Diagnostics → Audio.
fn audio_facts(a: &Value, model: &AppModel) -> Vec<Fact> {
    let shown = |v: &Value| match v {
        Value::Null => "Not reported".to_owned(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    vec![
        Fact::new("Source codec", shown(&a["source"]["codec"])),
        Fact::new("Source rate (Hz)", shown(&a["source"]["sample_rate"])),
        Fact::new("Source bits", shown(&a["source"]["source_bits"])),
        Fact::new("Decoded format", shown(&a["decoder_format"])),
        Fact::new("DSP format", shown(&a["internal_processing_format"])),
        Fact::new(
            "ReplayGain applied (dB)",
            shown(&a["replay_gain"]["applied_gain_db"]),
        ),
        Fact::new("Crossfade (ms)", model.settings.crossfade_ms.to_string()),
        Fact::new(
            "Sink",
            if model.playback == PlaybackState::Playing {
                "Current playback".to_owned()
            } else {
                "Last opened; playback inactive".to_owned()
            },
        ),
        Fact::new("PCM format", shown(&a["alsa"]["format"])),
        Fact::new("PCM rate (Hz)", shown(&a["alsa"]["rate"])),
        Fact::new("PCM channels", shown(&a["alsa"]["channels"])),
    ]
}

/// Remove Reborn's disposable cache files; directories and anything else
/// outside the application-owned cache are never touched.
fn clear_cache(cache: &Path) -> std::io::Result<()> {
    let mut result = Ok(());
    for entry in fs::read_dir(cache)?.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            if let Err(error) = fs::remove_file(entry.path()) {
                result = Err(error);
            }
        }
    }
    result
}

/// Screens whose content comes from a platform observation.
fn platform_backed(screen: Screen) -> bool {
    matches!(
        screen,
        Screen::Settings
            | Screen::PcTransfer
            | Screen::SettingsLibrary
            | Screen::SettingsSystem
            | Screen::Sleep
            | Screen::Wifi
            | Screen::Bluetooth
            | Screen::Storage
            | Screen::Update
            | Screen::About
            | Screen::Diagnostics
            | Screen::DiagnosticSection
    )
}

fn codec_change_allowed(playback: PlaybackState, reconfiguring: bool) -> bool {
    // Pause releases the sink; the platform's PCM lease still authoritatively
    // refuses a switch while the asynchronous release is finishing.
    !reconfiguring && matches!(playback, PlaybackState::Stopped | PlaybackState::Paused)
}

/// A codec change can replace its PCM object. Remember the connected peer,
/// not that object generation, to avoid starting a new Auto policy repeatedly.
#[derive(Default)]
struct CodecReconnect {
    attempted: HashSet<String>,
}
impl CodecReconnect {
    fn next(
        &mut self,
        status: &bluetooth::Status,
        playback: PlaybackState,
        reconfiguring: bool,
    ) -> Option<String> {
        if !status.available {
            return None; // A failed observation is not a disconnection.
        }
        self.attempted.retain(|address| {
            status.powered
                && status
                    .devices
                    .iter()
                    .any(|d| d.connected && d.address.eq_ignore_ascii_case(address))
        });
        if !status.powered || !status.bluealsa || !codec_change_allowed(playback, reconfiguring) {
            return None;
        }
        status
            .devices
            .iter()
            .find(|d| {
                d.connected
                    && d.audio
                    && d.paired
                    && d.bonded
                    && !self.attempted.contains(&d.address)
                    && status.playback_pcm(&d.address).is_ok_and(|pcm| {
                        pcm.running == Some(false) && pcm.transport_generation != 0
                    })
            })
            .map(|d| d.address.clone())
    }
    fn sent(&mut self, address: String) {
        self.attempted.insert(address);
    }
}

/// Bluetooth headphones win over the headphone jack. The first time a
/// connected peer's playback PCM is ready, audio moves to it (playback
/// continues there). Once per connection: choosing the jack afterwards sticks
/// until the peer reconnects, and a codec change that recreates the PCM is not
/// a new connection. A working Bluetooth output is never displaced. Losing the
/// peer returns to the jack and pauses (`Event::BluetoothDisconnected`).
#[derive(Default)]
struct AutoOutput {
    routed: HashSet<String>,
}
impl AutoOutput {
    fn next(&mut self, status: &bluetooth::Status, output: &AudioOutput) -> Option<String> {
        if !status.available {
            return None; // A failed observation is not a disconnection.
        }
        self.routed.retain(|address| {
            status.powered
                && status
                    .devices
                    .iter()
                    .any(|d| d.connected && d.address.eq_ignore_ascii_case(address))
        });
        if !status.powered {
            return None;
        }
        let ready = |address: &str| status.playback_pcm(address).is_ok();
        let peer = status.devices.iter().find(|d| {
            d.connected && d.audio && !self.routed.contains(&d.address) && ready(&d.address)
        })?;
        match output {
            AudioOutput::Bluetooth(current) if current.eq_ignore_ascii_case(&peer.address) => {
                // Already there (restored or chosen by hand): this connection is done.
                self.routed.insert(peer.address.clone());
                None
            }
            AudioOutput::Bluetooth(current) if ready(current) => None,
            _ => Some(peer.address.clone()),
        }
    }
    fn routed(&mut self, address: String) {
        self.routed.insert(address);
    }
}

/// Observation cadence while such a screen is visible. A cable plug should
/// appear promptly on PC Transfer; other pages change rarely.
fn platform_refresh_interval(screen: Screen) -> Duration {
    Duration::from_secs(if matches!(screen, Screen::PcTransfer | Screen::Sleep) {
        4
    } else {
        15
    })
}

fn friendly_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("connect bluetooth") || lower.contains("pause music") {
        // Already user language.
        return error.to_owned();
    }
    if lower.contains("source offline") || lower.contains("source unavailable") {
        return "This song is on storage that isn't available. Reinsert the SD card.".into();
    }
    if lower.contains("not inserted") {
        return "Insert an SD card first.".into();
    }
    if lower.contains("bluetooth") && lower.contains("unavailable") {
        return "Bluetooth is starting. Try again in a moment.".into();
    }
    if lower.contains("wi-fi") && lower.contains("unavailable") {
        return "Wi-Fi is starting. Try again in a moment.".into();
    }
    if lower.contains("unavailable") || lower.contains("not ready") || lower.contains("starting") {
        return "Not ready yet. Try again in a moment.".into();
    }
    if lower.contains("busy") {
        return "Still working on the last request. Try again in a moment.".into();
    }
    if lower.contains("timeout") {
        return "That took too long. Please try again.".into();
    }
    if lower.contains("password") || lower.contains("ssid") {
        return "Check the network name and password.".into();
    }
    if lower.contains("queue is full") {
        return "The queue is full.".into();
    }
    "Couldn't complete that. Please try again.".into()
}
fn secure_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = fs::symlink_metadata(path) {
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err(format!(
                "persistent path is not a real directory: {}",
                path.display()
            ));
        }
    }
    fs::create_dir_all(path).map_err(|e| e.to_string())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}
fn option(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
fn startup_phase(log: &Observer, started: Instant, phase: &str) {
    // The early splash maps the same milestone names to its progress bar.
    reborn_platform::contract::boot_milestone(phase);
    log.emit(
        Level::Info,
        "startup",
        "phase",
        phase,
        None,
        json!({"phase":phase,"elapsed_ms":started.elapsed().as_millis()}),
    );
}
fn run() -> Result<(), String> {
    let process_started = Instant::now();
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut performance = performance::Trace::new(option(&args, "--performance-trace"));
    if args.iter().any(|s| s == "--default-settings") {
        let value = json!({"session_schema":reborn_core::SESSION_SCHEMA_VERSION,
            "settings":reborn_core::Settings::default()});
        writeln!(std::io::stdout(), "{value}").map_err(|e| e.to_string())?;
        return Ok(());
    }
    if args.iter().any(|s| s == "--version") {
        let _ = writeln!(std::io::stdout(), "{}", reborn_core::BUILD_LABEL);
        return Ok(());
    }
    if args.iter().any(|s| s == "--help") {
        let _ = writeln!(
            std::io::stdout(),
            "reborn [--headless --data-dir DIR --socket PATH --music-dir DIR --fixtures DIR] [--performance-trace PATH]"
        );
        return Ok(());
    }
    let headless = args.iter().any(|s| s == "--headless");
    if !headless && reborn_platform::contract::maintenance_pending() {
        return Err("owner maintenance is incomplete; resume through y2-platform reset".into());
    }
    if !headless && !storage::data_ready() {
        return Err("Y2DATA is not ready; refusing mutable data on Y2ROOT".into());
    }
    let root = PathBuf::from(option(&args, "--data-dir").unwrap_or("/data/reborn".into()));
    if !headless && root != Path::new("/data/reborn") {
        return Err("production data directory must be /data/reborn".into());
    }
    for p in [
        root.clone(),
        root.join("state"),
        root.join("cache"),
        root.join("logs"),
        root.join("diagnostics"),
    ] {
        secure_dir(&p)?
    }
    let log = Observer::new(&root.join("logs")).map_err(|e| e.to_string())?;
    log.install_panic_hook(reborn_core::BUILD_LABEL);
    reborn_platform::install_signals();
    reborn_media::initialize_logging(log.clone());
    reborn_audio::initialize_logging(log.clone());
    log.emit(Level::Info,"startup","starting","Reborn Baseline 01",None,json!({"version":reborn_core::BUILD_LABEL,"ffmpeg":reborn_media::version(),"headless":headless,"process_setup_ms":process_started.elapsed().as_millis()}));
    let session_path = root.join("state/session.json");
    let mut model = match AppModel::restore(&session_path) {
        Ok(model) => model,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AppModel::default(),
        Err(error) => {
            log.emit(
                Level::Warn,
                "core",
                "restore_failed",
                "Saved session was rejected; starting with a clean session",
                None,
                json!({"path":session_path,"error":error.to_string()}),
            );
            AppModel::default()
        }
    };
    startup_phase(&log, process_started, "model_restored");
    if !headless {
        // A previous instance may have ended on the dark shutdown frame.
        let _ = power::blank(false);
        model.platform.brightness_available = power::brightness_available();
        if model.platform.brightness_available {
            let _ = power::set_brightness(model.settings.brightness);
        }
        model.platform.battery = power::battery(&power::status());
    }
    // Open the display as soon as the process is alive. The first bounded
    // frame below hands KMS to Reborn while storage and library workers start.
    let graphics = if headless {
        log.health_set(
            "graphics",
            HealthState::Unavailable,
            true,
            "explicit headless mode",
        );
        None
    } else {
        match Renderer::open(&reborn_ui::atlases(), log.clone()) {
            Ok(g) => Some(g),
            Err(e) => {
                log.health_set("graphics", HealthState::Failed, true, &e);
                log.emit(
                    Level::Error,
                    "graphics",
                    "startup_failed",
                    &e,
                    None,
                    json!({}),
                );
                None
            }
        }
    };
    startup_phase(&log, process_started, "graphics_ready");
    // The player animates (Beta 7.2); previews and tests draw still frames.
    let mut ui = Ui::default();
    ui.ui_motion = true;
    // The early splash keeps the display, showing real startup progress, until
    // the first complete UI frame below is ready. That frame is the hand-off:
    // it is presented under a full boot-screen cover that dissolves at once.
    let mut first_frame_presented = false;
    let mut first_interactive_presented = false;
    let mut initial_library_ready = false;
    let mut initial_platform_ready = false;
    if let Some(m) = option(&args, "--music-dir") {
        model.settings.music_directory = m.into();
    } else if !headless && storage::platform_manages_media() {
        // The platform decides where internal music lives (Y2Media once mounted, else Y2DATA),
        // whatever an older settings file recorded.
        model.settings.music_directory = storage::internal_music_directory();
    }
    if !headless
        && !model.settings.music_directory.starts_with("/data/")
        && !model
            .settings
            .music_directory
            .starts_with("/media/internal/")
    {
        return Err("internal music must live under Y2DATA or Y2Media".into());
    }
    fs::create_dir_all(&model.settings.music_directory).map_err(|e| e.to_string())?;
    if !headless && storage::sd_present() && !storage::platform_manages_media() {
        let _ = storage::mount_sd();
    }
    model.sources = if headless {
        vec![Source {
            id: "internal".into(),
            kind: reborn_core::MediaSource::Internal,
            root: model.settings.music_directory.clone(),
            online: true,
            mount: "host-test".into(),
            mount_id: None,
        }]
    } else {
        storage::sources(&model.settings.music_directory)
    };
    startup_phase(&log, process_started, "storage_ready");
    let db = Database::spawn(root.join("library.db"), log.clone())?;
    log.emit(
        Level::Info,
        "startup",
        "database_ready",
        "Database open completed",
        None,
        json!({"elapsed_ms":process_started.elapsed().as_millis()}),
    );
    db.sources(model.sources.clone())?;
    let initial_catalog = db.catalog()?;
    let scanner = Scanner::spawn(db.clone(), log.clone())?;
    scanner.scan(model.sources.clone())?;
    startup_phase(&log, process_started, "library_workers_ready");
    let inputs = if headless {
        None
    } else {
        Some(input::InputManager::open())
    };
    log.health_set(
        "input",
        if inputs.as_ref().is_some_and(|i| i.count() > 0) {
            HealthState::Ok
        } else {
            HealthState::Unavailable
        },
        true,
        "evdev mapping",
    );
    log.health_set("storage", HealthState::Ok, true, "Y2DATA ready");
    log.health_set(
        "audio",
        if reborn_audio::wired_device().is_ok() {
            HealthState::Ok
        } else {
            HealthState::Unavailable
        },
        true,
        "ALSA card discovery",
    );
    startup_phase(&log, process_started, "core_services_ready");
    let playback = playback::Playback::spawn(log.clone(), root.join("cache"))?;
    startup_phase(&log, process_started, "audio_ready");
    let wifi = if headless {
        None
    } else {
        Some(wifi::Wifi::spawn(log.clone()).map_err(|e| e.to_string())?)
    };
    let bluetooth = if headless {
        None
    } else {
        Some(bluetooth::Bluetooth::spawn(log.clone()).map_err(|e| e.to_string())?)
    };
    startup_phase(&log, process_started, "radio_workers_ready");
    let socket =
        PathBuf::from(option(&args, "--socket").unwrap_or("/run/reborn/control.sock".into()));
    let server = reborn_control::Server::spawn(&socket, log.clone()).map_err(|e| e.to_string())?;
    let diagnostics = diagnostics::Diagnostics::spawn(
        log.clone(),
        db.clone(),
        PathBuf::from(option(&args, "--fixtures").unwrap_or("/usr/share/reborn/fixtures".into())),
        root.join("diagnostics"),
    )
    .map_err(|e| e.to_string())?;
    let background = background::Gate::new(&model.settings.music_directory, &model.sources);
    let mut rt = Runtime {
        zone_lookup_again: false,
        playback_hints: reborn_platform::workload::Hints::default(),
        interaction_hints: reborn_platform::workload::Hints::default(),
        model,
        ui,
        platform: if headless {
            None
        } else {
            Some(client::Client::spawn().map_err(|e| e.to_string())?)
        },
        platform_refresh_pending: false,
        platform_observed: false,
        announced_update: None,
        boot_fade: reborn_ui::BOOT_FADE_FRAMES,
        thumbs: if headless {
            None
        } else {
            Some(artwork::Worker::spawn().map_err(|e| e.to_string())?)
        },
        playback,
        log: log.clone(),
        db,
        scanner,
        wifi,
        bluetooth,
        wifi_state: Default::default(),
        bt_state: Default::default(),
        graphics,
        graphics_recovery: graphics_recovery::Recovery::default(),
        power: power::status(),
        kernel_events: Arc::new(Mutex::new(Vec::new())),
        background,
        art: false,
        dirty: DirtyState {
            render: true,
            persistence: false,
        },
        root: root.clone(),
        radio_favorites: reborn_platform::radio::load_favorites(&root.join("state/radio.json")),
        radio_current: None,
        radio_sequence: 0,
        radio_replies: std::sync::mpsc::channel(),
        radio_directory: Arc::new(Mutex::new(None)),
        radio_live: None,
        radio_list: vec![],
        fm: reborn_platform::fm::Paths::default(),
        fm_saved: reborn_platform::fm::load(&root.join("state/fm.json")),
        fm_scanning: false,
        fm_searched_now: false,
        fm_checked: None,
        radio_logos: if headless {
            None
        } else {
            LogoFetcher::spawn(root.join("cache"))
        },
        radio_art: None,
        radio_art_pending: None,
        playlist_worker: playlists::Worker::spawn(),
        headless,
        last_activity: Instant::now(),
        query: Some(initial_catalog),
        action_depth: 0,
        action_before: None,
        action_dirty_before: None,
        pending_reconfiguration: None,
        held_since: None,
        seek_until: None,
        reconfiguration_attempted: false,
        sink_release_started: false,
        transition_generation: None,
        active_transport_generation: None,
        input_resume: false,
        sleep_prepared: false,
        sleep_prepared_since: None,
        dark_since: None,
        sleep_attempt: None,
        wifi_idle_off: false,
        wifi_idle_failed: false,
    };
    rt.ui.radio.favorites = rt
        .radio_favorites
        .iter()
        .map(|s| s.key().to_owned())
        .collect();
    rt.ui.radio.fm = rt.fm_view();
    if !rt.model.settings.theme.is_empty() {
        let id = rt.model.settings.theme.clone();
        // A skin on a missing SD card or a refused skin falls back to Reborn's
        // own look for this session; the choice stays for the next start.
        if let Err(e) = rt.apply_theme(&id, false) {
            rt.log.emit(
                Level::Warn,
                "ui",
                "skin_unavailable",
                &e,
                None,
                json!({"skin":id}),
            );
        }
    }
    startup_phase(&log, process_started, "runtime_ready");
    let last_snapshot = Arc::new(Mutex::new(json!({"starting":true})));
    let watcher = last_snapshot.clone();
    let kernel_history = rt.kernel_events.clone();
    let observation_authority = rt.background.authority();
    let (observation_requests, observation_rx) =
        std::sync::mpsc::sync_channel::<background::Request>(1);
    let (observation_tx, observations) = std::sync::mpsc::sync_channel::<Observation>(1);
    let wl = log.clone();
    let diagnostic_root = root.join("diagnostics");
    thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || {
            let mut last_failed = false;
            let mut kernel_reader = kernel_events::KernelEvents::default();
            loop {
                let events = kernel_reader.poll();
                if let Ok(mut history) = kernel_history.lock() {
                    *history = events;
                }
                match observation_rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(request) => {
                        let started = Instant::now();
                        if let Some((power, media)) = observation_authority.run(&request, || {
                            (
                                power::status(),
                                (!headless).then(|| {
                                    (storage::sd_present(), storage::sources(&request.music))
                                }),
                            )
                        }) {
                            let _ = observation_tx.try_send(Observation {
                                request,
                                power,
                                media,
                                elapsed_ms: started.elapsed().as_secs_f64() * 1000.,
                            });
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
                let health = wl.health();
                let failed = health["overall"] == "failed";
                if failed && !last_failed {
                    wl.emit(
                        Level::Error,
                        "diagnostics",
                        "worker_or_subsystem_failed",
                        "Health transitioned to failed",
                        None,
                        json!({"health":health}),
                    );
                    if let Ok(s) = watcher.try_lock() {
                        let _ = wl.diagnostic(&diagnostic_root, s.clone(), true);
                    }
                }
                last_failed = failed;
                if reborn_platform::stop_requested() {
                    break;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let mut inputs = inputs;
    let mut workload_tick = Instant::now();
    let (mut periodic, mut checkpoint, mut render_time) = (
        Instant::now(),
        Instant::now(),
        Instant::now() - Duration::from_secs(1),
    );
    let mut monitor: Option<(Instant, SyncSender<Response>, u64, Vec<Value>)> = None;
    let mut action_router = input::ActionRouter::default();
    let mut input_screen_off = rt.model.screen_off;
    let mut last_sd = storage::sd_present();
    let mut usb_refresh = storage::TransferRefresh::default();
    let mut observation_pending = None;
    let mut xrun_bundle = 0.;
    let mut shutdown_intent = None;
    let mut power_poll = Instant::now() - Duration::from_secs(1);
    let player = if headless {
        None
    } else {
        let wake = server.notifier().map_err(|e| e.to_string())?;
        avrcp::Player::spawn_notified(log.clone(), move || {
            use std::io::Write;
            let _ = (&wake).write(&[1]);
        })
        .ok()
    };
    let mut player_publish = Instant::now() - Duration::from_secs(1);
    let mut platform_refresh = Instant::now() - Duration::from_secs(30);
    let mut usb_poll = Instant::now();
    // Automatic time zone (Beta 7.1): one lookup per Wi-Fi network, in the
    // background; a failed one is tried again after ten minutes.
    let (zone_tx, zone_rx) = std::sync::mpsc::channel::<(String, Result<String, String>)>();
    let mut zone_found_on: Option<String> = None;
    let mut zone_tried: Option<Instant> = None;
    let mut zone_in_flight = false;
    let mut download_poll = Instant::now();
    let mut last_platform_screen = rt.model.screen;
    let mut codec_reconnect = CodecReconnect::default();
    let mut auto_output = AutoOutput::default();
    while !reborn_platform::stop_requested() {
        // Drain before inspecting any producer queue: a later event keeps the
        // descriptor readable until the next loop and cannot be lost in wait.
        server.drain_wake();
        if rt.background.update(
            &rt.model.settings.music_directory,
            &rt.model.sources,
            rt.sleep_prepared,
        ) {
            observation_pending = None;
        }
        if let Some(worker) = &mut rt.thumbs {
            // Upload finished thumbnails (a few per pass, so a burst never
            // stalls a frame), then queue what the last frame could not show.
            for _ in 0..3 {
                let Some(reply) = worker.try_reply() else {
                    break;
                };
                if performance.enabled() {
                    performance.record("artwork", json!({"decode_ms":reply.decode_ms,"cache_hit":reply.cache_hit,"pixels":reply.rgba.as_ref().map(Vec::len)}));
                }
                if reply.full && rt.radio_art == Some(reply.thumb) {
                    // The station's logo as Now Playing's picture and colour.
                    if reply.rgba.is_some() {
                        rt.radio_art_pending = None;
                    }
                    if let (Some(rgba), Some(renderer)) = (reply.rgba, rt.graphics.as_mut()) {
                        rt.ui.accent = reborn_ui::art::accent_from_rgba(&rgba);
                        let mut rounded = rgba;
                        reborn_ui::art::round_corners(&mut rounded, 160);
                        if renderer.artwork(&rounded).is_ok() {
                            rt.art = true;
                            rt.dirty.mark_render();
                        }
                    }
                    continue;
                }
                if reply.full {
                    // Cover Flow's centre cover at full resolution.
                    if let (Some(mut rgba), Some(renderer)) = (reply.rgba, rt.graphics.as_mut()) {
                        reborn_ui::art::round_corners(&mut rgba, 160);
                        if renderer.collection_artwork(&rgba).is_ok() {
                            rt.ui.collection_art.set(Some(reply.thumb));
                            rt.dirty.mark_render();
                        }
                    }
                    continue;
                }
                match (reply.rgba, rt.graphics.as_mut()) {
                    (Some(rgba), Some(renderer)) => {
                        if renderer.upload_thumb(reply.thumb, &rgba).is_ok() {
                            rt.dirty.mark_render();
                        }
                    }
                    (None, _) => {
                        worker.record_failure(reply.thumb);
                        // A logo that does not decode is not asked for again.
                        if reply.thumb >> 63 == 1 {
                            if let Some(fetcher) = &mut rt.radio_logos {
                                fetcher.settle(reply.thumb);
                            }
                        }
                    }
                    _ => {}
                }
            }
            let wanted = rt.ui.take_wanted_thumbs();
            if !rt.model.screen_off && !rt.sleep_prepared && rt.graphics.is_some() {
                let tracks = &rt.model.library.tracks;
                let jobs = wanted
                    .into_iter()
                    .filter(|(key, _)| {
                        rt.graphics
                            .as_ref()
                            .is_some_and(|g| !g.thumb_resident(*key))
                    })
                    .filter_map(|(key, index)| {
                        let track = tracks.get(index)?;
                        Some(artwork::Job {
                            thumb: key,
                            art: artwork::Key::new(
                                track,
                                rt.model
                                    .sources
                                    .iter()
                                    .find(|s| s.id == track.source_id)
                                    .and_then(|s| s.mount_id),
                                &rt.model.settings.music_directory,
                            )
                            .into(),
                            cache: rt.root.join("cache"),
                        })
                    })
                    .collect::<Vec<_>>();
                let mut jobs = jobs;
                // Station logos: decode the ones on disk, download the others.
                let cache = rt.root.join("cache");
                if rt.radio_logos.as_mut().is_some_and(|f| f.finished() > 0) {
                    rt.dirty.mark_render();
                }
                for (key, url) in rt.ui.take_wanted_logos() {
                    if rt.graphics.as_ref().is_some_and(|g| g.thumb_resident(key))
                        || rt.radio_logos.as_ref().is_some_and(|f| f.is_settled(key))
                    {
                        continue;
                    }
                    let file = reborn_platform::radio::logo_file(&cache, key);
                    if file.exists() {
                        jobs.push(artwork::Job {
                            thumb: key,
                            art: artwork::Art::Logo(file),
                            cache: cache.clone(),
                        });
                    } else if rt.ui.radio.online {
                        if let Some(fetcher) = &mut rt.radio_logos {
                            fetcher.want(key, &url);
                        }
                    }
                }
                if !jobs.is_empty() {
                    worker.want(jobs);
                }
                // The playing station's logo becomes Now Playing's picture.
                if !rt.art {
                    if let Some((key, url)) = rt
                        .ui
                        .radio
                        .now
                        .as_ref()
                        .and_then(|s| Some((s.logo_key()?, s.favicon.clone())))
                    {
                        let file = reborn_platform::radio::logo_file(&cache, key);
                        if file.exists() {
                            if rt.radio_art_pending != Some(key) {
                                rt.radio_art = Some(key);
                                rt.radio_art_pending = Some(key);
                                worker.want_cover(artwork::Job {
                                    thumb: key,
                                    art: artwork::Art::Logo(file),
                                    cache: cache.clone(),
                                });
                            }
                        } else if rt.ui.radio.online {
                            if let Some(fetcher) = &mut rt.radio_logos {
                                fetcher.want(key, &url);
                            }
                        }
                    }
                }
                if let Some((key, index)) = rt.ui.take_cover_request() {
                    if let Some(track) = tracks.get(index) {
                        worker.want_cover(artwork::Job {
                            thumb: key,
                            art: artwork::Key::new(
                                track,
                                rt.model
                                    .sources
                                    .iter()
                                    .find(|s| s.id == track.source_id)
                                    .and_then(|s| s.mount_id),
                                &rt.model.settings.music_directory,
                            )
                            .into(),
                            cache: rt.root.join("cache"),
                        });
                    }
                }
            }
        }
        // The cable and the transfer receipt, twice a second on every screen
        // (once a second with the screen off): the computer screen appears
        // within half a second instead of with the next Settings refresh.
        let usb_every = Duration::from_millis(if rt.model.screen_off { 1000 } else { 500 });
        if usb_poll.elapsed() >= usb_every {
            usb_poll = Instant::now();
            let usb = client::usb_now();
            let was = rt.model.platform.snapshot.usb;
            if usb != was && usb != reborn_core::platform::UsbTransfer::Unknown {
                rt.model.platform.snapshot.usb = usb;
                use reborn_core::platform::UsbTransfer::{Copying, Ready};
                // A computer opening the drives lights the screen to say so.
                if rt.model.screen_off && usb == Ready && !matches!(was, Ready | Copying) {
                    if let Err(error) = rt.effect(Effect::ScreenWake) {
                        rt.ui.flash(error);
                    }
                }
                rt.dirty.mark_render();
            }
        }
        if std::mem::take(&mut rt.zone_lookup_again) {
            zone_found_on = None;
            zone_tried = None;
        }
        if let WifiStatus::Connected(network) = &rt.ui.wifi.status {
            if rt.model.settings.zone_auto()
                && !zone_in_flight
                && zone_found_on.as_ref() != Some(network)
                && zone_tried.is_none_or(|t| t.elapsed() >= Duration::from_secs(600))
            {
                zone_in_flight = true;
                zone_tried = Some(Instant::now());
                let (tx, network) = (zone_tx.clone(), network.clone());
                let _ = thread::Builder::new()
                    .name("time-zone".into())
                    .spawn(move || {
                        let _ = tx.send((network, reborn_platform::geo::zone()));
                    });
            }
        }
        if let Ok((network, result)) = zone_rx.try_recv() {
            zone_in_flight = false;
            match result {
                Ok(zone) => {
                    zone_found_on = Some(network);
                    if rt.model.settings.zone_auto() && zone != rt.model.settings.time_zone {
                        log.emit(
                            Level::Info,
                            "clock",
                            "zone_found",
                            "Time zone set automatically",
                            None,
                            json!({"zone": zone}),
                        );
                        rt.model.settings.time_zone = zone;
                        rt.checkpoint();
                        rt.dirty.mark_render();
                    }
                }
                Err(error) => log.emit(
                    Level::Warn,
                    "clock",
                    "zone_lookup_failed",
                    "Automatic time zone lookup failed",
                    None,
                    json!({"error": error}),
                ),
            }
        }
        rt.poll_platform();
        if rt.platform_observed && !initial_platform_ready {
            initial_platform_ready = true;
            log.emit(
                Level::Info,
                "startup",
                "platform_ready",
                "First platform snapshot published",
                None,
                json!({"elapsed_ms":process_started.elapsed().as_millis()}),
            );
        }
        if !rt.model.screen_off
            && !rt.platform_refresh_pending
            && rt.model.platform.busy.is_none()
            && platform_backed(rt.model.screen)
            && (rt.model.screen != last_platform_screen
                || platform_refresh.elapsed() >= platform_refresh_interval(rt.model.screen))
            && platform_refresh.elapsed() >= Duration::from_secs(2)
        {
            if let Some(worker) = &rt.platform {
                if worker
                    .commands
                    .try_send(reborn_core::PlatformTask::Refresh)
                    .is_ok()
                {
                    rt.platform_refresh_pending = true;
                    platform_refresh = Instant::now();
                }
            }
        }
        last_platform_screen = rt.model.screen;
        // The download task answers only when it is finished: show its live
        // progress from the platform's RAM file meanwhile (OTA-48).
        if (rt.model.platform.busy == Some(reborn_core::PlatformTask::UpdateStage)
            || rt.model.platform.snapshot.update.phase
                == reborn_core::platform::UpdatePhase::Downloading)
            && !rt.model.screen_off
            && download_poll.elapsed() >= Duration::from_millis(200)
        {
            download_poll = Instant::now();
            let live = client::download_progress();
            if live != rt.model.platform.download {
                rt.model.platform.download = live;
                rt.dirty.mark_render();
            }
        }
        if let Some(player) = &player {
            for request in player.actions.try_iter().take(8) {
                if let Some(action) = request.semantic(&rt.model) {
                    let effect = rt.ui.model_action(&mut rt.model, action);
                    if let Err(error) = rt.effect(effect) {
                        rt.fail("avrcp", error);
                    }
                }
            }
            if player_publish.elapsed() >= Duration::from_millis(250) {
                player.publish(&rt.model);
                player_publish = Instant::now();
            }
        }
        if !headless && power_poll.elapsed() >= Duration::from_millis(250) {
            power_poll = Instant::now();
            shutdown_intent = power::shutdown_intent();
            if shutdown_intent.is_some() {
                break;
            }
        }
        rt.wifi_idle_tick();
        log.heartbeat("ui", 10);
        if rt.ui.observe(&rt.model) | rt.ui.expire_notice() | rt.ui.animating() {
            rt.dirty.mark_render();
        }
        // A glide continues after the last wheel detent; keep the short
        // interactive CPU lease alive until it rests. The playing wave keeps
        // the frame rate (`gliding`) but not the lease (Beta 7.4).
        if rt.ui.responding() && !rt.model.screen_off {
            rt.interaction_hints.interactive();
        }
        if let Some(effect) = rt.ui.power_hold_complete() {
            if let Err(error) = rt.effect(effect) {
                rt.fail("power", error);
            }
        }
        rt.poll_pending_reconfiguration();
        rt.apply_seek();
        if rt
            .held_since
            .is_some_and(|since| since.elapsed() >= HOLD_LIMIT)
        {
            rt.release_hold();
        }
        if rt.model.screen_off {
            rt.interaction_hints.clear();
        }
        if std::mem::take(&mut rt.input_resume) {
            if let Some(input) = &mut inputs {
                input.resumed();
            }
            action_router.reset();
        }
        if let Some(input) = &mut inputs {
            if input_screen_off != rt.model.screen_off {
                input.reset_wheel();
                input_screen_off = rt.model.screen_off;
            }
            action_router.begin_batch();
            for event in input.poll() {
                // The first frame is revealed under the boot cover; input only
                // counts once the finished UI is fully visible.
                if rt.boot_fade > 0 || rt.sleep_prepared {
                    continue;
                }
                if !rt.model.screen_off {
                    rt.interaction_hints.interactive();
                }
                log.emit(
                    Level::Debug,
                    "input",
                    "normalized_event",
                    "Normalized physical input",
                    None,
                    json!({"device":event.device,"input":event.input}),
                );
                // Per event: an action earlier in this batch may have opened
                // or closed the keyboard.
                action_router.set_keyboard(rt.ui.keyboard.is_some());
                let actions = action_router.route(&event, !rt.model.screen_off);
                for &action in &actions {
                    let action_started = Instant::now();
                    if performance.enabled() {
                        performance.record("input", json!({"action":action,"screen":rt.model.screen,"focus":rt.model.navigation.focus,"kernel_mono_us":event.kernel_mono_us}));
                    }

                    log.emit(
                        Level::Debug,
                        "input",
                        "action",
                        "Semantic application action",
                        None,
                        json!({"action":action}),
                    );
                    if !rt.model.screen_off || matches!(action, reborn_core::Action::ScreenWake) {
                        rt.last_activity = Instant::now();
                    }
                    let effect = rt.ui.model_action(&mut rt.model, action);
                    if let Err(e) = rt.effect(effect) {
                        rt.fail("ui", e)
                    }
                    if performance.enabled() {
                        performance.record("state", json!({"action":action,"screen":rt.model.screen,"focus":rt.model.navigation.focus,"action_ms":action_started.elapsed().as_secs_f64()*1000.,"playback":rt.model.playback,"volume":rt.model.settings.volume}));
                    }
                }
                if let Some((_, _, _, events)) = &mut monitor {
                    if events.len() < 512 {
                        events.push(json!({"device":event.device,"normalized":event.input,
                            "kernel_mono_us":event.kernel_mono_us,"actions":actions,
                            "state_applied_mono_us":input::monotonic_us(),
                            "screen":rt.model.screen,"screen_off":rt.model.screen_off,
                            "focus":rt.model.navigation.focus,"scroll":rt.model.navigation.scroll,
                            "modal_focus":rt.model.navigation.modal_focus,"volume":rt.model.settings.volume,
                            "pending_frame":!actions.is_empty() && !rt.model.screen_off && rt.dirty.render,
                            "present_return_mono_us":null}));
                    }
                }
                if input_screen_off != rt.model.screen_off {
                    input.reset_wheel();
                    input_screen_off = rt.model.screen_off;
                }
            }
        }
        if monitor.as_ref().is_some_and(|m| Instant::now() > m.0) {
            let (_, reply, id, events) = monitor.take().unwrap();
            let _ = reply.try_send(Response::ok(
                id,
                json!({"events":events,"count":events.len(),
                    "raw":inputs.as_mut().and_then(input::InputManager::finish_capture),
                    "clock":"CLOCK_MONOTONIC","frame_boundary":"renderer present returned"}),
            ));
        }
        while let Ok(e) = rt.playback.events.try_recv() {
            match e {
                playback::PlaybackEvent::Artwork {
                    generation,
                    entry_id,
                    track_id,
                    bytes,
                } => {
                    if artwork_matches_current(&rt.model, generation, entry_id, track_id) {
                        rt.ui.accent = reborn_ui::art::accent_from_rgba(&bytes);
                        if let Some(g) = &mut rt.graphics {
                            let mut rounded = bytes.to_vec();
                            reborn_ui::art::round_corners(&mut rounded, 160);
                            if g.artwork(&rounded).is_ok() {
                                rt.art = true;
                                rt.dirty.mark_render();
                            }
                        }
                    }
                }
                playback::PlaybackEvent::Core(event) => {
                    if let Event::Position { generation, ms } = event {
                        if generation == rt.model.generation && ms != rt.model.position_ms {
                            rt.dirty.mark_persistence();
                            if ms / 1000 != rt.model.position_ms / 1000
                                && rt.model.screen == Screen::NowPlaying
                                && !rt.model.screen_off
                            {
                                rt.dirty.mark_render();
                            }
                        }
                        rt.model.apply(Event::Position { generation, ms });
                        continue;
                    }
                    let persists = matches!(
                        &event,
                        Event::TrackBoundary { .. }
                            | Event::PlaybackError { .. }
                            | Event::SourceChanged(_)
                            | Event::BluetoothDisconnected(_)
                    );
                    let error = matches!(&event,Event::PlaybackError{generation,..}if *generation==rt.model.generation);
                    let ended = matches!(
                        &event,
                        Event::TrackEnded { generation } if *generation == rt.model.generation
                    );
                    if matches!(
                        &event,
                        Event::TrackBoundary { generation, .. }
                            if *generation == rt.model.generation
                    ) {
                        rt.art = false;
                        if rt.model.screen == Screen::NowPlaying {
                            rt.model.navigation.modal = None;
                            rt.model.navigation.context_target = None;
                            rt.model.navigation.context_key = None;
                        }
                    }
                    rt.model.apply(event);
                    rt.dirty.mark_render();
                    if persists {
                        rt.dirty.mark_persistence();
                    }
                    if error {
                        rt.model.invalidate();
                        rt.playback.stop(rt.model.generation);
                        let _ = log.diagnostic(&root.join("diagnostics"), rt.snapshot(), true);
                    }
                    if ended {
                        if let Err(e) = rt.finish_track() {
                            rt.fail("playback", e);
                        }
                    }
                }
            }
        }
        if let Ok(result) = rt.scanner.results.try_recv() {
            // A no-op scan must not rematerialize the resident catalog or
            // invalidate its cached navigation rows. Deletions alone count.
            let refresh_catalog = match &result {
                Ok(stats) => stats.rescanned != 0 || stats.removed != 0,
                Err(_) => true,
            };
            match result {
                Ok(stats) if stats.complete => {
                    rt.model
                        .apply(Event::LibraryScanFinished(reborn_core::ScanSummary {
                            discovered: stats.discovered,
                            reused: stats.reused,
                            elapsed_ms: stats.elapsed_ms,
                        }));
                    rt.ui.flash(match stats.discovered {
                        _ if stats.rescanned > 0 && stats.discovered > 1 => {
                            format!("{} new songs added", stats.rescanned.min(stats.discovered))
                        }
                        0 => "Scan finished. No music found.".to_owned(),
                        1 => "Scan finished. 1 song in your library.".to_owned(),
                        n => format!("Scan finished. {n} songs in your library."),
                    });
                }
                Ok(stats) => {
                    let error = format!(
                        "scan incomplete; retained existing entries ({} traversal failures)",
                        stats.failures
                    );
                    rt.model.apply(Event::LibraryScanFailed(error.clone()));
                    log.health_set("scanner", HealthState::Degraded, true, &error);
                    rt.fail("scanner", error);
                }
                Err(e) => {
                    rt.model.apply(Event::LibraryScanFailed(e.clone()));
                    log.health_set("scanner", HealthState::Failed, true, &e);
                    rt.fail("scanner", e);
                }
            }
            // An incomplete scan still publishes validated rows to SQLite,
            // while intentionally refusing pruning. Show those usable rows;
            // keep the failed/incomplete scan state visible and truthful.
            if refresh_catalog {
                rt.query = rt.db.catalog().ok();
            }
            rt.dirty.mark_render();
        }
        if let Some(rx) = &rt.query {
            if let Ok(value) = rx.try_recv() {
                match value {
                    Ok(t) => {
                        if matches!(
                            rt.model.screen,
                            Screen::Tracks
                                | Screen::Album
                                | Screen::Artist
                                | Screen::Folders
                                | Screen::TrackInfo
                        ) {
                            rt.model.navigation.modal = None;
                            rt.model.navigation.context_target = None;
                            rt.model.navigation.context_key = None;
                            if rt.model.screen == Screen::TrackInfo {
                                rt.model.screen = Screen::Tracks;
                                rt.model.navigation.filter.clear();
                            }
                        }
                        rt.model.library.tracks = t;
                        // Playlists are read again with the library (files
                        // copied over USB, the heart's Favorites), but not
                        // while a scan walks the same storage: its own
                        // publish when it ends comes next.
                        if !rt.model.library.scanning {
                            if let Err(e) = rt.playlist(reborn_core::PlaylistRequest::Refresh) {
                                rt.fail("playlists", e);
                            }
                        }
                        if !initial_library_ready {
                            initial_library_ready = true;
                            log.emit(Level::Info, "startup", "library_ready", "Initial library published", None, json!({"elapsed_ms":process_started.elapsed().as_millis(),"tracks":rt.model.library.tracks.len()}));
                        }
                        rt.ui.invalidate_catalog();
                        if let Some(worker) = &mut rt.thumbs {
                            worker.forget_failures();
                        }
                    }
                    Err(e) => rt.fail("database", e),
                }
                rt.query = None;
                rt.dirty.mark_render();
            }
        }
        if let Some(service) = &rt.wifi {
            while let Ok(s) = service.events.try_recv() {
                let view = wifi_view(&s);
                if view != rt.ui.wifi {
                    rt.ui.wifi = view;
                    rt.dirty.mark_render();
                }
                rt.wifi_state = s;
            }
        }
        // From the current Wi-Fi view every pass, not only when an event arrives
        // (Beta 4 refused a station right after start while Wi-Fi was up).
        rt.ui.radio.online = matches!(
            rt.ui.wifi.status,
            WifiStatus::Connected(_) | WifiStatus::NoInternet(_)
        );
        let playlist_replies: Vec<playlists::Reply> = rt
            .playlist_worker
            .as_ref()
            .map(|w| w.replies.try_iter().collect())
            .unwrap_or_default();
        for reply in playlist_replies {
            rt.playlist_reply(reply);
        }
        let radio_replies: Vec<RadioReply> = rt.radio_replies.1.try_iter().collect();
        for reply in radio_replies {
            rt.radio_reply(reply);
        }
        rt.radio_tick();
        let mut bt_lost = false;
        if let Some(service) = &rt.bluetooth {
            while let Ok(s) = service.events.try_recv() {
                if let Some(address) = codec_reconnect.next(
                    &s,
                    rt.model.playback,
                    rt.pending_reconfiguration.is_some(),
                ) {
                    if service
                        .commands
                        .try_send(bluetooth::Command::Codec {
                            address: address.clone(),
                            preference: rt.model.settings.codec_preference,
                        })
                        .is_ok()
                    {
                        codec_reconnect.sent(address);
                    }
                }
                if let AudioOutput::Bluetooth(address) = &rt.model.output {
                    bt_lost = rt
                        .bt_state
                        .devices
                        .iter()
                        .any(|d| &d.address == address && d.connected)
                        && !s
                            .devices
                            .iter()
                            .any(|d| &d.address == address && d.connected);
                }
                let view = bluetooth_view(&s);
                let facts = s.diagnostic_facts();
                if view != rt.ui.bluetooth || facts != rt.model.platform.bluetooth_facts {
                    rt.ui.bluetooth = view;
                    rt.model.platform.bluetooth_facts = facts;
                    rt.dirty.mark_render();
                }
                let pairing = s.pending.as_ref().map(|p| {
                    let name = s
                        .devices
                        .iter()
                        .find(|d| d.path == p.device)
                        .map(|d| d.name.as_str())
                        .unwrap_or("Bluetooth device");
                    format!(
                        "{name} shows code {}. Pair only if the code matches.",
                        p.display
                    )
                });
                if pairing != rt.ui.pairing {
                    rt.ui.pairing_focus = 1;
                    rt.dirty.mark_render();
                }
                rt.ui.pairing = pairing;
                rt.bt_state = s;
            }
        }
        if bt_lost {
            rt.abandon_pending_reconfiguration();
            if let AudioOutput::Bluetooth(address) = rt.model.output.clone() {
                rt.model.apply(Event::BluetoothDisconnected(address));
            }
            rt.pause();
            rt.ui.flash("Bluetooth disconnected. Playback paused.");
            log.emit(
                Level::Warn,
                "bluetooth",
                "transport_lost",
                "Selected audio peer disconnected",
                None,
                json!({"recovery":"paused"}),
            );
        }
        if rt.pending_reconfiguration.is_none() {
            if let Some(address) = auto_output.next(&rt.bt_state, &rt.model.output) {
                let name = rt
                    .bt_state
                    .devices
                    .iter()
                    .find(|d| d.address == address)
                    .map(|d| d.name.clone())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "Bluetooth headphones".into());
                // One attempt per connection, also when it fails: no retry loop.
                auto_output.routed(address.clone());
                match rt
                    .with_model_action(|runtime| runtime.switch(AudioOutput::Bluetooth(address)))
                {
                    Ok(()) => {
                        rt.ui.flash(format!("Audio switched to {name}"));
                        log.emit(
                            Level::Info,
                            "audio",
                            "output_automatic",
                            "Bluetooth audio peer ready; output follows it",
                            None,
                            json!({"reason":"bluetooth_connected"}),
                        );
                    }
                    Err(error) => {
                        log.emit(
                            Level::Warn,
                            "audio",
                            "output_automatic_failed",
                            "Automatic switch to Bluetooth failed",
                            None,
                            json!({"error":error}),
                        );
                    }
                }
                rt.dirty.mark_render();
            }
        }
        match transport_epoch_state(
            &rt.model.output,
            rt.model.playback,
            rt.active_transport_generation,
            &rt.bt_state,
        ) {
            TransportEpochState::Current => {}
            TransportEpochState::Changed => {
                if rt.pending_reconfiguration.is_some() {
                    rt.fail_active_transport(
                        "Bluetooth transport changed while a new sink was opening",
                    );
                    rt.ui
                        .flash("Bluetooth audio transport changed; playback stopped");
                    rt.dirty.mark_render();
                } else if let Err(error) = rt.with_model_action(|runtime| runtime.load()) {
                    rt.ui
                        .flash(format!("Bluetooth audio transport changed: {error}"));
                    rt.dirty.mark_render();
                }
            }
            TransportEpochState::Unavailable => {
                rt.fail_active_transport(
                    "Bluetooth playback PCM disappeared or lost its negotiated identity",
                );
                rt.ui
                    .flash("Bluetooth audio transport changed; playback stopped");
                rt.dirty.mark_render();
            }
        }
        while let Ok(env) = server.requests.try_recv() {
            let req = env.request;
            let id = req.id;
            let reply = env.reply;
            if rt.sleep_prepared && !sleep_control_allowed(&req.command) {
                let _ = reply.try_send(Response::error(id, "sleep_operation_in_progress"));
                continue;
            }
            let result: Result<Value, String> = match req.command.clone() {
                Command::Status => Ok(rt.status()),
                Command::SleepPrepare => rt.prepare_sleep(),
                Command::SleepRestore => rt.restore_sleep(),
                Command::Audio => Ok(
                    json!({"audio":rt.playback.audio_state(),"ffmpeg":reborn_media::runtime_components_if_loaded()}),
                ),
                Command::Health => Ok(log.health()),
                Command::Metrics => Ok(log.metrics()),
                Command::Snapshot => Ok(rt.snapshot()),
                Command::Events { last } => Ok(json!(log.events(last, None, None, None))),
                Command::Logs {
                    last,
                    subsystem,
                    level,
                    since_ms,
                } => Ok(json!(log.events(
                    last,
                    subsystem.as_deref(),
                    level,
                    since_ms
                ))),
                Command::LogLevel { subsystem, level } => log.level(subsystem.as_deref(), level),
                Command::Tests => Ok(
                    json!({"tests":reborn_control::TESTS.iter().map(|(n,a)|json!({"name":n,"audible":a,"bounded":true})).collect::<Vec<_>>()}),
                ),
                Command::Scan => rt
                    .effect(Effect::ScanLibrary)
                    .map(|_| json!({"accepted":true})),
                Command::BluetoothCodec {
                    address,
                    preference,
                } => {
                    if !codec_change_allowed(
                        rt.model.playback,
                        rt.pending_reconfiguration.is_some(),
                    ) {
                        Err("Pause music before changing codec".into())
                    } else {
                        let result = rt
                            .bluetooth
                            .as_ref()
                            .ok_or("Bluetooth service unavailable".into())
                            .and_then(|worker| {
                                worker
                                    .commands
                                    .try_send(bluetooth::Command::Codec {
                                        address,
                                        preference,
                                    })
                                    .map(|_| json!({"accepted":true,"negotiated":false}))
                                    .map_err(|_| "Bluetooth busy".into())
                            });
                        if result.is_ok() {
                            rt.model.settings.codec_preference = preference;
                            rt.dirty.mark_both();
                        }
                        result
                    }
                }
                Command::Radio { radio, action } => {
                    use reborn_control::{Radio, RadioAction};
                    // Scan uses exactly the button/UI path, not an independent
                    // diagnostic implementation which could hide UI-worker bugs.
                    let r = match (radio, action) {
                        (Radio::Wifi, RadioAction::Scan) => rt.effect(Effect::WifiScan),
                        (Radio::Bluetooth, RadioAction::Scan) => rt.effect(Effect::BluetoothScan),
                        (Radio::Wifi, RadioAction::On | RadioAction::Off) => rt
                            .wifi
                            .as_ref()
                            .ok_or("Wi-Fi service unavailable".to_string())
                            .and_then(|w| {
                                w.commands
                                    .try_send(wifi::Command::Power(matches!(
                                        action,
                                        RadioAction::On
                                    )))
                                    .map_err(|_| "Wi-Fi busy".into())
                            }),
                        (Radio::Bluetooth, RadioAction::On | RadioAction::Off) => rt
                            .bluetooth
                            .as_ref()
                            .ok_or("Bluetooth service unavailable".to_string())
                            .and_then(|w| {
                                w.commands
                                    .try_send(bluetooth::Command::Power(matches!(
                                        action,
                                        RadioAction::On
                                    )))
                                    .map_err(|_| "Bluetooth busy".into())
                            }),
                    };
                    r.map(|_| json!({"accepted":true,"radio":radio,"action":action}))
                }
                Command::Playback { action } => rt.play_control(action).map(|_| rt.status()),
                Command::Theme { id } => {
                    let applied = match id {
                        Some(id) => rt.effect(Effect::SetTheme(id)),
                        None => rt.effect(Effect::ScanThemes),
                    };
                    applied.map(|_| {
                        json!({"theme":rt.model.settings.theme,
                            "active":rt.ui.skin.as_ref().map(|s| json!({"id":s.id,"name":s.name,"layouts":s.layout_names(),"warnings":s.warnings,"atlas":[s.atlas_width,s.atlas_height]})),
                            "notice":rt.ui.notice,
                            "installed":rt.ui.themes.iter().map(|t| json!({"id":t.id,"name":t.name,"author":t.author,"version":t.version,"dir":t.dir,"problem":t.problem})).collect::<Vec<_>>()})
                    })
                }
                Command::InputMonitor { seconds } => {
                    if monitor.is_some() {
                        Err("input monitor busy".into())
                    } else {
                        if let Some(input) = &mut inputs {
                            input.begin_capture();
                        }
                        monitor = Some((
                            Instant::now() + Duration::from_secs(seconds),
                            reply,
                            id,
                            vec![],
                        ));
                        continue;
                    }
                }
                Command::Diagnose | Command::Test { .. } => {
                    let graphics = if matches!(
                        req.command,
                        Command::Test {
                            name: reborn_control::Test::Baseline | reborn_control::Test::Graphics,
                            ..
                        }
                    ) {
                        match &mut rt.graphics {
                            Some(g) => match g.test() {
                                Ok(()) => {
                                    json!({"passed":true,"renderer":g.info,"offscreen_readback":true})
                                }
                                Err(e) => json!({"passed":false,"error":e}),
                            },
                            None => json!({"passed":false,"error":"graphics unavailable"}),
                        }
                    } else {
                        json!({})
                    };
                    let job = diagnostics::Job {
                        request: req,
                        reply: reply.clone(),
                        snapshot: rt.snapshot(),
                        sources: rt.model.sources.clone(),
                        graphics,
                        output: rt.model.output.clone(),
                        output_rate: rt.output_rate(),
                        audio_busy: matches!(
                            rt.model.playback,
                            PlaybackState::Playing | PlaybackState::Buffering
                        ),
                    };
                    if diagnostics.sender.try_send(job).is_err() {
                        Err("diagnostic worker busy".into())
                    } else {
                        continue;
                    }
                }
            };
            let response = match result {
                Ok(v) => Response::ok(id, v),
                Err(e) => Response::error(id, e),
            };
            let _ = reply.try_send(response);
        }
        if workload_tick.elapsed() >= Duration::from_secs(1) {
            workload_tick = Instant::now();
            rt.playback_hints
                .publish(reborn_platform::workload::playback_class(
                    matches!(
                        rt.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    ),
                    rt.model.settings.crossfade_ms > 0 || rt.model.settings.eq_enabled,
                    false, // The scanner owns its independent scoped lease.
                ));
        }
        if rt.background.update(
            &rt.model.settings.music_directory,
            &rt.model.sources,
            rt.sleep_prepared,
        ) {
            observation_pending = None;
        }
        if let Ok(observation) = observations.try_recv().and_then(|o| {
            if rt.background.accepts(&o.request) {
                Ok(o)
            } else {
                Err(std::sync::mpsc::TryRecvError::Empty)
            }
        }) {
            observation_pending = None;
            if performance.enabled() {
                performance.record(
                    "background_observation",
                    json!({"elapsed_ms":observation.elapsed_ms}),
                );
            }
            let next_power = observation.power;
            if next_power["supplies"] != rt.power["supplies"] {
                log.emit(
                    Level::Debug,
                    "power",
                    "supply_changed",
                    "Power supply state changed",
                    None,
                    next_power.clone(),
                );
            }
            rt.present_battery(power::battery(&next_power));
            rt.power = next_power;
            if let Some((sd, sources)) = observation
                .media
                .filter(|_| observation.request.music == rt.model.settings.music_directory)
            {
                if sd && !last_sd && !storage::platform_manages_media() {
                    let _ = storage::mount_sd();
                }
                if !sd && last_sd {
                    rt.pause();
                    if !storage::platform_manages_media() {
                        let _ = storage::unmount_sd();
                    }
                }
                last_sd = sd;
                if sources != rt.model.sources {
                    rt.abandon_pending_reconfiguration();
                    let old = rt.model.sources.clone();
                    for s in &sources {
                        if !old.iter().any(|v| v.id == s.id) {
                            log.add("sd_insertions", 1.);
                            log.emit(
                                Level::Info,
                                "storage",
                                "source_inserted",
                                "Media source mounted",
                                None,
                                json!({"source":s}),
                            );
                        }
                    }
                    for s in &old {
                        if !sources.iter().any(|v| v.id == s.id) {
                            if matches!(s.kind, reborn_core::MediaSource::SdCard(_)) {
                                rt.ui.flash("SD card removed");
                            }
                            log.add("sd_removals", 1.);
                            log.emit(
                                Level::Info,
                                "storage",
                                "source_removed",
                                "Media source offline",
                                None,
                                json!({"source_id":s.id}),
                            );
                        }
                    }
                    let g = rt.model.generation;
                    rt.model.apply(Event::SourceChanged(sources.clone()));
                    rt.ui.invalidate_catalog();
                    if let Some(worker) = &mut rt.thumbs {
                        worker.forget_failures();
                    }
                    if g != rt.model.generation {
                        rt.playback.stop(rt.model.generation)
                    }
                    let _ = rt.db.sources(sources);
                    // Publish cached metadata with the new online flags now;
                    // scanner completion reloads only actual row changes.
                    rt.query = rt.db.catalog().ok();
                    rt.model.apply(Event::LibraryScanStarted);
                    if let Err(error) = rt.scanner.scan(rt.model.sources.clone()) {
                        rt.model.apply(Event::LibraryScanFailed(error.clone()));
                        rt.fail("scanner", error);
                    }
                    rt.dirty.mark_render();
                    rt.dirty.mark_persistence();
                }
                let (generation, copying) = storage::usb_changes();
                let now = Instant::now();
                usb_refresh.observe(generation, copying, now);
                if usb_refresh.ready(
                    rt.model.library.scanning,
                    !rt.model.sources.is_empty(),
                    rt.sleep_prepared,
                    now,
                ) && rt.scanner.scan(rt.model.sources.clone()).is_ok()
                {
                    usb_refresh.requested();
                    rt.model.apply(Event::LibraryScanStarted);
                    rt.dirty.mark_render();
                }
            }
        }
        if periodic.elapsed() > Duration::from_secs(2) {
            periodic = Instant::now();
            rt.automatic_sleep_tick();
            if rt.background.update(
                &rt.model.settings.music_directory,
                &rt.model.sources,
                rt.sleep_prepared,
            ) {
                observation_pending = None;
            }
            rt.background.refresh_pending(&mut observation_pending);
            if observation_pending.is_none() {
                if let Some(request) = rt.background.request() {
                    if observation_requests.try_send(request.clone()).is_ok() {
                        observation_pending = Some(request);
                    }
                }
            }
            if !headless
                && !rt.model.screen_off
                && !matches!(
                    rt.model.platform.busy,
                    Some(
                        reborn_core::PlatformTask::UpdateApply
                            | reborn_core::PlatformTask::UpdateRollback
                    )
                )
                && rt.model.settings.screen_timeout_seconds != 0
                && rt.last_activity.elapsed().as_secs()
                    >= rt.model.settings.screen_timeout_seconds as u64
            {
                let _ = rt.effect(Effect::ScreenSleep);
            }
            if let Ok(mut s) = last_snapshot.lock() {
                *s = rt.snapshot();
                log.context(rt.status());
            }
            let xruns = log.metrics()["audio_xruns"].as_f64().unwrap_or(0.);
            if xruns - xrun_bundle >= 3. {
                xrun_bundle = xruns;
                let _ = log.diagnostic(&root.join("diagnostics"), rt.snapshot(), true);
            }
        }
        if checkpoint.elapsed() > Duration::from_secs(15) && rt.dirty.checkpoint_due() {
            rt.checkpoint();
            checkpoint = Instant::now();
        } else if checkpoint.elapsed() > Duration::from_secs(15) {
            checkpoint = Instant::now();
        }
        if rt.dirty.render
            && !rt.model.screen_off
            && render_time.elapsed() >= frame_gap(rt.ui.gliding())
        {
            render_time = Instant::now();
            let frame_started = Instant::now();
            let tracks = std::mem::take(&mut rt.model.library.tracks);
            rt.ui.normalize(&mut rt.model, &tracks);
            rt.model.library.tracks = tracks;
            if rt.model.screen == Screen::DiagnosticSection && rt.model.navigation.filter == "audio"
            {
                rt.model.platform.audio_facts = audio_facts(&rt.playback.audio_state(), &rt.model);
            }
            if let Some(g) = &rt.graphics {
                rt.ui.set_resident_thumbs(g.resident_thumbs());
            }
            let mut draw = rt.ui.draw(&rt.model, &rt.model.library.tracks, rt.art);
            let layout_ms = frame_started.elapsed().as_secs_f64() * 1000.;
            if !first_frame_presented {
                // The splash's bar completes while this first frame renders.
                reborn_platform::contract::boot_milestone("ready");
            }
            let fading = rt.boot_fade > 0;
            if fading {
                // The first frame is the complete boot screen over the real UI
                // (identical to the splash's last frame); it dissolves at once.
                // A step is only consumed once its frame was presented.
                draw = reborn_ui::boot_transition(
                    draw,
                    f32::from(rt.boot_fade) / f32::from(reborn_ui::BOOT_FADE_FRAMES),
                );
            }
            if rt.graphics.is_none() && fading {
                rt.boot_fade -= 1;
            }
            let mut presentation_failed = false;
            if let Some(g) = &mut rt.graphics {
                if let Err(e) = g.render(&draw) {
                    presentation_failed = true;
                    rt.fail("graphics", e);
                    rt.graphics = None;
                    // One bounded recreation attempt. Further attempts wait for an explicit wake.
                    if rt.graphics_recovery.failed() {
                        if let Err(error) = rt.recreate_graphics() {
                            rt.fail("graphics", error);
                        }
                    }
                    let _ = log.diagnostic(&root.join("diagnostics"), rt.snapshot(), true);
                } else {
                    rt.graphics_recovery.presented();
                    if performance.enabled() {
                        performance.record("frame", json!({"screen":rt.model.screen,"focus":rt.model.navigation.focus,"quads":draw.len(),"layout_ms":layout_ms,"frame_ms":frame_started.elapsed().as_secs_f64()*1000.,"render":g.timings}));
                    }
                    if let Some((_, _, _, events)) = &mut monitor {
                        let presented = input::monotonic_us();
                        for event in events
                            .iter_mut()
                            .filter(|event| event["pending_frame"] == true)
                        {
                            event["present_return_mono_us"] = json!(presented);
                            event["pending_frame"] = json!(false);
                        }
                    }
                    if fading {
                        rt.boot_fade -= 1;
                    }
                    if rt.boot_fade == 0 && !first_interactive_presented {
                        first_interactive_presented = true;
                        log.emit(
                            Level::Info,
                            "startup",
                            "interactive",
                            "Boot cover fully revealed; input enabled",
                            None,
                            json!({"elapsed_ms":process_started.elapsed().as_millis()}),
                        );
                    }
                    if !first_frame_presented {
                        first_frame_presented = true;
                        if !headless {
                            if let Err(error) = reborn_platform::contract::application_ready() {
                                log.emit(
                                    Level::Warn,
                                    "platform",
                                    "readiness",
                                    &error,
                                    None,
                                    json!({}),
                                );
                            }
                        }
                        log.emit(
                            Level::Info,
                            "startup",
                            "ready",
                            "First Reborn frame presented",
                            None,
                            json!({"display_handoff":"explicit KMS presentation",
                            "elapsed_ms":process_started.elapsed().as_millis()}),
                        );
                        // Early splash evidence lives on the /run mount carried out
                        // of initramfs; retain this bounded record alongside app logs.
                        if let Some(bytes) = reborn_platform::contract::splash_evidence() {
                            let _ = reborn_core::atomic_write(
                                &root.join("logs/splash-boot.jsonl"),
                                &bytes,
                            );
                        }
                    }
                }
            }
            if rt.boot_fade == 0 && !presentation_failed {
                rt.dirty.rendered();
            }
        }
        let interval = main_loop_interval(
            rt.model.screen_off,
            rt.dirty.render,
            rt.ui.gliding(),
            matches!(
                rt.model.playback,
                PlaybackState::Playing | PlaybackState::Buffering
            ),
            render_time.elapsed(),
        );
        if let Some(input) = &inputs {
            if input.wait_with(interval, Some(server.wake_fd())).is_err() {
                thread::sleep(interval);
            }
        } else {
            thread::sleep(interval);
        }
    }
    // Platform shutdown contract: present the transition, save state, close
    // audio and the database, end on a dark frame with the backlight off,
    // then acknowledge. The platform's own deadline bounds all of this; it
    // proceeds without us if Reborn hangs or crashes.
    let mut farewell = shutdown_intent.as_ref().and_then(|intent| {
        rt.model.platform.shutting_down = Some(intent.clone());
        rt.model.navigation.modal = None;
        let last = rt.ui.draw(&rt.model, &rt.model.library.tracks, rt.art);
        let closing = reborn_ui::closing_label(intent.restart, intent.low_battery);
        let plan = reborn_ui::shutdown_plan(intent.low_battery);
        rt.graphics.take().map(|g| (g, last, closing, plan))
    });
    // The UI dissolves into the shutdown screen, which says "Saving" while the
    // real save below runs; its status then becomes the closing label.
    if let Some((g, last, closing, plan)) = farewell.as_mut() {
        for frame in 0..plan.close_frame {
            let started = Instant::now();
            let _ = g.render(&reborn_ui::shutdown_frame(last, frame, closing, *plan));
            thread::sleep(Duration::from_millis(34).saturating_sub(started.elapsed()));
        }
    }
    rt.model.invalidate();
    let mut shutdown_ready = true;
    if let Err(e) = rt.playback.shutdown(rt.model.generation) {
        shutdown_ready = false;
        log.emit(
            Level::Error,
            "audio",
            "shutdown_failed",
            &e,
            None,
            json!({}),
        );
    }
    rt.model.playback = PlaybackState::Paused;
    rt.pending_reconfiguration = None;
    rt.checkpoint();
    shutdown_ready &= !rt.dirty.persistence;
    rt.scanner.stop();
    if let Err(error) = rt.db.shutdown(Duration::from_secs(2)) {
        shutdown_ready = false;
        log.emit(
            Level::Error,
            "database",
            "shutdown_failed",
            &error,
            None,
            json!({}),
        );
    }
    if let Some((g, last, closing, plan)) = farewell.as_mut() {
        for frame in plan.close_frame..plan.frames {
            let started = Instant::now();
            let _ = g.render(&reborn_ui::shutdown_frame(last, frame, closing, *plan));
            thread::sleep(Duration::from_millis(34).saturating_sub(started.elapsed()));
        }
        // Final dark frame first, then the backlight: the panel never shows a
        // white, stale or console framebuffer while it is lit. One refresh
        // lets the black frame reach the panel before its light goes out.
        let _ = g.render(&reborn_ui::black_frame());
        thread::sleep(Duration::from_millis(34));
        if let Err(error) = power::blank(true) {
            log.emit(
                Level::Warn,
                "power",
                "backlight_off_failed",
                &error,
                None,
                json!({}),
            );
        }
        log.emit(
            Level::Info,
            "power",
            "shutdown_presented",
            "Shutdown transition presented",
            None,
            json!({"frames":farewell.as_ref().map_or(0, |f| f.3.frames) + 1}),
        );
    }
    if let Some(intent) = &shutdown_intent {
        if let Err(error) = power::acknowledge_shutdown(&intent.id, shutdown_ready) {
            log.emit(Level::Warn, "power", "ack_failed", &error, None, json!({}));
        }
    }
    // Keep the DRM device (and its dark frame) open until the process exits;
    // the platform powers the panel down after the acknowledgement.
    let _keep_scanout = farewell;
    if let Some(w) = rt.wifi {
        let _ = w.commands.try_send(wifi::Command::Stop);
    }
    if let Some(b) = rt.bluetooth {
        let _ = b.commands.try_send(bluetooth::Command::Stop);
    }
    let _ = fs::remove_file(socket);
    log.emit(
        Level::Info,
        "core",
        "shutdown",
        "Reborn stopped",
        None,
        json!({}),
    );
    Ok(())
}

/// Why the platform did not turn off to install, in the owner's words.
fn install_refusal(error: &str) -> String {
    use reborn_core::platform::UpdateProblem;
    match client::update_problem(error) {
        UpdateProblem::NeedsCharge => "Connect the charger or charge to half first".into(),
        UpdateProblem::NeedsComputer => "This player needs a computer to update".into(),
        UpdateProblem::VerificationFailed => "The update couldn't be verified".into(),
        _ => "The update couldn't start. Nothing was changed.".into(),
    }
}

fn main() {
    if let Err(error) = run() {
        let record = json!({"level":"ERROR","subsystem":"startup","event":"fatal","message":error});
        let _ = serde_json::to_writer(std::io::stderr(), &record);
        let _ = std::io::stderr().write_all(b"\n");
        std::process::exit(1)
    }
}

#[cfg(test)]
mod artwork_presentation_tests {
    use super::artwork_matches_current;
    use reborn_core::{AppModel, Event, QueueEntryId, Track};

    #[test]
    fn duplicate_track_occurrences_accept_art_only_after_their_boundary() {
        let track = Track {
            id: 4,
            ..Default::default()
        };
        let mut model = AppModel {
            generation: 7,
            queue: vec![track.clone(), track],
            queue_entry_ids: vec![QueueEntryId(21), QueueEntryId(22)],
            ..Default::default()
        };

        assert!(artwork_matches_current(&model, 7, QueueEntryId(21), 4));
        assert!(!artwork_matches_current(&model, 7, QueueEntryId(22), 4));

        model.apply(Event::TrackBoundary {
            generation: 7,
            next_entry_id: Some(QueueEntryId(22)),
            next_track_id: Some(4),
            output_position_ms: 0,
        });

        assert_eq!(model.current_entry_id(), Some(QueueEntryId(22)));
        assert!(artwork_matches_current(&model, 7, QueueEntryId(22), 4));
        assert!(!artwork_matches_current(&model, 6, QueueEntryId(22), 4));
    }
}

#[cfg(test)]
mod bluetooth_epoch_tests {
    use super::{transport_epoch_state, TransportEpochState};
    use crate::bluetooth::{self, Device};
    use reborn_core::{AudioOutput, BluetoothPcm, PlaybackState};

    fn status(generation: u64) -> bluetooth::Status {
        let mut status = bluetooth::Status::default();
        status.bluealsa = true;
        status.devices = vec![Device {
            path: "/org/bluez/hci0/dev_01_02_03_04_05_06".into(),
            address: "01:02:03:04:05:06".into(),
            connected: true,
            audio: true,
            ..Default::default()
        }];
        status.pcms = vec![BluetoothPcm {
            object: "/org/bluealsa/hci0/dev_01_02_03_04_05_06/a2dp".into(),
            device: status.devices[0].path.clone(),
            transport: "A2DP-source".into(),
            mode: "sink".into(),
            codec: Some("SBC".into()),
            format: Some(0x8210),
            rate: Some(44_100),
            channels: Some(2),
            transport_generation: generation,
            ..Default::default()
        }];
        status
    }

    #[test]
    fn active_sink_is_reopened_or_stopped_when_transport_epoch_is_stale() {
        let output = AudioOutput::Bluetooth("01:02:03:04:05:06".into());
        let current = status(10);
        assert_eq!(
            transport_epoch_state(&output, PlaybackState::Playing, Some(10), &current),
            TransportEpochState::Current
        );
        assert_eq!(
            transport_epoch_state(&output, PlaybackState::Playing, Some(10), &status(11)),
            TransportEpochState::Changed
        );

        let mut disappeared = status(10);
        disappeared.pcms.clear();
        assert_eq!(
            transport_epoch_state(&output, PlaybackState::Playing, Some(10), &disappeared),
            TransportEpochState::Unavailable
        );
        assert_eq!(
            transport_epoch_state(&output, PlaybackState::Paused, Some(10), &status(11)),
            TransportEpochState::Current
        );
    }
}

#[cfg(test)]
mod runtime_dirty_tests {
    use super::DirtyState;

    #[test]
    fn stopped_setting_save_keeps_its_redraw_request() {
        let mut dirty = DirtyState::default();
        dirty.mark_both(); // SetScreenTimeout while playback is stopped.

        dirty.checkpoint_succeeded();

        assert!(!dirty.checkpoint_due());
        assert!(dirty.render);
    }

    #[test]
    fn rendering_before_checkpoint_keeps_persistence_pending() {
        let mut dirty = DirtyState::default();
        dirty.mark_both();

        dirty.rendered();

        assert!(dirty.checkpoint_due());
        dirty.checkpoint_succeeded();
        assert!(!dirty.checkpoint_due());
    }

    #[test]
    fn checkpoint_before_render_keeps_redraw_pending() {
        let mut dirty = DirtyState::default();
        dirty.mark_both();

        dirty.checkpoint_succeeded();

        assert!(dirty.render);
        dirty.rendered();
        assert!(!dirty.render);
    }

    #[test]
    fn bluetooth_loss_notice_can_be_saved_and_still_rendered() {
        let mut dirty = DirtyState::default();
        dirty.mark_both(); // Lost transport changes output/playback and sets a notice.

        dirty.checkpoint_succeeded();

        assert!(!dirty.checkpoint_due());
        assert!(dirty.render);
    }

    #[test]
    fn rendered_position_change_remains_due_for_bounded_checkpoint() {
        let mut dirty = DirtyState::default();
        dirty.mark_persistence(); // Position events are saved on the 15-second cadence.
        dirty.mark_render();
        dirty.rendered();

        assert!(dirty.checkpoint_due());
        dirty.checkpoint_succeeded();
        assert!(!dirty.checkpoint_due());
    }

    #[test]
    fn unchanged_state_does_not_schedule_a_save_loop() {
        let mut dirty = DirtyState::default();
        assert!(!dirty.checkpoint_due());

        dirty.mark_render();
        dirty.rendered();

        assert!(!dirty.checkpoint_due());
    }

    #[test]
    fn failed_checkpoint_retains_persistence_intent() {
        let mut dirty = DirtyState::default();
        dirty.mark_persistence();

        dirty.checkpoint_failed();

        assert!(dirty.checkpoint_due());
    }
}

#[cfg(test)]
mod logo_fetcher_tests {
    use super::LogoFetcher;

    /// A logo that cannot be fetched is settled: asking again sends nothing
    /// and finishing it asks for no new frame (the redraw loop of 5.1–6).
    #[test]
    fn a_failed_logo_is_never_asked_for_again() {
        let cache = std::env::temp_dir().join(format!("reborn-logos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        let mut fetcher = LogoFetcher::spawn(cache.clone()).unwrap();
        let key = reborn_core::logo_key("not a web address");
        fetcher.want(key, "not a web address");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut frames = 0;
        while !fetcher.is_settled(key) && std::time::Instant::now() < deadline {
            frames += fetcher.finished();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(fetcher.is_settled(key));
        assert_eq!(frames, 0, "a failure asks for no frame");
        fetcher.want(key, "not a web address");
        assert!(fetcher.pending.is_empty(), "never sent again");
        let _ = std::fs::remove_dir_all(cache);
    }
}

#[cfg(test)]
mod runtime_reconfiguration_tests {
    use super::{failed_reconfiguration_model, run_sink_reconfiguration};
    use crate::playback::Playback;
    use reborn_audio::{AudioSink, Parameters, SinkSpec};
    use reborn_core::{
        AppModel, AudioOutput, EqBand, PcmFormat, PlaybackState, QueueEntryId, ReplayGainMode,
        Track,
    };
    use reborn_media::DspConfig;
    use reborn_observability::Observer;
    use std::{
        path::PathBuf,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    struct CountingSink {
        active: Arc<AtomicUsize>,
    }
    impl AudioSink for CountingSink {
        fn parameters(&self) -> Parameters {
            Parameters {
                rate: 44_100,
                period: 512,
                buffer: 4096,
                format: PcmFormat::S32LE,
                channels: 2,
                hardware_mixer_gain_db: None,
                device: "transaction-test".into(),
                fallback: false,
                fallback_reason: String::new(),
            }
        }
        fn write(&mut self, pcm: &[u8]) -> Result<usize, String> {
            Ok(pcm.len() / 8)
        }
        fn discard(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn delay(&self) -> u64 {
            0
        }
    }
    impl Drop for CountingSink {
        fn drop(&mut self) {
            self.active.fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn active_model() -> AppModel {
        let first = Track {
            id: 1,
            path: PathBuf::from("/music/first.flac"),
            ..Default::default()
        };
        let second = Track {
            id: 2,
            path: PathBuf::from("/music/second.flac"),
            ..Default::default()
        };
        let mut model = AppModel {
            playback: PlaybackState::Playing,
            queue: vec![first, second],
            queue_entry_ids: vec![QueueEntryId(11), QueueEntryId(12)],
            queue_position: 0,
            position_ms: 12_345,
            generation: 7,
            output: AudioOutput::Wired,
            ..Default::default()
        };
        model.settings.volume = 42;
        model.settings.replay_gain = ReplayGainMode::Off;
        model.settings.eq_enabled = false;
        model.settings.eq_bands = vec![EqBand::default()];
        model
    }

    fn test_spec() -> SinkSpec {
        SinkSpec {
            output: AudioOutput::Wired,
            rate: 44_100,
            format: PcmFormat::S32LE,
            physical_bits: 32,
            valid_bits: 32,
            channels: 2,
            layout: "stereo".into(),
            device: "transaction-test".into(),
            codec: None,
            transport_object: None,
            transport_device: None,
            transport: None,
            mode: None,
            transport_generation: 0,
            fallback: false,
            fallback_reason: String::new(),
        }
    }

    #[test]
    fn plan_failure_after_release_never_starts_or_keeps_the_old_sink() {
        let root = std::env::temp_dir().join(format!("reborn-plan-failure-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let open_count = Arc::new(AtomicUsize::new(0));
        let factory_count = open_count.clone();
        let peak = Arc::new(AtomicUsize::new(0));
        let factory_peak = peak.clone();
        let mut playback = Playback::with_sink(
            log,
            root,
            Box::new(move |_, _, _| {
                let active = factory_count.fetch_add(1, Ordering::AcqRel) + 1;
                factory_peak.fetch_max(active, Ordering::AcqRel);
                Ok(Box::new(CountingSink {
                    active: factory_count.clone(),
                }))
            }),
        )
        .unwrap();
        playback
            .load(
                Track::default(),
                vec![],
                0,
                test_spec(),
                1,
                DspConfig::with_volume(42),
                1,
            )
            .unwrap();
        assert_eq!(open_count.load(Ordering::Acquire), 1);
        let mut started = false;
        let error = run_sink_reconfiguration(
            &mut playback,
            2,
            || Err::<SinkSpec, _>("injected sink plan failure".into()),
            |_, _| {
                started = true;
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.contains("injected sink plan failure"));
        assert!(!started);
        assert_eq!(open_count.load(Ordering::Acquire), 0);
        playback.shutdown(3).unwrap();
    }

    #[test]
    fn new_sink_opens_only_after_old_sink_release_acknowledges() {
        let root = std::env::temp_dir().join(format!("reborn-plan-start-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let factory_active = active.clone();
        let peak = Arc::new(AtomicUsize::new(0));
        let factory_peak = peak.clone();
        let mut playback = Playback::with_sink(
            log,
            root,
            Box::new(move |_, _, _| {
                let count = factory_active.fetch_add(1, Ordering::AcqRel) + 1;
                factory_peak.fetch_max(count, Ordering::AcqRel);
                Ok(Box::new(CountingSink {
                    active: factory_active.clone(),
                }))
            }),
        )
        .unwrap();
        playback
            .load(
                Track::default(),
                vec![],
                0,
                test_spec(),
                1,
                DspConfig::with_volume(42),
                1,
            )
            .unwrap();
        assert_eq!(active.load(Ordering::Acquire), 1);
        let track = Track {
            id: 3,
            source_id: "internal".into(),
            path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac"),
            ..Default::default()
        };

        run_sink_reconfiguration(
            &mut playback,
            2,
            || Ok(test_spec()),
            move |playback, spec| {
                playback.start_with_gapless(crate::playback::LoadRequest {
                    music_root: track.path.parent().unwrap().into(),
                    track,
                    entry_id: QueueEntryId(3),
                    queue: vec![],
                    queue_entry_ids: vec![],
                    position: 0,
                    spec,
                    generation: 2,
                    dsp: DspConfig::with_volume(42),
                    gapless_enabled: true,
                    id: 2,
                })
            },
        )
        .unwrap();

        assert_eq!(active.load(Ordering::Acquire), 1);
        assert_eq!(peak.load(Ordering::Acquire), 1);
        playback.shutdown(3).unwrap();
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn output_seek_volume_replaygain_and_eq_failures_restore_prior_model() {
        let previous = active_model();
        let transition_generation = 10;
        let mut intended = previous.clone();
        intended.output = AudioOutput::Bluetooth("12:34:56:78:90:AB".into());
        intended.queue_position = 1;
        intended.position_ms = 54_321;
        intended.settings.volume = 80;
        intended.settings.replay_gain = ReplayGainMode::Track;
        intended.settings.eq_enabled = true;
        assert_ne!(intended.output, previous.output);
        assert_ne!(intended.settings.volume, previous.settings.volume);

        let restored = failed_reconfiguration_model(
            previous.clone(),
            "injected start failure",
            true,
            Some(transition_generation),
        );
        assert_eq!(restored.output, previous.output);
        assert_eq!(restored.settings.volume, previous.settings.volume);
        assert_eq!(restored.settings.replay_gain, previous.settings.replay_gain);
        assert_eq!(restored.settings.eq_enabled, previous.settings.eq_enabled);
        assert_eq!(restored.queue_position, previous.queue_position);
        assert_eq!(restored.position_ms, previous.position_ms);
        assert_eq!(restored.queue[0].id, previous.queue[0].id);
        assert_eq!(restored.playback, PlaybackState::Error);
        assert_eq!(
            restored.last_error.as_deref(),
            Some("injected start failure")
        );
        assert_eq!(restored.generation, transition_generation + 1);
    }

    #[test]
    fn pre_release_failure_preserves_the_still_running_model() {
        let previous = active_model();
        let restored = failed_reconfiguration_model(
            previous.clone(),
            "transport observation unavailable",
            false,
            None,
        );

        assert_eq!(restored.playback, PlaybackState::Playing);
        assert_eq!(restored.output, previous.output);
        assert_eq!(restored.settings.volume, previous.settings.volume);
        assert_eq!(restored.generation, previous.generation);
        assert_eq!(restored.last_error, previous.last_error);
    }
}

#[cfg(test)]
mod ui_readiness_tests {
    use super::*;
    #[test]
    fn authentication_never_means_connected_and_problems_are_typed() {
        let mut s = wifi::Status {
            available: true,
            enabled: true,
            ssid: "Home".into(),
            state: "COMPLETED".into(),
            ..Default::default()
        };
        assert_eq!(
            wifi_view(&s).status,
            WifiStatus::GettingAddress("Home".into())
        );
        for state in [
            "Starting",
            "Scanning",
            "Associating",
            "Authenticated",
            "AcquiringIP",
        ] {
            s.readiness = state.into();
            assert!(!matches!(wifi_view(&s).status, WifiStatus::Connected(_)));
        }
        s.readiness = "Online".into();
        assert_eq!(wifi_view(&s).status, WifiStatus::Connected("Home".into()));
        // A late platform record falls back to the supplicant: associated
        // with an address is connected, not "Not connected" (Beta 4).
        s.readiness = String::new();
        s.readiness_reason = Some("platform_readiness_stale".into());
        s.ip = "192.0.2.2".into();
        assert_eq!(wifi_view(&s).status, WifiStatus::Connected("Home".into()));
        s.ip.clear();
        s.readiness = "Failed".into();
        s.readiness_reason = Some("wrong_credentials".into());
        assert_eq!(
            wifi_view(&s).problem,
            Some(reborn_core::platform::WifiProblem::WrongPassword)
        );
        // Service text can never reach the screen: problems are an enum.
        s.readiness_reason = Some("org.secret.service credential=abc".into());
        assert_eq!(
            wifi_view(&s).problem,
            Some(reborn_core::platform::WifiProblem::WrongPassword)
        );
    }

    #[test]
    fn joined_network_is_listed_first_and_saved_networks_keep_their_identity() {
        let s = wifi::Status {
            available: true,
            enabled: true,
            ssid: "Home".into(),
            readiness: "Online".into(),
            networks: vec![
                wifi::Network {
                    ssid: "Cafe".into(),
                    signal: -40,
                    security: "[WPA2-PSK-CCMP]".into(),
                    saved_id: None,
                },
                wifi::Network {
                    ssid: "Home".into(),
                    signal: -75,
                    security: "[WPA2-PSK-CCMP]".into(),
                    saved_id: None,
                },
            ],
            saved: vec![wifi::Network {
                ssid: "Home".into(),
                signal: 0,
                security: String::new(),
                saved_id: Some(4),
            }],
            ..Default::default()
        };
        let view = wifi_view(&s);
        assert_eq!(view.networks[0].ssid, "Home");
        assert_eq!(view.networks[0].saved_id, Some(4));
        assert_eq!(view.networks.len(), 2, "saved and visible are merged");
    }
}

#[cfg(test)]
mod loop_interval_tests {
    use super::main_loop_interval;
    use std::time::Duration;

    #[test]
    fn screen_off_wakes_less_but_stays_responsive() {
        assert_eq!(
            main_loop_interval(false, false, false, false, Duration::ZERO),
            Duration::from_millis(250)
        );
        assert_eq!(
            main_loop_interval(true, true, false, false, Duration::ZERO),
            Duration::from_secs(1)
        );
        assert_eq!(
            main_loop_interval(false, true, false, false, Duration::from_millis(16)),
            Duration::from_millis(18)
        );
        assert_eq!(
            main_loop_interval(false, true, false, false, Duration::from_millis(40)),
            Duration::ZERO
        );
        // A glide keeps frames coming, even with nothing marked dirty yet
        // (Cover Flow measured ~10 fps without this), and the page flip paces
        // them rather than the calm 34-ms budget (27 fps measured on 24).
        assert_eq!(
            main_loop_interval(false, false, true, false, Duration::from_millis(6)),
            Duration::from_millis(6)
        );
        assert_eq!(
            main_loop_interval(false, true, true, true, Duration::from_millis(16)),
            Duration::ZERO
        );
        assert!(super::frame_gap(true) < Duration::from_micros(15_800));
        // Screen off never animates.
        assert_eq!(
            main_loop_interval(true, false, true, false, Duration::ZERO),
            Duration::from_secs(1)
        );
    }
}

#[cfg(test)]
mod feature_product_tests {
    use super::*;
    #[test]
    fn paused_player_can_change_codec_without_a_hidden_stop_action() {
        for state in [PlaybackState::Stopped, PlaybackState::Paused] {
            assert!(codec_change_allowed(state, false));
            assert!(!codec_change_allowed(state, true));
        }
        for state in [
            PlaybackState::Playing,
            PlaybackState::Buffering,
            PlaybackState::Error,
        ] {
            assert!(!codec_change_allowed(state, false));
        }
    }
}

#[cfg(test)]
mod auto_output_tests {
    use super::*;
    fn status(address: &str, path: &str) -> bluetooth::Status {
        let mut status = bluetooth::Status::default();
        status.available = true;
        status.powered = true;
        status.bluealsa = true;
        add(&mut status, address, path);
        status
    }
    fn add(status: &mut bluetooth::Status, address: &str, path: &str) {
        status.devices.push(bluetooth::Device {
            address: address.into(),
            path: path.into(),
            name: "AirPods Pro".into(),
            connected: true,
            audio: true,
            paired: true,
            bonded: true,
            ..Default::default()
        });
        status.pcms.push(reborn_core::BluetoothPcm {
            device: path.into(),
            transport: "A2DP-source".into(),
            mode: "sink".into(),
            running: Some(false),
            transport_generation: 1,
            ..Default::default()
        });
    }
    #[test]
    fn bluetooth_headphones_win_over_the_jack_once_per_connection() {
        let mut auto = AutoOutput::default();
        let mut s = status("AA", "/a");
        let pcm = s.pcms.clone();
        // Connected but BlueALSA has no playback PCM yet: wait.
        s.pcms.clear();
        assert!(auto.next(&s, &AudioOutput::Wired).is_none());
        s.pcms = pcm.clone();
        assert_eq!(auto.next(&s, &AudioOutput::Wired).as_deref(), Some("AA"));
        auto.routed("AA".into());
        // The owner then picks the jack by hand: that choice sticks.
        assert!(auto.next(&s, &AudioOutput::Wired).is_none());
        // A codec change recreates the PCM; still the same connection.
        s.pcms.clear();
        assert!(auto.next(&s, &AudioOutput::Wired).is_none());
        s.pcms = pcm.clone();
        assert!(auto.next(&s, &AudioOutput::Wired).is_none());
        // A failed observation proves nothing.
        s.available = false;
        s.devices[0].connected = false;
        assert!(auto.next(&s, &AudioOutput::Wired).is_none());
        s.available = true;
        s.devices[0].connected = true;
        assert!(auto.next(&s, &AudioOutput::Wired).is_none());
        // A real reconnect routes again.
        s.devices[0].connected = false;
        assert!(auto.next(&s, &AudioOutput::Wired).is_none());
        s.devices[0].connected = true;
        assert_eq!(auto.next(&s, &AudioOutput::Wired).as_deref(), Some("AA"));
        // Radio off and on again is a new connection too.
        auto.routed("AA".into());
        s.powered = false;
        assert!(auto.next(&s, &AudioOutput::Wired).is_none());
        s.powered = true;
        assert_eq!(auto.next(&s, &AudioOutput::Wired).as_deref(), Some("AA"));
    }
    #[test]
    fn already_on_bluetooth_is_never_displaced() {
        let mut auto = AutoOutput::default();
        let mut s = status("AA", "/a");
        // Restored or chosen by hand: nothing to do, and none later.
        assert!(auto
            .next(&s, &AudioOutput::Bluetooth("aa".into()))
            .is_none());
        assert!(auto.next(&s, &AudioOutput::Wired).is_none());
        // A second peer does not take over a working first one...
        add(&mut s, "BB", "/b");
        assert!(auto
            .next(&s, &AudioOutput::Bluetooth("AA".into()))
            .is_none());
        // ...but takes over once the first is gone and the jack is back.
        s.devices[0].connected = false;
        assert_eq!(auto.next(&s, &AudioOutput::Wired).as_deref(), Some("BB"));
        // A peer without the audio sink profile is never an output.
        let mut keyboard = status("CC", "/c");
        keyboard.devices[0].audio = false;
        assert!(AutoOutput::default()
            .next(&keyboard, &AudioOutput::Wired)
            .is_none());
    }
}

#[cfg(test)]
mod codec_reconnect_tests {
    use super::*;
    fn status() -> bluetooth::Status {
        let mut status = bluetooth::Status::default();
        status.available = true;
        status.powered = true;
        status.bluealsa = true;
        status.devices.push(bluetooth::Device {
            address: "AA".into(),
            path: "/peer".into(),
            connected: true,
            audio: true,
            paired: true,
            bonded: true,
            ..Default::default()
        });
        status.pcms.push(reborn_core::BluetoothPcm {
            device: "/peer".into(),
            transport: "A2DP-source".into(),
            mode: "sink".into(),
            running: Some(false),
            transport_generation: 1,
            ..Default::default()
        });
        status
    }
    #[test]
    fn saved_codec_retries_only_on_real_reconnect_and_never_during_playback() {
        let mut guard = CodecReconnect::default();
        let mut s = status();
        assert!(guard.next(&s, PlaybackState::Playing, false).is_none());
        assert!(guard.next(&s, PlaybackState::Paused, true).is_none());
        s.pcms[0].running = None;
        assert!(guard.next(&s, PlaybackState::Paused, false).is_none());
        s.pcms[0].running = Some(false);
        assert_eq!(
            guard.next(&s, PlaybackState::Paused, false).as_deref(),
            Some("AA")
        );
        // Queue-full does not consume the sole attempt; caller confirms send.
        assert_eq!(
            guard.next(&s, PlaybackState::Paused, false).as_deref(),
            Some("AA")
        );
        guard.sent("AA".into());
        s.pcms[0].transport_generation = 2; // Codec selection may recreate PCM.
        assert!(guard.next(&s, PlaybackState::Paused, false).is_none());
        s.available = false;
        s.devices[0].connected = false;
        assert!(guard.next(&s, PlaybackState::Paused, false).is_none());
        s.available = true;
        s.devices[0].connected = true; // Failed snapshot did not prove a disconnect.
        assert!(guard.next(&s, PlaybackState::Paused, false).is_none());
        s.devices[0].connected = false;
        assert!(guard.next(&s, PlaybackState::Paused, false).is_none());
        s.devices[0].connected = true;
        assert_eq!(
            guard.next(&s, PlaybackState::Stopped, false).as_deref(),
            Some("AA")
        );
    }
    #[test]
    fn prepared_sleep_allows_restore_and_observation_and_refuses_mutations() {
        for command in [
            Command::Status,
            Command::Audio,
            Command::Health,
            Command::Metrics,
            Command::Snapshot,
            Command::SleepRestore,
        ] {
            assert!(sleep_control_allowed(&command));
        }
        for command in [
            Command::SleepPrepare,
            Command::Scan,
            Command::Diagnose,
            Command::Tests,
        ] {
            assert!(!sleep_control_allowed(&command));
        }
    }
}
