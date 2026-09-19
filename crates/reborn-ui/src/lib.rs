#![forbid(unsafe_code)]

mod components;
mod screens;
pub mod theme;

use reborn_core::{
    Action, AppModel, AudioOutput, ConfirmAction, Modal, RadioScan, RepeatMode, Screen, Track,
};
use reborn_graphics::Quad;
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

pub use reborn_core::Effect;
pub use screens::PreviewScreen;

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

    pub(crate) fn message(&self, bluetooth: bool) -> String {
        let name = if bluetooth { "Bluetooth" } else { "Wi-Fi" };
        match self.scan {
            RadioScan::Starting => return format!("Starting {name}..."),
            RadioScan::Scanning => return format!("Scanning... {} found", self.count),
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PowerView {
    pub battery_percent: Option<u8>,
    pub charging: bool,
}

/// Presentation-owned transient state. Playback, queue, navigation, settings,
/// and screen state remain authoritative in `AppModel`; services are reached
/// only through the typed `Effect` returned by `action`.
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

const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 !@#$%^&*()-_=+[]{};:'\",.<>/?\\|`~";

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
        m.navigation.focus = usize::from(screen == Screen::NowPlaying);
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
                Item::new("Music", "music").with_secondary("Albums · Artists · Songs"),
                Item::new("Now Playing", "now_playing").with_secondary("Open the player"),
                Item::new("Queue", "queue").with_secondary("Up next"),
                Item::new("Connectivity", "connectivity").with_secondary("Bluetooth · Wi-Fi"),
                Item::new("Settings", "settings").with_secondary("Audio · Playback · System"),
            ],
            Screen::Music => vec![
                Item::new("Albums", "albums"),
                Item::new("Artists", "artists"),
                Item::new("Songs", "tracks"),
                Item::new("Folders", "folders"),
                Item::new("Scan Library", "scan_library").with_secondary(if m.library.scanning {
                    "Scanning..."
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
                let mut rows = vec![Item::new(format!("Play {name}"), "play:collection")];
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
                Item::new("Wi-Fi", "wifi").with_secondary(self.wifi_summary()),
                Item::new("Bluetooth", "bluetooth").with_secondary(self.bluetooth_summary()),
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
                Item::new("Output", "output").with_secondary(components::output_label(&m.output)),
                Item::new("ReplayGain", "replay_gain")
                    .with_secondary(replay_gain_label(m.settings.replay_gain)),
                Item::new("Equalizer / Tone Control", "equalizer")
                    .with_secondary(if m.settings.eq_enabled { "On" } else { "Off" }),
                Item::new("Audio Information", "audio_info")
                    .with_secondary("Codec · sample rate · output"),
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
                Item::new("Resume Playback", "resume").with_secondary("On"),
            ],
            Screen::SettingsLibrary => vec![
                Item::new("Internal Storage", "internal").with_secondary("Music library"),
                Item::new("SD Card", "sd").with_secondary(
                    if m.sources
                        .iter()
                        .any(|source| source.id == "sd" && source.online)
                    {
                        "Available"
                    } else {
                        "Not inserted"
                    },
                ),
                Item::new("Scan Library", "scan_library"),
                Item::new("Rebuild Library", "rebuild_library"),
            ],
            Screen::SettingsDisplay => vec![
                Item::new("Brightness", "brightness").with_secondary("Automatic"),
                Item::new("Screen Timeout", "timeout")
                    .with_secondary(timeout_label(m.settings.screen_timeout_seconds)),
            ],
            Screen::SettingsPower => {
                vec![Item::new("Power Menu", "power_menu").with_secondary("Shut down or reboot")]
            }
            Screen::SettingsSystem => vec![
                Item::new("About Reborn", "about").with_secondary(reborn_core::VERSION),
                Item::new("Diagnostics", "diagnostics").with_secondary("Health · metrics · logs"),
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
                Item::new(
                    if track.title.is_empty() {
                        track.filename.clone()
                    } else {
                        track.title.clone()
                    },
                    format!("track:{index}"),
                )
                .with_secondary(format!(
                    "{} · {}",
                    display_or_unknown(&track.artist),
                    display_or_unknown(&track.album)
                ))
            })
            .collect()
    }

    fn queue_rows(&self, m: &AppModel) -> Vec<Item> {
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

    pub(crate) fn bluetooth_summary(&self) -> String {
        if !self.bluetooth.powered {
            "Off".into()
        } else if !self.bluetooth.connection.is_empty() {
            self.bluetooth.connection.clone()
        } else {
            "On · No device".into()
        }
    }
    pub(crate) fn wifi_summary(&self) -> String {
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
            Screen::Album => vec![
                Item::new("Play Album", "album_play"),
                Item::new("Shuffle Album", "album_shuffle"),
                Item::new("Play Next", "album_next"),
                Item::new("Add to Queue", "album_queue"),
                Item::new("Album Information", "album_info"),
            ],
            Screen::Tracks | Screen::Artist | Screen::NowPlaying => vec![
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
                Item::new("Cancel", "cancel"),
                Item::new("Confirm", "confirm"),
            ],
            None => vec![],
        }
    }

    pub(crate) fn modal_rows_public(&self, m: &AppModel) -> Vec<Item> {
        self.modal_rows(m)
    }

    pub(crate) fn modal_copy(&self, m: &AppModel) -> (&'static str, &'static str) {
        match m.navigation.modal {
            Some(Modal::PowerMenu) => ("Power menu", "Choose an action for the player."),
            Some(Modal::Confirm(action)) => match action {
                ConfirmAction::ClearQueue => {
                    ("Clear queue?", "Remove all tracks after the current one.")
                }
                ConfirmAction::RebuildLibrary => {
                    ("Rebuild library?", "Scan storage and rebuild the index.")
                }
                ConfirmAction::ForgetBluetooth => {
                    ("Forget device?", "The paired device will be removed.")
                }
                ConfirmAction::ForgetWifi => {
                    ("Forget network?", "The saved network will be removed.")
                }
                ConfirmAction::PowerOff => ("Power off?", "The player will shut down safely."),
                ConfirmAction::Reboot => ("Reboot?", "The player will restart safely."),
            },
            Some(Modal::ContextMenu) => ("Track options", "Choose an action for the focused item."),
            None => ("", ""),
        }
    }

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
                if m.navigation.modal.is_none() {
                    let rows = self.rows(m, tracks);
                    if rows.get(m.navigation.focus).is_some_and(|row| row.enabled)
                        && !self.context_rows(m).is_empty()
                    {
                        m.navigation.modal = Some(Modal::ContextMenu);
                        m.navigation.modal_focus = 0;
                        m.navigation.context_target = Some(m.navigation.focus);
                    }
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
        self.select_row(m, tracks, &row.key.clone())
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
            Screen::Artist | Screen::Album | Screen::Tracks => {
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
            Screen::Queue => {
                if let Some(index) = key
                    .strip_prefix("queue:")
                    .and_then(|value| value.parse::<usize>().ok())
                {
                    if let Some(track) = m.queue.get(index) {
                        if let Some(library_index) =
                            tracks.iter().position(|candidate| candidate.id == track.id)
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
            Screen::Bluetooth | Screen::SettingsBluetooth => return self.bluetooth_action(key),
            Screen::Wifi | Screen::SettingsWifi => return self.wifi_action(key),
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
                    self.flash("Connect Bluetooth headphones first");
                }
                "replay_gain" => {
                    return Effect::SetReplayGain(match m.settings.replay_gain {
                        reborn_core::ReplayGainMode::Off => reborn_core::ReplayGainMode::Track,
                        reborn_core::ReplayGainMode::Track => reborn_core::ReplayGainMode::Album,
                        reborn_core::ReplayGainMode::Album => reborn_core::ReplayGainMode::Off,
                    })
                }
                "equalizer" => return Effect::ToggleEq,
                "audio_info" => Self::go(m, Screen::Diagnostics, "audio"),
                _ => {}
            },
            Screen::SettingsPlayback => match key {
                "shuffle" => return Effect::SetShuffle(!m.settings.shuffle),
                "repeat" => {
                    return Effect::SetRepeat(match m.settings.repeat {
                        RepeatMode::Off => RepeatMode::Track,
                        RepeatMode::Track => RepeatMode::All,
                        RepeatMode::All => RepeatMode::Off,
                    })
                }
                "gapless" => return Effect::SetGapless(!m.settings.gapless_enabled),
                "crossfade" => {
                    return Effect::SetCrossfade(match m.settings.crossfade_ms {
                        0 => 5_000,
                        5_000 => 10_000,
                        10_000 => 15_000,
                        _ => 0,
                    })
                }
                _ => {}
            },
            Screen::SettingsLibrary => match key {
                "internal" => self.flash("Internal storage · library source"),
                "sd" => self.flash(
                    if m.sources
                        .iter()
                        .any(|source| source.id == "sd" && source.online)
                    {
                        "SD card available"
                    } else {
                        "Insert an SD card to use it"
                    },
                ),
                "scan_library" => return Effect::ScanLibrary,
                "rebuild_library" => {
                    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::RebuildLibrary));
                    m.navigation.modal_focus = 0;
                }
                _ => {}
            },
            Screen::SettingsDisplay => {
                if key == "timeout" {
                    return Effect::SetScreenTimeout(match m.settings.screen_timeout_seconds {
                        15 => 30,
                        30 => 60,
                        60 => 120,
                        120 => 0,
                        _ => 15,
                    });
                }
            }
            Screen::SettingsPower => {
                if key == "power_menu" {
                    m.navigation.modal = Some(Modal::PowerMenu);
                    m.navigation.modal_focus = 2;
                }
            }
            Screen::SettingsSystem => match key {
                "about" => self.flash(format!("Reborn {}", reborn_core::VERSION)),
                "diagnostics" => Self::go(m, Screen::Diagnostics, ""),
                "reboot" => {
                    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::Reboot));
                    m.navigation.modal_focus = 0;
                }
                "power_off" => {
                    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::PowerOff));
                    m.navigation.modal_focus = 0;
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
        let Some(index) = key
            .strip_prefix("track:")
            .and_then(|value| value.parse().ok())
        else {
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

    fn bluetooth_action(&mut self, key: &str) -> Effect {
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

    fn wifi_action(&mut self, key: &str) -> Effect {
        match key {
            "wifi_power" => Effect::WifiPower,
            "wifi_scan" => Effect::WifiScan,
            key if key.starts_with("saved:") => key
                .strip_prefix("saved:")
                .and_then(|id| id.parse().ok())
                .map(Effect::WifiSaved)
                .unwrap_or(Effect::None),
            _ => {
                self.ssid = key.into();
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
                    m.navigation.modal_focus = 0;
                    Effect::None
                }
                "reboot" => {
                    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::Reboot));
                    m.navigation.modal_focus = 0;
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
                m.navigation.modal = None;
                m.navigation.context_target = None;
                match confirm {
                    ConfirmAction::ClearQueue => Effect::ClearQueue,
                    ConfirmAction::RebuildLibrary => Effect::RebuildLibrary,
                    ConfirmAction::PowerOff => Effect::PowerOff,
                    ConfirmAction::Reboot => Effect::Reboot,
                    ConfirmAction::ForgetBluetooth | ConfirmAction::ForgetWifi => Effect::None,
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
                        self.flash(
                            track_index
                                .and_then(|i| tracks.get(i))
                                .map(track_info)
                                .unwrap_or_else(|| "Track information unavailable".into()),
                        );
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
                        m.navigation.modal_focus = 0;
                        m.navigation.context_target = Some(target);
                        Effect::None
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
                self.letter = (self.letter + LETTERS.len() - usize::from(steps) % LETTERS.len())
                    % LETTERS.len()
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

    pub fn draw(
        &self,
        m: &AppModel,
        tracks: &[Track],
        health: &str,
        has_art: bool,
        power: PowerView,
    ) -> Vec<Quad> {
        screens::draw(self, m, tracks, health, has_art, power)
    }
    pub fn draw_preview(
        &self,
        m: AppModel,
        tracks: &[Track],
        power: PowerView,
        screen: PreviewScreen,
    ) -> Vec<Quad> {
        screens::preview(self, m, tracks, power, screen)
    }
}

pub fn font_atlas() -> Vec<u8> {
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/fonts/reborn-ui.rgba"
    ))
    .to_vec()
}
pub fn display_font_atlas() -> Vec<u8> {
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/fonts/reborn-display.rgba"
    ))
    .to_vec()
}
pub fn icons_atlas() -> Vec<u8> {
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/icons/reborn-icons.rgba"
    ))
    .to_vec()
}

fn display_or_unknown(value: &str) -> &str {
    if value.is_empty() {
        "Unknown"
    } else {
        value
    }
}
fn track_matches(track: &Track, filter: &str) -> bool {
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
            .is_some_and(|parent| parent.to_string_lossy().starts_with(value));
    }
    true
}
fn track_info(track: &Track) -> String {
    format!(
        "{} · {} · {} Hz",
        display_or_unknown(&track.codec),
        display_or_unknown(&track.album),
        track.sample_rate
    )
}
fn replay_gain_label(mode: reborn_core::ReplayGainMode) -> &'static str {
    match mode {
        reborn_core::ReplayGainMode::Off => "Off",
        reborn_core::ReplayGainMode::Track => "Track",
        reborn_core::ReplayGainMode::Album => "Album",
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
        15 => "15 sec".into(),
        30 => "30 sec".into(),
        60 => "1 min".into(),
        120 => "2 min",
        value => return format!("{value} sec"),
    }
    .into()
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
        assert_eq!(
            ui.action(&mut app, &[], Action::WheelClockwise(1)),
            Effect::None
        );
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
        ui.action(&mut app, &[], Action::Back);
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
        assert_eq!(app.navigation.modal_focus, 0);
    }
    #[test]
    fn password_is_never_drawn() {
        let ui = Ui {
            text_entry: true,
            password: "secretpass".into(),
            ..Default::default()
        };
        let draw = ui.draw(&AppModel::default(), &[], "ok", false, PowerView::default());
        let glyphs = draw
            .iter()
            .filter_map(|q| q.glyph.map(char::from))
            .collect::<String>();
        assert!(!glyphs.contains("secretpass"));
    }
    #[test]
    fn font_and_icon_assets_have_fixed_dimensions() {
        assert_eq!(font_atlas().len(), 256 * 128 * 4);
        assert_eq!(display_font_atlas().len(), 256 * 128 * 4);
        assert_eq!(icons_atlas().len(), 192 * 160 * 4);
    }
}
