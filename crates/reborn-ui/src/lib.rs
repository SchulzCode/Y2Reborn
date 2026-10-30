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
mod screens;
mod search;
pub mod skin;
mod text;
pub mod theme;
mod time_zone;
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

/// Spotify as the player knows it (sign-in, lists received, the song).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpotifyView {
    /// spotifyd and the library helper are installed.
    pub available: bool,
    /// A sign-in is stored.
    pub signed_in: bool,
    /// A sign-in in progress: the address its QR code holds, and the code
    /// to type at spotify.com/pair instead.
    pub approval: Option<String>,
    pub user_code: String,
    pub code: Option<qr::Code>,
    /// A sign-in request is running.
    pub busy: bool,
    /// spotifyd isn't running, and why (shown on the Spotify page).
    pub problem: Option<String>,
    /// Lists by key (`SpotifyList::key`), kept so Back shows them at once.
    pub lists: std::collections::HashMap<String, SpotifyList>,
    /// The song spotifyd plays now: (title, artist).
    pub song: Option<(String, String)>,
}

/// One Spotify list: a playlist, an album, Liked Songs, an artist's songs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpotifyList {
    pub title: String,
    pub rows: Vec<SpotifyRow>,
    /// Cursor of the next page, when there is one.
    pub next: Option<String>,
    /// A context for the whole list (Liked Songs).
    pub context: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpotifyRow {
    pub title: String,
    /// The artist (songs, albums) or owner (playlists).
    pub detail: String,
    pub uri: String,
    /// The playlist's, album's or artist's own id; a song's album.
    pub id: String,
}
impl SpotifyRow {
    /// From the helper's "Name - Artist" label.
    pub fn from_label(label: &str, uri: &str, id: &str) -> Self {
        let (title, detail) = match label.rsplit_once(" - ") {
            Some((t, d)) if !t.trim().is_empty() && !d.trim().is_empty() => (t.trim(), d.trim()),
            _ => (label.trim(), ""),
        };
        Self {
            title: title.into(),
            detail: detail.into(),
            uri: uri.into(),
            id: id.into(),
        }
    }
}

/// What a list key (the navigation filter of `Screen::SpotifyList`) shows:
/// the helper's command and its argument; an artist is a menu of its own.
pub fn spotify_list_kind(key: &str) -> (&str, &str) {
    key.split_once(':').unwrap_or((key, ""))
}

/// `Screen::SpotifyList`'s navigation filter: (list key, title).
pub fn spotify_filter(filter: &str) -> (&str, &str) {
    filter.split_once('\u{1f}').unwrap_or((filter, "Spotify"))
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
    pub spotify: SpotifyView,
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
    /// The clock text for (zone, minute): the zone's rule is looked up and
    /// the text made once a minute, not on every pass of the main loop.
    clock_cache: RefCell<Option<(String, i64, Option<String>)>>,
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

    /// An asynchronous operation may finish after navigation moved elsewhere.
    /// Keep that current page as the parent of the result, just like Select.
    pub fn show_operation_result(&mut self, model: &mut AppModel) {
        Self::go(model, Screen::DiagnosticSection, "result");
    }

    pub fn normalize(&self, m: &mut AppModel, tracks: &[Track]) {
        let count = self.row_count(m, tracks);
        m.navigation.focus = m.navigation.focus.min(count.saturating_sub(1));
        if m.screen == Screen::Queue {
            self.keep_up_next_focus(m, tracks);
        }
        if count > 0
            && !self
                .row_at(m, tracks, m.navigation.focus)
                .is_some_and(|r| r.enabled)
        {
            // The nearest row that takes the focus, below first.
            let focus = m.navigation.focus;
            m.navigation.focus = (focus..count)
                .chain((0..focus).rev())
                .find(|i| self.row_at(m, tracks, *i).is_some_and(|r| r.enabled))
                .unwrap_or(0);
        }
        if m.screen == Screen::Queue {
            self.follow_moving(m, tracks);
            self.up_next_focus.replace(
                self.row_at(m, tracks, m.navigation.focus)
                    .map(|r| (m.navigation.focus, r.key)),
            );
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
        self.notice_until = Some(Instant::now() + Duration::from_millis(1800));
    }

    pub fn expire_notice(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        if self.notice_until.is_some_and(|until| now >= until) {
            self.notice.clear();
            self.notice_until = None;
            changed = true;
        }
        if self.volume_until.is_some_and(|until| now >= until) {
            self.volume_until = None;
            changed = true;
        }
        if self.fast_scroll_until.is_some_and(|until| now >= until) {
            self.fast_scroll_until = None;
            changed = true;
        }
        changed
    }

    /// Whether something on screen changes with time (a fade, a ring), so the
    /// caller keeps redrawing.
    pub fn animating(&self) -> bool {
        self.volume_until.is_some()
            || self.power_hold.is_some()
            || self.spinning
            || self.gliding()
            || glyph_cache::pending()
    }

    /// Motion that wants the panel's full frame rate: Cover Flow's glide and
    /// the cover's flight, and (Beta 7.2) every eased value on its way, a
    /// screen sliding in and the playing wave. Spinners stay at the calm rate.
    pub fn gliding(&self) -> bool {
        self.flow.borrow().moving()
            || self.transition_progress().is_some()
            || (self.ui_motion
                && (self.motion.moving()
                    || self.slide.borrow().is_some()
                    || self.folding.borrow().is_some()
                    || self.download_shown))
    }

    /// Motion a button started (a glide, a slide, Cover Flow): it earns the
    /// short Interactive CPU lease. The playing wave alone does not: its
    /// ≈3-ms frames fit the lowest clock (Beta 7.4: less heat on Now Playing).
    pub fn responding(&self) -> bool {
        self.flow.borrow().moving()
            || self.transition_progress().is_some()
            || (self.ui_motion
                && (self.motion.settling()
                    || self.slide.borrow().is_some()
                    || self.folding.borrow().is_some()))
    }

    /// `target` eased in time under `key` (see `motion::Motion::ease`);
    /// exactly `target` without UI motion.
    pub(crate) fn ease(
        &self,
        key: &str,
        start: Option<f32>,
        target: f32,
        rate: f32,
        settle: f32,
    ) -> f32 {
        if !self.ui_motion {
            return target;
        }
        self.motion
            .ease(key, start, target, rate, settle, Instant::now())
    }

    /// Now Playing's changes since its last frame: (new song, new play state).
    pub(crate) fn np_changes(&self, song: i64, playing: bool) -> (bool, bool) {
        let before = self.np_seen.replace(Some((song, playing)));
        match before {
            Some((s, p)) => (s != song, p != playing),
            None => (false, false),
        }
    }

    /// The frame draws motion that goes on by itself (the playing wave).
    pub(crate) fn mark_live(&self) {
        if self.ui_motion {
            self.motion.mark_live();
        }
    }

    /// The playing position for this frame: the last report moved on by the
    /// time since, while playing (reports come a few times a second).
    pub(crate) fn shown_position(&self, m: &AppModel) -> u64 {
        let now = Instant::now();
        let (reported, at) = match self.position_seen.get() {
            Some((p, at)) if p == m.position_ms => (p, at),
            _ => {
                self.position_seen.set(Some((m.position_ms, now)));
                (m.position_ms, now)
            }
        };
        let playing = m.playback == reborn_core::PlaybackState::Playing;
        if !self.ui_motion || !playing {
            return reported;
        }
        let ahead = (now.saturating_duration_since(at).as_millis() as u64).min(1_500);
        // A faster (or slower) book moves through its time at its speed.
        let speed = m.book_part().map_or(1., |(book, _)| book.speed);
        let ahead = (ahead as f32 * speed) as u64;
        let length = m.current().map_or(u64::MAX, |t| t.duration_ms.max(1));
        (reported + ahead).min(length)
    }

    /// Every album's songs and length, counted once per library.
    fn count_albums(&self, tracks: &[Track]) {
        let library = crate::library_stamp(tracks);
        let mut totals = self.album_totals.borrow_mut();
        if totals.as_ref().is_some_and(|(l, _)| *l == library) {
            return;
        }
        let mut map: HashMap<(String, String), (usize, u64)> = HashMap::new();
        for t in tracks.iter().filter(|t| t.is_music()) {
            let entry = map
                .entry((t.album_artist.clone(), t.album.clone()))
                .or_default();
            entry.0 += 1;
            entry.1 += t.duration_ms;
        }
        *totals = Some((library, map));
    }

    /// While the screen is off, the work a long library's lists do when
    /// first opened is done ahead, one piece per call (each up to a second
    /// or two on the player with 120 000 songs): Music's counts, Songs,
    /// Artists, Albums, Genres, Folders, the search, the album lines and the
    /// playlists' songs.
    /// Returns whether a piece was done (more may follow).
    pub fn prewarm(&self, m: &AppModel, tracks: &[Track]) -> bool {
        const STEPS: usize = 9;
        let library = crate::library_stamp(tracks);
        let (seen, step) = self.prewarmed.get();
        let step = if seen == Some(library) { step } else { 0 };
        if step >= STEPS || tracks.is_empty() {
            return false;
        }
        match step {
            0 => {
                self.library_counts(m, tracks);
            }
            1..=4 => {
                let screen = [
                    Screen::Tracks,
                    Screen::Artists,
                    Screen::Albums,
                    Screen::Genres,
                ][step - 1];
                self.catalog.borrow_mut().prewarm(m, tracks, screen);
            }
            5 => self.catalog.borrow_mut().folders.ensure(tracks),
            6 => self.catalog.borrow_mut().index.prepare(tracks),
            7 => self.count_albums(tracks),
            // The playlists matched to the library (Playlists, the search),
            // once they are read.
            _ if !self.playlists.loaded => return false,
            _ => {
                for p in &self.playlists.lists {
                    self.facts(tracks, p);
                }
            }
        }
        self.prewarmed.set((Some(library), step + 1));
        true
    }

    /// "9 songs · 38 min" for the album with filter `key`, computed once per
    /// album and library rather than on every animation frame.
    pub(crate) fn album_detail(&self, m: &AppModel, tracks: &[Track], key: &str) -> String {
        let library = crate::library_stamp(tracks);
        let mut detail = self.detail.borrow_mut();
        if let Some((l, k, text)) = detail.as_ref() {
            if *l == library && k == key {
                return text.clone();
            }
        }
        // A plain album from the totals counted once; one opened from a
        // genre (only that genre's songs) is counted on its own.
        let text = match album_parts(key) {
            Some((artist, album, None)) => {
                self.count_albums(tracks);
                let totals = self.album_totals.borrow();
                let (songs, ms) = totals
                    .as_ref()
                    .and_then(|(_, map)| map.get(&(artist.to_owned(), album.to_owned())))
                    .copied()
                    .unwrap_or_default();
                views_library::album_line(songs, ms)
            }
            _ => views_library::album_detail(m, tracks, key),
        };
        *detail = Some((library, key.to_owned(), text.clone()));
        text
    }

    /// Cover Flow's fractional centre for this frame, advanced in time.
    pub(crate) fn flow_position(&self, m: &AppModel, focus: usize) -> f32 {
        self.flow.borrow_mut().advance(
            (m.screen, m.navigation.filter.clone()),
            focus,
            self.motion_fixed,
            Instant::now(),
        )
    }

    /// The running cover flight and its eased 0..1 progress.
    pub(crate) fn transition_progress(&self) -> Option<(flow::TransitionKind, f32)> {
        let t = self.transition.get()?;
        if self.motion_fixed {
            return None;
        }
        match t.progress(Instant::now()) {
            Some(p) => Some((t.kind, p)),
            None => {
                self.transition.set(None);
                None
            }
        }
    }

    /// The centre cover wants its full-resolution image once the row rests.
    pub(crate) fn request_cover(&self, key: u64, track_index: usize) {
        if self.collection_art.get() != Some(key) && self.flow.borrow().rested(120, Instant::now())
        {
            self.cover_request.set(Some((key, track_index)));
        }
    }

    /// The full-resolution cover the screen wants: art key and library track.
    /// The caller loads it and reports it with `collection_art`.
    pub fn take_cover_request(&self) -> Option<(u64, usize)> {
        self.cover_request.take()
    }

    /// The download bar's fill for this frame: it glides toward the reported
    /// value (reports come four times a second), and starts from empty when
    /// the step changes from download to check.
    pub(crate) fn download_fill(&self, target: f32, verifying: bool) -> f32 {
        let target = target.clamp(0., 1.);
        if self.motion_fixed || self.spin_fixed.is_some() {
            return target;
        }
        let now = Instant::now();
        let (shown, step, at) = self.download_bar.get();
        let shown = if step != verifying { 0. } else { shown };
        let dt = at.map_or(0., |t| now.duration_since(t).as_secs_f32().min(0.1));
        let next = if target < shown {
            target
        } else {
            shown + (target - shown) * (1. - (-dt * 6.).exp())
        };
        self.download_bar.set((next, verifying, Some(now)));
        next
    }

    /// A 0..1 phase for spinners, one turn about every 1.6 s.
    pub fn spin_phase(&self) -> f32 {
        if let Some(fixed) = self.spin_fixed {
            return fixed;
        }
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let elapsed = START.get_or_init(Instant::now).elapsed().as_millis() % 1600;
        elapsed as f32 / 1600.
    }

    /// A computer connected since the last call (#28): one haptic tick, as
    /// its message shows. Taking it clears it.
    pub fn take_connect_tick(&mut self) -> bool {
        std::mem::take(&mut self.connect_tick)
    }

    /// Watch platform state the UI reacts to by itself: a computer connecting,
    /// a Bluetooth request finishing or failing, the battery running low,
    /// work that shows a spinner and the clock's minute turning over.
    /// Returns whether the screen needs redrawing.
    pub fn observe(&mut self, m: &AppModel) -> bool {
        let before = (
            self.usb_state,
            self.alert.clone(),
            self.bt_busy.is_some(),
            self.spinning,
        );
        let usb = m.platform.snapshot.usb;
        if self.usb_state != Some(usb) {
            // Beta 7.3: a computer never covers the player; a short message
            // says what happened and the status bar shows the cable.
            if let Some(was) = self.usb_state {
                let attached = |s| matches!(s, UsbTransfer::Ready | UsbTransfer::Copying);
                let connected = attached(usb) && !attached(was);
                if usb == UsbTransfer::Copying && was != UsbTransfer::Copying {
                    self.flash("Copying files from the computer…");
                } else if connected {
                    self.flash("Connected to a computer");
                }
                self.connect_tick |= connected;
            }
            self.usb_state = Some(usb);
        }
        let level = match m.platform.battery.level {
            LowBattery::Normal => 0,
            LowBattery::Low => 1,
            LowBattery::Critical => 2,
            LowBattery::ShuttingDown => 3,
        };
        if level == 0 {
            self.battery_acked = 0;
        } else if level > self.battery_acked && level < 3 && self.alert.is_none() {
            self.alert = Some(Alert::BatteryLow);
            self.alert_focus = 0;
        }
        if let Some((path, name, operation, since)) = self.bt_busy.clone() {
            let connected = self
                .bluetooth
                .devices
                .iter()
                .any(|d| d.path == path && d.connected);
            if connected || since.elapsed() > Duration::from_secs(45) {
                self.bt_busy = None;
            } else if self.bluetooth.problem.is_some()
                && since.elapsed() > Duration::from_millis(800)
            {
                self.bt_busy = None;
                self.alert = Some(Alert::BluetoothFailed {
                    path,
                    name,
                    operation,
                });
                self.alert_focus = 1;
            }
        }
        let downloading = m.platform.snapshot.update.phase
            == reborn_core::platform::UpdatePhase::Downloading
            || m.platform.busy == Some(reborn_core::PlatformTask::UpdateStage);
        self.spinning = m.library.scanning || downloading || self.bt_busy.is_some();
        // Before Beta 7.6 the download drew at the spinners' calm rate
        // (≤ 29 fps): its sweep stepped ≈8 px a frame.
        self.download_shown =
            downloading && !m.screen_off && (self.update_run || m.screen == Screen::Update);
        let clock = self.clock(m);
        let ticked = clock != self.clock_shown && !m.screen_off;
        self.clock_shown = clock;
        before
            != (
                self.usb_state,
                self.alert.clone(),
                self.bt_busy.is_some(),
                self.spinning,
            )
            || self.spinning
            || ticked
    }

    /// The status-bar clock in the chosen zone, once the system time is set.
    pub fn clock(&self, m: &AppModel) -> Option<String> {
        time_zone::text(self, m)
    }

    /// The device a Bluetooth request is waiting on, with its operation.
    pub(crate) fn bluetooth_busy(&self) -> Option<(&str, &str)> {
        self.bt_busy
            .as_ref()
            .map(|(_, name, operation, _)| (name.as_str(), operation.as_str()))
    }

    pub(crate) fn alert_focus(&self) -> usize {
        self.alert_focus
    }

    /// Input for the overlays above every screen. `Some` means consumed.
    fn overlay_action(&mut self, m: &AppModel, action: Action) -> Option<Effect> {
        if let Some(alert) = self.alert.clone() {
            let buttons = match alert {
                Alert::BluetoothFailed { .. } => 2,
                Alert::BatteryLow => 1,
            };
            return Some(match action {
                Action::WheelClockwise(_) | Action::NavigateDown => {
                    self.alert_focus = (self.alert_focus + 1).min(buttons - 1);
                    Effect::None
                }
                Action::WheelCounterClockwise(_) | Action::NavigateUp => {
                    self.alert_focus = self.alert_focus.saturating_sub(1);
                    Effect::None
                }
                Action::Back | Action::Home => {
                    self.dismiss_alert(m);
                    Effect::None
                }
                Action::Select => {
                    let retry = self.alert_focus == 1;
                    self.dismiss_alert(m);
                    match alert {
                        Alert::BluetoothFailed {
                            path, operation, ..
                        } if retry => {
                            self.bt_busy = self
                                .bluetooth
                                .devices
                                .iter()
                                .find(|d| d.path == path)
                                .map(|d| {
                                    (
                                        path.clone(),
                                        d.name.clone(),
                                        operation.clone(),
                                        Instant::now(),
                                    )
                                });
                            Effect::BluetoothDevice { path, operation }
                        }
                        _ => Effect::None,
                    }
                }
                _ => Effect::None,
            });
        }
        None
    }

    fn dismiss_alert(&mut self, m: &AppModel) {
        if self.alert == Some(Alert::BatteryLow) {
            self.battery_acked = match m.platform.battery.level {
                LowBattery::Low => 1,
                LowBattery::Critical => 2,
                _ => 3,
            };
        }
        self.alert = None;
        self.alert_focus = 0;
    }

    /// The turn-off hold completed: the effect to run, once.
    pub fn power_hold_complete(&mut self) -> Option<Effect> {
        if self.power_hold_progress().is_some_and(|p| p >= 1.) {
            self.power_hold = None;
            return Some(Effect::PowerOff);
        }
        None
    }

    /// Artists, albums and songs in the library, recounted only when it or
    /// Customize > Artists changes.
    pub(crate) fn library_counts(&self, m: &AppModel, tracks: &[Track]) -> [usize; 4] {
        let stamp = crate::library_stamp(tracks);
        let list = m.settings.artists_list;
        if let Some((seen, seen_list, counts)) = *self.counts.borrow() {
            if seen == stamp && seen_list == list {
                return counts;
            }
        }
        let mut artists = std::collections::BTreeSet::new();
        let mut albums = std::collections::BTreeSet::new();
        let mut genres = std::collections::BTreeSet::new();
        let mut songs = 0;
        for t in tracks.iter().filter(|t| t.is_music()) {
            if list == reborn_core::ArtistsList::AlbumArtists {
                artists.insert(album_artist_of(t));
            } else {
                artists.extend(t.artists());
            }
            albums.insert((t.album.as_str(), t.album_artist.as_str()));
            genres.extend(t.genres());
            songs += 1;
        }
        let counts = [artists.len(), albums.len(), songs, genres.len()];
        *self.counts.borrow_mut() = Some((stamp, list, counts));
        counts
    }

    /// The accent of every control and state (UI v4): a skin's fixed accent,
    /// otherwise the one theme accent. It never follows the cover, so lists,
    /// Settings and sheets look the same with every song.
    pub fn accent(&self) -> u32 {
        self.skin
            .as_deref()
            .and_then(skin::Skin::accent)
            .unwrap_or(theme::color::ACCENT)
    }

    /// The playing cover's colour: Now Playing's backdrop and the Home cover
    /// card only, and the accent skins without a fixed one are remapped to.
    pub fn cover_accent(&self) -> u32 {
        if let Some(fixed) = self.skin.as_deref().and_then(skin::Skin::accent) {
            return fixed;
        }
        if self.accent == 0 {
            theme::color::ACCENT
        } else {
            self.accent
        }
    }

    /// Show the volume line for about a second.
    pub fn show_volume(&mut self) {
        self.volume_until = Some(Instant::now() + VOLUME_LINE);
    }

    /// Whether the volume line is visible now, and how opaque (0..=1).
    pub fn volume_visibility(&self) -> f32 {
        match self.volume_until {
            Some(until) => {
                let left = until.saturating_duration_since(Instant::now());
                (left.as_secs_f32() / VOLUME_FADE.as_secs_f32()).clamp(0., 1.)
            }
            None => 0.,
        }
    }

    /// Progress (0..=1) of the Power hold toward turning the player off.
    pub fn power_hold_progress(&self) -> Option<f32> {
        self.power_hold
            .map(|since| (since.elapsed().as_secs_f32() / POWER_HOLD.as_secs_f32()).min(1.))
    }

    /// Replace the set of thumbnails the renderer holds.
    pub fn set_resident_thumbs(&mut self, keys: impl IntoIterator<Item = u64>) {
        self.resident = keys.into_iter().collect();
    }

    pub(crate) fn thumb_resident(&self, key: u64) -> bool {
        self.resident.contains(&key)
    }

    pub(crate) fn want_thumb(&self, key: u64, track_index: usize) {
        let mut wanted = self.wanted.borrow_mut();
        if wanted.len() < 64 && !wanted.iter().any(|(k, _)| *k == key) {
            wanted.push((key, track_index));
        }
    }

    pub(crate) fn want_logo(&self, key: u64, url: &str) {
        let mut wanted = self.wanted_logos.borrow_mut();
        if wanted.len() < 32 && !wanted.iter().any(|(k, _)| *k == key) {
            wanted.push((key, url.to_owned()));
        }
    }

    /// Station logos the last `draw` could not show yet, with their address.
    pub fn take_wanted_logos(&self) -> Vec<(u64, String)> {
        std::mem::take(&mut self.wanted_logos.borrow_mut())
    }

    /// Thumbnails the last `draw` could not show yet, with a library track
    /// index that carries each cover.
    pub fn take_wanted_thumbs(&self) -> Vec<(u64, usize)> {
        std::mem::take(&mut self.wanted.borrow_mut())
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
        Self::close_modal(m);
    }

    fn close_modal(m: &mut AppModel) {
        m.navigation.modal = None;
        m.navigation.modal_focus = 0;
        m.navigation.context_target = None;
        m.navigation.context_key = None;
    }

    fn open_modal(m: &mut AppModel, modal: Modal) {
        m.navigation.modal = Some(modal);
        m.navigation.modal_focus = 0;
    }

    fn back(m: &mut AppModel) {
        if m.navigation.modal.is_some() {
            Self::close_modal(m);
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

    /// A Home Menu switch changes Home's rows while Home waits in the
    /// history: Back returns to the place that was focused, not its index.
    fn keep_home_focus(m: &mut AppModel, tracks: &[Track], effect: &Effect, spotify: bool) {
        let mut after = m.settings.clone();
        match effect {
            Effect::SetHomePin(key, on) => after.pin_home(key, *on),
            Effect::SetHomeShown(key, on) => after.show_on_home(key, *on),
            Effect::SetAudiobooksOnHome(on) => after.audiobooks_on_home = *on,
            _ => return,
        }
        let Some(at) = m
            .navigation
            .history
            .iter()
            .rposition(|f| f.screen == Screen::Home)
        else {
            return;
        };
        let before = pages::home_rows(&m.settings, tracks, spotify);
        let after = pages::home_rows(&after, tracks, spotify);
        let frame = &mut m.navigation.history[at];
        let key = before.get(frame.focus).map(|r| r.key.as_str());
        frame.focus = key
            .and_then(|k| after.iter().position(|r| r.key == k))
            .unwrap_or(frame.focus.min(after.len() - 1));
    }

    fn home(m: &mut AppModel) {
        m.navigation.stack.clear();
        m.navigation.history.clear();
        m.screen = Screen::Home;
        m.navigation.focus = 0;
        m.navigation.scroll = 0;
        m.navigation.filter.clear();
        Self::close_modal(m);
    }

    pub fn rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        match m.screen {
            Screen::Artists
            | Screen::Albums
            | Screen::Genres
            | Screen::Folders
            | Screen::Artist
            | Screen::Album
            | Screen::Tracks
            | Screen::Search => self.catalog_rows(m, tracks),
            Screen::Queue => self.queue_rows(m, tracks),
            Screen::Playlists | Screen::Playlist => self.playlist_rows(m, tracks),
            Screen::Audiobooks => views_books::book_rows(self, m, tracks),
            Screen::Book => views_books::page_rows(self, m, tracks),
            Screen::Diagnostics => diagnostics::root(m),
            Screen::DiagnosticSection => diagnostics::section_rows(m, &m.navigation.filter),
            Screen::ValueDetail => vec![Item::new("Back", "back")],
            Screen::TextEntry | Screen::Pairing => vec![],
            _ => pages::page(self, m, tracks).rows,
        }
    }

    pub fn collection_track<'a>(&self, m: &AppModel, tracks: &'a [Track]) -> Option<&'a Track> {
        let mut catalog = self.catalog.borrow_mut();
        self.ensure_catalog(&mut catalog, m, tracks);
        catalog
            .rows
            .iter()
            .find_map(|row| row.track().and_then(|i| tracks.get(i)))
    }
    pub fn invalidate_catalog(&self) {
        self.catalog.borrow_mut().clear();
    }
    /// The catalog for this screen, with the playlists the search looks
    /// through as Playlists lists them (refreshed when they change).
    fn ensure_catalog(&self, c: &mut catalog::Catalog, m: &AppModel, tracks: &[Track]) {
        let generation = self.playlists.generation();
        if m.screen == Screen::Search && c.playlists_generation != Some(generation) {
            let items = self
                .playlists
                .lists
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let facts = self.facts(tracks, p);
                    let mut item = Item::new(&p.name, format!("playlist:{i}"))
                        .with_secondary(playlists_ui::summary(p.entries.len(), facts.ms));
                    item.art = facts.mosaic.first().copied();
                    item
                })
                .collect();
            c.set_playlists(generation, items);
        }
        c.ensure(m, tracks);
    }
    fn catalog_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        let mut c = self.catalog.borrow_mut();
        self.ensure_catalog(&mut c, m, tracks);
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
        self.ensure_catalog(&mut c, m, tracks);
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
            self.ensure_catalog(&mut c, m, tracks);
            c.rows.len()
        } else if m.screen == Screen::Queue {
            self.up_next(m, tracks).len()
        } else if matches!(m.screen, Screen::Playlists | Screen::Playlist) {
            self.with_playlist_rows(m, tracks, <[Item]>::len)
        } else {
            self.rows(m, tracks).len()
        }
    }
    /// Lists that wrap around only when Wrap Around is Everywhere: the
    /// library, the queue, playlists, stations and the time zones.
    fn is_long_list(s: Screen) -> bool {
        Self::is_catalog(s)
            || matches!(
                s,
                Screen::Queue
                    | Screen::Playlists
                    | Screen::Playlist
                    | Screen::RadioStations
                    | Screen::TimeZone
                    | Screen::Audiobooks
                    | Screen::Book
            )
    }

    fn is_catalog(s: Screen) -> bool {
        matches!(
            s,
            Screen::Albums
                | Screen::Artists
                | Screen::Genres
                | Screen::Folders
                | Screen::Album
                | Screen::Artist
                | Screen::Tracks
                | Screen::Search
        )
    }
    pub(crate) fn row_at(&self, m: &AppModel, tracks: &[Track], index: usize) -> Option<Item> {
        let mut item = self.row_at_plain(m, tracks, index)?;
        // Select Songs: chosen rows carry a check (skins draw it as one).
        if let Some(on) = self.selection_mark(m, &item.key) {
            item.kind = RowKind::Check(on);
        }
        Some(item)
    }
    fn row_at_plain(&self, m: &AppModel, tracks: &[Track], index: usize) -> Option<Item> {
        if Self::is_catalog(m.screen) {
            self.track_rows_page(m, tracks, index, 1).pop()
        } else if m.screen == Screen::Queue {
            self.up_next_item(m, tracks, &self.up_next(m, tracks), index)
        } else if matches!(m.screen, Screen::Playlists | Screen::Playlist) {
            self.with_playlist_rows(m, tracks, |rows| rows.get(index).cloned())
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
        if Self::is_catalog(m.screen) || m.screen == Screen::Queue {
            (offset..offset + count)
                .filter_map(|i| self.row_at(m, tracks, i))
                .collect()
        } else if matches!(m.screen, Screen::Playlists | Screen::Playlist) {
            self.with_playlist_rows(m, tracks, |rows| {
                rows.iter().skip(offset).take(count).cloned().collect()
            })
        } else {
            self.rows(m, tracks)
                .into_iter()
                .skip(offset)
                .take(count)
                .collect()
        }
    }
    fn queue_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        let layout = self.up_next(m, tracks);
        (0..layout.len())
            .filter_map(|i| self.up_next_item(m, tracks, &layout, i))
            .collect()
    }

    pub(crate) fn bluetooth_summary(&self) -> String {
        if !self.bluetooth.available {
            String::new()
        } else if !self.bluetooth.powered {
            "Off".into()
        } else if let Some(device) = self.bluetooth.devices.iter().find(|d| d.usable_audio()) {
            device.name.clone()
        } else {
            "On".into()
        }
    }
    pub(crate) fn wifi_summary(&self) -> String {
        if !self.wifi.available {
            String::new()
        } else if !self.wifi.powered {
            "Off".into()
        } else {
            match &self.wifi.status {
                WifiStatus::Connected(n) => n.clone(),
                WifiStatus::NoInternet(n) => format!("{n} · No internet"),
                WifiStatus::Connecting(_) | WifiStatus::GettingAddress(_) => "Connecting…".into(),
                WifiStatus::Disconnected => "Not connected".into(),
            }
        }
    }

    fn device(&self, key: Option<&str>) -> Option<&BluetoothDeviceView> {
        let path = key?.strip_prefix("bt_device:")?;
        self.bluetooth.devices.iter().find(|d| d.path == path)
    }

    fn context_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        if m.screen == Screen::NowPlaying {
            let Some(current) = m.current() else {
                return vec![];
            };
            if current.is_radio() {
                let key = if current.filename.is_empty() {
                    current.path.to_string_lossy().into_owned()
                } else {
                    current.filename.clone()
                };
                let saved = self.radio.favorites.contains(&key);
                let mut rows = vec![
                    Item::new(
                        if saved {
                            "Remove from Favorites"
                        } else {
                            "Add to Favorites"
                        },
                        "radio_favorite_current",
                    ),
                    Item::new("Stop Radio", "radio_stop"),
                ];
                if self.radio.now.is_some() {
                    rows.insert(1, Item::new("Station Info", "radio_info_current"));
                }
                return rows;
            }
            // Spotify keeps its own queue, shuffle and library.
            if current.is_spotify() {
                return vec![
                    Item::new("Spotify", "spotify_page"),
                    Item::new("Sleep Timer", "sleep_timer").with_secondary(sleep_label(m, false)),
                    Item::new("Stop Spotify", "radio_stop"),
                ];
            }
            // A book: its chapters; shuffle and repeat do not apply to it.
            if let Some((book, _)) = m.book_part() {
                return vec![
                    Item::new("Chapters", "book_chapters"),
                    Item::new("Speed", "book_speed")
                        .with_secondary(format!("{}×", speed_label(book.speed))),
                    Item::new("Sleep Timer", "sleep_timer").with_secondary(sleep_label(m, true)),
                    Item::new("Up Next", "show_queue"),
                ];
            }
            // The song after this one, named where Up Next opens.
            let next = m
                .queue
                .get(m.queue_position + 1)
                .filter(|_| m.settings.repeat != RepeatMode::Track)
                .map(|t| track_title(t).to_owned())
                .unwrap_or_default();
            return vec![
                Item::new("Up Next", "show_queue").with_secondary(next),
                Item::new("Sleep Timer", "sleep_timer").with_secondary(sleep_label(m, false)),
                Item::new("Shuffle", "toggle_shuffle")
                    .with_secondary(m.settings.shuffle_mode().label()),
                Item::new("Repeat", "cycle_repeat").with_secondary(repeat_label(m.settings.repeat)),
                self.favorite_row(m, tracks, self.current_index(m, tracks)),
                Item::new("Add to Playlist…", "add_playlist"),
                // Back to where the music came from (#25, #60): the Go to
                // row of that place, Go to Album's for a playlist or folder.
                match self.origin_label(m) {
                    Some(label) if label != "Go to Artist" => Item::new(label, "go_source"),
                    _ => Item::new("Go to Album", "go_album"),
                },
                match self.origin_label(m) {
                    Some(label @ "Go to Artist") => Item::new(label, "go_source"),
                    _ => Item::new("Go to Artist", "go_artist"),
                },
            ];
        }
        let selected_key = m.navigation.context_key.clone().or_else(|| {
            m.navigation
                .context_target
                .and_then(|target| self.row_at(m, tracks, target).map(|item| item.key))
        });
        let key = selected_key.as_deref();
        if key == Some("selection") {
            return self.selection_menu(m);
        }
        // A playlist, also among search results (#44).
        if matches!(m.screen, Screen::Playlists | Screen::Playlist)
            || key.is_some_and(|k| k.starts_with("playlist:"))
        {
            return self.playlist_context_rows(m, key.unwrap_or(""));
        }
        if let Some(book) = key
            .and_then(|k| k.strip_prefix("book:"))
            .map(str::to_owned)
            .or_else(|| (m.screen == Screen::Book).then(|| m.navigation.filter.clone()))
        {
            return self.book_context_rows(m, tracks, &book);
        }
        if let Some(index) = key
            .and_then(|key| key.strip_prefix("track:"))
            .and_then(|i| i.parse::<usize>().ok())
        {
            // Every song menu reads the same (8.36): Play Next, Add to Queue,
            // what this list can change, keep it, go to it, then many songs.
            let mut rows = vec![
                Item::new("Play Next", "play_next"),
                Item::new("Add to Queue", "add_queue"),
                self.favorite_row(m, tracks, Some(index)),
                Item::new("Add to Playlist…", "add_playlist"),
                Item::new("Go to Album", "go_album"),
                Item::new("Go to Artist", "go_artist"),
            ];
            if Self::selects(m.screen) {
                rows.push(Item::new("Select Songs…", "select_songs"));
            }
            return rows;
        }
        // An album, artist, genre or folder row, the search's Play All.
        if let Some(key) = key.filter(|k| Self::group_key(k)) {
            return Self::group_menu(key);
        }
        if matches!(m.screen, Screen::Album | Screen::Artist) && key == Some("play:collection") {
            let mut rows = vec![
                Item::new("Play", "collection_play"),
                Item::new("Shuffle", "collection_shuffle"),
                Item::new("Play Next", "collection_next"),
                Item::new("Add to Queue", "collection_queue"),
                Item::new("Add to Playlist…", "collection_playlist"),
            ];
            // An artist's albums, each whole, in random order.
            if m.screen == Screen::Artist {
                rows.insert(2, Item::new("Shuffle Albums", "collection_shuffle_albums"));
            }
            return rows;
        }
        match m.screen {
            Screen::Albums | Screen::Artists | Screen::Genres | Screen::Search => vec![],
            Screen::RadioStations => {
                let Some(station) = key
                    .and_then(|k| k.strip_prefix("station:"))
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| self.radio.stations.get(i))
                else {
                    return vec![];
                };
                vec![
                    Item::new("Play", "radio_play"),
                    Item::new(
                        if self.radio.is_favorite(station) {
                            "Remove from Favorites"
                        } else {
                            "Add to Favorites"
                        },
                        "radio_favorite",
                    ),
                    Item::new("Station Info", "radio_info"),
                ]
            }
            Screen::Queue => self.up_next_menu(m, key),
            Screen::Bluetooth => {
                let Some(device) = self.device(key) else {
                    return vec![];
                };
                let mut rows = vec![Item::new(
                    match device.operation() {
                        "pair" => "Pair",
                        "disconnect" => "Disconnect",
                        _ => "Connect",
                    },
                    "bt_connect",
                )];
                let current = matches!(&m.output, AudioOutput::Bluetooth(a) if a.eq_ignore_ascii_case(&device.address));
                if device.usable_audio() && !current {
                    rows.push(Item::new("Use for Audio", "bt_output"));
                }
                if device.usable_audio() && !self.bluetooth.codec_choices.is_empty() {
                    rows.push(
                        Item::new("Codec", "bt_codec")
                            .with_secondary(m.settings.codec_preference.label()),
                    );
                }
                if device.saved_pairing() {
                    rows.push(Item::new("Forget Device", "bt_forget"));
                }
                rows
            }
            Screen::Wifi => {
                let Some(id) = key.and_then(|k| k.strip_prefix("saved:")) else {
                    return vec![];
                };
                let joined = self
                    .wifi
                    .networks
                    .iter()
                    .find(|n| n.saved_id.map(|v| v.to_string()).as_deref() == Some(id))
                    .is_some_and(|n| self.wifi.status.network() == Some(n.ssid.as_str()));
                let secured = self
                    .wifi
                    .networks
                    .iter()
                    .find(|n| n.saved_id.map(|v| v.to_string()).as_deref() == Some(id))
                    .is_some_and(|n| n.secured == Some(true));
                let mut items = vec![if joined {
                    Item::new("Disconnect", "wifi_disconnect")
                } else {
                    Item::new("Connect", "wifi_connect")
                }];
                if secured {
                    items.push(Item::new("Change Password", "wifi_change_password"));
                }
                items.push(Item::new("Forget Network", "wifi_forget"));
                items
            }
            _ => vec![],
        }
    }

    fn queue_target(&self, m: &AppModel, key: Option<&str>) -> Option<usize> {
        key.and_then(|s| s.strip_prefix("queue:"))
            .and_then(|s| s.parse::<u64>().ok())
            .and_then(|id| {
                m.queue_entry_ids
                    .iter()
                    .position(|v| v.0 == id)
                    .or_else(|| m.queue_entry_ids.is_empty().then_some(id as usize))
            })
            .filter(|i| *i < m.queue.len())
    }

    fn usable_outputs(&self) -> impl Iterator<Item = &BluetoothDeviceView> {
        self.bluetooth.devices.iter().filter(|d| d.usable_audio())
    }

    fn modal_rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        match m.navigation.modal {
            Some(Modal::ContextMenu) => self.context_rows(m, tracks),
            Some(Modal::PlaylistPicker) => self.picker_rows(m),
            Some(Modal::OutputPicker) => {
                let mut rows = vec![Item::new("Headphone Jack", "output:wired").with_secondary(
                    if m.output == AudioOutput::Wired {
                        "Current"
                    } else {
                        ""
                    },
                )];
                rows.extend(self.usable_outputs().map(|d| {
                    let current = matches!(&m.output, AudioOutput::Bluetooth(a) if a.eq_ignore_ascii_case(&d.address));
                    Item::new(&d.name, format!("output:{}", d.path))
                        .with_secondary(if current { "Current" } else { "Bluetooth" })
                }));
                rows
            }
            Some(Modal::CodecPicker) => self
                .bluetooth
                .codec_choices
                .iter()
                .map(|c| {
                    Item::new(
                        if *c == CodecPreference::Auto {
                            "Automatic"
                        } else {
                            c.label()
                        },
                        format!("codec:{}", c.label()),
                    )
                    .with_secondary(if *c == m.settings.codec_preference {
                        "Preferred"
                    } else {
                        ""
                    })
                })
                .collect(),
            Some(Modal::EqBand(_)) => (-12..=12)
                .map(|gain| Item::new(format!("{gain:+} dB"), format!("eq_gain:{gain}")))
                .collect(),
            Some(Modal::DateField(field)) => date_time::picker(self, m, field).0,
            Some(Modal::SleepTimer) => {
                use reborn_core::SleepChoice;
                let set = m.sleep_timer.map(|t| t.choice);
                let book = m.book_part().is_some();
                SleepChoice::all()
                    .into_iter()
                    .map(|c| {
                        let (key, running) = match c {
                            SleepChoice::Off => ("sleep_set:off".to_owned(), String::new()),
                            SleepChoice::EndOfItem => ("sleep_set:end".to_owned(), "On".to_owned()),
                            SleepChoice::Minutes(n) => (
                                format!("sleep_set:{n}"),
                                sleep_minutes_left(m).map_or_else(String::new, |l| {
                                    format!("{} left", minutes_label(l))
                                }),
                            ),
                        };
                        // The running choice says so, a timed one how long it has left.
                        let detail = if set == Some(c) {
                            running
                        } else {
                            String::new()
                        };
                        Item::new(sleep_choice_label(c, book), key).with_secondary(detail)
                    })
                    .collect()
            }
            Some(Modal::Confirm(action)) => {
                let mut rows = vec![
                    Item::new("Cancel", "cancel"),
                    Item::new(confirm_label(action), "confirm"),
                ];
                if action == ConfirmAction::ReportsQuestion {
                    // A real answer either way, so the question is not asked again.
                    rows[0] = Item::new("No Thanks", "decline");
                }
                if let ConfirmAction::Platform(task) = action {
                    let u = &m.platform.snapshot.update;
                    let idle = m.platform.busy.is_none();
                    rows[1].enabled = idle
                        && match task {
                            PlatformTask::UpdateApply => u.can_install,
                            PlatformTask::UpdateCancel => u.can_cancel,
                            PlatformTask::UpdateRollback => u.can_rollback,
                            // Only the old storage the platform still reports waiting.
                            PlatformTask::InternalStorageSetup => matches!(
                                m.platform.snapshot.internal_setup,
                                reborn_core::platform::InternalSetup::Waiting { .. }
                            ),
                            // Only a card the platform marked (read-only or refused).
                            PlatformTask::SdRepair => m.platform.snapshot.storage.sd.repairable(),
                            _ => true,
                        };
                }
                rows
            }
            None => vec![],
        }
    }

    /// Focus the open menu's row with `key` (previews pick rows by name, so
    /// a menu's order can change under them); false when it has none.
    pub fn focus_menu_row(&self, m: &mut AppModel, tracks: &[Track], key: &str) -> bool {
        let at = self.modal_rows(m, tracks).iter().position(|r| r.key == key);
        if let Some(at) = at {
            m.navigation.modal_focus = at;
        }
        at.is_some()
    }

    pub(crate) fn modal_rows_public(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        self.modal_rows(m, tracks)
    }

    pub(crate) fn modal_copy(&self, m: &AppModel) -> (String, String) {
        let (title, body) = match m.navigation.modal {
            Some(Modal::OutputPicker) => ("Output", "Choose where music plays."),
            Some(Modal::CodecPicker) => (
                "Bluetooth Codec",
                "Your headphones use the closest codec they support.",
            ),
            Some(Modal::EqBand(index)) => {
                let defaults = reborn_core::flat_eq_bands();
                let bands = if m.settings.eq_bands.is_empty() {
                    &defaults
                } else {
                    &m.settings.eq_bands
                };
                let title = bands
                    .get(index)
                    .map(|b| pages::eq_frequency(b.frequency_hz))
                    .unwrap_or_else(|| "Equalizer".into());
                return (
                    title,
                    "Turn the wheel to choose gain. Select applies; Back cancels.".into(),
                );
            }
            Some(Modal::DateField(field)) => return date_time::picker_copy(field),
            Some(Modal::SleepTimer) => (
                "Sleep Timer",
                "Turn the wheel to choose. Select starts it; Back cancels.",
            ),
            Some(Modal::Confirm(action)) => confirm_copy(action),
            Some(Modal::PlaylistPicker) => ("Add to Playlist", "Songs are added at the end."),
            Some(Modal::ContextMenu) => {
                let key = m.navigation.context_key.as_deref();
                let named = match m.screen {
                    Screen::NowPlaying => Some("Now Playing".to_owned()),
                    Screen::Bluetooth => self.device(key).map(|d| d.name.clone()),
                    Screen::Wifi => key.and_then(|k| k.strip_prefix("saved:")).and_then(|id| {
                        self.wifi
                            .networks
                            .iter()
                            .find(|n| n.saved_id.map(|v| v.to_string()).as_deref() == Some(id))
                            .map(|n| n.ssid.clone())
                    }),
                    Screen::Queue => Some("Queue".to_owned()),
                    Screen::Playlists => key
                        .and_then(|k| k.strip_prefix("playlist:"))
                        .and_then(|i| i.parse::<usize>().ok())
                        .and_then(|i| self.playlists.lists.get(i))
                        .map(|p| p.name.clone()),
                    Screen::RadioStations => key
                        .and_then(|k| k.strip_prefix("station:"))
                        .and_then(|i| i.parse::<usize>().ok())
                        .and_then(|i| self.radio.stations.get(i))
                        .map(|s| s.name.clone()),
                    _ => None,
                };
                return (named.unwrap_or_else(|| "Options".into()), String::new());
            }
            None => ("", ""),
        };
        (title.into(), body.into())
    }

    pub fn action(&mut self, m: &mut AppModel, tracks: &[Track], action: Action) -> Effect {
        let before = (m.screen, m.navigation.filter.clone());
        let effect = self.action_inner(m, tracks, action);
        // Cover Flow and the album page hand the centre cover to each other.
        if m.settings.album_view == reborn_core::AlbumView::CoverFlow && !self.motion_fixed {
            let kind = match (before.0, m.screen) {
                (Screen::Albums, Screen::Album) => Some(flow::TransitionKind::Open),
                (Screen::Album, Screen::Albums) => Some(flow::TransitionKind::Close),
                _ => None,
            };
            if let Some(kind) = kind {
                self.transition.set(Some(flow::Transition {
                    kind,
                    start: Instant::now(),
                }));
            }
        }
        effect
    }

    fn action_inner(&mut self, m: &mut AppModel, tracks: &[Track], action: Action) -> Effect {
        if !m.screen_off {
            self.normalize(m, tracks);
        }
        // The one-step update owns the screen until it restarts the player
        // or ends; Select is its Cancel while the download runs.
        if self.update_run {
            return match action {
                Action::ScreenWake => Effect::ScreenWake,
                Action::ScreenSleep | Action::PowerShort => Effect::ScreenSleep,
                Action::VolumeUp => Effect::AdjustVolume(2),
                Action::VolumeDown => Effect::AdjustVolume(-2),
                Action::Select if update_cancellable(m) => Effect::CancelDownload,
                _ => Effect::None,
            };
        }
        // Installing/restoring the root must not compete with navigation,
        // shutdown or playback. The platform owns the resulting restart.
        if matches!(
            m.platform.busy,
            Some(PlatformTask::UpdateApply | PlatformTask::UpdateRollback)
        ) || m.platform.shutting_down.is_some()
        {
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
            Action::PowerShort => {
                if matches!(
                    m.playback,
                    reborn_core::PlaybackState::Playing | reborn_core::PlaybackState::Buffering
                ) {
                    return Effect::ScreenSleep;
                }
                // As stock: the screen goes off at once and a wake returns to
                // where the person was (the Sleep page showed "Preparing to
                // Sleep…" to a press during the preparation; Y2Linux F48).
                return Effect::Platform(reborn_core::PlatformTask::SleepRequest);
            }
            Action::PowerHold => {
                self.power_hold = Some(Instant::now());
                return Effect::None;
            }
            Action::PowerHoldCancel => {
                self.power_hold = None;
                return Effect::None;
            }
            Action::PlayPause => return Effect::TogglePlayback,
            Action::ShowNowPlaying
                if !m.screen_off
                    && m.navigation.modal.is_none()
                    && self.keyboard.is_none()
                    && self.pairing.is_none() =>
            {
                Self::go(m, Screen::NowPlaying, "");
                return Effect::None;
            }
            Action::PreviousTrack => return Effect::PreviousTrack,
            Action::NextTrack => return Effect::NextTrack,
            Action::SeekBackward => return Effect::Seek(-1),
            Action::SeekForward => return Effect::Seek(1),
            Action::VolumeUp => return Effect::AdjustVolume(2),
            Action::VolumeDown => return Effect::AdjustVolume(-2),
            _ => {}
        }
        if m.screen_off {
            return Effect::None;
        }
        if let Some(effect) = self.overlay_action(m, action) {
            return effect;
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
        if self.keyboard.is_some() {
            return self.entry(m, tracks, action);
        }
        // Up Next's song moving with the wheel; Select Songs.
        if let Some(effect) = self.list_mode_input(m, tracks, action) {
            return effect;
        }
        if m.screen == Screen::Playlist && m.navigation.modal.is_none() {
            if let Some(effect) = self.moving_action(m, tracks, action) {
                return effect;
            }
        }
        match action {
            Action::Home => {
                if m.navigation.modal.is_some() {
                    Self::back(m);
                } else {
                    Self::home(m);
                }
                return Effect::None;
            }
            Action::Back => {
                // The search's results: Back goes up one level, to typing
                // (#44); one section alone (See All) back to all of them.
                if m.screen == Screen::Search
                    && m.navigation.modal.is_none()
                    && catalog::search_parts(&m.navigation.filter).1.is_none()
                {
                    self.open_library_search(m, tracks);
                    return Effect::None;
                }
                Self::back(m);
                return Effect::None;
            }
            Action::ContextMenu => {
                self.open_context(m, tracks);
                return Effect::None;
            }
            Action::WheelClockwise(steps) => return self.wheel(m, tracks, steps as i32),
            Action::WheelCounterClockwise(steps) => return self.wheel(m, tracks, -(steps as i32)),
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
        if m.screen == Screen::NowPlaying && m.current().is_some() {
            // Select on Now Playing opens its options; the wheel is volume
            // (or seeks: Customize > Now Playing Wheel).
            Self::open_modal(m, Modal::ContextMenu);
            m.navigation.context_target = Some(0);
            return Effect::None;
        }
        if self.row_count(m, tracks) == 0 {
            return match m.screen {
                Screen::Queue => {
                    Self::home(m);
                    Effect::None
                }
                s if Self::is_catalog(s) && !m.library.scanning => Effect::ScanLibrary,
                _ => Effect::None,
            };
        }
        let Some(row) = self
            .row_at(m, tracks, m.navigation.focus)
            .filter(|row| row.enabled)
        else {
            return Effect::None;
        };
        self.select_row(m, tracks, &row.key.clone())
    }

    fn open_context(&mut self, m: &mut AppModel, tracks: &[Track]) {
        if m.navigation.modal.is_some() {
            return;
        }
        if m.screen == Screen::NowPlaying {
            if m.current().is_some() {
                Self::open_modal(m, Modal::ContextMenu);
                m.navigation.context_target = Some(0);
            }
            return;
        }
        let Some(row) = self
            .row_at(m, tracks, m.navigation.focus)
            .filter(|row| row.enabled)
        else {
            return;
        };
        m.navigation.context_target = Some(m.navigation.focus);
        m.navigation.context_key = Some(row.key);
        if self.context_rows(m, tracks).is_empty() {
            m.navigation.context_target = None;
            m.navigation.context_key = None;
        } else {
            Self::open_modal(m, Modal::ContextMenu);
        }
    }

    /// Context-specific wheel policy. Only long library lists honour the
    /// input layer's acceleration suggestion; every other context moves
    /// exactly one row, item or value per detent.
    fn wheel(&mut self, m: &mut AppModel, tracks: &[Track], suggested: i32) -> Effect {
        let direction = suggested.signum();
        if m.navigation.modal.is_none() && m.screen == Screen::NowPlaying && m.current().is_some() {
            // Customize > Now Playing Wheel (#34): Seek moves through the song
            // as holding Previous/Next does, small steps first. A station has
            // no place to move to: its wheel stays the volume.
            let seek = m.settings.now_playing_wheel == reborn_core::NowPlayingWheel::Seek
                && !m.current().is_some_and(Track::is_radio);
            return if seek {
                Effect::Seek(i64::from(direction))
            } else {
                Effect::AdjustVolume(WHEEL_VOLUME_STEP * direction as i8)
            };
        }
        // The country list (≈240) scrolls as fast as a long library list.
        let accelerate = m.navigation.modal.is_none()
            && (Self::is_catalog(m.screen) || m.screen == Screen::TimeZone)
            && self.row_count(m, tracks) >= ACCELERATED_LIST_MINIMUM;
        let now = Instant::now();
        // #30: while the letter shows, each detent goes to the next or the
        // previous letter, and the letter stays `FAST_SCROLL_LINGER` after
        // the last one: turning back after overshooting stays by letter.
        let lettered = accelerate && self.fast_scroll_until.is_some_and(|until| now < until);
        if let Some(target) = lettered
            .then(|| self.letter_step(m, tracks, direction > 0))
            .flatten()
        {
            self.fast_scroll_until = Some(now + FAST_SCROLL_LINGER);
            // At an end one row at a time: a rested wheel wraps as always.
            let delta = match target as i32 - m.navigation.focus as i32 {
                0 => direction,
                jump => jump,
            };
            self.move_focus(m, tracks, delta);
            return Effect::None;
        }
        if accelerate && suggested.abs() > 1 {
            self.fast_scroll_until = Some(now + FAST_SCROLL_LINGER);
        }
        self.move_focus(m, tracks, if accelerate { suggested } else { direction });
        Effect::None
    }

    /// Where a letter jump from the focused row lands (#30), in a list that
    /// shows its letter (`row_letter`); None elsewhere.
    fn letter_step(&self, m: &AppModel, tracks: &[Track], forward: bool) -> Option<usize> {
        if !Self::lettered(m) {
            return None;
        }
        let mut c = self.catalog.borrow_mut();
        self.ensure_catalog(&mut c, m, tracks);
        let by_artist = m.settings.album_order == reborn_core::AlbumOrder::Artist;
        c.letter_jump(tracks, m.navigation.focus, forward, m.screen, by_artist)
    }

    /// The A to Z lists that show the letter while scrolling fast: Songs
    /// (a genre's keep the library's order), Artists, Albums (a genre's too;
    /// #56: by artist, the artist's letter) and Folders.
    fn lettered(m: &AppModel) -> bool {
        m.navigation.modal.is_none()
            && match m.screen {
                Screen::Artists | Screen::Albums | Screen::Folders => true,
                Screen::Tracks => m.navigation.filter.is_empty(),
                _ => false,
            }
    }

    /// The letter row `index` files under in a lettered list (`initial`).
    pub(crate) fn row_letter(&self, m: &AppModel, tracks: &[Track], index: usize) -> Option<char> {
        if !Self::lettered(m) {
            return None;
        }
        let mut c = self.catalog.borrow_mut();
        self.ensure_catalog(&mut c, m, tracks);
        let by_artist = m.settings.album_order == reborn_core::AlbumOrder::Artist;
        c.letter(tracks, index, m.screen, by_artist)
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
        // A scale (an equalizer band's dB) never jumps from one end to the other.
        let scale = matches!(
            m.navigation.modal,
            Some(Modal::EqBand(_) | Modal::SleepTimer)
        );
        let wraps = match m.settings.wrap_around {
            _ if scale => false,
            WrapAround::Off => false,
            WrapAround::Menus => modal || !Self::is_long_list(m.screen),
            WrapAround::Everywhere => true,
        };
        // A long list wraps only from a single step taken at its end after a
        // rest; a short menu at once.
        let now = Instant::now();
        let rested = count < ACCELERATED_LIST_MINIMUM
            || self
                .last_step
                .is_none_or(|at| now.duration_since(at) >= WRAP_REST);
        self.last_step = Some(now);
        // Up Next's parts, for its headings below (before `focus` borrows).
        let up_next = (!modal && m.screen == Screen::Queue).then(|| self.up_next(m, tracks));
        let focus = if modal {
            &mut m.navigation.modal_focus
        } else {
            &mut m.navigation.focus
        };
        let last = count as i32 - 1;
        let target = *focus as i32 + delta;
        let at_end = (*focus == 0 && delta < 0) || (*focus as i32 == last && delta > 0);
        let wrap = wraps && rested && delta.abs() == 1 && at_end;
        let mut next = if wrap {
            if delta > 0 {
                0
            } else {
                last as usize
            }
        } else {
            target.clamp(0, last) as usize
        };
        if let Some(rows) = rows {
            let step = delta.signum();
            let mut skipped = 0;
            while !rows[next].enabled {
                let mut n = next as i32 + step;
                if wraps && rested && delta.abs() == 1 && (n < 0 || n > last) {
                    n = n.rem_euclid(count as i32);
                }
                skipped += 1;
                if n < 0 || n > last || skipped >= count {
                    return;
                }
                next = n as usize;
            }
        }
        if !modal && matches!(m.screen, Screen::Search | Screen::Queue) {
            // The search page's and Up Next's headings are not stops.
            let catalog = self.catalog.borrow();
            let heading = |i: usize| match &up_next {
                Some(layout) => layout.row(i).is_some_and(up_next::Row::heading),
                None => catalog
                    .rows
                    .get(i)
                    .is_some_and(|r| r.search_kind() == catalog::SearchKind::Heading),
            };
            while heading(next) {
                let n = next as i32 + delta.signum();
                if n < 0 || n > last {
                    next = *focus;
                    break;
                }
                next = n as usize;
            }
        }
        *focus = next;
        if !modal && self.native_search(m, tracks) {
            self.scroll_search(m, tracks);
        } else if !modal && self.native_header_page(m, tracks) {
            let (lead, count) = (self.header_lead(m, tracks), self.row_count(m, tracks));
            m.navigation.scroll =
                components::header_list_first(next, lead, count, m.navigation.scroll);
        } else if !modal {
            let visible = self.visible_count(m, tracks);
            if next < m.navigation.scroll {
                m.navigation.scroll = next;
            } else if next >= m.navigation.scroll + visible {
                m.navigation.scroll = next + 1 - visible;
            }
        }
    }

    fn select_row(&mut self, m: &mut AppModel, tracks: &[Track], key: &str) -> Effect {
        if key == "back" {
            Self::back(m);
            return Effect::None;
        }
        if let Some(book) = key.strip_prefix("book:") {
            Self::go(m, Screen::Book, book);
            return Effect::LoadChapters(book.to_owned());
        }
        if m.screen == Screen::Book {
            let start = match key {
                "book_resume" => Some(reborn_core::BookStart::Resume),
                "book_restart" => Some(reborn_core::BookStart::Beginning),
                _ => key
                    .strip_prefix("chapter:")
                    .and_then(|n| n.parse().ok())
                    .map(reborn_core::BookStart::Chapter),
            };
            if let Some(start) = start {
                let book = m.navigation.filter.clone();
                Self::go(m, Screen::NowPlaying, "");
                return Effect::PlayBook(book, start);
            }
        }
        if key == "home" {
            Self::home(m);
            return Effect::None;
        }
        if key.starts_with("value:") {
            Self::go(m, Screen::ValueDetail, key.trim_start_matches("value:"));
            return Effect::None;
        }
        if let Some(id) = key.strip_prefix("diag:") {
            Self::go(m, Screen::DiagnosticSection, id);
            return Effect::None;
        }
        if key == "update_now" || key == "update_install" {
            if !update_charge_ok(m) {
                self.flash("Connect the charger or charge to half first");
                return Effect::None;
            }
            self.update_run = true;
            return Effect::Platform(if key == "update_now" {
                PlatformTask::UpdateStage
            } else {
                PlatformTask::UpdateApply
            });
        }
        if let Some(name) = key.strip_prefix("task:") {
            return Effect::Platform(platform_task(name));
        }
        if key == "report_toggle" {
            let on = m.platform.snapshot.reports.as_ref().and_then(|r| r.enabled) == Some(true);
            return Effect::Platform(PlatformTask::ReportEnable(!on));
        }
        if key == "report_info" {
            Self::go(m, Screen::ErrorReports, "info");
            return Effect::None;
        }
        if key == "report_save" {
            Self::go(m, Screen::ErrorReports, "save");
            return Effect::None;
        }
        if let Some(destination) = key
            .strip_prefix("report_dest:")
            .and_then(reborn_core::platform::ReportDestination::from_argument)
        {
            // A saved report always has everything, so there is no topic to
            // choose. Back to the Error Reports page (not the picker): it shows
            // the progress, then where the file went.
            while !(m.screen == Screen::ErrorReports && m.navigation.filter.is_empty())
                && !m.navigation.history.is_empty()
            {
                Self::back(m);
            }
            return Effect::Platform(PlatformTask::ReportSave(destination));
        }
        if key == "report_send" {
            // Every report holds everything: no topic to choose. The page shows
            // the progress, then the result.
            return Effect::Platform(PlatformTask::ReportSend);
        }
        if let Some(index) = key
            .strip_prefix("eq_band:")
            .and_then(|s| s.parse::<usize>().ok())
        {
            let defaults = reborn_core::flat_eq_bands();
            let bands = if m.settings.eq_bands.is_empty() {
                &defaults
            } else {
                &m.settings.eq_bands
            };
            if let Some(band) = bands.get(index) {
                let focus = (band.gain_db.round().clamp(-12., 12.) as i32 + 12) as usize;
                Self::open_modal(m, Modal::EqBand(index));
                m.navigation.modal_focus = focus;
            }
            return Effect::None;
        }
        if let Some(name) = key.strip_prefix("confirm:") {
            if let Some(action) = confirm_action(name) {
                Self::open_modal(m, Modal::Confirm(action));
            }
            return Effect::None;
        }
        if key == "eq_custom" {
            Self::go(m, Screen::Equalizer, "custom");
            return Effect::None;
        }
        if key == "eq_profile_saved" {
            return Effect::None; // already playing; its file is gone
        }
        if key == "eq_profile" {
            // The list shows what is on the storage now (files arrive over MTP).
            Self::go(m, Screen::Equalizer, "profile");
            return Effect::ScanEqProfiles;
        }
        if let Some(index) = key
            .strip_prefix("eq_profile_file:")
            .and_then(|s| s.parse::<usize>().ok())
        {
            return self
                .eq_profiles
                .get(index)
                .map_or(Effect::None, |f| Effect::ImportEqProfile(f.path.clone()));
        }
        if key == "date_time" {
            // From the clock as it reads now.
            date_time::begin(self, m);
            Self::go(m, Screen::DateTime, "");
            return Effect::None;
        }
        if key.starts_with("date_") && date_time::network_time(m) {
            return Effect::None;
        }
        if let Some(field) = key.strip_prefix("date_field:").and_then(date_time::field) {
            let focus = date_time::picker(self, m, field).1;
            Self::open_modal(m, Modal::DateField(field));
            m.navigation.modal_focus = focus;
            return Effect::None;
        }
        if key == "date_set" {
            if m.platform.busy.is_some() {
                self.flash("Try again in a moment.");
                return Effect::None;
            }
            return match date_time::instant(self, m) {
                Ok(utc) => {
                    // Back to Display, whose row shows the clock it sets.
                    self.date_draft = None;
                    Self::back(m);
                    Effect::Platform(PlatformTask::SetClock(utc))
                }
                Err(why) => {
                    self.flash(why);
                    Effect::None
                }
            };
        }
        if key == "time_zone" {
            // Open at the choice in use.
            Self::go(m, Screen::TimeZone, "");
            m.navigation.focus = time_zone::focus(self, m);
            return Effect::None;
        }
        if let Some(code) = key.strip_prefix("tz_country:") {
            let Some((_, _, zones)) = reborn_core::clock::countries()
                .iter()
                .find(|(c, _, _)| *c == code)
            else {
                return Effect::None;
            };
            // One zone: chosen at once; several: the country's cities.
            if let Some(zone) = time_zone::single_zone(zones) {
                return Effect::SetTimeZone(zone.into());
            }
            Self::go(m, Screen::TimeZone, format!("country:{code}"));
            m.navigation.focus = time_zone::focus(self, m);
            return Effect::None;
        }
        if key == "version" {
            self.version_taps = self.version_taps.saturating_add(1);
            if self.version_taps >= VERSION_TAPS {
                self.version_taps = 0;
                Self::go(m, Screen::SettingsSystem, "");
            } else if self.version_taps >= 3 {
                self.flash(format!(
                    "{} more taps for the service menu",
                    VERSION_TAPS - self.version_taps
                ));
            }
            return Effect::None;
        }
        if key == "shuffle:collection" {
            return self.play_filtered(m, tracks, ShuffleMode::Songs);
        }
        if key == "playlists" {
            Self::go(m, Screen::Playlists, "");
            self.playlists.loading = !self.playlists.loaded;
            return Effect::Playlist(Box::new(reborn_core::PlaylistRequest::Refresh));
        }
        if matches!(m.screen, Screen::Playlists | Screen::Playlist) {
            return self.playlist_select(m, tracks, key);
        }
        if key == "search" || key == catalog::SEARCH_FIELD {
            self.open_library_search(m, tracks);
            // The playlists are searched too: read them if not yet.
            if !self.playlists.loaded && !self.playlists.loading {
                self.playlists.loading = true;
                return Effect::Playlist(Box::new(reborn_core::PlaylistRequest::Refresh));
            }
            return Effect::None;
        }
        if key == catalog::SEARCH_PLAY_ALL {
            return self.play_filtered(m, tracks, ShuffleMode::Off);
        }
        if let Some(screen) = route(key) {
            Self::go(m, screen, "");
            if screen == Screen::Balance {
                // 21 rows: open on the current balance, not Left +10 dB.
                m.navigation.focus =
                    (m.settings.balance + reborn_core::MAX_BALANCE).max(0) as usize;
            }
            // The picker lists what is installed now (skins arrive over MTP).
            return if screen == Screen::Theme {
                Effect::ScanThemes
            } else {
                Effect::None
            };
        }
        if let Some(effect) = self.setting(m, key) {
            Self::keep_home_focus(m, tracks, &effect, self.spotify.available);
            return effect;
        }
        match m.screen {
            Screen::Artists => Self::go(m, Screen::Artist, key),
            Screen::Genres => Self::go(m, Screen::Albums, key),
            // A genre's albums start with all of its songs.
            Screen::Albums if key == "all_songs" => {
                Self::go(m, Screen::Tracks, m.navigation.filter.clone())
            }
            Screen::Albums => Self::go(m, Screen::Album, key),
            Screen::Folders if !key.starts_with("track:") => Self::go(m, Screen::Folders, key),
            // An album, or on a genre's album its whole tracklist (#50).
            Screen::Artist | Screen::Album if key.starts_with("album:") => {
                Self::go(m, Screen::Album, key)
            }
            // See All: that section alone, Back returns (#44).
            Screen::Search if key.starts_with(catalog::SEARCH_SEE_ALL) => {
                let (query, _) = catalog::search_parts(&m.navigation.filter);
                let section = &key[catalog::SEARCH_SEE_ALL.len()..];
                let filter = format!("{query}\u{1f}{section}");
                Self::go(m, Screen::Search, filter);
            }
            Screen::Search if key.starts_with("playlist:") => {
                let path = key
                    .strip_prefix("playlist:")
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| self.playlists.lists.get(i))
                    .map(|p| p.path.to_string_lossy().into_owned());
                if let Some(path) = path {
                    Self::go(m, Screen::Playlist, path);
                }
            }
            Screen::Search if key.starts_with("artist:") => Self::go(m, Screen::Artist, key),
            Screen::Search if key.starts_with("album:") => Self::go(m, Screen::Album, key),
            Screen::Search => return self.play_track(m, tracks, key),
            Screen::Artist | Screen::Album | Screen::Tracks | Screen::Folders => {
                if key == "artist_albums" {
                    Self::go(m, Screen::Albums, m.navigation.filter.clone());
                    return Effect::None;
                }
                if key.starts_with("track:") {
                    return self.play_track(m, tracks, key);
                }
                if key == "play:collection" {
                    return self.play_filtered(m, tracks, ShuffleMode::Off);
                }
            }
            Screen::Queue => {
                // A song from the history plays now, before what was playing.
                if let Some(lib) = key.strip_prefix("hist:").and_then(|v| v.parse().ok()) {
                    Self::go(m, Screen::NowPlaying, "");
                    return Effect::PlayNow(lib);
                }
                if let Some(index) = self.queue_target(m, Some(key)) {
                    Self::go(m, Screen::NowPlaying, "");
                    if index == m.queue_position {
                        return Effect::None;
                    }
                    return Effect::PlayQueue(index);
                }
            }
            Screen::Bluetooth => return self.bluetooth_select(m, key),
            Screen::Wifi => return self.wifi_select(m, key),
            Screen::Radio | Screen::RadioStations | Screen::RadioCategories | Screen::RadioInfo => {
                return self.radio_select(m, key)
            }
            Screen::Spotify | Screen::SpotifyList => return self.spotify_select(m, key),
            _ => {}
        }
        Effect::None
    }

    /// Open Spotify list `key` titled `title`; loaded unless it is an
    /// artist's menu (nothing to fetch) or already here.
    fn spotify_open(&mut self, m: &mut AppModel, key: String, title: &str) -> Effect {
        let (kind, _) = spotify_list_kind(&key);
        let list = self.spotify.lists.entry(key.clone()).or_default();
        list.title = title.into();
        Self::go(m, Screen::SpotifyList, format!("{key}\u{1f}{title}"));
        if kind == "artist" || (!list.rows.is_empty() && list.error.is_none()) {
            return Effect::None;
        }
        list.loading = true;
        list.error = None;
        Effect::Spotify(reborn_core::SpotifyRequest::Load(key))
    }

    fn spotify_select(&mut self, m: &mut AppModel, key: &str) -> Effect {
        use reborn_core::SpotifyRequest as R;
        match key {
            "spotify_connect" => return Effect::Spotify(R::Connect),
            "spotify_cancel" => return Effect::Spotify(R::Cancel),
            "spotify_disconnect" => return Effect::Spotify(R::Disconnect),
            "spotify_wifi" => {
                Self::go(m, Screen::Wifi, "");
                return Effect::None;
            }
            "spotify_now" => {
                Self::go(m, Screen::NowPlaying, "");
                return Effect::None;
            }
            "spotify_more" => {
                let list = spotify_filter(&m.navigation.filter).0.to_owned();
                if let Some(l) = self.spotify.lists.get_mut(&list) {
                    l.loading = true;
                }
                return Effect::Spotify(R::More(list));
            }
            "spotify_retry" => {
                let list = spotify_filter(&m.navigation.filter).0.to_owned();
                if let Some(l) = self.spotify.lists.get_mut(&list) {
                    l.loading = true;
                    l.error = None;
                }
                return Effect::Spotify(R::Load(list));
            }
            "spotify_play_all" => {
                let list = spotify_filter(&m.navigation.filter).0.to_owned();
                Self::go(m, Screen::NowPlaying, "");
                return Effect::Spotify(R::PlayAll(list));
            }
            _ => {}
        }
        if let Some(key) = key.strip_prefix("spotify_open:") {
            let title = match key {
                "playlists" => "Playlists",
                "liked-songs" => "Liked Songs",
                "saved-albums" => "Albums",
                "followed-artists" => "Artists",
                _ => "Spotify",
            };
            return self.spotify_open(m, key.to_owned(), title);
        }
        // An artist's own menu: (list prefix, title).
        if let Some((what, id)) = key
            .strip_prefix("spotify_artist:")
            .and_then(|k| k.split_once(':'))
        {
            let artist = spotify_filter(&m.navigation.filter).1.to_owned();
            let title = match what {
                "artist-top-tracks" => format!("{artist}: Top Songs"),
                "artist-albums" => format!("{artist}: Albums"),
                _ => format!("{artist}: All Songs"),
            };
            return self.spotify_open(m, format!("{what}:{id}"), &title);
        }
        let Some(index) = key
            .strip_prefix("spotify_row:")
            .and_then(|i| i.parse::<usize>().ok())
        else {
            return Effect::None;
        };
        let list = spotify_filter(&m.navigation.filter).0.to_owned();
        let Some(row) = self
            .spotify
            .lists
            .get(&list)
            .and_then(|l| l.rows.get(index))
            .cloned()
        else {
            return Effect::None;
        };
        match spotify_list_kind(&list).0 {
            "playlists" => self.spotify_open(m, format!("playlist-tracks:{}", row.id), &row.title),
            "saved-albums" | "artist-albums" => {
                self.spotify_open(m, format!("album-tracks:{}", row.id), &row.title)
            }
            "followed-artists" => self.spotify_open(m, format!("artist:{}", row.id), &row.title),
            _ => {
                Self::go(m, Screen::NowPlaying, "");
                Effect::Spotify(R::Play { list, index })
            }
        }
    }

    fn radio_select(&mut self, m: &mut AppModel, key: &str) -> Effect {
        let list = |m: &mut AppModel, ui: &mut Self, title: &str, request: RadioRequest| {
            ui.radio.title = title.into();
            ui.radio.stations.clear();
            ui.radio.loading = true;
            ui.radio.error = None;
            ui.radio.last_request = Some(request.clone());
            Self::go(m, Screen::RadioStations, title);
            Effect::Radio(request)
        };
        let categories = |m: &mut AppModel, ui: &mut Self, genres: bool| {
            ui.radio.categories.clear();
            ui.radio.genres = genres;
            ui.radio.loading = true;
            ui.radio.error = None;
            let request = if genres {
                RadioRequest::Genres
            } else {
                RadioRequest::Countries
            };
            ui.radio.last_request = Some(request.clone());
            Self::go(
                m,
                Screen::RadioCategories,
                if genres { "Genres" } else { "Countries" },
            );
            Effect::Radio(request)
        };
        match key {
            "radio_favorites" => list(m, self, "Favorites", RadioRequest::Favorites),
            "radio_own" => list(m, self, "My Stations", RadioRequest::Own),
            "radio_popular" => list(m, self, "Popular", RadioRequest::Popular),
            "radio_countries" => categories(m, self, false),
            "radio_genres" => categories(m, self, true),
            "radio_search" => {
                self.keyboard = Some(keyboard::Keyboard::new(
                    keyboard::Purpose::RadioSearch,
                    self.radio_query.clone(),
                ));
                Effect::None
            }
            "radio_retry" => match self.radio.last_request.clone() {
                Some(request) => {
                    self.radio.loading = true;
                    self.radio.error = None;
                    Effect::Radio(request)
                }
                None => Effect::None,
            },
            "radio_wifi" => {
                Self::go(m, Screen::Wifi, "");
                Effect::None
            }
            "radio_now" => {
                Self::go(m, Screen::NowPlaying, "");
                Effect::None
            }
            "radio_info_play" => match self.radio.info.clone() {
                Some(station) => {
                    Self::go(m, Screen::NowPlaying, "");
                    Effect::Radio(RadioRequest::PlayStation(Box::new(station)))
                }
                None => Effect::None,
            },
            "radio_info_favorite" => match self.radio.info.clone() {
                Some(station) => {
                    Effect::Radio(RadioRequest::ToggleFavoriteStation(Box::new(station)))
                }
                None => Effect::None,
            },
            "radio_back" => {
                Self::back(m);
                Effect::None
            }
            k => {
                if let Some(i) = k
                    .strip_prefix("station:")
                    .and_then(|i| i.parse::<usize>().ok())
                {
                    if i < self.radio.stations.len() {
                        Self::go(m, Screen::NowPlaying, "");
                        return Effect::Radio(RadioRequest::Play(i));
                    }
                }
                if let Some(category) = k
                    .strip_prefix("category:")
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| self.radio.categories.get(i).cloned())
                {
                    let request = if self.radio.genres {
                        RadioRequest::Genre(category.id.clone())
                    } else {
                        RadioRequest::Country {
                            code: category.id.clone(),
                            name: category.name.clone(),
                        }
                    };
                    return list(m, self, &category.name, request);
                }
                Effect::None
            }
        }
    }

    /// Settings rows that change a value in place: one Select, one step.
    fn setting(&mut self, m: &mut AppModel, key: &str) -> Option<Effect> {
        Some(match key {
            "output" => {
                Self::open_modal(m, Modal::OutputPicker);
                Effect::None
            }
            "codec_preference" => {
                Self::open_modal(m, Modal::CodecPicker);
                Effect::None
            }
            "ldac_quality" => {
                let q = m.platform.snapshot.bluetooth_quality;
                if !q.ldac_supported {
                    return None;
                }
                Effect::Platform(PlatformTask::LdacQuality(q.requested_quality?.next()))
            }
            "sbc_quality" => {
                let q = m.platform.snapshot.bluetooth_quality;
                if !q.sbc_supported {
                    return None;
                }
                Effect::Platform(PlatformTask::SbcQuality(q.requested_sbc?.next()))
            }
            "aac_quality" => {
                let q = m.platform.snapshot.bluetooth_quality;
                if !q.aac_supported {
                    return None;
                }
                Effect::Platform(PlatformTask::AacQuality(q.requested_aac?.next()))
            }
            "aac_abr" => {
                let q = m.platform.snapshot.bluetooth_quality;
                if !q.aac_abr_supported {
                    return None;
                }
                Effect::Platform(PlatformTask::AacAbr(!q.requested_aac_abr?))
            }
            "ldac_abr" => {
                let q = m.platform.snapshot.bluetooth_quality;
                if !q.abr_supported {
                    return None;
                }
                Effect::Platform(PlatformTask::LdacAbr(!q.requested_abr?))
            }
            "screen_off" => Effect::ScreenSleep,
            "eq_enabled" => Effect::SetEqEnabled(!m.settings.eq_enabled),
            "eq_reset" => Effect::ResetEq,
            "replay_gain" => Effect::SetReplayGain(match m.settings.replay_gain {
                reborn_core::ReplayGainMode::Off => reborn_core::ReplayGainMode::Track,
                _ => reborn_core::ReplayGainMode::Off,
            }),
            k if k.starts_with("crossfade_set:") => {
                Effect::SetCrossfade(k["crossfade_set:".len()..].parse().ok()?)
            }
            k if k.starts_with("balance_set:") => {
                Effect::SetBalance(k["balance_set:".len()..].parse().ok()?)
            }
            k if k.starts_with("theme_set:") => {
                Effect::SetTheme(k["theme_set:".len()..].to_owned())
            }
            k if k.starts_with("timeout_set:") => {
                Effect::SetScreenTimeout(k["timeout_set:".len()..].parse().ok()?)
            }
            k if k.starts_with("tz_set:") => Effect::SetTimeZone(k["tz_set:".len()..].into()),
            "tz_auto" => Effect::SetTimeZoneAuto(!m.settings.zone_auto()),
            k if k.starts_with("home_pin:") => {
                let key = &k["home_pin:".len()..];
                let pinned = m.settings.home_pins.iter().any(|p| p == key);
                Effect::SetHomePin(key.into(), !pinned)
            }
            k if k.starts_with("home_show:") => {
                let key = &k["home_show:".len()..];
                Effect::SetHomeShown(key.into(), !m.settings.home_shows(key))
            }
            "output_set:wired" => Effect::Output(AudioOutput::Wired),
            k if k.starts_with("output_set:") => Effect::BluetoothDevice {
                path: k["output_set:".len()..].into(),
                operation: "output".into(),
            },
            k if k.starts_with("hp_level_set:") => {
                Effect::SetHpLevel(reborn_core::HpLevel::from_key(&k["hp_level_set:".len()..])?)
            }
            k if k.starts_with("dac_filter_set:") => Effect::SetDacFilter(
                reborn_core::DacFilter::from_key(&k["dac_filter_set:".len()..])?,
            ),
            k if k.starts_with("eq_preset:") => {
                let name = &k["eq_preset:".len()..];
                let (_, _, gains) = pages::EQ_PRESETS.iter().find(|(_, key, _)| *key == name)?;
                Effect::SetEqPreset(*gains)
            }
            "shuffle" => Effect::SetShuffle(m.settings.shuffle_mode().next()),
            "repeat" => Effect::SetRepeat(next_repeat(m.settings.repeat)),
            "gapless" => Effect::SetGapless(!m.settings.gapless_enabled),
            "scrobble_log" => Effect::SetScrobbleLog(!m.settings.scrobble_log),
            "lastfm_scrobble" => Effect::SetLastFmScrobble(!m.settings.lastfm_scrobble),
            "lastfm_now_playing" => Effect::SetLastFmNowPlaying(!m.settings.lastfm_now_playing),
            "lastfm_connect" => Effect::LastFm(reborn_core::LastFmRequest::Connect),
            "lastfm_cancel" => Effect::LastFm(reborn_core::LastFmRequest::Cancel),
            "lastfm_send" => Effect::LastFm(reborn_core::LastFmRequest::SendNow),
            "lastfm_disconnect" => Effect::LastFm(reborn_core::LastFmRequest::Disconnect),
            "lastfm_password" => {
                self.keyboard = Some(keyboard::Keyboard::new(keyboard::Purpose::LastFmUser, ""));
                Effect::None
            }
            "wrap_around" => Effect::SetWrapAround(m.settings.wrap_around.next()),
            "home_audiobooks" => Effect::SetAudiobooksOnHome(!m.settings.audiobooks_on_home),
            "screen_off_buttons" => {
                Effect::SetScreenOffButtons(m.settings.screen_off_buttons.next())
            }
            "haptics" => Effect::SetHaptics(m.settings.haptics.next()),
            "speaker" => Effect::SetSpeaker(!m.settings.speaker),
            "album_view" => Effect::SetAlbumView(m.settings.album_view.next()),
            "album_order" => Effect::SetAlbumOrder(m.settings.album_order.next()),
            "artists_list" => Effect::SetArtistsList(m.settings.artists_list.next()),
            "now_playing_wheel" => Effect::SetNowPlayingWheel(m.settings.now_playing_wheel.next()),
            "crossfade" => Effect::SetCrossfade(match m.settings.crossfade_ms {
                0 => 3_000,
                3_000 => 6_000,
                6_000 => 10_000,
                _ => 0,
            }),
            "brightness" => Effect::SetBrightness(next_brightness(m.settings.brightness)),
            "timeout" => Effect::SetScreenTimeout(match m.settings.screen_timeout_seconds {
                15 => 30,
                30 => 60,
                60 => 120,
                120 => 300,
                300 => 0,
                _ => 15,
            }),
            "scan_library" => Effect::ScanLibrary,
            "wifi_power" => Effect::WifiPower,
            "wifi_scan" => Effect::WifiScan,
            "bt_power" => Effect::BluetoothPower,
            "bt_scan" => Effect::BluetoothScan,
            _ => return None,
        })
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
        let effect = self.play_effect(m, tracks, index);
        Self::go(m, Screen::NowPlaying, "");
        effect
    }

    fn play_filtered(
        &mut self,
        m: &mut AppModel,
        tracks: &[Track],
        shuffle: ShuffleMode,
    ) -> Effect {
        let members = self.filtered_indices(m, tracks);
        let Some(&index) = members.first() else {
            return Effect::None;
        };
        let effect = Effect::PlayCollection {
            selected: index,
            members,
            shuffle,
            source: source_label(m),
            origin: page_origin(m),
        };
        Self::go(m, Screen::NowPlaying, "");
        effect
    }
    fn filtered_indices(&self, m: &AppModel, tracks: &[Track]) -> Vec<usize> {
        if matches!(
            m.screen,
            Screen::Album | Screen::Search | Screen::Tracks | Screen::Folders
        ) {
            // The page's own order (album, search results, Songs A to Z, a
            // folder's own songs by file name), not library order: Next
            // plays the row below (Y2Reborn #21).
            // The search: every song found, also those See All would show.
            let mut c = self.catalog.borrow_mut();
            self.ensure_catalog(&mut c, m, tracks);
            if m.screen == Screen::Search {
                return c.search_songs.clone();
            }
            return c.rows.iter().filter_map(catalog::Row::track).collect();
        }
        tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track_matches(track, &m.navigation.filter))
            .map(|(index, _)| index)
            .collect()
    }
    fn play_effect(&self, m: &AppModel, tracks: &[Track], selected: usize) -> Effect {
        let members = self.filtered_indices(m, tracks);
        if members.len() > 1 || !m.navigation.filter.is_empty() {
            Effect::PlayCollection {
                selected,
                members,
                shuffle: ShuffleMode::Off,
                source: source_label(m),
                origin: page_origin(m),
            }
        } else {
            Effect::Play(selected)
        }
    }

    fn bluetooth_select(&mut self, m: &mut AppModel, key: &str) -> Effect {
        let Some(device) = self.device(Some(key)) else {
            return Effect::None;
        };
        if device.connected && device.saved_pairing() {
            // Connected devices offer their actions instead of disconnecting
            // on a single press.
            m.navigation.context_target = Some(m.navigation.focus);
            m.navigation.context_key = Some(key.into());
            Self::open_modal(m, Modal::ContextMenu);
            return Effect::None;
        }
        let (path, name, operation) = (
            device.path.clone(),
            device.name.clone(),
            device.operation().to_owned(),
        );
        if matches!(operation.as_str(), "pair" | "connect") {
            self.bt_busy = Some((path.clone(), name, operation.clone(), Instant::now()));
        }
        Effect::BluetoothDevice { path, operation }
    }

    fn wifi_select(&mut self, m: &mut AppModel, key: &str) -> Effect {
        if let Some(id) = key.strip_prefix("saved:") {
            let joined = self.wifi.networks.iter().any(|n| {
                n.saved_id.map(|v| v.to_string()).as_deref() == Some(id)
                    && self.wifi.status.network() == Some(n.ssid.as_str())
            });
            if joined {
                m.navigation.context_target = Some(m.navigation.focus);
                m.navigation.context_key = Some(key.into());
                Self::open_modal(m, Modal::ContextMenu);
                return Effect::None;
            }
            // Its password failed: ask for the new one instead of trying it again.
            if let Some(ssid) = self.wifi_failed_ssid(id) {
                self.open_wifi_keyboard(ssid, "");
                return Effect::None;
            }
            return id.parse().map(Effect::WifiSaved).unwrap_or(Effect::None);
        }
        let Some(ssid) = key.strip_prefix("ssid:") else {
            return Effect::None;
        };
        match self
            .wifi
            .networks
            .iter()
            .find(|n| n.ssid == ssid)
            .and_then(|n| n.secured)
        {
            Some(false) => Effect::WifiConnect {
                ssid: ssid.into(),
                password: String::new(),
            },
            Some(true) => {
                self.open_wifi_keyboard(ssid.to_owned(), "");
                Effect::None
            }
            None => Effect::None,
        }
    }

    fn open_wifi_keyboard(&mut self, ssid: String, text: &str) {
        self.keyboard = Some(keyboard::Keyboard::new(
            keyboard::Purpose::WifiPassword { ssid },
            text,
        ));
    }

    /// The name of saved network `id` when its password failed.
    fn wifi_failed_ssid(&self, id: &str) -> Option<String> {
        let ssid = &self
            .wifi
            .networks
            .iter()
            .find(|n| n.saved_id.map(|v| v.to_string()).as_deref() == Some(id))?
            .ssid;
        let failed =
            self.wifi.problem == Some(WifiProblem::WrongPassword)
                && self.wifi.problem_network.as_deref() == Some(ssid.as_str())
                || self.wifi.join.as_ref().is_some_and(|j| {
                    j.ssid == *ssid && j.outcome == WifiJoinOutcome::WrongPassword
                });
        failed.then(|| ssid.clone())
    }

    /// Say how a join ended, once (called after each Wi-Fi observation). A
    /// wrong typed password reopens the keyboard with it and says so.
    pub fn wifi_join_changed(&mut self) {
        let Some(join) = self.wifi.join.clone() else {
            return;
        };
        if join.outcome == WifiJoinOutcome::Checking || join.attempt <= self.wifi_join_seen {
            return;
        }
        self.wifi_join_seen = join.attempt;
        let typed = self
            .wifi_typed
            .take()
            .filter(|(ssid, _)| *ssid == join.ssid)
            .map(|(_, password)| password);
        match join.outcome {
            WifiJoinOutcome::Joined => self.flash(format!("Connected to {}", join.ssid)),
            WifiJoinOutcome::WrongPassword => match typed {
                Some(password) => {
                    self.open_wifi_keyboard(join.ssid.clone(), &password);
                    if let Some(k) = self.keyboard.as_mut() {
                        k.note = "Wrong password".into();
                    }
                }
                None => self.flash(format!(
                    "Wrong password for {}. Select it to enter the new one",
                    join.ssid
                )),
            },
            WifiJoinOutcome::NotFound => self.flash(format!(
                "{} is not in range. Move closer and try again",
                join.ssid
            )),
            WifiJoinOutcome::Failed | WifiJoinOutcome::Checking => {
                self.flash(format!("Couldn't connect to {}. Try again", join.ssid))
            }
        }
    }

    fn modal_action(&mut self, m: &mut AppModel, tracks: &[Track]) -> Effect {
        let rows = self.modal_rows(m, tracks);
        let Some(row) = rows.get(m.navigation.modal_focus).filter(|r| r.enabled) else {
            return Effect::None;
        };
        let key = row.key.clone();
        match m.navigation.modal.clone() {
            Some(Modal::OutputPicker) => {
                Self::close_modal(m);
                match key.strip_prefix("output:") {
                    Some("wired") => Effect::Output(AudioOutput::Wired),
                    Some(path) => Effect::BluetoothDevice {
                        path: path.into(),
                        operation: "output".into(),
                    },
                    None => Effect::None,
                }
            }
            Some(Modal::CodecPicker) => {
                Self::close_modal(m);
                let preference = self
                    .bluetooth
                    .codec_choices
                    .iter()
                    .copied()
                    .find(|c| key == format!("codec:{}", c.label()));
                preference
                    .map(Effect::SetCodecPreference)
                    .unwrap_or(Effect::None)
            }
            Some(Modal::EqBand(index)) => {
                Self::close_modal(m);
                key.strip_prefix("eq_gain:")
                    .and_then(|s| s.parse::<i8>().ok())
                    .map(|gain_db| Effect::SetEqBandGain { index, gain_db })
                    .unwrap_or(Effect::None)
            }
            Some(Modal::SleepTimer) => {
                Self::close_modal(m);
                match key.strip_prefix("sleep_set:") {
                    Some("off") => Effect::SetSleepTimer(reborn_core::SleepChoice::Off),
                    Some("end") => Effect::SetSleepTimer(reborn_core::SleepChoice::EndOfItem),
                    Some(n) => n
                        .parse()
                        .map(|n| Effect::SetSleepTimer(reborn_core::SleepChoice::Minutes(n)))
                        .unwrap_or(Effect::None),
                    None => Effect::None,
                }
            }
            Some(Modal::DateField(field)) => {
                Self::close_modal(m);
                if let Some(n) = key.strip_prefix("date_value:").and_then(|s| s.parse().ok()) {
                    date_time::choose(self, m, field, n);
                    // Time: the hour, then the minute.
                    if let Some(next) = date_time::then(field) {
                        let focus = date_time::picker(self, m, next).1;
                        Self::open_modal(m, Modal::DateField(next));
                        m.navigation.modal_focus = focus;
                    }
                }
                Effect::None
            }
            Some(Modal::Confirm(confirm)) => {
                if key == "cancel" {
                    Self::close_modal(m);
                    return Effect::None;
                }
                let context = m.navigation.context_key.clone();
                Self::close_modal(m);
                match confirm {
                    ConfirmAction::ForgetBluetooth => context
                        .as_deref()
                        .and_then(|s| s.strip_prefix("bt_device:"))
                        .filter(|path| self.bluetooth.devices.iter().any(|d| d.path == *path))
                        .map(|path| Effect::BluetoothDevice {
                            path: path.into(),
                            operation: "forget".into(),
                        })
                        .unwrap_or(Effect::None),
                    ConfirmAction::ForgetWifi => context
                        .as_deref()
                        .and_then(|s| s.strip_prefix("saved:"))
                        .and_then(|s| s.parse().ok())
                        .map(Effect::WifiForget)
                        .unwrap_or(Effect::None),
                    ConfirmAction::ClearQueue => Effect::ClearQueue,
                    ConfirmAction::RebuildLibrary => Effect::RebuildLibrary,
                    ConfirmAction::ResetSettings => Effect::ResetSettings,
                    ConfirmAction::ForgetAllWifi => Effect::ForgetAllWifi,
                    ConfirmAction::ForgetAllBluetooth => Effect::ForgetAllBluetooth,
                    ConfirmAction::ClearCache => Effect::ClearCache,
                    ConfirmAction::DeletePlaylist => match self.playlists.target.take() {
                        Some(path) => {
                            Effect::Playlist(Box::new(reborn_core::PlaylistRequest::Delete(path)))
                        }
                        None => Effect::None,
                    },
                    ConfirmAction::PowerOff => Effect::PowerOff,
                    ConfirmAction::Reboot => Effect::Reboot,
                    ConfirmAction::Platform(task) => Effect::Platform(task),
                    ConfirmAction::ReportsQuestion => {
                        Effect::Platform(PlatformTask::ReportEnable(key == "confirm"))
                    }
                    ConfirmAction::LoudVolume => Effect::AllowLoudVolume,
                }
            }
            Some(Modal::ContextMenu) => self.context_action(m, tracks, &key),
            Some(Modal::PlaylistPicker) => self.picker_action(m, tracks, &key),
            None => Effect::None,
        }
    }

    /// A book's hold menu: Continue (or Play), Start Over, Mark as Finished
    /// (or Not Finished) and Forget Progress.
    fn book_context_rows(&self, m: &AppModel, tracks: &[Track], key: &str) -> Vec<Item> {
        let Some(book) = self.book(tracks, key) else {
            return vec![];
        };
        let saved = m.library.shelf.find(&book, tracks).map(|(_, p)| p.finished);
        let mut rows = vec![
            Item::new(
                if saved == Some(false) {
                    "Continue"
                } else {
                    "Play"
                },
                "book_menu_resume",
            ),
            Item::new("Start Over", "book_menu_restart"),
            Item::new(
                if saved == Some(true) {
                    "Mark as Not Finished"
                } else {
                    "Mark as Finished"
                },
                "book_menu_finish",
            ),
        ];
        if saved == Some(false) {
            rows.push(Item::new("Forget Progress", "book_menu_forget"));
        }
        rows
    }

    fn context_action(&mut self, m: &mut AppModel, tracks: &[Track], menu_key: &str) -> Effect {
        let target = m.navigation.context_target.unwrap_or(0);
        let source_key = m
            .navigation
            .context_key
            .clone()
            .or_else(|| self.row_at(m, tracks, target).map(|r| r.key))
            .unwrap_or_default();
        if menu_key == "sleep_timer" {
            // A picker (#29): the wheel chooses, Select starts it, Back leaves
            // the timer as it was. It opens on the timer set, else 30 min.
            let now = m
                .sleep_timer
                .map_or(reborn_core::SleepChoice::Minutes(30), |t| t.choice);
            let focus = reborn_core::SleepChoice::all()
                .iter()
                .position(|c| *c == now)
                .unwrap_or(0);
            Self::open_modal(m, Modal::SleepTimer);
            m.navigation.modal_focus = focus;
            return Effect::None;
        }
        if menu_key.starts_with("sel_") {
            return self.selection_effect(m, tracks, menu_key);
        }
        if menu_key.starts_with("group_") {
            return self.group_effect(m, tracks, &source_key, menu_key);
        }
        if menu_key == "book_speed" {
            // The menu stays open: each press steps the speed on.
            return m.book_part().map_or(Effect::None, |(book, _)| {
                Effect::SetBookSpeed(reborn_core::books::next_speed(book.speed))
            });
        }
        if menu_key == "book_chapters" {
            Self::close_modal(m);
            if let Some(book) = m.book.as_ref().map(|b| b.key.clone()) {
                Self::go(m, Screen::Book, book.clone());
                return Effect::LoadChapters(book);
            }
            return Effect::None;
        }
        if let Some(action) = menu_key.strip_prefix("book_menu_") {
            let book = source_key
                .strip_prefix("book:")
                .map(str::to_owned)
                .unwrap_or_else(|| m.navigation.filter.clone());
            Self::close_modal(m);
            let finished = self
                .book(tracks, &book)
                .and_then(|b| m.library.shelf.find(&b, tracks).map(|(_, p)| p.finished))
                .unwrap_or(false);
            return match action {
                "resume" | "restart" => {
                    Self::go(m, Screen::NowPlaying, "");
                    Effect::PlayBook(
                        book,
                        if action == "resume" {
                            reborn_core::BookStart::Resume
                        } else {
                            reborn_core::BookStart::Beginning
                        },
                    )
                }
                "finish" => Effect::SetBookFinished(book, !finished),
                "forget" => Effect::ForgetBook(book),
                _ => Effect::None,
            };
        }
        let queue_target = self.queue_target(m, Some(&source_key));
        let track_index = if m.screen == Screen::NowPlaying {
            m.current()
                .and_then(|current| tracks.iter().position(|track| track.id == current.id))
        } else {
            self.menu_song(m, tracks, &source_key)
        };
        let station = source_key
            .strip_prefix("station:")
            .and_then(|value| value.parse::<usize>().ok());
        if matches!(m.screen, Screen::Playlists | Screen::Playlist)
            || source_key.starts_with("playlist:")
        {
            if let Some(effect) = self.playlist_context_action(m, tracks, &source_key, menu_key) {
                return effect;
            }
        }
        match menu_key {
            "add_playlist" => {
                Self::close_modal(m);
                if let Some(t) = track_index {
                    self.open_picker(m, vec![t]);
                }
                return Effect::None;
            }
            "collection_playlist" => {
                Self::close_modal(m);
                let members = self.filtered_indices(m, tracks);
                self.open_picker(m, members);
                return Effect::None;
            }
            "fav_toggle" => {
                Self::close_modal(m);
                return match track_index {
                    Some(t) => self.toggle_favorite_track(m, tracks, t),
                    None => Effect::None,
                };
            }
            "save_queue" => {
                Self::close_modal(m);
                self.playlists.pending = m
                    .queue
                    .iter()
                    .filter_map(|q| {
                        tracks
                            .iter()
                            .position(|t| t.id == q.id && t.source_id == q.source_id)
                    })
                    .collect();
                if self.playlists.pending.is_empty() {
                    self.flash("The queue has no songs to save");
                } else {
                    self.keyboard =
                        Some(keyboard::Keyboard::new(keyboard::Purpose::PlaylistNew, ""));
                }
                return Effect::None;
            }
            _ => {}
        }
        match menu_key {
            "radio_play" if station.is_some() => {
                Self::close_modal(m);
                Self::go(m, Screen::NowPlaying, "");
                return Effect::Radio(RadioRequest::Play(station.unwrap_or(0)));
            }
            "radio_favorite" if station.is_some() => {
                Self::close_modal(m);
                return Effect::Radio(RadioRequest::ToggleFavorite(station.unwrap_or(0)));
            }
            "radio_favorite_current" => {
                Self::close_modal(m);
                return Effect::Radio(RadioRequest::ToggleFavoriteCurrent);
            }
            "radio_info" | "radio_info_current" => {
                let shown = if menu_key == "radio_info" {
                    station.and_then(|i| self.radio.stations.get(i)).cloned()
                } else {
                    self.radio.now.clone()
                };
                Self::close_modal(m);
                if let Some(shown) = shown {
                    self.radio.info = Some(shown);
                    Self::go(m, Screen::RadioInfo, "");
                }
                return Effect::None;
            }
            "radio_stop" => {
                Self::close_modal(m);
                return Effect::StopPlayback;
            }
            "spotify_page" => {
                Self::close_modal(m);
                Self::go(m, Screen::Spotify, "");
                return Effect::None;
            }
            "bt_forget" => {
                m.navigation.modal = Some(Modal::Confirm(ConfirmAction::ForgetBluetooth));
                m.navigation.modal_focus = 0;
                return Effect::None;
            }
            "wifi_change_password" => {
                let ssid = m
                    .navigation
                    .context_key
                    .as_deref()
                    .and_then(|k| k.strip_prefix("saved:"))
                    .and_then(|id| {
                        self.wifi
                            .networks
                            .iter()
                            .find(|n| n.saved_id.map(|v| v.to_string()).as_deref() == Some(id))
                    })
                    .map(|n| n.ssid.clone());
                Self::close_modal(m);
                if let Some(ssid) = ssid {
                    // Joined like a new network: kept only if it connects,
                    // and then it replaces the old entry.
                    self.open_wifi_keyboard(ssid, "");
                }
                return Effect::None;
            }
            "wifi_forget" => {
                m.navigation.modal = Some(Modal::Confirm(ConfirmAction::ForgetWifi));
                m.navigation.modal_focus = 0;
                return Effect::None;
            }
            "clear_queue" => {
                m.navigation.modal = Some(Modal::Confirm(ConfirmAction::ClearQueue));
                m.navigation.modal_focus = 0;
                return Effect::None;
            }
            "bt_codec" => {
                Self::open_modal(m, Modal::CodecPicker);
                return Effect::None;
            }
            _ => {}
        }
        Self::close_modal(m);
        match menu_key {
            "bt_connect" | "bt_output" => self
                .device(Some(&source_key))
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
            "wifi_connect" => source_key
                .strip_prefix("saved:")
                .and_then(|s| s.parse().ok())
                .map(Effect::WifiSaved)
                .unwrap_or(Effect::None),
            "wifi_disconnect" => Effect::WifiDisconnect,
            "play" => track_index.map(Effect::Play).unwrap_or(Effect::None),
            "play_next" => track_index.map(Effect::PlayNext).unwrap_or(Effect::None),
            "add_queue" => track_index.map(Effect::AddToQueue).unwrap_or(Effect::None),
            "collection_play" => self.play_filtered(m, tracks, ShuffleMode::Off),
            "collection_shuffle" => self.play_filtered(m, tracks, ShuffleMode::Songs),
            "collection_shuffle_albums" => self.play_filtered(m, tracks, ShuffleMode::Albums),
            "collection_next" | "collection_queue" => {
                let members = self.filtered_indices(m, tracks);
                if members.is_empty() {
                    Effect::None
                } else if menu_key == "collection_next" {
                    Effect::PlayNextCollection(members)
                } else {
                    Effect::AddToQueueCollection(members)
                }
            }
            "show_queue" => {
                Self::go(m, Screen::Queue, "");
                self.open_up_next(m, tracks);
                Effect::None
            }
            "toggle_shuffle" => Effect::SetShuffle(m.settings.shuffle_mode().next()),
            "cycle_repeat" => Effect::SetRepeat(next_repeat(m.settings.repeat)),
            "go_album" => {
                if let Some(track) = track_index.and_then(|i| tracks.get(i)) {
                    Self::go(m, Screen::Album, album_filter(track));
                }
                Effect::None
            }
            "go_artist" => {
                if let Some(track) = track_index.and_then(|i| tracks.get(i)) {
                    Self::go(m, Screen::Artist, artist_filter(track));
                }
                Effect::None
            }
            "go_source" => self.go_to_origin(m, tracks),
            "track_info" => {
                Self::go(
                    m,
                    Screen::TrackInfo,
                    track_index.map(|i| i.to_string()).unwrap_or_default(),
                );
                Effect::None
            }
            "queue_play" => queue_target.map(Effect::PlayQueue).unwrap_or(Effect::None),
            "queue_remove" => queue_target
                .map(Effect::QueueRemove)
                .unwrap_or(Effect::None),
            "queue_up" | "queue_down" => queue_target
                .map(|index| Effect::QueueMove {
                    index,
                    delta: if menu_key == "queue_up" { -1 } else { 1 },
                })
                .unwrap_or(Effect::None),
            "queue_next" => Self::entry_of(m, &source_key)
                .map(|id| Effect::QueueEntriesNext(vec![id]))
                .unwrap_or(Effect::None),
            "queue_last" => Self::entry_of(m, &source_key)
                .map(|id| Effect::QueueEntriesLast(vec![id]))
                .unwrap_or(Effect::None),
            "queue_move" => {
                if let Some(id) = Self::entry_of(m, &source_key) {
                    self.start_moving(id);
                }
                Effect::None
            }
            "select_songs" => {
                self.start_selection(m, &source_key);
                Effect::None
            }
            "clear_queued" => Effect::ClearQueued,
            _ => Effect::None,
        }
    }

    /// The keyboard has the buttons: see `keyboard` and the router's keyboard
    /// mode. Playback buttons never reach here while it is open.
    fn entry(&mut self, m: &mut AppModel, tracks: &[Track], action: Action) -> Effect {
        use keyboard::{Outcome, Purpose};
        let Some(k) = self.keyboard.as_mut() else {
            return Effect::None;
        };
        match k.action(action) {
            Outcome::Stay => Effect::None,
            Outcome::Edited => {
                if k.purpose == Purpose::LibrarySearch {
                    self.search_typed(m, tracks);
                }
                Effect::None
            }
            Outcome::Browse => {
                self.fold_search(m, tracks);
                Effect::None
            }
            Outcome::Cancel => {
                // Cancel or a held Back in the search leaves it, typed text
                // or not, to the screen it was opened from (#51).
                if self.keyboard.take().is_some_and(|k| k.live()) {
                    Self::leave_search(m);
                }
                Effect::None
            }
            Outcome::Done => {
                let text = k.text.trim().to_owned();
                match k.purpose.clone() {
                    purpose @ (Purpose::PlaylistNew | Purpose::PlaylistRename) => {
                        if text.is_empty() {
                            self.flash("Type a name for the playlist");
                            return Effect::None;
                        }
                        self.keyboard = None;
                        self.playlist_named(&purpose, text)
                    }
                    Purpose::WifiPassword { ssid } => {
                        // WPA passphrases keep their spaces.
                        if k.text.len() < 8 {
                            self.flash("Wi-Fi passwords have at least 8 characters");
                            return Effect::None;
                        }
                        let password = std::mem::take(&mut k.text);
                        self.keyboard = None;
                        self.wifi_typed = Some((ssid.clone(), password.clone()));
                        Effect::WifiConnect { ssid, password }
                    }
                    Purpose::LastFmUser => {
                        if text.is_empty() {
                            self.flash("Type your Last.fm username");
                            return Effect::None;
                        }
                        self.keyboard = Some(keyboard::Keyboard::new(
                            Purpose::LastFmPassword { username: text },
                            "",
                        ));
                        Effect::None
                    }
                    Purpose::LastFmPassword { username } => {
                        if k.text.is_empty() {
                            self.flash("Type your Last.fm password");
                            return Effect::None;
                        }
                        let password = std::mem::take(&mut k.text);
                        self.keyboard = None;
                        Effect::LastFm(reborn_core::LastFmRequest::Login { username, password })
                    }
                    Purpose::RadioSearch => {
                        if text.chars().count() < 2 {
                            self.flash("Type at least two letters");
                            return Effect::None;
                        }
                        self.keyboard = None;
                        self.radio_query = text.clone();
                        let title = format!("“{text}”");
                        self.radio.title = title.clone();
                        self.radio.stations.clear();
                        self.radio.loading = true;
                        self.radio.error = None;
                        self.radio.last_request = Some(RadioRequest::Search(text.clone()));
                        Self::go(m, Screen::RadioStations, &title);
                        Effect::Radio(RadioRequest::Search(text))
                    }
                    Purpose::LibrarySearch => {
                        if text.is_empty() {
                            self.flash("Type a name to search for");
                            return Effect::None;
                        }
                        self.fold_search(m, tracks);
                        Effect::None
                    }
                }
            }
        }
    }

    /// Now Playing's Go to row for the place the music was played from
    /// (#25, #60), while the song playing came from it (not one you queued)
    /// and that place is still there.
    fn origin_label(&self, m: &AppModel) -> Option<&'static str> {
        let origin = m.queue_origin.as_ref()?;
        if m.current_entry_id().is_none_or(|id| m.is_queued(id)) {
            return None;
        }
        Some(match origin.screen {
            Screen::Album => "Go to Album",
            Screen::Artist => "Go to Artist",
            Screen::Playlist
                if !self.playlists.loaded
                    || self
                        .playlists
                        .get(std::path::Path::new(&origin.filter))
                        .is_some() =>
            {
                "Go to Playlist"
            }
            Screen::Folders => "Go to Folder",
            Screen::Search => "Go to Search",
            Screen::Tracks if origin.filter.starts_with("genre:") => "Go to Genre",
            Screen::Tracks if origin.filter.is_empty() => "Go to Songs",
            _ => return None,
        })
    }

    /// Open the page the music was played from, the song playing focused.
    fn go_to_origin(&mut self, m: &mut AppModel, tracks: &[Track]) -> Effect {
        let Some(origin) = m.queue_origin.clone() else {
            return Effect::None;
        };
        let current = self.current_index(m, tracks);
        Self::go(m, origin.screen, origin.filter);
        if m.screen == Screen::Playlist && !self.playlists.loaded {
            // After a restart: the playlist shows once they are read.
            self.playlists.loading = true;
            return Effect::Playlist(Box::new(reborn_core::PlaylistRequest::Refresh));
        }
        let Some(current) = current else {
            return Effect::None;
        };
        let row = if m.screen == Screen::Playlist {
            let resolved = self
                .playlists
                .get(std::path::Path::new(&m.navigation.filter))
                .map(|p| self.resolved(tracks, p))
                .unwrap_or_default();
            self.with_playlist_rows(m, tracks, |rows| {
                rows.iter().position(|r| {
                    r.key
                        .strip_prefix("pl_entry:")
                        .and_then(|i| i.parse::<usize>().ok())
                        .is_some_and(|i| resolved.get(i).copied().flatten() == Some(current))
                })
            })
        } else if Self::is_catalog(m.screen) {
            let mut c = self.catalog.borrow_mut();
            self.ensure_catalog(&mut c, m, tracks);
            c.rows.iter().position(|r| r.track() == Some(current))
        } else {
            None
        };
        if let Some(row) = row {
            self.move_focus(m, tracks, row as i32);
        }
        Effect::None
    }

    /// The library index of the song playing.
    fn current_index(&self, m: &AppModel, tracks: &[Track]) -> Option<usize> {
        let current = m.current()?;
        tracks
            .iter()
            .position(|t| t.id == current.id && t.source_id == current.source_id)
    }

    /// The heart in a song's menu.
    fn favorite_row(&self, m: &AppModel, tracks: &[Track], index: Option<usize>) -> Item {
        let favorite = index.is_some_and(|i| self.is_favorite_track(m, tracks, i));
        Item::new(
            if favorite {
                "Remove from Favorites"
            } else {
                "Add to Favorites"
            },
            "fav_toggle",
        )
    }

    /// Open the search with its keys up: on the search page with its text,
    /// else a new, empty search (#44: a search
    /// left is not offered again). The search index is built here (once per
    /// library), not on the first letter.
    fn open_library_search(&mut self, m: &mut AppModel, tracks: &[Track]) {
        let k = if m.screen == Screen::Search {
            let text = catalog::search_parts(&m.navigation.filter).0.to_owned();
            let mut k = keyboard::Keyboard::new(keyboard::Purpose::LibrarySearch, text.clone());
            k.restore = Some(text);
            k
        } else {
            Self::go(m, Screen::Search, "");
            keyboard::Keyboard::new(keyboard::Purpose::LibrarySearch, "")
        };
        self.keyboard = Some(k);
        self.folding.borrow_mut().take();
        self.search_typed(m, tracks);
    }

    /// The search's text changed: the page under the keys searches it, and
    /// the field counts what it finds.
    pub(crate) fn search_typed(&mut self, m: &mut AppModel, tracks: &[Track]) {
        let Some(text) = self.keyboard.as_ref().map(|k| k.text.clone()) else {
            return;
        };
        m.navigation.filter = text.clone();
        m.navigation.focus = 0;
        m.navigation.scroll = 0;
        let (found, summary) = {
            let mut catalog = self.catalog.borrow_mut();
            self.ensure_catalog(&mut catalog, m, tracks);
            (catalog.found, catalog.summary.clone())
        };
        let blank = text.trim().is_empty();
        if let Some(k) = self.keyboard.as_mut() {
            k.found = if blank { 0 } else { found };
            k.note = if blank { String::new() } else { summary };
        }
    }

    /// Out of the search to the screen it was opened from, past its own
    /// levels (a section's See All is a search page too).
    fn leave_search(m: &mut AppModel) {
        while m.screen == Screen::Search && m.navigation.modal.is_none() {
            Self::back(m);
        }
    }

    /// Fold the search's keys away: the page lists everything found with
    /// the focus on the first result (on the field when nothing was found).
    fn fold_search(&mut self, m: &mut AppModel, tracks: &[Track]) {
        let Some(k) = self.keyboard.take() else {
            return;
        };
        let text = k.text.trim().to_owned();
        m.navigation.filter = text;
        m.navigation.scroll = 0;
        m.navigation.focus = {
            let mut catalog = self.catalog.borrow_mut();
            self.ensure_catalog(&mut catalog, m, tracks);
            catalog
                .rows
                .iter()
                .position(|r| r.search_kind() == catalog::SearchKind::Result)
                .unwrap_or(0)
        };
        self.scroll_search(m, tracks);
        self.start_fold(k);
    }

    fn start_fold(&self, k: keyboard::Keyboard) {
        if self.ui_motion {
            *self.folding.borrow_mut() = Some((Instant::now(), k));
        }
    }

    /// The search's keys folding away over its page, until they are gone.
    pub(crate) fn draw_folding(&self, c: &mut canvas::Canvas) {
        let mut folding = self.folding.borrow_mut();
        let Some((at, k)) = folding.as_ref() else {
            return;
        };
        let p = motion::ease_out(at.elapsed().as_secs_f32() / FOLD_S);
        if p >= 1. || self.keyboard.is_some() || !self.ui_motion {
            *folding = None;
            return;
        }
        keyboard::draw_folding(c, self, k, p);
    }

    /// The search page or Up Next in Reborn's own drawing, rows of
    /// differing heights (a skin's list scrolls by its own rows).
    fn native_search(&self, m: &AppModel, tracks: &[Track]) -> bool {
        matches!(m.screen, Screen::Search | Screen::Queue)
            && self
                .skin
                .as_deref()
                .and_then(|skin| screens::skin_layout(skin, self, m, tracks))
                .is_none()
    }

    /// An album, a playlist or a book drawn by Reborn: its header scrolls
    /// stays and its list scrolls (`components::header_list_first`).
    fn native_header_page(&self, m: &AppModel, tracks: &[Track]) -> bool {
        matches!(m.screen, Screen::Album | Screen::Playlist | Screen::Book)
            && self
                .skin
                .as_deref()
                .and_then(|skin| screens::skin_layout(skin, self, m, tracks))
                .is_none()
    }

    /// The music's levels as heard now (none while nothing plays).
    pub fn set_heard(&mut self, levels: Option<Levels>) {
        self.heard = levels;
    }

    /// The levels the wave shows this frame: they rise with the music at
    /// once and fall back slowly, as a meter does; none without music.
    pub(crate) fn wave_levels(&self) -> Option<Levels> {
        let target = self.heard?;
        let now = Instant::now();
        let mut shown = self.shown_levels.borrow_mut();
        let (last, at) = shown.unwrap_or((target, now));
        let dt = now.duration_since(at).as_secs_f32().min(0.25);
        let step = |from: f32, to: f32| {
            let rate = if to > from { 18. } else { 4. };
            from + (to - from) * (1. - (-rate * dt).exp())
        };
        let levels = Levels {
            bass: step(last.bass, target.bass),
            mid: step(last.mid, target.mid),
            high: step(last.high, target.high),
        };
        *shown = Some((levels, now));
        Some(levels)
    }

    /// A settings page's first row shown: kept until the focus reaches an
    /// edge, as every list (8.37).
    pub(crate) fn group_window(
        &self,
        m: &AppModel,
        focus: usize,
        visible: usize,
        count: usize,
    ) -> usize {
        let mut last = self.group_scroll.borrow_mut();
        let here = (m.screen, m.navigation.filter.as_str());
        let prev = last
            .as_ref()
            .filter(|(s, f, _)| (*s, f.as_str()) == here)
            .map_or(0, |(_, _, first)| *first);
        let first = prev
            .min(focus)
            .max((focus + 1).saturating_sub(visible))
            .min(count.saturating_sub(visible));
        *last = Some((m.screen, m.navigation.filter.clone(), first));
        first
    }

    /// The buttons in such a page's header (Play, Shuffle, Continue…): its
    /// first rows.
    pub(crate) fn header_lead(&self, m: &AppModel, tracks: &[Track]) -> usize {
        const BUTTONS: [&str; 6] = [
            "play:collection",
            "shuffle:collection",
            "pl_play",
            "pl_shuffle",
            "book_resume",
            "book_restart",
        ];
        (0..2)
            .take_while(|&i| {
                self.row_at(m, tracks, i)
                    .is_some_and(|r| BUTTONS.contains(&r.key.as_str()))
            })
            .count()
    }

    fn scroll_search(&self, m: &mut AppModel, tracks: &[Track]) {
        let heights = self.search_heights(m, tracks);
        let (focus, scroll) = (m.navigation.focus, m.navigation.scroll);
        m.navigation.scroll = if m.screen == Screen::Queue {
            views_search::window(&heights, focus, scroll)
        } else {
            let fixed = self
                .row_at(m, tracks, 0)
                .is_some_and(|r| r.key == catalog::SEARCH_FIELD);
            views_search::search_first(&heights, focus, scroll, fixed)
        };
    }

    /// Each search page (or Up Next) row's height.
    pub(crate) fn search_heights(&self, m: &AppModel, tracks: &[Track]) -> Vec<f32> {
        if m.screen == Screen::Queue {
            let layout = self.up_next(m, tracks);
            return (0..layout.len())
                .map(|i| {
                    if layout.row(i).is_some_and(up_next::Row::heading) {
                        views_search::HEADING_ROW
                    } else {
                        views_search::ROW
                    }
                })
                .collect();
        }
        let mut catalog = self.catalog.borrow_mut();
        self.ensure_catalog(&mut catalog, m, tracks);
        catalog
            .rows
            .iter()
            .map(|r| views_search::row_height(r.search_kind()))
            .collect()
    }

    /// What the search found, in words, and how many results.
    pub(crate) fn search_summary(&self, m: &AppModel, tracks: &[Track]) -> (String, usize) {
        let mut catalog = self.catalog.borrow_mut();
        self.ensure_catalog(&mut catalog, m, tracks);
        (catalog.summary.clone(), catalog.found)
    }

    /// The search page's first `n` results, each saying what it is: the
    /// preview above the keys, where there are no headings.
    pub(crate) fn search_preview(&self, m: &AppModel, tracks: &[Track], n: usize) -> Vec<Item> {
        let mut catalog = self.catalog.borrow_mut();
        self.ensure_catalog(&mut catalog, m, tracks);
        catalog
            .rows
            .iter()
            .filter(|r| r.search_kind() == catalog::SearchKind::Result)
            .take(n)
            .map(|r| {
                let mut item = r.item(tracks);
                item.secondary = match r {
                    catalog::Row::SearchSong(i) => {
                        format!("Song · {}", names_or_unknown(&tracks[*i].artist))
                    }
                    _ if item.key.starts_with("artist:") => format!("Artist · {}", item.secondary),
                    _ if item.key.starts_with("album:") => format!("Album · {}", item.secondary),
                    _ if item.secondary == "Audiobook" => item.secondary,
                    _ => format!("Audiobook · {}", item.secondary),
                };
                item
            })
            .collect()
    }

    pub fn draw(&self, m: &AppModel, tracks: &[Track], has_art: bool) -> Vec<Quad> {
        self.motion.begin_frame();
        glyph_cache::begin_frame();
        let frame = screens::draw(self, m, tracks, has_art);
        let frame = if self.ui_motion {
            self.slide_screens(m, frame)
        } else {
            frame
        };
        glyph_cache::end_frame(&frame);
        frame
    }

    /// A new screen slides in from the right over the last one (Back: the
    /// last one slides out to the right), 200 ms, eased out.
    fn slide_screens(&self, m: &AppModel, frame: Vec<Quad>) -> Vec<Quad> {
        let now = Instant::now();
        let here = (
            m.screen,
            m.navigation.filter.clone(),
            m.navigation.history.len(),
        );
        {
            let mut seen = self.seen.borrow_mut();
            if let Some((screen, filter, depth, old)) = seen.take() {
                // Typing changes the search page's text, not the page; a
                // section shown alone (See All) is a page of its own.
                let section = |f: &str| catalog::search_parts(f).1;
                let moved = screen != here.0
                    || (filter != here.1 && screen != Screen::Search)
                    || (screen == Screen::Search && section(&filter) != section(&here.1));
                // Cover Flow's cover flight is its own transition.
                if moved && !m.screen_off && self.transition.get().is_none() && !old.is_empty() {
                    *self.slide.borrow_mut() = Some(Slide {
                        started: now,
                        push: here.2 >= depth,
                        old,
                    });
                }
            }
            *seen = Some((here.0, here.1, here.2, frame.clone()));
        }
        let mut slide = self.slide.borrow_mut();
        let Some(s) = slide.as_ref() else {
            return frame;
        };
        let p = motion::ease_out(now.saturating_duration_since(s.started).as_secs_f32() / SLIDE_S);
        if p >= 1. || m.screen_off {
            *slide = None;
            return frame;
        }
        let mut old = s.old.clone();
        for q in &mut old {
            q.focus_target = false;
        }
        let mut new = frame;
        let w = theme::WIDTH;
        let mut out = Vec::with_capacity(old.len() + new.len());
        if s.push {
            motion::shift(&mut old, -w * 0.3 * p);
            motion::shift(&mut new, w * (1. - p));
            out.extend(old);
            out.extend(new);
        } else {
            motion::shift(&mut new, -w * 0.3 * (1. - p));
            motion::shift(&mut old, w * p);
            out.extend(new);
            out.extend(old);
        }
        out
    }

    /// List rows one screen shows: the skin's list, else Reborn's own.
    fn visible_count(&self, m: &AppModel, tracks: &[Track]) -> usize {
        self.skin
            .as_deref()
            .and_then(|skin| {
                let (key, _, _) = screens::skin_layout(skin, self, m, tracks)?;
                skin.list_rows(key)
            })
            .unwrap_or_else(|| screens::visible_rows_for(m.screen))
    }
}

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
        "spotify" => Screen::Spotify,
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
