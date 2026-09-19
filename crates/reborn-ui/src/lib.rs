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

// Reborn's visual language is deliberately small and consistent. These are
// product tokens, not per-screen decoration: the same background, surfaces,
// border hierarchy and warm focus color are used by every route.
const BG: u32 = 0x080b10ff;
const HEADER: u32 = 0x0d1117f5;
const SURFACE: u32 = 0x11171eff;
const RAISED: u32 = 0x171e27ff;
const CARD: u32 = 0x121920f2;
const CARD_ALT: u32 = 0x161d25f2;
const FOCUS: u32 = 0x241f17f2;
const PRIMARY: u32 = 0xf4f1ebff;
const SECONDARY: u32 = 0xb2b7c0ff;
const MUTED: u32 = 0x737d8aff;
const GOLD: u32 = 0xf0c36cff;
const GOLD_SOFT: u32 = 0xd5a85cff;
const SUCCESS: u32 = 0x9ac9b0ff;
const DANGER: u32 = 0xdb7c70ff;
const BORDER: u32 = 0x2a333eff;
const BORDER_SOFT: u32 = 0x202932ff;
const DIVIDER: u32 = 0x26303aff;

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
        let mut draw = Vec::with_capacity(4096);
        rect(&mut draw, 0., 0., 480., 360., BG);
        self.draw_premium_status(&mut draw, m);
        if let Some(pair) = &self.pairing {
            self.draw_premium_pairing(&mut draw, pair);
            self.draw_brand_footer(&mut draw);
            return draw;
        }
        if self.text_entry {
            self.draw_premium_text_entry(&mut draw);
            self.draw_brand_footer(&mut draw);
            return draw;
        }
        match m.screen {
            Screen::NowPlaying => self.draw_premium_now_playing(&mut draw, m, has_art),
            Screen::Queue => self.draw_premium_queue(&mut draw, m, has_art),
            Screen::Settings
            | Screen::SettingsAudio
            | Screen::SettingsPlayback
            | Screen::SettingsLibrary
            | Screen::SettingsBluetooth
            | Screen::SettingsWifi
            | Screen::SettingsDisplay
            | Screen::SettingsPower
            | Screen::SettingsSystem => self.draw_premium_settings(&mut draw, m),
            Screen::Connectivity | Screen::Bluetooth | Screen::Wifi => {
                self.draw_premium_connectivity(&mut draw, m)
            }
            Screen::Album | Screen::Artist => {
                self.draw_premium_collection(&mut draw, m, tracks, has_art)
            }
            Screen::Albums => self.draw_premium_albums(&mut draw, m, tracks, has_art),
            Screen::Diagnostics => self.draw_premium_diagnostics(&mut draw, m, tracks, health),
            Screen::Home => self.draw_premium_home(&mut draw, m, tracks, has_art),
            Screen::Music | Screen::Artists | Screen::Tracks | Screen::Folders => {
                self.draw_premium_library(&mut draw, m, tracks, has_art)
            }
            Screen::TextEntry | Screen::Pairing => {}
        }
        if m.navigation.modal.is_some() {
            self.draw_premium_modal(&mut draw, m);
        }
        if m.screen != Screen::NowPlaying {
            self.draw_premium_mini_player(&mut draw, m, has_art);
        }
        self.draw_brand_footer(&mut draw);
        if !self.notice.is_empty() && self.notice_until.is_none_or(|until| Instant::now() < until) {
            self.draw_toast(&mut draw, &self.notice);
        }
        draw
    }

    fn draw_premium_status(&self, d: &mut Vec<Quad>, m: &AppModel) {
        rect(d, 0., 0., 480., 31., HEADER);
        text(d, 12., 7., "Reborn", 1.35, PRIMARY);
        rect(d, 69., 7., 1., 16., BORDER);
        text(d, 80., 7., "Y2", 1.35, GOLD);

        let title = screen_title(m.screen);
        centered_text(d, 240., 6., title, 1.05, GOLD);
        let underline = (title.chars().count() as f32 * 8.4).clamp(28., 72.);
        rounded_rect(d, 240. - underline / 2., 26., underline, 2., 1., GOLD);

        icon(d, "headphones", 326., 8., 13., PRIMARY);
        text(d, 344., 9., output_short(&m.output), 0.78, SECONDARY);
        if self.bluetooth.powered {
            circle(d, 396., 14., 2., GOLD);
        }
        if self.wifi.powered {
            circle(d, 404., 14., 2., SUCCESS);
        }
        text(
            d,
            418.,
            9.,
            &format!("V{}", m.settings.volume),
            0.82,
            PRIMARY,
        );
        rect(d, 0., 30., 480., 1., BORDER_SOFT);
    }

    fn draw_premium_home(&self, d: &mut Vec<Quad>, m: &AppModel, _tracks: &[Track], has_art: bool) {
        panel(d, 12., 42., 456., 94., CARD, BORDER);
        if let Some(track) = m.current() {
            artwork_card(d, 20., 50., 78., CARD, has_art);
            micro(d, 112., 52., "NOW PLAYING", GOLD_SOFT);
            text(d, 112., 68., &fit_text(&track.title, 22), 1.22, PRIMARY);
            text(d, 112., 88., &fit_text(&track.artist, 25), 0.94, SECONDARY);
            text(
                d,
                112.,
                108.,
                &format!("{}  ·  {}", time(m.position_ms), time(track.duration_ms)),
                0.78,
                MUTED,
            );
            text(d, 401., 74., ">", 1.35, GOLD);
            micro(d, 370., 101., "OPEN", MUTED);
        } else {
            icon(d, "music", 28., 65., 25., GOLD);
            micro(d, 76., 56., "REBORN AUDIO", GOLD_SOFT);
            text(d, 76., 72., "Listen deeper", 1.7, PRIMARY);
            text(d, 76., 98., "Your music, simply present.", 0.9, SECONDARY);
            micro(d, 365., 78., "READY", SUCCESS);
        }

        micro(d, 14., 151., "LIBRARY", MUTED);
        let rows = self.rows(m, &m.library.tracks);
        for (index, row) in rows.iter().enumerate() {
            let (x, y, w) = if index == 4 {
                (12., 238., 456.)
            } else {
                (
                    12. + (index % 2) as f32 * 228.,
                    162. + (index / 2) as f32 * 39.,
                    216.,
                )
            };
            let selected = index == m.navigation.focus;
            panel(
                d,
                x,
                y,
                w,
                34.,
                if selected { FOCUS } else { CARD },
                if selected { GOLD } else { BORDER_SOFT },
            );
            icon(
                d,
                home_icon(&row.key),
                x + 12.,
                y + 8.,
                15.,
                if selected { GOLD } else { SECONDARY },
            );
            text(
                d,
                x + 39.,
                y + 6.,
                &fit_text(&row.label, if w > 300. { 31 } else { 20 }),
                0.85,
                if selected { PRIMARY } else { SECONDARY },
            );
            if !row.secondary.is_empty() {
                text(
                    d,
                    x + 39.,
                    y + 20.,
                    &fit_text(&row.secondary, if w > 300. { 45 } else { 26 }),
                    0.62,
                    if selected { GOLD_SOFT } else { MUTED },
                );
            }
            text(
                d,
                x + w - 19.,
                y + 10.,
                ">",
                0.95,
                if selected { GOLD } else { MUTED },
            );
        }
    }

    fn draw_premium_library(
        &self,
        d: &mut Vec<Quad>,
        m: &AppModel,
        tracks: &[Track],
        has_art: bool,
    ) {
        self.draw_library_rail(d, m);
        if m.screen == Screen::Music {
            let rows = self.rows(m, tracks);
            micro(d, 130., 46., "LIBRARY", MUTED);
            text(d, 130., 58., "Choose a view", 1.65, PRIMARY);
            for (index, row) in rows.iter().enumerate() {
                let x = 130. + (index % 2) as f32 * 170.;
                let y = 82. + (index / 2) as f32 * 58.;
                let selected = index == m.navigation.focus;
                panel(
                    d,
                    x,
                    y,
                    158.,
                    48.,
                    if selected { FOCUS } else { CARD },
                    if selected { GOLD } else { BORDER_SOFT },
                );
                icon(
                    d,
                    home_icon(&row.key),
                    x + 12.,
                    y + 13.,
                    19.,
                    if selected { GOLD } else { SECONDARY },
                );
                text(
                    d,
                    x + 42.,
                    y + 10.,
                    &fit_text(&row.label, 16),
                    0.95,
                    if selected { PRIMARY } else { SECONDARY },
                );
                if !row.secondary.is_empty() {
                    text(
                        d,
                        x + 42.,
                        y + 27.,
                        &fit_text(&row.secondary, 20),
                        0.67,
                        MUTED,
                    );
                }
            }
            return;
        }
        if m.screen == Screen::Artists || m.screen == Screen::Tracks || m.screen == Screen::Folders
        {
            let rows = self.rows(m, tracks);
            micro(d, 130., 46., "LIBRARY", MUTED);
            text(d, 130., 58., screen_title(m.screen), 1.65, PRIMARY);
            text(d, 402., 61., &format!("{}", rows.len()), 0.82, MUTED);
            self.draw_premium_rows(
                d,
                &rows,
                m.navigation.focus,
                130.,
                76.,
                338.,
                5,
                false,
                has_art,
            );
            return;
        }
        self.draw_premium_albums(d, m, tracks, has_art);
    }

    fn draw_premium_albums(
        &self,
        d: &mut Vec<Quad>,
        m: &AppModel,
        tracks: &[Track],
        has_art: bool,
    ) {
        self.draw_library_rail(d, m);
        let rows = self.rows(m, tracks);
        micro(d, 130., 46., "LIBRARY / ALBUMS", MUTED);
        text(d, 130., 58., "Albums", 1.65, PRIMARY);
        text(d, 407., 61., &format!("{}", rows.len()), 0.82, MUTED);
        if rows.is_empty() {
            self.draw_premium_empty(
                d,
                130.,
                86.,
                338.,
                "No albums yet",
                "Scan your music library to begin",
            );
            return;
        }
        let start = m
            .navigation
            .focus
            .saturating_sub(2)
            .min(rows.len().saturating_sub(6));
        for (position, row) in rows.iter().enumerate().skip(start).take(6) {
            let tile = position - start;
            let x = 130. + (tile % 3) as f32 * 111.;
            let y = 78. + (tile / 3) as f32 * 111.;
            let selected = position == m.navigation.focus;
            panel(
                d,
                x - 2.,
                y - 2.,
                101.,
                101.,
                if selected { FOCUS } else { CARD },
                if selected { GOLD } else { BORDER_SOFT },
            );
            artwork_card(
                d,
                x + 4.,
                y + 4.,
                89.,
                if selected { FOCUS } else { CARD },
                has_art && selected,
            );
            text(
                d,
                x + 4.,
                y + 96.,
                &fit_text(&row.label, 15),
                0.72,
                if selected { PRIMARY } else { SECONDARY },
            );
            text(
                d,
                x + 4.,
                y + 107.,
                &fit_text(&row.secondary, 17),
                0.62,
                MUTED,
            );
            if selected {
                text(d, x + 83., y + 96., "·", 1.0, GOLD);
            }
        }
    }

    fn draw_library_rail(&self, d: &mut Vec<Quad>, m: &AppModel) {
        panel(d, 10., 42., 106., 258., CARD, BORDER_SOFT);
        micro(d, 23., 53., "MUSIC", MUTED);
        let entries = [
            ("Albums", "albums", "album"),
            ("Artists", "artists", "artist"),
            ("Songs", "tracks", "song"),
            ("Folders", "folders", "folder"),
        ];
        for (index, (label, key, screen_key)) in entries.iter().enumerate() {
            let y = 70. + index as f32 * 35.;
            let selected = screen_title(m.screen).eq_ignore_ascii_case(label)
                || (m.screen == Screen::Music && index == 0);
            if selected {
                panel(d, 16., y - 3., 94., 29., FOCUS, GOLD);
            }
            icon(
                d,
                home_icon(key),
                23.,
                y + 4.,
                15.,
                if selected { GOLD } else { SECONDARY },
            );
            text(
                d,
                46.,
                y + 4.,
                label,
                0.82,
                if selected { PRIMARY } else { SECONDARY },
            );
            let _ = screen_key;
        }
        rect(d, 22., 218., 82., 1., BORDER_SOFT);
        icon(d, "storage", 23., 233., 15., SECONDARY);
        text(d, 46., 232., "Local", 0.78, SECONDARY);
        text(d, 46., 247., "Storage", 0.68, MUTED);
        icon(d, "sd", 23., 269., 15., SECONDARY);
        text(d, 46., 268., "SD Card", 0.78, SECONDARY);
    }

    fn draw_premium_rows(
        &self,
        d: &mut Vec<Quad>,
        rows: &[Item],
        focus: usize,
        x: f32,
        y: f32,
        width: f32,
        visible: usize,
        artwork: bool,
        has_art: bool,
    ) {
        if rows.is_empty() {
            self.draw_premium_empty(
                d,
                x,
                y + 12.,
                width,
                "Nothing here yet",
                "Scan your library or connect storage",
            );
            return;
        }
        let start = focus
            .saturating_sub(visible.saturating_sub(2))
            .min(rows.len().saturating_sub(visible));
        for (position, row) in rows.iter().enumerate().skip(start).take(visible) {
            let row_y = y + (position - start) as f32 * 36.;
            let selected = position == focus;
            panel(
                d,
                x,
                row_y,
                width,
                32.,
                if selected { FOCUS } else { CARD },
                if selected { GOLD } else { BORDER_SOFT },
            );
            if artwork {
                artwork_card(
                    d,
                    x + 5.,
                    row_y + 4.,
                    24.,
                    if selected { FOCUS } else { CARD },
                    has_art && selected,
                );
                text(
                    d,
                    x + 39.,
                    row_y + 7.,
                    &fit_text(&row.label, 25),
                    0.9,
                    if selected { PRIMARY } else { SECONDARY },
                );
            } else {
                icon(
                    d,
                    row_icon(&row.key),
                    x + 10.,
                    row_y + 8.,
                    15.,
                    if selected { GOLD } else { SECONDARY },
                );
                text(
                    d,
                    x + 36.,
                    row_y + 7.,
                    &fit_text(&row.label, 28),
                    0.9,
                    if selected { PRIMARY } else { SECONDARY },
                );
            }
            if !row.secondary.is_empty() {
                text(
                    d,
                    x + width - 111.,
                    row_y + 9.,
                    &fit_text(&row.secondary, 15),
                    0.7,
                    if selected { GOLD_SOFT } else { MUTED },
                );
            }
            text(
                d,
                x + width - 18.,
                row_y + 9.,
                ">",
                0.95,
                if selected { GOLD } else { MUTED },
            );
        }
    }

    fn draw_premium_now_playing(&self, d: &mut Vec<Quad>, m: &AppModel, has_art: bool) {
        let Some(track) = m.current() else {
            self.draw_premium_empty(
                d,
                12.,
                52.,
                456.,
                "Nothing is playing",
                "Choose music to begin listening",
            );
            return;
        };

        panel(d, 12., 42., 456., 244., CARD, BORDER);
        artwork_card(d, 21., 51., 146., CARD_ALT, has_art);
        micro(d, 184., 53., "NOW PLAYING", GOLD_SOFT);
        text(
            d,
            184.,
            69.,
            &fit_text(display_or_unknown(&track.title), 31),
            1.32,
            PRIMARY,
        );
        text(
            d,
            184.,
            93.,
            &fit_text(display_or_unknown(&track.artist), 31),
            0.94,
            SECONDARY,
        );
        text(
            d,
            184.,
            111.,
            &fit_text(display_or_unknown(&track.album), 31),
            0.83,
            MUTED,
        );

        let codec = fit_text(&track.codec.to_uppercase(), 8);
        let format = fit_text(&technical_format(track), 9);
        badge(d, 184., 133., 58., &codec, GOLD);
        badge(d, 248., 133., 72., &format, SECONDARY);
        badge(d, 326., 133., 72., output_short(&m.output), SUCCESS);

        rect(d, 184., 166., 264., 4., BORDER);
        rect(
            d,
            184.,
            166.,
            264. * progress(m.position_ms, track.duration_ms),
            4.,
            GOLD,
        );
        circle(
            d,
            184. + 264. * progress(m.position_ms, track.duration_ms),
            168.,
            4.,
            GOLD,
        );
        text(d, 184., 178., &time(m.position_ms), 0.72, SECONDARY);
        text(d, 412., 178., &time(track.duration_ms), 0.72, MUTED);

        circle_outline(d, 239., 232., 19., BORDER);
        icon(d, "previous", 231., 224., 16., SECONDARY);
        circle_outline(d, 315., 232., 25., GOLD);
        icon(
            d,
            if matches!(
                m.playback,
                reborn_core::PlaybackState::Playing | reborn_core::PlaybackState::Buffering
            ) {
                "pause"
            } else {
                "play"
            },
            304.,
            221.,
            22.,
            PRIMARY,
        );
        circle_outline(d, 391., 232., 19., BORDER);
        icon(d, "next", 383., 224., 16., SECONDARY);

        self.draw_output_strip(d, m, track);
    }

    fn draw_premium_queue(&self, d: &mut Vec<Quad>, m: &AppModel, has_art: bool) {
        panel(d, 12., 42., 456., 58., CARD, BORDER);
        if let Some(track) = m.current() {
            artwork_card(d, 20., 50., 42., CARD_ALT, has_art);
            micro(d, 76., 49., "NOW PLAYING", GOLD_SOFT);
            text(
                d,
                76.,
                64.,
                &fit_text(display_or_unknown(&track.title), 28),
                0.95,
                PRIMARY,
            );
            text(
                d,
                76.,
                80.,
                &fit_text(display_or_unknown(&track.artist), 28),
                0.72,
                SECONDARY,
            );
            text(d, 394., 66., playback_glyph(m.playback), 1.15, GOLD);
        } else {
            micro(d, 24., 61., "NOW PLAYING", MUTED);
            text(d, 24., 76., "Nothing is playing", 0.95, SECONDARY);
        }
        micro(d, 14., 114., "UP NEXT", MUTED);
        circle_outline(d, 393., 111., 12., BORDER);
        icon(d, "shuffle", 386., 104., 14., SECONDARY);
        circle_outline(d, 429., 111., 12., BORDER);
        icon(d, "menu", 422., 104., 14., GOLD);

        let rows = self.rows(m, &[]);
        self.draw_premium_rows(
            d,
            &rows,
            m.navigation.focus,
            12.,
            126.,
            456.,
            4,
            true,
            has_art,
        );
    }

    fn draw_premium_settings(&self, d: &mut Vec<Quad>, m: &AppModel) {
        let category = settings_category_index(m.screen, m.navigation.focus);
        let entries = [
            ("Audio", "audio"),
            ("Playback", "playback"),
            ("Library", "library"),
            ("Bluetooth", "bluetooth"),
            ("Wi-Fi", "wifi"),
            ("Display", "display"),
            ("Power", "power"),
            ("System", "system"),
        ];
        panel(d, 10., 42., 106., 258., CARD, BORDER_SOFT);
        micro(d, 22., 53., "SETTINGS", MUTED);
        for (index, (label, key)) in entries.iter().enumerate() {
            let y = 69. + index as f32 * 27.5;
            let selected = index == category;
            if selected {
                panel(d, 16., y - 2., 94., 24., FOCUS, GOLD);
            }
            icon(
                d,
                row_icon(key),
                23.,
                y + 3.,
                13.,
                if selected { GOLD } else { SECONDARY },
            );
            text(
                d,
                43.,
                y + 3.,
                label,
                0.72,
                if selected { PRIMARY } else { SECONDARY },
            );
        }

        panel(d, 126., 42., 342., 258., CARD, BORDER);
        micro(d, 140., 53., "SYSTEM PREFERENCES", MUTED);
        text(d, 140., 64., screen_title(m.screen), 1.55, PRIMARY);
        let description = settings_description(m.screen);
        text(d, 140., 84., description, 0.68, SECONDARY);
        let rows = self.rows(m, &[]);
        self.draw_premium_rows(
            d,
            &rows,
            m.navigation.focus,
            136.,
            101.,
            322.,
            5,
            false,
            false,
        );
    }

    fn draw_premium_connectivity(&self, d: &mut Vec<Quad>, m: &AppModel) {
        if m.screen == Screen::Connectivity {
            micro(d, 14., 46., "CONNECTIONS", MUTED);
            text(d, 14., 57., "Stay connected", 1.55, PRIMARY);
            let cards = [
                (
                    12.,
                    "Bluetooth",
                    "bluetooth",
                    self.bluetooth_summary(),
                    self.bluetooth.powered,
                ),
                (
                    244.,
                    "Wi-Fi",
                    "wifi",
                    self.wifi_summary(),
                    self.wifi.powered,
                ),
            ];
            for (x, label, key, summary, active) in cards {
                let selected = (key == "bluetooth" && m.navigation.focus == 0)
                    || (key == "wifi" && m.navigation.focus == 1);
                panel(
                    d,
                    x,
                    88.,
                    224.,
                    108.,
                    if selected { FOCUS } else { CARD },
                    if selected { GOLD } else { BORDER },
                );
                icon(
                    d,
                    key,
                    x + 16.,
                    105.,
                    22.,
                    if selected { GOLD } else { SECONDARY },
                );
                text(
                    d,
                    x + 50.,
                    106.,
                    label,
                    1.05,
                    if selected { PRIMARY } else { SECONDARY },
                );
                text(
                    d,
                    x + 50.,
                    127.,
                    &fit_text(&summary, 20),
                    0.75,
                    if active { SUCCESS } else { MUTED },
                );
                micro(
                    d,
                    x + 16.,
                    171.,
                    if active { "AVAILABLE" } else { "OFFLINE" },
                    if active { SUCCESS } else { MUTED },
                );
                text(
                    d,
                    x + 194.,
                    166.,
                    ">",
                    1.1,
                    if selected { GOLD } else { MUTED },
                );
            }
            panel(d, 12., 210., 456., 68., CARD_ALT, BORDER_SOFT);
            micro(d, 26., 222., "OUTPUT", MUTED);
            icon(d, "headphones", 26., 239., 16., GOLD);
            text(d, 51., 239., &output_label(&m.output), 0.9, PRIMARY);
            text(
                d,
                51.,
                256.,
                "Audio follows the selected output automatically",
                0.68,
                SECONDARY,
            );
            return;
        }

        panel(d, 12., 42., 456., 47., CARD, BORDER);
        icon(
            d,
            if m.screen == Screen::Bluetooth {
                "bluetooth"
            } else {
                "wifi"
            },
            25.,
            56.,
            18.,
            GOLD,
        );
        text(d, 54., 53., screen_title(m.screen), 1.25, PRIMARY);
        let connection_message = if m.screen == Screen::Bluetooth {
            self.bluetooth.message(true)
        } else {
            self.wifi.message(false)
        };
        text(
            d,
            54.,
            72.,
            &fit_text(&connection_message, 48),
            0.72,
            SECONDARY,
        );
        let rows = self.rows(m, &[]);
        self.draw_premium_rows(
            d,
            &rows,
            m.navigation.focus,
            12.,
            101.,
            456.,
            5,
            false,
            false,
        );
    }

    fn draw_premium_collection(
        &self,
        d: &mut Vec<Quad>,
        m: &AppModel,
        tracks: &[Track],
        has_art: bool,
    ) {
        let (kind, name, detail) = match m.screen {
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
                ("ALBUM", name.to_owned(), artist.to_owned())
            }
            Screen::Artist => {
                let name = m
                    .navigation
                    .filter
                    .strip_prefix("artist:")
                    .unwrap_or("Artist");
                ("ARTIST", name.to_owned(), "Albums and tracks".into())
            }
            _ => ("COLLECTION", "Collection".into(), String::new()),
        };
        panel(d, 12., 42., 456., 84., CARD, BORDER);
        artwork_card(d, 20., 50., 68., CARD_ALT, has_art);
        micro(d, 104., 53., kind, GOLD_SOFT);
        text(d, 104., 67., &fit_text(&name, 32), 1.35, PRIMARY);
        text(d, 104., 91., &fit_text(&detail, 36), 0.82, SECONDARY);
        text(d, 104., 107., "Select for options", 0.68, MUTED);
        let rows = self.rows(m, tracks);
        self.draw_premium_rows(
            d,
            &rows,
            m.navigation.focus,
            12.,
            137.,
            456.,
            4,
            true,
            has_art,
        );
    }

    fn draw_premium_diagnostics(
        &self,
        d: &mut Vec<Quad>,
        m: &AppModel,
        tracks: &[Track],
        health: &str,
    ) {
        if m.navigation.filter == "audio" {
            self.draw_premium_audio_information(d, m);
            return;
        }
        micro(d, 14., 46., "SYSTEM", MUTED);
        text(d, 14., 57., "Everything is ready", 1.55, PRIMARY);
        text(
            d,
            14.,
            79.,
            "Detailed diagnostics remain available over SSH.",
            0.75,
            SECONDARY,
        );
        let rows = [
            ("Audio", health_label(health), "audio"),
            (
                "Storage",
                if m.sources.iter().any(|source| source.online) {
                    "OK"
                } else {
                    "Unavailable"
                },
                "storage",
            ),
            (
                "Library",
                if tracks.is_empty() { "Empty" } else { "OK" },
                "library",
            ),
            (
                "Bluetooth",
                if self.bluetooth.available {
                    "OK"
                } else {
                    "Starting"
                },
                "bluetooth",
            ),
            (
                "Wi-Fi",
                if self.wifi.available {
                    "OK"
                } else {
                    "Starting"
                },
                "wifi",
            ),
            ("System", "OK", "system"),
        ];
        for (index, (label, value, key)) in rows.iter().enumerate() {
            let x = 12. + (index % 2) as f32 * 228.;
            let y = 111. + (index / 2) as f32 * 55.;
            panel(d, x, y, 216., 45., CARD, BORDER_SOFT);
            icon(
                d,
                key,
                x + 13.,
                y + 13.,
                15.,
                if *value == "OK" { SUCCESS } else { GOLD },
            );
            text(d, x + 38., y + 9., label, 0.82, SECONDARY);
            text(
                d,
                x + 38.,
                y + 25.,
                value,
                0.78,
                if *value == "OK" { SUCCESS } else { PRIMARY },
            );
        }
    }

    fn draw_premium_audio_information(&self, d: &mut Vec<Quad>, m: &AppModel) {
        panel(d, 12., 42., 456., 246., CARD, BORDER);
        micro(d, 26., 54., "AUDIO", MUTED);
        text(d, 26., 65., "Audio information", 1.45, PRIMARY);
        let Some(track) = m.current() else {
            self.draw_premium_empty(
                d,
                26.,
                101.,
                428.,
                "Nothing is playing",
                "Start a track to see its format",
            );
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
            (
                "Bluetooth",
                if self.bluetooth.connection.is_empty() {
                    "Not connected".into()
                } else {
                    self.bluetooth.connection.clone()
                },
            ),
        ];
        for (index, (label, value)) in rows.iter().enumerate() {
            let y = 96. + index as f32 * 29.;
            rect(d, 26., y + 23., 428., 1., BORDER_SOFT);
            text(d, 26., y + 5., label, 0.78, MUTED);
            text(d, 180., y + 5., &fit_text(value, 31), 0.8, PRIMARY);
        }
    }

    fn draw_premium_pairing(&self, d: &mut Vec<Quad>, pair: &str) {
        panel(d, 32., 60., 416., 220., CARD, GOLD);
        icon(d, "bluetooth", 62., 86., 30., GOLD);
        micro(d, 62., 126., "PAIRING REQUEST", GOLD_SOFT);
        text(d, 62., 139., "Connect this device?", 1.45, PRIMARY);
        text(d, 62., 166., &fit_text(pair, 42), 0.92, SECONDARY);
        panel(d, 62., 202., 156., 39., FOCUS, GOLD);
        text(d, 79., 216., "Select  Allow", 0.86, PRIMARY);
        text(d, 258., 216., "Back  Reject", 0.86, DANGER);
    }

    fn draw_premium_text_entry(&self, d: &mut Vec<Quad>) {
        panel(d, 18., 47., 444., 244., CARD, BORDER);
        micro(d, 34., 60., "WI-FI", MUTED);
        text(d, 34., 70., "Join network", 1.5, PRIMARY);
        text(d, 34., 96., &fit_text(&self.ssid, 43), 0.86, SECONDARY);
        panel(d, 34., 119., 412., 35., RAISED, BORDER_SOFT);
        text(d, 48., 132., &"*".repeat(self.password.len()), 1.0, PRIMARY);
        micro(d, 34., 177., "CHARACTER", MUTED);
        panel(d, 34., 186., 84., 49., FOCUS, GOLD);
        centered_text(
            d,
            76.,
            198.,
            &(LETTERS[self.letter] as char).to_string(),
            2.5,
            GOLD,
        );
        text(d, 138., 198., "Wheel choose", 0.8, SECONDARY);
        text(d, 138., 215., "Select add · Previous delete", 0.72, MUTED);
        text(d, 34., 259., "Hold Select to connect", 0.78, GOLD_SOFT);
        text(d, 302., 259., "Back cancel", 0.78, MUTED);
    }

    fn draw_premium_empty(
        &self,
        d: &mut Vec<Quad>,
        x: f32,
        y: f32,
        width: f32,
        title: &str,
        detail: &str,
    ) {
        panel(d, x, y, width, 84., CARD_ALT, BORDER_SOFT);
        icon(d, "music", x + 18., y + 22., 22., GOLD);
        text(d, x + 53., y + 18., &fit_text(title, 34), 1.08, PRIMARY);
        text(d, x + 53., y + 43., &fit_text(detail, 40), 0.7, SECONDARY);
    }

    fn draw_premium_modal(&self, d: &mut Vec<Quad>, m: &AppModel) {
        rect(d, 0., 31., 480., 299., 0x05070bd9);
        let rows = self.modal_rows(m);
        let title = match m.navigation.modal {
            Some(Modal::ContextMenu) => "Track options",
            Some(Modal::PowerMenu) => "Power",
            Some(Modal::Confirm(_)) => "Are you sure?",
            None => "",
        };
        panel(d, 34., 50., 412., 248., CARD, GOLD);
        micro(d, 54., 65., "ACTION", GOLD_SOFT);
        text(d, 54., 75., title, 1.42, PRIMARY);
        if let Some(Modal::Confirm(action)) = m.navigation.modal {
            text(d, 54., 99., confirmation_copy(action), 0.75, SECONDARY);
        }
        for (index, row) in rows.iter().enumerate() {
            let y = if matches!(m.navigation.modal, Some(Modal::Confirm(_))) {
                133.
            } else {
                116.
            } + index as f32 * 31.;
            let selected = index == m.navigation.modal_focus;
            panel(
                d,
                50.,
                y,
                380.,
                26.,
                if selected { FOCUS } else { CARD_ALT },
                if selected { GOLD } else { BORDER_SOFT },
            );
            text(
                d,
                64.,
                y + 8.,
                &fit_text(&row.label, 39),
                0.84,
                if selected { PRIMARY } else { SECONDARY },
            );
            if selected {
                text(d, 411., y + 7., ">", 0.95, GOLD);
            }
        }
    }

    fn draw_premium_mini_player(&self, d: &mut Vec<Quad>, m: &AppModel, has_art: bool) {
        let Some(track) = m.current() else { return };
        panel(d, 12., 286., 456., 37., CARD_ALT, BORDER_SOFT);
        artwork_card(d, 18., 292., 25., CARD, has_art);
        text(
            d,
            52.,
            291.,
            &fit_text(display_or_unknown(&track.title), 29),
            0.82,
            PRIMARY,
        );
        text(
            d,
            52.,
            306.,
            &fit_text(display_or_unknown(&track.artist), 30),
            0.63,
            MUTED,
        );
        icon(
            d,
            if matches!(
                m.playback,
                reborn_core::PlaybackState::Playing | reborn_core::PlaybackState::Buffering
            ) {
                "pause"
            } else {
                "play"
            },
            430.,
            296.,
            14.,
            GOLD,
        );
    }

    fn draw_output_strip(&self, d: &mut Vec<Quad>, m: &AppModel, track: &Track) {
        panel(d, 12., 294., 456., 29., CARD_ALT, BORDER_SOFT);
        micro(d, 22., 302., "OUTPUT", MUTED);
        icon(d, "headphones", 89., 299., 14., GOLD);
        text(d, 109., 302., output_short(&m.output), 0.72, PRIMARY);
        text(d, 164., 302., "·", 0.85, MUTED);
        text(
            d,
            181.,
            302.,
            &fit_text(&track.codec.to_uppercase(), 8),
            0.72,
            SECONDARY,
        );
        text(d, 273., 302., "VOL", 0.66, MUTED);
        rect(d, 307., 305., 103., 3., BORDER);
        rect(
            d,
            307.,
            305.,
            103. * (m.settings.volume as f32 / 100.),
            3.,
            GOLD,
        );
        text(
            d,
            421.,
            301.,
            &format!("{}", m.settings.volume),
            0.72,
            PRIMARY,
        );
    }

    fn draw_brand_footer(&self, d: &mut Vec<Quad>) {
        rect(d, 0., 331., 480., 29., HEADER);
        rect(d, 0., 330., 480., 1., BORDER_SOFT);
        micro(d, 14., 340., "REborn  ·  Y2 AUDIO SYSTEMS", MUTED);
        text(d, 381., 340., "BACK", 0.66, GOLD_SOFT);
    }

    fn draw_toast(&self, d: &mut Vec<Quad>, message: &str) {
        let width = (message.chars().count() as f32 * 6.2 + 28.).clamp(150., 365.);
        let x = (480. - width) / 2.;
        panel(d, x, 304., width, 25., FOCUS, GOLD);
        centered_text(d, 240., 312., &fit_text(message, 48), 0.72, PRIMARY);
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

fn rounded_rect(d: &mut Vec<Quad>, x: f32, y: f32, w: f32, h: f32, radius: f32, color: u32) {
    if w <= 0. || h <= 0. {
        return;
    }
    let radius = radius.max(0.).min(w.min(h) / 2.);
    if radius < 1. {
        rect(d, x, y, w, h, color);
        return;
    }
    rect(d, x + radius, y, w - radius * 2., h, color);
    rect(d, x, y + radius, w, h - radius * 2., color);
    let rows = radius.ceil() as i32;
    for row in 0..rows {
        let top_distance = radius - row as f32 - 0.5;
        let half = (radius * radius - top_distance * top_distance)
            .max(0.)
            .sqrt();
        let inset = (radius - half).max(0.);
        rect(d, x + inset, y + row as f32, w - inset * 2., 1., color);
        rect(
            d,
            x + inset,
            y + h - row as f32 - 1.,
            w - inset * 2.,
            1.,
            color,
        );
    }
}

fn panel(d: &mut Vec<Quad>, x: f32, y: f32, w: f32, h: f32, fill: u32, border: u32) {
    rounded_rect(d, x, y, w, h, 7., border);
    if w > 2. && h > 2. {
        rounded_rect(d, x + 1., y + 1., w - 2., h - 2., 6., fill);
    }
}

fn circle(d: &mut Vec<Quad>, cx: f32, cy: f32, radius: f32, color: u32) {
    let radius = radius.max(0.).ceil() as i32;
    let squared = radius * radius;
    for dy in -radius..=radius {
        let span = ((squared - dy * dy).max(0) as f32).sqrt() as i32;
        rect(
            d,
            cx - span as f32,
            cy + dy as f32,
            span as f32 * 2. + 1.,
            1.,
            color,
        );
    }
}

fn circle_outline(d: &mut Vec<Quad>, cx: f32, cy: f32, radius: f32, color: u32) {
    circle(d, cx, cy, radius, color);
    circle(d, cx, cy, (radius - 2.).max(0.), CARD);
}

fn line(d: &mut Vec<Quad>, x1: f32, y1: f32, x2: f32, y2: f32, width: f32, color: u32) {
    let steps = ((x2 - x1).abs().max((y2 - y1).abs())).ceil() as usize;
    for step in 0..=steps.max(1) {
        let t = step as f32 / steps.max(1) as f32;
        rect(
            d,
            x1 + (x2 - x1) * t - width / 2.,
            y1 + (y2 - y1) * t - width / 2.,
            width,
            width,
            color,
        );
    }
}

fn triangle(d: &mut Vec<Quad>, x: f32, y: f32, width: f32, height: f32, color: u32, right: bool) {
    let rows = height.max(1.).ceil() as usize;
    for row in 0..rows {
        let amount = width * (row as f32 + 1.) / rows as f32;
        let row_y = y + row as f32;
        if right {
            rect(d, x, row_y, amount, 1., color);
        } else {
            rect(d, x + width - amount, row_y, amount, 1., color);
        }
    }
}

fn icon(d: &mut Vec<Quad>, kind: &str, x: f32, y: f32, size: f32, color: u32) {
    let s = size.max(6.);
    match kind {
        "play" => triangle(d, x, y, s, s, color, true),
        "pause" => {
            rect(d, x + s * 0.18, y, s * 0.22, s, color);
            rect(d, x + s * 0.60, y, s * 0.22, s, color);
        }
        "previous" => {
            rect(d, x + s * 0.12, y, s * 0.13, s, color);
            triangle(d, x + s * 0.28, y, s * 0.50, s, color, false);
        }
        "next" => {
            triangle(d, x + s * 0.10, y, s * 0.50, s, color, true);
            rect(d, x + s * 0.76, y, s * 0.13, s, color);
        }
        "headphones" => {
            line(
                d,
                x + s * 0.18,
                y + s * 0.65,
                x + s * 0.18,
                y + s * 0.38,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.18,
                y + s * 0.38,
                x + s * 0.50,
                y + s * 0.10,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.50,
                y + s * 0.10,
                x + s * 0.82,
                y + s * 0.38,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.82,
                y + s * 0.38,
                x + s * 0.82,
                y + s * 0.65,
                s * 0.10,
                color,
            );
            rect(d, x + s * 0.08, y + s * 0.58, s * 0.20, s * 0.28, color);
            rect(d, x + s * 0.72, y + s * 0.58, s * 0.20, s * 0.28, color);
        }
        "bluetooth" => {
            line(
                d,
                x + s * 0.50,
                y + s * 0.05,
                x + s * 0.50,
                y + s * 0.95,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.50,
                y + s * 0.05,
                x + s * 0.85,
                y + s * 0.33,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.85,
                y + s * 0.33,
                x + s * 0.18,
                y + s * 0.70,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.18,
                y + s * 0.30,
                x + s * 0.85,
                y + s * 0.70,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.85,
                y + s * 0.70,
                x + s * 0.50,
                y + s * 0.95,
                s * 0.10,
                color,
            );
        }
        "wifi" => {
            line(
                d,
                x + s * 0.15,
                y + s * 0.35,
                x + s * 0.50,
                y + s * 0.12,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.50,
                y + s * 0.12,
                x + s * 0.85,
                y + s * 0.35,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.28,
                y + s * 0.56,
                x + s * 0.50,
                y + s * 0.40,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.50,
                y + s * 0.40,
                x + s * 0.72,
                y + s * 0.56,
                s * 0.10,
                color,
            );
            circle(d, x + s * 0.50, y + s * 0.80, s * 0.10, color);
        }
        "music" | "song" => {
            rect(d, x + s * 0.62, y + s * 0.08, s * 0.12, s * 0.62, color);
            rect(d, x + s * 0.30, y + s * 0.25, s * 0.44, s * 0.12, color);
            circle(d, x + s * 0.26, y + s * 0.78, s * 0.18, color);
            circle(d, x + s * 0.68, y + s * 0.68, s * 0.18, color);
        }
        "album" => {
            rounded_rect(d, x, y, s, s, s * 0.12, color);
            circle(d, x + s * 0.50, y + s * 0.50, s * 0.22, CARD);
            circle(d, x + s * 0.50, y + s * 0.50, s * 0.07, color);
        }
        "artist" => {
            circle(d, x + s * 0.50, y + s * 0.30, s * 0.18, color);
            rounded_rect(
                d,
                x + s * 0.18,
                y + s * 0.55,
                s * 0.64,
                s * 0.30,
                s * 0.13,
                color,
            );
        }
        "folder" => {
            rect(d, x + s * 0.08, y + s * 0.25, s * 0.84, s * 0.58, color);
            rect(d, x + s * 0.16, y + s * 0.13, s * 0.34, s * 0.18, color);
            rect(d, x + s * 0.17, y + s * 0.35, s * 0.66, s * 0.08, CARD);
        }
        "storage" | "sd" => {
            rounded_rect(
                d,
                x + s * 0.18,
                y + s * 0.06,
                s * 0.64,
                s * 0.88,
                s * 0.08,
                color,
            );
            for row in 0..3 {
                rect(
                    d,
                    x + s * 0.34,
                    y + s * (0.28 + row as f32 * 0.16),
                    s * 0.32,
                    s * 0.07,
                    CARD,
                );
            }
        }
        "settings" | "system" => {
            circle(d, x + s * 0.50, y + s * 0.50, s * 0.35, color);
            circle(d, x + s * 0.50, y + s * 0.50, s * 0.14, CARD);
            for angle in 0..4 {
                let (dx, dy) = match angle {
                    0 => (0., -s * 0.46),
                    1 => (s * 0.46, 0.),
                    2 => (0., s * 0.46),
                    _ => (-s * 0.46, 0.),
                };
                rect(
                    d,
                    x + s * 0.50 + dx - s * 0.07,
                    y + s * 0.50 + dy - s * 0.07,
                    s * 0.14,
                    s * 0.14,
                    color,
                );
            }
        }
        "queue" => {
            for row in 0..3 {
                let yy = y + s * (0.18 + row as f32 * 0.30);
                rect(d, x + s * 0.08, yy, s * 0.14, s * 0.14, color);
                rect(d, x + s * 0.34, yy + s * 0.02, s * 0.58, s * 0.10, color);
            }
        }
        "connectivity" | "output" => {
            circle(d, x + s * 0.50, y + s * 0.50, s * 0.12, color);
            line(
                d,
                x + s * 0.50,
                y + s * 0.38,
                x + s * 0.50,
                y + s * 0.12,
                s * 0.09,
                color,
            );
            line(
                d,
                x + s * 0.39,
                y + s * 0.57,
                x + s * 0.16,
                y + s * 0.76,
                s * 0.09,
                color,
            );
            line(
                d,
                x + s * 0.61,
                y + s * 0.57,
                x + s * 0.84,
                y + s * 0.76,
                s * 0.09,
                color,
            );
            circle(d, x + s * 0.14, y + s * 0.80, s * 0.10, color);
            circle(d, x + s * 0.86, y + s * 0.80, s * 0.10, color);
        }
        "scan" => {
            line(
                d,
                x + s * 0.15,
                y + s * 0.35,
                x + s * 0.15,
                y + s * 0.15,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.15,
                y + s * 0.15,
                x + s * 0.35,
                y + s * 0.15,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.65,
                y + s * 0.15,
                x + s * 0.85,
                y + s * 0.15,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.85,
                y + s * 0.15,
                x + s * 0.85,
                y + s * 0.35,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.15,
                y + s * 0.65,
                x + s * 0.15,
                y + s * 0.85,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.15,
                y + s * 0.85,
                x + s * 0.35,
                y + s * 0.85,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.65,
                y + s * 0.85,
                x + s * 0.85,
                y + s * 0.85,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.85,
                y + s * 0.85,
                x + s * 0.85,
                y + s * 0.65,
                s * 0.10,
                color,
            );
        }
        "shuffle" => {
            line(
                d,
                x + s * 0.12,
                y + s * 0.25,
                x + s * 0.34,
                y + s * 0.25,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.34,
                y + s * 0.25,
                x + s * 0.68,
                y + s * 0.75,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.68,
                y + s * 0.75,
                x + s * 0.88,
                y + s * 0.75,
                s * 0.10,
                color,
            );
            triangle(
                d,
                x + s * 0.73,
                y + s * 0.67,
                s * 0.20,
                s * 0.16,
                color,
                true,
            );
            line(
                d,
                x + s * 0.12,
                y + s * 0.75,
                x + s * 0.34,
                y + s * 0.75,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.34,
                y + s * 0.75,
                x + s * 0.68,
                y + s * 0.25,
                s * 0.10,
                color,
            );
            line(
                d,
                x + s * 0.68,
                y + s * 0.25,
                x + s * 0.88,
                y + s * 0.25,
                s * 0.10,
                color,
            );
            triangle(
                d,
                x + s * 0.73,
                y + s * 0.17,
                s * 0.20,
                s * 0.16,
                color,
                true,
            );
        }
        "menu" => {
            for row in 0..3 {
                rounded_rect(
                    d,
                    x + s * 0.12,
                    y + s * (0.20 + row as f32 * 0.28),
                    s * 0.76,
                    s * 0.10,
                    s * 0.04,
                    color,
                );
            }
        }
        "display" => {
            rounded_rect(
                d,
                x + s * 0.08,
                y + s * 0.18,
                s * 0.84,
                s * 0.58,
                s * 0.08,
                color,
            );
            rect(d, x + s * 0.38, y + s * 0.82, s * 0.24, s * 0.08, color);
        }
        "power" => {
            line(
                d,
                x + s * 0.50,
                y + s * 0.06,
                x + s * 0.50,
                y + s * 0.43,
                s * 0.12,
                color,
            );
            circle_outline(d, x + s * 0.50, y + s * 0.52, s * 0.34, color);
            rect(d, x + s * 0.40, y + s * 0.10, s * 0.20, s * 0.25, CARD);
        }
        _ => text(d, x, y + s * 0.12, "·", s / 8., color),
    }
}

fn badge(d: &mut Vec<Quad>, x: f32, y: f32, width: f32, label: &str, color: u32) {
    panel(d, x, y, width, 18., CARD_ALT, color);
    centered_text(d, x + width / 2., y + 5., label, 0.62, color);
}

fn centered_text(d: &mut Vec<Quad>, center_x: f32, y: f32, value: &str, scale: f32, color: u32) {
    let width = value.chars().count() as f32 * 8. * scale;
    text(d, center_x - width / 2., y, value, scale, color);
}

fn micro(d: &mut Vec<Quad>, x: f32, y: f32, value: &str, color: u32) {
    let mut cursor = x;
    for character in value.chars().take(42) {
        let value = character.to_ascii_uppercase().to_string();
        text(d, cursor, y, &value, 0.58, color);
        cursor += 6.2;
    }
}

fn fit_text(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_owned();
    }
    if max_chars <= 3 {
        return value.chars().take(max_chars).collect();
    }
    format!(
        "{}...",
        value.chars().take(max_chars - 3).collect::<String>()
    )
}

fn artwork_card(d: &mut Vec<Quad>, x: f32, y: f32, size: f32, fallback: u32, has_art: bool) {
    if has_art {
        let mut art = Quad::rect(x, y, size, size, PRIMARY);
        art.artwork = true;
        d.push(art);
        rounded_rect(d, x, y, size, 1., 0., GOLD);
    } else {
        rounded_rect(d, x, y, size, size, 5., fallback);
        icon(
            d,
            "album",
            x + size * 0.28,
            y + size * 0.28,
            size * 0.44,
            GOLD,
        );
    }
}

fn home_icon(key: &str) -> &'static str {
    match key {
        "music" | "now_playing" => "music",
        "queue" => "queue",
        "connectivity" => "connectivity",
        "settings" => "settings",
        "albums" | "album" => "album",
        "artists" | "artist" => "artist",
        "tracks" | "song" => "song",
        "folders" | "folder" => "folder",
        "scan_library" | "scan" => "scan",
        "storage" => "storage",
        "sd" => "sd",
        _ => "music",
    }
}

fn row_icon(key: &str) -> &'static str {
    if key.starts_with("bt") || key == "bluetooth" {
        return "bluetooth";
    }
    if key.starts_with("wifi") || key == "wifi" {
        return "wifi";
    }
    if key.contains("power") || key == "reboot" {
        return "power";
    }
    if key.contains("storage") || key == "internal" || key == "sd" {
        return "storage";
    }
    if key.contains("scan") || key.contains("library") {
        return "scan";
    }
    if key.contains("audio") || key == "output" {
        return "headphones";
    }
    if key.contains("display") || key == "timeout" {
        return "display";
    }
    home_icon(key)
}

fn settings_category_index(screen: Screen, focus: usize) -> usize {
    match screen {
        Screen::Settings => focus.min(7),
        Screen::SettingsAudio => 0,
        Screen::SettingsPlayback => 1,
        Screen::SettingsLibrary => 2,
        Screen::SettingsBluetooth => 3,
        Screen::SettingsWifi => 4,
        Screen::SettingsDisplay => 5,
        Screen::SettingsPower => 6,
        Screen::SettingsSystem => 7,
        _ => 0,
    }
}

fn settings_description(screen: Screen) -> &'static str {
    match screen {
        Screen::SettingsAudio => "Output, ReplayGain and equalizer",
        Screen::SettingsPlayback => "How your music moves between tracks",
        Screen::SettingsLibrary => "Music sources and library maintenance",
        Screen::SettingsBluetooth => "Pair and manage wireless headphones",
        Screen::SettingsWifi => "Connect to networks and saved Wi-Fi",
        Screen::SettingsDisplay => "Brightness and screen timeout",
        Screen::SettingsPower => "Power and idle behavior",
        Screen::SettingsSystem => "About, diagnostics and safe actions",
        _ => "Choose a category",
    }
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
