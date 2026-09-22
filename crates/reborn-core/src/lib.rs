#![forbid(unsafe_code)]
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-premium.01");
pub const MAX_QUEUE: usize = 20_000;
pub const SESSION_SCHEMA_VERSION: u32 = 2;
pub const SESSION_MAX_BYTES: usize = 8 * 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RadioScan {
    #[default]
    Idle,
    Starting,
    Scanning,
    Complete {
        found: usize,
    },
    Failed {
        message: String,
    },
}
impl RadioScan {
    pub fn active(&self) -> bool {
        matches!(self, Self::Starting | Self::Scanning)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackState {
    #[default]
    Stopped,
    Buffering,
    Playing,
    Paused,
    Error,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", content = "device", rename_all = "snake_case")]
pub enum AudioOutput {
    #[default]
    Wired,
    Bluetooth(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PcmFormat {
    #[serde(rename = "S16_LE")]
    S16LE,
    /// Signed 24-bit PCM in the low three bytes of a 32-bit little-endian
    /// container, matching ALSA `SND_PCM_FORMAT_S24_LE` (not packed S24_3LE).
    #[serde(rename = "S24_LE")]
    S24LE,
    #[default]
    #[serde(rename = "S32_LE")]
    S32LE,
}
impl PcmFormat {
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::S16LE => 2,
            Self::S24LE => 4,
            Self::S32LE => 4,
        }
    }
    pub const fn bytes_per_frame(self) -> usize {
        self.bytes_per_sample() * 2
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::S16LE => "S16_LE",
            Self::S24LE => "S24_LE",
            Self::S32LE => "S32_LE",
        }
    }
    pub const fn physical_bits(self) -> u8 {
        match self {
            Self::S16LE => 16,
            Self::S24LE | Self::S32LE => 32,
        }
    }
    pub const fn valid_bits(self) -> u8 {
        match self {
            Self::S16LE => 16,
            Self::S24LE => 24,
            Self::S32LE => 32,
        }
    }
}

/// A read-only observation of one BlueALSA PCM object. The format and rate
/// come from the negotiated PCM object, while `codec` is only descriptive.
/// Keeping the observation typed prevents a requested codec from being
/// mistaken for the transport that is actually active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BluetoothPcm {
    pub object: String,
    pub device: String,
    pub transport: String,
    pub mode: String,
    pub codec: Option<String>,
    pub format: Option<u16>,
    pub rate: Option<u32>,
    pub channels: Option<u8>,
    pub running: Option<bool>,
    pub transport_generation: u64,
}
impl BluetoothPcm {
    pub fn is_a2dp_playback_for(&self, device: &str) -> bool {
        self.device == device && self.mode == "sink" && self.transport == "A2DP-source"
    }

    pub fn negotiated_format(&self) -> Result<PcmFormat, String> {
        match self.format {
            Some(0x8210) => Ok(PcmFormat::S16LE),
            Some(0x8418) => Ok(PcmFormat::S24LE),
            Some(0x8420) => Ok(PcmFormat::S32LE),
            Some(0x8318) => {
                Err("packed 24-bit Bluetooth PCM is not supported by the sink membrane".into())
            }
            Some(value) => Err(format!("unsupported BlueALSA PCM format 0x{value:04x}")),
            None => Err("BlueALSA PCM format is not observed yet".into()),
        }
    }

    pub fn negotiated_rate(&self) -> Result<u32, String> {
        match self.rate {
            Some(rate @ (44_100 | 48_000)) => Ok(rate),
            Some(rate) => Err(format!(
                "Bluetooth PCM rate {rate} Hz is outside the qualified baseline"
            )),
            None => Err("BlueALSA PCM rate is not observed yet".into()),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReplayGainMode {
    #[default]
    Off,
    Track,
    Album,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EqBand {
    pub frequency_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}
impl Default for EqBand {
    fn default() -> Self {
        Self {
            frequency_hz: 1000.0,
            gain_db: 0.0,
            q: 1.0,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum MediaSource {
    Internal,
    SdCard(String),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    pub kind: MediaSource,
    pub root: PathBuf,
    pub online: bool,
    pub mount: String,
    #[serde(default)]
    pub mount_id: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Track {
    pub id: i64,
    pub source_id: String,
    pub path: PathBuf,
    pub filename: String,
    pub size: u64,
    pub mtime: i64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub track: u32,
    pub disc: u32,
    pub duration_ms: u64,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub bitrate: u64,
    pub artwork: bool,
    pub online: bool,
}

/* A queue occurrence is a playback identity, not a library identity. Two
 * occurrences may reference the same Track, so boundary and artwork updates
 * must be able to address the occurrence that was scheduled. */
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QueueEntryId(pub u64);
/// Semantic actions are the only input vocabulary exposed to the product/UI
/// layer. Linux event types and key codes stop at `reborn-platform::input`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    NavigateUp,
    NavigateDown,
    NavigateLeft,
    NavigateRight,
    Select,
    Back,
    Home,
    ContextMenu,
    PlayPause,
    ShowNowPlaying,
    PreviousTrack,
    NextTrack,
    SeekBackward,
    SeekForward,
    VolumeUp,
    VolumeDown,
    WheelClockwise(u8),
    WheelCounterClockwise(u8),
    ScreenWake,
    ScreenSleep,
    PowerMenu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhysicalControl {
    Select,
    Back,
    Previous,
    Next,
    PlayPause,
    Power,
    VolumeUp,
    VolumeDown,
    WheelClockwise,
    WheelCounterClockwise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizedInput {
    Press(PhysicalControl),
    Release(PhysicalControl),
    Cancel(PhysicalControl),
    LongPress(PhysicalControl),
    Repeat(PhysicalControl),
    WheelClockwise(u8),
    WheelCounterClockwise(u8),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    #[default]
    #[serde(alias = "main")]
    Home,
    Music,
    Artists,
    Albums,
    Tracks,
    Folders,
    NowPlaying,
    Queue,
    Connectivity,
    Bluetooth,
    Wifi,
    Settings,
    SettingsAudio,
    SettingsPlayback,
    SettingsLibrary,
    SettingsBluetooth,
    SettingsWifi,
    SettingsDisplay,
    SettingsPower,
    SettingsSystem,
    Diagnostics,
    Album,
    Artist,
    TextEntry,
    Pairing,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NavigationState {
    /// Parent routes only. The current route is authoritative in AppModel::screen.
    pub stack: Vec<Screen>,
    pub history: Vec<NavigationFrame>,
    pub focus: usize,
    pub scroll: usize,
    pub filter: String,
    pub modal: Option<Modal>,
    pub modal_focus: usize,
    pub context_target: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationFrame {
    pub screen: Screen,
    pub focus: usize,
    pub scroll: usize,
    pub filter: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    ContextMenu,
    PowerMenu,
    Confirm(ConfirmAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    ClearQueue,
    RebuildLibrary,
    ForgetBluetooth,
    ForgetWifi,
    PowerOff,
    Reboot,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LibraryState {
    #[serde(skip)]
    pub tracks: Vec<Track>,
    pub scanning: bool,
    pub last_scan: Option<ScanSummary>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScanSummary {
    pub discovered: u64,
    pub reused: u64,
    pub elapsed_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub volume: u8,
    pub screen_timeout_seconds: u32,
    pub music_directory: PathBuf,
    #[serde(default)]
    pub replay_gain: ReplayGainMode,
    #[serde(default)]
    pub eq_enabled: bool,
    #[serde(default)]
    pub eq_bands: Vec<EqBand>,
    #[serde(default)]
    pub crossfade_ms: u32,
    #[serde(default = "default_true")]
    pub gapless_enabled: bool,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default)]
    pub repeat: RepeatMode,
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    #[default]
    Off,
    Track,
    All,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 35,
            screen_timeout_seconds: 60,
            music_directory: "/data/music".into(),
            replay_gain: ReplayGainMode::Off,
            eq_enabled: false,
            eq_bands: Vec::new(),
            crossfade_ms: 0,
            gapless_enabled: true,
            shuffle: false,
            repeat: RepeatMode::Off,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppModel {
    pub playback: PlaybackState,
    pub queue: Vec<Track>,
    #[serde(default)]
    pub queue_entry_ids: Vec<QueueEntryId>,
    pub queue_position: usize,
    pub position_ms: u64,
    pub output: AudioOutput,
    pub screen: Screen,
    pub settings: Settings,
    pub generation: u64,
    pub sources: Vec<Source>,
    pub screen_off: bool,
    pub last_error: Option<String>,
    #[serde(default)]
    pub library: LibraryState,
    #[serde(skip)]
    pub navigation: NavigationState,
}

/// Effects are requests to the application/service boundary. They carry no
/// platform handles and can be asserted in host tests without hardware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Play(usize),
    PlayShuffled(usize),
    PlayCollection {
        selected: usize,
        members: Vec<usize>,
        shuffle: bool,
    },
    PlayQueue(usize),
    PlayNext(usize),
    PlayNextCollection(Vec<usize>),
    AddToQueue(usize),
    AddToQueueCollection(Vec<usize>),
    TogglePlayback,
    NextTrack,
    PreviousTrack,
    Seek(i64),
    AdjustVolume(i8),
    ScanLibrary,
    WifiPower,
    WifiScan,
    WifiConnect {
        ssid: String,
        password: String,
    },
    WifiSaved(u32),
    WifiForget(u32),
    BluetoothPower,
    BluetoothScan,
    BluetoothDevice {
        path: String,
        operation: String,
    },
    Output(AudioOutput),
    ConfirmPairing(bool),
    ScreenSleep,
    ScreenWake,
    SetReplayGain(ReplayGainMode),
    ToggleEq,
    SetCrossfade(u32),
    SetGapless(bool),
    SetShuffle(bool),
    SetRepeat(RepeatMode),
    SetScreenTimeout(u32),
    QueueRemove(usize),
    QueueMove {
        index: usize,
        delta: i8,
    },
    ClearQueue,
    RebuildLibrary,
    PowerOff,
    Reboot,
    Checkpoint,
}
#[derive(Debug, Clone)]
pub enum PlaybackCommand {
    Load {
        track: Box<Track>,
        position_ms: u64,
        output: AudioOutput,
        generation: u64,
        correlation_id: u64,
        paused: bool,
    },
    Pause,
    Resume,
    Stop {
        generation: u64,
    },
    Volume(u8),
    Shutdown,
}
#[derive(Debug, Clone)]
pub enum Event {
    TrackStarted {
        generation: u64,
    },
    TrackBoundary {
        generation: u64,
        next_entry_id: Option<QueueEntryId>,
        next_track_id: Option<i64>,
        output_position_ms: u64,
    },
    TrackEnded {
        generation: u64,
    },
    Position {
        generation: u64,
        ms: u64,
    },
    PlaybackError {
        generation: u64,
        message: String,
    },
    Input(Action),
    SourceChanged(Vec<Source>),
    BluetoothDisconnected(String),
    WifiConnected,
    WifiDisconnected,
    PowerChanged,
    LibraryScanStarted,
    LibraryScanFinished(ScanSummary),
    LibraryScanFailed(String),
    ScreenSleep,
    ScreenWake,
}
impl AppModel {
    pub fn current(&self) -> Option<&Track> {
        self.queue.get(self.queue_position)
    }
    pub fn replace_queue(&mut self, tracks: Vec<Track>, index: usize) -> Result<(), String> {
        if tracks.is_empty() || tracks.len() > MAX_QUEUE || index >= tracks.len() {
            return Err("invalid queue bounds".into());
        }
        self.queue = tracks;
        self.queue_entry_ids = (1..=self.queue.len())
            .map(|id| QueueEntryId(id as u64))
            .collect();
        self.queue_position = index;
        self.position_ms = 0;
        self.invalidate();
        Ok(())
    }
    pub fn current_entry_id(&self) -> Option<QueueEntryId> {
        self.queue_entry_ids.get(self.queue_position).copied()
    }
    pub fn insert_queue_entry(
        &mut self,
        index: usize,
        track: Track,
    ) -> Result<QueueEntryId, String> {
        if self.queue.len() >= MAX_QUEUE || index > self.queue.len() {
            return Err("queue is full or insertion index is invalid".into());
        }
        let mut raw = self
            .queue_entry_ids
            .iter()
            .map(|id| id.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        if raw == 0 {
            raw = 1;
        }
        while self.queue_entry_ids.iter().any(|id| id.0 == raw) {
            raw = raw.saturating_add(1);
            if raw == 0 {
                return Err("queue entry identity exhausted".into());
            }
        }
        let id = QueueEntryId(raw);
        self.queue.insert(index, track);
        self.queue_entry_ids.insert(index, id);
        if index <= self.queue_position && self.queue.len() > 1 {
            self.queue_position += 1;
        }
        Ok(id)
    }
    pub fn remove_queue_entry(&mut self, index: usize) -> Option<Track> {
        if index >= self.queue.len() || index == self.queue_position {
            return None;
        }
        self.queue_entry_ids.remove(index);
        let track = self.queue.remove(index);
        if index < self.queue_position {
            self.queue_position = self.queue_position.saturating_sub(1);
        }
        Some(track)
    }
    pub fn move_queue_entry(&mut self, index: usize, next: usize) -> bool {
        if index >= self.queue.len()
            || next >= self.queue.len()
            || index == self.queue_position
            || next == self.queue_position
        {
            return false;
        }
        self.queue.swap(index, next);
        self.queue_entry_ids.swap(index, next);
        true
    }
    pub fn clear_future_queue(&mut self) -> bool {
        let end = self.queue_position.saturating_add(1);
        if end >= self.queue.len() {
            return false;
        }
        self.queue.truncate(end);
        self.queue_entry_ids.truncate(end);
        true
    }
    pub fn invalidate(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }
    pub fn step(&mut self, delta: i32) -> bool {
        let n = self.queue_position as i64 + delta as i64;
        if n < 0 || n >= self.queue.len() as i64 {
            self.playback = PlaybackState::Stopped;
            return false;
        }
        self.queue_position = n as usize;
        self.position_ms = 0;
        self.invalidate();
        true
    }
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::TrackStarted { generation } if generation == self.generation => {
                self.playback = PlaybackState::Playing
            }
            Event::TrackEnded { generation } if generation == self.generation => {
                self.playback = PlaybackState::Stopped
            }
            Event::TrackBoundary {
                generation,
                next_entry_id,
                next_track_id,
                output_position_ms,
            } if generation == self.generation => {
                let next_position = next_entry_id
                    .and_then(|id| self.queue_entry_ids.iter().position(|entry| *entry == id))
                    .or_else(|| {
                        next_track_id
                            .and_then(|id| self.queue.iter().position(|track| track.id == id))
                    })
                    .filter(|position| *position > self.queue_position);
                if let Some(next_position) = next_position {
                    self.queue_position = next_position;
                    self.position_ms = output_position_ms;
                    self.playback = PlaybackState::Buffering;
                }
            }
            Event::Position { generation, ms } if generation == self.generation => {
                self.position_ms = ms
            }
            Event::PlaybackError {
                generation,
                message,
            } if generation == self.generation => {
                self.playback = PlaybackState::Error;
                self.last_error = Some(message)
            }
            Event::SourceChanged(sources) => {
                self.sources = sources;
                if self
                    .current()
                    .is_some_and(|t| !self.sources.iter().any(|s| s.online && s.id == t.source_id))
                {
                    self.playback = PlaybackState::Paused;
                    self.invalidate();
                }
            }
            Event::BluetoothDisconnected(id)
                if self.output == AudioOutput::Bluetooth(id.clone()) =>
            {
                self.playback = PlaybackState::Paused;
                self.output = AudioOutput::Wired;
                self.invalidate();
            }
            Event::LibraryScanStarted => {
                self.library.scanning = true;
                self.library.error = None;
            }
            Event::LibraryScanFinished(summary) => {
                self.library.scanning = false;
                self.library.last_scan = Some(summary);
                self.library.error = None;
            }
            Event::LibraryScanFailed(message) => {
                self.library.scanning = false;
                self.library.error = Some(message);
            }
            Event::ScreenSleep => self.screen_off = true,
            Event::ScreenWake => self.screen_off = false,
            _ => {}
        }
    }
    pub fn restore(path: &Path) -> io::Result<Self> {
        use std::io::Read;
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(SESSION_MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > SESSION_MAX_BYTES {
            return Err(io::Error::other("state too large"));
        }
        #[derive(Deserialize)]
        struct PersistedSession {
            schema: u32,
            model: AppModel,
        }
        let mut m = match serde_json::from_slice::<PersistedSession>(&bytes) {
            Ok(session) => {
                if session.schema > SESSION_SCHEMA_VERSION {
                    return Err(io::Error::other("state schema is newer than Reborn"));
                }
                session.model
            }
            Err(_) => serde_json::from_slice::<Self>(&bytes)?,
        };
        if m.queue.len() > MAX_QUEUE || (!m.queue.is_empty() && m.queue_position >= m.queue.len()) {
            return Err(io::Error::other("invalid saved queue"));
        }
        let ids_valid = m.queue_entry_ids.len() == m.queue.len()
            && m.queue_entry_ids.iter().all(|id| id.0 != 0)
            && m.queue_entry_ids.iter().enumerate().all(|(index, id)| {
                m.queue_entry_ids[..index]
                    .iter()
                    .all(|previous| previous != id)
            });
        if !ids_valid {
            m.queue_entry_ids = (1..=m.queue.len())
                .map(|id| QueueEntryId(id as u64))
                .collect();
        }
        m.playback = if m.current().is_some() {
            PlaybackState::Paused
        } else {
            PlaybackState::Stopped
        };
        m.screen_off = false;
        m.generation = 0;
        m.navigation = NavigationState::default();
        m.settings.volume = m.settings.volume.min(100);
        m.settings.crossfade_ms = m.settings.crossfade_ms.min(30_000);
        m.settings.eq_bands.truncate(8);
        Ok(m)
    }
    pub fn checkpoint(&self, path: &Path) -> io::Result<()> {
        #[derive(Serialize)]
        struct PersistedSession<'a> {
            schema: u32,
            model: &'a AppModel,
        }
        let bytes = serde_json::to_vec(&PersistedSession {
            schema: SESSION_SCHEMA_VERSION,
            model: self,
        })?;
        if bytes.len() > SESSION_MAX_BYTES {
            return Err(io::Error::other("state exceeds bounded write size"));
        }
        atomic_write(path, &bytes)
    }
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
    fs::create_dir_all(parent)?;
    let temp = path.with_extension("new");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(temp, path)?;
    fs::File::open(parent)?.sync_all()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn s24_le_has_24_valid_bits_in_a_four_byte_stereo_frame() {
        assert_eq!(PcmFormat::S24LE.bytes_per_sample(), 4);
        assert_eq!(PcmFormat::S24LE.bytes_per_frame(), 8);
        assert_eq!(PcmFormat::S24LE.physical_bits(), 32);
        assert_eq!(PcmFormat::S24LE.valid_bits(), 24);
    }
    #[test]
    fn stale_events_cannot_restart_after_stop() {
        let mut m = AppModel::default();
        m.invalidate();
        m.apply(Event::TrackStarted { generation: 0 });
        assert_eq!(m.playback, PlaybackState::Stopped)
    }
    #[test]
    fn queue_boundaries() {
        let mut m = AppModel::default();
        assert!(m.replace_queue(vec![], 0).is_err());
        m.replace_queue(vec![Track::default()], 0).unwrap();
        assert!(!m.step(1));
        assert_eq!(m.queue_position, 0)
    }
    #[test]
    fn duplicate_tracks_keep_distinct_queue_identity() {
        let mut m = AppModel::default();
        m.replace_queue(
            vec![
                Track {
                    id: 7,
                    ..Default::default()
                },
                Track {
                    id: 7,
                    ..Default::default()
                },
            ],
            0,
        )
        .unwrap();
        assert_ne!(m.queue_entry_ids[0], m.queue_entry_ids[1]);
        let generation = m.generation;
        m.apply(Event::TrackBoundary {
            generation,
            next_entry_id: Some(m.queue_entry_ids[1]),
            next_track_id: Some(7),
            output_position_ms: 0,
        });
        assert_eq!(m.queue_position, 1);
    }
    #[test]
    fn session_checkpoint_has_explicit_schema_and_matching_bounds() {
        let path = std::env::temp_dir().join(format!("reborn-session-{}.json", std::process::id()));
        let mut model = AppModel::default();
        model
            .replace_queue(vec![Track::default()], 0)
            .expect("valid queue");
        model.checkpoint(&path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["schema"], SESSION_SCHEMA_VERSION);
        assert!(fs::metadata(&path).unwrap().len() <= SESSION_MAX_BYTES as u64);
        assert!(AppModel::restore(&path).unwrap().current().is_some());
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn bluetooth_loss_pauses_only_selected_peer() {
        let mut m = AppModel {
            output: AudioOutput::Bluetooth("a".into()),
            playback: PlaybackState::Playing,
            ..Default::default()
        };
        m.apply(Event::BluetoothDisconnected("b".into()));
        assert_eq!(m.playback, PlaybackState::Playing);
        m.apply(Event::BluetoothDisconnected("a".into()));
        assert_eq!(m.playback, PlaybackState::Paused)
    }
    #[test]
    fn source_loss_invalidates_pcm() {
        let mut m = AppModel::default();
        m.replace_queue(
            vec![Track {
                source_id: "sd".into(),
                ..Default::default()
            }],
            0,
        )
        .unwrap();
        let g = m.generation;
        m.apply(Event::SourceChanged(vec![]));
        assert!(m.generation > g);
        assert_eq!(m.playback, PlaybackState::Paused)
    }

    #[test]
    fn background_scan_and_display_events_update_authoritative_state() {
        let mut m = AppModel::default();
        m.apply(Event::LibraryScanStarted);
        assert!(m.library.scanning);
        m.apply(Event::LibraryScanFinished(ScanSummary {
            discovered: 12,
            reused: 4,
            elapsed_ms: 50,
        }));
        assert!(!m.library.scanning);
        assert_eq!(m.library.last_scan.as_ref().map(|s| s.discovered), Some(12));
        m.apply(Event::ScreenSleep);
        assert!(m.screen_off);
        m.apply(Event::ScreenWake);
        assert!(!m.screen_off);
    }
}
