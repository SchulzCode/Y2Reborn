#![forbid(unsafe_code)]

pub mod art;
mod boot;
mod canvas;
mod catalog;
pub mod charge;
mod components;
mod date_time;
pub mod diagnostics;
mod flow;
mod folders;
mod fontdata;
mod glyph_cache;
pub use glyph_cache::{
    available as cjk_font_available, set_budget as set_glyph_budget, GLYPHS_PER_FRAME,
};
pub mod keyboard;
mod maskdata;
mod motion;
pub mod pages;
pub mod playlist_index;
mod playlists_ui;
pub mod qr;
pub use playlists_ui::{is_favorites as playlists_is_favorites, PlaylistsView};
mod actions;
mod context_menus;
mod drawing;
mod navigation;
mod rows;
mod screens;
mod search;
mod search_flow;
mod selects;
pub mod skin;
mod text;
pub mod theme;
mod time_zone;
mod ui_state;
mod up_next;
mod views_books;
mod views_coverflow;
mod views_library;
mod views_playing;
mod views_playlists;
mod views_queue;
mod views_radio;
mod views_search;
mod views_settings;
mod views_system;

use reborn_core::{
    platform::{ChargingState, LowBattery, UsbTransfer, WifiProblem},
    Action, AppModel, AudioOutput, CodecPreference, ConfirmAction, Modal, NavigationFrame,
    PlatformTask, RadioRequest, RadioScan, RepeatMode, Screen, ShuffleMode, Track, WrapAround,
};
use reborn_graphics::Quad;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

pub use boot::{
    black_frame, boot_failure_screen, boot_label_frame, boot_mark_frame, boot_screen,
    boot_transition, closing_label, failure_line_y, shutdown_frame, shutdown_plan, with_overlay,
    BootPhase, ShutdownPlan, BAR_FILL, BAR_H, BAR_TRACK, BAR_W, BAR_X, BAR_Y, BOOT_FADE_FRAMES,
    BOOT_FAILURE_LABELS, BOOT_FAILURE_TOKENS, BOOT_FINAL_LABEL, BOOT_PHASES, FAILURE_LINE_PITCH,
    LABEL_Y, SAVING_LABEL,
};
pub use components::art_key;
pub use reborn_core::Effect;

/// Count the semantic focus markers emitted by the presentation layer. The
/// marker is metadata only; the native renderer ignores it, while preview and
/// state tests use it to enforce the one-focus invariant.
pub fn focus_target_count(quads: &[Quad]) -> usize {
    quads.iter().filter(|quad| quad.focus_target).count()
}

/// Library size stamp and the artist, album and song counts for it.
type CountsCache = Option<((usize, usize), reborn_core::ArtistsList, [usize; 4])>;
/// Library identity, album filter key and its detail line.
type DetailCache = Option<((usize, usize), String, String)>;
/// A group menu's song count: (library, group key, songs).
type GroupCount = Option<((usize, usize), String, usize)>;
/// Songs and milliseconds per (album artist, album), for a library.
type AlbumTotals = Option<((usize, usize), HashMap<(String, String), (usize, u64)>)>;

/// How a settings-style row presents its value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowKind {
    /// A value or nothing at the right.
    #[default]
    Plain,
    /// An on/off switch.
    Toggle(bool),
    /// A choice in a picker; the current one carries a check mark.
    Check(bool),
    /// Opens another page.
    Chevron,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub key: String,
    pub secondary: String,
    pub enabled: bool,
    pub kind: RowKind,
    /// Index of the library track whose cover represents this row.
    pub art: Option<usize>,
}

impl Item {
    pub fn new(label: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            key: key.into(),
            secondary: String::new(),
            enabled: true,
            kind: RowKind::Plain,
            art: None,
        }
    }

    pub fn with_secondary(mut self, secondary: impl Into<String>) -> Self {
        self.secondary = secondary.into();
        self
    }

    pub fn with_kind(mut self, kind: RowKind) -> Self {
        self.kind = kind;
        self
    }
}

/// Select Songs (Beta 8.33): the list it was started on and the row keys
/// chosen there (songs `track:`, Up Next entries `queue:`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub screen: Screen,
    pub filter: String,
    pub keys: Vec<String>,
}

/// Connection progress of the joined network, in user terms.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WifiStatus {
    #[default]
    Disconnected,
    Connecting(String),
    GettingAddress(String),
    Connected(String),
    NoInternet(String),
}
impl WifiStatus {
    pub fn network(&self) -> Option<&str> {
        match self {
            Self::Disconnected => None,
            Self::Connecting(n)
            | Self::GettingAddress(n)
            | Self::Connected(n)
            | Self::NoInternet(n) => Some(n),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkView {
    pub ssid: String,
    /// 0–3 signal bars.
    pub bars: u8,
    /// `None`: security this player cannot join.
    pub secured: Option<bool>,
    pub saved_id: Option<u32>,
    pub visible: bool,
}

/// How the owner's last join went (Y2Reborn #20: tried before it is saved).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiJoinOutcome {
    Checking,
    Joined,
    WrongPassword,
    NotFound,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiJoin {
    pub attempt: u64,
    pub ssid: String,
    pub outcome: WifiJoinOutcome,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiView {
    pub available: bool,
    pub powered: bool,
    pub scan: RadioScan,
    pub status: WifiStatus,
    pub problem: Option<WifiProblem>,
    /// The saved network the problem belongs to (its password failed).
    pub problem_network: Option<String>,
    pub join: Option<WifiJoin>,
    /// Joined network first, then saved, then others; already de-duplicated.
    pub networks: Vec<NetworkView>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BluetoothDeviceView {
    pub path: String,
    pub address: String,
    pub name: String,
    pub paired: bool,
    pub bonded: bool,
    pub connected: bool,
    pub audio_ready: bool,
    /// Actually negotiated codec, never the preference.
    pub codec: Option<String>,
    /// A Connect waiting for another Bluetooth operation to finish.
    pub connecting: bool,
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
    /// The device's state in the list: Connected, Paired or Not paired (UI
    /// v4); its codec is on Sound > Output.
    fn description(&self) -> String {
        if !self.saved_pairing() {
            if self.connected {
                "Finishing pairing…".into()
            } else {
                "Not paired".into()
            }
        } else if self.usable_audio() {
            "Connected".into()
        } else if self.connected {
            "Connecting audio…".into()
        } else if self.connecting {
            "Connecting…".into()
        } else {
            "Paired".into()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BluetoothView {
    pub available: bool,
    pub powered: bool,
    pub scan: RadioScan,
    pub devices: Vec<BluetoothDeviceView>,
    /// Policy-enabled codecs the connected peer can use; empty hides the row.
    pub codec_choices: Vec<CodecPreference>,
    /// SBC quality is useful even when SBC is the peer's only usable codec.
    pub sbc_quality_available: bool,
    pub problem: Option<String>,
}

/// Settings > Last.fm as the player knows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LastFmView {
    /// This build carries the app's Last.fm key.
    pub available: bool,
    /// The account plays go to.
    pub user: Option<String>,
    /// Plays waiting to be sent.
    pub waiting: usize,
    /// A sign-in in progress: the address its QR code holds, and the code.
    pub approval: Option<String>,
    pub code: Option<qr::Code>,
    /// A request is running (sign-in step or sending).
    pub busy: bool,
}

/// Internet radio lists as the player last received them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RadioView {
    /// Title of the station list on screen.
    pub title: String,
    pub stations: Vec<reborn_core::RadioStation>,
    /// Countries or genres of the directory.
    pub categories: Vec<RadioCategory>,
    pub genres: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// `RadioStation::key` of every favorite.
    pub favorites: Vec<String>,
    /// Wi-Fi is online: the directory and stations are reachable.
    pub online: bool,
    /// Repeats the last list request ("Try Again").
    pub last_request: Option<reborn_core::RadioRequest>,
    /// The station playing now (Radio's On Air card, Now Playing).
    pub now: Option<reborn_core::RadioStation>,
    /// The song the station announces: (title, artist), artist possibly empty.
    pub song: Option<(String, String)>,
    /// The station whose details Station Info shows.
    pub info: Option<reborn_core::RadioStation>,
    /// The list starts with this many stations ranked by popularity (the
    /// directory's order); the rest follow A to Z.
    pub ranked: usize,
}
impl RadioView {
    pub fn is_favorite(&self, station: &reborn_core::RadioStation) -> bool {
        self.favorites.iter().any(|k| k == station.key())
    }
    /// A station list, A to Z by name (case, accents and "The" aside).
    pub fn set_stations(&mut self, mut stations: Vec<reborn_core::RadioStation>) {
        sort_stations(&mut stations);
        self.stations = stations;
        self.ranked = 0;
    }
    /// A list in the directory's popularity order: its first `top` stay on
    /// top, ranked, and every other station follows A to Z.
    pub fn set_stations_ranked(
        &mut self,
        mut stations: Vec<reborn_core::RadioStation>,
        top: usize,
    ) {
        let top = top.min(stations.len());
        let mut rest = stations.split_off(top);
        sort_stations(&mut rest);
        stations.extend(rest);
        self.stations = stations;
        self.ranked = top;
    }
    /// Countries or genres, A to Z; genre names capitalised ("Hip Hop").
    pub fn set_categories(&mut self, mut categories: Vec<RadioCategory>) {
        if self.genres {
            for c in &mut categories {
                c.name = reborn_core::capitalise(&c.name);
            }
        }
        categories.sort_by_cached_key(|c| {
            reborn_core::sort::sort_key(&c.name, reborn_core::sort::SortAs::Title)
        });
        self.categories = categories;
    }
    /// The station announces "Artist - Title" (most do); split it.
    pub fn set_song(&mut self, line: Option<&str>) {
        self.song = line.map(|line| match line.split_once(" - ") {
            Some((artist, title)) if !artist.trim().is_empty() && !title.trim().is_empty() => {
                (title.trim().to_owned(), artist.trim().to_owned())
            }
            _ => (line.trim().to_owned(), String::new()),
        });
    }
}

/// Stations A to Z by name, as library lists sort.
pub fn sort_stations(stations: &mut [reborn_core::RadioStation]) {
    stations.sort_by_cached_key(|s| {
        reborn_core::sort::sort_key(&s.name, reborn_core::sort::SortAs::Title)
    });
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RadioCategory {
    pub id: String,
    pub name: String,
    pub stations: u32,
}

/// Presentation-owned transient state. Playback, queue, navigation, settings,
/// and screen state remain authoritative in `AppModel`; services are reached
/// only through the typed `Effect` returned by `action`.
#[derive(Default)]
pub struct Ui {
    pub notice: String,
    pub notice_until: Option<Instant>,
    pub wifi: WifiView,
    pub bluetooth: BluetoothView,
    pub radio: RadioView,
    pub lastfm: LastFmView,
    pub pairing: Option<String>,
    pub pairing_focus: usize,
    /// The full-screen keyboard, while it is open.
    pub keyboard: Option<keyboard::Keyboard>,
    /// The network and password of the join being checked, in memory only:
    /// a wrong password reopens the keyboard with it, to fix one letter.
    wifi_typed: Option<(String, String)>,
    /// The last join outcome already said.
    wifi_join_seen: u64,
    /// Playlist files as read, and what the Playlist screens are doing.
    pub playlists: playlists_ui::PlaylistsView,
    /// Which library song each playlist entry names.
    playlist_index: RefCell<playlist_index::Index>,
    playlist_rows_cache: RefCell<Option<(playlists_ui::RowsKey, Vec<Item>)>>,
    playlist_facts: RefCell<
        std::collections::HashMap<
            std::path::PathBuf,
            (playlists_ui::FactsKey, std::rc::Rc<playlists_ui::Facts>),
        >,
    >,
    /// The last station search, offered again when reopened (a library
    /// search starts empty, #44).
    radio_query: String,
    catalog: std::cell::RefCell<catalog::Catalog>,
    /// Accent colour taken from the current cover (0 = the default accent).
    pub accent: u32,
    /// The volume line is shown until this moment.
    pub volume_until: Option<Instant>,
    /// When the Power button reached the turn-off hold.
    pub power_hold: Option<Instant>,
    /// Fast list scrolling shows the current letter until this moment.
    pub fast_scroll_until: Option<Instant>,
    /// The last wheel or arrow step in a list, even one held at its end: a
    /// long list wraps around only after the wheel rested there (`WRAP_REST`).
    last_step: Option<Instant>,
    /// The library's audiobooks, kept until the library changes.
    books: RefCell<Option<views_books::BookCache>>,
    /// Taps on the version row; the service menu opens after a deliberate run.
    version_taps: u8,
    resident: HashSet<u64>,
    wanted: RefCell<Vec<(u64, usize)>>,
    /// Station logos the last frame wanted: thumbnail key and logo address.
    wanted_logos: RefCell<Vec<(u64, String)>>,
    counts: RefCell<CountsCache>,
    /// A dialog the player raises by itself (Bluetooth failure, low battery).
    pub alert: Option<Alert>,
    alert_focus: usize,
    /// The device a pair/connect request is waiting on: path, name, operation, start.
    bt_busy: Option<(String, String, String, Instant)>,
    usb_state: Option<UsbTransfer>,
    /// A computer connected (#28): the runtime owes one haptic tick.
    connect_tick: bool,
    /// The one-step update is running (Beta 7.1): download, check and the
    /// restart to install follow each other on one screen that cannot be
    /// left; Cancel stops it while it downloads.
    pub update_run: bool,
    /// The update is installed: the platform restarts the player now.
    pub update_restarting: bool,
    /// UI motion (Beta 7.2): gliding focus, scrolling, screen slides,
    /// sheets, the Now Playing wave. The player turns it on; previews and
    /// tests draw still, exact frames.
    pub ui_motion: bool,
    motion: motion::Motion,
    /// The screen last drawn (screen, filter, depth) and its frame, for
    /// the slide to the next screen.
    seen: RefCell<Option<Seen>>,
    slide: RefCell<Option<Slide>>,
    /// The search's keys folding away into its page (Beta 8.32): when it
    /// began, and the keys as they were.
    folding: RefCell<Option<(Instant, keyboard::Keyboard)>>,
    /// Up Next's history rows and library lookups (Beta 8.33).
    up_next_cache: RefCell<up_next::Cache>,
    /// The Up Next entry being moved with the wheel.
    queue_moving: std::cell::Cell<Option<reborn_core::QueueEntryId>>,
    /// Up Next's focused row and its key as last seen: the focus follows
    /// that song when the rows move under it.
    up_next_focus: RefCell<Option<(usize, String)>>,
    /// The music's loudness as heard now (the player measures it) and the
    /// smoothed levels the wave last showed, when.
    heard: Option<Levels>,
    shown_levels: RefCell<Option<(Levels, Instant)>>,
    /// A settings page's first row shown, as last drawn (its own window:
    /// the rows fit under its text differ from page to page).
    group_scroll: RefCell<Option<(Screen, String, usize)>>,
    /// Select Songs: the rows chosen on one list.
    pub selection: Option<Selection>,
    /// The playing position as last reported, and when: Now Playing moves
    /// on between reports.
    position_seen: std::cell::Cell<Option<(u64, Instant)>>,
    /// The song and the play state Now Playing last showed: a change fades
    /// the new song in and pulses the button (Beta 7.2).
    np_seen: std::cell::Cell<Option<(i64, bool)>>,
    battery_acked: u8,
    /// Something on screen sweeps with time (a spinner), so keep redrawing.
    spinning: bool,
    /// The update's download or check is on screen (Beta 7.6): its bar,
    /// sweep and count run at the panel's full frame rate, like motion.
    download_shown: bool,
    /// Freeze spinners at this phase (previews and tests).
    pub spin_fixed: Option<f32>,
    /// The update download bar as drawn: fill, step (check), last frame.
    download_bar: std::cell::Cell<(f32, bool, Option<Instant>)>,
    /// Freeze the clock at this instant, unix seconds (previews and tests).
    pub clock_fixed: Option<i64>,
    /// The status-bar clock as last drawn; a new minute redraws.
    clock_shown: Option<String>,
    /// The clock and date texts for (zone, minute): the zone's rule is looked
    /// up and the texts made once a minute, not on every pass of the main loop.
    clock_cache: RefCell<Option<(String, i64, Option<time_zone::Clock>)>>,
    /// The time zone picker's rows for (zone, minute): several hundred
    /// zones parsed and sorted once, not on every frame.
    zone_rows: RefCell<Option<(String, i64, Vec<Item>)>>,
    /// The date and time being set by hand (Settings > Display > Date & Time).
    date_draft: Option<reborn_core::clock::Local>,
    /// Cover Flow's scroll position and the cover's flight to the album page.
    flow: RefCell<flow::Flow>,
    transition: std::cell::Cell<Option<flow::Transition>>,
    /// Freeze Cover Flow motion at its target (previews and tests).
    pub motion_fixed: bool,
    /// The cover now in the collection-art texture (full resolution).
    pub collection_art: std::cell::Cell<Option<u64>>,
    /// The centre cover the last frame wants at full resolution.
    cover_request: std::cell::Cell<Option<(u64, usize)>>,
    /// The last album detail line.
    detail: RefCell<DetailCache>,
    /// Every album's songs and length, counted once per library: Cover
    /// Flow names each album it passes (Beta 8.39).
    album_totals: RefCell<AlbumTotals>,
    /// The song count a group's menu shows, for (library, group).
    group_count: RefCell<GroupCount>,
    /// How far `prewarm` got, for which library.
    prewarmed: std::cell::Cell<(Option<(usize, usize)>, usize)>,
    /// The skin in use; `None` is Reborn's own look.
    pub skin: Option<std::rc::Rc<skin::Skin>>,
    /// Installed skins, as last scanned for the picker.
    pub themes: Vec<skin::SkinInfo>,
    /// Headphone profile files, as last scanned for the profile list.
    pub eq_profiles: Vec<reborn_core::eq::ProfileFile>,
}

/// A dialog the player raises by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Alert {
    /// A Bluetooth pair/connect request failed.
    BluetoothFailed {
        path: String,
        name: String,
        operation: String,
    },
    /// The battery crossed a warning level.
    BatteryLow,
}

/// Library lists at least this long may use cadence-based wheel acceleration.
const ACCELERATED_LIST_MINIMUM: usize = 40;
/// The fast-scroll letter stays this long after the last detent; while it
/// shows, the wheel steps by letter (#30).
const FAST_SCROLL_LINGER: Duration = Duration::from_millis(700);
/// A list longer than this that reaches an end stops there; one more step
/// after the wheel rested this long continues at the other end, so a fast
/// spin through a long library never flies past its end.
const WRAP_REST: Duration = Duration::from_millis(350);
/// Volume change per wheel detent on Now Playing.
const WHEEL_VOLUME_STEP: i8 = 2;
/// Taps on the version row that open the service menu.
const VERSION_TAPS: u8 = 7;
/// How long the volume line stays, and how long it takes to fade at the end.
const VOLUME_LINE: Duration = Duration::from_millis(1300);
const VOLUME_FADE: Duration = Duration::from_millis(300);
/// The Power hold: it starts when the input layer reports the long press and
/// fills the ring over this long; releasing before the end cancels it.
pub const POWER_HOLD: Duration = Duration::from_millis(1200);

impl Ui {}

/// The screen last drawn: screen, filter, history depth and its frame.
type Seen = (Screen, String, usize, Vec<Quad>);

/// A screen slide in progress: the frame being left and the direction.
struct Slide {
    started: Instant,
    push: bool,
    old: Vec<Quad>,
}

/// How long a screen slide takes.
const SLIDE_S: f32 = 0.2;
/// How long the search's keys take to fold away.
const FOLD_S: f32 = 0.18;

fn route(key: &str) -> Option<Screen> {
    Some(match key {
        "music" => Screen::Music,
        "now_playing" => Screen::NowPlaying,
        "queue" => Screen::Queue,
        "settings" => Screen::Settings,
        "albums" => Screen::Albums,
        "artists" => Screen::Artists,
        "genres" => Screen::Genres,
        "tracks" => Screen::Tracks,
        "folders" => Screen::Folders,
        "wifi" => Screen::Wifi,
        "bluetooth" => Screen::Bluetooth,
        "pc_transfer" => Screen::PcTransfer,
        "audio" => Screen::SettingsAudio,
        "equalizer" => Screen::Equalizer,
        "playback" => Screen::SettingsPlayback,
        "crossfade" => Screen::Crossfade,
        "balance" => Screen::Balance,
        "screen_timeout" => Screen::ScreenTimeout,
        "output_page" => Screen::Output,
        "hp_level" => Screen::HeadphoneLevel,
        "dac_filter" => Screen::DacFilter,
        "theme" => Screen::Theme,
        "library" => Screen::SettingsLibrary,
        "display" => Screen::SettingsDisplay,
        "customize" => Screen::Customize,
        "audiobooks" => Screen::Audiobooks,
        "home_menu" => Screen::HomeMenu,
        "system" => Screen::SettingsSystem,
        "battery" => Screen::Battery,
        "sleep" => Screen::Sleep,
        "storage" => Screen::Storage,
        "update" => Screen::Update,
        "about" => Screen::About,
        "maintenance" => Screen::Maintenance,
        "diagnostics" => Screen::Diagnostics,
        "radio" => Screen::Radio,
        "lastfm" => Screen::LastFm,
        "error_reports" => Screen::ErrorReports,
        _ => return None,
    })
}

/// The platform installs with the charger connected or at least half a
/// battery (`update.battery_gate`); asked before the download, not after it.
pub(crate) fn update_charge_ok(m: &AppModel) -> bool {
    let b = &m.platform.battery;
    matches!(b.charging, ChargingState::Charging | ChargingState::Full)
        || b.percent.is_some_and(|p| p >= 50)
}

/// The running download can still be stopped: nothing changed on the player.
pub(crate) fn update_cancellable(m: &AppModel) -> bool {
    m.platform.busy == Some(PlatformTask::UpdateStage)
        && !m.platform.download.is_some_and(|l| l.verifying)
}

fn platform_task(name: &str) -> PlatformTask {
    use PlatformTask::*;
    match name {
        "health" => Health,
        "update_check" => UpdateCheck,
        "update_stage" => UpdateStage,
        "update_apply" => UpdateApply,
        "update_cancel" => UpdateCancel,
        "update_rollback" => UpdateRollback,
        "export" => Export,
        "diagnostics_export" => DiagnosticsExport,
        "sleep" => SleepRequest,
        "storage_benchmark" => StorageBenchmark,
        "library_benchmark" => LibraryBenchmark,
        "network" => NetworkCheck,
        "internal_setup" => InternalStorageSetup,
        "sd_repair" => SdRepair,
        _ => Refresh,
    }
}

fn confirm_action(name: &str) -> Option<ConfirmAction> {
    Some(match name {
        "reboot" => ConfirmAction::Reboot,
        "power_off" => ConfirmAction::PowerOff,
        "rebuild_library" => ConfirmAction::RebuildLibrary,
        "reset_settings" => ConfirmAction::ResetSettings,
        "forget_wifi" => ConfirmAction::ForgetAllWifi,
        "forget_bluetooth" => ConfirmAction::ForgetAllBluetooth,
        "clear_cache" => ConfirmAction::ClearCache,
        "update_apply" | "update_cancel" | "update_rollback" | "storage_benchmark"
        | "library_benchmark" | "internal_setup" | "sd_repair" => {
            ConfirmAction::Platform(platform_task(name))
        }
        "report_new_id" => ConfirmAction::Platform(PlatformTask::ReportNewId),
        _ => return None,
    })
}

/// Red (cannot be undone): everything except platform checks; erasing the old
/// storage and repairing the SD card (it may shorten damaged files) are red.
pub(crate) fn confirm_is_danger(action: ConfirmAction) -> bool {
    !matches!(action, ConfirmAction::Platform(task)
        if !matches!(task, PlatformTask::InternalStorageSetup | PlatformTask::SdRepair))
}

fn confirm_label(action: ConfirmAction) -> &'static str {
    match action {
        ConfirmAction::ClearQueue => "Clear",
        ConfirmAction::RebuildLibrary => "Rebuild",
        ConfirmAction::ForgetBluetooth | ConfirmAction::ForgetWifi => "Forget",
        ConfirmAction::ForgetAllWifi => "Forget All",
        ConfirmAction::ForgetAllBluetooth => "Remove All",
        ConfirmAction::ResetSettings => "Reset",
        ConfirmAction::ClearCache => "Clear",
        ConfirmAction::DeletePlaylist => "Delete",
        ConfirmAction::PowerOff => "Power Off",
        ConfirmAction::Reboot => "Restart",
        ConfirmAction::Platform(PlatformTask::UpdateApply) => "Restart and Install",
        ConfirmAction::Platform(PlatformTask::UpdateCancel) => "Cancel Update",
        ConfirmAction::Platform(PlatformTask::UpdateRollback) => "Restore",
        ConfirmAction::Platform(PlatformTask::InternalStorageSetup) => "Set Up Now",
        ConfirmAction::Platform(PlatformTask::SdRepair) => "Repair",
        ConfirmAction::Platform(PlatformTask::ReportNewId) => "New ID",
        ConfirmAction::ReportsQuestion => "Send Reports",
        ConfirmAction::LoudVolume => "Raise Volume",
        ConfirmAction::Platform(_) => "Run",
    }
}

fn confirm_copy(action: ConfirmAction) -> (&'static str, &'static str) {
    match action {
        ConfirmAction::DeletePlaylist => (
            "Delete This Playlist?",
            "The playlist file is deleted from the player. Your songs stay.",
        ),
        ConfirmAction::ClearQueue => (
            "Clear Upcoming Songs?",
            "The current song keeps playing. Songs after it are removed from the queue.",
        ),
        ConfirmAction::RebuildLibrary => (
            "Rebuild Music Library?",
            "Every music file is read again. Your music is not changed, and the library stays usable while this runs.",
        ),
        ConfirmAction::ForgetBluetooth => (
            "Forget This Device?",
            "You'll need to pair it again to use it.",
        ),
        ConfirmAction::ForgetWifi => (
            "Forget This Network?",
            "Its saved password is removed from the player.",
        ),
        ConfirmAction::ForgetAllWifi => (
            "Forget All Wi-Fi Networks?",
            "Every saved network and password is removed.",
        ),
        ConfirmAction::ForgetAllBluetooth => (
            "Remove All Bluetooth Pairings?",
            "Every paired headphone and speaker must be paired again.",
        ),
        ConfirmAction::ResetSettings => (
            "Reset Reborn Settings?",
            "Playback, audio and display settings return to their defaults. Music, queue and pairings stay.",
        ),
        ConfirmAction::ClearCache => (
            "Clear Cache?",
            "Saved artwork copies are removed and recreated when needed. Music is not affected.",
        ),
        ConfirmAction::PowerOff => ("Power Off?", "Your queue and position are saved."),
        ConfirmAction::Reboot => ("Restart?", "Your queue and position are saved."),
        ConfirmAction::Platform(PlatformTask::UpdateApply) => (
            "Restart and Install?",
            "The player restarts, installs the update and starts the new version by itself. It takes about a minute. Use the charger or at least half a battery.",
        ),
        ConfirmAction::Platform(PlatformTask::UpdateCancel) => (
            "Cancel This Update?",
            "The installed software stays as it is.",
        ),
        ConfirmAction::Platform(PlatformTask::UpdateRollback) => (
            "Restore Previous System?",
            "The player restarts into the system that was installed before the last update.",
        ),
        ConfirmAction::Platform(PlatformTask::InternalStorageSetup) => (
            "Set Up Internal Storage?",
            "You get 5.4 GB for music. The old Innioasis storage and every file on it are erased: copy them to a computer first (the Old Storage drive). The player restarts afterwards.",
        ),
        ConfirmAction::Platform(PlatformTask::SdRepair) => (
            "Repair SD Card?",
            "Fixes the card so music can be added again. Damaged files may be shortened or removed; the rest stays. Keep the player charging or above 30 %.",
        ),
        ConfirmAction::Platform(PlatformTask::ReportNewId) => (
            "Make a New Install ID?",
            "Reports from now on can't be linked to the ones this player sent before.",
        ),
        ConfirmAction::ReportsQuestion => (
            "Help Improve Y2Linux?",
            "If something goes wrong, the player can send a report over Wi-Fi: logs and technical details, never your music, names or accounts. Change it in About & Updates.",
        ),
        ConfirmAction::LoudVolume => (
            "Raise the Volume?",
            "Listening at high volume for a long time can damage your hearing. This is asked again after 20 hours of loud listening.",
        ),
        ConfirmAction::Platform(_) => (
            "Run This Check?",
            "It uses a private scratch area. Your music is not touched.",
        ),
    }
}

fn next_repeat(mode: RepeatMode) -> RepeatMode {
    match mode {
        RepeatMode::Off => RepeatMode::All,
        RepeatMode::All => RepeatMode::Track,
        RepeatMode::Track => RepeatMode::Off,
    }
}

fn next_brightness(current: u8) -> u8 {
    reborn_core::BRIGHTNESS_LEVELS
        .iter()
        .copied()
        .find(|level| *level > current)
        .unwrap_or(reborn_core::BRIGHTNESS_LEVELS[0])
}

/// The letter a row of `screen` files under in A to Z order (the fast-scroll
/// overlay): "The Postal Service" is under P on Artists.
pub(crate) fn initial(label: &str, screen: Screen) -> char {
    use reborn_core::sort::{natural_key, sort_initial, sort_key, SortAs};
    let key = match screen {
        Screen::Artists => sort_key(label, SortAs::Artist),
        // Folders sort by the whole name, "The" included.
        Screen::Folders => natural_key(label),
        _ => sort_key(label, SortAs::Title),
    };
    sort_initial(&key)
}

/// The coverage-mask atlases the native renderer needs.
pub fn atlases() -> reborn_graphics::Atlases<'static> {
    reborn_graphics::Atlases {
        font_pages: [
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/fonts/reborn-regular.a8"
            )),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/fonts/reborn-semibold.a8"
            )),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/fonts/reborn-fallback.a8"
            )),
        ],
        masks: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/icons/reborn-masks.a8"
        )),
        mask_size: maskdata::MASK_SIZE,
    }
}

pub(crate) fn track_title(t: &Track) -> &str {
    if t.title.is_empty() {
        &t.filename
    } else {
        &t.title
    }
}
fn display_or_unknown(value: &str) -> &str {
    if value.is_empty() {
        "Unknown"
    } else {
        value
    }
}
/// The music's loudness in three bands as heard now, each 0 (silence) to 1
/// (loud): the playing wave moves with it (Beta 8.38).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Levels {
    pub bass: f32,
    pub mid: f32,
    pub high: f32,
}

/// Customize > Cover Flow is on: Albums and Audiobooks show their covers in
/// a row (8.36; Albums only before).
pub(crate) fn cover_flow(m: &AppModel) -> bool {
    m.settings.album_view == reborn_core::AlbumView::CoverFlow
}
/// An artist tag as shown: names joined by ";" read with commas (#40).
fn names_or_unknown(value: &str) -> std::borrow::Cow<'_, str> {
    if value.is_empty() {
        "Unknown".into()
    } else {
        reborn_core::joined_names(value)
    }
}
/// Identifies one published library: a new scan publishes a new list, so
/// caches built from the songs (search, playlists, catalog) start again.
/// A folder's name; a storage's root by what it is ("SD Card", not "sd").
pub(crate) fn folder_name(m: &AppModel, folder: &str) -> String {
    let path = std::path::Path::new(folder);
    match m.sources.iter().find(|s| s.root == path).map(|s| &s.kind) {
        Some(reborn_core::MediaSource::Internal) => "Internal Storage".into(),
        Some(reborn_core::MediaSource::SdCard(_)) => "SD Card".into(),
        None => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Folders".into()),
    }
}
pub(crate) fn library_stamp(tracks: &[Track]) -> (usize, usize) {
    (tracks.as_ptr() as usize, tracks.len())
}
/// A music track (never an audiobook part) shown under `filter`.
fn track_matches(track: &Track, filter: &str) -> bool {
    if !track.is_music() {
        return false;
    }
    if let Some(value) = filter.strip_prefix("artist:") {
        // One of its artists (#38); the whole tag still matches, as a page
        // saved before the split restores it.
        return track.artist == value
            || (track.artist.contains(value) && track.artists().contains(&value));
    }
    if let Some(value) = filter.strip_prefix("albumartist:") {
        return album_artist_of(track) == value;
    }
    if let Some(value) = filter.strip_prefix("genre:") {
        return in_genre(track, value);
    }
    if filter.starts_with("album:") {
        return match album_parts(filter) {
            Some((artist, album, genre)) => {
                track.album_artist == artist
                    && track.album == album
                    && genre.is_none_or(|g| in_genre(track, g))
            }
            None => filter.strip_prefix("album:") == Some(track.album.as_str()),
        };
    }
    if let Some(value) = filter.strip_prefix("folder:") {
        return track
            .path
            .parent()
            .is_some_and(|parent| parent.starts_with(value));
    }
    true
}
/// One of the song's genres (#40), or its whole tag as before the split.
fn in_genre(track: &Track, value: &str) -> bool {
    track.genre == value || (track.genre.contains(value) && track.genres().contains(&value))
}
/// The album artist a song is listed under when Artists shows album artists
/// (Customize > Artists): its album artist, else its first artist.
pub(crate) fn album_artist_of(track: &Track) -> &str {
    if track.album_artist.is_empty() {
        track.artists()[0]
    } else {
        &track.album_artist
    }
}
/// An album filter: "album:ARTIST\u{1f}ALBUM", and opened from a genre
/// "\u{1f}genre:GENRE" after it (only that genre's songs, #50).
pub(crate) fn album_parts(filter: &str) -> Option<(&str, &str, Option<&str>)> {
    let mut parts = filter.strip_prefix("album:")?.splitn(3, '\u{1f}');
    let artist = parts.next()?;
    let album = parts.next()?;
    Some((
        artist,
        album,
        parts.next().and_then(|g| g.strip_prefix("genre:")),
    ))
}
/// The artist an Artist page shows, from its filter.
pub(crate) fn artist_of_filter(filter: &str) -> Option<&str> {
    filter
        .strip_prefix("artist:")
        .or_else(|| filter.strip_prefix("albumartist:"))
}
/// What Up Next calls a collection played from the screen showing it.
/// The page on screen as the origin of what it plays (#25, #60).
pub(crate) fn page_origin(m: &AppModel) -> Option<reborn_core::QueueOrigin> {
    Some(reborn_core::QueueOrigin {
        screen: m.screen,
        filter: m.navigation.filter.clone(),
    })
}
pub(crate) fn source_label(m: &AppModel) -> String {
    let filter = &m.navigation.filter;
    match m.screen {
        Screen::Album => album_parts(filter)
            .map(|(_, album, _)| display_or_unknown(album).to_owned())
            .unwrap_or_else(|| "Album".into()),
        Screen::Artist => artist_of_filter(filter)
            .map(|artist| display_or_unknown(artist).to_owned())
            .unwrap_or_else(|| "Artist".into()),
        Screen::Search => format!("Search “{}”", catalog::search_parts(filter).0.trim()),
        Screen::Tracks => match filter.strip_prefix("genre:") {
            Some("") => "No Genre".into(),
            Some(genre) => genre.to_owned(),
            None => "Songs".into(),
        },
        _ => screens::screen_title(m),
    }
}
fn album_filter(track: &Track) -> String {
    format!("album:{}\u{1f}{}", track.album_artist, track.album)
}
/// The page of a track's (first) artist.
fn artist_filter(track: &Track) -> String {
    format!("artist:{}", track.artists()[0])
}
/// The sleep timer for Now Playing's menu: "Off", "23 min", "End of Chapter".
pub(crate) fn sleep_label(m: &AppModel, book: bool) -> String {
    match m.sleep_timer {
        None => "Off".into(),
        Some(t) => match (t.choice, t.until) {
            (reborn_core::SleepChoice::EndOfItem, _) if book => "End of Chapter".into(),
            (reborn_core::SleepChoice::EndOfItem, _) => "End of Song".into(),
            (_, Some(_)) => minutes_label(sleep_minutes_left(m).unwrap_or(1)),
            _ => "Off".into(),
        },
    }
}

/// A sleep timer choice as the picker names it: "45 min", "1 h 15 min".
pub(crate) fn sleep_choice_label(choice: reborn_core::SleepChoice, book: bool) -> String {
    match choice {
        reborn_core::SleepChoice::Off => "Off".into(),
        reborn_core::SleepChoice::EndOfItem if book => "End of Chapter".into(),
        reborn_core::SleepChoice::EndOfItem => "End of Song".into(),
        reborn_core::SleepChoice::Minutes(n) => minutes_label(u64::from(n)),
    }
}

/// Minutes as a sleep timer says them: "45 min", "2 h", "1 h 59 min".
pub(crate) fn minutes_label(minutes: u64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// The minutes a timed sleep has left, at least 1; none for the end of an item.
pub(crate) fn sleep_minutes_left(m: &AppModel) -> Option<u64> {
    let until = m.sleep_timer?.until?;
    Some(
        until
            .saturating_duration_since(Instant::now())
            .as_secs()
            .div_ceil(60)
            .max(1),
    )
}

/// "1.25", "2", "0.75": a speed without trailing zeros.
pub(crate) fn speed_label(speed: f32) -> String {
    let s = format!("{speed:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn repeat_label(mode: RepeatMode) -> &'static str {
    match mode {
        RepeatMode::Off => "Off",
        RepeatMode::Track => "One",
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
        0 => "Never".into(),
        s if s < 60 => format!("{s} sec"),
        60 => "1 min".into(),
        s => format!("{} min", s / 60),
    }
}

#[cfg(test)]
mod tests;
