#![forbid(unsafe_code)]

pub mod art;
mod boot;
mod canvas;
mod catalog;
pub mod charge;
mod components;
pub mod diagnostics;
mod flow;
mod fontdata;
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
mod views_coverflow;
mod views_library;
mod views_playing;
mod views_playlists;
mod views_radio;
mod views_settings;
mod views_system;

use reborn_core::{
    platform::{ChargingState, LowBattery, UsbTransfer, WifiProblem},
    Action, AppModel, AudioOutput, CodecPreference, ConfirmAction, Modal, NavigationFrame,
    PlatformTask, RadioRequest, RadioScan, RepeatMode, Screen, Track,
};
use reborn_graphics::Quad;
use std::{
    cell::RefCell,
    collections::HashSet,
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
type CountsCache = Option<((usize, usize), [usize; 4])>;
/// Library identity, album filter key and its detail line.
type DetailCache = Option<((usize, usize), String, String)>;

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiView {
    pub available: bool,
    pub powered: bool,
    pub scan: RadioScan,
    pub status: WifiStatus,
    pub problem: Option<WifiProblem>,
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
    /// The last station and library searches, offered again when reopened.
    radio_query: String,
    library_query: String,
    catalog: std::cell::RefCell<catalog::Catalog>,
    /// Accent colour taken from the current cover (0 = the default accent).
    pub accent: u32,
    /// The volume line is shown until this moment.
    pub volume_until: Option<Instant>,
    /// When the Power button reached the turn-off hold.
    pub power_hold: Option<Instant>,
    /// Fast list scrolling shows the current letter until this moment.
    pub fast_scroll_until: Option<Instant>,
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
    /// The skin in use; `None` is Reborn's own look.
    pub skin: Option<std::rc::Rc<skin::Skin>>,
    /// Installed skins, as last scanned for the picker.
    pub themes: Vec<skin::SkinInfo>,
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
        self.volume_until.is_some() || self.power_hold.is_some() || self.spinning || self.gliding()
    }

    /// Motion that wants the panel's full frame rate: Cover Flow's glide and
    /// the cover's flight, and (Beta 7.2) every eased value on its way, a
    /// screen sliding in and the playing wave. Spinners stay at the calm rate.
    pub fn gliding(&self) -> bool {
        self.flow.borrow().moving()
            || self.transition_progress().is_some()
            || (self.ui_motion
                && (self.motion.moving() || self.slide.borrow().is_some() || self.download_shown))
    }

    /// Motion a button started (a glide, a slide, Cover Flow): it earns the
    /// short Interactive CPU lease. The playing wave alone does not: its
    /// ≈3-ms frames fit the lowest clock (Beta 7.4: less heat on Now Playing).
    pub fn responding(&self) -> bool {
        self.flow.borrow().moving()
            || self.transition_progress().is_some()
            || (self.ui_motion && (self.motion.settling() || self.slide.borrow().is_some()))
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
        let length = m.current().map_or(u64::MAX, |t| t.duration_ms.max(1));
        (reported + ahead).min(length)
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
        let text = views_library::album_detail(m, tracks, key);
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

    /// A 0..1 phase for spinners, one turn about every 1.6 s. A frame that
    /// draws one marks live motion (as the playing wave), so a ring in view
    /// turns at the panel's rate; work with no spinner in view keeps the
    /// calm rate.
    pub fn spin_phase(&self) -> f32 {
        self.mark_live();
        if let Some(fixed) = self.spin_fixed {
            return fixed;
        }
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let elapsed = START.get_or_init(Instant::now).elapsed().as_millis() % 1600;
        elapsed as f32 / 1600.
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
                if usb == UsbTransfer::Copying && was != UsbTransfer::Copying {
                    self.flash("Copying files from the computer…");
                } else if attached(usb) && !attached(was) {
                    self.flash("Connected to a computer");
                }
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

    /// Artists, albums and songs in the library, recounted only when it changes.
    pub(crate) fn library_counts(&self, tracks: &[Track]) -> [usize; 4] {
        let stamp = crate::library_stamp(tracks);
        if let Some((seen, counts)) = *self.counts.borrow() {
            if seen == stamp {
                return counts;
            }
        }
        let mut artists = std::collections::BTreeSet::new();
        let mut albums = std::collections::BTreeSet::new();
        let mut genres = std::collections::BTreeSet::new();
        let mut songs = 0;
        for t in tracks.iter().filter(|t| t.online) {
            artists.insert(t.artist.as_str());
            albums.insert((t.album.as_str(), t.album_artist.as_str()));
            genres.insert(t.genre.as_str());
            songs += 1;
        }
        let counts = [artists.len(), albums.len(), songs, genres.len()];
        *self.counts.borrow_mut() = Some((stamp, counts));
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
            Screen::Queue => self.queue_rows(m),
            Screen::Playlists | Screen::Playlist => self.playlist_rows(m, tracks),
            Screen::Diagnostics => diagnostics::root(m),
            Screen::DiagnosticSection => diagnostics::section_rows(m, &m.navigation.filter),
            Screen::ValueDetail => vec![Item::new("Back", "back")],
            Screen::TextEntry | Screen::Pairing => vec![],
            _ => pages::page(self, m, tracks).rows,
        }
    }

    pub fn collection_track<'a>(&self, m: &AppModel, tracks: &'a [Track]) -> Option<&'a Track> {
        let mut catalog = self.catalog.borrow_mut();
        catalog.ensure(m, tracks);
        catalog
            .rows
            .iter()
            .find_map(|row| row.track().and_then(|i| tracks.get(i)))
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
        } else if matches!(m.screen, Screen::Playlists | Screen::Playlist) {
            self.with_playlist_rows(m, tracks, <[Item]>::len)
        } else {
            self.rows(m, tracks).len()
        }
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
        if Self::is_catalog(m.screen) {
            self.track_rows_page(m, tracks, index, 1).pop()
        } else if m.screen == Screen::Queue {
            m.queue.get(index).map(|t| {
                Item::new(
                    track_title(t),
                    format!(
                        "queue:{}",
                        m.queue_entry_ids
                            .get(index)
                            .map(|v| v.0)
                            .unwrap_or(index as u64)
                    ),
                )
                .with_secondary(display_or_unknown(&t.artist))
            })
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
    fn queue_rows(&self, m: &AppModel) -> Vec<Item> {
        (0..m.queue.len())
            .filter_map(|i| self.row_at(m, &[], i))
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
            return vec![
                Item::new("Up Next", "show_queue"),
                Item::new("Shuffle", "toggle_shuffle").with_secondary(if m.settings.shuffle {
                    "On"
                } else {
                    "Off"
                }),
                Item::new("Repeat", "cycle_repeat").with_secondary(repeat_label(m.settings.repeat)),
                self.favorite_row(m, tracks, self.current_index(m, tracks)),
                Item::new("Add to Playlist…", "add_playlist"),
                Item::new("Save Queue as Playlist", "save_queue"),
                Item::new("Go to Album", "go_album"),
                Item::new("Go to Artist", "go_artist"),
            ];
        }
        let selected_key = m.navigation.context_key.clone().or_else(|| {
            m.navigation
                .context_target
                .and_then(|target| self.row_at(m, tracks, target).map(|item| item.key))
        });
        let key = selected_key.as_deref();
        if matches!(m.screen, Screen::Playlists | Screen::Playlist) {
            return self.playlist_context_rows(m, key.unwrap_or(""));
        }
        if let Some(index) = key
            .and_then(|key| key.strip_prefix("track:"))
            .and_then(|i| i.parse::<usize>().ok())
        {
            return vec![
                Item::new("Play Next", "play_next"),
                Item::new("Add to Queue", "add_queue"),
                Item::new("Add to Playlist…", "add_playlist"),
                self.favorite_row(m, tracks, Some(index)),
                Item::new("Go to Album", "go_album"),
                Item::new("Go to Artist", "go_artist"),
            ];
        }
        if matches!(m.screen, Screen::Album | Screen::Artist) && key == Some("play:collection") {
            return vec![
                Item::new("Play", "collection_play"),
                Item::new("Shuffle", "collection_shuffle"),
                Item::new("Play Next", "collection_next"),
                Item::new("Add to Queue", "collection_queue"),
                Item::new("Add to Playlist…", "collection_playlist"),
            ];
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
            Screen::Queue => {
                let Some(i) = self.queue_target(m, key) else {
                    return vec![];
                };
                let mut rows = vec![
                    Item::new("Play Now", "queue_play"),
                    Item::new("Remove from Queue", "queue_remove"),
                    Item::new("Move Up", "queue_up"),
                    Item::new("Move Down", "queue_down"),
                    Item::new("Clear Upcoming", "clear_queue"),
                ];
                rows[1].enabled = i != m.queue_position;
                rows[2].enabled = i > m.queue_position.saturating_add(1);
                rows[3].enabled = i > m.queue_position && i + 1 < m.queue.len();
                rows[4].enabled = m.queue_position + 1 < m.queue.len();
                rows
            }
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
                vec![
                    if joined {
                        Item::new("Disconnect", "wifi_disconnect")
                    } else {
                        Item::new("Connect", "wifi_connect")
                    },
                    Item::new("Forget Network", "wifi_forget"),
                ]
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
            Some(Modal::Confirm(action)) => {
                let mut rows = vec![
                    Item::new("Cancel", "cancel"),
                    Item::new(confirm_label(action), "confirm"),
                ];
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
                            _ => true,
                        };
                }
                rows
            }
            None => vec![],
        }
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
                Self::go(m, Screen::Sleep, "");
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
            Action::SeekBackward => return Effect::Seek(-30_000),
            Action::SeekForward => return Effect::Seek(30_000),
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
            // Select on Now Playing opens its options; the wheel is volume.
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
            return Effect::AdjustVolume(WHEEL_VOLUME_STEP * direction as i8);
        }
        // The country list (≈240) scrolls as fast as a long library list.
        let accelerate = m.navigation.modal.is_none()
            && (Self::is_catalog(m.screen) || m.screen == Screen::TimeZone)
            && self.row_count(m, tracks) >= ACCELERATED_LIST_MINIMUM;
        if accelerate && suggested.abs() > 1 {
            self.fast_scroll_until = Some(Instant::now() + Duration::from_millis(700));
        }
        self.move_focus(m, tracks, if accelerate { suggested } else { direction });
        Effect::None
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
            return self.play_filtered(m, tracks, true);
        }
        if key == "playlists" {
            Self::go(m, Screen::Playlists, "");
            self.playlists.loading = !self.playlists.loaded;
            return Effect::Playlist(Box::new(reborn_core::PlaylistRequest::Refresh));
        }
        if matches!(m.screen, Screen::Playlists | Screen::Playlist) {
            return self.playlist_select(m, tracks, key);
        }
        if key == "search" || key == "search_edit" {
            self.open_library_search(m, tracks);
            return Effect::None;
        }
        if let Some(screen) = route(key) {
            Self::go(m, screen, "");
            // The picker lists what is installed now (skins arrive over MTP).
            return if screen == Screen::Theme {
                Effect::ScanThemes
            } else {
                Effect::None
            };
        }
        if let Some(effect) = self.setting(m, key) {
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
            Screen::Artist if key.starts_with("album:") => Self::go(m, Screen::Album, key),
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
                    return self.play_filtered(m, tracks, false);
                }
            }
            Screen::Queue => {
                if let Some(index) = self.queue_target(m, Some(key)) {
                    Self::go(m, Screen::NowPlaying, "");
                    return Effect::PlayQueue(index);
                }
            }
            Screen::Bluetooth => return self.bluetooth_select(m, key),
            Screen::Wifi => return self.wifi_select(m, key),
            Screen::Radio | Screen::RadioStations | Screen::RadioCategories | Screen::RadioInfo => {
                return self.radio_select(m, key)
            }
            _ => {}
        }
        Effect::None
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
            k if k.starts_with("eq_preset:") => {
                let name = &k["eq_preset:".len()..];
                let (_, _, gains) = pages::EQ_PRESETS.iter().find(|(_, key, _)| *key == name)?;
                Effect::SetEqPreset(*gains)
            }
            "shuffle" => Effect::SetShuffle(!m.settings.shuffle),
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
            "album_view" => Effect::SetAlbumView(match m.settings.album_view {
                reborn_core::AlbumView::CoverFlow => reborn_core::AlbumView::Grid,
                reborn_core::AlbumView::Grid => reborn_core::AlbumView::CoverFlow,
            }),
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

    fn play_filtered(&mut self, m: &mut AppModel, tracks: &[Track], shuffle: bool) -> Effect {
        let members = self.filtered_indices(m, tracks);
        let Some(&index) = members.first() else {
            return Effect::None;
        };
        let effect = Effect::PlayCollection {
            selected: index,
            members,
            shuffle,
        };
        Self::go(m, Screen::NowPlaying, "");
        effect
    }
    fn filtered_indices(&self, m: &AppModel, tracks: &[Track]) -> Vec<usize> {
        if m.screen == Screen::Album || m.screen == Screen::Search {
            // The page's own order (album, or search results), not library order.
            let mut c = self.catalog.borrow_mut();
            c.ensure(m, tracks);
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
                shuffle: false,
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
                self.keyboard = Some(keyboard::Keyboard::new(
                    keyboard::Purpose::WifiPassword { ssid: ssid.into() },
                    "",
                ));
                Effect::None
            }
            None => Effect::None,
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
                }
            }
            Some(Modal::ContextMenu) => self.context_action(m, tracks, &key),
            Some(Modal::PlaylistPicker) => self.picker_action(m, tracks, &key),
            None => Effect::None,
        }
    }

    fn context_action(&mut self, m: &mut AppModel, tracks: &[Track], menu_key: &str) -> Effect {
        let target = m.navigation.context_target.unwrap_or(0);
        let source_key = m
            .navigation
            .context_key
            .clone()
            .or_else(|| self.row_at(m, tracks, target).map(|r| r.key))
            .unwrap_or_default();
        let queue_target = self.queue_target(m, Some(&source_key));
        let track_index = if m.screen == Screen::NowPlaying {
            m.current()
                .and_then(|current| tracks.iter().position(|track| track.id == current.id))
        } else {
            source_key
                .strip_prefix("track:")
                .and_then(|value| value.parse().ok())
        };
        let station = source_key
            .strip_prefix("station:")
            .and_then(|value| value.parse::<usize>().ok());
        if matches!(m.screen, Screen::Playlists | Screen::Playlist) {
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
            "bt_forget" => {
                m.navigation.modal = Some(Modal::Confirm(ConfirmAction::ForgetBluetooth));
                m.navigation.modal_focus = 0;
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
            "collection_play" => self.play_filtered(m, tracks, false),
            "collection_shuffle" => self.play_filtered(m, tracks, true),
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
                Effect::None
            }
            "toggle_shuffle" => Effect::SetShuffle(!m.settings.shuffle),
            "cycle_repeat" => Effect::SetRepeat(next_repeat(m.settings.repeat)),
            "go_album" => {
                if let Some(track) = track_index.and_then(|i| tracks.get(i)) {
                    Self::go(m, Screen::Album, album_filter(track));
                }
                Effect::None
            }
            "go_artist" => {
                if let Some(track) = track_index.and_then(|i| tracks.get(i)) {
                    Self::go(m, Screen::Artist, format!("artist:{}", track.artist));
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
                    let text = k.text.clone();
                    self.show_live_results(tracks, &text);
                }
                Effect::None
            }
            Outcome::Open(index) => {
                let Some(key) = k.results.get(index).map(|item| item.key.clone()) else {
                    return Effect::None;
                };
                let text = k.text.trim().to_owned();
                self.keyboard = None;
                self.library_query = text.clone();
                // The full results come first, so Back from what opens returns
                // to them (with Edit Search on top).
                if m.screen == Screen::Search {
                    m.navigation.filter = text;
                } else {
                    Self::go(m, Screen::Search, text);
                }
                m.navigation.focus = 0;
                m.navigation.scroll = 0;
                if key.starts_with("artist:") {
                    Self::go(m, Screen::Artist, key);
                    Effect::None
                } else if key.starts_with("album:") {
                    Self::go(m, Screen::Album, key);
                    Effect::None
                } else {
                    self.play_track(m, tracks, &key)
                }
            }
            Outcome::Cancel => {
                self.keyboard = None;
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
                        self.keyboard = None;
                        self.library_query = text.clone();
                        // A new search replaces the results it was edited from.
                        if m.screen == Screen::Search {
                            m.navigation.filter = text;
                            m.navigation.focus = 0;
                            m.navigation.scroll = 0;
                        } else {
                            Self::go(m, Screen::Search, text);
                        }
                        Effect::None
                    }
                }
            }
        }
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

    /// Open the library search's keyboard with the last search. The search
    /// index is built here (once per library), not on the first letter.
    fn open_library_search(&mut self, m: &AppModel, tracks: &[Track]) {
        let text = if m.screen == Screen::Search {
            m.navigation.filter.clone()
        } else {
            self.library_query.clone()
        };
        self.keyboard = Some(keyboard::Keyboard::new(
            keyboard::Purpose::LibrarySearch,
            text.clone(),
        ));
        self.show_live_results(tracks, &text);
    }

    /// Search as the text is typed and show the results above the keys:
    /// artists, then albums, then songs (at most `LIVE_RESULTS`).
    fn show_live_results(&mut self, tracks: &[Track], text: &str) {
        const LIVE_RESULTS: usize = 60;
        let results = self.catalog.borrow_mut().index.search(tracks, text);
        let (note, items) = if text.trim().is_empty() {
            (String::new(), vec![])
        } else {
            let mut items = Vec::with_capacity(LIVE_RESULTS);
            for (artist, i, albums) in results.artists.iter().take(LIVE_RESULTS) {
                let mut item = Item::new(display_or_unknown(artist), format!("artist:{artist}"))
                    .with_secondary(format!(
                        "Artist · {}",
                        crate::components::count(*albums, "album", "albums")
                    ));
                item.art = Some(*i);
                items.push(item);
            }
            for &i in results.albums.iter().take(LIVE_RESULTS - items.len()) {
                let t = &tracks[i];
                let mut item = Item::new(display_or_unknown(&t.album), album_filter(t))
                    .with_secondary(format!(
                        "Album · {}",
                        display_or_unknown(if t.album_artist.is_empty() {
                            &t.artist
                        } else {
                            &t.album_artist
                        })
                    ));
                item.art = Some(i);
                items.push(item);
            }
            for &i in results.songs.iter().take(LIVE_RESULTS - items.len()) {
                let t = &tracks[i];
                let mut item = Item::new(track_title(t), format!("track:{i}"))
                    .with_secondary(format!("Song · {}", display_or_unknown(&t.artist)));
                item.art = Some(i);
                items.push(item);
            }
            (results.summary(), items)
        };
        if let Some(k) = self.keyboard.as_mut() {
            k.note = note;
            k.set_results(items);
        }
    }

    pub fn draw(&self, m: &AppModel, tracks: &[Track], has_art: bool) -> Vec<Quad> {
        self.motion.begin_frame();
        let frame = screens::draw(self, m, tracks, has_art);
        if self.ui_motion {
            self.slide_screens(m, frame)
        } else {
            frame
        }
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
                let moved = (screen, &filter) != (here.0, &here.1);
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
        "screen_timeout" => Screen::ScreenTimeout,
        "theme" => Screen::Theme,
        "library" => Screen::SettingsLibrary,
        "display" => Screen::SettingsDisplay,
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
        | "library_benchmark" | "internal_setup" => ConfirmAction::Platform(platform_task(name)),
        _ => return None,
    })
}

/// Red (cannot be undone): everything except platform checks, and erasing the old storage.
pub(crate) fn confirm_is_danger(action: ConfirmAction) -> bool {
    !matches!(action, ConfirmAction::Platform(task) if task != PlatformTask::InternalStorageSetup)
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
    use reborn_core::sort::{sort_initial, sort_key, SortAs};
    let kind = if screen == Screen::Artists {
        SortAs::Artist
    } else {
        SortAs::Title
    };
    sort_initial(&sort_key(label, kind))
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
/// Identifies one published library: a new scan publishes a new list, so
/// caches built from the songs (search, playlists, catalog) start again.
pub(crate) fn library_stamp(tracks: &[Track]) -> (usize, usize) {
    (tracks.as_ptr() as usize, tracks.len())
}
fn track_matches(track: &Track, filter: &str) -> bool {
    if !track.online {
        return false;
    }
    if let Some(value) = filter.strip_prefix("artist:") {
        return track.artist == value;
    }
    if let Some(value) = filter.strip_prefix("genre:") {
        return track.genre == value;
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
