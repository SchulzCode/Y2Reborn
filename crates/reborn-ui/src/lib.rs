#![forbid(unsafe_code)]

mod catalog;
mod components;
mod glyphs;
mod platform;
mod screens;
pub mod theme;

use reborn_core::{
    Action, AppModel, AudioOutput, ConfirmAction, MediaSource, Modal, NavigationFrame, RadioScan,
    RepeatMode, Screen, Track,
};
use reborn_graphics::Quad;
use std::time::{Duration, Instant};

pub use reborn_core::Effect;
pub use screens::PreviewScreen;

/// Count the semantic focus markers emitted by the presentation layer. The
/// marker is metadata only; the native renderer ignores it, while preview and
/// state tests use it to enforce the one-focus invariant.
pub fn focus_target_count(quads: &[Quad]) -> usize {
    quads.iter().filter(|quad| quad.focus_target).count()
}

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
        if let Some(error) = &self.error {
            return error.clone();
        }
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
            return format!("{name} unavailable");
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
    pub charging: bool,
    pub percent: Option<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct BluetoothDeviceView {
    pub path: String,
    pub name: String,
    pub paired: bool,
    pub bonded: bool,
    pub connected: bool,
    pub audio_ready: bool,
}

impl BluetoothDeviceView {
    fn saved_pairing(&self) -> bool {
        self.paired && self.bonded
    }
    fn usable_audio(&self) -> bool {
        self.saved_pairing() && self.connected && self.audio_ready
    }
    fn operation(&self) -> &'static str {
        if !self.saved_pairing() {
            "pair"
        } else if self.connected {
            "disconnect"
        } else {
            "connect"
        }
    }
    fn description(&self) -> &'static str {
        if !self.saved_pairing() {
            if self.connected { "Pairing required" } else { "Available" }
        } else if self.usable_audio() {
            "Audio ready"
        } else if self.connected {
            "Connected; audio unavailable"
        } else {
            "Paired"
        }
    }
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
    pub bluetooth_devices: Vec<BluetoothDeviceView>,
    pub pairing: Option<String>,
    pub pairing_focus: usize,
    pub collection_art: Option<(String, i64)>,
    password: String,
    ssid: String,
    letter: usize,
    pub text_entry: bool,
    catalog: std::cell::RefCell<catalog::Catalog>,
    letter_index: Vec<Item>,
}

const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 !@#$%^&*()-_=+[]{};:'\",.<>/?\\|`~";

impl Ui {
    /// Borrow the authoritative library for one synchronous semantic action.
    /// Restore it before the caller executes effects; never clone every Track
    /// merely to satisfy disjoint borrowing of navigation and the library.
    pub fn model_action(&mut self, model: &mut AppModel, action: Action) -> Effect {
        let tracks = std::mem::take(&mut model.library.tracks);
        let effect = self.action(model, &tracks, action);
        model.library.tracks = tracks;
        effect
    }

    pub fn normalize(&self, m: &mut AppModel, tracks: &[Track]) {
        let count = self.row_count(m, tracks);
        m.navigation.focus = m.navigation.focus.min(count.saturating_sub(1));
        if count > 0
            && !self
                .row_at(m, tracks, m.navigation.focus)
                .is_some_and(|r| r.enabled)
        {
            m.navigation.focus = (0..count)
                .find(|i| self.row_at(m, tracks, *i).is_some_and(|r| r.enabled))
                .unwrap_or(0);
        }
        if m.navigation.modal.is_some() {
            let rows = self.modal_rows(m, tracks);
            if rows.is_empty() {
                m.navigation.modal = None;
            } else {
                m.navigation.modal_focus = m.navigation.modal_focus.min(rows.len() - 1);
                if !rows[m.navigation.modal_focus].enabled {
                    m.navigation.modal_focus = rows.iter().position(|r| r.enabled).unwrap_or(0);
                }
            }
        }
    }
    pub fn flash(&mut self, message: impl Into<String>) {
        self.notice = message.into();
        self.notice_until = Some(Instant::now() + Duration::from_millis(1600));
    }

    pub fn expire_notice(&mut self) -> bool {
        if self
            .notice_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.notice.clear();
            self.notice_until = None;
            return true;
        }
        false
    }

    fn go(m: &mut AppModel, screen: Screen, filter: impl Into<String>) {
        let filter = filter.into();
        if m.screen != screen || m.navigation.filter != filter {
            if m.navigation.history.len() >= 64 {
                m.navigation.history.remove(0);
                m.navigation.stack.remove(0);
            }
            m.navigation.stack.push(m.screen);
            m.navigation.history.push(NavigationFrame {
                screen: m.screen,
                focus: m.navigation.focus,
                scroll: m.navigation.scroll,
                filter: m.navigation.filter.clone(),
            });
        }
        m.screen = screen;
        m.navigation.focus = 0;
        m.navigation.scroll = 0;
        m.navigation.filter = filter;
        m.navigation.modal = None;
        m.navigation.modal_focus = 0;
        m.navigation.context_target = None;
        m.navigation.context_key = None;
    }

    fn back(m: &mut AppModel) {
        if m.navigation.modal.is_some() {
            m.navigation.modal = None;
            m.navigation.modal_focus = 0;
            m.navigation.context_target = None;
            m.navigation.context_key = None;
            return;
        }
        if m.screen == Screen::TextEntry {
            return;
        }
        if let Some(previous) = m.navigation.history.pop() {
            m.navigation.stack.pop();
            m.screen = previous.screen;
            m.navigation.focus = previous.focus;
            m.navigation.scroll = previous.scroll;
            m.navigation.filter = previous.filter;
            return;
        }
        if let Some(previous) = m.navigation.stack.pop() {
            m.screen = previous;
            m.navigation.focus = 0;
            m.navigation.scroll = 0;
            m.navigation.filter.clear();
        } else if m.screen != Screen::Home {
            Self::home(m);
        }
    }

    fn home(m: &mut AppModel) {
        m.navigation.stack.clear();
        m.navigation.history.clear();
        m.screen = Screen::Home;
        m.navigation.focus = 0;
        m.navigation.scroll = 0;
        m.navigation.filter.clear();
        m.navigation.modal = None;
        m.navigation.context_target = None;
        m.navigation.context_key = None;
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
            Screen::Artists
            | Screen::Albums
            | Screen::Folders
            | Screen::Artist
            | Screen::Album
            | Screen::Tracks => self.catalog_rows(m, tracks),
            Screen::LibraryIndex => {
                if self.letter_index.is_empty() {
                    vec![Item::new("Back", "back")
                        .with_secondary("No letter index for this collection")]
                } else {
                    self.letter_index.clone()
                }
            }
            Screen::Queue => self.queue_rows(m),
            Screen::Platform => platform::rows(m, &m.navigation.filter),
            Screen::QuickSettings => vec![
                Item::new("Wi-Fi", "wifi").with_secondary(self.wifi_summary()),
                Item::new("Bluetooth", "bluetooth").with_secondary(self.bluetooth_summary()),
                Item::new("Audio Output", "output")
                    .with_secondary(components::output_label(&m.output)),
                Item::new("Display", "display")
                    .with_secondary(timeout_label(m.settings.screen_timeout_seconds)),
                Item::new("System Settings", "settings"),
            ],
            Screen::TrackInfo => {
                let track = m
                    .navigation
                    .filter
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| tracks.get(i))
                    .or_else(|| m.current());
                if let Some(t) = track {
                    [
                        ("Title", t.title.clone()),
                        ("Artist", display_or_unknown(&t.artist).into()),
                        ("Album", display_or_unknown(&t.album).into()),
                        ("File", t.filename.clone()),
                        ("Source codec", t.codec.clone()),
                        ("Source rate", format!("{} Hz", t.sample_rate)),
                        ("Duration", components::time(t.duration_ms)),
                        ("Source", t.source_id.clone()),
                    ]
                    .into_iter()
                    .map(|(name, value)| {
                        Item::new(name, format!("value:{name}\u{1f}{value}")).with_secondary(value)
                    })
                    .collect()
                } else {
                    vec![Item::new("Back", "back").with_secondary("Track unavailable")]
                }
            }
            Screen::Connectivity => vec![
                Item::new("Wi-Fi", "wifi").with_secondary(self.wifi_summary()),
                Item::new("Bluetooth", "bluetooth").with_secondary(self.bluetooth_summary()),
                Item::new("PC Transfer", "page:usb").with_secondary("Authenticated USB SFTP"),
                Item::new("Quick Settings", "quick_settings"),
            ],
            Screen::Bluetooth | Screen::SettingsBluetooth => self.bluetooth_rows(),
            Screen::Wifi | Screen::SettingsWifi => self.wifi_rows(),
            Screen::Settings => vec![
                Item::new("Audio", "audio").with_secondary("Audio controls"),
                Item::new("Playback", "playback").with_secondary("Gapless + repeat"),
                Item::new("Library", "library").with_secondary("Storage + scan"),
                Item::new("Connectivity", "connectivity").with_secondary("Wi-Fi · Bluetooth · USB"),
                Item::new("Storage", "page:storage").with_secondary("Internal storage and SD"),
                Item::new("Display", "display").with_secondary("Screen timeout"),
                Item::new("Power", "power").with_secondary("Power options"),
                Item::new("System", "system").with_secondary("About + health"),
            ],
            Screen::SettingsAudio => vec![
                Item::new("Output", "output").with_secondary(components::output_label(&m.output)),
                Item::new("ReplayGain", "replay_gain")
                    .with_secondary(replay_gain_label(m.settings.replay_gain)),
                Item::new("Audio Info", "audio_info").with_secondary("Codec + output"),
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
            Screen::SettingsLibrary => {
                let state = if m.library.scanning {
                    "Scanning your music. Existing entries remain available.".into()
                } else if m.library.error.is_some() {
                    "Scan interrupted. Existing entries are kept. Check for missing or damaged files, then rescan.".into()
                } else if let Some(scan) = &m.library.last_scan {
                    format!(
                        "Complete: {} files, {} reused.",
                        scan.discovered, scan.reused
                    )
                } else {
                    "Not scanned yet".into()
                };
                vec![
                    Item::new("Internal Storage", "internal").with_secondary("Music library"),
                    Item::new("SD Card", "sd").with_secondary(
                        if m.sources.iter().any(|source| {
                            matches!(source.kind, MediaSource::SdCard(_)) && source.online
                        }) {
                            "Available"
                        } else {
                            "Not inserted"
                        },
                    ),
                    Item::new("Scan Library", "scan_library"),
                    Item::new("Rebuild Library", "rebuild_library"),
                    Item::new("Scan Status", format!("value:Library scan\u{1f}{state}"))
                        .with_secondary(state),
                ]
            }
            Screen::SettingsDisplay => vec![Item::new("Screen Timeout", "timeout")
                .with_secondary(timeout_label(m.settings.screen_timeout_seconds))],
            Screen::SettingsPower => vec![
                Item::new("Battery & Source", "page:power"),
                Item::new("Power Menu", "power_menu"),
            ],
            Screen::SettingsSystem => vec![
                Item::new("About Reborn", "page:about").with_secondary(reborn_core::VERSION),
                Item::new("Software Update", "page:update")
                    .with_secondary("Signed system root updates"),
                Item::new("Backup & Export", "page:backup"),
                Item::new("Reset & Maintenance", "page:maintenance"),
                Item::new("Date & Time", "page:clock"),
                Item::new("Diagnostics", "diagnostics")
                    .with_secondary("Platform health and observations"),
            ],
            Screen::Diagnostics => platform::rows(m, &m.navigation.filter),
            Screen::TextEntry | Screen::Pairing => vec![],
            Screen::NowPlaying if m.current().is_none() => vec![Item::new("Open Music", "music")],
            Screen::NowPlaying => vec![
                Item::new("Track Options", "now_options"),
                Item::new("Queue", "queue"),
                Item::new("Audio Information", "page:audio"),
            ],
        }
    }

    pub fn collection_track<'a>(&self, m: &AppModel, tracks: &'a [Track]) -> Option<&'a Track> {
        let mut catalog = self.catalog.borrow_mut();
        catalog.ensure(m, tracks);
        catalog.rows.iter().find_map(|row| match row {
            catalog::Row::Track(index) => tracks.get(*index),
            _ => None,
        })
    }
    pub fn invalidate_catalog(&self) {
        self.catalog.borrow_mut().clear();
    }
    fn catalog_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        let mut c = self.catalog.borrow_mut();
        c.ensure(m, tracks);
        c.rows.iter().map(|r| r.item(tracks)).collect()
    }
    pub(crate) fn track_rows_page(
        &self,
        m: &AppModel,
        tracks: &[Track],
        offset: usize,
        limit: usize,
    ) -> Vec<Item> {
        let mut c = self.catalog.borrow_mut();
        c.ensure(m, tracks);
        c.rows
            .iter()
            .skip(offset)
            .take(limit)
            .map(|r| r.item(tracks))
            .collect()
    }
    pub(crate) fn row_count(&self, m: &AppModel, tracks: &[Track]) -> usize {
        if Self::is_catalog(m.screen) {
            let mut c = self.catalog.borrow_mut();
            c.ensure(m, tracks);
            c.rows.len()
        } else if m.screen == Screen::Queue {
            m.queue.len()
        } else {
            self.rows(m, tracks).len()
        }
    }
    fn is_catalog(s: Screen) -> bool {
        matches!(
            s,
            Screen::Albums
                | Screen::Artists
                | Screen::Folders
                | Screen::Album
                | Screen::Artist
                | Screen::Tracks
        )
    }
    pub(crate) fn row_at(&self, m: &AppModel, tracks: &[Track], index: usize) -> Option<Item> {
        if Self::is_catalog(m.screen) {
            self.track_rows_page(m, tracks, index, 1).pop()
        } else if m.screen == Screen::Queue {
            m.queue.get(index).map(|t| {
                Item::new(
                    if t.title.is_empty() {
                        &t.filename
                    } else {
                        &t.title
                    },
                    format!(
                        "queue:{}",
                        m.queue_entry_ids
                            .get(index)
                            .map(|v| v.0)
                            .unwrap_or(index as u64)
                    ),
                )
                .with_secondary(format!(
                    "{} · {}",
                    if index == m.queue_position {
                        "Now Playing"
                    } else if index > m.queue_position {
                        "Up Next"
                    } else {
                        "Played"
                    },
                    display_or_unknown(&t.artist)
                ))
            })
        } else {
            self.rows(m, tracks).get(index).cloned()
        }
    }
    pub(crate) fn visible_rows(
        &self,
        m: &AppModel,
        tracks: &[Track],
        offset: usize,
        count: usize,
    ) -> Vec<Item> {
        (offset..offset + count)
            .filter_map(|i| self.row_at(m, tracks, i))
            .collect()
    }
    fn queue_rows(&self, m: &AppModel) -> Vec<Item> {
        (0..m.queue.len())
            .filter_map(|i| self.row_at(m, &[], i))
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
        rows[0].enabled = self.bluetooth.available;
        rows[1].enabled =
            self.bluetooth.available && self.bluetooth.powered && !self.bluetooth.scan.active();
        rows.extend(self.bluetooth_devices.iter().map(|device| {
            Item::new(&device.name, format!("bt_device:{}", device.path))
                .with_secondary(device.description())
        }));
        rows.push(Item::new("Codec & Audio Details", "page:bluetooth"));
        if let Some(error) = &self.bluetooth.error {
            rows.push(
                Item::new(
                    "Connection Problem",
                    format!("value:Bluetooth error\u{1f}{error}"),
                )
                .with_secondary(error),
            );
        }
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
        rows[0].enabled = self.wifi.available;
        rows[1].enabled = self.wifi.available && self.wifi.powered && !self.wifi.scan.active();
        rows.extend(self.saved_networks.iter().cloned());
        rows.extend(self.networks.iter().cloned());
        let mut disconnect = Item::new("Disconnect", "wifi_disconnect");
        disconnect.enabled = self.wifi.available && self.wifi.powered;
        rows.push(disconnect);
        rows.push(Item::new("Network Details", "page:network"));
        if let Some(error) = &self.wifi.error {
            rows.push(
                Item::new(
                    "Connection Problem",
                    format!("value:Wi-Fi error\u{1f}{error}"),
                )
                .with_secondary(error),
            );
        }
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

    fn context_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        if m.screen == Screen::NowPlaying && m.current().is_none() {
            return vec![];
        }
        let selected_key = m.navigation.context_key.clone().or_else(|| {
            m.navigation
                .context_target
                .and_then(|target| self.row_at(m, tracks, target).map(|item| item.key))
        });
        if selected_key
            .as_deref()
            .is_some_and(|key| key.starts_with("track:"))
        {
            let mut actions = vec![
                Item::new("Play", "play"),
                Item::new("Play Next", "play_next"),
                Item::new("Add to Queue", "add_queue"),
                Item::new("Go to Album", "go_album"),
                Item::new("Go to Artist", "go_artist"),
                Item::new("Track Information", "track_info"),
            ];
            if m.screen == Screen::Tracks && m.navigation.filter.is_empty() {
                actions.push(Item::new("Jump to Letter", "letter_index"));
            }
            return actions;
        }
        if m.screen == Screen::Artist && selected_key.as_deref() == Some("play:collection") {
            return vec![
                Item::new("Play Artist", "artist_play"),
                Item::new("Shuffle Artist", "artist_shuffle"),
                Item::new("Play Next", "artist_next"),
                Item::new("Add to Queue", "artist_queue"),
            ];
        }
        match m.screen {
            Screen::Albums | Screen::Artists => vec![Item::new("Jump to Letter", "letter_index")],
            Screen::Album => vec![
                Item::new("Play Album", "album_play"),
                Item::new("Shuffle Album", "album_shuffle"),
                Item::new("Play Next", "album_next"),
                Item::new("Add to Queue", "album_queue"),
                Item::new("Audio Information", "audio_info"),
            ],
            Screen::Tracks | Screen::NowPlaying => vec![
                Item::new("Play", "play"),
                Item::new("Play Next", "play_next"),
                Item::new("Add to Queue", "add_queue"),
                Item::new("Go to Album", "go_album"),
                Item::new("Go to Artist", "go_artist"),
                Item::new("Track Information", "track_info"),
            ],
            Screen::Queue => {
                let target = selected_key
                    .as_deref()
                    .and_then(|s| s.strip_prefix("queue:"))
                    .and_then(|s| s.parse::<u64>().ok())
                    .and_then(|id| {
                        m.queue_entry_ids
                            .iter()
                            .position(|v| v.0 == id)
                            .or_else(|| m.queue_entry_ids.is_empty().then_some(id as usize))
                    })
                    .filter(|i| *i < m.queue.len());
                let Some(i) = target else {
                    return vec![];
                };
                let mut rows = vec![
                    Item::new("Play Now", "queue_play"),
                    Item::new("Remove from Queue", "queue_remove"),
                    Item::new("Move Up", "queue_up"),
                    Item::new("Move Down", "queue_down"),
                    Item::new("Clear Future", "clear_queue"),
                ];
                rows[1].enabled = i != m.queue_position;
                rows[2].enabled = i > m.queue_position.saturating_add(1);
                rows[3].enabled = i > m.queue_position && i + 1 < m.queue.len();
                rows[4].enabled = m.queue_position + 1 < m.queue.len();
                rows
            }
            Screen::Bluetooth | Screen::SettingsBluetooth => {
                let path = selected_key
                    .as_deref()
                    .and_then(|key| key.strip_prefix("bt_device:"));
                if let Some(device) =
                    path.and_then(|p| self.bluetooth_devices.iter().find(|d| d.path == p))
                {
                    let mut rows = vec![
                        Item::new(
                            if !device.saved_pairing() {
                                "Pair & Connect"
                            } else if device.connected {
                                "Disconnect"
                            } else {
                                "Connect"
                            },
                            "bt_connect",
                        ),
                        Item::new("Use for Audio", "bt_output"),
                        Item::new("Forget Device", "bt_forget"),
                        Item::new("Codec & Audio Details", "bt_info"),
                    ];
                    rows[1].enabled = device.usable_audio();
                    rows
                } else {
                    vec![]
                }
            }
            Screen::Wifi | Screen::SettingsWifi => {
                if selected_key
                    .as_deref()
                    .is_some_and(|s| s.starts_with("saved:"))
                {
                    vec![
                        Item::new("Connect", "wifi_connect"),
                        Item::new("Forget Network", "wifi_forget"),
                        Item::new("Disconnect", "wifi_disconnect"),
                        Item::new("Network Details", "wifi_info"),
                    ]
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }

    fn modal_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        match m.navigation.modal {
            Some(Modal::ContextMenu) => self.context_rows(m, tracks),
            Some(Modal::PowerMenu) => vec![
                Item::new("Power Off", "power_off"),
                Item::new("Reboot", "reboot"),
                Item::new("Cancel", "cancel"),
            ],
            Some(Modal::Confirm(action)) => {
                let mut rows = vec![
                    Item::new("Cancel", "cancel"),
                    Item::new("Confirm", "confirm"),
                ];
                if let ConfirmAction::Platform(task) = action {
                    let (page, key) = match task {
                        reborn_core::PlatformTask::UpdateApply => {
                            ("update", "confirm:update_apply")
                        }
                        reborn_core::PlatformTask::UpdateCancel => {
                            ("update", "confirm:update_cancel")
                        }
                        reborn_core::PlatformTask::UpdateRollback => {
                            ("update", "confirm:update_rollback")
                        }
                        reborn_core::PlatformTask::StorageBenchmark => {
                            ("benchmarks", "confirm:storage_benchmark")
                        }
                        reborn_core::PlatformTask::LibraryBenchmark => {
                            ("benchmarks", "confirm:library_benchmark")
                        }
                        _ => ("", ""),
                    };
                    rows[1].enabled = platform::rows(m, page)
                        .iter()
                        .any(|row| row.key == key && row.enabled);
                }
                rows
            }
            None => vec![],
        }
    }

    pub(crate) fn modal_rows_public(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        self.modal_rows(m, tracks)
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
                ConfirmAction::Platform(task) => match task {
                    reborn_core::PlatformTask::UpdateApply => (
                        "Install & Restart?",
                        "Verified root only. Keep external power connected.",
                    ),
                    reborn_core::PlatformTask::UpdateRollback => (
                        "Restore previous root?",
                        "Restart into verified system recovery.",
                    ),
                    reborn_core::PlatformTask::UpdateCancel => (
                        "Cancel queued update?",
                        "The installed system will stay unchanged.",
                    ),
                    _ => (
                        "Run advanced check?",
                        "Private scratch writes. Your music is not used.",
                    ),
                },
            },
            Some(Modal::ContextMenu) => ("Track options", "Choose an action for the focused item."),
            None => ("", ""),
        }
    }

    pub fn action(&mut self, m: &mut AppModel, tracks: &[Track], action: Action) -> Effect {
        if !m.screen_off {
            self.normalize(m, tracks);
        }
        // Installing/restoring the root must not compete with navigation,
        // shutdown or playback. The platform owns the resulting restart.
        if matches!(
            m.platform.busy,
            Some(
                reborn_core::PlatformTask::UpdateApply | reborn_core::PlatformTask::UpdateRollback
            )
        ) {
            return match action {
                Action::ScreenWake => Effect::ScreenWake,
                Action::VolumeUp => Effect::AdjustVolume(2),
                Action::VolumeDown => Effect::AdjustVolume(-2),
                _ => Effect::None,
            };
        }
        match action {
            Action::ScreenSleep => return Effect::ScreenSleep,
            Action::ScreenWake => return Effect::ScreenWake,
            Action::PlayPause => return Effect::TogglePlayback,
            Action::ShowNowPlaying
                if !m.screen_off
                    && m.navigation.modal.is_none()
                    && !self.text_entry
                    && self.pairing.is_none() =>
            {
                Self::go(m, Screen::NowPlaying, "");
                return Effect::None;
            }
            Action::PreviousTrack => return Effect::PreviousTrack,
            Action::NextTrack => return Effect::NextTrack,
            Action::SeekBackward => return Effect::Seek(-30_000),
            Action::SeekForward => return Effect::Seek(30_000),
            Action::VolumeUp => return Effect::AdjustVolume(2),
            Action::VolumeDown => return Effect::AdjustVolume(-2),
            _ => {}
        }
        if m.screen_off {
            return Effect::None;
        }
        if self.pairing.is_some() {
            return match action {
                Action::Select => {
                    self.pairing = None;
                    Effect::ConfirmPairing(self.pairing_focus == 0)
                }
                Action::WheelClockwise(_) | Action::NavigateDown => {
                    self.pairing_focus = 1;
                    Effect::None
                }
                Action::WheelCounterClockwise(_) | Action::NavigateUp => {
                    self.pairing_focus = 0;
                    Effect::None
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
            Action::Home => {
                if m.navigation.modal.is_some() {
                    Self::back(m);
                    return Effect::None;
                }
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
                if m.navigation.modal.is_none()
                    && self
                        .row_at(m, tracks, m.navigation.focus)
                        .is_some_and(|row| row.enabled)
                {
                    m.navigation.context_target = Some(m.navigation.focus);
                    m.navigation.context_key =
                        self.row_at(m, tracks, m.navigation.focus).map(|r| r.key);
                    if !self.context_rows(m, tracks).is_empty() {
                        m.navigation.modal = Some(Modal::ContextMenu);
                        m.navigation.modal_focus = 0;
                    } else {
                        m.navigation.context_target = None;
                        m.navigation.context_key = None;
                    }
                }
                return Effect::None;
            }
            Action::WheelClockwise(steps) => {
                self.move_focus(m, tracks, steps as i32);
                return Effect::None;
            }
            Action::WheelCounterClockwise(steps) => {
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
            _ => return Effect::None,
        }
        if m.navigation.modal.is_some() {
            return self.modal_action(m, tracks);
        }
        if self.row_count(m, tracks) == 0
            || (m.screen == Screen::NowPlaying && m.current().is_none())
        {
            if m.screen == Screen::Queue || m.screen == Screen::NowPlaying {
                Self::go(m, Screen::Music, "");
                return Effect::None;
            }
            return Effect::ScanLibrary;
        }
        let Some(row) = self
            .row_at(m, tracks, m.navigation.focus)
            .filter(|row| row.enabled)
        else {
            return Effect::None;
        };
        self.select_row(m, tracks, &row.key.clone())
    }

    fn move_focus(&mut self, m: &mut AppModel, tracks: &[Track], delta: i32) {
        if delta == 0 {
            return;
        }
        let modal = m.navigation.modal.is_some();
        let rows = if modal {
            Some(self.modal_rows(m, tracks))
        } else if !Self::is_catalog(m.screen) && m.screen != Screen::Queue {
            Some(self.rows(m, tracks))
        } else {
            None
        };
        let count = rows
            .as_ref()
            .map(|r| r.len())
            .unwrap_or_else(|| self.row_count(m, tracks));
        if count == 0 {
            return;
        }
        let focus = if modal {
            &mut m.navigation.modal_focus
        } else {
            &mut m.navigation.focus
        };
        let mut next = (*focus as i32 + delta).clamp(0, count as i32 - 1) as usize;
        if let Some(rows) = rows {
            let step = delta.signum();
            while !rows[next].enabled {
                let n = next as i32 + step;
                if n < 0 || n >= count as i32 {
                    return;
                }
                next = n as usize;
            }
        }
        *focus = next;
        if !modal {
            let visible = if matches!(m.screen, Screen::Album | Screen::Artist) {
                3
            } else {
                5
            };
            if next < m.navigation.scroll {
                m.navigation.scroll = next;
            } else if next >= m.navigation.scroll + visible {
                m.navigation.scroll = next + 1 - visible;
            }
        }
    }

    fn select_row(&mut self, m: &mut AppModel, tracks: &[Track], key: &str) -> Effect {
        if let Some(letter) = key.strip_prefix("jump_to:").and_then(|s| s.chars().next()) {
            Self::back(m);
            let mut catalog = self.catalog.borrow_mut();
            catalog.ensure(m, tracks);
            let target = catalog.rows.iter().position(|row| {
                let label = match row {
                    catalog::Row::Track(i) => {
                        let t = &tracks[*i];
                        if t.title.is_empty() {
                            &t.filename
                        } else {
                            &t.title
                        }
                    }
                    catalog::Row::Item(i) => &i.label,
                };
                label
                    .chars()
                    .next()
                    .filter(char::is_ascii_alphabetic)
                    .map(|c| c.to_ascii_uppercase())
                    .unwrap_or('#')
                    == letter
            });
            drop(catalog);
            if let Some(index) = target {
                m.navigation.focus = index;
                m.navigation.scroll = index.saturating_sub(2);
            } else {
                self.flash("This section is no longer in the library");
            }
            return Effect::None;
        }
        if key == "wifi_disconnect" {
            return Effect::WifiDisconnect;
        }
        if key == "stop_playback" {
            return Effect::StopPlayback;
        }
        if key == "codec_sbc" {
            return Effect::SetCodecPreference(reborn_core::CodecPreference::Sbc);
        }
        if key == "codec_auto" {
            return Effect::SetCodecPreference(reborn_core::CodecPreference::Auto);
        }
        if key == "back" {
            Self::back(m);
            return Effect::None;
        }
        if key == "quick_settings" {
            Self::go(m, Screen::QuickSettings, "");
            return Effect::None;
        }
        if let Some(page) = key
            .strip_prefix("page:")
            .or_else(|| key.strip_prefix("value:").map(|_| key))
        {
            Self::go(m, Screen::Platform, page);
            return Effect::None;
        }
        if let Some(name) = key
            .strip_prefix("task:")
            .or_else(|| key.strip_prefix("confirm:"))
        {
            use reborn_core::PlatformTask::*;
            let task = match name {
                "health" => Health,
                "update_check" => UpdateCheck,
                "update_stage" => UpdateStage,
                "update_apply" => UpdateApply,
                "update_cancel" => UpdateCancel,
                "update_rollback" => UpdateRollback,
                "export" => Export,
                "storage_benchmark" => StorageBenchmark,
                "library_benchmark" => LibraryBenchmark,
                "network" => NetworkCheck,
                _ => Refresh,
            };
            if key.starts_with("confirm:") {
                m.navigation.modal = Some(Modal::Confirm(ConfirmAction::Platform(task)));
                m.navigation.modal_focus = 0;
                return Effect::None;
            }
            return Effect::Platform(task);
        }
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
            Screen::Folders => {
                if key.starts_with("track:") {
                    return self.play_track(m, tracks, key);
                }
                Self::go(m, Screen::Folders, key)
            }
            Screen::Artist | Screen::Album | Screen::Tracks => {
                if key == "artist_albums" {
                    Self::go(m, Screen::Albums, m.navigation.filter.clone());
                    return Effect::None;
                }
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
                    let members = self.filtered_indices(m, tracks);
                    return if members.is_empty() {
                        Effect::None
                    } else {
                        Effect::PlayNextCollection(members)
                    };
                }
                if key == "add:collection" {
                    let members = self.filtered_indices(m, tracks);
                    return if members.is_empty() {
                        Effect::None
                    } else {
                        Effect::AddToQueueCollection(members)
                    };
                }
            }
            Screen::Queue => {
                if let Some(index) = key
                    .strip_prefix("queue:")
                    .and_then(|value| value.parse::<usize>().ok())
                {
                    let index = m
                        .queue_entry_ids
                        .iter()
                        .position(|id| id.0 == index as u64)
                        .or_else(|| m.queue_entry_ids.is_empty().then_some(index));
                    if let Some(index) = index.filter(|i| *i < m.queue.len()) {
                        Self::go(m, Screen::NowPlaying, "");
                        return Effect::PlayQueue(index);
                    }
                }
            }
            Screen::Connectivity | Screen::QuickSettings => match key {
                "bluetooth" => Self::go(m, Screen::Bluetooth, ""),
                "wifi" => Self::go(m, Screen::Wifi, ""),
                "settings" => Self::go(m, Screen::Settings, ""),
                "display" => Self::go(m, Screen::SettingsDisplay, ""),
                "output" => Self::go(m, Screen::SettingsAudio, ""),
                _ => {}
            },
            Screen::Bluetooth | Screen::SettingsBluetooth => {
                if key.starts_with("bt_device:") {
                    m.navigation.context_target = Some(m.navigation.focus);
                    m.navigation.context_key = Some(key.into());
                    m.navigation.modal = Some(Modal::ContextMenu);
                    m.navigation.modal_focus = 0;
                    return Effect::None;
                }
                return self.bluetooth_action(key);
            }
            Screen::Wifi | Screen::SettingsWifi => return self.wifi_action(key),
            Screen::Settings => match key {
                "connectivity" => Self::go(m, Screen::Connectivity, ""),
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
                        .find(|device| device.usable_audio())
                        .map(|device| device.path.clone())
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
                "internal" | "sd" => Self::go(m, Screen::Platform, "storage"),
                "scan_library" => return Effect::ScanLibrary,
                "rebuild_library" => {
                    Self::go(m, Screen::Platform, "reset_library");
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
            Screen::Diagnostics => {
                if key == "back_diagnostics" {
                    m.navigation.filter.clear();
                    m.navigation.focus = 0;
                }
            }
            Screen::NowPlaying => match key {
                "now_options" => {
                    m.navigation.context_target = Some(0);
                    m.navigation.modal = Some(Modal::ContextMenu);
                    m.navigation.modal_focus = 0;
                }
                "queue" => Self::go(m, Screen::Queue, ""),
                "previous" => return Effect::PreviousTrack,
                "toggle" => return Effect::TogglePlayback,
                "next" => return Effect::NextTrack,
                _ => {}
            },
            Screen::TextEntry
            | Screen::Pairing
            | Screen::Platform
            | Screen::TrackInfo
            | Screen::LibraryIndex => {}
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
        let effect = self.play_effect(m, tracks, index, false);
        Self::go(m, Screen::NowPlaying, "");
        effect
    }

    fn play_first_filtered(&mut self, m: &mut AppModel, tracks: &[Track]) -> Effect {
        let members = self.filtered_indices(m, tracks);
        let Some(&index) = members.first() else {
            return Effect::None;
        };
        let effect = Effect::PlayCollection {
            selected: index,
            members,
            shuffle: false,
        };
        Self::go(m, Screen::NowPlaying, "");
        effect
    }
    fn play_first_filtered_shuffled(&mut self, m: &mut AppModel, tracks: &[Track]) -> Effect {
        let members = self.filtered_indices(m, tracks);
        let Some(&index) = members.first() else {
            return Effect::None;
        };
        let effect = Effect::PlayCollection {
            selected: index,
            members,
            shuffle: true,
        };
        Self::go(m, Screen::NowPlaying, "");
        effect
    }
    fn filtered_indices(&self, m: &AppModel, tracks: &[Track]) -> Vec<usize> {
        tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track_matches(track, &m.navigation.filter))
            .map(|(index, _)| index)
            .collect()
    }
    fn play_effect(
        &self,
        m: &AppModel,
        tracks: &[Track],
        selected: usize,
        shuffle: bool,
    ) -> Effect {
        let members = self.filtered_indices(m, tracks);
        if members.len() > 1 || !m.navigation.filter.is_empty() {
            Effect::PlayCollection {
                selected,
                members,
                shuffle,
            }
        } else if shuffle {
            Effect::PlayShuffled(selected)
        } else {
            Effect::Play(selected)
        }
    }

    fn bluetooth_action(&mut self, key: &str) -> Effect {
        match key {
            "bt_power" => Effect::BluetoothPower,
            "bt_scan" => Effect::BluetoothScan,
            "wired_output" => Effect::Output(AudioOutput::Wired),
            key if key.starts_with("bt_device:") => {
                let path = key.trim_start_matches("bt_device:").to_owned();
                let device = self
                    .bluetooth_devices
                    .iter()
                    .find(|device| device.path == path);
                device.map(|device| Effect::BluetoothDevice {
                    path,
                    operation: device.operation().into(),
                }).unwrap_or(Effect::None)
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
                if self
                    .networks
                    .iter()
                    .any(|n| n.key == key && n.secondary.contains("Open"))
                {
                    return Effect::WifiConnect {
                        ssid: key.into(),
                        password: String::new(),
                    };
                }
                self.ssid = key.into();
                self.password.clear();
                self.letter = 0;
                self.text_entry = true;
                Effect::None
            }
        }
    }

    fn modal_action(&mut self, m: &mut AppModel, tracks: &[Track]) -> Effect {
        let rows = self.modal_rows(m, tracks);
        let Some(row) = rows.get(m.navigation.modal_focus).filter(|r| r.enabled) else {
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
                    m.navigation.context_key = None;
                    return Effect::None;
                }
                let effect = match confirm {
                    ConfirmAction::ForgetBluetooth => m
                        .navigation
                        .context_key
                        .as_deref()
                        .and_then(|s| s.strip_prefix("bt_device:"))
                        .filter(|path| self.bluetooth_devices.iter().any(|d| d.path == *path))
                        .map(|path| Effect::BluetoothDevice {
                            path: path.into(),
                            operation: "forget".into(),
                        })
                        .unwrap_or(Effect::None),
                    ConfirmAction::ForgetWifi => m
                        .navigation
                        .context_key
                        .as_deref()
                        .and_then(|s| s.strip_prefix("saved:"))
                        .and_then(|s| s.parse().ok())
                        .map(Effect::WifiForget)
                        .unwrap_or(Effect::None),
                    ConfirmAction::ClearQueue => Effect::ClearQueue,
                    ConfirmAction::RebuildLibrary => Effect::RebuildLibrary,
                    ConfirmAction::PowerOff => Effect::PowerOff,
                    ConfirmAction::Reboot => Effect::Reboot,
                    ConfirmAction::Platform(task) => Effect::Platform(task),
                };
                m.navigation.modal = None;
                m.navigation.context_target = None;
                m.navigation.context_key = None;
                effect
            }
            Some(Modal::ContextMenu) => {
                let Some(target) = m.navigation.context_target else {
                    m.navigation.modal = None;
                    return Effect::None;
                };
                let source_key = m
                    .navigation
                    .context_key
                    .clone()
                    .or_else(|| self.row_at(m, tracks, target).map(|r| r.key))
                    .unwrap_or_default();
                let queue_target = source_key
                    .strip_prefix("queue:")
                    .and_then(|s| s.parse::<u64>().ok())
                    .and_then(|id| {
                        m.queue_entry_ids
                            .iter()
                            .position(|entry| entry.0 == id)
                            .or_else(|| m.queue_entry_ids.is_empty().then_some(target))
                    });
                let track_index = if m.screen == Screen::NowPlaying {
                    m.current()
                        .and_then(|current| tracks.iter().position(|track| track.id == current.id))
                } else {
                    source_key
                        .strip_prefix("track:")
                        .and_then(|value| value.parse().ok())
                };
                let menu_key = row.key.clone();
                if menu_key == "bt_forget" {
                    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::ForgetBluetooth));
                    m.navigation.modal_focus = 0;
                    return Effect::None;
                }
                if menu_key == "wifi_forget" {
                    m.navigation.modal = Some(Modal::Confirm(ConfirmAction::ForgetWifi));
                    m.navigation.modal_focus = 0;
                    return Effect::None;
                }
                m.navigation.modal = None;
                m.navigation.context_target = None;
                m.navigation.context_key = None;
                match menu_key.as_str() {
                    "letter_index" => {
                        let mut catalog = self.catalog.borrow_mut();
                        catalog.ensure(m, tracks);
                        let mut letters = std::collections::BTreeMap::new();
                        for (i, row) in catalog.rows.iter().enumerate() {
                            let label = match row {
                                catalog::Row::Track(index) => {
                                    let t = &tracks[*index];
                                    if t.title.is_empty() {
                                        &t.filename
                                    } else {
                                        &t.title
                                    }
                                }
                                catalog::Row::Item(item) => &item.label,
                            };
                            let letter = label
                                .chars()
                                .next()
                                .filter(char::is_ascii_alphabetic)
                                .map(|c| c.to_ascii_uppercase())
                                .unwrap_or('#');
                            letters.entry(letter).or_insert(i);
                        }
                        self.letter_index = letters
                            .into_keys()
                            .map(|letter| {
                                Item::new(letter.to_string(), format!("jump_to:{letter}"))
                                    .with_secondary("Jump to the first matching item")
                            })
                            .collect();
                        drop(catalog);
                        Self::go(m, Screen::LibraryIndex, "");
                        Effect::None
                    }
                    "bt_connect" | "bt_output" => source_key
                        .strip_prefix("bt_device:")
                        .and_then(|path| self.bluetooth_devices.iter().find(|d| d.path == path))
                        .filter(|device| menu_key != "bt_output" || device.usable_audio())
                        .map(|device| Effect::BluetoothDevice {
                            path: device.path.clone(),
                            operation: if menu_key == "bt_output" {
                                "output"
                            } else {
                                device.operation()
                            }
                            .into(),
                        })
                        .unwrap_or(Effect::None),
                    "bt_info" => {
                        Self::go(m, Screen::Platform, "bluetooth");
                        Effect::None
                    }
                    "wifi_info" => {
                        Self::go(m, Screen::Platform, "network");
                        Effect::None
                    }
                    "wifi_connect" => source_key
                        .strip_prefix("saved:")
                        .and_then(|s| s.parse().ok())
                        .map(Effect::WifiSaved)
                        .unwrap_or(Effect::None),
                    "wifi_disconnect" => Effect::WifiDisconnect,
                    "play" => track_index.map(Effect::Play).unwrap_or(Effect::None),
                    "play_next" => track_index.map(Effect::PlayNext).unwrap_or(Effect::None),
                    "add_queue" => track_index.map(Effect::AddToQueue).unwrap_or(Effect::None),
                    "album_play" => self.play_first_filtered(m, tracks),
                    "album_shuffle" => self.play_first_filtered_shuffled(m, tracks),
                    "album_next" | "artist_next" => {
                        let members = self.filtered_indices(m, tracks);
                        if members.is_empty() {
                            Effect::None
                        } else {
                            Effect::PlayNextCollection(members)
                        }
                    }
                    "album_queue" | "artist_queue" => {
                        let members = self.filtered_indices(m, tracks);
                        if members.is_empty() {
                            Effect::None
                        } else {
                            Effect::AddToQueueCollection(members)
                        }
                    }
                    "artist_play" => self.play_first_filtered(m, tracks),
                    "artist_shuffle" => self.play_first_filtered_shuffled(m, tracks),
                    "go_album" => {
                        if let Some(index) = track_index.and_then(|i| tracks.get(i)) {
                            Self::go(m, Screen::Album, album_filter(index));
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
                        Self::go(
                            m,
                            Screen::TrackInfo,
                            track_index.map(|i| i.to_string()).unwrap_or_default(),
                        );
                        Effect::None
                    }
                    "audio_info" => {
                        Self::go(m, Screen::Platform, "audio");
                        Effect::None
                    }
                    "queue_play" => queue_target.map(Effect::PlayQueue).unwrap_or(Effect::None),
                    "queue_remove" => queue_target
                        .map(Effect::QueueRemove)
                        .unwrap_or(Effect::None),
                    "queue_up" => Effect::QueueMove {
                        index: queue_target.unwrap_or(usize::MAX),
                        delta: -1,
                    },
                    "queue_down" => Effect::QueueMove {
                        index: queue_target.unwrap_or(usize::MAX),
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
        let count = LETTERS.len() + 3;
        match action {
            Action::WheelClockwise(n) => self.letter = (self.letter + usize::from(n)) % count,
            Action::WheelCounterClockwise(n) => {
                self.letter = (self.letter + count - usize::from(n) % count) % count
            }
            Action::NavigateUp => self.letter = (self.letter + count - 1) % count,
            Action::NavigateDown => self.letter = (self.letter + 1) % count,
            Action::Select if self.letter < LETTERS.len() => {
                if self.password.len() < 63 {
                    self.password.push(LETTERS[self.letter] as char);
                }
            }
            Action::Select if self.letter == LETTERS.len() => {
                self.password.pop();
            }
            Action::ContextMenu | Action::Select
                if action == Action::ContextMenu || self.letter == LETTERS.len() + 1 =>
            {
                if self.password.len() >= 8 {
                    self.text_entry = false;
                    return Effect::WifiConnect {
                        ssid: self.ssid.clone(),
                        password: std::mem::take(&mut self.password),
                    };
                }
                self.flash("Use at least 8 characters for a secured network");
            }
            Action::Back | Action::Home | Action::Select => {
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

pub fn font_atlas() -> &'static [u8] {
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/fonts/reborn-ui.rgba"
    ))
}
pub fn display_font_atlas() -> &'static [u8] {
    font_atlas()
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
    if !track.online {
        return false;
    }
    if let Some(value) = filter.strip_prefix("artist:") {
        return track.artist == value;
    }
    if let Some(value) = filter.strip_prefix("album:") {
        if let Some((artist, album)) = value.split_once('\u{1f}') {
            return track.album_artist == artist && track.album == album;
        }
        return track.album == value;
    }
    if let Some(value) = filter.strip_prefix("folder:") {
        return track
            .path
            .parent()
            .is_some_and(|parent| parent.starts_with(value));
    }
    true
}
fn album_filter(track: &Track) -> String {
    format!("album:{}\u{1f}{}", track.album_artist, track.album)
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
pub(crate) fn timeout_label(seconds: u32) -> String {
    match seconds {
        0 => "Never",
        15 => "15 sec",
        30 => "30 sec",
        60 => "1 min",
        120 => "2 min",
        value => return format!("{value} sec"),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn model(screen: Screen) -> AppModel {
        AppModel {
            screen,
            navigation: reborn_core::NavigationState::default(),
            ..Default::default()
        }
    }

    fn preview_track() -> Track {
        Track {
            id: 1,
            path: PathBuf::from("/data/music/example.flac"),
            filename: "example.flac".into(),
            title: "A Brighter Silence".into(),
            artist: "Northark".into(),
            album: "Echoes of a Higher Place".into(),
            codec: "FLAC".into(),
            sample_rate: 96_000,
            channels: 2,
            duration_ms: 318_000,
            online: true,
            ..Default::default()
        }
    }
    #[test]
    fn wheel_on_now_playing_moves_focus_and_dedicated_volume_is_global() {
        let mut ui = Ui::default();
        let mut app = model(Screen::NowPlaying);
        app.queue = vec![preview_track()];
        assert_eq!(
            ui.action(&mut app, &[], Action::WheelClockwise(1)),
            Effect::None
        );
        assert_eq!(app.navigation.focus, 1);
        assert_eq!(
            ui.action(&mut app, &[], Action::VolumeUp),
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
    fn interactive_previews_have_one_visible_focus_target() {
        let track = preview_track();
        let mut next = preview_track();
        next.id = 2;
        next.title = "The Still Procession".into();
        let tracks = vec![track.clone(), next.clone()];
        let ui = Ui::default();
        let cases = [
            (PreviewScreen::NowPlaying, 1),
            (PreviewScreen::Library, 0),
            (PreviewScreen::Artist, 1),
            (PreviewScreen::Queue, 1),
            (PreviewScreen::Settings, 0),
            (PreviewScreen::QuickSettings, 0),
        ];
        for (screen, focus) in cases {
            let mut app = model(Screen::Home);
            app.queue = vec![track.clone(), next.clone()];
            app.queue_position = 0;
            app.playback = reborn_core::PlaybackState::Playing;
            app.navigation.focus = focus;
            let draw = ui.draw_preview(app, &tracks, PowerView::default(), screen);
            assert_eq!(focus_target_count(&draw), 1, "{screen:?}");
        }
    }

    #[test]
    fn settings_labels_use_physical_device_names() {
        let ui = Ui::default();
        let app = model(Screen::SettingsAudio);
        let labels = ui.rows(&app, &[]);
        assert!(!labels.iter().any(|row| row.label == "Equalizer"));
        assert!(labels.iter().any(|row| row.label == "Audio Info"));
        assert!(!labels.iter().any(|row| row.label.contains("/ Tone")));
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
            online: true,
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
    fn filtered_track_context_actions_stay_on_the_selected_track() {
        let other = Track {
            id: 0,
            path: PathBuf::from("/other/elsewhere.flac"),
            artist: "Other Artist".into(),
            album: "Other Album".into(),
            ..preview_track()
        };
        let first = Track {
            id: 1,
            path: PathBuf::from("/data/music/selected/first.flac"),
            artist: "Northark".into(),
            album: "Echoes".into(),
            ..preview_track()
        };
        let selected = Track {
            id: 2,
            path: PathBuf::from("/data/music/selected/selected.flac"),
            artist: "Northark".into(),
            album: "Echoes".into(),
            ..preview_track()
        };
        let tracks = vec![other, first, selected];
        let cases = [
            (Screen::Album, "album:Echoes", 2),
            (Screen::Artist, "artist:Northark", 3),
            (Screen::Tracks, "folder:/data/music/selected", 1),
        ];

        for (screen, filter, selected_row) in cases {
            for (menu_row, expected) in [
                (0, Effect::Play(2)),
                (1, Effect::PlayNext(2)),
                (2, Effect::AddToQueue(2)),
            ] {
                let mut ui = Ui::default();
                let mut app = model(screen);
                app.navigation.filter = filter.into();
                app.navigation.focus = selected_row;
                assert_eq!(
                    ui.action(&mut app, &tracks, Action::ContextMenu),
                    Effect::None
                );
                assert_eq!(app.navigation.modal, Some(Modal::ContextMenu));
                let labels = ui
                    .modal_rows_public(&app, &tracks)
                    .into_iter()
                    .map(|item| item.label)
                    .take(3)
                    .collect::<Vec<_>>();
                assert_eq!(labels, vec!["Play", "Play Next", "Add to Queue"]);
                app.navigation.modal_focus = menu_row;

                assert_eq!(ui.action(&mut app, &tracks, Action::Select), expected);
                assert_eq!(app.navigation.modal, None);
            }
        }
    }

    #[test]
    fn album_collection_actions_remain_collection_wide() {
        let tracks = vec![
            Track {
                id: 0,
                album: "Other Album".into(),
                ..preview_track()
            },
            Track {
                id: 1,
                album: "Echoes".into(),
                ..preview_track()
            },
            Track {
                id: 2,
                album: "Echoes".into(),
                ..preview_track()
            },
        ];
        for (row, expected) in [
            (
                0,
                Effect::PlayCollection {
                    selected: 1,
                    members: vec![1, 2],
                    shuffle: false,
                },
            ),
            (2, Effect::PlayNextCollection(vec![1, 2])),
            (3, Effect::AddToQueueCollection(vec![1, 2])),
        ] {
            let mut ui = Ui::default();
            let mut app = model(Screen::Album);
            app.navigation.filter = "album:Echoes".into();
            ui.action(&mut app, &tracks, Action::ContextMenu);
            app.navigation.modal_focus = row;
            assert_eq!(ui.action(&mut app, &tracks, Action::Select), expected);
        }
    }
    #[test]
    fn destructive_actions_default_to_cancel() {
        let mut ui = Ui::default();
        let mut app = model(Screen::Home);
        ui.action(&mut app, &[], Action::PowerMenu);
        assert_eq!(app.navigation.modal_focus, 2);
        ui.action(&mut app, &[], Action::WheelCounterClockwise(2));
        ui.action(&mut app, &[], Action::Select);
        assert_eq!(
            app.navigation.modal,
            Some(Modal::Confirm(ConfirmAction::PowerOff))
        );
        assert_eq!(app.navigation.modal_focus, 0);
        assert_eq!(ui.action(&mut app, &[], Action::Select), Effect::None);
    }
    #[test]
    fn large_filtered_pages_keep_catalog_identity_and_visible_focus() {
        let tracks: Vec<_> = (0..20_000)
            .map(|i| Track {
                id: i,
                title: format!("Track {i}"),
                artist: if i % 2 == 0 {
                    "Even".into()
                } else {
                    "Odd".into()
                },
                online: true,
                ..Track::default()
            })
            .collect();
        let mut ui = Ui::default();
        let mut app = model(Screen::Tracks);
        app.navigation.filter = "artist:Odd".into();
        app.navigation.focus = 9998;
        ui.action(&mut app, &tracks, Action::WheelClockwise(8));
        assert_eq!(app.navigation.focus, 9999);
        let page = ui.track_rows_page(&app, &tracks, 9997, 5);
        assert_eq!(
            page.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            vec!["track:19995", "track:19997", "track:19999"]
        );
        assert_eq!(
            focus_target_count(&ui.draw(&app, &tracks, "ok", false, PowerView::default())),
            1
        );
        app.navigation.modal = Some(Modal::Confirm(ConfirmAction::ClearQueue));
        ui.action(&mut app, &tracks, Action::WheelClockwise(8));
        assert_eq!(app.navigation.modal_focus, 1);
        assert_eq!(app.navigation.focus, 9999);
        let original_ptr = tracks.as_ptr();
        app.library.tracks = tracks;
        ui.model_action(&mut app, Action::Back);
        assert_eq!(app.library.tracks.len(), 20_000);
        assert_eq!(app.library.tracks.as_ptr(), original_ptr);
    }
    #[test]
    fn forgetting_radio_entries_requires_confirmation_and_emits_service_effect() {
        let mut ui = Ui {
            saved_networks: vec![Item::new("Saved: Studio", "saved:7")],
            ..Default::default()
        };
        let mut app = model(Screen::Wifi);
        app.navigation.focus = 2;
        ui.action(&mut app, &[], Action::ContextMenu);
        assert_eq!(app.navigation.modal, Some(Modal::ContextMenu));
        app.navigation.modal_focus = 1;
        ui.action(&mut app, &[], Action::Select);
        assert_eq!(
            app.navigation.modal,
            Some(Modal::Confirm(ConfirmAction::ForgetWifi))
        );
        app.navigation.modal_focus = 1;
        assert_eq!(
            ui.action(&mut app, &[], Action::Select),
            Effect::WifiForget(7)
        );
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
            .filter_map(|q| q.glyph.and_then(|g| char::from_u32(g as u32)))
            .collect::<String>();
        assert!(!glyphs.contains("secretpass"));
    }
    #[test]
    fn font_and_icon_assets_have_fixed_dimensions() {
        assert_eq!(font_atlas().len(), 1024 * 1024 * 4);
        assert_eq!(display_font_atlas().len(), 1024 * 1024 * 4);
        assert_eq!(icons_atlas().len(), 192 * 160 * 4);
    }
}

#[cfg(test)]
mod tests_v1;
