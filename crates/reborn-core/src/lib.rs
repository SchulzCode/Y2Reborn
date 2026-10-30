#![forbid(unsafe_code)]
pub mod albums;
pub mod books;
pub mod clock;
mod countries;
pub mod eq;
pub mod platform;
pub mod playlist;
pub mod queue;
pub mod scrobble;
pub mod sort;
mod zones;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
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
/// The play queue holds at most this many songs (about two weeks of music).
/// The session stores every queued song (≈500 bytes each, `SESSION_MAX_BYTES`)
/// and rewrites it at each song change: 20 000 never fit, and a larger
/// collection is played as a window of it (`queue_window`), a shuffle as a
/// random part of it. A library of 28 600 songs refused every play from All
/// Songs ("invalid queue bounds").
pub const MAX_QUEUE: usize = 5_000;

/// The part of a `len`-song collection queued when `selected` starts playing:
/// at most `cap` songs, the selected one with a tenth of the window before it.
pub fn queue_window(len: usize, selected: usize, cap: usize) -> std::ops::Range<usize> {
    if len <= cap {
        return 0..len;
    }
    let start = selected.saturating_sub(cap / 10).min(len - cap);
    start..start + cap
}
/// 3 (Beta 8.33): the queue names library songs by reference; your queued
/// songs, the source's order and name, and the history.
pub const SESSION_SCHEMA_VERSION: u32 = 3;
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
/// The shape of one equalizer filter (RBJ cookbook biquads, as FFmpeg's
/// `equalizer`, `lowshelf` and `highshelf` with a Q width).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EqKind {
    #[default]
    Peak,
    LowShelf,
    HighShelf,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub frequency_hz: f32,
    pub gain_db: f32,
    pub q: f32,
    #[serde(default)]
    pub kind: EqKind,
}
impl Default for EqBand {
    fn default() -> Self {
        Self {
            frequency_hz: 1000.0,
            gain_db: 0.0,
            q: 1.0,
            kind: EqKind::Peak,
        }
    }
}

/// Headphone output level: the DAC's full-scale voltage (Y2Linux kernel
/// control "HP Output Level"). Low is the stock headphone level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HpLevel {
    #[default]
    Low,
    Medium,
    High,
}
impl HpLevel {
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }
    /// The level's index in the kernel control.
    pub fn index(self) -> u32 {
        self as u32
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.key() == key)
    }
    /// The amplifier's full scale in dBV: 1.0, 1.41 and 1.73 Vrms.
    pub fn full_scale_dbv(self) -> f64 {
        match self {
            Self::Low => 0.0,
            Self::Medium => 2.984,
            Self::High => 4.761,
        }
    }
    /// The DAC Master (dB) a wired stream plays this level with: at full
    /// volume Low gives about 0.35 Vrms, Medium 0.71 (stock's loudest) and
    /// High the chip's 1.73.
    pub fn master_db(self) -> f64 {
        match self {
            Self::Low => -9.0,
            Self::Medium => -6.0,
            Self::High => 0.0,
        }
    }
    /// The output at full volume and a full-scale signal, in dBV.
    pub fn peak_dbv(self) -> f64 {
        self.full_scale_dbv() + self.master_db()
    }
    /// The loudest volume at or below `SAFE_PEAK_DBV`.
    pub fn safe_volume(self) -> u8 {
        volume_at_most(SAFE_PEAK_DBV - self.peak_dbv())
    }
}
/// The loudest wired output before Beta 8.35 (High's 1.73 Vrms with the
/// Master at −19.5 dB, about 0.18 Vrms). Louder takes the listener's
/// agreement (`Settings::loud_ok`).
pub const SAFE_PEAK_DBV: f64 = 4.761 - 19.5;
/// After this much listening above `SAFE_PEAK_DBV` the agreement lapses.
pub const LOUD_LISTENING_LIMIT_S: u32 = 20 * 3600;
/// The volume's level in dB: 10–100 rise 0.6 dB a step (a wheel click,
/// two steps, is 1.2 dB), 1–9 fall 3 dB a step to −81 dB, 0 is silence.
pub fn volume_db(volume: u8) -> f64 {
    let v = f64::from(volume.clamp(1, 100));
    if v >= 10.0 {
        -0.6 * (100.0 - v)
    } else {
        -54.0 - 3.0 * (10.0 - v)
    }
}
/// The software gain of a volume (1.0 at 100).
pub fn volume_gain(volume: u8) -> f64 {
    if volume == 0 {
        0.0
    } else {
        10f64.powf(volume_db(volume) / 20.0)
    }
}
/// The loudest volume at or below `db`; 1 when even 1 is louder.
pub fn volume_at_most(db: f64) -> u8 {
    (1..=100u8)
        .rev()
        .find(|&v| volume_db(v) <= db + 1e-9)
        .unwrap_or(1)
}
/// See `Settings::volume_scale`.
pub const VOLUME_SCALE: u8 = 1;
/// The DAC's interpolation filter (CS43131 PCM Filter Option).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DacFilter {
    /// Fast roll-off, low latency: the chip's and stock's default.
    #[default]
    Fast,
    FastPhaseCompensated,
    Slow,
    SlowPhaseCompensated,
    /// Nonoversampling emulation (no interpolation filter).
    Nos,
}
impl DacFilter {
    pub const ALL: [Self; 5] = [
        Self::Fast,
        Self::FastPhaseCompensated,
        Self::Slow,
        Self::SlowPhaseCompensated,
        Self::Nos,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Fast => "Fast",
            Self::FastPhaseCompensated => "Fast, Phase Compensated",
            Self::Slow => "Slow",
            Self::SlowPhaseCompensated => "Slow, Phase Compensated",
            Self::Nos => "Nonoversampling",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::FastPhaseCompensated => "fast_phase",
            Self::Slow => "slow",
            Self::SlowPhaseCompensated => "slow_phase",
            Self::Nos => "nos",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.key() == key)
    }
    /// (slow, phase compensated, nonoversampling) for the three kernel controls.
    pub fn switches(self) -> (bool, bool, bool) {
        match self {
            Self::Fast => (false, false, false),
            Self::FastPhaseCompensated => (false, true, false),
            Self::Slow => (true, false, false),
            Self::SlowPhaseCompensated => (true, true, false),
            Self::Nos => (false, false, true),
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
            kind: EqKind::Peak,
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
    /// An audiobook part (`books::is_book`), set when the library loads; never
    /// stored, so the rule can change without a rescan.
    #[serde(default)]
    pub book: bool,
    /// The song's own Album Artist tag when `album_artist` holds its album's
    /// (`albums::unify_album_artists`, #45): set when the library loads, never
    /// stored in the library. None: `album_artist` is the song's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_album_artist: Option<String>,
}

/// The largest left/right balance in dB (Settings > Sound > Balance).
pub const MAX_BALANCE: i8 = 10;

/// The names in an artist tag (#38, #40): a multi-value tag comes joined by
/// ";" ("Calvin Harris;Kelis"), and a guest follows "feat.", "ft." or
/// "featuring". "&" and "," stay, as in "Simon & Garfunkel". An empty tag
/// is one unknown artist.
pub fn split_artists(tag: &str) -> Vec<&str> {
    const GUEST: [&str; 5] = [" featuring ", " feat. ", " feat ", " ft. ", " ft "];
    let mut names: Vec<&str> = Vec::new();
    for mut rest in tag.split(';') {
        loop {
            // ASCII markers, so every match starts and ends on a char boundary.
            let guest = GUEST
                .iter()
                .filter_map(|guest| {
                    rest.as_bytes()
                        .windows(guest.len())
                        .position(|w| w.eq_ignore_ascii_case(guest.as_bytes()))
                        .map(|at| (at, guest.len()))
                })
                .min();
            let (name, next) = match guest {
                Some((at, len)) => (&rest[..at], Some(&rest[at + len..])),
                None => (rest, None),
            };
            let name = name.trim();
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
            match next {
                Some(next) => rest = next,
                None => break,
            }
        }
    }
    if names.is_empty() {
        names.push(tag.trim());
    }
    names
}

/// The genres in a genre tag (#40): a multi-value tag comes joined by ";",
/// as artists do. An empty tag is one "No Genre".
pub fn split_genres(tag: &str) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    for name in tag.split(';').map(str::trim) {
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    if names.is_empty() {
        names.push("");
    }
    names
}

/// A tag of names joined by ";" as people read it (#40):
/// "Lauryn Hill;Mary J. Blige" shows as "Lauryn Hill, Mary J. Blige".
pub fn joined_names(tag: &str) -> std::borrow::Cow<'_, str> {
    if !tag.contains(';') {
        return tag.into();
    }
    let names: Vec<&str> = tag
        .split(';')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    names.join(", ").into()
}

/// `Track::source_id` of an internet radio station in the queue.
pub const RADIO_SOURCE: &str = "radio";
/// `Track::source_id` of Spotify in the queue: one entry stands for whatever
/// spotifyd plays (its own queue follows the playlist or album chosen).
pub const SPOTIFY_SOURCE: &str = "spotify";
impl Track {
    /// An internet radio station: `path` holds the stream address, `filename`
    /// the directory id (if any), `album` a short description.
    pub fn is_radio(&self) -> bool {
        self.source_id == RADIO_SOURCE
    }
    /// Spotify: `path` holds the URI chosen, the tags follow the song
    /// spotifyd plays.
    pub fn is_spotify(&self) -> bool {
        self.source_id == SPOTIFY_SOURCE
    }
    /// Played from a stream, not a file: never in the library, the history,
    /// gapless pre-opening or scrobbling (Spotify scrobbles itself).
    pub fn is_live(&self) -> bool {
        self.is_radio() || self.is_spotify()
    }
    /// Shown in the music lists, counts and search: available and not part
    /// of an audiobook (books have their own place).
    pub fn is_music(&self) -> bool {
        self.online && !self.book
    }
    /// The Album Artist tag the file carries (Last.fm gets this), not the
    /// album's the lists group it under (#45).
    pub fn tagged_album_artist(&self) -> &str {
        self.own_album_artist
            .as_deref()
            .unwrap_or(&self.album_artist)
    }
    /// The artists one by one, as the Artists list files the track.
    pub fn artists(&self) -> Vec<&str> {
        split_artists(&self.artist)
    }
    /// The genres one by one, as the Genres list files the track.
    pub fn genres(&self) -> Vec<&str> {
        split_genres(&self.genre)
    }
    /// The Spotify queue entry for `uri`, titled until spotifyd says more.
    pub fn spotify(uri: &str, title: &str) -> Self {
        Self {
            id: -2,
            source_id: SPOTIFY_SOURCE.into(),
            path: PathBuf::from(uri),
            title: title.into(),
            artist: "Spotify".into(),
            codec: "Vorbis".into(),
            sample_rate: 44_100,
            channels: 2,
            bitrate: 320_000,
            online: true,
            ..Default::default()
        }
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

/// Settings > Last.fm requests. A password lives only in `Login` on its way
/// to the one sign-in request and is never printed.
#[derive(Clone, PartialEq, Eq)]
pub enum LastFmRequest {
    /// Start a sign-in: the player shows a QR code to approve on a phone.
    Connect,
    Cancel,
    /// Fallback sign-in with username and password.
    Login {
        username: String,
        password: String,
    },
    SendNow,
    Disconnect,
}
impl std::fmt::Debug for LastFmRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect => f.write_str("Connect"),
            Self::Cancel => f.write_str("Cancel"),
            Self::Login { .. } => f.write_str("Login { .. }"),
            Self::SendNow => f.write_str("SendNow"),
            Self::Disconnect => f.write_str("Disconnect"),
        }
    }
}

/// What the Spotify screens ask the player for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpotifyRequest {
    /// Start a sign-in: the player shows a QR code to approve on a phone.
    Connect,
    Cancel,
    Disconnect,
    /// Load a list (`SpotifyList::key`) for the screen showing it.
    Load(String),
    /// The next page of a list.
    More(String),
    /// Play row `index` of list `list`.
    Play {
        list: String,
        index: usize,
    },
    /// Play a whole list (a playlist, an album, Liked Songs) from its start.
    PlayAll(String),
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
    /// Left/right balance picker.
    Balance,
    /// Screen-off delay picker.
    ScreenTimeout,
    /// Where music plays, and the headphone jack's settings (Settings > Sound).
    Output,
    /// Wired headphone output level picker (Settings > Sound > Output).
    HeadphoneLevel,
    /// DAC interpolation filter picker (Settings > Sound).
    DacFilter,
    /// Skin picker: the built-in look and the installed skins.
    Theme,
    /// Time zone picker (Settings > Display).
    TimeZone,
    /// The date and time set by hand (Settings > Display, #53).
    DateTime,
    /// Which Music lists also stand on Home (Settings > Customize).
    HomeMenu,
    SettingsLibrary,
    SettingsDisplay,
    /// How Reborn behaves: wrap-around lists, Home Menu, Album View.
    Customize,
    /// The audiobooks: in progress first, then new, then finished.
    Audiobooks,
    /// One audiobook (`navigation.filter` is its key): buttons and chapters.
    Book,
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
    /// Settings > Last.fm: sign-in, scrobbling and the scrobbler log.
    LastFm,
    /// Spotify: sign-in, then the account's library.
    Spotify,
    /// One Spotify list; the navigation filter holds its key.
    SpotifyList,
    /// One playlist; the navigation filter holds its file's path.
    Playlist,
    /// About & Updates > Error Reports; filter "info": what is sent, "save":
    /// where Save Report writes the file.
    ErrorReports,
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
    /// One field of Settings > Display > Date & Time.
    DateField(DateField),
    /// The sleep timer's lengths (Beta 8.36, #29).
    SleepTimer,
    Confirm(ConfirmAction),
    /// "Add to Playlist": the playlists, the last used first, and New.
    PlaylistPicker,
}

/// The fields of the date and time set by hand, in the order shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateField {
    Year,
    Month,
    Day,
    Hour,
    Minute,
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
    /// The one-time question after the update: send error reports or not.
    ReportsQuestion,
    /// Wired volume above the safe level (hearing warning).
    LoudVolume,
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
    /// Set up Internal Storage over the old storage the platform reported waiting (erases it).
    InternalStorageSetup,
    /// Repair the SD card the platform marked (read-only with errors, or
    /// refused); damaged files may be shortened or removed.
    SdRepair,
    /// Error reports on (true) or off; off also drops the waiting reports.
    ReportEnable(bool),
    /// A new random install ID.
    ReportNewId,
    /// Build a report now (about ten seconds) and send it, or queue it when it
    /// cannot go out yet.
    ReportSend,
    /// Build a report now and save it to storage for the owner to send.
    ReportSave(platform::ReportDestination),
    /// Set the system clock to this instant (unix seconds), the owner's own
    /// date and time (#53); network time replaces it when Wi-Fi connects.
    SetClock(i64),
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
    /// Live progress of a report the owner is sending or saving.
    pub report: Option<platform::ReportLive>,
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
    /// Where every audiobook was left (`state/books.json`, kept apart from
    /// the session and the library database).
    #[serde(skip)]
    pub shelf: books::Shelf,
    /// Chapter marks read from book files, by path (filled in the background).
    #[serde(skip)]
    pub chapters: HashMap<PathBuf, Vec<books::ChapterMark>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScanSummary {
    pub discovered: u64,
    pub reused: u64,
    pub elapsed_ms: u64,
    /// Supported files that could not be read (kept, never an error).
    #[serde(default)]
    pub unreadable: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub volume: u8,
    /// The wired volume, kept while Bluetooth headphones with their own
    /// (AVRCP absolute) volume follow `volume` (Y2Reborn #22).
    #[serde(default)]
    pub wired_volume: Option<u8>,
    /// Left/right balance in dB, -MAX_BALANCE..=MAX_BALANCE: negative makes
    /// the left louder (the right is lowered), positive the right.
    #[serde(default)]
    pub balance: i8,
    /// Headphones have followed `volume` before: their own level is the
    /// whole loudness, Reborn's software level no longer applies to them.
    #[serde(default)]
    pub bluetooth_absolute_volume: bool,
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
    /// Shuffle mixes whole albums, not songs (`shuffle_mode`, Beta 8.39).
    /// Kept apart from `shuffle` so older sessions and skins read it as on.
    #[serde(default)]
    pub shuffle_albums: bool,
    #[serde(default)]
    pub repeat: RepeatMode,
    /// Backlight level in percent of the panel maximum.
    #[serde(default = "default_brightness")]
    pub brightness: u8,
    /// How Albums and Audiobooks present their covers.
    #[serde(default)]
    pub album_view: AlbumView,
    /// How Albums orders them (#56).
    #[serde(default)]
    pub album_order: AlbumOrder,
    /// What the wheel does on Now Playing (Beta 8.36).
    #[serde(default)]
    pub now_playing_wheel: NowPlayingWheel,
    /// Who Artists lists (Beta 8.36).
    #[serde(default)]
    pub artists_list: ArtistsList,
    /// Whether lists continue at the other end (Beta 8.18; Menus by default).
    #[serde(default)]
    pub wrap_around: WrapAround,
    /// What the wheel and its buttons do while the screen is off (Beta 8.18).
    #[serde(default)]
    pub screen_off_buttons: ScreenOffButtons,
    /// A tick of the vibration motor on each button press (#28).
    #[serde(default)]
    pub haptics: Haptics,
    /// Play on the internal speaker while no headphones are plugged in, as
    /// stock's "Speaker" setting (#28 speaker).
    #[serde(default = "default_true")]
    pub speaker: bool,
    /// Audiobooks stand on Home once the library holds one (Beta 8.19).
    #[serde(default = "default_true")]
    pub audiobooks_on_home: bool,
    /// Playlists shown in another order than their file's, by path.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub playlist_sort: std::collections::BTreeMap<String, PlaylistSort>,
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
    /// Home's own places the listener took off Home, by key
    /// (`HOME_HIDEABLE`; Y2Reborn #52, #55). Now Playing and Settings stay.
    #[serde(default)]
    pub home_hidden: Vec<String>,
    /// Also write every scrobble to `.scrobbler.log` on Internal Storage
    /// (Rockbox format, for upload tools); works without an account.
    #[serde(default)]
    pub scrobble_log: bool,
    /// With a Last.fm account: send counted plays, and the song playing now.
    #[serde(default = "default_true")]
    pub lastfm_scrobble: bool,
    #[serde(default = "default_true")]
    pub lastfm_now_playing: bool,
    /// The one-time "Set Up Internal Storage?" question was shown.
    #[serde(default)]
    pub internal_setup_asked: bool,
    /// Wired headphone output level.
    #[serde(default)]
    pub hp_level: HpLevel,
    /// 1 once the volume uses the dB scale and the louder levels (Beta
    /// 8.35). Older sessions saved a squared scale with the Master at
    /// −19.5 dB; their volume moves to the same output level once.
    #[serde(default)]
    pub volume_scale: u8,
    /// The listener agreed to wired volumes above `SAFE_PEAK_DBV`.
    #[serde(default)]
    pub loud_ok: bool,
    /// Seconds of wired listening above `SAFE_PEAK_DBV` since the agreement.
    #[serde(default)]
    pub loud_seconds: u32,
    /// Wired DAC interpolation filter.
    #[serde(default)]
    pub dac_filter: DacFilter,
    /// The imported headphone profile (kept when the file is removed).
    #[serde(default)]
    pub eq_profile: Option<eq::EqProfile>,
    /// The profile plays instead of the five-band equalizer.
    #[serde(default)]
    pub eq_profile_on: bool,
}

/// The Music lists that can be pinned to Home, in Home's order: (key, label).
pub const HOME_PINNABLE: [(&str, &str); 5] = [
    ("artists", "Artists"),
    ("albums", "Albums"),
    ("tracks", "Songs"),
    ("genres", "Genres"),
    ("folders", "Folders"),
];
/// Home's own places that can be taken off Home, in Home's order: (key,
/// label). Music stands first, the others after the pinned lists.
pub const HOME_HIDEABLE: [(&str, &str); 5] = [
    ("music", "Music"),
    ("playlists", "Playlists"),
    ("radio", "Radio"),
    ("spotify", "Spotify"),
    ("search", "Search"),
];
/// How Albums and Audiobooks show their covers: Customize > Cover Flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AlbumView {
    /// Cover Flow On.
    #[default]
    CoverFlow,
    /// Cover Flow Off: the plain lists (saved as "grid").
    Grid,
}
impl AlbumView {
    pub fn label(self) -> &'static str {
        match self {
            Self::CoverFlow => "On",
            Self::Grid => "Off",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::CoverFlow => Self::Grid,
            Self::Grid => Self::CoverFlow,
        }
    }
}
/// How Albums (and Cover Flow) order the albums: Customize > Album Order (#56).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AlbumOrder {
    /// A to Z by title, then artist.
    #[default]
    Title,
    /// A to Z by the artist shown under the album, then title: an artist's
    /// albums stand together.
    Artist,
}
impl AlbumOrder {
    pub fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Artist => "Artist",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Title => Self::Artist,
            Self::Artist => Self::Title,
        }
    }
}
/// What the wheel does on Now Playing: Customize > Now Playing Wheel. The
/// volume buttons always change the volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NowPlayingWheel {
    #[default]
    Volume,
    /// It moves through the song as holding Previous/Next does.
    Seek,
}
impl NowPlayingWheel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Volume => "Volume",
            Self::Seek => "Seek",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Volume => Self::Seek,
            Self::Seek => Self::Volume,
        }
    }
}
/// What Shuffle mixes (Settings > Playback, Now Playing; as the iPod's
/// Shuffle Off, Songs and Albums): every song, or whole albums in random
/// order, each album in its own order (Beta 8.39).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShuffleMode {
    #[default]
    Off,
    Songs,
    Albums,
}
impl ShuffleMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Songs => "Songs",
            Self::Albums => "Albums",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Songs,
            Self::Songs => Self::Albums,
            Self::Albums => Self::Off,
        }
    }
}
/// The order a playlist's page shows and plays its songs in (Beta 8.39).
/// Only the view changes: the file keeps its own order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistSort {
    /// As the file lists them (songs can be moved).
    #[default]
    Playlist,
    /// Album by album (album artist, album, disc, track).
    Album,
    Artist,
    Title,
    FileName,
}
impl PlaylistSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Playlist => "Playlist Order",
            Self::Album => "Album",
            Self::Artist => "Artist",
            Self::Title => "Title",
            Self::FileName => "File Name",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Playlist => Self::Album,
            Self::Album => Self::Artist,
            Self::Artist => Self::Title,
            Self::Title => Self::FileName,
            Self::FileName => Self::Playlist,
        }
    }
}
/// Who Artists lists: Customize > Artists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ArtistsList {
    /// Every artist of every song, guests too (#38).
    #[default]
    All,
    /// The album artists only (#41): a compilation is one entry.
    AlbumArtists,
}
impl ArtistsList {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::AlbumArtists => "Album Artists",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::AlbumArtists,
            Self::AlbumArtists => Self::All,
        }
    }
}
/// Which lists continue at the other end: past the last row to the first and
/// before the first to the last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WrapAround {
    /// Every list stops at its ends.
    Off,
    /// Home, Music, Settings and their pages, and the hold and context menus.
    #[default]
    Menus,
    /// Also the library, the queue, playlists, stations and the time zones.
    Everywhere,
}
impl WrapAround {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Menus => "Menus",
            Self::Everywhere => "Everywhere",
        }
    }
    /// The next choice, as one press on its row steps through them.
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Menus,
            Self::Menus => Self::Everywhere,
            Self::Everywhere => Self::Off,
        }
    }
}
/// What the wheel, its centre and its four buttons do while the screen is
/// off. Power, the volume keys and headphone controls always work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScreenOffButtons {
    /// Nothing: safe in a pocket.
    Locked,
    /// Play/Pause, Previous and Next (held: seek); the wheel and centre rest.
    #[default]
    Buttons,
    /// Also the wheel (volume) and the centre (play/pause), like Now Playing.
    WheelAndButtons,
}
impl ScreenOffButtons {
    pub fn label(self) -> &'static str {
        match self {
            Self::Locked => "Locked",
            Self::Buttons => "Buttons",
            Self::WheelAndButtons => "Wheel + Buttons",
        }
    }
    /// The next choice, as one press on its row steps through them.
    pub fn next(self) -> Self {
        match self {
            Self::Locked => Self::Buttons,
            Self::Buttons => Self::WheelAndButtons,
            Self::WheelAndButtons => Self::Locked,
        }
    }
}
/// The vibration motor's tick on the wheel and buttons (#28). Off until
/// chosen: an update does not start buzzing on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Haptics {
    #[default]
    Off,
    Light,
    Strong,
}
impl Haptics {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Light => "Light",
            Self::Strong => "Strong",
        }
    }
    /// The next choice, as one press on its row steps through them.
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Light,
            Self::Light => Self::Strong,
            Self::Strong => Self::Off,
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
            wired_volume: None,
            balance: 0,
            bluetooth_absolute_volume: false,
            codec_preference: CodecPreference::Auto,
            screen_timeout_seconds: 60,
            music_directory: "/data/music".into(),
            replay_gain: ReplayGainMode::Off,
            eq_enabled: false,
            eq_bands: Vec::new(),
            crossfade_ms: 0,
            gapless_enabled: true,
            shuffle: false,
            shuffle_albums: false,
            repeat: RepeatMode::Off,
            brightness: default_brightness(),
            album_view: AlbumView::CoverFlow,
            album_order: AlbumOrder::Title,
            now_playing_wheel: NowPlayingWheel::Volume,
            artists_list: ArtistsList::All,
            wrap_around: WrapAround::Menus,
            screen_off_buttons: ScreenOffButtons::Buttons,
            haptics: Haptics::Off,
            speaker: true,
            audiobooks_on_home: true,
            playlist_sort: Default::default(),
            codec_revision: CODEC_REVISION,
            theme: String::new(),
            time_zone: default_time_zone(),
            time_zone_auto: None,
            home_pins: Vec::new(),
            home_hidden: Vec::new(),
            scrobble_log: false,
            lastfm_scrobble: true,
            lastfm_now_playing: true,
            internal_setup_asked: false,
            hp_level: HpLevel::Low,
            volume_scale: VOLUME_SCALE,
            loud_ok: false,
            loud_seconds: 0,
            dac_filter: DacFilter::Fast,
            eq_profile: None,
            eq_profile_on: false,
        }
    }
}
impl Settings {
    /// Keep a stored profile within what the engine plays: at most ten
    /// filters and a preamp that still covers them; drop an empty one.
    pub fn sanitize_eq_profile(&mut self) {
        if let Some(p) = &mut self.eq_profile {
            p.bands.truncate(eq::MAX_BANDS);
            p.preamp_db = eq::safe_preamp_db(Some(p.preamp_db), &p.bands);
        }
        if self.eq_profile.as_ref().is_none_or(|p| p.bands.is_empty()) {
            self.eq_profile = None;
            self.eq_profile_on = false;
        }
    }
    /// The filters and preamp playback uses: the headphone profile when it is
    /// on, else the five-band equalizer (no preamp; the limiter catches peaks).
    pub fn active_eq(&self) -> Option<(f32, Vec<EqBand>)> {
        match &self.eq_profile {
            Some(p) if self.eq_profile_on => Some((p.preamp_db, p.bands.clone())),
            _ if self.eq_enabled => Some((0.0, self.eq_bands.clone())),
            _ => None,
        }
    }
    /// Whether playback asks the platform for its heavy class (a 747.5 MHz
    /// floor and a 1 ms wake-up limit that keep the CPU out of deep idle):
    /// only crossfade, which runs two decoders at a track change. An
    /// equalizer or headphone profile costs a few percent of one core, which
    /// the governor follows by itself (Beta 8.5 review).
    pub fn playback_heavy(&self) -> bool {
        self.crossfade_ms > 0
    }
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
    /// Whether one of Home's own places (`HOME_HIDEABLE`) stands on Home.
    pub fn home_shows(&self, key: &str) -> bool {
        !self.home_hidden.iter().any(|h| h == key)
    }
    /// Show or hide one of Home's own places; kept in `HOME_HIDEABLE`'s
    /// order, unknown keys are ignored (Now Playing and Settings stay).
    pub fn show_on_home(&mut self, key: &str, on: bool) {
        self.home_hidden = HOME_HIDEABLE
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| if *k == key { !on } else { !self.home_shows(k) })
            .map(str::to_owned)
            .collect();
    }
}
impl Settings {
    pub fn shuffle_mode(&self) -> ShuffleMode {
        match (self.shuffle, self.shuffle_albums) {
            (false, _) => ShuffleMode::Off,
            (true, false) => ShuffleMode::Songs,
            (true, true) => ShuffleMode::Albums,
        }
    }
    pub fn set_shuffle_mode(&mut self, mode: ShuffleMode) {
        self.shuffle = mode != ShuffleMode::Off;
        self.shuffle_albums = mode == ShuffleMode::Albums;
    }
    /// The order a playlist's page shows its songs in.
    pub fn playlist_sort(&self, path: &std::path::Path) -> PlaylistSort {
        self.playlist_sort
            .get(&*path.to_string_lossy())
            .copied()
            .unwrap_or_default()
    }
    pub fn set_playlist_sort(&mut self, path: &std::path::Path, sort: PlaylistSort) {
        let key = path.to_string_lossy().into_owned();
        if sort == PlaylistSort::Playlist {
            self.playlist_sort.remove(&key);
        } else {
            self.playlist_sort.insert(key, sort);
        }
    }
    /// The loudest wired volume allowed now: the safe one until the
    /// listener agrees to more.
    pub fn wired_volume_limit(&self) -> u8 {
        if self.loud_ok {
            100
        } else {
            self.hp_level.safe_volume()
        }
    }
    /// A restored session starts on the jack: the wired volume comes back
    /// (headphones holding their own take over again when they connect), a
    /// volume from the old scale moves to the same output level, and the
    /// start is never above the safe level (other headphones may be on).
    pub fn prepare_wired_start(&mut self) {
        if let Some(wired) = self.wired_volume.take() {
            self.volume = wired;
        }
        self.volume = self.volume.min(100);
        if self.volume_scale < VOLUME_SCALE {
            // Old: squared scale, Master −19.5 dB at every level.
            if self.volume > 0 {
                let old = 40.0 * (f64::from(self.volume) / 100.0).log10();
                self.volume = volume_at_most(old - 19.5 - self.hp_level.master_db());
            }
            self.volume_scale = VOLUME_SCALE;
        }
        self.volume = self.volume.min(self.hp_level.safe_volume());
    }
    /// The volume that keeps the output level when the wired level changes
    /// from `from` to `self.hp_level` (never above 100).
    pub fn volume_for_level(&self, volume: u8, from: HpLevel) -> u8 {
        if volume == 0 {
            return 0;
        }
        volume_at_most(volume_db(volume) + from.peak_dbv() - self.hp_level.peak_dbv())
    }
}
/// See `Settings::codec_revision`.
pub const CODEC_REVISION: u8 = 1;
/// A page songs were played from: the screen and its filter (a playlist's
/// path, `album:…`, `folder:…`, `genre:…`, a search).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueOrigin {
    pub screen: Screen,
    #[serde(default)]
    pub filter: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppModel {
    pub playback: PlaybackState,
    #[serde(with = "queue::stored")]
    pub queue: Vec<Track>,
    #[serde(default)]
    pub queue_entry_ids: Vec<QueueEntryId>,
    pub queue_position: usize,
    /// Entries you queued (Play Next, Add to Queue); see `queue`.
    #[serde(default)]
    pub queue_queued: Vec<QueueEntryId>,
    /// The source's entries in its own order (Shuffle off restores it).
    #[serde(default)]
    pub queue_order: Vec<QueueEntryId>,
    /// What is playing, as Up Next names it ("Night Drive", "Songs").
    #[serde(default)]
    pub queue_source: String,
    /// The page the source was played from, which Now Playing's Go to
    /// <Source> reopens (Y2Reborn #25, #60); none for older sessions.
    #[serde(default)]
    pub queue_origin: Option<QueueOrigin>,
    /// Songs that played, newest last (`queue::MAX_HISTORY`).
    #[serde(default)]
    pub history: Vec<queue::HistoryEntry>,
    pub position_ms: u64,
    pub output: AudioOutput,
    /// The headphone jack output plays on the internal speaker now: no
    /// headphones are in and the speaker is on (#28). Set by the runtime.
    #[serde(skip)]
    pub speaker_active: bool,
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
    /// The audiobook being listened to, while the queue is its parts.
    #[serde(default)]
    pub book: Option<books::Listening>,
    /// The running sleep timer (a restart ends it).
    #[serde(skip)]
    pub sleep_timer: Option<SleepTimer>,
}

/// The sleep timer's choices, picked with the wheel from Now Playing's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepChoice {
    Off,
    Minutes(u16),
    /// The end of the chapter (a book) or the song playing.
    EndOfItem,
}
/// The timed lengths in minutes (#29): short steps first, up to three hours.
pub const SLEEP_MINUTES: [u16; 12] = [5, 10, 15, 20, 30, 45, 60, 75, 90, 120, 150, 180];
impl SleepChoice {
    /// The picker's rows: Off, the end of this song or chapter, the lengths.
    pub fn all() -> Vec<Self> {
        let mut all = vec![Self::Off, Self::EndOfItem];
        all.extend(SLEEP_MINUTES.iter().map(|&m| Self::Minutes(m)));
        all
    }
}
/// A running sleep timer: playback pauses at `until` (after a fade) or when
/// the item playing when it was set (queue entry, chapter) ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SleepTimer {
    pub choice: SleepChoice,
    pub until: Option<std::time::Instant>,
    pub item: (usize, Option<usize>),
}
impl AppModel {
    /// What plays now, as the sleep timer tells items apart: the queue entry
    /// and, in a book, the chapter.
    pub fn sleep_item(&self) -> (usize, Option<usize>) {
        (
            self.queue_position,
            self.book_part()
                .and_then(|(book, part)| book.chapter_at(part, self.position_ms)),
        )
    }
}

/// How an audiobook starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookStart {
    /// Where it was left, a little before.
    Resume,
    /// From its first chapter.
    Beginning,
    /// At the start of chapter `n`.
    Chapter(usize),
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
    LastFm(LastFmRequest),
    Spotify(SpotifyRequest),
    SetScrobbleLog(bool),
    SetLastFmScrobble(bool),
    SetLastFmNowPlaying(bool),
    SetCodecPreference(CodecPreference),
    Play(usize),
    PlayShuffled(usize),
    PlayCollection {
        selected: usize,
        members: Vec<usize>,
        /// Off: as the Shuffle setting says; Songs or Albums: a Shuffle row
        /// (any song or album may come first).
        shuffle: ShuffleMode,
        /// What Up Next calls it ("Night Drive", "Search “bri”").
        source: String,
        /// The page it was played from (Now Playing's Go to <Source>).
        origin: Option<QueueOrigin>,
    },
    PlayQueue(usize),
    /// Play this library song now; what was playing goes on after it.
    PlayNow(usize),
    PlayNext(usize),
    PlayNextCollection(Vec<usize>),
    AddToQueue(usize),
    AddToQueueCollection(Vec<usize>),
    TogglePlayback,
    NextTrack,
    PreviousTrack,
    /// One step of a held Previous/Next: the sign is the direction, the app
    /// sizes the step by how long the button has been held.
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
    /// Read a `ParametricEQ.txt` and play it instead of the five bands.
    ImportEqProfile(PathBuf),
    /// Look for profile files again (the profile list opened).
    ScanEqProfiles,
    SetHpLevel(HpLevel),
    /// The listener agreed to wired volumes above the safe level.
    AllowLoudVolume,
    SetDacFilter(DacFilter),
    SetBrightness(u8),
    SetCrossfade(u32),
    SetBalance(i8),
    SetGapless(bool),
    SetAlbumView(AlbumView),
    SetAlbumOrder(AlbumOrder),
    SetNowPlayingWheel(NowPlayingWheel),
    SetArtistsList(ArtistsList),
    SetWrapAround(WrapAround),
    SetScreenOffButtons(ScreenOffButtons),
    SetHaptics(Haptics),
    SetSpeaker(bool),
    /// Play the audiobook with this key.
    PlayBook(String, BookStart),
    /// Mark the book finished (it starts over) or not.
    SetBookFinished(String, bool),
    /// Forget where the book was left.
    ForgetBook(String),
    /// Read the chapter marks of this book's files (its page opened).
    LoadChapters(String),
    SetAudiobooksOnHome(bool),
    /// The playing book's speed (kept for that book).
    SetBookSpeed(u16),
    SetSleepTimer(SleepChoice),
    SetShuffle(ShuffleMode),
    /// How a playlist's page orders its songs (its file's path).
    SetPlaylistSort(std::path::PathBuf, PlaylistSort),
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
    /// Show (true) or hide one of Home's own places (`HOME_HIDEABLE` key).
    SetHomeShown(String, bool),
    /// Stop the update download in progress (Cancel on the update screen).
    CancelDownload,
    QueueRemove(usize),
    QueueMove {
        index: usize,
        delta: i8,
    },
    /// Remove these Up Next entries (chosen together).
    QueueRemoveEntries(Vec<QueueEntryId>),
    /// These Up Next entries go to the top of your queue, together.
    QueueEntriesNext(Vec<QueueEntryId>),
    /// These Up Next entries go to the end of your queue, together.
    QueueEntriesLast(Vec<QueueEntryId>),
    /// Clear your queue (the songs you queued).
    ClearQueued,
    /// Clear everything after the song playing.
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
        self.queue_queued.clear();
        self.queue_order = self.queue_entry_ids.clone();
        self.queue_source.clear();
        self.queue_origin = None;
        self.queue_position = index;
        self.position_ms = 0;
        // A new queue is no longer the book (starting a book sets it again).
        self.book = None;
        self.invalidate();
        Ok(())
    }
    /// The playing book and which of its parts is current, when the current
    /// queue entry is one of its parts.
    pub fn book_part(&self) -> Option<(&books::Listening, usize)> {
        let book = self.book.as_ref()?;
        Some((book, book.part_of(self.current()?)?))
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
        let id = self.queue_entry_ids.remove(index);
        self.queue_queued.retain(|q| *q != id);
        self.queue_order.retain(|q| *q != id);
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
        let kept = &self.queue_entry_ids;
        self.queue_queued.retain(|q| kept.contains(q));
        self.queue_order.retain(|q| kept.contains(q));
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
        // Back a song: your queued songs still come right after it.
        if delta < 0 {
            self.anchor_queued();
        }
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
                let found = next_entry_id
                    .and_then(|id| self.queue_entry_ids.iter().position(|entry| *entry == id))
                    .or_else(|| {
                        next_track_id
                            .and_then(|id| self.queue.iter().position(|track| track.id == id))
                    });
                match (found, next_entry_id) {
                    (Some(next_position), _) if next_position > self.queue_position => {
                        self.queue_position = next_position;
                        self.position_ms = output_position_ms;
                        self.playback = PlaybackState::Buffering;
                    }
                    // Repeat All: the decoder went on from the end to the start.
                    (Some(_), Some(id)) if self.settings.repeat == RepeatMode::All => {
                        self.wrap_to(id);
                        self.position_ms = output_position_ms;
                        self.playback = PlaybackState::Buffering;
                    }
                    _ => {}
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
                    !t.is_live() && !self.sources.iter().any(|s| s.online && s.id == t.source_id)
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
        if !m.queue.is_empty() && m.queue_position >= m.queue.len() {
            return Err(io::Error::other("invalid saved queue"));
        }
        if m.queue.len() > MAX_QUEUE {
            // Saved by a Reborn with a larger limit: keep the part around
            // the current song.
            let window = queue_window(m.queue.len(), m.queue_position, MAX_QUEUE);
            if m.queue_entry_ids.len() == m.queue.len() {
                m.queue_entry_ids = m.queue_entry_ids[window.clone()].to_vec();
            }
            m.queue_position -= window.start;
            m.queue = m.queue[window].to_vec();
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
        m.settings.prepare_wired_start();
        m.settings.crossfade_ms = m.settings.crossfade_ms.min(30_000);
        m.settings.balance = m.settings.balance.clamp(-MAX_BALANCE, MAX_BALANCE);
        m.settings.eq_bands.truncate(8);
        m.settings.sanitize_eq_profile();
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
    fn only_crossfade_asks_for_the_heavy_playback_class() {
        let mut s = Settings::default();
        assert!(!s.playback_heavy());
        s.eq_enabled = true;
        s.eq_profile = Some(eq::parse("x", "Filter 1: ON PK Fc 100 Hz Gain 3 dB Q 1\n").unwrap());
        s.eq_profile_on = true;
        assert!(s.active_eq().is_some() && !s.playback_heavy());
        s.crossfade_ms = 4000;
        assert!(s.playback_heavy());
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
    /// The output at the chip (dBV) for a level and volume, full-scale signal.
    fn wired_dbv(level: HpLevel, volume: u8) -> f64 {
        level.peak_dbv() + volume_db(volume)
    }
    #[test]
    fn safe_volume_is_the_old_loudest_output_at_every_level() {
        let expected = [
            (HpLevel::Low, 90),
            (HpLevel::Medium, 80),
            (HpLevel::High, 67),
        ];
        for (level, safe) in expected {
            assert_eq!(level.safe_volume(), safe, "{level:?}");
            assert!(wired_dbv(level, safe) <= SAFE_PEAK_DBV + 1e-9);
            assert!(wired_dbv(level, safe + 1) > SAFE_PEAK_DBV);
        }
        // Full volume: about 0.35, 0.71 and 1.73 Vrms.
        let volts = |level: HpLevel| 10f64.powf(level.peak_dbv() / 20.0);
        assert!((volts(HpLevel::Low) - 0.355).abs() < 0.005);
        assert!((volts(HpLevel::Medium) - 0.706).abs() < 0.005);
        assert!((volts(HpLevel::High) - 1.73).abs() < 0.005);
        let mut s = Settings::default();
        assert_eq!(s.wired_volume_limit(), 90);
        s.loud_ok = true;
        assert_eq!(s.wired_volume_limit(), 100);
    }
    #[test]
    fn an_old_scale_volume_moves_to_the_same_output_and_never_louder() {
        let path = std::env::temp_dir().join(format!("reborn-volume-{}.json", std::process::id()));
        let restore = |level: HpLevel, volume: u8, wired: Option<u8>, scale: Option<u8>| {
            let mut model = AppModel::default();
            model.settings.hp_level = level;
            model.settings.volume = volume;
            model.settings.wired_volume = wired;
            model.settings.loud_ok = true;
            let mut value = serde_json::to_value(&model).unwrap();
            match scale {
                Some(s) => value["settings"]["volume_scale"] = s.into(),
                None => {
                    value["settings"]
                        .as_object_mut()
                        .unwrap()
                        .remove("volume_scale");
                }
            }
            fs::write(
                &path,
                serde_json::to_vec(&json!({"schema":3,"model":value})).unwrap(),
            )
            .unwrap();
            AppModel::restore(&path).unwrap().settings
        };
        for level in HpLevel::ALL {
            for old in [1u8, 10, 25, 50, 75, 90, 100] {
                let s = restore(level, old, None, None);
                assert_eq!(s.volume_scale, VOLUME_SCALE);
                // Old: squared scale, Master -19.5 dB.
                let before =
                    level.full_scale_dbv() - 19.5 + 40.0 * (f64::from(old) / 100.0).log10();
                let after = wired_dbv(level, s.volume);
                assert!(
                    after <= before + 1e-9 || s.volume == 1,
                    "{level:?} {old}: {after} > {before}"
                );
                assert!(
                    before - after < 0.6 + 1e-9 || old < 10,
                    "{level:?} {old}: {before} → {after}"
                );
            }
            assert_eq!(restore(level, 0, None, None).volume, 0);
        }
        assert_eq!(restore(HpLevel::High, 100, None, None).volume, 67);
        assert_eq!(restore(HpLevel::Low, 100, None, None).volume, 82);
        // Saved while headphones held the volume: the wired one comes back.
        assert_eq!(
            restore(HpLevel::Low, 90, Some(40), Some(VOLUME_SCALE)).volume,
            40
        );
        // Already on the new scale: kept, but the start is never above the
        // safe level, even after the listener agreed to more.
        let s = restore(HpLevel::High, 100, None, Some(VOLUME_SCALE));
        assert_eq!((s.volume, s.loud_ok), (67, true));
        assert_eq!(
            restore(HpLevel::Low, 50, None, Some(VOLUME_SCALE)).volume,
            50
        );
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn a_level_change_keeps_the_output_where_the_volume_reaches_it() {
        for from in HpLevel::ALL {
            for to in HpLevel::ALL {
                for volume in [5u8, 30, 60, 67, 90, 100] {
                    let s = Settings {
                        hp_level: to,
                        ..Settings::default()
                    };
                    let moved = s.volume_for_level(volume, from);
                    let before = wired_dbv(from, volume);
                    let after = wired_dbv(to, moved);
                    // Volume 1 is the floor (−81 dB): a quieter target stays there.
                    assert!(
                        after <= before + 1e-9 || moved == 1,
                        "{from:?}→{to:?} {volume}"
                    );
                    if (10..100).contains(&moved) {
                        assert!(before - after < 0.6 + 1e-9, "{from:?}→{to:?} {volume}");
                    }
                }
                let s = Settings {
                    hp_level: to,
                    ..Settings::default()
                };
                assert_eq!(s.volume_for_level(0, from), 0);
            }
        }
        // Low → High at the safe volume stays at the safe output.
        let high = Settings {
            hp_level: HpLevel::High,
            ..Settings::default()
        };
        assert_eq!(high.volume_for_level(90, HpLevel::Low), 67);
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
            unreadable: 0,
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
        assert!(old.home_hidden.is_empty());
    }

    #[test]
    fn queue_origin_is_kept_old_sessions_have_none_and_a_new_queue_forgets_it() {
        let mut m = AppModel {
            queue_origin: Some(QueueOrigin {
                screen: Screen::Playlist,
                filter: "/data/music/Playlists/Road Trip.m3u8".into(),
            }),
            ..Default::default()
        };
        let saved = serde_json::to_value(&m).unwrap();
        let back: AppModel = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(back.queue_origin, m.queue_origin);
        let mut old = saved;
        old.as_object_mut().unwrap().remove("queue_origin");
        let old: AppModel = serde_json::from_value(old).unwrap();
        assert!(old.queue_origin.is_none());
        m.replace_queue(vec![Track::default()], 0).unwrap();
        assert!(m.queue_origin.is_none());
    }

    #[test]
    fn home_places_hide_in_home_order_and_the_fixed_ones_stay() {
        let mut s = Settings::default();
        assert!(s.home_shows("radio"));
        s.show_on_home("search", false);
        s.show_on_home("radio", false);
        s.show_on_home("settings", false);
        s.show_on_home("now_playing", false);
        assert_eq!(s.home_hidden, ["radio", "search"]);
        assert!(!s.home_shows("radio") && s.home_shows("music"));
        s.show_on_home("radio", true);
        assert_eq!(s.home_hidden, ["search"]);
        let saved = serde_json::to_value(&s).unwrap();
        let back: Settings = serde_json::from_value(saved).unwrap();
        assert_eq!(back.home_hidden, ["search"]);
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

#[cfg(test)]
mod sleep_timer_tests {
    use super::*;
    #[test]
    fn the_sleep_timer_offers_off_the_end_of_the_item_and_lengths_to_three_hours() {
        let all = SleepChoice::all();
        assert_eq!(all.len(), 14);
        assert_eq!(
            all[..3],
            [
                SleepChoice::Off,
                SleepChoice::EndOfItem,
                SleepChoice::Minutes(5)
            ]
        );
        assert_eq!(all.last(), Some(&SleepChoice::Minutes(180)));
        assert!(SLEEP_MINUTES.windows(2).all(|w| w[0] < w[1]));
    }
}

#[cfg(test)]
mod queue_window_tests {
    use super::{queue_window, AppModel, Track, MAX_QUEUE};

    #[test]
    fn a_large_collection_queues_a_window_around_the_selected_song() {
        assert_eq!(queue_window(300, 120, MAX_QUEUE), 0..300);
        assert_eq!(queue_window(28_600, 0, 5_000), 0..5_000);
        assert_eq!(queue_window(28_600, 10_000, 5_000), 9_500..14_500);
        assert_eq!(queue_window(28_600, 28_599, 5_000), 23_600..28_600);
        for selected in [0, 499, 500, 14_000, 28_599] {
            let w = queue_window(28_600, selected, 5_000);
            assert_eq!(w.len(), 5_000);
            assert!(w.contains(&selected));
        }
    }

    #[test]
    fn a_saved_queue_over_the_limit_is_trimmed_not_lost() {
        let path = std::env::temp_dir().join(format!("reborn-queue-{}.json", std::process::id()));
        let m = AppModel {
            queue: (0..MAX_QUEUE as i64 + 3_000)
                .map(|id| Track {
                    id,
                    ..Default::default()
                })
                .collect(),
            queue_position: 6_000,
            ..Default::default()
        };
        // Written by hand: checkpoint() would refuse this size.
        let bytes = serde_json::to_vec(
            &serde_json::json!({"schema": super::SESSION_SCHEMA_VERSION, "model": m}),
        )
        .unwrap();
        std::fs::write(&path, bytes).unwrap();
        let restored = AppModel::restore(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(restored.queue.len(), MAX_QUEUE);
        assert_eq!(restored.current().unwrap().id, 6_000);
        assert_eq!(restored.queue_entry_ids.len(), MAX_QUEUE);
    }
}

#[cfg(test)]
mod artist_split_tests {
    use super::{joined_names, split_artists, split_genres};

    #[test]
    fn joined_genres_split_and_joined_names_read_with_commas() {
        assert_eq!(split_genres("Jazz;R&B"), ["Jazz", "R&B"]);
        assert_eq!(split_genres(" Jazz ; Soul;Jazz"), ["Jazz", "Soul"]);
        assert_eq!(split_genres("Rock/Pop"), ["Rock/Pop"]);
        assert_eq!(split_genres(""), [""], "one No Genre");
        assert_eq!(split_genres(";"), [""]);
        assert_eq!(
            joined_names("Lauryn Hill;Mary J. Blige"),
            "Lauryn Hill, Mary J. Blige"
        );
        assert_eq!(joined_names("A; B ;"), "A, B");
        assert_eq!(joined_names("Simon & Garfunkel"), "Simon & Garfunkel");
        assert!(matches!(
            joined_names("Kelis"),
            std::borrow::Cow::Borrowed("Kelis")
        ));
    }

    #[test]
    fn joined_tags_and_guests_split_but_band_names_stay() {
        assert_eq!(
            split_artists("Calvin Harris;Kelis"),
            ["Calvin Harris", "Kelis"]
        );
        assert_eq!(
            split_artists("Calvin Harris; Dillon Francis ;Dizzee Rascal"),
            ["Calvin Harris", "Dillon Francis", "Dizzee Rascal"]
        );
        assert_eq!(
            split_artists("Calvin Harris Feat. Kelis"),
            ["Calvin Harris", "Kelis"]
        );
        assert_eq!(
            split_artists("Daft Punk ft. Pharrell Williams featuring Nile Rodgers"),
            ["Daft Punk", "Pharrell Williams", "Nile Rodgers"]
        );
        assert_eq!(split_artists("Simon & Garfunkel"), ["Simon & Garfunkel"]);
        assert_eq!(split_artists("Earth, Wind & Fire"), ["Earth, Wind & Fire"]);
        assert_eq!(
            split_artists("Soft Cell"),
            ["Soft Cell"],
            "\"ft\" only as a word"
        );
        assert_eq!(split_artists("Kelis;Kelis"), ["Kelis"]);
        assert_eq!(split_artists(""), [""], "one unknown artist");
        assert_eq!(
            split_artists("宇多田ヒカル feat. 椎名林檎"),
            ["宇多田ヒカル", "椎名林檎"]
        );
    }
}
