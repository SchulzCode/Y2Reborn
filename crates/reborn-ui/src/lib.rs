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
mod maskdata;
pub mod pages;
mod screens;
pub mod skin;
mod text;
pub mod theme;
mod views_coverflow;
mod views_library;
mod views_playing;
mod views_settings;
mod views_system;

use reborn_core::{
    platform::{LowBattery, UsbTransfer, WifiProblem},
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
    fn description(&self, output: &AudioOutput) -> String {
        let codec = self
            .codec
            .as_deref()
            .map(|c| format!(" · {c}"))
            .unwrap_or_default();
        if !self.saved_pairing() {
            if self.connected {
                "Finishing pairing…".into()
            } else {
                "Not paired".into()
            }
        } else if self.usable_audio() {
            if matches!(output, AudioOutput::Bluetooth(a) if a.eq_ignore_ascii_case(&self.address))
            {
                format!("Playing audio{codec}")
            } else {
                format!("Connected{codec}")
            }
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
}
impl RadioView {
    pub fn is_favorite(&self, station: &reborn_core::RadioStation) -> bool {
        self.favorites.iter().any(|k| k == station.key())
    }
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
    /// The text entry searches stations instead of taking a Wi-Fi password.
    pub entry_search: bool,
    pub pairing: Option<String>,
    pub pairing_focus: usize,
    password: String,
    ssid: String,
    letter: usize,
    pub text_entry: bool,
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
    counts: RefCell<CountsCache>,
    /// A dialog the player raises by itself (Bluetooth failure, low battery).
    pub alert: Option<Alert>,
    alert_focus: usize,
    /// The device a pair/connect request is waiting on: path, name, operation, start.
    bt_busy: Option<(String, String, String, Instant)>,
    usb_state: Option<UsbTransfer>,
    usb_dismissed: bool,
    battery_acked: u8,
    /// Something on screen sweeps with time (a spinner), so keep redrawing.
    spinning: bool,
    /// Freeze spinners at this phase (previews and tests).
    pub spin_fixed: Option<f32>,
    /// The update download bar as drawn: fill, step (check), last frame.
    download_bar: std::cell::Cell<(f32, bool, Option<Instant>)>,
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

const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 !@#$%^&*()-_=+[]{};:'\",.<>/?\\|`~";
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
        self.volume_until.is_some()
            || self.power_hold.is_some()
            || self.spinning
            || self.flow.borrow().moving()
            || self.transition_progress().is_some()
    }

    /// Motion that wants the panel's full frame rate: Cover Flow's glide and
    /// the cover's flight. Spinners and fades stay at the calm rate.
    pub fn gliding(&self) -> bool {
        self.flow.borrow().moving() || self.transition_progress().is_some()
    }

    /// "9 songs · 38 min" for the album with filter `key`, computed once per
    /// album and library rather than on every animation frame.
    pub(crate) fn album_detail(&self, m: &AppModel, tracks: &[Track], key: &str) -> String {
        let library = (tracks.as_ptr() as usize, tracks.len());
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

    /// A 0..1 phase for spinners, one turn about every 1.6 s.
    pub fn spin_phase(&self) -> f32 {
        if let Some(fixed) = self.spin_fixed {
            return fixed;
        }
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let elapsed = START.get_or_init(Instant::now).elapsed().as_millis() % 1600;
        elapsed as f32 / 1600.
    }

    /// Watch platform state the UI reacts to by itself: a computer connecting,
    /// a Bluetooth request finishing or failing, the battery running low and
    /// work that shows a spinner. Returns whether the screen needs redrawing.
    pub fn observe(&mut self, m: &AppModel) -> bool {
        let before = (
            self.usb_overlay(m),
            self.alert.clone(),
            self.bt_busy.is_some(),
            self.spinning,
        );
        let usb = m.platform.snapshot.usb;
        if self.usb_state != Some(usb) {
            let was_attached = matches!(
                self.usb_state,
                Some(UsbTransfer::Ready | UsbTransfer::Copying)
            );
            let now_attached = matches!(usb, UsbTransfer::Ready | UsbTransfer::Copying);
            // A new connection, or copying starting, shows the screen again.
            if !now_attached
                || !was_attached
                || (usb == UsbTransfer::Copying && self.usb_state == Some(UsbTransfer::Ready))
            {
                self.usb_dismissed = false;
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
        self.spinning = m.library.scanning
            || m.platform.snapshot.update.phase == reborn_core::platform::UpdatePhase::Downloading
            || m.platform.busy == Some(reborn_core::PlatformTask::UpdateStage)
            || self.usb_overlay(m) && usb == UsbTransfer::Copying
            || self.bt_busy.is_some();
        before
            != (
                self.usb_overlay(m),
                self.alert.clone(),
                self.bt_busy.is_some(),
                self.spinning,
            )
            || self.spinning
    }

    /// Whether the computer-connection screen covers the UI.
    pub fn usb_overlay(&self, m: &AppModel) -> bool {
        // Only a state `observe` has seen counts, so a stale fixture or the
        // first frames of a session never raise it.
        !self.usb_dismissed
            && !m.screen_off
            && self.usb_state == Some(m.platform.snapshot.usb)
            && matches!(
                m.platform.snapshot.usb,
                UsbTransfer::Ready | UsbTransfer::Copying
            )
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
        if self.usb_overlay(m) {
            return match action {
                Action::Back | Action::Home | Action::Select | Action::ContextMenu => {
                    self.usb_dismissed = true;
                    Some(Effect::None)
                }
                Action::WheelClockwise(_)
                | Action::WheelCounterClockwise(_)
                | Action::NavigateUp
                | Action::NavigateDown => Some(Effect::None),
                _ => None,
            };
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
        let stamp = (tracks.as_ptr() as usize, tracks.len());
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

    /// The accent in use: the cover's, or the default before one is known.
    pub fn accent(&self) -> u32 {
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
            | Screen::Playlists
            | Screen::Playlist
            | Screen::Tracks => self.catalog_rows(m, tracks),
            Screen::Queue => self.queue_rows(m),
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
                | Screen::Playlists
                | Screen::Playlist
                | Screen::Tracks
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
                return vec![
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
            }
            return vec![
                Item::new("Up next", "show_queue"),
                Item::new("Shuffle", "toggle_shuffle").with_secondary(if m.settings.shuffle {
                    "On"
                } else {
                    "Off"
                }),
                Item::new("Repeat", "cycle_repeat").with_secondary(repeat_label(m.settings.repeat)),
                Item::new("Go to album", "go_album"),
                Item::new("Go to artist", "go_artist"),
            ];
        }
        let selected_key = m.navigation.context_key.clone().or_else(|| {
            m.navigation
                .context_target
                .and_then(|target| self.row_at(m, tracks, target).map(|item| item.key))
        });
        let key = selected_key.as_deref();
        if key.is_some_and(|key| key.starts_with("track:")) {
            return vec![
                Item::new("Play next", "play_next"),
                Item::new("Add to queue", "add_queue"),
                Item::new("Go to album", "go_album"),
                Item::new("Go to artist", "go_artist"),
            ];
        }
        if m.screen == Screen::Playlists && key.is_some_and(|key| key.starts_with("playlist:")) {
            return vec![
                Item::new("Play", "collection_play"),
                Item::new("Shuffle", "collection_shuffle"),
                Item::new("Play Next", "collection_next"),
                Item::new("Add to Queue", "collection_queue"),
            ];
        }
        if matches!(m.screen, Screen::Album | Screen::Artist) && key == Some("play:collection") {
            return vec![
                Item::new("Play", "collection_play"),
                Item::new("Shuffle", "collection_shuffle"),
                Item::new("Play Next", "collection_next"),
                Item::new("Add to Queue", "collection_queue"),
            ];
        }
        match m.screen {
            Screen::Albums | Screen::Artists | Screen::Genres => vec![],
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
            Some(Modal::OutputPicker) => {
                let mut rows = vec![Item::new("Headphone jack", "output:wired").with_secondary(
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
                    Screen::RadioStations => key
                        .and_then(|k| k.strip_prefix("station:"))
                        .and_then(|i| i.parse::<usize>().ok())
                        .and_then(|i| self.radio.stations.get(i))
                        .map(|s| s.name.clone()),
                    Screen::Playlists => key
                        .and_then(|k| catalog::playlist_by_key(m, k))
                        .map(|p| p.name.clone()),
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
        if self.text_entry {
            return self.entry(m, action);
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
        let accelerate = m.navigation.modal.is_none()
            && Self::is_catalog(m.screen)
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
            Screen::Playlists if key.starts_with("playlist:") => Self::go(m, Screen::Playlist, key),
            Screen::Playlist if key.starts_with("track:") => return self.play_playlist(m, tracks),
            Screen::Artists => Self::go(m, Screen::Artist, key),
            Screen::Genres => Self::go(m, Screen::Albums, key),
            // A genre's albums start with all of its songs.
            Screen::Albums if key == "all_songs" => {
                Self::go(m, Screen::Tracks, m.navigation.filter.clone())
            }
            Screen::Albums => Self::go(m, Screen::Album, key),
            Screen::Folders if !key.starts_with("track:") => Self::go(m, Screen::Folders, key),
            Screen::Artist if key.starts_with("album:") => Self::go(m, Screen::Album, key),
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
            Screen::Radio | Screen::RadioStations | Screen::RadioCategories => {
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
                self.entry_search = true;
                self.password.clear();
                self.ssid = "Search Stations".into();
                self.letter = 0;
                self.text_entry = true;
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
            k if k.starts_with("eq_preset:") => {
                let name = &k["eq_preset:".len()..];
                let (_, _, gains) = pages::EQ_PRESETS.iter().find(|(_, key, _)| *key == name)?;
                Effect::SetEqPreset(*gains)
            }
            "shuffle" => Effect::SetShuffle(!m.settings.shuffle),
            "repeat" => Effect::SetRepeat(next_repeat(m.settings.repeat)),
            "gapless" => Effect::SetGapless(!m.settings.gapless_enabled),
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
    /// Select on a playlist song: the whole playlist, in file order, becomes
    /// the queue and playback starts at that occurrence of the song.
    fn play_playlist(&mut self, m: &mut AppModel, tracks: &[Track]) -> Effect {
        let members = self.filtered_indices(m, tracks);
        if members.is_empty() {
            return Effect::None;
        }
        let position = m.navigation.focus.min(members.len() - 1);
        Self::go(m, Screen::NowPlaying, "");
        Effect::PlayPlaylist { members, position }
    }
    fn filtered_indices(&self, m: &AppModel, tracks: &[Track]) -> Vec<usize> {
        if matches!(m.screen, Screen::Album | Screen::Playlist) {
            // Album order, not library order.
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
                self.ssid = ssid.into();
                self.password.clear();
                self.letter = 0;
                self.text_entry = true;
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
                    ConfirmAction::PowerOff => Effect::PowerOff,
                    ConfirmAction::Reboot => Effect::Reboot,
                    ConfirmAction::Platform(task) => Effect::Platform(task),
                }
            }
            Some(Modal::ContextMenu) => self.context_action(m, tracks, &key),
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
        if m.screen == Screen::Playlists && menu_key.starts_with("collection_") {
            let members = catalog::playlist_by_key(m, &source_key)
                .map(|p| catalog::playlist_members(tracks, p))
                .unwrap_or_default();
            let Some(&first) = members.first() else {
                return Effect::None;
            };
            return match menu_key {
                "collection_play" | "collection_shuffle" => {
                    Self::go(m, Screen::NowPlaying, "");
                    if menu_key == "collection_play" {
                        Effect::PlayPlaylist {
                            members,
                            position: 0,
                        }
                    } else {
                        Effect::PlayCollection {
                            selected: first,
                            members,
                            shuffle: true,
                        }
                    }
                }
                "collection_next" => Effect::PlayNextCollection(members),
                _ => Effect::AddToQueueCollection(members),
            };
        }
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

    fn entry(&mut self, m: &mut AppModel, action: Action) -> Effect {
        let count = LETTERS.len() + 3;
        match action {
            // Character entry is a value context: one detent, one character.
            Action::WheelClockwise(_) | Action::NavigateDown => {
                self.letter = (self.letter + 1) % count
            }
            Action::WheelCounterClockwise(_) | Action::NavigateUp => {
                self.letter = (self.letter + count - 1) % count
            }
            Action::Select if self.letter < LETTERS.len() => {
                if self.password.len() < if self.entry_search { 40 } else { 63 } {
                    self.password.push(LETTERS[self.letter] as char);
                }
            }
            Action::Select if self.letter == LETTERS.len() => {
                self.password.pop();
            }
            Action::ContextMenu | Action::Select
                if action == Action::ContextMenu || self.letter == LETTERS.len() + 1 =>
            {
                if self.entry_search {
                    let text = self.password.trim().to_owned();
                    if text.chars().count() < 2 {
                        self.flash("Type at least two letters");
                        return Effect::None;
                    }
                    self.text_entry = false;
                    self.entry_search = false;
                    self.password.clear();
                    let title = format!("“{text}”");
                    self.radio.title = title.clone();
                    self.radio.stations.clear();
                    self.radio.loading = true;
                    self.radio.error = None;
                    self.radio.last_request = Some(RadioRequest::Search(text.clone()));
                    Self::go(m, Screen::RadioStations, &title);
                    return Effect::Radio(RadioRequest::Search(text));
                }
                if self.password.len() >= 8 {
                    self.text_entry = false;
                    return Effect::WifiConnect {
                        ssid: self.ssid.clone(),
                        password: std::mem::take(&mut self.password),
                    };
                }
                self.flash("Wi-Fi passwords have at least 8 characters");
            }
            Action::Back | Action::Home | Action::Select => {
                self.password.clear();
                self.text_entry = false;
                self.entry_search = false;
            }
            _ => {}
        }
        Effect::None
    }

    pub fn draw(&self, m: &AppModel, tracks: &[Track], has_art: bool) -> Vec<Quad> {
        screens::draw(self, m, tracks, has_art)
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
        "playlists" => Screen::Playlists,
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
        "system" => Screen::SettingsSystem,
        "battery" => Screen::Battery,
        "sleep" => Screen::Sleep,
        "storage" => Screen::Storage,
        "update" => Screen::Update,
        "about" => Screen::About,
        "maintenance" => Screen::Maintenance,
        "diagnostics" => Screen::Diagnostics,
        "radio" => Screen::Radio,
        _ => return None,
    })
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
        | "library_benchmark" => ConfirmAction::Platform(platform_task(name)),
        _ => return None,
    })
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
        ConfirmAction::PowerOff => "Power Off",
        ConfirmAction::Reboot => "Restart",
        ConfirmAction::Platform(PlatformTask::UpdateApply) => "Restart and Install",
        ConfirmAction::Platform(PlatformTask::UpdateCancel) => "Cancel Update",
        ConfirmAction::Platform(PlatformTask::UpdateRollback) => "Restore",
        ConfirmAction::Platform(_) => "Run",
    }
}

fn confirm_copy(action: ConfirmAction) -> (&'static str, &'static str) {
    match action {
        ConfirmAction::ClearQueue => (
            "Clear upcoming songs?",
            "The current song keeps playing. Songs after it are removed from the queue.",
        ),
        ConfirmAction::RebuildLibrary => (
            "Rebuild music library?",
            "Every music file is read again. Your music is not changed, and the library stays usable while this runs.",
        ),
        ConfirmAction::ForgetBluetooth => (
            "Forget this device?",
            "You'll need to pair it again to use it.",
        ),
        ConfirmAction::ForgetWifi => (
            "Forget this network?",
            "Its saved password is removed from the player.",
        ),
        ConfirmAction::ForgetAllWifi => (
            "Forget all Wi-Fi networks?",
            "Every saved network and password is removed.",
        ),
        ConfirmAction::ForgetAllBluetooth => (
            "Remove all Bluetooth pairings?",
            "Every paired headphone and speaker must be paired again.",
        ),
        ConfirmAction::ResetSettings => (
            "Reset Reborn settings?",
            "Playback, audio and display settings return to their defaults. Music, queue and pairings stay.",
        ),
        ConfirmAction::ClearCache => (
            "Clear cache?",
            "Saved artwork copies are removed and recreated when needed. Music is not affected.",
        ),
        ConfirmAction::PowerOff => ("Power off?", "Your queue and position are saved."),
        ConfirmAction::Reboot => ("Restart?", "Your queue and position are saved."),
        ConfirmAction::Platform(PlatformTask::UpdateApply) => (
            "Restart and install?",
            "The player restarts, installs the update and starts the new version by itself. It takes about a minute. Use the charger or at least half a battery.",
        ),
        ConfirmAction::Platform(PlatformTask::UpdateCancel) => (
            "Cancel this update?",
            "The installed software stays as it is.",
        ),
        ConfirmAction::Platform(PlatformTask::UpdateRollback) => (
            "Restore previous system?",
            "The player restarts into the system that was installed before the last update.",
        ),
        ConfirmAction::Platform(_) => (
            "Run this check?",
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
