#![forbid(unsafe_code)]

use font8x8::UnicodeFonts;
use reborn_core::{
    Action, AppModel, AudioOutput, ConfirmAction, Modal, RadioScan, RepeatMode, ReplayGainMode,
    Screen, Track,
};
use reborn_graphics::Quad;
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

pub use reborn_core::Effect;

const BG: u32 = 0x090d12ff;
const SURFACE: u32 = 0x111820ff;
const RAISED: u32 = 0x18222cff;
const FOCUS: u32 = 0x29251dff;
const PRIMARY: u32 = 0xf5f2ebff;
const SECONDARY: u32 = 0xaeb7c3ff;
const MUTED: u32 = 0x707a87ff;
const GOLD: u32 = 0xe9bc68ff;
const SUCCESS: u32 = 0x86c39fff;
const DANGER: u32 = 0xdb7c70ff;
const DIVIDER: u32 = 0x2b3440ff;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub key: String,
    pub secondary: String,
    pub enabled: bool,
}

impl Item {
    pub fn new(label: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            key: key.into(),
            secondary: String::new(),
            enabled: true,
        }
    }

    pub fn with_secondary(mut self, secondary: impl Into<String>) -> Self {
        self.secondary = secondary.into();
        self
    }
}

#[derive(Default, Clone)]
pub struct RadioView {
    pub available: bool,
    pub powered: bool,
    pub scan: RadioScan,
    pub error: Option<String>,
    pub connection: String,
    pub count: usize,
}

impl RadioView {
    pub fn requested(&mut self) {
        self.scan = RadioScan::Starting;
        self.error = None;
    }

    pub fn failed(&mut self, error: String) {
        if self.scan.active() {
            self.scan = RadioScan::Failed {
                message: error.clone(),
            };
        }
        self.error = Some(error);
    }

    fn message(&self, bluetooth: bool) -> String {
        let name = if bluetooth { "Bluetooth" } else { "Wi-Fi" };
        match self.scan {
            RadioScan::Starting => return format!("Starting {name}…"),
            RadioScan::Scanning => {
                return format!(
                    "Scanning… {} {}",
                    self.count,
                    if bluetooth { "devices" } else { "networks" }
                )
            }
            RadioScan::Complete { found: 0 } => {
                return if bluetooth {
                    "No devices found. Put headphones in pairing mode.".into()
                } else {
                    "No networks found. Select Scan to try again.".into()
                }
            }
            RadioScan::Complete { found } => return format!("Scan complete · {found} found"),
            RadioScan::Failed { .. } => return "Could not complete the scan. Try again.".into(),
            RadioScan::Idle => {}
        }
        if !self.available {
            return format!("{name} is starting");
        }
        if !self.powered {
            return format!("{name} is off");
        }
        if !self.connection.is_empty() {
            return self.connection.clone();
        }
        format!("{name} is on")
    }
}

/// Product-facing UI state. It owns only presentation data that is not
/// authoritative elsewhere (radio result lists, password entry, artwork
/// availability, and short-lived notices). Navigation/modal state lives in
/// `AppModel::navigation` so it is explicit and testable.
#[derive(Default)]
pub struct Ui {
    pub notice: String,
    pub notice_until: Option<Instant>,
    pub wifi: RadioView,
    pub bluetooth: RadioView,
    pub networks: Vec<Item>,
    pub saved_networks: Vec<Item>,
    pub bluetooth_devices: Vec<Item>,
    pub pairing: Option<String>,
    password: String,
    ssid: String,
    letter: usize,
    pub text_entry: bool,
}

const LETTERS: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 !@#$%^&*()-_=+[]{};:'\",.<>/?\\|`~";

impl Ui {
    pub fn flash(&mut self, message: impl Into<String>) {
        self.notice = message.into();
        self.notice_until = Some(Instant::now() + Duration::from_millis(1600));
    }

    pub fn expire_notice(&mut self) {
        if self
            .notice_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.notice.clear();
            self.notice_until = None;
        }
    }

    fn go(m: &mut AppModel, screen: Screen, filter: impl Into<String>) {
        if m.screen != screen {
            m.navigation.stack.push(m.screen);
        }
        m.screen = screen;
        m.navigation.focus = 0;
        m.navigation.scroll = 0;
        m.navigation.filter = filter.into();
        m.navigation.modal = None;
        m.navigation.modal_focus = 0;
        m.navigation.context_target = None;
    }

    fn back(m: &mut AppModel) {
        if m.navigation.modal.is_some() {
            m.navigation.modal = None;
            m.navigation.modal_focus = 0;
            m.navigation.context_target = None;
            return;
        }
        if m.screen == Screen::TextEntry {
            return;
        }
        if let Some(previous) = m.navigation.stack.pop() {
            m.screen = previous;
            m.navigation.focus = 0;
            m.navigation.scroll = 0;
            m.navigation.filter.clear();
        }
    }

    fn home(m: &mut AppModel) {
        m.navigation.stack.clear();
        m.screen = Screen::Home;
        m.navigation.focus = 0;
        m.navigation.scroll = 0;
        m.navigation.filter.clear();
        m.navigation.modal = None;
        m.navigation.context_target = None;
    }

    pub fn rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        match m.screen {
            Screen::Home => vec![
                Item::new("Music", "music").with_secondary("Artists · Albums · Tracks"),
                Item::new("Now Playing", "now_playing").with_secondary("Open the player"),
                Item::new("Queue", "queue").with_secondary("Up next"),
                Item::new("Connectivity", "connectivity").with_secondary("Bluetooth · Wi-Fi"),
                Item::new("Settings", "settings").with_secondary("Audio · Playback · System"),
            ],
            Screen::Music => vec![
                Item::new("Artists", "artists"),
                Item::new("Albums", "albums"),
                Item::new("Tracks", "tracks"),
                Item::new("Folders", "folders"),
                Item::new("Scan Library", "scan_library").with_secondary(if m.library.scanning {
                    "Scanning…"
                } else {
                    "Find new music"
                }),
            ],
            Screen::Artists | Screen::Albums | Screen::Folders => self.catalog_rows(m, tracks),
            Screen::Artist => {
                let name = m
                    .navigation
                    .filter
                    .strip_prefix("artist:")
                    .unwrap_or("Artist");
                let mut rows = vec![
                    Item::new(format!("Play {name}"), "play:collection"),
                    Item::new("Tracks", "tracks:collection"),
                ];
                rows.extend(self.filtered_track_rows(m, tracks));
                rows
            }
            Screen::Album => {
                let name = m
                    .navigation
                    .filter
                    .strip_prefix("album:")
                    .unwrap_or("Album");
                let mut rows = vec![
                    Item::new(format!("Play {name}"), "play:collection"),
                    Item::new("Shuffle Album", "shuffle:collection"),
                    Item::new("Play Next", "play_next:collection"),
                    Item::new("Add to Queue", "add:collection"),
                ];
                rows.extend(self.filtered_track_rows(m, tracks));
                rows
            }
            Screen::Tracks => self.filtered_track_rows(m, tracks),
            Screen::Queue => self.queue_rows(m),
            Screen::Connectivity => vec![
                Item::new("Bluetooth", "bluetooth").with_secondary(self.bluetooth_summary()),
                Item::new("Wi-Fi", "wifi").with_secondary(self.wifi_summary()),
            ],
            Screen::Bluetooth | Screen::SettingsBluetooth => self.bluetooth_rows(),
            Screen::Wifi | Screen::SettingsWifi => self.wifi_rows(),
            Screen::Settings => vec![
                Item::new("Audio", "audio").with_secondary("Output · ReplayGain · EQ"),
                Item::new("Playback", "playback").with_secondary("Gapless · Shuffle · Repeat"),
                Item::new("Library", "library").with_secondary("Storage · Scan"),
                Item::new("Bluetooth", "bluetooth"),
                Item::new("Wi-Fi", "wifi"),
                Item::new("Display", "display").with_secondary("Screen timeout"),
                Item::new("Power", "power"),
                Item::new("System", "system").with_secondary("About · Diagnostics"),
            ],
            Screen::SettingsAudio => vec![
                Item::new("Output", "output").with_secondary(output_label(&m.output)),
                Item::new("ReplayGain", "replay_gain")
                    .with_secondary(replay_gain_label(m.settings.replay_gain)),
                Item::new("Equalizer", "equalizer").with_secondary(if m.settings.eq_enabled {
                    "On"
                } else {
                    "Off"
                }),
                Item::new("Audio Information", "audio_info"),
            ],
            Screen::SettingsPlayback => vec![
                Item::new("Shuffle", "shuffle").with_secondary(if m.settings.shuffle {
                    "On"
                } else {
                    "Off"
                }),
                Item::new("Repeat", "repeat").with_secondary(repeat_label(m.settings.repeat)),
                Item::new("Gapless Playback", "gapless").with_secondary(
                    if m.settings.gapless_enabled {
                        "On"
                    } else {
                        "Off"
                    },
                ),
                Item::new("Crossfade", "crossfade")
                    .with_secondary(crossfade_label(m.settings.crossfade_ms)),
            ],
            Screen::SettingsLibrary => vec![
                Item::new("Internal Storage", "internal").with_secondary("Music library"),
                Item::new("SD Card", "sd").with_secondary(
                    if m.sources.iter().any(|s| s.id == "sd" && s.online) {
                        "Available"
                    } else {
                        "Not inserted"
                    },
                ),
                Item::new("Scan Library", "scan_library"),
                Item::new("Rebuild Library", "rebuild_library"),
            ],
            Screen::SettingsDisplay => vec![Item::new("Screen Timeout", "timeout")
                .with_secondary(timeout_label(m.settings.screen_timeout_seconds))],
            Screen::SettingsPower => vec![Item::new("Power Menu", "power_menu")],
            Screen::SettingsSystem => vec![
                Item::new("About Reborn", "about"),
                Item::new("Diagnostics", "diagnostics"),
                Item::new("Reboot", "reboot"),
                Item::new("Power Off", "power_off"),
            ],
            Screen::Diagnostics => vec![
                Item::new("Audio", "diag_audio"),
                Item::new("Storage", "diag_storage"),
                Item::new("Bluetooth", "diag_bluetooth"),
                Item::new("Wi-Fi", "diag_wifi"),
                Item::new("System", "diag_system"),
            ],
            Screen::TextEntry | Screen::Pairing => vec![],
            Screen::NowPlaying => vec![
                Item::new("Previous Track", "previous"),
                Item::new("Play / Pause", "toggle"),
                Item::new("Next Track", "next"),
            ],
        }
    }

    fn catalog_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        let mut values = BTreeSet::new();
        for track in tracks.iter().filter(|track| track.online) {
            let value = match m.screen {
                Screen::Artists => track.artist.clone(),
                Screen::Albums => track.album.clone(),
                Screen::Folders => track
                    .path
                    .parent()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                _ => String::new(),
            };
            values.insert(if value.is_empty() {
                "Unknown".into()
            } else {
                value
            });
        }
        values
            .into_iter()
            .map(|value| {
                let prefix = match m.screen {
                    Screen::Artists => "artist:",
                    Screen::Albums => "album:",
                    _ => "folder:",
                };
                Item::new(value.clone(), format!("{prefix}{value}"))
            })
            .collect()
    }

    fn filtered_track_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track_matches(track, &m.navigation.filter))
            .map(|(index, track)| {
                let title = if track.title.is_empty() {
                    track.filename.clone()
                } else {
                    track.title.clone()
                };
                Item::new(title, format!("track:{index}")).with_secondary(format!(
                    "{} · {}",
                    display_or_unknown(&track.artist),
                    display_or_unknown(&track.album)
                ))
            })
            .collect()
    }

    fn queue_rows(&self, m: &AppModel) -> Vec<Item> {
        if m.queue.is_empty() {
            return vec![];
        }
        m.queue
            .iter()
            .enumerate()
            .map(|(index, track)| {
                let title = if track.title.is_empty() {
                    track.filename.clone()
                } else {
                    track.title.clone()
                };
                let section = if index == m.queue_position {
                    "Now Playing"
                } else if index > m.queue_position {
                    "Up Next"
                } else {
                    "Played"
                };
                Item::new(title, format!("queue:{index}"))
                    .with_secondary(format!("{section} · {}", display_or_unknown(&track.artist)))
            })
            .collect()
    }

    fn bluetooth_rows(&self) -> Vec<Item> {
        let mut rows = vec![
            Item::new(
                if self.bluetooth.powered {
                    "Turn Bluetooth Off"
                } else {
                    "Turn Bluetooth On"
                },
                "bt_power",
            ),
            Item::new("Scan for Devices", "bt_scan"),
            Item::new("Use Wired Output", "wired_output"),
        ];
        rows.extend(self.bluetooth_devices.iter().cloned().map(|device| {
            Item::new(device.label, format!("bt_device:{}", device.key))
                .with_secondary("Select for options")
        }));
        rows
    }

    fn wifi_rows(&self) -> Vec<Item> {
        let mut rows = vec![
            Item::new(
                if self.wifi.powered {
                    "Turn Wi-Fi Off"
                } else {
                    "Turn Wi-Fi On"
                },
                "wifi_power",
            ),
            Item::new("Scan for Networks", "wifi_scan"),
        ];
        rows.extend(self.saved_networks.iter().cloned());
        rows.extend(self.networks.iter().cloned());
        rows
    }

    fn bluetooth_summary(&self) -> String {
        if !self.bluetooth.powered {
            "Off".into()
        } else if !self.bluetooth.connection.is_empty() {
            self.bluetooth.connection.clone()
        } else {
            "On · No device".into()
        }
    }

    fn wifi_summary(&self) -> String {
        if !self.wifi.powered {
            "Off".into()
        } else if !self.wifi.connection.is_empty() {
            self.wifi.connection.clone()
        } else {
            "On · Not connected".into()
        }
    }

    fn context_rows(&self, m: &AppModel) -> Vec<Item> {
        match m.screen {
            Screen::Album if m.navigation.context_target.is_some_and(|target| target < 4) => vec![
                Item::new("Play Album", "album_play"),
                Item::new("Shuffle Album", "album_shuffle"),
                Item::new("Play Next", "album_next"),
                Item::new("Add to Queue", "album_queue"),
                Item::new("Album Information", "album_info"),
            ],
            Screen::Artist if m.navigation.context_target.is_some_and(|target| target < 2) => {
                vec![]
            }
            Screen::Tracks | Screen::Artist | Screen::Album | Screen::NowPlaying => vec![
                Item::new("Play", "play"),
                Item::new("Play Next", "play_next"),
                Item::new("Add to Queue", "add_queue"),
                Item::new("Go to Album", "go_album"),
                Item::new("Go to Artist", "go_artist"),
                Item::new("Track Information", "track_info"),
            ],
            Screen::Queue => vec![
                Item::new("Play", "queue_play"),
                Item::new("Remove from Queue", "queue_remove"),
                Item::new("Move Up", "queue_up"),
                Item::new("Move Down", "queue_down"),
                Item::new("Clear Queue", "clear_queue"),
            ],
            Screen::Bluetooth | Screen::SettingsBluetooth => {
                let target = m.navigation.context_target.unwrap_or(0);
                if target >= 3 && target - 3 < self.bluetooth_devices.len() {
                    let connected = self
                        .bluetooth_devices
                        .get(target - 3)
                        .is_some_and(|item| item.label.contains("connected"));
                    vec![
                        Item::new(
                            if connected { "Disconnect" } else { "Connect" },
                            "bt_connect",
                        ),
                        Item::new("Use for Audio", "bt_output"),
                        Item::new("Forget Device", "bt_forget"),
                    ]
                } else {
                    vec![]
                }
            }
            Screen::Wifi | Screen::SettingsWifi => {
                let target = m.navigation.context_target.unwrap_or(0);
                if target >= 2 && self.saved_networks.get(target - 2).is_some() {
                    vec![
                        Item::new("Connect", "wifi_connect"),
                        Item::new("Forget Network", "wifi_forget"),
                    ]
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }

    fn modal_rows(&self, m: &AppModel) -> Vec<Item> {
        match m.navigation.modal {
            Some(Modal::ContextMenu) => self.context_rows(m),
            Some(Modal::PowerMenu) => vec![
                Item::new("Power Off", "power_off"),
                Item::new("Reboot", "reboot"),
                Item::new("Cancel", "cancel"),
            ],
            Some(Modal::Confirm(_)) => vec![
                Item::new("Confirm", "confirm"),
                Item::new("Cancel", "cancel"),
            ],
            None => vec![],
        }
    }

    /// Convert a semantic action into a pure service request and/or a
    /// navigation transition. No playback, radio, or platform call occurs in
    /// this method.
    pub fn action(&mut self, m: &mut AppModel, tracks: &[Track], action: Action) -> Effect {
        if self.pairing.is_some() {
            return match action {
                Action::Select => {
                    self.pairing = None;
                    Effect::ConfirmPairing(true)
                }
                Action::Back => {
                    self.pairing = None;
                    Effect::ConfirmPairing(false)
                }
                _ => Effect::None,
            };
        }
        if self.text_entry {
            return self.entry(action);
        }
        match action {
            Action::ScreenSleep => return Effect::ScreenSleep,
            Action::ScreenWake => return Effect::ScreenWake,
            Action::PlayPause => return Effect::TogglePlayback,
            Action::ShowNowPlaying => {
                Self::go(m, Screen::NowPlaying, "");
                return Effect::None;
            }
            Action::PreviousTrack => return Effect::PreviousTrack,
            Action::NextTrack => return Effect::NextTrack,
            Action::SeekBackward => return Effect::Seek(-30_000),
            Action::SeekForward => return Effect::Seek(30_000),
            Action::VolumeUp => return Effect::AdjustVolume(2),
            Action::VolumeDown => return Effect::AdjustVolume(-2),
            Action::Home => {
                Self::home(m);
                return Effect::None;
            }
            Action::Back => {
                Self::back(m);
                return Effect::None;
            }
            Action::PowerMenu => {
                m.navigation.modal = Some(Modal::PowerMenu);
                m.navigation.modal_focus = 2;
                return Effect::None;
            }
            Action::ContextMenu => {
                if m.navigation.modal.is_some() {
                    return Effect::None;
                }
                let rows = self.rows(m, tracks);
                if rows.get(m.navigation.focus).is_some_and(|row| row.enabled)
                    && !self.context_rows(m).is_empty()
                    && !matches!(m.screen, Screen::Home | Screen::Settings)
                {
                    m.navigation.modal = Some(Modal::ContextMenu);
                    m.navigation.modal_focus = 0;
                    m.navigation.context_target = Some(m.navigation.focus);
                }
                return Effect::None;
            }
            Action::WheelClockwise(steps) => {
                if m.screen == Screen::NowPlaying {
                    return Effect::AdjustVolume((steps as i8).saturating_mul(2));
                }
                self.move_focus(m, tracks, steps as i32);
                return Effect::None;
            }
            Action::WheelCounterClockwise(steps) => {
                if m.screen == Screen::NowPlaying {
                    return Effect::AdjustVolume(-((steps as i8).saturating_mul(2)));
                }
                self.move_focus(m, tracks, -(steps as i32));
                return Effect::None;
            }
            Action::NavigateUp => {
                self.move_focus(m, tracks, -1);
                return Effect::None;
            }
            Action::NavigateDown => {
                self.move_focus(m, tracks, 1);
                return Effect::None;
            }
            Action::NavigateLeft | Action::NavigateRight => return Effect::None,
            Action::Select => {}
        }
        if m.navigation.modal.is_some() {
            return self.modal_action(m, tracks);
        }
        let rows = self.rows(m, tracks);
        let Some(row) = rows.get(m.navigation.focus).filter(|row| row.enabled) else {
            return Effect::None;
        };
        let key = row.key.clone();
        self.select_row(m, tracks, &key)
    }

    fn move_focus(&mut self, m: &mut AppModel, tracks: &[Track], delta: i32) {
        let rows = if m.navigation.modal.is_some() {
            self.modal_rows(m)
        } else {
            self.rows(m, tracks)
        };
        let Some(last) = rows.len().checked_sub(1) else {
            return;
        };
        let focus = if m.navigation.modal.is_some() {
            &mut m.navigation.modal_focus
        } else {
            &mut m.navigation.focus
        };
        *focus = (*focus as i32 + delta).clamp(0, last as i32) as usize;
        if m.navigation.modal.is_none() {
            m.navigation.scroll = m.navigation.scroll.min(*focus);
        }
    }

    fn select_row(&mut self, m: &mut AppModel, tracks: &[Track], key: &str) -> Effect {
        match m.screen {
            Screen::Home => match key {
                "music" => Self::go(m, Screen::Music, ""),
                "now_playing" => Self::go(m, Screen::NowPlaying, ""),
                "queue" => Self::go(m, Screen::Queue, ""),
                "connectivity" => Self::go(m, Screen::Connectivity, ""),
                "settings" => Self::go(m, Screen::Settings, ""),
                _ => {}
            },
            Screen::Music => match key {
                "artists" => Self::go(m, Screen::Artists, ""),
                "albums" => Self::go(m, Screen::Albums, ""),
                "tracks" => Self::go(m, Screen::Tracks, ""),
                "folders" => Self::go(m, Screen::Folders, ""),
                "scan_library" => return Effect::ScanLibrary,
                _ => {}
            },
            Screen::Artists => Self::go(m, Screen::Artist, key),
            Screen::Albums => Self::go(m, Screen::Album, key),
            Screen::Folders => Self::go(m, Screen::Tracks, key),
            Screen::Artist | Screen::Album => {
                if key.starts_with("track:") {
                    return self.play_track(m, tracks, key);
                }
                if key == "play:collection" {
                    return self.play_first_filtered(m, tracks);
                }
                if key == "shuffle:collection" {
                    return self.play_first_filtered_shuffled(m, tracks);
                }
                if key == "play_next:collection" {
                    return self
                        .first_filtered_index(m, tracks)
                        .map(Effect::PlayNext)
                        .unwrap_or(Effect::None);
                }
                if key == "add:collection" {
                    return self
                        .first_filtered_index(m, tracks)
                        .map(Effect::AddToQueue)
                        .unwrap_or(Effect::None);
                }
            }
            Screen::Tracks => {
                if key.starts_with("track:") {
                    return self.play_track(m, tracks, key);
                }
            }
            Screen::Queue => {
                if let Some(index) = key
                    .strip_prefix("queue:")
                    .and_then(|v| v.parse::<usize>().ok())
                {
                    if let Some(track) = m.queue.get(index) {
                        if let Some(library_index) =
                            tracks.iter().position(|t: &Track| t.id == track.id)
                        {
                            Self::go(m, Screen::NowPlaying, "");
                            return Effect::Play(library_index);
                        }
                    }
                }
            }
            Screen::Connectivity => match key {
                "bluetooth" => Self::go(m, Screen::Bluetooth, ""),
                "wifi" => Self::go(m, Screen::Wifi, ""),
                _ => {}
            },
            Screen::Bluetooth | Screen::SettingsBluetooth => return self.bluetooth_action(m, key),
            Screen::Wifi | Screen::SettingsWifi => return self.wifi_action(m, key),
            Screen::Settings => match key {
                "audio" => Self::go(m, Screen::SettingsAudio, ""),
                "playback" => Self::go(m, Screen::SettingsPlayback, ""),
                "library" => Self::go(m, Screen::SettingsLibrary, ""),
                "bluetooth" => Self::go(m, Screen::SettingsBluetooth, ""),
                "wifi" => Self::go(m, Screen::SettingsWifi, ""),
                "display" => Self::go(m, Screen::SettingsDisplay, ""),
                "power" => Self::go(m, Screen::SettingsPower, ""),
                "system" => Self::go(m, Screen::SettingsSystem, ""),
                _ => {}
            },
            Screen::SettingsAudio => match key {
                "output" => {
                    if matches!(m.output, AudioOutput::Bluetooth(_)) {
                        return Effect::Output(AudioOutput::Wired);
                    }
                    if let Some(path) = self
                        .bluetooth_devices
                        .iter()
                        .find(|item| item.label.contains("connected"))
                        .map(|item| item.key.clone())
                    {
                        return Effect::BluetoothDevice {
                            path,
                            operation: "output".into(),
                        };
                    }
                    self.notice = "Connect Bluetooth headphones first".into();
                }
                "replay_gain" => {
                    let next = match m.settings.replay_gain {
                        ReplayGainMode::Off => ReplayGainMode::Track,
                        ReplayGainMode::Track => ReplayGainMode::Album,
                        ReplayGainMode::Album => ReplayGainMode::Off,
                    };
                    return Effect::SetReplayGain(next);
                }
                "equalizer" => return Effect::ToggleEq,
                "audio_info" => Self::go(m, Screen::Diagnostics, "audio"),
                _ => {}
            },
            Screen::SettingsPlayback => match key {
                "shuffle" => return Effect::SetShuffle(!m.settings.shuffle),
                "repeat" => {
                    let next = match m.settings.repeat {
                        RepeatMode::Off => RepeatMode::Track,
                        RepeatMode::Track => RepeatMode::All,
                        RepeatMode::All => RepeatMode::Off,
                    };
                    return Effect::SetRepeat(next);
                }
                "gapless" => return Effect::SetGapless(!m.settings.gapless_enabled),
                "crossfade" => {
                    let next = match m.settings.crossfade_ms {
                        0 => 5_000,
                        5_000 => 10_000,
                        10_000 => 15_000,
                        _ => 0,
                    };
                    return Effect::SetCrossfade(next);
                }
                _ => {}
            },
            Screen::SettingsLibrary => match key {
                "internal" => {
                    self.notice = "Internal storage · library source".into();
                }
                "sd" => {
                    self.notice = if m
                        .sources
                        .iter()
                        .any(|source| source.id == "sd" && source.online)
                    {
                        "SD card available".into()
                    } else {
                        "Insert an SD card to use it".into()
                    };
                }
                "scan_library" => return Effect::ScanLibrary,
                "rebuild_library" => {
                    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::RebuildLibrary));
                    m.navigation.modal_focus = 1;
                }
                _ => {}
            },
            Screen::SettingsDisplay => {
                if key == "timeout" {
                    let next = match m.settings.screen_timeout_seconds {
                        15 => 30,
                        30 => 60,
                        60 => 120,
                        120 => 0,
                        _ => 15,
                    };
                    return Effect::SetScreenTimeout(next);
                }
            }
            Screen::SettingsPower => {
                if key == "power_menu" {
                    m.navigation.modal = Some(Modal::PowerMenu);
                    m.navigation.modal_focus = 2;
                }
            }
            Screen::SettingsSystem => match key {
                "about" => {
                    self.notice = format!("Reborn {}", reborn_core::VERSION);
                }
                "diagnostics" => Self::go(m, Screen::Diagnostics, ""),
                "reboot" => {
                    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::Reboot));
                    m.navigation.modal_focus = 1;
                }
                "power_off" => {
                    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::PowerOff));
                    m.navigation.modal_focus = 1;
                }
                _ => {}
            },
            Screen::Diagnostics => {}
            Screen::NowPlaying => match key {
                "previous" => return Effect::PreviousTrack,
                "toggle" => return Effect::TogglePlayback,
                "next" => return Effect::NextTrack,
                _ => {}
            },
            Screen::TextEntry | Screen::Pairing => {}
        }
        Effect::None
    }

    fn play_track(&mut self, m: &mut AppModel, tracks: &[Track], key: &str) -> Effect {
        let Some(index) = key.strip_prefix("track:").and_then(|v| v.parse().ok()) else {
            return Effect::None;
        };
        if tracks.get(index).is_none() {
            return Effect::None;
        }
        Self::go(m, Screen::NowPlaying, "");
        Effect::Play(index)
    }

    fn play_first_filtered(&mut self, m: &mut AppModel, tracks: &[Track]) -> Effect {
        let Some(index) = self.first_filtered_index(m, tracks) else {
            return Effect::None;
        };
        Self::go(m, Screen::NowPlaying, "");
        Effect::Play(index)
    }

    fn play_first_filtered_shuffled(&mut self, m: &mut AppModel, tracks: &[Track]) -> Effect {
        let Some(index) = self.first_filtered_index(m, tracks) else {
            return Effect::None;
        };
        Self::go(m, Screen::NowPlaying, "");
        Effect::PlayShuffled(index)
    }

    fn first_filtered_index(&self, m: &AppModel, tracks: &[Track]) -> Option<usize> {
        tracks
            .iter()
            .position(|track| track_matches(track, &m.navigation.filter))
    }

    fn bluetooth_action(&mut self, _m: &mut AppModel, key: &str) -> Effect {
        match key {
            "bt_power" => Effect::BluetoothPower,
            "bt_scan" => Effect::BluetoothScan,
            "wired_output" => Effect::Output(AudioOutput::Wired),
            key if key.starts_with("bt_device:") => {
                let path = key.trim_start_matches("bt_device:").to_owned();
                let connected = self
                    .bluetooth_devices
                    .iter()
                    .find(|item| item.key == path)
                    .is_some_and(|item| item.label.contains("connected"));
                Effect::BluetoothDevice {
                    path,
                    operation: if connected { "disconnect" } else { "connect" }.into(),
                }
            }
            _ => Effect::None,
        }
    }

    fn wifi_action(&mut self, _m: &mut AppModel, key: &str) -> Effect {
        match key {
            "wifi_power" => Effect::WifiPower,
            "wifi_scan" => Effect::WifiScan,
            key if key.starts_with("saved:") => key
                .strip_prefix("saved:")
                .and_then(|id| id.parse().ok())
                .map(Effect::WifiSaved)
                .unwrap_or(Effect::None),
            _ => {
                self.ssid = key.to_owned();
                self.password.clear();
                self.letter = 0;
                self.text_entry = true;
                Effect::None
            }
        }
    }

    fn modal_action(&mut self, m: &mut AppModel, tracks: &[Track]) -> Effect {
        let rows = self.modal_rows(m);
        let Some(row) = rows.get(m.navigation.modal_focus) else {
            return Effect::None;
        };
        match m.navigation.modal.clone() {
            Some(Modal::PowerMenu) => match row.key.as_str() {
                "power_off" => {
                    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::PowerOff));
                    m.navigation.modal_focus = 1;
                    Effect::None
                }
                "reboot" => {
                    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::Reboot));
                    m.navigation.modal_focus = 1;
                    Effect::None
                }
                _ => {
                    m.navigation.modal = None;
                    Effect::None
                }
            },
            Some(Modal::Confirm(confirm)) => {
                if row.key == "cancel" {
                    m.navigation.modal = None;
                    m.navigation.context_target = None;
                    return Effect::None;
                }
                let source_key = m.navigation.context_target.and_then(|target| {
                    self.rows(m, tracks)
                        .get(target)
                        .map(|item| item.key.clone())
                });
                m.navigation.modal = None;
                m.navigation.context_target = None;
                match confirm {
                    ConfirmAction::ClearQueue => Effect::ClearQueue,
                    ConfirmAction::RebuildLibrary => Effect::RebuildLibrary,
                    ConfirmAction::PowerOff => Effect::PowerOff,
                    ConfirmAction::Reboot => Effect::Reboot,
                    ConfirmAction::ForgetBluetooth => source_key
                        .map(|key| Effect::BluetoothDevice {
                            path: key.trim_start_matches("bt_device:").into(),
                            operation: "forget".into(),
                        })
                        .unwrap_or(Effect::None),
                    ConfirmAction::ForgetWifi => source_key
                        .and_then(|key| key.strip_prefix("saved:")?.parse().ok())
                        .map(Effect::WifiForget)
                        .unwrap_or(Effect::None),
                }
            }
            Some(Modal::ContextMenu) => {
                let Some(target) = m.navigation.context_target else {
                    m.navigation.modal = None;
                    return Effect::None;
                };
                let source_key = self
                    .rows(m, tracks)
                    .get(target)
                    .map(|item| item.key.clone())
                    .unwrap_or_default();
                let track_index = if m.screen == Screen::NowPlaying {
                    m.current()
                        .and_then(|current| tracks.iter().position(|track| track.id == current.id))
                } else {
                    source_key
                        .strip_prefix("track:")
                        .and_then(|value| value.parse().ok())
                };
                let collection_action = match row.key.as_str() {
                    "album_play" => self.first_filtered_index(m, tracks).map(Effect::Play),
                    "album_shuffle" => self
                        .first_filtered_index(m, tracks)
                        .map(Effect::PlayShuffled),
                    "album_next" => self.first_filtered_index(m, tracks).map(Effect::PlayNext),
                    "album_queue" => self.first_filtered_index(m, tracks).map(Effect::AddToQueue),
                    "album_info" => {
                        self.notice = format!(
                            "Album {}",
                            m.navigation
                                .filter
                                .strip_prefix("album:")
                                .unwrap_or("Unknown")
                        );
                        Some(Effect::None)
                    }
                    _ => None,
                };
                if let Some(effect) = collection_action {
                    m.navigation.modal = None;
                    m.navigation.context_target = None;
                    return effect;
                }
                m.navigation.modal = None;
                m.navigation.context_target = None;
                match row.key.as_str() {
                    "play" => track_index.map(Effect::Play).unwrap_or(Effect::None),
                    "play_next" => track_index.map(Effect::PlayNext).unwrap_or(Effect::None),
                    "add_queue" => track_index.map(Effect::AddToQueue).unwrap_or(Effect::None),
                    "go_album" => {
                        if let Some(index) = track_index.and_then(|i| tracks.get(i)) {
                            Self::go(m, Screen::Album, format!("album:{}", index.album));
                        }
                        Effect::None
                    }
                    "go_artist" => {
                        if let Some(index) = track_index.and_then(|i| tracks.get(i)) {
                            Self::go(m, Screen::Artist, format!("artist:{}", index.artist));
                        }
                        Effect::None
                    }
                    "track_info" => {
                        self.notice = track_index
                            .and_then(|i| tracks.get(i))
                            .map(track_info)
                            .unwrap_or_else(|| "Track information unavailable".into());
                        Effect::None
                    }
                    "queue_remove" => Effect::QueueRemove(target),
                    "queue_up" => Effect::QueueMove {
                        index: target,
                        delta: -1,
                    },
                    "queue_down" => Effect::QueueMove {
                        index: target,
                        delta: 1,
                    },
                    "clear_queue" => {
                        m.navigation.modal = Some(Modal::Confirm(ConfirmAction::ClearQueue));
                        m.navigation.modal_focus = 1;
                        m.navigation.context_target = Some(target);
                        Effect::None
                    }
                    "bt_connect" => {
                        let path = source_key.trim_start_matches("bt_device:").to_owned();
                        let connected = self
                            .bluetooth_devices
                            .iter()
                            .find(|item| item.key == path)
                            .is_some_and(|item| item.label.contains("connected"));
                        Effect::BluetoothDevice {
                            path,
                            operation: if connected { "disconnect" } else { "connect" }.into(),
                        }
                    }
                    "bt_output" => Effect::BluetoothDevice {
                        path: source_key.trim_start_matches("bt_device:").into(),
                        operation: "output".into(),
                    },
                    "bt_forget" => {
                        m.navigation.modal = Some(Modal::Confirm(ConfirmAction::ForgetBluetooth));
                        m.navigation.modal_focus = 1;
                        m.navigation.context_target = Some(target);
                        Effect::None
                    }
                    "wifi_connect" => source_key
                        .strip_prefix("saved:")
                        .and_then(|id| id.parse().ok())
                        .map(Effect::WifiSaved)
                        .unwrap_or(Effect::None),
                    "wifi_forget" => {
                        m.navigation.modal = Some(Modal::Confirm(ConfirmAction::ForgetWifi));
                        m.navigation.modal_focus = 1;
                        m.navigation.context_target = Some(target);
                        Effect::None
                    }
                    "queue_play" => {
                        if let Some(track) = m.queue.get(target) {
                            tracks
                                .iter()
                                .position(|candidate| candidate.id == track.id)
                                .map(Effect::Play)
                                .unwrap_or(Effect::None)
                        } else {
                            Effect::None
                        }
                    }
                    _ => Effect::None,
                }
            }
            None => Effect::None,
        }
    }

    fn entry(&mut self, action: Action) -> Effect {
        match action {
            Action::NavigateUp => self.letter = (self.letter + LETTERS.len() - 1) % LETTERS.len(),
            Action::NavigateDown => self.letter = (self.letter + 1) % LETTERS.len(),
            Action::WheelClockwise(steps) => {
                self.letter = (self.letter + usize::from(steps)) % LETTERS.len()
            }
            Action::WheelCounterClockwise(steps) => {
                let amount = usize::from(steps) % LETTERS.len();
                self.letter = (self.letter + LETTERS.len() - amount) % LETTERS.len();
            }
            Action::Select => {
                if self.password.len() < 63 {
                    self.password.push(LETTERS[self.letter] as char);
                }
            }
            Action::NavigateLeft | Action::PreviousTrack => {
                self.password.pop();
            }
            Action::ContextMenu => {
                if self.password.len() >= 8 {
                    self.text_entry = false;
                    return Effect::WifiConnect {
                        ssid: self.ssid.clone(),
                        password: std::mem::take(&mut self.password),
                    };
                }
            }
            Action::Back | Action::Home => {
                self.password.clear();
                self.text_entry = false;
            }
            _ => {}
        }
        Effect::None
    }

    pub fn draw(&self, m: &AppModel, tracks: &[Track], health: &str, has_art: bool) -> Vec<Quad> {
        let mut draw = Vec::with_capacity(512);
        rect(&mut draw, 0., 0., 480., 360., BG);
        self.draw_status(&mut draw, m);
        if let Some(pair) = &self.pairing {
            self.draw_pairing(&mut draw, pair);
            return draw;
        }
        if self.text_entry {
            self.draw_text_entry(&mut draw);
            return draw;
        }
        if m.screen == Screen::NowPlaying {
            self.draw_now_playing(&mut draw, m, has_art);
        } else if m.screen == Screen::Diagnostics {
            self.draw_diagnostics(&mut draw, m, tracks, health);
        } else {
            self.draw_list(&mut draw, m, tracks, has_art);
            self.draw_mini_player(&mut draw, m, has_art);
        }
        if m.navigation.modal.is_some() {
            self.draw_modal(&mut draw, m, tracks);
        }
        self.draw_footer(&mut draw, m);
        draw
    }

    fn draw_status(&self, d: &mut Vec<Quad>, m: &AppModel) {
        rect(d, 0., 0., 480., 34., SURFACE);
        text(d, 12., 10., "Reborn", 1.6, PRIMARY);
        text(d, 86., 10., "| Y2", 1.6, MUTED);
        text(d, 198., 10., screen_title(m.screen), 1.6, GOLD);
        if self.bluetooth.powered {
            text(d, 278., 10., "BT", 0.9, SUCCESS);
        }
        if self.wifi.powered {
            text(d, 306., 10., "WF", 0.9, SUCCESS);
        }
        text(d, 348., 10., output_short(&m.output), 1.0, SECONDARY);
        text(
            d,
            414.,
            10.,
            &format!("V{}", m.settings.volume),
            1.0,
            PRIMARY,
        );
        rect(d, 0., 33., 480., 1., DIVIDER);
    }

    fn draw_list(&self, d: &mut Vec<Quad>, m: &AppModel, tracks: &[Track], has_art: bool) {
        let rows = self.rows(m, tracks);
        let collection_view = matches!(m.screen, Screen::Album | Screen::Artist);
        if collection_view {
            self.draw_collection_banner(d, m, tracks, has_art);
        } else {
            let title = screen_title(m.screen);
            text(d, 14., 50., title, 2.0, PRIMARY);
        }
        if m.screen == Screen::Home {
            text(d, 14., 72., "Listen deeper", 1.2, SECONDARY);
        } else if m.screen == Screen::Bluetooth || m.screen == Screen::SettingsBluetooth {
            text(d, 14., 72., &self.bluetooth.message(true), 1.0, SECONDARY);
        } else if m.screen == Screen::Wifi || m.screen == Screen::SettingsWifi {
            text(d, 14., 72., &self.wifi.message(false), 1.0, SECONDARY);
        } else if m.screen == Screen::Music && tracks.is_empty() {
            self.draw_empty(
                d,
                "No music found",
                "Scan your library or insert an SD card",
            );
            return;
        }
        let list_top = if collection_view {
            128.
        } else if matches!(
            m.screen,
            Screen::Home
                | Screen::Bluetooth
                | Screen::SettingsBluetooth
                | Screen::Wifi
                | Screen::SettingsWifi
        ) {
            92.
        } else {
            72.
        };
        let bottom = if m.current().is_some() { 282. } else { 322. };
        let visible = (((bottom - list_top) / 39.0_f32).max(1.0_f32)) as usize;
        let start = m
            .navigation
            .focus
            .saturating_sub(visible.saturating_sub(2))
            .min(rows.len().saturating_sub(visible));
        for (position, row) in rows.iter().enumerate().skip(start).take(visible) {
            let y = list_top + ((position - start) as f32 * 39.);
            if position == m.navigation.focus {
                rect(d, 10., y, 460., 34., FOCUS);
                rect(d, 10., y, 3., 34., GOLD);
                rect(d, 13., y + 33., 457., 1., GOLD);
            } else {
                rect(d, 14., y + 33., 456., 1., DIVIDER);
            }
            let foreground = if position == m.navigation.focus {
                PRIMARY
            } else {
                SECONDARY
            };
            text(d, 24., y + 6., &row.label, 1.35, foreground);
            if !row.secondary.is_empty() {
                text(
                    d,
                    24.,
                    y + 22.,
                    &row.secondary,
                    0.9,
                    if position == m.navigation.focus {
                        GOLD
                    } else {
                        MUTED
                    },
                );
            }
            text(
                d,
                450.,
                y + 10.,
                ">",
                1.2,
                if position == m.navigation.focus {
                    GOLD
                } else {
                    MUTED
                },
            );
        }
        if rows.is_empty() {
            self.draw_empty(
                d,
                "Nothing here yet",
                "Your library will appear after a scan",
            );
        }
    }

    fn draw_collection_banner(
        &self,
        d: &mut Vec<Quad>,
        m: &AppModel,
        tracks: &[Track],
        has_art: bool,
    ) {
        let (kind, name, meta) = match m.screen {
            Screen::Album => {
                let name = m
                    .navigation
                    .filter
                    .strip_prefix("album:")
                    .unwrap_or("Album");
                let artist = tracks
                    .iter()
                    .find(|track| track_matches(track, &m.navigation.filter))
                    .map(|track| display_or_unknown(&track.artist))
                    .unwrap_or("Unknown");
                ("Album", name, artist.to_owned())
            }
            Screen::Artist => (
                "Artist",
                m.navigation
                    .filter
                    .strip_prefix("artist:")
                    .unwrap_or("Artist"),
                "Albums and tracks".into(),
            ),
            _ => return,
        };
        if has_art {
            let mut art = Quad::rect(14., 48., 66., 66., PRIMARY);
            art.artwork = true;
            d.push(art);
        } else {
            rect(d, 14., 48., 66., 66., RAISED);
            text(d, 38., 70., "♪", 2.4, GOLD);
        }
        text(d, 94., 54., name, 1.65, PRIMARY);
        text(d, 94., 82., kind, 0.9, GOLD);
        text(d, 94., 101., &meta, 1.0, SECONDARY);
        rect(d, 14., 121., 452., 1., DIVIDER);
    }

    fn draw_mini_player(&self, d: &mut Vec<Quad>, m: &AppModel, has_art: bool) {
        let Some(track) = m.current() else { return };
        rect(d, 10., 282., 460., 38., RAISED);
        rect(d, 10., 282., 460., 1., DIVIDER);
        if has_art {
            let mut art = Quad::rect(16., 287., 28., 28., PRIMARY);
            art.artwork = true;
            d.push(art);
        } else {
            rect(d, 16., 287., 28., 28., 0x25313cff);
            text(d, 26., 296., "♪", 1.2, GOLD);
        }
        text(d, 54., 288., display_or_unknown(&track.title), 1.1, PRIMARY);
        text(d, 54., 304., display_or_unknown(&track.artist), 0.95, MUTED);
        text(d, 424., 295., playback_glyph(m.playback), 1.5, GOLD);
    }

    fn draw_now_playing(&self, d: &mut Vec<Quad>, m: &AppModel, has_art: bool) {
        let Some(track) = m.current() else {
            self.draw_empty(d, "Nothing is playing", "Choose music to begin listening");
            return;
        };
        if has_art {
            let mut art = Quad::rect(14., 51., 154., 154., PRIMARY);
            art.artwork = true;
            d.push(art);
        } else {
            rect(d, 14., 51., 154., 154., RAISED);
            text(d, 76., 116., "♪", 5., GOLD);
        }
        text(d, 184., 58., display_or_unknown(&track.title), 1.7, PRIMARY);
        text(
            d,
            184.,
            92.,
            display_or_unknown(&track.artist),
            1.35,
            SECONDARY,
        );
        text(d, 184., 121., display_or_unknown(&track.album), 1.2, MUTED);
        text(
            d,
            184.,
            154.,
            &format!(
                "{} · {}",
                track.codec.to_uppercase(),
                technical_format(track)
            ),
            0.95,
            GOLD,
        );
        text(
            d,
            184.,
            178.,
            &format!("{} / {}", time(m.position_ms), time(track.duration_ms)),
            1.0,
            SECONDARY,
        );
        rect(d, 184., 204., 280., 5., DIVIDER);
        rect(
            d,
            184.,
            204.,
            280. * progress(m.position_ms, track.duration_ms),
            5.,
            GOLD,
        );
        text(d, 42., 236., "‹", 2.0, SECONDARY);
        text(d, 110., 236., playback_glyph(m.playback), 2.0, GOLD);
        text(d, 176., 236., ">", 2.0, SECONDARY);
        text(d, 256., 236., "Volume", 0.9, MUTED);
        rect(d, 256., 252., 190., 5., DIVIDER);
        rect(
            d,
            256.,
            252.,
            190. * (m.settings.volume as f32 / 100.),
            5.,
            GOLD,
        );
        text(
            d,
            256.,
            266.,
            &format!("{} / 100", m.settings.volume),
            0.9,
            SECONDARY,
        );
        text(
            d,
            14.,
            296.,
            &format!("{} · {}", output_label(&m.output), track.codec),
            0.95,
            MUTED,
        );
    }

    fn draw_diagnostics(&self, d: &mut Vec<Quad>, m: &AppModel, tracks: &[Track], health: &str) {
        if m.navigation.filter == "audio" {
            self.draw_audio_information(d, m);
            return;
        }
        text(d, 14., 50., "System status", 2.0, PRIMARY);
        text(
            d,
            14.,
            74.,
            "Simple when you are listening. Detailed via SSH.",
            0.95,
            SECONDARY,
        );
        let rows = [
            ("Audio", health_label(health)),
            (
                "Storage",
                if m.sources.iter().any(|s| s.online) {
                    "OK"
                } else {
                    "Unavailable"
                },
            ),
            ("Library", if tracks.is_empty() { "Empty" } else { "OK" }),
            ("Output", &output_label(&m.output)),
            ("Reborn", "Running"),
        ];
        for (index, (label, value)) in rows.iter().enumerate() {
            let y = 112. + index as f32 * 38.;
            rect(d, 14., y + 30., 452., 1., DIVIDER);
            text(d, 22., y + 7., label, 1.2, SECONDARY);
            text(
                d,
                330.,
                y + 7.,
                value,
                1.1,
                if *value == "OK" { SUCCESS } else { PRIMARY },
            );
        }
    }

    fn draw_audio_information(&self, d: &mut Vec<Quad>, m: &AppModel) {
        text(d, 14., 50., "Audio information", 2.0, PRIMARY);
        let Some(track) = m.current() else {
            self.draw_empty(d, "Nothing is playing", "Start a track to see its format");
            return;
        };
        let rows = [
            ("Codec", display_or_unknown(&track.codec).to_owned()),
            ("Source", technical_format(track)),
            ("Output", output_label(&m.output)),
            (
                "ReplayGain",
                replay_gain_label(m.settings.replay_gain).to_owned(),
            ),
            (
                "Equalizer",
                if m.settings.eq_enabled { "On" } else { "Off" }.into(),
            ),
            ("Bluetooth", self.bluetooth.connection.clone()),
        ];
        for (index, (label, value)) in rows.iter().enumerate() {
            let y = 88. + index as f32 * 36.;
            rect(d, 14., y + 27., 452., 1., DIVIDER);
            text(d, 22., y + 5., label, 1.1, SECONDARY);
            text(
                d,
                220.,
                y + 5.,
                if value.is_empty() { "—" } else { value },
                1.05,
                PRIMARY,
            );
        }
    }

    fn draw_empty(&self, d: &mut Vec<Quad>, title: &str, detail: &str) {
        rect(d, 24., 112., 432., 96., SURFACE);
        text(d, 42., 136., title, 1.5, PRIMARY);
        text(d, 42., 166., detail, 1.0, SECONDARY);
    }

    fn draw_pairing(&self, d: &mut Vec<Quad>, pair: &str) {
        rect(d, 24., 72., 432., 190., SURFACE);
        text(d, 42., 100., "Bluetooth pairing", 1.7, PRIMARY);
        text(d, 42., 138., pair, 1.4, GOLD);
        text(d, 42., 188., "Select  Allow", 1.0, SUCCESS);
        text(d, 42., 212., "Back    Reject", 1.0, DANGER);
    }

    fn draw_text_entry(&self, d: &mut Vec<Quad>) {
        text(d, 14., 52., "Join Wi-Fi", 2.0, PRIMARY);
        text(d, 14., 82., &self.ssid, 1.2, SECONDARY);
        rect(d, 14., 112., 452., 36., SURFACE);
        text(
            d,
            26.,
            125.,
            &"•".repeat(self.password.len()),
            1.25,
            PRIMARY,
        );
        rect(d, 14., 174., 452., 54., FOCUS);
        text(d, 38., 188., "Character", 0.9, MUTED);
        text(
            d,
            198.,
            184.,
            &(LETTERS[self.letter] as char).to_string(),
            3.0,
            GOLD,
        );
        text(
            d,
            14.,
            258.,
            "Wheel choose · Select add · Previous delete",
            0.9,
            SECONDARY,
        );
        text(
            d,
            14.,
            278.,
            "Menu/hold Select connect · Back cancel",
            0.9,
            MUTED,
        );
    }

    fn draw_modal(&self, d: &mut Vec<Quad>, m: &AppModel, tracks: &[Track]) {
        rect(d, 0., 34., 480., 286., 0x070a0ee8);
        let rows = self.modal_rows(m);
        let title = match m.navigation.modal {
            Some(Modal::ContextMenu) => "Track options",
            Some(Modal::PowerMenu) => "Power",
            Some(Modal::Confirm(_)) => "Are you sure?",
            None => "",
        };
        rect(d, 34., 58., 412., 234., SURFACE);
        text(d, 52., 78., title, 1.7, PRIMARY);
        if let Some(Modal::Confirm(action)) = m.navigation.modal {
            text(d, 52., 102., confirmation_copy(action), 0.9, SECONDARY);
        }
        for (index, row) in rows.iter().enumerate() {
            let y = 126. + index as f32 * 31.;
            if index == m.navigation.modal_focus {
                rect(d, 48., y, 384., 27., FOCUS);
                rect(d, 48., y, 3., 27., GOLD);
            }
            text(
                d,
                62.,
                y + 8.,
                &row.label,
                1.1,
                if index == m.navigation.modal_focus {
                    PRIMARY
                } else {
                    SECONDARY
                },
            );
        }
        let _ = tracks;
    }

    fn draw_footer(&self, d: &mut Vec<Quad>, m: &AppModel) {
        rect(d, 0., 332., 480., 28., SURFACE);
        text(d, 12., 342., "Wheel navigate", 0.9, MUTED);
        text(d, 170., 342., "Select open", 0.9, MUTED);
        text(d, 304., 342., "Back return", 0.9, MUTED);
        if !self.notice.is_empty() && self.notice_until.is_none_or(|until| Instant::now() < until) {
            rect(d, 98., 300., 284., 25., FOCUS);
            text(d, 108., 308., &self.notice, 0.85, GOLD);
        }
        if m.screen == Screen::NowPlaying {
            text(d, 418., 342., "Vol", 0.9, GOLD);
        }
    }
}

fn display_or_unknown(value: &str) -> &str {
    if value.trim().is_empty() {
        "Unknown"
    } else {
        value
    }
}

fn track_matches(track: &Track, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    if let Some(value) = filter.strip_prefix("artist:") {
        return track.artist == value;
    }
    if let Some(value) = filter.strip_prefix("album:") {
        return track.album == value;
    }
    if let Some(value) = filter.strip_prefix("folder:") {
        return track
            .path
            .parent()
            .is_some_and(|path| path.to_string_lossy() == value);
    }
    false
}

fn track_info(track: &Track) -> String {
    format!(
        "{} · {} · {}",
        track.codec.to_uppercase(),
        technical_format(track),
        time(track.duration_ms)
    )
}

fn screen_title(screen: Screen) -> &'static str {
    match screen {
        Screen::Home => "Home",
        Screen::Music => "Music",
        Screen::Artists | Screen::Artist => "Artists",
        Screen::Albums | Screen::Album => "Albums",
        Screen::Tracks => "Tracks",
        Screen::Folders => "Folders",
        Screen::NowPlaying => "Now Playing",
        Screen::Queue => "Queue",
        Screen::Connectivity => "Connectivity",
        Screen::Bluetooth | Screen::SettingsBluetooth => "Bluetooth",
        Screen::Wifi | Screen::SettingsWifi => "Wi-Fi",
        Screen::Settings => "Settings",
        Screen::SettingsAudio => "Audio",
        Screen::SettingsPlayback => "Playback",
        Screen::SettingsLibrary => "Library",
        Screen::SettingsDisplay => "Display",
        Screen::SettingsPower => "Power",
        Screen::SettingsSystem => "System",
        Screen::Diagnostics => "Diagnostics",
        Screen::TextEntry => "Wi-Fi",
        Screen::Pairing => "Pairing",
    }
}

fn output_label(output: &AudioOutput) -> String {
    match output {
        AudioOutput::Wired => "Wired output".into(),
        AudioOutput::Bluetooth(_) => "Bluetooth output".into(),
    }
}

fn output_short(output: &AudioOutput) -> &'static str {
    match output {
        AudioOutput::Wired => "WIRED",
        AudioOutput::Bluetooth(_) => "BT",
    }
}

fn replay_gain_label(mode: ReplayGainMode) -> &'static str {
    match mode {
        ReplayGainMode::Off => "Off",
        ReplayGainMode::Track => "Track",
        ReplayGainMode::Album => "Album",
    }
}

fn repeat_label(mode: RepeatMode) -> &'static str {
    match mode {
        RepeatMode::Off => "Off",
        RepeatMode::Track => "Track",
        RepeatMode::All => "All",
    }
}

fn crossfade_label(ms: u32) -> String {
    if ms == 0 {
        "Off".into()
    } else {
        format!("{} sec", ms / 1000)
    }
}

fn timeout_label(seconds: u32) -> String {
    match seconds {
        0 => "Never".into(),
        60 => "1 minute".into(),
        120 => "2 minutes".into(),
        value => format!("{value} seconds"),
    }
}

fn confirmation_copy(action: ConfirmAction) -> &'static str {
    match action {
        ConfirmAction::ClearQueue => "Remove all upcoming tracks?",
        ConfirmAction::RebuildLibrary => "Rebuild the library index?",
        ConfirmAction::ForgetBluetooth => "Forget this Bluetooth device?",
        ConfirmAction::ForgetWifi => "Forget this Wi-Fi network?",
        ConfirmAction::PowerOff => "The player will shut down.",
        ConfirmAction::Reboot => "The player will restart.",
    }
}

fn health_label(health: &str) -> &'static str {
    if health == "ok" {
        "OK"
    } else {
        "Check"
    }
}

fn playback_glyph(state: reborn_core::PlaybackState) -> &'static str {
    match state {
        reborn_core::PlaybackState::Playing | reborn_core::PlaybackState::Buffering => "Ⅱ",
        _ => "▶",
    }
}

fn technical_format(track: &Track) -> String {
    if track.sample_rate == 0 {
        return "audio".into();
    }
    format!("{} kHz", track.sample_rate / 1000)
}

fn progress(position: u64, duration: u64) -> f32 {
    if duration == 0 {
        0.
    } else {
        (position as f32 / duration as f32).clamp(0., 1.)
    }
}

fn time(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, ms / 1_000 % 60)
}

fn rect(d: &mut Vec<Quad>, x: f32, y: f32, w: f32, h: f32, color: u32) {
    d.push(Quad::rect(x, y, w, h, color));
}

fn text(d: &mut Vec<Quad>, x: f32, y: f32, value: &str, scale: f32, color: u32) {
    let limit = ((474. - x) / (8. * scale)).max(0.) as usize;
    for (index, character) in value.chars().take(limit).enumerate() {
        let mut quad = Quad::rect(
            x + index as f32 * 8. * scale,
            y,
            8. * scale,
            8. * scale,
            color,
        );
        quad.glyph = Some(if character.is_ascii() {
            character as u8
        } else {
            b'?'
        });
        d.push(quad);
    }
}

pub fn font_atlas() -> Vec<u8> {
    let mut out = vec![0u8; 128 * 64 * 4];
    for character in 0..128 {
        if let Some(rows) = font8x8::BASIC_FONTS.get(char::from(character)) {
            for (y, bits) in rows.into_iter().enumerate() {
                for x in 0..8 {
                    let index = (((character as usize / 16) * 8 + y) * 128
                        + (character as usize % 16) * 8
                        + x)
                        * 4;
                    out[index..index + 4].copy_from_slice(&[
                        255,
                        255,
                        255,
                        if bits & (1 << x) != 0 { 255 } else { 0 },
                    ]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(screen: Screen) -> AppModel {
        AppModel {
            screen,
            navigation: reborn_core::NavigationState::default(),
            ..Default::default()
        }
    }

    #[test]
    fn wheel_on_now_playing_changes_volume_only() {
        let mut ui = Ui::default();
        let mut app = model(Screen::NowPlaying);
        assert_eq!(
            ui.action(&mut app, &[], Action::WheelClockwise(1)),
            Effect::AdjustVolume(2)
        );
        assert_eq!(app.playback, reborn_core::PlaybackState::Stopped);
    }

    #[test]
    fn ordinary_navigation_does_not_emit_transport_effects() {
        let mut ui = Ui::default();
        let mut app = model(Screen::Music);
        let _ = ui.action(&mut app, &[], Action::WheelClockwise(1));
        assert_eq!(app.playback, reborn_core::PlaybackState::Stopped);
    }

    #[test]
    fn back_at_home_is_a_safe_noop() {
        let mut ui = Ui::default();
        let mut app = model(Screen::Home);
        ui.action(&mut app, &[], Action::Back);
        assert_eq!(app.screen, Screen::Home);
        assert_eq!(app.playback, reborn_core::PlaybackState::Stopped);
    }

    #[test]
    fn context_menu_is_explicit_and_focusable() {
        let track = Track {
            title: "Example".into(),
            ..Default::default()
        };
        let mut ui = Ui::default();
        let mut app = model(Screen::Tracks);
        assert_eq!(
            ui.action(&mut app, &[track], Action::ContextMenu),
            Effect::None
        );
        assert_eq!(app.navigation.modal, Some(Modal::ContextMenu));
        assert_eq!(ui.action(&mut app, &[], Action::Back), Effect::None);
        assert_eq!(app.navigation.modal, None);
    }

    #[test]
    fn destructive_actions_default_to_cancel() {
        let mut ui = Ui::default();
        let mut app = model(Screen::SettingsLibrary);
        app.navigation.focus = 3;
        ui.action(&mut app, &[], Action::Select);
        assert_eq!(
            app.navigation.modal,
            Some(Modal::Confirm(ConfirmAction::RebuildLibrary))
        );
        assert_eq!(app.navigation.modal_focus, 1);
    }

    #[test]
    fn password_is_never_drawn() {
        let ui = Ui {
            text_entry: true,
            password: "secretpass".into(),
            ..Default::default()
        };
        let draw = ui.draw(&AppModel::default(), &[], "ok", false);
        let glyphs = draw
            .iter()
            .filter_map(|q| q.glyph.map(char::from))
            .collect::<String>();
        assert!(!glyphs.contains("secretpass"));
    }

    #[test]
    fn font_dimensions_remain_fixed_for_native_renderer() {
        assert_eq!(font_atlas().len(), 128 * 64 * 4);
    }
}
