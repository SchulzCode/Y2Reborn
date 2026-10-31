#![forbid(unsafe_code)]
mod artwork;
mod background;
mod books;
mod dark;
mod diagnostics;
mod eq_files;
mod graphics_recovery;
mod main_loop;
mod meter;
mod outputs;
mod performance;
mod playback;
mod playlists;
mod runtime;
mod scrobbling;
mod skins;
mod sleep_timer;
mod views;
mod volume;
use eq_files::*;
use main_loop::*;
use outputs::*;
use reborn_control::{Command, PlaybackAction, Response};
use reborn_core::platform::{Fact, LowBattery};
use reborn_core::{
    AppModel, AudioOutput, Effect, Event, PlaybackState, QueueEntryId, RepeatMode, Screen,
    ShuffleMode, Source, Track,
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
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use views::*;
/// A pause held in place gives its sink back after this long (Bluetooth
/// headphones and the wired codec may then rest).
const HOLD_LIMIT: Duration = Duration::from_secs(180);
/// The main loop's periodic block (battery and media observation, the
/// automatic sleep, the diagnostic snapshot) runs after more than this.
const PERIODIC_EVERY: Duration = Duration::from_secs(2);
/// Unsaved state is written after more than this.
const CHECKPOINT_EVERY: Duration = Duration::from_secs(15);
/// Seek steps closer together than this are one seek.
const SEEK_SETTLE: Duration = Duration::from_millis(350);
/// Held seeks tick at most this often (#28).
const SEEK_TICK_GAP: Duration = Duration::from_millis(300);
/// Previous restarts the song after this far into it (Y2Player, iPod).
const RESTART_SONG_AFTER_MS: u64 = 3_000;
/// Headphone volume reports this soon after the player set it are its echo
/// (headphones round to their own steps), not a change on the headphones.
const BT_VOLUME_ECHO: Duration = Duration::from_secs(1);
/// How long after a Bluetooth loss (or the wake after one) Play waits for
/// those headphones instead of the jack or the speaker (F51).
const LOST_PEER_WINDOW: Duration = Duration::from_secs(120);
/// How long one Play waits for them to come back.
const LOST_PEER_PLAY_WAIT: Duration = Duration::from_secs(20);

/// Bluetooth headphones that report their own volume (AVRCP absolute
/// volume) follow the player's volume, and Reborn's software level stays
/// at full for them; others keep the software level (Y2Reborn #22).
#[derive(Default)]
struct BtVolume {
    /// The headphones (device path) whose level `settings.volume` is.
    device: Option<String>,
    /// Their last reported level, 0..=127.
    reported: Option<u8>,
    /// When the player last set it.
    sent: Option<Instant>,
    /// A level the full command queue refused; sent again next tick.
    unsent: bool,
}
fn avrcp_to_percent(volume: u8) -> u8 {
    ((u32::from(volume.min(127)) * 100 + 63) / 127) as u8
}
fn percent_to_avrcp(volume: u8) -> u8 {
    ((u32::from(volume.min(100)) * 127 + 50) / 100) as u8
}
/// The headphones' level when they take the volume over: their level times
/// the software level they played at, so nothing gets louder.
fn takeover_level(reported: u8, software: u8) -> u8 {
    ((u16::from(reported.min(127)) * u16::from(software.min(100)) + 50) / 100) as u8
}

#[cfg(test)]
#[path = "main_tests/bt_volume_tests.rs"]
mod bt_volume_tests;

/// The step of a held Previous/Next seek (one every 90 ms): small at first,
/// larger the longer the button is held, at most 1/60 of the song (a fixed
/// 30 s per step crossed a normal song in about a second, user feedback).
fn seek_step(held: Duration, length_ms: u64) -> u64 {
    let step = if held < Duration::from_millis(1500) {
        1_000
    } else if held < Duration::from_secs(4) {
        3_000
    } else {
        10_000
    };
    step.min((length_ms / 60).max(1_000))
}

/// A new seed for every shuffle, from the kernel's random pool (#4: a seed
/// made from the track ID gave the same order every time).
fn shuffle_seed() -> u64 {
    reborn_platform::random_u64()
}

use reborn_core::queue::shuffle_from;
/// Songs the decoder knows ahead of the one playing (`Playback::set_upcoming`
/// keeps it filled as songs play and the queue changes).
const UPCOMING: usize = 32;

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
}

/// The one-step update's screen is up, but no update task runs or waits.
fn update_screen_waits(
    update_run: bool,
    restarting: bool,
    busy: Option<reborn_core::PlatformTask>,
    queued: Option<reborn_core::PlatformTask>,
) -> bool {
    use reborn_core::PlatformTask::{UpdateApply, UpdateStage};
    update_run
        && !restarting
        && !matches!(busy, Some(UpdateStage | UpdateApply))
        && !matches!(queued, Some(UpdateStage | UpdateApply))
}

struct Runtime {
    /// A task the owner started while the platform worker was busy (a status
    /// observation takes seconds). Dropping it left the one-step update at
    /// step 1 with no download running until a restart; it now starts as
    /// soon as the worker is free.
    queued_platform: Option<reborn_core::PlatformTask>,
    /// Since when the update screen has shown no running update task.
    update_idle_since: Option<Instant>,
    /// Look the time zone up again (Automatic was just turned on).
    zone_lookup_again: bool,
    /// Reads chapter marks of audiobook files in the background.
    chapter_reader: Option<books::ChapterReader>,
    /// Book files whose chapter marks were asked for (each once).
    chapters_asked: HashSet<PathBuf>,
    /// The audiobook shelf as last written to `state/books.json`.
    shelf_written: reborn_core::books::Shelf,
    /// When playback was last paused (a book rewinds after a long pause).
    paused_at: Option<Instant>,
    /// The sleep timer has lowered the playback volume.
    sleep_fading: bool,
    /// The sleep timer's minutes left as last drawn (they change on screen).
    sleep_minutes_shown: Option<u64>,
    /// Headphones in the jack, as the kernel last reported (#28 speaker);
    /// None until it has read the detector.
    headphones: Option<bool>,
    /// A jack change still to act on: it waits while an output start is
    /// pending (a reconfiguration takes one change at a time).
    jack_pending: Option<bool>,
    /// The Bluetooth headphones just lost; they usually return (F51).
    lost_peer: Option<LostPeer>,
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
    /// "Internal Storage is ready" was said this start.
    announced_setup: bool,
    /// The error report question was shown this start.
    asked_reports: bool,
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
    bt_volume: BtVolume,
    graphics: Option<Renderer>,
    graphics_recovery: graphics_recovery::Recovery,
    /// The last screen wake, until its first frame was presented or retried.
    woke_at: Option<Instant>,
    power: Value,
    kernel_events: Arc<Mutex<Vec<String>>>,
    background: background::Gate,
    art: bool,
    /// The song last seen playing (entry, library id) and its record: a
    /// change puts it in the history.
    history_seen: Option<(Option<QueueEntryId>, i64)>,
    history_track: Option<Track>,
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
    /// Scrobbling: the play being followed, plays counted before the clock
    /// was known, the queue for Last.fm and when the clock was last checked.
    scrobbles: reborn_core::scrobble::Tracker,
    scrobbles_unstamped: Vec<reborn_core::scrobble::Counted>,
    scrobble_queue: reborn_platform::scrobble::Queue,
    clock_trusted: Option<(Instant, bool)>,
    /// Last.fm: the app's key, the listener's session, sign-in and sending.
    lastfm: scrobbling::LastFm,
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
    /// The gain-relevant sound settings of the last load: a load that plays
    /// louder than it (`dsp_rise_fades`) fades in.
    loaded_dsp: Option<LoadedDsp>,
    /// Playback restarts once at the model's song and place at this moment
    /// (and only when no start is in progress): seeking with held
    /// Previous/Next, skipping while a song is still starting.
    seek_until: Option<Instant>,
    /// First and latest step of the current held seek (`seek_step`).
    seek_run: Option<(Instant, Instant)>,
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
    /// Sleeps refused in a row after preparation; spaces automatic tries.
    sleep_refusals: u32,
    /// The platform refused the last sleep because a charger or computer is
    /// connected: automatic sleep waits until the battery shows no cable.
    sleep_refused_for_charger: bool,
    /// Power asked to sleep (the screen went dark at once); a wake before the
    /// platform prepares means the person changed their mind (F48).
    sleep_requested_at: Option<Instant>,
    /// When the current pause with Bluetooth headphones connected began.
    headset_paused: Option<Instant>,
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
            | Command::SleepRestore { .. }
    )
}

/// Ask the watchdog for one observation, when the gate allows one; true
/// when it was sent (and is now pending).
fn request_observation(
    gate: &background::Gate,
    requests: &SyncSender<background::Request>,
    pending: &mut Option<background::Request>,
) -> bool {
    let Some(request) = gate.request() else {
        return false;
    };
    let sent = requests.try_send(request.clone()).is_ok();
    if sent {
        *pending = Some(request);
    }
    sent
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

/// Release what plays, plan the next sink and start it. A wired sink stays
/// open for the next stream when `keep` takes its spec as the plan (the
/// plan's probe cannot run while it is open: the device is exclusive);
/// otherwise it closes before the plan.
fn run_sink_reconfiguration<P>(
    playback: &mut playback::Playback,
    generation: u64,
    keep: impl FnOnce(&reborn_audio::SinkSpec) -> Option<P>,
    plan: impl FnOnce() -> Result<P, String>,
    start: impl FnOnce(&mut playback::Playback, P) -> Result<(), String>,
) -> Result<(), String> {
    if let Some(kept) = playback.switch_and_wait(generation)? {
        if let Some(plan) = keep(&kept) {
            return start(playback, plan);
        }
        playback.stop_and_wait(generation)?;
    }
    let plan = plan()?;
    start(playback, plan)
}

/// The kept wired sink plays the next stream when that stream's plan would
/// give its spec: the wired plan converts every source to 44.1 kHz (#52), so
/// any track plays on in the open headphone sink.
fn keeps_wired_sink(kept: &reborn_audio::SinkSpec, output: &AudioOutput) -> bool {
    matches!(output, AudioOutput::Wired)
        && kept.output == *output
        && kept.rate == reborn_audio::WIRED_RATE
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

/// Keep the headphone DAC powered while the screen is on, as the stock
/// firmware does (`power::dac_hold`); playback keeps it up anyway. What the
/// kernel took: Some(false) without the hold (older Y2Linux), None failed.
fn hold_dac(log: &Observer, on: bool) -> Option<bool> {
    match power::dac_hold(on) {
        Ok(held) => Some(held),
        Err(error) => {
            log.emit(
                Level::Warn,
                "audio",
                "dac_hold_failed",
                &error,
                None,
                json!({"hold":on}),
            );
            None
        }
    }
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
            | Screen::ErrorReports
    )
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

/// A song on a card the platform refused: reinserting a damaged card does not
/// help (F12: the user swapped cards eight times on that advice).
fn refused_card_error(error: &str, sd: reborn_core::platform::SdCard) -> Option<&'static str> {
    use reborn_core::platform::SdCard;
    let lower = error.to_ascii_lowercase();
    if !(lower.contains("source offline") || lower.contains("source unavailable")) {
        return None;
    }
    match sd {
        SdCard::NeedsRepair => Some("The SD card needs repair. Check it on a computer."),
        SdCard::CheckTimedOut => {
            Some("The SD card took too long to check. Remove it and insert it again.")
        }
        _ => None,
    }
}

fn friendly_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("a2dp peer required") {
        return "Bluetooth headphones aren't connected. Connect them or choose the headphone jack."
            .into();
    }
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

/// What can be kept of a session `AppModel::restore` refused (for example one
/// an update newer than this Reborn wrote, read again after a rollback): every
/// saved setting this version understands, field by field; the rest keeps its
/// default. The queue is not recovered.
fn recover_settings(path: &Path) -> Option<reborn_core::Settings> {
    use reborn_core::{Settings, BRIGHTNESS_MIN, SESSION_MAX_BYTES};
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(SESSION_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let saved = value
        .get("model")
        .unwrap_or(&value)
        .get("settings")?
        .as_object()?;
    let mut merged = serde_json::to_value(Settings::default()).ok()?;
    // A session without the field saved the old volume scale.
    merged["volume_scale"] = 0.into();
    for (key, field) in saved {
        let mut candidate = merged.clone();
        candidate[key.as_str()] = field.clone();
        if serde_json::from_value::<Settings>(candidate.clone()).is_ok() {
            merged = candidate;
        }
    }
    let mut settings: Settings = serde_json::from_value(merged).ok()?;
    // Saved while headphones held the volume: the wired one comes back
    // until they connect again; an old-scale volume moves once.
    settings.prepare_wired_start();
    settings.balance = settings
        .balance
        .clamp(-reborn_core::MAX_BALANCE, reborn_core::MAX_BALANCE);
    settings.crossfade_ms = settings.crossfade_ms.min(30_000);
    settings.eq_bands.truncate(8);
    settings.sanitize_eq_profile();
    settings.brightness = settings.brightness.clamp(BRIGHTNESS_MIN, 100);
    Some(settings)
}

/// The verdict Y2Linux's update health check reads (`healthy` in `rebornctl
/// status`): Reborn is drawing, its database works and no worker is stuck.
/// A library scan that could not read some files is not a reason: one broken
/// song must never roll an update back.
fn update_health(
    headless: bool,
    graphics: bool,
    uptime: u64,
    health: &Value,
) -> (bool, Vec<String>) {
    let mut reasons = Vec::new();
    if !headless && !graphics {
        reasons.push("graphics unavailable".to_owned());
    }
    if uptime < 10 {
        reasons.push("starting".to_owned());
    }
    if let Some(subsystems) = health["subsystems"].as_object() {
        for (name, entry) in subsystems {
            let failed = entry["state"] == "failed" && entry["required"] == true;
            if failed && (name == "database" || name.starts_with("worker_")) {
                reasons.push(format!("{name} failed"));
            }
        }
    }
    (reasons.is_empty(), reasons)
}

/// What a jack change does to playing music (#28): headphones in move a
/// playing speaker to them; headphones out pause what played in them.
#[derive(Debug, PartialEq, Eq)]
enum JackMove {
    ToHeadphones,
    Pause,
    Nothing,
}

fn jack_move(
    wired: bool,
    active: bool,
    plugged: bool,
    before: Option<bool>,
    speaker_active: bool,
) -> JackMove {
    if wired && active && plugged && speaker_active {
        JackMove::ToHeadphones
    } else if wired && active && !plugged && before == Some(true) && !speaker_active {
        JackMove::Pause
    } else {
        JackMove::Nothing
    }
}

/// The message when unplugged headphones paused the music (#62).
const HEADPHONES_LOST: &str = "Headphones unplugged. Playback paused.";

/// #28: one haptic tick on a lit screen (not the press that wakes it) per
/// button press and per seek step of a held Previous/Next (`seeked`: this
/// event's actions moved the song); none for wheel steps, also not the
/// Now Playing wheel's seek.
fn input_ticks(
    lit: bool,
    input: reborn_core::NormalizedInput,
    actions: &[reborn_core::Action],
    seeked: bool,
) -> bool {
    use reborn_core::{Action, NormalizedInput};
    lit && (matches!(input, NormalizedInput::Press(_))
        || (seeked
            && actions
                .iter()
                .any(|a| matches!(a, Action::SeekForward | Action::SeekBackward))))
}

/// #67: a click per wheel step on a lit screen, while the music plays or not
/// as Customize > Wheel Click says.
fn wheel_clicks(
    lit: bool,
    input: reborn_core::NormalizedInput,
    when: reborn_core::WheelClick,
    playing: bool,
) -> bool {
    use reborn_core::NormalizedInput;
    lit && matches!(
        input,
        NormalizedInput::WheelClockwise(_) | NormalizedInput::WheelCounterClockwise(_)
    ) && when.clicks(playing)
}

/// One haptic tick at the strength set; a motor that fails is dropped.
fn haptic_tick(
    motor: &mut Option<reborn_platform::haptics::Motor>,
    strength: reborn_core::Haptics,
    log: &Observer,
) {
    if let Some(m) = motor {
        if let Err(e) = m.tick(strength, Instant::now()) {
            log.emit(
                Level::Warn,
                "input",
                "haptics_failed",
                &e.to_string(),
                None,
                json!({}),
            );
            *motor = None;
        }
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
#[path = "main_tests/jack_and_haptics_tests.rs"]
mod jack_and_haptics_tests;

#[cfg(test)]
#[path = "main_tests/dsp_rise_tests.rs"]
mod dsp_rise_tests;
#[cfg(test)]
#[path = "main_tests/session_recovery_tests.rs"]
mod session_recovery_tests;

#[cfg(test)]
#[path = "main_tests/eq_profile_file_tests.rs"]
mod eq_profile_file_tests;

#[cfg(test)]
#[path = "main_tests/artwork_presentation_tests.rs"]
mod artwork_presentation_tests;
#[cfg(test)]
#[path = "main_tests/update_health_tests.rs"]
mod update_health_tests;

#[cfg(test)]
#[path = "main_tests/bluetooth_epoch_tests.rs"]
mod bluetooth_epoch_tests;

#[cfg(test)]
#[path = "main_tests/runtime_dirty_tests.rs"]
mod runtime_dirty_tests;

#[cfg(test)]
#[path = "main_tests/logo_fetcher_tests.rs"]
mod logo_fetcher_tests;

#[cfg(test)]
#[path = "main_tests/runtime_reconfiguration_tests.rs"]
mod runtime_reconfiguration_tests;

#[cfg(test)]
#[path = "main_tests/ui_readiness_tests.rs"]
mod ui_readiness_tests;

#[cfg(test)]
#[path = "main_tests/loop_interval_tests.rs"]
mod loop_interval_tests;

#[cfg(test)]
#[path = "main_tests/feature_product_tests.rs"]
mod feature_product_tests;

#[cfg(test)]
#[path = "main_tests/auto_output_tests.rs"]
mod auto_output_tests;

#[cfg(test)]
#[path = "main_tests/codec_reconnect_tests.rs"]
mod codec_reconnect_tests;
#[cfg(test)]
#[path = "main_tests/shuffle_and_seek_tests.rs"]
mod shuffle_and_seek_tests;

#[cfg(test)]
#[path = "main_tests/update_screen_tests.rs"]
mod update_screen_tests;
