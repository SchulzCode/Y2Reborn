#![forbid(unsafe_code)]
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-baseline.01");
pub const MAX_QUEUE: usize = 20_000;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    Select,
    Back,
    Menu,
    PlayPause,
    Previous,
    Next,
    VolumeUp,
    VolumeDown,
    Wheel(i32),
    ScreenToggle,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    #[default]
    Main,
    Music,
    Artists,
    Albums,
    Tracks,
    Folders,
    NowPlaying,
    Bluetooth,
    Wifi,
    Diagnostics,
    Settings,
    TextEntry,
    Pairing,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub volume: u8,
    pub screen_timeout_seconds: u32,
    pub music_directory: PathBuf,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 35,
            screen_timeout_seconds: 60,
            music_directory: "/data/music".into(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppModel {
    pub playback: PlaybackState,
    pub queue: Vec<Track>,
    pub queue_position: usize,
    pub position_ms: u64,
    pub output: AudioOutput,
    pub screen: Screen,
    pub settings: Settings,
    pub generation: u64,
    pub sources: Vec<Source>,
    pub screen_off: bool,
    pub last_error: Option<String>,
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
    TrackStarted { generation: u64 },
    TrackEnded { generation: u64 },
    Position { generation: u64, ms: u64 },
    PlaybackError { generation: u64, message: String },
    Input(Action),
    SourceChanged(Vec<Source>),
    BluetoothDisconnected(String),
    WifiConnected,
    WifiDisconnected,
    PowerChanged,
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
        self.queue_position = index;
        self.position_ms = 0;
        self.invalidate();
        Ok(())
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
                self.invalidate();
            }
            _ => {}
        }
    }
    pub fn restore(path: &Path) -> io::Result<Self> {
        use std::io::Read;
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(io::Error::other("state too large"));
        }
        let mut m: Self = serde_json::from_slice(&bytes)?;
        if m.queue.len() > MAX_QUEUE || (!m.queue.is_empty() && m.queue_position >= m.queue.len()) {
            return Err(io::Error::other("invalid saved queue"));
        }
        m.playback = if m.current().is_some() {
            PlaybackState::Paused
        } else {
            PlaybackState::Stopped
        };
        m.screen_off = false;
        m.generation = 0;
        m.settings.volume = m.settings.volume.min(100);
        Ok(m)
    }
    pub fn checkpoint(&self, path: &Path) -> io::Result<()> {
        atomic_write(path, &serde_json::to_vec(self)?)
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
}
