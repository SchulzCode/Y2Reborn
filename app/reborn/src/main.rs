#![forbid(unsafe_code)]
mod artwork;
mod background;
mod books;
mod diagnostics;
mod graphics_recovery;
mod meter;
mod performance;
mod playback;
mod playlists;
mod scrobbling;
mod skins;
mod sleep_timer;
mod spotify;
mod volume;
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
/// A pause held in place gives its sink back after this long (Bluetooth
/// headphones and the wired codec may then rest).
const HOLD_LIMIT: Duration = Duration::from_secs(180);
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
mod bt_volume_tests {
    use super::{avrcp_to_percent, percent_to_avrcp, takeover_level};

    #[test]
    fn levels_round_trip_and_a_takeover_never_gets_louder() {
        for percent in 0..=100 {
            assert_eq!(avrcp_to_percent(percent_to_avrcp(percent)), percent);
        }
        assert_eq!((percent_to_avrcp(0), percent_to_avrcp(100)), (0, 127));
        // Software at 100 % (the #22 report): the headphones' own level stays.
        assert_eq!(takeover_level(40, 100), 40);
        // Software at 30 % with the headphones at full: 30 % on the headphones.
        assert_eq!(takeover_level(127, 30), 38);
        assert_eq!(takeover_level(0, 100), 0);
        assert!((0..=127).all(|r| takeover_level(r, 55) <= r));
    }
}

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
    /// Spotify: sign-in, spotifyd and its lists.
    spotify: spotify::Spotify,
    /// The cover of the Spotify song playing: (logo key, address).
    spotify_cover: Option<(u64, String)>,
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
            // The library, book progress and chapter marks are not rolled
            // back (and not copied for every action).
            let tracks = std::mem::take(&mut self.model.library.tracks);
            let shelf = std::mem::take(&mut self.model.library.shelf);
            let chapters = std::mem::take(&mut self.model.library.chapters);
            let platform = std::mem::take(&mut self.model.platform);
            self.action_before = Some(self.model.clone());
            self.model.library.tracks = tracks;
            self.model.library.shelf = shelf;
            self.model.library.chapters = chapters;
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
        previous.library.shelf = std::mem::take(&mut self.model.library.shelf);
        previous.library.chapters = std::mem::take(&mut self.model.library.chapters);
        previous.platform = std::mem::take(&mut self.model.platform);
        self.keep_held_volume(&mut previous);
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
        if let Some(mut pending) = self.pending_reconfiguration.take() {
            self.keep_held_volume(&mut pending.previous);
            self.model = pending.previous;
            self.dirty = pending.dirty_before;
        }
    }
    /// A rolled-back model keeps the level headphones hold: it is theirs,
    /// set on them already, not part of the change that failed.
    fn keep_held_volume(&self, previous: &mut AppModel) {
        if self.bt_volume.device.is_some() {
            let now = &self.model.settings;
            previous.settings.volume = now.volume;
            previous.settings.wired_volume = now.wired_volume;
            previous.settings.bluetooth_absolute_volume = now.bluetooth_absolute_volume;
        }
    }

    /// Music plays on the headphone jack (where the safe level applies).
    fn wired_sink(&self) -> bool {
        matches!(self.model.output, AudioOutput::Wired) && self.bt_volume.device.is_none()
    }
    /// Back on the jack: a volume above the safe level starts at it, since
    /// other headphones may be plugged in now.
    fn cap_wired_volume(&mut self) {
        let safe = self.model.settings.hp_level.safe_volume();
        if self.wired_sink() && self.model.settings.volume > safe {
            self.model.settings.volume = safe;
            self.playback.set_volume(self.sink_volume());
            self.dirty.mark_both();
            self.dirty.mark_persistence();
        }
    }
    /// Counts wired listening above the safe level (every few seconds);
    /// after `LOUD_LISTENING_LIMIT_S` the agreement lapses and the volume
    /// returns to the safe level.
    fn loud_listening_tick(&mut self, elapsed: Duration) {
        let s = &self.model.settings;
        let loud = s.loud_ok
            && self.wired_sink()
            && self.model.playback == PlaybackState::Playing
            && s.volume > s.hp_level.safe_volume();
        if !loud {
            return;
        }
        // Instant does not advance in suspend; a stalled loop counts once.
        let add = elapsed.as_secs().min(10) as u32;
        let s = &mut self.model.settings;
        s.loud_seconds = s.loud_seconds.saturating_add(add);
        if s.loud_seconds < reborn_core::LOUD_LISTENING_LIMIT_S {
            if s.loud_seconds % 60 < add {
                self.dirty.mark_persistence();
            }
            return;
        }
        s.loud_ok = false;
        s.loud_seconds = 0;
        self.log.emit(
            Level::Info,
            "audio",
            "loud_volume_lapsed",
            "20 hours above the safe level: volume back to it",
            None,
            json!({"hp_level":self.model.settings.hp_level,"volume":self.model.settings.volume}),
        );
        self.cap_wired_volume();
        self.ui
            .flash("Volume lowered after 20 hours of loud listening");
        self.checkpoint();
    }
    /// The software level: full for headphones that hold the volume.
    fn sink_volume(&self) -> u8 {
        if self.bt_volume.device.is_some() {
            100
        } else {
            self.model.settings.volume
        }
    }
    /// The playing Bluetooth headphones and their level, when they report one.
    fn absolute_volume_peer(&self) -> Option<(String, u8)> {
        let AudioOutput::Bluetooth(address) = &self.model.output else {
            return None;
        };
        self.bt_state
            .devices
            .iter()
            .find(|d| d.address.eq_ignore_ascii_case(address) && d.connected)
            .and_then(|d| Some((d.path.clone(), d.volume?)))
    }
    fn send_bt_volume(&mut self) {
        let (Some(device), Some(service)) = (self.bt_volume.device.clone(), &self.bluetooth) else {
            return;
        };
        let volume = percent_to_avrcp(self.model.settings.volume);
        self.bt_volume.unsent = service
            .commands
            .try_send(bluetooth::Command::Volume { device, volume })
            .is_err();
        self.bt_volume.sent = Some(Instant::now());
    }
    /// Follows headphones that hold their own volume: taking it over when
    /// they become the output, adopting changes made on them, and giving
    /// the wired volume back when they stop being the output.
    fn sync_bt_volume(&mut self) {
        let peer = self.absolute_volume_peer();
        if self.bt_volume.device.is_some()
            && peer.as_ref().map(|(device, _)| device) != self.bt_volume.device.as_ref()
        {
            if let Some(wired) = self.model.settings.wired_volume.take() {
                self.model.settings.volume = wired;
            }
            self.bt_volume = BtVolume::default();
            self.cap_wired_volume();
            self.playback.set_volume(self.model.settings.volume);
            self.dirty.mark_both();
            self.dirty.mark_persistence();
        }
        let Some((device, reported)) = peer else {
            return;
        };
        if self.bt_volume.device.is_none() {
            let wired = self.model.settings.volume;
            // The loudness so far stays: the software level (before any
            // headphones held the volume) moves into the headphones' level.
            let software = if self.model.settings.bluetooth_absolute_volume {
                100
            } else {
                wired
            };
            let level = takeover_level(reported, software);
            self.model.settings.wired_volume = Some(wired);
            self.model.settings.volume = avrcp_to_percent(level);
            self.model.settings.bluetooth_absolute_volume = true;
            self.bt_volume = BtVolume {
                device: Some(device),
                reported: Some(reported),
                sent: None,
                unsent: level != reported,
            };
            self.playback.set_volume(100);
            self.log.emit(
                Level::Info,
                "audio",
                "bluetooth_volume",
                "headphones hold the volume (absolute volume)",
                None,
                json!({"reported":reported,"volume":self.model.settings.volume,"wired":wired}),
            );
            self.dirty.mark_both();
            self.dirty.mark_persistence();
        } else if self.bt_volume.reported != Some(reported) {
            self.bt_volume.reported = Some(reported);
            let echo = self
                .bt_volume
                .sent
                .is_some_and(|sent| sent.elapsed() < BT_VOLUME_ECHO);
            if !echo && !self.bt_volume.unsent {
                // Changed on the headphones.
                self.model.settings.volume = avrcp_to_percent(reported);
                self.ui.show_volume();
                self.dirty.mark_both();
                self.dirty.mark_persistence();
            }
        }
        if self.bt_volume.unsent {
            self.send_bt_volume();
        }
    }
    fn play_index(&mut self, index: usize, shuffle: ShuffleMode) -> Result<(), String> {
        // The music library around the song; audiobook parts stay out of
        // it (Y2Linux #58), they play through Effect::PlayBook.
        let tracks = &self.model.library.tracks;
        let members = (0..tracks.len())
            .filter(|&i| i == index || tracks[i].is_music())
            .collect();
        self.play_members(index, members, shuffle, "Songs".into(), None)
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
            _ => return,
        };
        let _ = sequence;
        self.ui.radio.loading = false;
        self.ui.radio.error = error;
        self.dirty.mark_render();
    }
    fn play_radio(&mut self, station: reborn_core::RadioStation) -> Result<(), String> {
        if self.ui.wifi.status == WifiStatus::Disconnected {
            return Err("Connect to Wi-Fi to listen to radio".into());
        }
        self.save_book();
        self.model.replace_queue(vec![Track::radio(&station)], 0)?;
        self.model.set_source("Radio", None);
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
        self.log.emit(Level::Info, "radio", "station_started", "Internet radio station started", None,
            json!({"codec":station.codec,"bitrate":station.bitrate,"directory":!station.uuid.is_empty()}));
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
            if let Some((StreamState::Failed(reason), _)) = reborn_platform::radio::live() {
                // Only the Now Playing line showed why a station failed (F19).
                let (status, content_type) = reborn_platform::radio::last_reply().unzip();
                self.log.emit(
                    Level::Warn,
                    "radio",
                    "station_failed",
                    &reason,
                    None,
                    json!({"http_status":status,"content_type":content_type,
                        "directory":self.radio_current.as_ref().map(|s| !s.uuid.is_empty())}),
                );
            }
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
                    .filter(|t| !t.is_live())
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
    /// Internal Storage and online SD cards, where Themes, Radio and EQ live.
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
        shuffle: ShuffleMode,
        source: String,
        origin: Option<reborn_core::QueueOrigin>,
    ) -> Result<(), String> {
        let online = self
            .model
            .library
            .tracks
            .get(index)
            .map(|track| track.online)
            .ok_or("invalid selection")?;
        if !online {
            return Err("media source offline".into());
        }
        // Positions first, songs last: a 28 600-song collection is cut to
        // MAX_QUEUE before any song is copied.
        let tracks = &self.model.library.tracks;
        let mut members = members
            .into_iter()
            .filter(|&member| tracks.get(member).is_some_and(|track| track.online))
            .collect::<Vec<_>>();
        let selected_position = members
            .iter()
            .position(|&member| member == index)
            .ok_or("selected track is not in the playback collection")?;
        // A Shuffle row says how; a song or Play follows the setting.
        let force_shuffle = shuffle != ShuffleMode::Off;
        let mode = if force_shuffle {
            shuffle
        } else {
            self.model.settings.shuffle_mode()
        };
        // The source's own order, for Shuffle off later (by library index:
        // a whole card is 120 000 songs).
        let mut rank = vec![usize::MAX; tracks.len()];
        for (at, &member) in members.iter().enumerate() {
            rank[member] = at;
        }
        let (window, start) = if mode == ShuffleMode::Albums {
            // Whole albums in random order; a picked song's album first,
            // from that song on (Beta 8.39).
            let picked = {
                let refs: Vec<&Track> = members.iter().map(|&m| &tracks[m]).collect();
                reborn_core::queue::album_shuffle(
                    &refs,
                    (!force_shuffle).then_some(selected_position),
                    shuffle_seed(),
                )
            };
            members = picked.into_iter().map(|p| members[p]).collect();
            (0..members.len().min(reborn_core::MAX_QUEUE), 0)
        } else if mode == ShuffleMode::Songs {
            if force_shuffle {
                // A Shuffle row: any song may come first.
                shuffle_from(&mut members, 0, shuffle_seed());
            } else {
                // A song picked with Shuffle on plays first, the rest follow.
                let selected = members.remove(selected_position);
                members.insert(0, selected);
                shuffle_from(&mut members, 1, shuffle_seed());
            }
            (0..members.len().min(reborn_core::MAX_QUEUE), 0)
        } else {
            let window =
                reborn_core::queue_window(members.len(), selected_position, reborn_core::MAX_QUEUE);
            let start = selected_position - window.start;
            (window, start)
        };
        let chosen = &members[window];
        let queue = chosen
            .iter()
            .filter_map(|&member| tracks.get(member).cloned())
            .collect::<Vec<_>>();
        let mut natural: Vec<usize> = (0..chosen.len()).collect();
        natural.sort_by_key(|&i| rank.get(chosen[i]).copied().unwrap_or(usize::MAX));
        self.save_book();
        self.model.replace_queue(queue, start)?;
        self.model.set_source(source, Some(&natural));
        self.model.queue_origin = origin;
        self.load()
    }

    /// A new song plays: the one before goes into the history.
    fn note_history(&mut self) {
        let now = self
            .model
            .current()
            .map(|t| (self.model.current_entry_id(), t.id));
        if now == self.history_seen {
            return;
        }
        if let Some(previous) = self.history_track.take() {
            self.model.note_played(&previous);
            self.dirty.mark_persistence();
        }
        self.history_seen = now;
        self.history_track = self.model.current().cloned();
    }

    /// The queue changed after the song playing: the decoder learns the
    /// songs to come without the song starting over.
    fn upcoming_changed(&mut self) {
        let (tracks, ids) = self.model.upcoming(UPCOMING);
        self.playback
            .set_upcoming(self.model.generation, tracks, ids);
    }

    /// Play Next (`next`) or Add to Queue. An empty queue becomes the songs;
    /// Play Next then starts them.
    fn queue_songs(&mut self, members: Vec<usize>, next: bool) -> Result<(), String> {
        let tracks = members
            .into_iter()
            .filter_map(|index| self.model.library.tracks.get(index).cloned())
            .filter(|track| track.online)
            .collect::<Vec<_>>();
        let count = tracks.len();
        if count == 0 {
            return Err("media source offline".into());
        }
        let fresh = if next {
            self.model.queue_next(tracks)?
        } else {
            self.model.queue_last(tracks)?
        };
        if fresh && next {
            self.load()?;
        } else if fresh {
            self.model.playback = PlaybackState::Paused;
        } else {
            self.upcoming_changed();
        }
        self.ui.flash(match (next, count) {
            (true, 1) => "Plays next".to_owned(),
            (true, n) => format!("{n} songs play next"),
            (false, 1) => "Added to queue".to_owned(),
            (false, n) => format!("{n} songs added to queue"),
        });
        self.checkpoint();
        Ok(())
    }

    fn fail(&mut self, sub: &str, error: String) {
        let peer_gone = selected_peer_gone(&self.model.output, &self.bt_state, &error);
        self.ui.flash(
            refused_card_error(&error, self.model.platform.snapshot.storage.sd)
                .map_or_else(|| friendly_error(&error), str::to_owned),
        );
        self.log.emit(
            Level::Error,
            sub,
            "operation_failed",
            &error,
            None,
            json!({"state":self.model.playback,"recovery_attempted":peer_gone}),
        );
        if peer_gone {
            // The disconnect itself was never seen: recover as if it had been.
            self.bluetooth_peer_lost("paused_after_failed_start");
        }
        self.dirty.mark_render();
    }
    /// The selected Bluetooth peer is gone: back to the jack, paused.
    fn bluetooth_peer_lost(&mut self, recovery: &str) {
        self.abandon_pending_reconfiguration();
        if let AudioOutput::Bluetooth(address) = self.model.output.clone() {
            self.lost_peer = Some(LostPeer::new(address.clone(), Instant::now()));
            self.model.apply(Event::BluetoothDisconnected(address));
        }
        self.pause();
        self.ui.flash("Bluetooth disconnected. Playback paused.");
        self.log.emit(
            Level::Warn,
            "bluetooth",
            "transport_lost",
            "Selected audio peer disconnected",
            None,
            json!({"recovery":recovery}),
        );
        self.dirty.mark_render();
    }
    fn prepare_sleep(&mut self) -> Result<Value, String> {
        if self.sleep_prepared {
            return Err("sleep_operation_in_progress".into());
        }
        // Power turned the screen off with the request; lit again now means a
        // second press woke it. Stay awake instead of blanking it once more.
        let requested = self
            .sleep_requested_at
            .take()
            .is_some_and(|at| at.elapsed() < Duration::from_secs(30));
        if requested && !self.model.screen_off {
            return Err("wake_event_in_progress".into());
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

    fn restore_sleep(&mut self, dark: bool) -> Result<Value, String> {
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
        if dark {
            // F47: the sleep was refused with nobody waking the player; the
            // screen stays off (it lit every two minutes for hours on a player
            // whose kernel refused every sleep) and the next try waits longer.
            self.sleep_refusals = self.sleep_refusals.saturating_add(1);
            self.sleep_attempt = Some(Instant::now());
            self.log.emit(
                Level::Info,
                "power",
                "sleep_refused_dark",
                "Sleep refused after preparation; the screen stays off",
                None,
                json!({"refusals":self.sleep_refusals,
                    "retry_s":reborn_core::platform::sleep_retry_seconds(self.sleep_refusals)}),
            );
            return Ok(json!({"sleep_ready":true,"screen_off":self.model.screen_off}));
        }
        self.sleep_refusals = 0;
        self.effect_inner(Effect::ScreenWake)?;
        if let Some(graphics) = self.graphics.as_mut() {
            let presented = graphics.render(&self.ui.draw(
                &self.model,
                &self.model.library.tracks,
                self.art,
            ));
            if let Err(error) = presented {
                // Not retried here: a screen off/on can block for seconds
                // (flip_done timeouts) and the platform's sleep_restore call
                // gives up after 5 s. The main loop's next frame tries it.
                self.dirty.mark_render();
                return Err(error);
            }
            self.graphics_recovery.presented();
        } else if !self.headless {
            return Err("display_restoration_failed".into());
        }
        self.playback_hints.clear();
        self.last_activity = Instant::now();
        self.dirty.mark_render();
        Ok(json!({"sleep_ready":true,"screen_off":self.model.screen_off}))
    }

    /// The first frames after a wake can fail when the display link came back
    /// lagging (report 51f3b06d: vblank timeouts, presentation -16, a dark
    /// screen); the next screen off/on cleared it there. That cycle is tried
    /// once per wake before the frame counts as failed.
    fn retry_after_wake(&mut self, error: &str) -> bool {
        let Some(woke) = self.woke_at.take() else {
            return false;
        };
        if woke.elapsed() > Duration::from_secs(30) {
            return false;
        }
        let draw = self
            .ui
            .draw(&self.model, &self.model.library.tracks, self.art);
        let Some(graphics) = self.graphics.as_mut() else {
            return false;
        };
        let result = graphics
            .sleep(true)
            .and_then(|()| graphics.sleep(false))
            .and_then(|()| graphics.render(&draw));
        self.log.emit(
            if result.is_ok() {
                Level::Info
            } else {
                Level::Warn
            },
            "graphics",
            "wake_presentation_retry",
            "Screen off/on after a failed frame following a wake",
            None,
            json!({"error":error,"recovery_attempted":true,"recovered":result.is_ok(),
                "retry_error":result.as_ref().err()}),
        );
        if result.is_ok() {
            self.graphics_recovery.presented();
            self.dirty.mark_render();
        }
        result.is_ok()
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

    /// Send the wired level and DAC filter to the audio layer (kept for every
    /// wired stream; applied now where the amplifier is down).
    fn configure_wired(&self) {
        if self.headless {
            return;
        }
        let s = &self.model.settings;
        match reborn_audio::wired_configure(s.hp_level, s.dac_filter) {
            Ok(levels) => self.log.emit(
                Level::Info,
                "audio",
                "wired_configured",
                "wired headphone level and DAC filter",
                None,
                json!({"hp_level":s.hp_level,"dac_filter":s.dac_filter,"level_control":levels}),
            ),
            Err(e) => self.log.emit(
                Level::Warn,
                "audio",
                "wired_configure_failed",
                &e,
                None,
                json!({"hp_level":s.hp_level,"dac_filter":s.dac_filter}),
            ),
        }
    }

    /// Error reports (ERROR-REPORTS-PLAN): asked once, after the update that
    /// brought them; "No Thanks" is an answer too. Only on a lit screen, with
    /// nothing else open and no button pressed for a while, so a scroll or a
    /// press already under way can't answer it.
    fn ask_about_reports(&mut self) {
        if self.asked_reports
            || self.model.screen_off
            || self.model.navigation.modal.is_some()
            || self.last_activity.elapsed() < Duration::from_secs(5)
            || !self
                .model
                .platform
                .snapshot
                .reports
                .as_ref()
                .is_some_and(|r| r.upload && r.enabled.is_none())
            || matches!(
                self.model.platform.snapshot.internal_setup,
                reborn_core::platform::InternalSetup::Waiting { .. }
            )
        {
            return;
        }
        self.asked_reports = true;
        self.model.navigation.modal = Some(reborn_core::Modal::Confirm(
            reborn_core::ConfirmAction::ReportsQuestion,
        ));
        self.model.navigation.modal_focus = 0;
        self.dirty.mark_both();
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
        for dir in self.storage_roots() {
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
        // Where the playing audiobook is, with every session checkpoint.
        self.save_book();
        self.write_shelf();
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
        if self.wait_for_lost_peer() {
            return Ok(());
        }
        // The output may just have become headphones with their own volume.
        self.sync_bt_volume();
        self.playback.set_balance(self.model.settings.balance);
        // Restore coordinator-owned cores before decoder/device startup.
        self.playback_hints
            .publish(reborn_platform::workload::playback_class(
                true,
                self.model.settings.playback_heavy(),
                false,
            ));
        self.reconfiguration_attempted = true;
        let track = self.model.current().cloned().ok_or("queue is empty")?;
        // Local music and Spotify never play at once.
        self.spotify_yield();
        let radio = track.is_live();
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
        // #28: the jack output plays on the speaker while nothing is in it.
        let speaker = self.speaker_route();
        self.model.speaker_active = speaker;
        let bluetooth_pcm = match &output {
            AudioOutput::Bluetooth(address) => Some(self.bt_state.playback_pcm(address)?),
            AudioOutput::Wired => None,
        };
        let generation = self.model.generation.wrapping_add(1);
        self.transition_generation = Some(generation);
        let entry_id = self
            .model
            .current_entry_id()
            .unwrap_or(QueueEntryId(self.model.queue_position as u64 + 1));
        // A station is never pre-opened for gapless playback: that would start
        // a second network transfer.
        // (`upcoming` leaves stations and Repeat One without a next song.)
        let (queue, queue_entry_ids) = self.model.upcoming(UPCOMING);
        let eq = self.model.settings.active_eq();
        let loaded = (self.model.settings.replay_gain, eq.clone());
        let fade_in = dsp_rise_fades(self.loaded_dsp.as_ref(), &loaded);
        let dsp = reborn_media::DspConfig {
            volume: self.sink_volume(),
            replay_gain: self.model.settings.replay_gain,
            eq_enabled: eq.is_some(),
            preamp_db: eq.as_ref().map_or(0.0, |(preamp, _)| *preamp),
            eq_bands: eq.map(|(_, bands)| bands).unwrap_or_default(),
            // A book's parts follow each other without a crossfade.
            crossfade_ms: if self.model.book_part().is_some() {
                0
            } else {
                self.model.settings.crossfade_ms
            },
            // A book's own speed; music always plays at its own.
            speed: self.model.book_part().map_or(1.0, |(book, _)| book.speed),
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
            fade_in,
        );
        let log = self.log.clone();
        let correlation = self.log.correlation();
        let plan_output = output.clone();
        self.sink_release_started = true;
        let playback = &mut self.playback;
        run_sink_reconfiguration(
            playback,
            generation,
            |kept| {
                (keeps_wired_sink(kept, &output) && kept.speaker == speaker).then(|| kept.clone())
            },
            || {
                match bluetooth_pcm.as_ref() {
                    Some(pcm) => reborn_audio::AlsaSink::plan_bluetooth(
                        &plan_output,
                        pcm,
                        log.clone(),
                        correlation,
                    ),
                    None => reborn_audio::AlsaSink::plan(&plan_output, log.clone(), correlation),
                }
                .map(|(mut spec, _planned)| {
                    spec.speaker = speaker;
                    spec
                })
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
                    fade_in,
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
                    fade_in,
                })
            },
        )?;
        self.loaded_dsp = Some(loaded);
        self.model.generation = generation;
        self.model.playback = PlaybackState::Buffering;
        self.active_transport_generation =
            bluetooth_pcm.as_ref().map(|pcm| pcm.transport_generation);
        self.art = false;
        self.dirty.mark_both();
        Ok(())
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
        // A held stream on the other route (headphones went in or out while
        // paused; the kernel turned a held speaker off) opens anew instead.
        if self.model.playback != PlaybackState::Paused
            || !self.playback.is_held()
            || self.pending_reconfiguration.is_some()
            || self.model.speaker_active != self.speaker_route()
            || self.awaits_lost_peer()
        {
            return false;
        }
        self.playback_hints
            .publish(reborn_platform::workload::playback_class(
                true,
                self.model.settings.playback_heavy(),
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
        self.paused_at = Some(Instant::now());
        self.spotify_pause();
        self.playback_hints.clear();
        // A file paused while it plays holds in place: resume is instant. A
        // station (live), a start in progress or a pending seek release.
        // So does Bluetooth (F43, AirPods Pro 2 and others): a stream drained
        // and prepared again inside BlueALSA stayed silent in the headphones,
        // while a new stream (a skip) always played. Releasing the sink ends
        // the stream as a phone's pause does, and Play opens a new one where
        // the song was (about half a second).
        let bluetooth = matches!(self.model.output, AudioOutput::Bluetooth(_));
        if self.model.playback == PlaybackState::Playing
            && self.pending_reconfiguration.is_none()
            && self.seek_until.is_none()
            && self.model.current().is_some_and(|t| !t.is_radio())
            && !bluetooth
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
        // Spotify hears Play/Pause first (or starts over after a restart).
        if let Some(result) = self.spotify_toggle() {
            return result;
        }
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.pause();
            Ok(())
        } else if let Some(back) = self.book_rewind() {
            // A book continues a little before where it was left.
            self.release_hold();
            self.model.position_ms = self.model.position_ms.saturating_sub(back);
            self.paused_at = None;
            self.load()
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
        // A book plays its parts in order once, whatever Repeat says.
        if let Some(result) = self.finish_book_part() {
            return result;
        }
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
                    // Your queued songs that played leave; the start follows.
                    self.model.wrap();
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
    /// F51 (reports 666edc3c, 46be199c): a sleep turns Bluetooth off and a
    /// link drops now and then; the output then falls back to the jack, and a
    /// Play in the seconds before the headphones returned played there (since
    /// #28 out loud on the speaker). With nothing in the jack, Play waits for
    /// them instead; the output follows them when they are back.
    fn awaits_lost_peer(&self) -> bool {
        self.lost_peer
            .as_ref()
            .is_some_and(|lost| lost.applies(&self.model.output, self.headphones, Instant::now()))
    }
    fn wait_for_lost_peer(&mut self) -> bool {
        if !self.awaits_lost_peer() {
            return false;
        }
        let Some(lost) = self.lost_peer.as_mut() else {
            return false;
        };
        lost.play.get_or_insert_with(Instant::now);
        let address = lost.address.clone();
        let name = self
            .bt_state
            .devices
            .iter()
            .find(|d| d.address.eq_ignore_ascii_case(&address))
            .map(|d| d.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Bluetooth headphones".into());
        self.ui.flash(format!("Waiting for {name}…"));
        self.log.emit(
            Level::Info,
            "audio",
            "play_waits_for_bluetooth",
            "Play waits for the Bluetooth headphones just lost",
            None,
            json!({}),
        );
        self.dirty.mark_both();
        true
    }
    /// A Play that waited too long gives up; afterwards the jack (or the
    /// speaker) plays again like before.
    fn lost_peer_tick(&mut self) {
        let now = Instant::now();
        let Some(lost) = self.lost_peer.as_ref() else {
            return;
        };
        if lost.gave_up(now) {
            self.lost_peer = None;
            self.ui
                .flash("Bluetooth headphones didn't come back. Press Play to use the player.");
            self.log.emit(
                Level::Info,
                "audio",
                "bluetooth_wait_ended",
                "The Bluetooth headphones did not return in time",
                None,
                json!({}),
            );
            self.dirty.mark_render();
        } else if lost.expired(now) {
            self.lost_peer = None;
        }
    }
    /// The jack output plays on the internal speaker: wired, Speaker on and
    /// the kernel has seen the jack empty (unknown counts as headphones; the
    /// kernel also refuses the speaker while it reads a plug).
    fn speaker_route(&self) -> bool {
        matches!(self.model.output, AudioOutput::Wired)
            && self.model.settings.speaker
            && self.headphones == Some(false)
    }
    /// The jack changed (#28). As stock: headphones in move a playing
    /// speaker to them; headphones out pause what played in them.
    fn jack_changed(&mut self, plugged: bool) -> Result<(), String> {
        let before = self.headphones.replace(plugged);
        if before == Some(plugged) {
            return Ok(());
        }
        let active = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        let wired = matches!(self.model.output, AudioOutput::Wired);
        self.log.emit(
            Level::Info,
            "audio",
            "jack",
            if plugged {
                "Headphones plugged in"
            } else {
                "Headphones unplugged"
            },
            None,
            json!({"plugged":plugged,"playing":active,"speaker_active":self.model.speaker_active}),
        );
        match jack_move(wired, active, plugged, before, self.model.speaker_active) {
            JackMove::ToHeadphones => self.load()?,
            JackMove::Pause => {
                self.pause();
                // #62: say why the music stopped, as a lost Bluetooth peer does.
                self.ui.flash(HEADPHONES_LOST);
            }
            JackMove::Nothing if !active && self.model.playback != PlaybackState::Paused => {
                self.model.speaker_active = self.speaker_route();
            }
            JackMove::Nothing => {}
        }
        self.dirty.mark_both();
        Ok(())
    }
    fn switch(&mut self, out: AudioOutput) -> Result<(), String> {
        if let AudioOutput::Bluetooth(address) = &out {
            self.bt_state.playback_rate(address)?;
        }
        // The headphones a Play waited for are back: it plays there now.
        let waited = self
            .lost_peer
            .take()
            .is_some_and(|lost| lost.play.is_some() && lost.is(&out));
        let active = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        if !active {
            self.active_transport_generation = None;
        }
        let to_jack =
            matches!(out, AudioOutput::Wired) && !matches!(self.model.output, AudioOutput::Wired);
        self.model.output = out;
        if to_jack {
            self.cap_wired_volume();
        }
        self.dirty.mark_both();
        if active || waited {
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
        if let Effect::Seek(direction) = e {
            if let Some(length) = self
                .model
                .current()
                .filter(|t| !t.is_live())
                .map(|t| t.duration_ms)
            {
                let now = Instant::now();
                let start = match self.seek_run {
                    Some((start, last)) if now.duration_since(last) < SEEK_SETTLE => start,
                    _ => now,
                };
                self.seek_run = Some((start, now));
                let held = now.duration_since(start);
                let step = if self.model.book_part().is_some() {
                    books::seek_step(held)
                } else {
                    seek_step(held, length)
                } as i64;
                self.seek_by(direction.signum() * step);
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
                    let file = !self.model.current().is_some_and(Track::is_live);
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
        // Turning Bluetooth off, disconnecting or choosing an output by hand
        // decides where audio goes; nothing waits for lost headphones then.
        if matches!(
            e,
            Effect::BluetoothPower | Effect::BluetoothDevice { .. } | Effect::Output(_)
        ) {
            self.lost_peer = None;
        }
        match e {
            Effect::None => {}
            Effect::Platform(task) => {
                if task == reborn_core::PlatformTask::SleepRequest
                    && !matches!(
                        self.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    )
                {
                    // F48: Power turns the screen off at once, as on stock. It
                    // stayed lit ≈1.7 s until the platform prepared, people
                    // pressed again, and that press cancelled the sleep.
                    self.effect_inner(Effect::ScreenSleep)?;
                    self.sleep_requested_at = Some(Instant::now());
                }
                if self.model.platform.busy.is_some() {
                    self.queue_platform(task);
                    return Ok(());
                }
                if task == reborn_core::PlatformTask::InternalStorageSetup
                    && matches!(
                        self.model.platform.snapshot.usb,
                        reborn_core::platform::UsbTransfer::Ready
                            | reborn_core::platform::UsbTransfer::Copying
                            | reborn_core::platform::UsbTransfer::Starting
                    )
                {
                    return Err("Unplug from the computer first".into());
                }
                if task == reborn_core::PlatformTask::SdRepair {
                    // The card is unmounted for the repair: nothing may hold a
                    // song or its folder open on it.
                    let on_card = self.model.current().is_some_and(|t| {
                        self.model.sources.iter().any(|s| {
                            s.id == t.source_id
                                && matches!(s.kind, reborn_core::MediaSource::SdCard(_))
                        })
                    });
                    if on_card {
                        if self.model.playback == PlaybackState::Playing {
                            self.pause();
                        }
                        self.release_hold_with(true);
                        self.playback.stop(self.model.generation);
                    }
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
                if worker.commands.try_send(task).is_err() {
                    // An observation runs and another waits: take the next turn.
                    if task == reborn_core::PlatformTask::Refresh {
                        return Err("busy: another operation is still running".into());
                    }
                    self.queue_platform(task);
                    return Ok(());
                }
                if matches!(
                    task,
                    reborn_core::PlatformTask::UpdateStage | reborn_core::PlatformTask::UpdateApply
                ) {
                    self.log.emit(
                        Level::Info,
                        "update",
                        "task_started",
                        "Update task sent to the platform",
                        None,
                        json!({"task":format!("{task:?}")}),
                    );
                }
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
            Effect::LastFm(request) => self.lastfm_request(request)?,
            Effect::Spotify(request) => self.spotify_request(request)?,
            Effect::SetScrobbleLog(on) => {
                self.model.settings.scrobble_log = on;
                self.checkpoint();
            }
            Effect::SetLastFmScrobble(on) => {
                self.model.settings.lastfm_scrobble = on;
                self.checkpoint();
            }
            Effect::SetLastFmNowPlaying(on) => {
                self.model.settings.lastfm_now_playing = on;
                self.checkpoint();
            }
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
            Effect::Play(index) => self.play_index(index, ShuffleMode::Off)?,
            Effect::PlayShuffled(index) => self.play_index(index, ShuffleMode::Songs)?,
            Effect::PlayCollection {
                selected,
                members,
                shuffle,
                source,
                origin,
            } => self.play_members(selected, members, shuffle, source, origin)?,
            Effect::PlayQueue(index) => {
                if !self.model.jump_to(index) {
                    return Err("invalid queue entry".into());
                }
                self.load()?;
            }
            Effect::PlayNow(index) => {
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
                if !self.model.queue_next(vec![track])? {
                    self.model.jump_to(self.model.queue_position + 1);
                }
                self.load()?;
                self.checkpoint();
            }
            Effect::PlayNext(index) => self.queue_songs(vec![index], true)?,
            Effect::PlayNextCollection(members) => self.queue_songs(members, true)?,
            Effect::AddToQueue(index) => self.queue_songs(vec![index], false)?,
            Effect::AddToQueueCollection(members) => self.queue_songs(members, false)?,
            Effect::TogglePlayback => self.toggle()?,
            Effect::NextTrack | Effect::PreviousTrack => {
                let delta = if matches!(e, Effect::NextTrack) {
                    1
                } else {
                    -1
                };
                // A book moves by chapter.
                if let Some(result) = self.book_step(delta) {
                    result?;
                // A station has no next track: the next station of its list.
                } else if self.model.current().is_some_and(Track::is_radio) {
                    self.zap(delta)?;
                // Spotify moves through its own queue.
                } else if self.spotify_step(delta) {
                    // Previous more than 3 s into a song starts it again; a
                    // second press goes back a song (Y2Reborn #21).
                } else if delta < 0
                    && self.model.current().is_some()
                    && (self.model.position_ms > RESTART_SONG_AFTER_MS
                        || self.model.queue_position == 0)
                {
                    // The first song has nothing before it: it starts over.
                    self.seek_by(-(self.model.position_ms as i64));
                // Next on the last song: Repeat All starts over.
                } else if (delta > 0
                    && self.model.queue_position + 1 >= self.model.queue.len()
                    && self.model.settings.repeat == RepeatMode::All
                    && self.model.wrap())
                    || self.model.step(delta)
                {
                    self.load()?
                } else {
                    self.model.invalidate();
                    self.playback.stop(self.model.generation);
                }
            }
            // A live station has nothing to seek in.
            Effect::Seek(_) if self.model.current().is_some_and(Track::is_radio) => {}
            Effect::Seek(delta) => {
                if !self.spotify_seek(delta) {
                    self.seek_by(delta)
                }
            }
            Effect::AdjustVolume(delta) => {
                let now = self.model.settings.volume;
                let mut volume = (now as i16 + delta as i16).clamp(0, 100) as u8;
                let limit = self.model.settings.wired_volume_limit();
                if self.wired_sink() && delta > 0 && volume > limit {
                    // Up to the safe level, then ask (never lower here).
                    volume = now.max(limit);
                    if self.model.navigation.modal.is_none() {
                        self.model.navigation.modal = Some(reborn_core::Modal::Confirm(
                            reborn_core::ConfirmAction::LoudVolume,
                        ));
                        self.model.navigation.modal_focus = 0;
                        self.dirty.mark_both();
                    }
                }
                self.model.settings.volume = volume;
                self.playback.set_volume(self.sink_volume());
                self.send_bt_volume();
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
                // Asked for: every folder is read again (a tag edit made on a
                // computer may leave the folder and the free space unchanged).
                if let Err(error) = self.scanner.scan_all(self.model.sources.clone()) {
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
                let mut dac_hold = None;
                if !self.headless {
                    power::blank(true)?;
                    if let Some(graphics) = self.graphics.as_mut() {
                        if let Err(error) = graphics.sleep(true) {
                            let _ = power::blank(false);
                            return Err(error);
                        }
                    }
                    dac_hold = hold_dac(&self.log, false);
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
                    json!({"playback":self.model.playback,"dac_hold":dac_hold}),
                );
            }
            Effect::ScreenWake => {
                if !self.model.screen_off && (self.headless || self.graphics.is_some()) {
                    return Ok(());
                }
                let mut dac_hold = None;
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
                    dac_hold = hold_dac(&self.log, true);
                }
                self.model.apply(Event::ScreenWake);
                self.woke_at = Some(Instant::now());
                // Sleep turned Bluetooth off; the headphones return after the wake.
                if let Some(lost) = self.lost_peer.as_mut() {
                    lost.since = Instant::now();
                }
                // Awake again: a sleep still waiting for its turn is dropped.
                if self.queued_platform == Some(reborn_core::PlatformTask::SleepRequest) {
                    self.queued_platform = None;
                }
                self.last_activity = Instant::now();
                self.dark_since = None;
                self.wifi_idle_restore();
                self.log.emit(
                    Level::Info,
                    "power",
                    "screen_wake",
                    "Display woke",
                    None,
                    json!({"playback":self.model.playback,"dac_hold":dac_hold}),
                );
            }
            Effect::SetReplayGain(mode) => {
                self.model.settings.replay_gain = mode;
                self.reload_dsp()?;
            }
            Effect::SetEqEnabled(enabled) => {
                if self.model.settings.eq_bands.is_empty() {
                    self.model.settings.eq_bands = reborn_core::flat_eq_bands();
                }
                self.model.settings.eq_enabled = enabled;
                self.model.settings.eq_profile_on = false;
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
                // Editing the bands means listening to them, not the profile.
                if self.model.settings.eq_enabled || self.model.settings.eq_profile_on {
                    self.model.settings.eq_profile_on = false;
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
                self.model.settings.eq_profile_on = false;
                self.reload_dsp()?;
            }
            Effect::ResetEq => {
                self.model.settings.eq_bands = reborn_core::flat_eq_bands();
                self.reload_dsp()?;
            }
            Effect::ScanEqProfiles => {
                self.ui.eq_profiles = scan_eq_profiles(&shipped_eq_dirs(), &self.storage_roots());
                self.dirty.mark_render();
            }
            Effect::ImportEqProfile(path) => match import_eq_profile(&path) {
                Ok(profile) => {
                    let name = profile.name.clone();
                    let preamp = profile.preamp_db;
                    self.log.emit(
                        Level::Info,
                        "audio",
                        "eq_profile_imported",
                        "headphone profile imported",
                        None,
                        json!({"name":name,"filters":profile.bands.len(),"preamp_db":preamp}),
                    );
                    self.model.settings.eq_profile = Some(profile);
                    self.model.settings.eq_profile_on = true;
                    self.ui
                        .flash(format!("Playing {name} · Preamp {preamp:+.1} dB"));
                    self.reload_dsp()?;
                }
                Err(e) => self.ui.flash(e),
            },
            // The level in use again: nothing to restart.
            Effect::SetHpLevel(level) if level == self.model.settings.hp_level => {}
            Effect::SetHpLevel(level) => {
                let from = self.model.settings.hp_level;
                let raised = level.index() > from.index();
                self.model.settings.hp_level = level;
                // The wired volume moves so the output stays as loud (a
                // lower level may not reach it: then it plays quieter).
                // Bluetooth without its own volume shares `volume`: left alone.
                let wired_sink = self.wired_sink();
                let s = &mut self.model.settings;
                match s.wired_volume {
                    Some(wired) => s.wired_volume = Some(s.volume_for_level(wired, from)),
                    None if wired_sink => {
                        let before = s.volume;
                        s.volume = s.volume_for_level(before, from);
                        // Quieter at once. A higher number waits for the new
                        // stream at the lower level: the start sets it only
                        // after the old one is released.
                        if s.volume < before {
                            self.playback.set_volume(self.sink_volume());
                        }
                    }
                    None => {}
                }
                self.configure_wired();
                // The amplifier takes a new level only when it powers up:
                // restart wired playback (a higher level fades in); a held
                // pause gives its stream back, so Play starts a new one.
                if matches!(self.model.output, AudioOutput::Wired) {
                    self.reload_dsp()?;
                } else {
                    self.checkpoint();
                }
                if raised {
                    self.ui.flash("Higher level · Volume lowered to match");
                }
            }
            Effect::AllowLoudVolume => {
                self.model.settings.loud_ok = true;
                self.model.settings.loud_seconds = 0;
                self.log.emit(
                    Level::Info,
                    "audio",
                    "loud_volume_allowed",
                    "listener agreed to wired volumes above the safe level",
                    None,
                    json!({"hp_level":self.model.settings.hp_level,"volume":self.model.settings.volume}),
                );
                self.checkpoint();
                // The step that was held back.
                self.effect(Effect::AdjustVolume(2))?;
            }
            Effect::SetDacFilter(filter) => {
                self.model.settings.dac_filter = filter;
                self.configure_wired();
                self.checkpoint();
            }
            Effect::SetBrightness(percent) => {
                self.model.settings.brightness = percent.clamp(10, 100);
                if !self.headless {
                    power::set_brightness(self.model.settings.brightness)?;
                }
                self.checkpoint();
            }
            Effect::ResetSettings => {
                // The Last.fm sign-in is a setting too: forget it and its plays.
                self.lastfm_forget();
                // Volume and the music location are part of the session, not
                // preferences a reset should surprise the listener with.
                let defaults = reborn_core::Settings {
                    volume: self.model.settings.volume,
                    wired_volume: self.model.settings.wired_volume,
                    bluetooth_absolute_volume: self.model.settings.bluetooth_absolute_volume,
                    loud_ok: self.model.settings.loud_ok,
                    loud_seconds: self.model.settings.loud_seconds,
                    music_directory: self.model.settings.music_directory.clone(),
                    ..Default::default()
                };
                let reload = self.model.settings.gapless_enabled != defaults.gapless_enabled
                    || self.model.settings.eq_enabled
                    || self.model.settings.eq_profile_on
                    || self.model.settings.hp_level != defaults.hp_level
                    || self.model.settings.replay_gain != defaults.replay_gain
                    || self.model.settings.crossfade_ms != defaults.crossfade_ms
                    || self.model.settings.repeat != defaults.repeat;
                self.model.settings = defaults;
                self.configure_wired();
                if self.ui.skin.is_some() {
                    let _ = self.apply_theme("", false);
                }
                if !self.headless && self.model.platform.brightness_available {
                    let _ = power::set_brightness(self.model.settings.brightness);
                }
                if reload {
                    self.reload_dsp()?;
                } else {
                    self.checkpoint();
                }
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
            Effect::SetBalance(balance) => {
                self.model.settings.balance =
                    balance.clamp(-reborn_core::MAX_BALANCE, reborn_core::MAX_BALANCE);
                self.playback.set_balance(self.model.settings.balance);
                self.dirty.mark_both();
                self.checkpoint();
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
            Effect::SetAlbumOrder(order) => {
                self.model.settings.album_order = order;
                self.checkpoint();
            }
            Effect::SetNowPlayingWheel(wheel) => {
                self.model.settings.now_playing_wheel = wheel;
                self.checkpoint();
            }
            Effect::SetArtistsList(list) => {
                self.model.settings.artists_list = list;
                self.checkpoint();
            }
            Effect::SetWrapAround(wrap) => {
                self.model.settings.wrap_around = wrap;
                self.checkpoint();
            }
            Effect::PlayBook(key, start) => self.start_book(&key, start)?,
            Effect::SetSleepTimer(choice) => self.set_sleep_timer(choice),
            Effect::SetBookSpeed(percent) => {
                let speed = f32::from(percent.clamp(50, 200)) / 100.;
                if let Some(book) = &mut self.model.book {
                    book.speed = speed;
                }
                // Applied at once, from where it is (the place is kept).
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                } else if self.playback.is_held() {
                    self.release_hold();
                }
                self.checkpoint();
            }
            Effect::SetAudiobooksOnHome(on) => {
                self.model.settings.audiobooks_on_home = on;
                self.checkpoint();
            }
            Effect::SetBookFinished(key, finished) => self.set_book_finished(&key, finished)?,
            Effect::ForgetBook(key) => self.forget_book(&key)?,
            Effect::LoadChapters(key) => {
                if let Some(book) = self.book_by_key(&key) {
                    self.request_chapters(&book);
                }
            }
            Effect::SetScreenOffButtons(mode) => {
                self.model.settings.screen_off_buttons = mode;
                self.checkpoint();
            }
            Effect::SetHaptics(strength) => {
                self.model.settings.haptics = strength;
                self.checkpoint();
            }
            Effect::SetSpeaker(on) => {
                self.model.settings.speaker = on;
                self.checkpoint();
                // Playing on the jack output: follow the new route at once.
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) && self.model.speaker_active != self.speaker_route()
                {
                    self.load()?;
                } else if self.model.playback == PlaybackState::Stopped {
                    self.model.speaker_active = self.speaker_route();
                }
                self.dirty.mark_both();
            }
            Effect::SetShuffle(mode) => {
                let was = self.model.settings.shuffle_mode();
                self.model.settings.set_shuffle_mode(mode);
                // What plays now follows at once: its songs to come shuffle,
                // as songs or albums, or go back to their own order (your
                // queue stays as it is).
                let music =
                    self.model.book.is_none() && self.model.current().is_some_and(|t| !t.is_live());
                if music && mode != was {
                    let mut changed = was != ShuffleMode::Off && self.model.unshuffle_source();
                    changed |= match mode {
                        ShuffleMode::Off => false,
                        ShuffleMode::Songs => self.model.shuffle_source(shuffle_seed()),
                        ShuffleMode::Albums => self.model.shuffle_source_albums(shuffle_seed()),
                    };
                    if changed {
                        self.upcoming_changed();
                    }
                }
                self.checkpoint();
            }
            Effect::SetPlaylistSort(path, sort) => {
                self.model.settings.set_playlist_sort(&path, sort);
                self.checkpoint();
            }
            Effect::SetRepeat(mode) => {
                self.model.settings.repeat = mode;
                self.upcoming_changed();
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
            Effect::SetHomeShown(key, on) => {
                self.model.settings.show_on_home(&key, on);
                self.checkpoint();
            }
            Effect::QueueRemove(index) => {
                if index == self.model.queue_position {
                    self.ui.flash("The current track stays in the queue");
                } else if let Some(&id) = self.model.queue_entry_ids.get(index) {
                    if self.model.remove_entries(&[id]) > 0 {
                        self.upcoming_changed();
                        self.checkpoint();
                    }
                }
            }
            Effect::QueueMove { index, delta } => {
                let Some(&id) = self.model.queue_entry_ids.get(index) else {
                    return Ok(());
                };
                if self.model.move_entry(id, delta < 0) {
                    self.upcoming_changed();
                    self.checkpoint();
                }
            }
            Effect::QueueRemoveEntries(ids) => {
                let removed = self.model.remove_entries(&ids);
                if removed > 0 {
                    self.upcoming_changed();
                    self.checkpoint();
                    self.ui.flash(if removed == 1 {
                        "1 song removed".to_owned()
                    } else {
                        format!("{removed} songs removed")
                    });
                }
            }
            Effect::QueueEntriesNext(ids) => {
                let moved = self.model.entries_next(&ids);
                if moved > 0 {
                    self.upcoming_changed();
                    self.checkpoint();
                    self.ui.flash(if moved == 1 {
                        "Plays next".to_owned()
                    } else {
                        format!("{moved} songs play next")
                    });
                }
            }
            Effect::QueueEntriesLast(ids) => {
                let moved = self.model.entries_last(&ids);
                if moved > 0 {
                    self.upcoming_changed();
                    self.checkpoint();
                    self.ui.flash(if moved == 1 {
                        "Added to queue".to_owned()
                    } else {
                        format!("{moved} songs added to queue")
                    });
                }
            }
            Effect::ClearQueued => {
                if self.model.clear_queued() {
                    self.upcoming_changed();
                    self.checkpoint();
                }
            }
            Effect::ClearQueue => {
                if self.model.clear_future_queue() {
                    self.upcoming_changed();
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
    /// Play the changed sound settings: restart what plays; a held pause
    /// gives its stream back, or Play would continue with the old ones.
    fn reload_dsp(&mut self) -> Result<(), String> {
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.load()?;
        } else if self.playback.is_held() {
            self.release_hold();
        }
        self.checkpoint();
        Ok(())
    }

    /// Keep a task the owner started for the worker's next free turn; a
    /// periodic observation is simply skipped.
    fn queue_platform(&mut self, task: reborn_core::PlatformTask) {
        if task == reborn_core::PlatformTask::Refresh {
            return;
        }
        if self.queued_platform != Some(task) {
            self.log.emit(
                Level::Info,
                "platform",
                "task_queued",
                "Platform task waits for the one still running",
                None,
                json!({"task":format!("{task:?}"),"running":format!("{:?}", self.model.platform.busy)}),
            );
        }
        self.queued_platform = Some(task);
    }

    /// Start a queued task once the worker is free, and never let the update
    /// screen own the display with no update task behind it.
    fn platform_tick(&mut self) {
        if self.model.platform.busy.is_none() {
            if let Some(task) = self.queued_platform.take() {
                if let Err(error) = self.effect(Effect::Platform(task)) {
                    if matches!(
                        task,
                        reborn_core::PlatformTask::UpdateStage
                            | reborn_core::PlatformTask::UpdateApply
                    ) {
                        self.ui.update_run = false;
                    }
                    self.ui.flash(error);
                }
            }
        }
        let waiting = update_screen_waits(
            self.ui.update_run,
            self.ui.update_restarting,
            self.model.platform.busy,
            self.queued_platform,
        );
        if !waiting {
            self.update_idle_since = None;
            return;
        }
        let since = *self.update_idle_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= Duration::from_secs(30) {
            self.update_idle_since = None;
            self.ui.update_run = false;
            self.log.emit(
                Level::Warn,
                "update",
                "screen_without_task",
                "Update screen showed no running update task for 30 s",
                None,
                json!({"busy":format!("{:?}", self.model.platform.busy)}),
            );
            self.ui
                .flash("The update didn't start. Nothing was changed. Please try again.");
            self.dirty.mark_render();
        }
    }

    fn poll_platform(&mut self) {
        let Some(worker) = &self.platform else {
            return;
        };
        let Ok(reply) = worker.replies.try_recv() else {
            return;
        };
        let mut refresh = false;
        // The one-step update installs right after its download (Beta 7.1).
        let mut install_next = false;
        let mut restart_after_setup = false;
        let p = &mut self.model.platform;
        match reply {
            client::Reply::Sleep(result) => {
                p.busy = None;
                match result {
                    Ok(state) => {
                        // A refusal leaves the screen as it is: the request
                        // turned it off already, and lit now means a press
                        // woke it (F48: it went dark again ≈1 s after a wake).
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
                let sd_before = p.snapshot.storage.sd;
                p.snapshot = *snapshot;
                // Y2Linux 8.27: a card with errors plays read-only; say so once
                // when it arrives (Settings > Storage offers the repair).
                if matches!(
                    p.snapshot.storage.sd,
                    reborn_core::platform::SdCard::ReadOnly(_)
                ) && !matches!(sd_before, reborn_core::platform::SdCard::ReadOnly(_))
                {
                    self.ui.flash(
                        "SD card has errors · Playing only. Repair it in Settings > Storage",
                    );
                }
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
                {
                    // Y2Linux STORAGE-SETUP: said once when it ran at this start;
                    // a real old library is asked about once (Settings > Storage
                    // keeps the choice).
                    use reborn_core::platform::InternalSetup;
                    match p.snapshot.internal_setup {
                        InternalSetup::Done { kept } if !self.announced_setup => {
                            self.announced_setup = true;
                            self.ui.flash(match kept {
                                0 => "Internal Storage is ready".to_owned(),
                                1 => "Internal Storage is ready · Kept 1 file in Old Player Files"
                                    .to_owned(),
                                n => format!(
                                    "Internal Storage is ready · Kept {n} files in Old Player Files"
                                ),
                            });
                        }
                        InternalSetup::Waiting { .. }
                            if !self.model.settings.internal_setup_asked
                                && self.model.navigation.modal.is_none() =>
                        {
                            self.model.settings.internal_setup_asked = true;
                            self.model.navigation.modal = Some(reborn_core::Modal::Confirm(
                                reborn_core::ConfirmAction::Platform(
                                    reborn_core::PlatformTask::InternalStorageSetup,
                                ),
                            ));
                            self.model.navigation.modal_focus = 0;
                            self.dirty.mark_both();
                        }
                        _ => {}
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
            client::Reply::Reports(task, result) => {
                use reborn_core::PlatformTask::*;
                p.busy = None;
                p.report = None;
                match result {
                    Ok(state) => {
                        let enabled = state.enabled;
                        // The platform sends a new report at once when it can.
                        let sent = state.last.as_ref().is_some_and(|l| l.state == "sent");
                        p.snapshot.reports = Some(state);
                        p.failure = None;
                        self.ui.flash(match task {
                            ReportEnable(true) => "Error reports are on".to_owned(),
                            ReportEnable(false) => "Error reports are off".to_owned(),
                            ReportNewId => "New install ID".to_owned(),
                            ReportSend if sent => "Report sent".to_owned(),
                            ReportSend if enabled == Some(true) => {
                                "Report saved · waiting to send".to_owned()
                            }
                            ReportSave(to) => format!("Report saved to {}", to.label()),
                            _ => "Done".to_owned(),
                        });
                    }
                    Err(error) => {
                        self.ui.flash(error.clone());
                        p.failure = Some(error);
                    }
                }
            }
            client::Reply::Health(level, section) => {
                p.busy = None;
                p.snapshot.health = level;
                p.snapshot.diagnostics.retain(|s| s.id != "health");
                p.snapshot.diagnostics.push(section);
            }
            client::Reply::Operation(task, result) => {
                if matches!(
                    task,
                    reborn_core::PlatformTask::UpdateStage | reborn_core::PlatformTask::UpdateApply
                ) {
                    self.log.emit(
                        if result.is_ok() {
                            Level::Info
                        } else {
                            Level::Warn
                        },
                        "update",
                        "task_finished",
                        "Update task answered",
                        None,
                        json!({"task":format!("{task:?}"),"error":result.as_ref().err()}),
                    );
                }
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
                            InternalStorageSetup => {
                                // Library, USB and folders start again on the new
                                // Internal Storage.
                                self.ui.flash("Internal Storage is ready. Restarting…");
                                restart_after_setup = true;
                            }
                            SdRepair => {
                                let shortened = p
                                    .result
                                    .as_ref()
                                    .and_then(|r| {
                                        r.facts.iter().find(|f| f.label == "Files shortened")
                                    })
                                    .and_then(|f| f.value.parse::<u64>().ok())
                                    .unwrap_or(0);
                                self.ui.flash(match shortened {
                                    0 => "SD card repaired".to_owned(),
                                    1 => {
                                        "SD card repaired · 1 damaged file was shortened".to_owned()
                                    }
                                    n => format!(
                                        "SD card repaired · {n} damaged files were shortened"
                                    ),
                                });
                            }
                            SetClock(_) => {
                                // A time set by hand is never trusted: read
                                // the platform's verdict again at once.
                                self.clock_trusted = None;
                                self.ui.flash("Date and time set");
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
                        } else if task == UpdateCheck {
                            // The page keeps a release it already offered.
                            self.ui
                                .flash("Couldn't reach the update server. Try again in a moment.");
                        } else {
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
        if restart_after_setup {
            if let Err(error) = self.effect(Effect::Reboot) {
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
            let result = self.restore_sleep(false);
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
        // Headphones connected: a pause stays awake a while, so their own
        // play button can continue (asleep, the radio is off).
        let headset = self.model.playback == PlaybackState::Paused
            && matches!(&self.model.output, AudioOutput::Bluetooth(address)
            if self.bt_state.devices.iter().any(|d| {
                d.connected && d.address.eq_ignore_ascii_case(address)
            }));
        if !headset {
            self.headset_paused = None;
        } else if self.headset_paused.is_none() {
            self.headset_paused = Some(Instant::now());
        }
        if reborn_core::platform::headset_pause_awake(
            self.headset_paused.map(|since| since.elapsed().as_secs()),
        ) {
            return;
        }
        if reborn_core::platform::automatic_sleep_due(
            self.model.screen_off,
            self.dark_since.map(|since| since.elapsed().as_secs()),
            self.last_activity.elapsed().as_secs(),
            self.sleep_attempt.map(|since| since.elapsed().as_secs()),
            playing,
            busy,
        ) && self.sleep_attempt.is_none_or(|at| {
            at.elapsed().as_secs()
                >= reborn_core::platform::sleep_retry_seconds(self.sleep_refusals)
        }) {
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
        let (healthy, reasons) = update_health(
            self.headless,
            self.graphics.is_some(),
            self.log.uptime(),
            &self.log.health(),
        );
        json!({"healthy":healthy,"health_reasons":reasons,"version":reborn_core::BUILD_LABEL,"build_id":option_env!("REBORN_BUILD_ID").unwrap_or("development"),"session":self.log.session(),"uptime_seconds":self.log.uptime(),"current_screen":self.model.screen,"screen_off":self.model.screen_off,"playback":{"state":self.model.playback,"track_id":self.model.current().map(|t|t.id),"position_ms":self.model.position_ms,"duration_ms":self.model.current().map(|t|t.duration_ms),"queue_length":self.model.queue.len(),"queue_position":self.model.queue_position,"generation":self.model.generation},"output":self.model.output,"audio":self.playback.audio_state(),"library":{"tracks_loaded":self.model.library.tracks.len(),"schema":reborn_library::SCHEMA_VERSION,"scanning":self.model.library.scanning,"last_scan":self.model.library.last_scan,"error":self.model.library.error},"wifi":self.wifi_state,"bluetooth":self.bt_state,"storage":self.model.sources,"power":self.power,"graphics":{"available":self.graphics.is_some(),"renderer":self.graphics.as_ref().map(|g|&g.info),"headless":self.headless},"decoder":{"ffmpeg":reborn_media::version(),"runtime":reborn_media::runtime_components_if_loaded()},"buffers":{"frames":self.log.metrics()["audio_buffer_frames"],"milliseconds":self.log.metrics()["audio_buffer_ms"]}})
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
                // No one to ask here: never above the agreed wired level.
                let limit = if self.wired_sink() {
                    self.model.settings.wired_volume_limit()
                } else {
                    100
                };
                self.model.settings.volume = v.min(limit.max(self.model.settings.volume)).min(100);
                self.dirty.mark_both();
                self.playback.set_volume(self.sink_volume());
                self.send_bt_volume();
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
    let problem = wifi::problem(s);
    WifiView {
        available: s.available,
        powered: s.enabled,
        scan: s.scan.clone(),
        problem_network: s
            .failed_network_id
            .filter(|_| problem == Some(reborn_core::platform::WifiProblem::WrongPassword))
            .and_then(|id| s.saved.iter().find(|n| n.saved_id == Some(id)))
            .map(|n| n.ssid.clone()),
        problem,
        join: s.join.as_ref().map(|j| reborn_ui::WifiJoin {
            attempt: j.attempt,
            ssid: j.ssid.clone(),
            outcome: match j.outcome {
                wifi::JoinOutcome::Checking => reborn_ui::WifiJoinOutcome::Checking,
                wifi::JoinOutcome::Joined => reborn_ui::WifiJoinOutcome::Joined,
                wifi::JoinOutcome::WrongPassword => reborn_ui::WifiJoinOutcome::WrongPassword,
                wifi::JoinOutcome::NotFound => reborn_ui::WifiJoinOutcome::NotFound,
                wifi::JoinOutcome::Failed => reborn_ui::WifiJoinOutcome::Failed,
            },
        }),
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
                connecting: s.waiting.as_deref() == Some(d.path.as_str()),
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
            | Screen::ErrorReports
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

/// Bluetooth headphones that were the output until they dropped (F51).
struct LostPeer {
    address: String,
    /// The loss, or the wake after it.
    since: Instant,
    /// A Play is waiting for them since then.
    play: Option<Instant>,
}
impl LostPeer {
    fn new(address: String, since: Instant) -> Self {
        Self {
            address,
            since,
            play: None,
        }
    }
    fn is(&self, output: &AudioOutput) -> bool {
        matches!(output, AudioOutput::Bluetooth(a) if a.eq_ignore_ascii_case(&self.address))
    }
    /// Play waits: the jack took over from them a short while ago and holds
    /// no headphones (a plug there is a choice of its own).
    fn applies(&self, output: &AudioOutput, headphones: Option<bool>, now: Instant) -> bool {
        matches!(output, AudioOutput::Wired)
            && headphones != Some(true)
            && !self.gave_up(now)
            && (self.play.is_some() || !self.expired(now))
    }
    fn gave_up(&self, now: Instant) -> bool {
        self.play
            .is_some_and(|at| now.saturating_duration_since(at) >= LOST_PEER_PLAY_WAIT)
    }
    fn expired(&self, now: Instant) -> bool {
        self.play.is_none() && now.saturating_duration_since(self.since) >= LOST_PEER_WINDOW
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

/// A start failed because the selected Bluetooth peer is not connected, and
/// the last observation agrees: the disconnect was missed, not just late.
fn selected_peer_gone(output: &AudioOutput, state: &bluetooth::Status, error: &str) -> bool {
    let AudioOutput::Bluetooth(address) = output else {
        return false;
    };
    error.contains("A2DP peer required")
        && state.available
        && !state
            .devices
            .iter()
            .any(|d| d.address.eq_ignore_ascii_case(address) && d.connected)
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
            // Keep the refused file (a newer Reborn may read it again after the
            // update is installed once more) and every setting this one knows.
            let settings = recover_settings(&session_path);
            let kept = session_path.with_file_name(format!(
                "session.json.rejected-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            ));
            let quarantined = fs::rename(&session_path, &kept).is_ok();
            log.emit(
                Level::Warn,
                "core",
                "restore_failed",
                "Saved session was rejected; starting with its readable settings",
                None,
                json!({"path":session_path,"error":error.to_string(),"settings_recovered":settings.is_some(),"kept":quarantined.then_some(&kept)}),
            );
            AppModel {
                settings: settings.unwrap_or_default(),
                ..AppModel::default()
            }
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
        // The wired level and DAC filter, before anything plays.
        if let Err(e) =
            reborn_audio::wired_configure(model.settings.hp_level, model.settings.dac_filter)
        {
            log.emit(
                Level::Warn,
                "audio",
                "wired_configure_failed",
                &e,
                None,
                json!({}),
            );
        }
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
        queued_platform: None,
        update_idle_since: None,
        zone_lookup_again: false,
        chapter_reader: if headless {
            None
        } else {
            books::ChapterReader::spawn().ok()
        },
        chapters_asked: HashSet::new(),
        shelf_written: Default::default(),
        paused_at: None,
        sleep_fading: false,
        sleep_minutes_shown: None,
        headphones: None,
        jack_pending: None,
        lost_peer: None,
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
        announced_setup: false,
        asked_reports: false,
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
        bt_volume: BtVolume::default(),
        graphics,
        graphics_recovery: graphics_recovery::Recovery::default(),
        woke_at: None,
        power: power::status(),
        kernel_events: Arc::new(Mutex::new(Vec::new())),
        background,
        art: false,
        history_seen: None,
        history_track: None,
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
        scrobbles: reborn_core::scrobble::Tracker::default(),
        scrobbles_unstamped: vec![],
        scrobble_queue: reborn_platform::scrobble::Queue::new(root.join("state/scrobbles.jsonl")),
        clock_trusted: None,
        lastfm: scrobbling::LastFm::load(&root),
        spotify: spotify::Spotify::load(&root),
        spotify_cover: None,
        radio_logos: if headless {
            None
        } else {
            LogoFetcher::spawn(root.join("cache"))
        },
        radio_art: None,
        radio_art_pending: None,
        playlist_worker: playlists::Worker::spawn(Some(root.join("cache/playlists.json"))),
        headless,
        last_activity: Instant::now(),
        query: Some(initial_catalog),
        action_depth: 0,
        action_before: None,
        action_dirty_before: None,
        pending_reconfiguration: None,
        held_since: None,
        loaded_dsp: None,
        seek_until: None,
        seek_run: None,
        reconfiguration_attempted: false,
        sink_release_started: false,
        transition_generation: None,
        active_transport_generation: None,
        input_resume: false,
        sleep_prepared: false,
        sleep_prepared_since: None,
        dark_since: None,
        sleep_attempt: None,
        sleep_refusals: 0,
        sleep_requested_at: None,
        headset_paused: None,
        wifi_idle_off: false,
        wifi_idle_failed: false,
    };
    rt.model.library.shelf = books::load_shelf(&root.join("state/books.json"));
    rt.shelf_written = rt.model.library.shelf.clone();
    rt.ui.radio.favorites = rt
        .radio_favorites
        .iter()
        .map(|s| s.key().to_owned())
        .collect();
    rt.lastfm_view();
    rt.spotify_view();
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
    // #28: the headphone jack, when the kernel reports it.
    let mut jack = if headless {
        None
    } else {
        reborn_platform::jack::Jack::open().ok()
    };
    if let Some(j) = &jack {
        rt.headphones = j.current();
        rt.model.speaker_active = rt.speaker_route();
    }
    // #28: the vibration motor, when the kernel has one (Linux with VIBR).
    let mut seek_ticked: Option<Instant> = None;
    let mut motor = if headless {
        None
    } else {
        reborn_platform::haptics::Motor::open()
            .map_err(|e| {
                log.emit(
                    Level::Info,
                    "input",
                    "haptics_unavailable",
                    &e.to_string(),
                    None,
                    json!({}),
                )
            })
            .ok()
    };
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
        rt.receive_chapters();
        rt.sleep_timer_tick();
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
                        reborn_ui::art::round_corners(&mut rounded, reborn_graphics::ART_SIZE);
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
                        reborn_ui::art::round_corners(&mut rgba, reborn_graphics::ART_SIZE);
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
                        .or_else(|| rt.spotify_cover.clone())
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
        rt.ask_about_reports();
        rt.platform_tick();
        // Platform screens observe on demand; one observation after start
        // (20 s in, after the boot work) also on any other screen, so the
        // error report question comes without opening Settings.
        if !rt.model.screen_off
            && !rt.platform_refresh_pending
            && rt.model.platform.busy.is_none()
            && (platform_backed(rt.model.screen)
                || (!rt.platform_observed && process_started.elapsed() >= Duration::from_secs(20)))
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
        // A report the owner started answers only when it is done: show the
        // platform's live record meanwhile, like the update download.
        if matches!(
            rt.model.platform.busy,
            Some(reborn_core::PlatformTask::ReportSend | reborn_core::PlatformTask::ReportSave(_))
        ) && !rt.model.screen_off
            && download_poll.elapsed() >= Duration::from_millis(200)
        {
            download_poll = Instant::now();
            let live = client::report_progress();
            if live != rt.model.platform.report {
                rt.model.platform.report = live;
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
        // #28: a computer connecting ticks with its message (it lights the
        // screen); never in the dark.
        if rt.ui.take_connect_tick() && !rt.model.screen_off {
            haptic_tick(&mut motor, rt.model.settings.haptics, &log);
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
        if let Some(plugged) = jack.as_mut().and_then(reborn_platform::jack::Jack::poll) {
            rt.jack_pending = Some(plugged);
        }
        // As a user action: the reconfiguration it starts is polled to its
        // end (a bare load() left the new output prepared and never fed:
        // "Starting" for good after headphones went in, test build 157).
        if rt.pending_reconfiguration.is_none() {
            if let Some(plugged) = rt.jack_pending.take() {
                if let Err(e) = rt.with_model_action(|runtime| runtime.jack_changed(plugged)) {
                    rt.fail("audio", e);
                }
            }
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
                action_router.set_keyboard_wheel(
                    rt.ui
                        .keyboard
                        .as_ref()
                        .is_some_and(reborn_ui::keyboard::Keyboard::live),
                );
                action_router.set_screen_off_buttons(rt.model.settings.screen_off_buttons);
                let lit = !rt.model.screen_off;
                let seek_run = rt.seek_run;
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
                    log.gauge(
                        "ui_action_ms",
                        action_started.elapsed().as_secs_f64() * 1000.,
                    );
                    if performance.enabled() {
                        performance.record("state", json!({"action":action,"screen":rt.model.screen,"focus":rt.model.navigation.focus,"action_ms":action_started.elapsed().as_secs_f64()*1000.,"playback":rt.model.playback,"volume":rt.model.settings.volume}));
                    }
                }
                // #28: as stock, one tick per button press on a lit screen
                // (not the press that wakes it), none for wheel steps; a held
                // Previous/Next ticks on its seek steps, at most every
                // SEEK_TICK_GAP (steps come every 90 ms: one 60 ms tick each
                // would be a buzz, as the rejected wheel ticks were). The
                // setting is read after the action, so choosing a strength
                // ticks at it.
                let seeked = rt.seek_run != seek_run
                    && seek_ticked.is_none_or(|at| at.elapsed() >= SEEK_TICK_GAP);
                if input_ticks(lit, event.input, &actions, seeked) {
                    if seeked {
                        seek_ticked = Some(Instant::now());
                    }
                    haptic_tick(&mut motor, rt.model.settings.haptics, &log);
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
                            reborn_ui::art::round_corners(&mut rounded, reborn_graphics::ART_SIZE);
                            if g.artwork(&rounded).is_ok() {
                                rt.art = true;
                                rt.dirty.mark_render();
                            }
                        }
                    }
                }
                playback::PlaybackEvent::Core(event) => {
                    if let Event::Position { generation, ms } = event {
                        // A pending seek's target is the position: a late
                        // report of the held stream would undo it (Previous
                        // after 3 s kept playing on, 8.30 device test).
                        if rt.seek_until.is_some() {
                            continue;
                        }
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
                    let boundary = matches!(
                        &event,
                        Event::TrackBoundary { generation, .. } if *generation == rt.model.generation
                    );
                    rt.model.apply(event);
                    if boundary {
                        // The decoder knows `UPCOMING` songs ahead: top it up.
                        rt.upcoming_changed();
                    }
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
            // The playlist files the scan met: Playlists need not walk the
            // storages again.
            if let (Ok(stats), Some(worker)) = (&result, &rt.playlist_worker) {
                if !stats.playlists.is_empty() {
                    worker.send(playlists::Job::Known(stats.playlists.clone()));
                }
            }
            match result {
                Ok(stats) if stats.complete => {
                    rt.model
                        .apply(Event::LibraryScanFinished(reborn_core::ScanSummary {
                            discovered: stats.discovered,
                            reused: stats.reused,
                            elapsed_ms: stats.elapsed_ms,
                            unreadable: stats.unreadable,
                        }));
                    // Files that could not be read are counted on Settings →
                    // Library, not announced after every start.
                    let songs = stats.discovered.saturating_sub(stats.unreadable);
                    rt.ui.flash(match songs {
                        _ if stats.rescanned > 0 && songs > 1 => {
                            format!("{} new songs added", stats.rescanned.min(songs))
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
                        // A restored queue names its songs: their details now.
                        rt.model.rehydrate_queue();
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
                    rt.ui.wifi_join_changed();
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
        rt.spotify_tick();
        rt.lost_peer_tick();
        rt.scrobble_tick();
        rt.note_history();
        rt.lastfm_tick();
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
                // Several snapshots can arrive in one tick (after a wake, or a
                // burst when a peer drops): a later one must not hide an earlier
                // disconnect (reports 61caa81e/297f1032: output stayed Bluetooth
                // and every Play failed until a restart).
                if let AudioOutput::Bluetooth(address) = &rt.model.output {
                    bt_lost |= rt
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
            rt.bluetooth_peer_lost("paused");
        }
        rt.sync_bt_volume();
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
                Command::SleepRestore { dark } => rt.restore_sleep(dark),
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
                Command::Jack { plugged } => {
                    // Taken where a kernel report is: the next loop acts on it.
                    if plugged.is_some() {
                        rt.jack_pending = plugged;
                    }
                    Ok(json!({"headphones":rt.headphones,"pending":rt.jack_pending,
                        "speaker_active":rt.model.speaker_active,"speaker":rt.model.settings.speaker,
                        "playback":rt.model.playback,"output":rt.model.output}))
                }
                Command::Action { action } => {
                    // As the router would hand it over (device tests).
                    rt.last_activity = Instant::now();
                    let effect = rt.ui.model_action(&mut rt.model, action);
                    rt.dirty.mark_render();
                    rt.effect(effect).map(|_| {
                        json!({"screen":rt.model.screen,"focus":rt.model.navigation.focus,
                            "modal":rt.model.navigation.modal.is_some(),
                            "modal_focus":rt.model.navigation.modal_focus,
                            "queue_position":rt.model.queue_position,"queue":rt.model.queue.len(),
                            "queued":rt.model.queued_count(),"notice":rt.ui.notice})
                    })
                }
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
                    rt.model.settings.playback_heavy(),
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
            rt.loud_listening_tick(periodic.elapsed());
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
            // The playing wave moves with what is heard (Beta 8.38).
            let heard = (rt.model.playback == PlaybackState::Playing)
                .then(|| rt.playback.heard())
                .flatten();
            rt.ui.set_heard(heard);
            let mut draw = rt.ui.draw(&rt.model, &rt.model.library.tracks, rt.art);
            let layout_ms = frame_started.elapsed().as_secs_f64() * 1000.;
            log.gauge("ui_layout_ms", layout_ms);
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
                    if !rt.retry_after_wake(&e) {
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
                    }
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
                            // The UI is up with the screen on: the DAC stays
                            // powered until the screen sleeps.
                            if !rt.model.screen_off {
                                hold_dac(&log, true);
                            }
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
        // Screen off and no scan running: a long library's lists are made
        // now, one a pass, not when the screen is woken (Beta 8.39). Input
        // still comes between the pieces.
        let idle = rt.model.screen_off && !rt.model.library.scanning && !rt.sleep_prepared;
        // The playlists are read then too, if nothing read them yet: the
        // prewarm's last piece matches them to the library.
        if idle
            && rt.playlist_worker.is_some()
            && !rt.ui.playlists.loaded
            && !rt.ui.playlists.loading
        {
            if let Err(e) = rt.playlist(reborn_core::PlaylistRequest::Refresh) {
                rt.fail("playlists", e);
            }
        }
        let prewarming = idle && rt.ui.prewarm(&rt.model, &rt.model.library.tracks);
        let interval = if prewarming {
            Duration::from_millis(20)
        } else {
            main_loop_interval(
                rt.model.screen_off,
                rt.dirty.render,
                rt.ui.gliding(),
                matches!(
                    rt.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ),
                render_time.elapsed(),
            )
        };
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
        // The screen is off for good: the DAC need not wait for it.
        hold_dac(&log, false);
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

/// What can be kept of a session `AppModel::restore` refused (for example one
/// an update newer than this Reborn wrote, read again after a rollback): every
/// saved setting this version understands, field by field; the rest keeps its
/// default. The queue is not recovered.
fn recover_settings(path: &Path) -> Option<reborn_core::Settings> {
    use reborn_core::{Settings, BRIGHTNESS_LEVELS, SESSION_MAX_BYTES};
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
    settings.brightness = settings.brightness.clamp(BRIGHTNESS_LEVELS[0], 100);
    Some(settings)
}

/// The profiles that come with the player (AutoEq, `assets/eq` in Reborn).
fn shipped_eq_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![];
    if let Ok(dev) = std::env::var("REBORN_EQ_PROFILES") {
        dirs.push(PathBuf::from(dev));
    }
    dirs.push(PathBuf::from("/usr/share/reborn/eq"));
    dirs
}

/// Normalize Volume's mode and the equalizer (preamp and filters) a load plays.
type LoadedDsp = (
    reborn_core::ReplayGainMode,
    Option<(f32, Vec<reborn_core::EqBand>)>,
);

/// Whether a load fades in like a raised headphone level: its sound settings
/// play at least 1 dB louder somewhere than the previous load's (a profile
/// turned off, a boosting preset), or Normalize changed (its gain comes from
/// each song's tags, so count it as a rise). Not the first load: that plays
/// what was saved, as the headphone level does.
fn dsp_rise_fades(previous: Option<&LoadedDsp>, next: &LoadedDsp) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    if previous == next {
        return false;
    }
    fn eq(dsp: &LoadedDsp) -> Option<(f32, &[reborn_core::EqBand])> {
        dsp.1
            .as_ref()
            .map(|(preamp, bands)| (*preamp, bands.as_slice()))
    }
    previous.0 != next.0 || reborn_core::eq::level_rise_db(eq(previous), eq(next)) >= 1.0
}

/// Profile files (not hidden, name ending in `suffix`, any letter case)
/// directly in `dir`.
fn eq_files_in(dir: &Path, suffix: &str, files: &mut Vec<reborn_core::eq::ProfileFile>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.')
            && name.to_ascii_lowercase().ends_with(suffix)
            && entry.file_type().is_ok_and(|t| t.is_file())
        {
            files.push(reborn_core::eq::ProfileFile {
                name: reborn_core::eq::profile_name(&name),
                path: entry.path(),
            });
        }
    }
}

/// `ParametricEQ.txt` files in an `EQ` folder (any letter case) on each
/// storage root, then the shipped ones, sorted by name. A profile on the
/// storage replaces a shipped one of the same name (a newer AutoEq file).
fn scan_eq_profiles(shipped: &[PathBuf], roots: &[PathBuf]) -> Vec<reborn_core::eq::ProfileFile> {
    let mut files = vec![];
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for folder in entries.flatten().filter(|e| {
            e.file_name().to_string_lossy().eq_ignore_ascii_case("eq")
                && e.file_type().is_ok_and(|t| t.is_dir())
        }) {
            eq_files_in(&folder.path(), reborn_core::eq::FILE_SUFFIX, &mut files);
        }
    }
    // The shipped folder also holds AutoEq's license and SOURCES.md.
    for dir in shipped {
        eq_files_in(dir, "parametriceq.txt", &mut files);
    }
    let mut seen = std::collections::HashSet::new();
    files.retain(|f| seen.insert(f.name.to_lowercase()));
    files.sort_by_key(|f| f.name.to_lowercase());
    files
}

/// Read and check one profile file; the error is a message for the screen.
fn import_eq_profile(path: &Path) -> Result<reborn_core::eq::EqProfile, String> {
    let size = fs::metadata(path)
        .map_err(|_| "That profile file is gone".to_owned())?
        .len();
    if size > reborn_core::eq::MAX_FILE_BYTES {
        return Err("That file is too large for an equalizer profile".into());
    }
    let text = fs::read(path).map_err(|_| "That profile file could not be read".to_owned())?;
    let text = String::from_utf8_lossy(&text);
    let name = path
        .file_name()
        .map(|n| reborn_core::eq::profile_name(&n.to_string_lossy()))
        .unwrap_or_else(|| "Profile".into());
    reborn_core::eq::parse(&name, &text)
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
mod jack_and_haptics_tests {
    use super::{input_ticks, jack_move, JackMove, HEADPHONES_LOST};
    use reborn_core::{Action, NormalizedInput, PhysicalControl};

    /// #62: unplugged headphones that were playing pause, with a message; a
    /// playing speaker moves to plugged headphones; anything else stays.
    #[test]
    fn unplugged_headphones_pause_and_say_so() {
        assert_eq!(
            jack_move(true, true, false, Some(true), false),
            JackMove::Pause
        );
        assert_eq!(HEADPHONES_LOST, "Headphones unplugged. Playback paused.");
        // Not playing, on Bluetooth, the speaker playing or the first reading:
        // nothing paused, so no message.
        assert_eq!(
            jack_move(true, false, false, Some(true), false),
            JackMove::Nothing
        );
        assert_eq!(
            jack_move(false, true, false, Some(true), false),
            JackMove::Nothing
        );
        assert_eq!(
            jack_move(true, true, false, Some(true), true),
            JackMove::Nothing
        );
        assert_eq!(jack_move(true, true, false, None, false), JackMove::Nothing);
        assert_eq!(
            jack_move(true, true, true, Some(false), true),
            JackMove::ToHeadphones
        );
    }

    /// #28: a tick per press and per seek step of a held Previous/Next on a
    /// lit screen; none for the wheel (also its Now Playing seek), a hold
    /// that seeks nothing (a station) or in the dark.
    #[test]
    fn haptics_tick_for_presses_and_held_seek_steps_only() {
        let next = PhysicalControl::Next;
        assert!(input_ticks(true, NormalizedInput::Press(next), &[], false));
        assert!(!input_ticks(
            false,
            NormalizedInput::Press(next),
            &[],
            false
        ));
        for held in [
            NormalizedInput::LongPress(next),
            NormalizedInput::Repeat(next),
        ] {
            assert!(input_ticks(true, held, &[Action::SeekForward], true));
            assert!(!input_ticks(true, held, &[Action::SeekForward], false));
            assert!(!input_ticks(false, held, &[Action::SeekForward], true));
            assert!(!input_ticks(true, held, &[], false));
        }
        let back = NormalizedInput::Repeat(PhysicalControl::Previous);
        assert!(input_ticks(true, back, &[Action::SeekBackward], true));
        let wheel = NormalizedInput::WheelClockwise(1);
        assert!(!input_ticks(
            true,
            wheel,
            &[Action::WheelClockwise(1)],
            true
        ));
        assert!(!input_ticks(
            true,
            NormalizedInput::Release(next),
            &[Action::NextTrack],
            false
        ));
    }
}

#[cfg(test)]
mod session_recovery_tests {
    use super::recover_settings;
    use reborn_core::{AppModel, CodecPreference, SESSION_SCHEMA_VERSION};
    use serde_json::json;
    use std::fs;

    #[test]
    fn settings_survive_a_session_from_a_newer_reborn() {
        let path = std::env::temp_dir().join(format!("reborn-newer-{}.json", std::process::id()));
        let mut model = AppModel::default();
        model.settings.volume = 62;
        model.settings.theme = "classic".into();
        let mut value = serde_json::to_value(&model).unwrap();
        value["settings"]["codec_preference"] = "Opus-Future".into();
        value["settings"]["a_later_setting"] = true.into();
        fs::write(
            &path,
            serde_json::to_vec(&json!({"schema": SESSION_SCHEMA_VERSION + 1, "model": value}))
                .unwrap(),
        )
        .unwrap();
        assert!(AppModel::restore(&path).is_err());
        let settings = recover_settings(&path).unwrap();
        assert_eq!(settings.volume, 62);
        assert_eq!(settings.theme, "classic");
        assert_eq!(settings.codec_preference, CodecPreference::Auto);
        fs::remove_file(path).unwrap();
    }
}
#[cfg(test)]
mod dsp_rise_tests {
    use super::{dsp_rise_fades, LoadedDsp};
    use reborn_core::{EqBand, EqKind, ReplayGainMode};

    #[test]
    fn a_louder_sound_setting_fades_in_and_the_same_or_quieter_does_not() {
        let profile = reborn_core::eq::parse(
            "x",
            "Preamp: -6 dB\nFilter 1: ON LSC Fc 105 Hz Gain 6 dB Q 0.7\n",
        )
        .unwrap();
        let with = |rg, eq: Option<(f32, Vec<EqBand>)>| -> LoadedDsp { (rg, eq) };
        let on = with(
            ReplayGainMode::Off,
            Some((profile.preamp_db, profile.bands)),
        );
        let off = with(ReplayGainMode::Off, None);
        assert!(
            !dsp_rise_fades(None, &off),
            "the first load plays what was saved"
        );
        assert!(
            !dsp_rise_fades(Some(&off), &off),
            "a new song, same settings"
        );
        assert!(
            !dsp_rise_fades(Some(&off), &on),
            "a clip-safe profile is never louder"
        );
        assert!(
            dsp_rise_fades(Some(&on), &off),
            "profile off: its preamp comes back"
        );
        let bass = vec![EqBand {
            frequency_hz: 60.,
            gain_db: 0.5,
            q: 1.,
            kind: EqKind::Peak,
        }];
        let small = with(ReplayGainMode::Off, Some((0.0, bass.clone())));
        assert!(!dsp_rise_fades(Some(&off), &small), "below 1 dB");
        let mut big = bass;
        big[0].gain_db = 6.0;
        assert!(dsp_rise_fades(
            Some(&small),
            &with(ReplayGainMode::Off, Some((0.0, big)))
        ));
        // Normalize's gain depends on each song: any change counts.
        assert!(dsp_rise_fades(
            Some(&off),
            &with(ReplayGainMode::Track, None)
        ));
        assert!(dsp_rise_fades(
            Some(&with(ReplayGainMode::Album, None)),
            &off
        ));
    }
}

#[cfg(test)]
mod eq_profile_file_tests {
    use super::{import_eq_profile, scan_eq_profiles};
    use std::fs;

    #[test]
    fn eq_folders_on_every_storage_are_listed_and_files_checked_on_import() {
        let root = std::env::temp_dir().join(format!("reborn-eq-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (internal, sd) = (root.join("internal"), root.join("sd"));
        fs::create_dir_all(internal.join("EQ")).unwrap();
        fs::create_dir_all(sd.join("eq")).unwrap();
        fs::create_dir_all(internal.join("Music")).unwrap();
        let good = "Preamp: -3.2 dB\nFilter 1: ON LSC Fc 105 Hz Gain 3 dB Q 0.7\nFilter 2: ON PK Fc 2000 Hz Gain -2 dB Q 2\n";
        fs::write(internal.join("EQ/Sennheiser HD 650 ParametricEQ.txt"), good).unwrap();
        fs::write(sd.join("eq/aria.TXT"), "GraphicEQ: 20 0\n").unwrap();
        fs::write(internal.join("EQ/.hidden.txt"), good).unwrap();
        fs::write(internal.join("EQ/readme.pdf"), "x").unwrap();
        fs::write(internal.join("Music/elsewhere.txt"), good).unwrap();
        fs::write(sd.join("eq/huge.txt"), "#".repeat(70 * 1024)).unwrap();
        // Shipped: one new model and one also on the storage (the storage wins).
        let shipped = root.join("shipped");
        fs::create_dir_all(&shipped).unwrap();
        fs::write(shipped.join("AKG K371 ParametricEQ.txt"), good).unwrap();
        fs::write(shipped.join("Sennheiser HD 650 ParametricEQ.txt"), good).unwrap();
        fs::write(shipped.join("SOURCES.md"), "x").unwrap();
        fs::write(shipped.join("LICENSE-AutoEq.txt"), "MIT License").unwrap();
        let files = scan_eq_profiles(
            std::slice::from_ref(&shipped),
            &[internal.clone(), sd.clone(), root.join("missing")],
        );
        let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["AKG K371", "aria", "huge", "Sennheiser HD 650"]);
        assert!(files[3].path.starts_with(&internal));
        let files: Vec<_> = files.into_iter().skip(1).collect();
        let profile = import_eq_profile(&files[2].path).unwrap();
        assert_eq!(profile.name, "Sennheiser HD 650");
        assert_eq!(profile.bands.len(), 2);
        assert!(profile.preamp_db <= -3.2);
        assert!(import_eq_profile(&files[0].path)
            .unwrap_err()
            .contains("Line 1"));
        assert!(import_eq_profile(&files[1].path)
            .unwrap_err()
            .contains("too large"));
        assert!(import_eq_profile(&root.join("gone.txt"))
            .unwrap_err()
            .contains("gone"));
        let _ = fs::remove_dir_all(&root);
    }

    /// The real shipped folder (installed as is to /usr/share/reborn/eq)
    /// lists its 47 profiles and nothing else: not the license, not SOURCES.md.
    #[test]
    fn the_shipped_folder_lists_exactly_its_profiles() {
        let shipped = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/eq");
        assert!(shipped.join("LICENSE-AutoEq.txt").is_file());
        let files = scan_eq_profiles(&[shipped], &[]);
        assert_eq!(files.len(), 47);
        assert!(files
            .iter()
            .all(|f| !f.name.to_lowercase().contains("license")));
        assert!(files.iter().all(|f| import_eq_profile(&f.path).is_ok()));
    }
}

#[cfg(test)]
mod update_health_tests {
    use super::update_health;
    use serde_json::json;

    #[test]
    fn an_incomplete_scan_is_healthy_and_a_failed_database_is_not() {
        let scanner = json!({"subsystems":{"scanner":{"state":"degraded","required":true,"detail":"scan incomplete"}}});
        assert_eq!(update_health(false, true, 30, &scanner), (true, vec![]));
        let database =
            json!({"subsystems":{"database":{"state":"failed","required":true,"detail":"x"}}});
        assert_eq!(
            update_health(false, true, 30, &database).1,
            vec!["database failed"]
        );
        let stuck = json!({"subsystems":{"worker_scanner":{"state":"failed","required":true,"detail":"x"}}});
        assert!(!update_health(false, true, 30, &stuck).0);
        assert_eq!(
            update_health(false, false, 5, &json!({})).1,
            vec!["graphics unavailable", "starting"]
        );
        assert!(update_health(true, false, 30, &json!({})).0);
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
    fn a_start_refused_for_a_missing_peer_counts_as_a_disconnect() {
        let output = AudioOutput::Bluetooth("01:02:03:04:05:06".into());
        let refused = "connected A2DP peer required";
        let mut gone = status(10);
        gone.available = true;
        gone.devices[0].connected = false;
        assert!(super::selected_peer_gone(&output, &gone, refused));
        // Still connected (audio not ready yet), another error, the jack, or no
        // observation at all: not a disconnect.
        let mut connected = status(10);
        connected.available = true;
        assert!(!super::selected_peer_gone(&output, &connected, refused));
        assert!(!super::selected_peer_gone(
            &output,
            &gone,
            "BlueALSA unavailable"
        ));
        assert!(!super::selected_peer_gone(
            &AudioOutput::Wired,
            &gone,
            refused
        ));
        gone.available = false;
        assert!(!super::selected_peer_gone(&output, &gone, refused));
        assert!(super::friendly_error(refused).starts_with("Bluetooth headphones aren't connected"));
    }

    #[test]
    fn a_song_on_a_refused_card_does_not_ask_for_a_reinsert() {
        use reborn_core::platform::SdCard;
        let missing = "track source unavailable";
        assert!(super::refused_card_error(missing, SdCard::NeedsRepair)
            .is_some_and(|m| m.contains("needs repair")));
        assert!(super::refused_card_error(missing, SdCard::CheckTimedOut)
            .is_some_and(|m| m.contains("too long")));
        assert_eq!(super::refused_card_error(missing, SdCard::Absent), None);
        assert_eq!(
            super::refused_card_error("queue is empty", SdCard::NeedsRepair),
            None
        );
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
    use super::{failed_reconfiguration_model, keeps_wired_sink, run_sink_reconfiguration};
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
                hp_level_live: None,
                level_raised: false,
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
            speaker: false,
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
            |_| None,
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
            |_| None,
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
                    fade_in: false,
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
    fn a_kept_wired_sink_is_taken_only_where_the_plan_would_give_it() {
        let kept = test_spec();
        let wired = AudioOutput::Wired;
        assert!(keeps_wired_sink(&kept, &wired));
        // A sink some other plan gave (not the 44.1 kHz all wired plans give).
        let mut other = test_spec();
        other.rate = 48_000;
        assert!(!keeps_wired_sink(&other, &wired));
        let bluetooth = AudioOutput::Bluetooth("12:34:56:78:90:AB".into());
        assert!(!keeps_wired_sink(&kept, &bluetooth));
    }

    #[test]
    fn a_track_change_for_the_same_wired_sink_starts_in_it_without_a_plan() {
        let root = std::env::temp_dir().join(format!("reborn-plan-kept-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let opens = Arc::new(AtomicUsize::new(0));
        let (factory_active, factory_opens) = (active.clone(), opens.clone());
        let mut playback = Playback::with_sink(
            log,
            root,
            Box::new(move |_, _, _| {
                factory_opens.fetch_add(1, Ordering::AcqRel);
                factory_active.fetch_add(1, Ordering::AcqRel);
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
        let track = Track {
            id: 3,
            source_id: "internal".into(),
            path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac"),
            ..Default::default()
        };
        run_sink_reconfiguration(
            &mut playback,
            2,
            |kept| Some(kept.clone()),
            || Err::<SinkSpec, _>("the device is open: no probe".into()),
            move |playback, spec| {
                assert_eq!(spec, test_spec());
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
                    fade_in: false,
                })
            },
        )
        .unwrap();

        assert_eq!(opens.load(Ordering::Acquire), 1);
        assert_eq!(active.load(Ordering::Acquire), 1);
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
        // Y2Reborn #20: the saved network whose password failed is named; a
        // join already removed again is not a problem.
        s.saved = vec![wifi::Network {
            ssid: "Home".into(),
            saved_id: Some(4),
            ..Default::default()
        }];
        s.failed_network_id = Some(4);
        assert_eq!(wifi_view(&s).problem_network.as_deref(), Some("Home"));
        s.failed_network_id = Some(9);
        assert_eq!(wifi_view(&s).problem, None);
        s.failed_network_id = None;
        // Service text can never reach the screen: problems are an enum.
        // Only the platform's exact credential reasons mean a wrong password
        // (F52: "not_authenticated" after a wake read as one).
        s.readiness_reason = Some("org.secret.service credential=abc".into());
        assert_eq!(
            wifi_view(&s).problem,
            Some(reborn_core::platform::WifiProblem::Other)
        );
        s.readiness_reason = Some("not_authenticated".into());
        assert_ne!(
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
    fn play_waits_briefly_for_bluetooth_headphones_just_lost() {
        let t0 = Instant::now();
        let mut lost = LostPeer::new("AA:BB".into(), t0);
        let wired = AudioOutput::Wired;
        // Jack empty or not read yet: wait. Headphones in the jack: play there.
        assert!(lost.applies(&wired, Some(false), t0));
        assert!(lost.applies(&wired, None, t0));
        assert!(!lost.applies(&wired, Some(true), t0));
        // The headphones are the output again: nothing to wait for.
        assert!(!lost.applies(&AudioOutput::Bluetooth("AA:BB".into()), Some(false), t0));
        assert!(lost.is(&AudioOutput::Bluetooth("aa:bb".into())));
        assert!(!lost.is(&AudioOutput::Bluetooth("CC:DD".into())));
        // Long after the loss (or the wake after it) the jack plays again.
        let late = t0 + LOST_PEER_WINDOW;
        assert!(lost.expired(late));
        assert!(!lost.applies(&wired, Some(false), late));
        // A Play waits its own time, also past the window, then gives up.
        lost.play = Some(t0 + Duration::from_secs(110));
        assert!(!lost.expired(late));
        assert!(lost.applies(&wired, Some(false), late));
        let after = t0 + Duration::from_secs(110) + LOST_PEER_PLAY_WAIT;
        assert!(lost.gave_up(after));
        assert!(!lost.applies(&wired, Some(false), after));
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
            Command::SleepRestore { dark: false },
            Command::SleepRestore { dark: true },
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
#[cfg(test)]
mod shuffle_and_seek_tests {
    use super::{seek_step, shuffle_from};
    use std::time::Duration;

    #[test]
    fn shuffles_differ_by_seed_and_keep_every_song() {
        let orders: Vec<Vec<u32>> = (1..=8u64)
            .map(|seed| {
                let mut v: Vec<u32> = (0..40).collect();
                shuffle_from(&mut v, 0, seed.wrapping_mul(0x1234_5678_9abc_def1));
                v
            })
            .collect();
        for order in &orders {
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, (0..40).collect::<Vec<_>>());
        }
        for (i, a) in orders.iter().enumerate() {
            for b in &orders[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn a_picked_song_stays_first_and_every_song_can_lead() {
        let mut first = [0u32; 10];
        for seed in 0..10_000u64 {
            let mut kept: Vec<u32> = (0..10).collect();
            shuffle_from(&mut kept, 1, seed);
            assert_eq!(kept[0], 0);
            let mut all: Vec<u32> = (0..10).collect();
            shuffle_from(&mut all, 0, seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            first[all[0] as usize] += 1;
        }
        // About 1000 each; a seed-derived order once always led with the same song.
        assert!(first.iter().all(|&n| (800..1200).contains(&n)), "{first:?}");
    }

    #[test]
    fn held_seek_starts_small_and_grows_within_the_song() {
        let song = 4 * 60_000;
        assert_eq!(seek_step(Duration::ZERO, song), 1_000);
        assert_eq!(seek_step(Duration::from_secs(2), song), 3_000);
        assert_eq!(seek_step(Duration::from_secs(5), song), 4_000); // 1/60 of 4 min
        assert_eq!(seek_step(Duration::from_secs(5), 60 * 60_000), 10_000);
        assert_eq!(seek_step(Duration::from_secs(5), 30_000), 1_000);
    }
}

#[cfg(test)]
mod update_screen_tests {
    use super::update_screen_waits;
    use reborn_core::PlatformTask::{Refresh, ReportSend, UpdateApply, UpdateStage};

    #[test]
    fn the_update_screen_never_stands_without_an_update_task() {
        // Update Now while a status observation ran: the stage task was
        // dropped and the screen stood at step 1 until a restart.
        assert!(update_screen_waits(true, false, None, None));
        assert!(update_screen_waits(true, false, Some(Refresh), None));
        assert!(update_screen_waits(true, false, Some(ReportSend), None));
        // Queued behind another task, running, or restarting to install: fine.
        assert!(!update_screen_waits(
            true,
            false,
            Some(Refresh),
            Some(UpdateStage)
        ));
        assert!(!update_screen_waits(true, false, Some(UpdateStage), None));
        assert!(!update_screen_waits(true, false, Some(UpdateApply), None));
        assert!(!update_screen_waits(true, true, None, None));
        assert!(!update_screen_waits(false, false, None, None));
    }
}
