#![forbid(unsafe_code)]
pub mod clock;
mod countries;
pub mod platform;
pub mod playlist;
pub mod sort;
mod zones;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CodecPreference {
    /// The best codec both ends support, from the platform's ranking.
    #[default]
    Auto,
    #[serde(rename = "SBC")]
    Sbc,
    #[serde(rename = "SBC-XQ")]
    SbcXq,
    #[serde(rename = "AAC")]
    Aac,
    #[serde(rename = "aptX")]
    Aptx,
    #[serde(rename = "aptX-HD")]
    AptxHd,
    #[serde(rename = "LDAC")]
    Ldac,
}
impl CodecPreference {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Sbc => "SBC",
            Self::SbcXq => "SBC-XQ",
            Self::Aac => "AAC",
            Self::Aptx => "aptX",
            Self::AptxHd => "aptX-HD",
            Self::Ldac => "LDAC",
        }
    }
}

/// The product version shown to people.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The build identity used by logs, the control socket and Diagnostics.
pub const BUILD_LABEL: &str = concat!(env!("CARGO_PKG_VERSION"), "-product-ui-v3");

/// A platform release string reduced to its version number, e.g.
/// `1.0.0-cpu-final-fix03-candidate.1` → `1.0.0`.
pub fn display_release(release: &str) -> &str {
    release.split('-').next().unwrap_or(release)
}
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

/// Five broad bands for the wheel-driven editor. The existing FFmpeg EQ and
/// limiter perform the processing; these are product defaults, not a new DSP.
pub const EQ_FREQUENCIES: [u32; 5] = [60, 250, 1_000, 4_000, 12_000];
pub fn flat_eq_bands() -> Vec<EqBand> {
    EQ_FREQUENCIES
        .iter()
        .map(|hz| EqBand {
            frequency_hz: *hz as f32,
            gain_db: 0.,
            q: 1.,
        })
        .collect()
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
    /// Library row id; radio stations use negative ids (never in the library).
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
    /// Genre tag as read from the file; empty when the file has none.
    #[serde(default)]
    pub genre: String,
}

/// `Track::source_id` of an internet radio station in the queue.
pub const RADIO_SOURCE: &str = "radio";
impl Track {
    /// An internet radio station: `path` holds the stream address, `filename`
    /// the directory id (if any), `album` a short description.
    pub fn is_radio(&self) -> bool {
        self.source_id == RADIO_SOURCE
    }
    pub fn radio(station: &RadioStation) -> Self {
        Self {
            id: -1,
            source_id: RADIO_SOURCE.into(),
            path: PathBuf::from(&station.url),
            filename: station.uuid.clone(),
            title: station.name.clone(),
            album: station.summary(),
            codec: station.codec.clone(),
            bitrate: u64::from(station.bitrate) * 1000,
            online: true,
            ..Default::default()
        }
    }
}

/// An internet radio station from the community directory or an owner's
/// own playlist.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RadioStation {
    /// radio-browser station UUID; empty for the owner's own playlists.
    #[serde(default)]
    pub uuid: String,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub codec: String,
    #[serde(default)]
    pub bitrate: u32,
    #[serde(default)]
    pub tags: String,
    /// The station's logo (an http(s) picture address from the directory).
    #[serde(default)]
    pub favicon: String,
    #[serde(default)]
    pub homepage: String,
    /// Languages as the directory names them ("german,english").
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub countrycode: String,
    /// Directory votes and plays (its popularity).
    #[serde(default)]
    pub votes: u32,
    #[serde(default)]
    pub clicks: u32,
}
impl RadioStation {
    /// The station's genres from its tags, capitalised, at most `n`.
    pub fn genres(&self, n: usize) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        for tag in self
            .tags
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            let name = capitalise(tag);
            if !out.iter().any(|o| o.eq_ignore_ascii_case(&name)) {
                out.push(name);
            }
            if out.len() == n {
                break;
            }
        }
        out
    }
    /// "AAC · 128 kbps", "MP3", or empty.
    pub fn format(&self) -> String {
        match (self.codec.is_empty(), self.bitrate) {
            (false, b) if b > 0 => format!("{} · {b} kbps", self.codec),
            (false, _) => self.codec.clone(),
            (true, b) if b > 0 => format!("{b} kbps"),
            _ => String::new(),
        }
    }
    /// Languages, capitalised ("German, English").
    pub fn languages(&self) -> String {
        self.language
            .split(',')
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(3)
            .map(capitalise)
            .collect::<Vec<_>>()
            .join(", ")
    }
    /// The logo's thumbnail identity (never equal to a cover's: top bit set),
    /// or `None` without a logo address.
    pub fn logo_key(&self) -> Option<u64> {
        (!self.favicon.is_empty()).then(|| logo_key(&self.favicon))
    }
    /// One line under the station name: "Germany · MP3 128 kbps · jazz".
    pub fn summary(&self) -> String {
        let mut parts = vec![];
        if !self.country.is_empty() {
            parts.push(self.country.clone());
        }
        match (self.codec.is_empty(), self.bitrate) {
            (false, b) if b > 0 => parts.push(format!("{} {b} kbps", self.codec)),
            (false, _) => parts.push(self.codec.clone()),
            (true, b) if b > 0 => parts.push(format!("{b} kbps")),
            _ => {}
        }
        if let Some(tag) = self.tags.split(',').map(str::trim).find(|t| !t.is_empty()) {
            parts.push(tag.to_owned());
        }
        parts.join(" · ")
    }
    /// Identity for favorites: the directory UUID, else the stream URL.
    pub fn key(&self) -> &str {
        if self.uuid.is_empty() {
            &self.url
        } else {
            &self.uuid
        }
    }
}

/// A logo's identity: FNV-1a of its address with the top bit set, so it never
/// meets a cover's thumbnail key. The player names its cache file by it.
pub fn logo_key(url: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in url.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash | 1 << 63
}

/// "hip hop" → "Hip Hop"; words already in capitals stay ("BBC", "80s").
pub fn capitalise(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the Playlist screens ask the player for; files are read and written
/// off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistRequest {
    /// Find and read every playlist file again.
    Refresh,
    /// A new playlist in the Playlists folder on Internal Storage, holding
    /// these library songs (indices into the library).
    Create {
        name: String,
        tracks: Vec<usize>,
    },
    /// Change a playlist; `create` makes the file first if it is missing
    /// (Favorites).
    Edit {
        path: std::path::PathBuf,
        edit: playlist::PlaylistEdit,
        create: bool,
    },
    Rename {
        path: std::path::PathBuf,
        name: String,
    },
    Duplicate(std::path::PathBuf),
    Delete(std::path::PathBuf),
}

/// What the Radio screens ask the player for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RadioRequest {
    Favorites,
    Own,
    Popular,
    Countries,
    Genres,
    Country {
        code: String,
        name: String,
    },
    Genre(String),
    Search(String),
    /// Play the station at this index of the list on screen.
    Play(usize),
    ToggleFavorite(usize),
    /// Add or remove the station playing now.
    ToggleFavoriteCurrent,
    /// Play, or add or remove, the station Station Info shows.
    PlayStation(Box<RadioStation>),
    ToggleFavoriteStation(Box<RadioStation>),
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
    PowerShort,
    /// The Power button has been held long enough to start the turn-off hold.
    PowerHold,
    /// Power was released (or the press cancelled) after `PowerHold`.
    PowerHoldCancel,
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
/// Every user-facing route. The tree is documented in
/// `docs/ui/REBORN-PRODUCT-NAVIGATION-V2.md`; Diagnostics is the only
/// technical subtree and is reached through Settings → System.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    #[default]
    #[serde(alias = "main")]
    Home,
    Music,
    Artists,
    Albums,
    /// Genres from the files' tags; each opens its albums.
    Genres,
    Tracks,
    Folders,
    Album,
    Artist,
    NowPlaying,
    Queue,
    TrackInfo,
    Settings,
    Wifi,
    Bluetooth,
    PcTransfer,
    SettingsAudio,
    Equalizer,
    SettingsPlayback,
    /// Crossfade length picker.
    Crossfade,
    /// Screen-off delay picker.
    ScreenTimeout,
    /// Skin picker: the built-in look and the installed skins.
    Theme,
    /// Time zone picker (Settings > Display).
    TimeZone,
    /// Which Music lists also stand on Home (Settings > Display).
    HomeMenu,
    SettingsLibrary,
    SettingsDisplay,
    SettingsSystem,
    Battery,
    Sleep,
    Storage,
    Update,
    About,
    Maintenance,
    Diagnostics,
    /// One Diagnostics section; the navigation filter holds its id.
    DiagnosticSection,
    /// A full-length value; the navigation filter holds label and text.
    ValueDetail,
    TextEntry,
    Pairing,
    /// Internet radio: favorites, own stations, the directory, search.
    Radio,
    /// A list of stations (the list's title is in the navigation filter).
    RadioStations,
    /// Countries or genres of the station directory.
    RadioCategories,
    /// Library search results; the navigation filter holds the search text.
    Search,
    /// One station's details (logo, country, language, genres, format).
    RadioInfo,
    /// Every playlist file, Favorites first.
    Playlists,
    /// One playlist; the navigation filter holds its file's path.
    Playlist,
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
    pub context_key: Option<String>,
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
    OutputPicker,
    CodecPicker,
    EqBand(usize),
    Confirm(ConfirmAction),
    /// "Add to Playlist": the playlists, the last used first, and New.
    PlaylistPicker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    ClearQueue,
    RebuildLibrary,
    ForgetBluetooth,
    ForgetWifi,
    PowerOff,
    Reboot,
    ResetSettings,
    ForgetAllWifi,
    ForgetAllBluetooth,
    ClearCache,
    DeletePlaylist,
    Platform(PlatformTask),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformTask {
    Refresh,
    Health,
    UpdateCheck,
    UpdateStage,
    UpdateApply,
    UpdateCancel,
    UpdateRollback,
    Export,
    DiagnosticsExport,
    SleepRequest,
    LdacQuality(platform::LdacQuality),
    LdacAbr(bool),
    SbcQuality(platform::SbcQuality),
    AacQuality(platform::AacQuality),
    AacAbr(bool),
    StorageBenchmark,
    LibraryBenchmark,
    NetworkCheck,
}

/// Presented platform state. Owned and filled by the application from the
/// typed platform client; the UI never parses platform records itself.
#[derive(Debug, Clone, Default)]
pub struct PlatformState {
    pub snapshot: platform::PlatformSnapshot,
    pub battery: platform::BatteryState,
    pub busy: Option<PlatformTask>,
    pub result: Option<platform::OperationResult>,
    /// Friendly text of the last failed explicit operation.
    pub failure: Option<String>,
    /// Application-owned audio pipeline observations (Diagnostics only).
    pub audio_facts: Vec<platform::Fact>,
    /// Bluetooth transport observations (Diagnostics only).
    pub bluetooth_facts: Vec<platform::Fact>,
    /// Whether the panel exposes an adjustable backlight.
    pub brightness_available: bool,
    /// Set while the shutdown/restart transition is being presented.
    pub shutting_down: Option<platform::ShutdownIntent>,
    /// Live progress of a running update download.
    pub download: Option<platform::DownloadLive>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LibraryState {
    #[serde(skip)]
    pub tracks: Vec<Track>,
    #[serde(skip)]
    pub scanning: bool,
    pub last_scan: Option<ScanSummary>,
    #[serde(skip)]
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
    #[serde(default)]
    pub codec_preference: CodecPreference,
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
    /// Backlight level in percent of the panel maximum.
    #[serde(default = "default_brightness")]
    pub brightness: u8,
    /// How Albums presents its covers.
    #[serde(default)]
    pub album_view: AlbumView,
    /// 1 once the codec default became Auto. Older sessions stored SBC only
    /// as the default (the choice was never offered), so they move to Auto.
    #[serde(default)]
    pub codec_revision: u8,
    /// The chosen skin's id; empty is Reborn's own look.
    #[serde(default)]
    pub theme: String,
    /// The clock's tz database zone, e.g. "Europe/Berlin".
    #[serde(default = "default_time_zone")]
    pub time_zone: String,
    /// Find the zone from the internet address when Wi-Fi connects. `None`
    /// (settings from before Beta 7.1): automatic unless a zone was chosen.
    #[serde(default)]
    pub time_zone_auto: Option<bool>,
    /// Music lists also pinned to Home, by key (`HOME_PINNABLE`); Home itself
    /// is Now Playing, Music, Playlists, Radio, Search and Settings.
    #[serde(default)]
    pub home_pins: Vec<String>,
}

/// The Music lists that can be pinned to Home, in Home's order: (key, label).
pub const HOME_PINNABLE: [(&str, &str); 5] = [
    ("artists", "Artists"),
    ("albums", "Albums"),
    ("tracks", "Songs"),
    ("genres", "Genres"),
    ("folders", "Folders"),
];
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AlbumView {
    #[default]
    CoverFlow,
    Grid,
    /// Every cover edge to edge, six across.
    Wall,
}
impl AlbumView {
    pub fn label(self) -> &'static str {
        match self {
            Self::CoverFlow => "Cover Flow",
            Self::Grid => "Grid",
            Self::Wall => "Wall",
        }
    }
    /// The view the Display row switches to next.
    pub fn next(self) -> Self {
        match self {
            Self::CoverFlow => Self::Grid,
            Self::Grid => Self::Wall,
            Self::Wall => Self::CoverFlow,
        }
    }
}
fn default_true() -> bool {
    true
}
fn default_brightness() -> u8 {
    80
}
fn default_time_zone() -> String {
    "UTC".into()
}
pub const BRIGHTNESS_LEVELS: [u8; 5] = [20, 40, 60, 80, 100];

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
            codec_preference: CodecPreference::Auto,
            screen_timeout_seconds: 60,
            music_directory: "/data/music".into(),
            replay_gain: ReplayGainMode::Off,
            eq_enabled: false,
            eq_bands: Vec::new(),
            crossfade_ms: 0,
            gapless_enabled: true,
            shuffle: false,
            repeat: RepeatMode::Off,
            brightness: default_brightness(),
            album_view: AlbumView::CoverFlow,
            codec_revision: CODEC_REVISION,
            theme: String::new(),
            time_zone: default_time_zone(),
            time_zone_auto: None,
            home_pins: Vec::new(),
        }
    }
}
impl Settings {
    /// Whether the clock's zone is found automatically (Beta 7.1).
    pub fn zone_auto(&self) -> bool {
        self.time_zone_auto.unwrap_or(self.time_zone == "UTC")
    }

    /// Pin or unpin a Music list on Home. Pins keep `HOME_PINNABLE`'s order,
    /// whatever order they were chosen in; unknown keys are ignored.
    pub fn pin_home(&mut self, key: &str, on: bool) {
        let mut pins: Vec<String> = HOME_PINNABLE
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| {
                if *k == key {
                    on
                } else {
                    self.home_pins.iter().any(|p| p == k)
                }
            })
            .map(str::to_owned)
            .collect();
        std::mem::swap(&mut self.home_pins, &mut pins);
    }
}
/// See `Settings::codec_revision`.
pub const CODEC_REVISION: u8 = 1;
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppModel {
    pub playback: PlaybackState,
    pub queue: Vec<Track>,
    #[serde(default)]
    pub queue_entry_ids: Vec<QueueEntryId>,
    pub queue_position: usize,
    pub position_ms: u64,
    pub output: AudioOutput,
    #[serde(skip)]
    pub screen: Screen,
    pub settings: Settings,
    #[serde(skip)]
    pub generation: u64,
    #[serde(skip)]
    pub sources: Vec<Source>,
    #[serde(skip)]
    pub screen_off: bool,
    #[serde(skip)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub library: LibraryState,
    #[serde(skip)]
    pub navigation: NavigationState,
    #[serde(skip)]
    pub platform: PlatformState,
}

/// Effects are requests to the application/service boundary. They carry no
/// platform handles and can be asserted in host tests without hardware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Platform(PlatformTask),
    WifiDisconnect,
    StopPlayback,
    Radio(RadioRequest),
    Playlist(Box<PlaylistRequest>),
    SetCodecPreference(CodecPreference),
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
    SetEqEnabled(bool),
    SetEqBandGain {
        index: usize,
        gain_db: i8,
    },
    ResetEq,
    /// Gains in dB for the five equalizer bands; all zero turns the equalizer off.
    SetEqPreset([i8; 5]),
    SetBrightness(u8),
    SetCrossfade(u32),
    SetGapless(bool),
    SetAlbumView(AlbumView),
    SetShuffle(bool),
    SetRepeat(RepeatMode),
    SetScreenTimeout(u32),
    /// Switch to the skin with this id ("" = Reborn's own look).
    SetTheme(String),
    /// Look for installed skins again (the picker opened).
    ScanThemes,
    /// Choose the clock's zone (tz database name); turns Automatic off.
    SetTimeZone(String),
    /// Find the clock's zone from the internet address, or stop doing so.
    SetTimeZoneAuto(bool),
    /// Pin (true) or unpin a Music list (`HOME_PINNABLE` key) on Home.
    SetHomePin(String, bool),
    /// Stop the update download in progress (Cancel on the update screen).
    CancelDownload,
    QueueRemove(usize),
    QueueMove {
        index: usize,
        delta: i8,
    },
    ClearQueue,
    RebuildLibrary,
    ResetSettings,
    ForgetAllWifi,
    ForgetAllBluetooth,
    ClearCache,
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
                if self.current().is_some_and(|t| {
                    !t.is_radio() && !self.sources.iter().any(|s| s.online && s.id == t.source_id)
                }) {
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
        if m.settings.codec_revision < CODEC_REVISION {
            if m.settings.codec_preference == CodecPreference::Sbc {
                m.settings.codec_preference = CodecPreference::Auto;
            }
            m.settings.codec_revision = CODEC_REVISION;
        }
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
        m.screen = Screen::Home;
        // Start on the jack; Bluetooth headphones take over again as soon as
        // their audio is ready (Reborn's automatic output). A saved Bluetooth
        // output whose peer is absent left playback with nowhere to go.
        m.output = AudioOutput::Wired;
        m.generation = 0;
        m.navigation = NavigationState::default();
        m.settings.volume = m.settings.volume.min(100);
        m.settings.crossfade_ms = m.settings.crossfade_ms.min(30_000);
        m.settings.eq_bands.truncate(8);
        m.settings.brightness = m.settings.brightness.clamp(BRIGHTNESS_LEVELS[0], 100);
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
/// Replace a file so a power cut leaves the old or the new one: a hidden
/// temporary next to it (`.name.reborn-new`, one per file, never shown over
/// USB as a stray file), flushed, renamed over it, the folder flushed.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.reborn-new", name.to_string_lossy()));
    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    fs::File::open(parent)?.sync_all()
}
#[cfg(test)]
mod radio_station_tests {
    use super::*;
    #[test]
    fn station_details_read_well() {
        let s = RadioStation {
            name: "FIP".into(),
            codec: "AAC".into(),
            bitrate: 192,
            tags: "eclectic,jazz,Jazz,world music".into(),
            language: "french,english".into(),
            favicon: "https://fip/logo.png".into(),
            ..Default::default()
        };
        assert_eq!(s.genres(5), ["Eclectic", "Jazz", "World Music"]);
        assert_eq!(s.format(), "AAC · 192 kbps");
        assert_eq!(s.languages(), "French, English");
        let key = s.logo_key().unwrap();
        assert_eq!(key, logo_key("https://fip/logo.png"));
        assert!(key >> 63 == 1, "never a cover's key");
        assert_eq!(RadioStation::default().logo_key(), None);
        assert_eq!(capitalise("80s hip hop BBC"), "80s Hip Hop BBC");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
    fn codec_default_is_auto_and_an_old_sbc_default_moves_to_auto_once() {
        assert_eq!(Settings::default().codec_preference, CodecPreference::Auto);
        let path = std::env::temp_dir().join(format!("reborn-codec-{}.json", std::process::id()));
        let save = |preference: CodecPreference, revision: Option<u8>| {
            let mut model = AppModel::default();
            model.settings.codec_preference = preference;
            let mut value = serde_json::to_value(&model).unwrap();
            match revision {
                Some(r) => value["settings"]["codec_revision"] = r.into(),
                None => {
                    value["settings"]
                        .as_object_mut()
                        .unwrap()
                        .remove("codec_revision");
                }
            }
            fs::write(
                &path,
                serde_json::to_vec(&json!({"schema":2,"model":value})).unwrap(),
            )
            .unwrap();
            AppModel::restore(&path).unwrap().settings
        };
        // A session from before the revision: its SBC was only ever the default.
        let old = save(CodecPreference::Sbc, None);
        assert_eq!(old.codec_preference, CodecPreference::Auto);
        assert_eq!(old.codec_revision, CODEC_REVISION);
        // An explicit older choice other than SBC is kept.
        assert_eq!(
            save(CodecPreference::Aac, None).codec_preference,
            CodecPreference::Aac
        );
        // SBC chosen after the revision is respected.
        let chosen = save(CodecPreference::Sbc, Some(CODEC_REVISION));
        assert_eq!(chosen.codec_preference, CodecPreference::Sbc);
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

    #[test]
    fn storage_changes_never_pause_a_radio_station() {
        let station = RadioStation {
            name: "Jazz".into(),
            url: "http://jazz.example/".into(),
            ..Default::default()
        };
        let mut m = AppModel::default();
        m.replace_queue(vec![Track::radio(&station)], 0).unwrap();
        m.playback = PlaybackState::Playing;
        m.apply(Event::SourceChanged(vec![]));
        assert_eq!(m.playback, PlaybackState::Playing);
        let track = m.current().unwrap();
        assert!(track.is_radio() && track.id < 0);
        assert_eq!(track.path, PathBuf::from("http://jazz.example/"));
    }

    #[test]
    fn home_pins_keep_home_order_and_old_settings_have_none() {
        let mut s = Settings::default();
        s.pin_home("genres", true);
        s.pin_home("artists", true);
        s.pin_home("nonsense", true);
        assert_eq!(s.home_pins, ["artists", "genres"]);
        s.pin_home("artists", false);
        assert_eq!(s.home_pins, ["genres"]);
        let old: Settings = serde_json::from_value(serde_json::json!({
            "volume": 35, "screen_timeout_seconds": 60, "music_directory": "/data/music"
        }))
        .unwrap();
        assert!(old.home_pins.is_empty());
    }

    #[test]
    fn countries_agree_with_the_zone_table_and_old_choices_stay_manual() {
        for (code, name, zones) in clock::countries() {
            assert!(!zones.is_empty() && code.len() == 2 && !name.is_empty());
            for zone in *zones {
                assert!(clock::zone(zone).is_some(), "{zone} of {name} has no rule");
            }
        }
        assert_eq!(clock::country_of("Europe/Berlin").map(|c| c.0), Some("DE"));
        assert!(clock::country_of("UTC").is_none());
        // Never chosen: automatic. Chosen before Beta 7.1: stays as chosen.
        let mut s = Settings::default();
        assert!(s.zone_auto());
        s.time_zone = "Europe/Berlin".into();
        assert!(!s.zone_auto());
        s.time_zone_auto = Some(true);
        assert!(s.zone_auto());
    }
}
