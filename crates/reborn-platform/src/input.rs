#![forbid(unsafe_code)]

use reborn_core::{Action, NormalizedInput, PhysicalControl};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};

pub const LONG_PRESS: Duration = Duration::from_millis(650);
pub const REPEAT_START: Duration = Duration::from_millis(400);
pub const REPEAT_RATE: Duration = Duration::from_millis(90);

/// Same-direction wheel records closer than this are one physical detent seen
/// twice (a bounced data-ready edge re-reading the latched APT32F frame). A
/// person spinning hard produces detents tens of milliseconds apart.
pub const WHEEL_DUPLICATE_US: u64 = 8_000;
/// A pause this long ends a rotation: the next detent is isolated again.
pub const WHEEL_RESET_US: u64 = 220_000;
/// Detents of one sustained rotation before any acceleration is considered.
pub const WHEEL_ACCELERATION_AFTER: u32 = 4;

/// Deliberate, cadence-based wheel acceleration over kernel event timestamps.
///
/// One isolated detent is always one step. Acceleration needs a sustained,
/// same-direction rotation and is forgotten after a pause or reversal. The UI
/// decides per context whether the suggested step count is used at all.
#[derive(Clone, Debug, Default)]
pub struct WheelCadence {
    last: Option<(bool, u64)>,
    streak: u32,
    /// Smoothed detent interval of the current rotation, microseconds.
    interval_us: u64,
}
impl WheelCadence {
    /// Returns `None` for a duplicate record, otherwise the step suggestion.
    pub fn detent(&mut self, clockwise: bool, at_us: u64) -> Option<u8> {
        let previous = self.last;
        self.last = Some((clockwise, at_us));
        let Some((direction, then)) = previous else {
            return Some(self.restart());
        };
        // A clock that ran backwards cannot prove cadence.
        let Some(gap) = at_us.checked_sub(then) else {
            return Some(self.restart());
        };
        if direction != clockwise || gap > WHEEL_RESET_US {
            return Some(self.restart());
        }
        if gap < WHEEL_DUPLICATE_US {
            // Keep the original detent time so a bounce train cannot extend it.
            self.last = Some((clockwise, then));
            return None;
        }
        self.streak = self.streak.saturating_add(1);
        self.interval_us = if self.streak == 2 {
            gap
        } else {
            (self.interval_us * 3 + gap) / 4
        };
        Some(self.steps())
    }
    fn restart(&mut self) -> u8 {
        self.streak = 1;
        self.interval_us = 0;
        1
    }
    fn steps(&self) -> u8 {
        if self.streak < WHEEL_ACCELERATION_AFTER {
            return 1;
        }
        match self.interval_us {
            0..=45_000 if self.streak >= 10 => 4,
            0..=70_000 if self.streak >= 6 => 3,
            0..=110_000 => 2,
            _ => 1,
        }
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

const NAVIGATION_BUTTONS: &str = "Y2 navigation buttons";
const PMIC_KEYS: &str = "mtk-pmic-keys";
const CLICK_WHEEL: &str = "APT32F click-wheel";
const KEYPAD: &str = "mt6582-keypad";

const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_SYN: u16 = 0;
const SYN_DROPPED: u16 = 3;
const SYN_REPORT: u16 = 0;
const REL_WHEEL: u16 = 8;
const KEY_UP: u16 = 103;
const KEY_PAGEUP: u16 = 104;
const KEY_DOWN: u16 = 108;
const KEY_PAGEDOWN: u16 = 109;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Device {
    pub path: PathBuf,
    pub name: String,
    pub identity: PathBuf,
}

pub fn devices() -> Vec<Device> {
    fs::read_dir("/sys/class/input")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("event"))
        .filter_map(|e| {
            let name = fs::read_to_string(e.path().join("device/name"))
                .ok()?
                .trim()
                .to_owned();
            if !matches!(
                name.as_str(),
                NAVIGATION_BUTTONS | PMIC_KEYS | CLICK_WHEEL | KEYPAD
            ) {
                return None;
            }
            let identity = fs::canonicalize(e.path().join("device")).ok()?;
            Some(Device {
                path: PathBuf::from("/dev/input").join(e.file_name()),
                name,
                identity,
            })
        })
        .collect()
}

fn rediscover_device(device: &Device, candidates: &[Device]) -> Option<Device> {
    candidates
        .iter()
        .find(|candidate| candidate.name == device.name && candidate.identity == device.identity)
        .cloned()
}

/// One decoded evdev record.
#[derive(Clone, Copy, Debug)]
struct Record {
    kind: u16,
    code: u16,
    value: i32,
    /// Kernel timestamp in microseconds, when the record carried one.
    stamp_us: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputEvent {
    pub device: String,
    pub input: NormalizedInput,
}

#[derive(Clone, Copy, Debug)]
struct Pressed {
    started: Instant,
    last_repeat: Instant,
    long_emitted: bool,
}

/// The only layer that knows Linux event types, key codes, and device names.
/// It converts those events into stable physical events and owns all timing.
pub struct InputManager {
    files: Vec<(Device, File, Vec<u8>)>,
    missing: Vec<Device>,
    pressed: HashMap<(String, PhysicalControl), Pressed>,
    wheel: HashMap<String, WheelCadence>,
    dropping: HashSet<String>,
    epoch: Instant,
}

impl InputManager {
    pub fn open() -> Self {
        Self {
            files: devices()
                .into_iter()
                .filter_map(|d| {
                    let f = open_device(&d.path)?;
                    Some((d, f, vec![]))
                })
                .collect(),
            missing: Vec::new(),
            pressed: HashMap::new(),
            wheel: HashMap::new(),
            dropping: HashSet::new(),
            epoch: Instant::now(),
        }
    }

    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            files: vec![],
            missing: Vec::new(),
            pressed: HashMap::new(),
            wheel: HashMap::new(),
            dropping: HashSet::new(),
            epoch: Instant::now(),
        }
    }

    pub fn count(&self) -> usize {
        self.files.len()
    }

    pub fn poll(&mut self) -> Vec<InputEvent> {
        self.poll_at(Instant::now())
    }

    #[cfg(test)]
    pub fn feed(
        &mut self,
        device: &str,
        kind: u16,
        code: u16,
        value: i32,
        now: Instant,
    ) -> Vec<InputEvent> {
        let record = Record {
            kind,
            code,
            value,
            stamp_us: None,
        };
        self.ingest(device, device, record, now)
    }

    /// Feed a record carrying its own kernel timestamp (microseconds).
    #[cfg(test)]
    pub fn feed_at(
        &mut self,
        device: &str,
        kind: u16,
        code: u16,
        value: i32,
        stamp_us: u64,
    ) -> Vec<InputEvent> {
        let record = Record {
            kind,
            code,
            value,
            stamp_us: Some(stamp_us),
        };
        self.ingest(device, device, record, Instant::now())
    }

    #[cfg(test)]
    pub fn tick(&mut self, now: Instant) -> Vec<InputEvent> {
        self.tick_at(now)
    }

    fn poll_at(&mut self, now: Instant) -> Vec<InputEvent> {
        self.reconnect_missing();
        let mut events = vec![];
        let size = std::mem::size_of::<libc::input_event>();
        let mut raw = Vec::new();
        let mut reopen = Vec::new();
        for (index, (device, file, pending)) in self.files.iter_mut().enumerate() {
            let mut bytes = [0u8; 1024];
            match file.read(&mut bytes) {
                Ok(0) => {
                    pending.clear();
                    reopen.push(index);
                }
                Ok(n) => pending.extend_from_slice(&bytes[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => {
                    pending.clear();
                    reopen.push(index);
                }
            }
            while pending.len() >= size {
                let offset = size - 8;
                let kind = u16::from_ne_bytes(pending[offset..offset + 2].try_into().unwrap());
                let code = u16::from_ne_bytes(pending[offset + 2..offset + 4].try_into().unwrap());
                let value = i32::from_ne_bytes(pending[offset + 4..offset + 8].try_into().unwrap());
                let stamp = record_timestamp_us(&pending[..offset]);
                pending.drain(..size);
                raw.push((
                    device.name.clone(),
                    device.identity.to_string_lossy().into_owned(),
                    Record {
                        kind,
                        code,
                        value,
                        stamp_us: stamp,
                    },
                ));
            }
        }
        for (device, identity, record) in raw {
            events.extend(self.ingest(&device, &identity, record, now));
        }
        if !reopen.is_empty() {
            let candidates = devices();
            reopen.sort_unstable();
            reopen.dedup();
            for index in reopen.into_iter().rev() {
                if index >= self.files.len() {
                    continue;
                }
                let device = self.files[index].0.clone();
                let identity = device.identity.to_string_lossy().into_owned();
                events.extend(self.cancel_stale_device(&identity));
                if let Some(replacement) = rediscover_device(&device, &candidates) {
                    if let Some(file) = open_device(&replacement.path) {
                        self.files[index] = (replacement, file, Vec::new());
                        continue;
                    }
                }
                self.files.remove(index);
                self.missing.push(device);
            }
        }
        events.extend(self.tick_at(now));
        events
    }

    fn reconnect_missing(&mut self) {
        if self.missing.is_empty() {
            return;
        }
        let candidates = devices();
        let mut still_missing = Vec::new();
        for device in self.missing.drain(..) {
            let Some(replacement) = rediscover_device(&device, &candidates) else {
                still_missing.push(device);
                continue;
            };
            match open_device(&replacement.path) {
                Some(file) => self.files.push((replacement, file, Vec::new())),
                None => still_missing.push(device),
            }
        }
        self.missing = still_missing;
    }

    fn cancel_stale_device(&mut self, identity: &str) -> Vec<InputEvent> {
        let controls = self
            .pressed
            .keys()
            .filter(|(device, _)| device == identity)
            .cloned()
            .collect::<Vec<_>>();
        for key in &controls {
            self.pressed.remove(key);
        }
        self.wheel.remove(identity);
        controls
            .into_iter()
            .map(|(_, control)| InputEvent {
                device: "input-recovery".into(),
                input: NormalizedInput::Cancel(control),
            })
            .collect()
    }

    fn ingest(
        &mut self,
        device: &str,
        identity: &str,
        record: Record,
        now: Instant,
    ) -> Vec<InputEvent> {
        let Record {
            kind,
            code,
            value,
            stamp_us,
        } = record;
        if kind == EV_SYN {
            if code == SYN_DROPPED {
                self.dropping.insert(identity.to_owned());
                return self.cancel_stale_device(identity);
            }
            if self.dropping.contains(identity) {
                if code == SYN_REPORT {
                    self.dropping.remove(identity);
                }
                return vec![];
            }
            return vec![];
        }
        if self.dropping.contains(identity) {
            return vec![];
        }
        let Some(input) = map(device, kind, code, value) else {
            return vec![];
        };
        if matches!(
            input,
            NormalizedInput::WheelClockwise(_) | NormalizedInput::WheelCounterClockwise(_)
        ) {
            let clockwise = matches!(input, NormalizedInput::WheelClockwise(_));
            let raw_detents = match input {
                NormalizedInput::WheelClockwise(steps)
                | NormalizedInput::WheelCounterClockwise(steps) => steps.max(1),
                _ => 1,
            };
            let at_us = stamp_us
                .unwrap_or_else(|| now.saturating_duration_since(self.epoch).as_micros() as u64);
            let cadence = self.wheel.entry(identity.to_owned()).or_default();
            let steps = if raw_detents > 1 {
                // A relative wheel reporting several detents in one record is
                // explicit fast movement, never a duplicate.
                cadence.detent(clockwise, at_us);
                raw_detents.min(4)
            } else {
                match cadence.detent(clockwise, at_us) {
                    Some(steps) => steps,
                    None => return vec![],
                }
            };
            return vec![InputEvent {
                device: device.to_owned(),
                input: if clockwise {
                    NormalizedInput::WheelClockwise(steps)
                } else {
                    NormalizedInput::WheelCounterClockwise(steps)
                },
            }];
        }
        match input {
            NormalizedInput::Press(control) => {
                let key = (identity.to_owned(), control);
                if self.pressed.contains_key(&key) {
                    return vec![];
                }
                self.pressed.insert(
                    key,
                    Pressed {
                        started: now,
                        last_repeat: now,
                        long_emitted: false,
                    },
                );
                vec![InputEvent {
                    device: device.to_owned(),
                    input: NormalizedInput::Press(control),
                }]
            }
            NormalizedInput::Release(control) => {
                let input = if self
                    .pressed
                    .remove(&(identity.to_owned(), control))
                    .is_some()
                {
                    NormalizedInput::Release(control)
                } else {
                    NormalizedInput::Cancel(control)
                };
                vec![InputEvent {
                    device: device.to_owned(),
                    input,
                }]
            }
            NormalizedInput::Repeat(control)
                if self.pressed.contains_key(&(identity.to_owned(), control)) =>
            {
                vec![InputEvent {
                    device: device.to_owned(),
                    input: NormalizedInput::Repeat(control),
                }]
            }
            NormalizedInput::Repeat(control) => vec![InputEvent {
                device: device.to_owned(),
                input: NormalizedInput::Cancel(control),
            }],
            _ => vec![],
        }
    }

    fn tick_at(&mut self, now: Instant) -> Vec<InputEvent> {
        let mut events = vec![];
        for (key, state) in &mut self.pressed {
            let control = key.1;
            let held = now.saturating_duration_since(state.started);
            if !state.long_emitted && held >= LONG_PRESS && is_long_press_control(control) {
                state.long_emitted = true;
                events.push(InputEvent {
                    device: "timing".into(),
                    input: NormalizedInput::LongPress(control),
                });
            }
            if is_repeat_control(control)
                && held >= REPEAT_START
                && now.saturating_duration_since(state.last_repeat) >= REPEAT_RATE
            {
                state.last_repeat = now;
                events.push(InputEvent {
                    device: "timing".into(),
                    input: NormalizedInput::Repeat(control),
                });
            }
        }
        events
    }
}

fn open_device(path: &std::path::Path) -> Option<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    crate::evdev_monotonic_clock(&file);
    Some(file)
}

/// Decode the `struct input_event` time prefix. Its two fields share one
/// width (32-bit `__sec/__usec` on ARMv7, 64-bit on the host).
fn record_timestamp_us(prefix: &[u8]) -> Option<u64> {
    let half = prefix.len() / 2;
    let field = |bytes: &[u8]| -> Option<u64> {
        match bytes.len() {
            4 => Some(u32::from_ne_bytes(bytes.try_into().ok()?) as u64),
            8 => Some(u64::from_ne_bytes(bytes.try_into().ok()?)),
            _ => None,
        }
    };
    let seconds = field(&prefix[..half])?;
    let micros = field(&prefix[half..half * 2])?;
    (micros < 1_000_000).then(|| seconds.saturating_mul(1_000_000).saturating_add(micros))
}

fn is_long_press_control(control: PhysicalControl) -> bool {
    matches!(
        control,
        PhysicalControl::Select
            | PhysicalControl::Back
            | PhysicalControl::Previous
            | PhysicalControl::Next
            | PhysicalControl::PlayPause
            | PhysicalControl::Power
    )
}

fn is_repeat_control(control: PhysicalControl) -> bool {
    matches!(
        control,
        PhysicalControl::VolumeUp
            | PhysicalControl::VolumeDown
            | PhysicalControl::Previous
            | PhysicalControl::Next
    )
}

/// The second boundary translates normalized physical events into product
/// actions and owns screen-off policy. It never sees a Linux key code.
#[derive(Default)]
pub struct ActionRouter {
    long_pressed: HashSet<PhysicalControl>,
}

impl ActionRouter {
    pub fn route(&mut self, event: &InputEvent, screen_on: bool) -> Vec<Action> {
        match event.input {
            NormalizedInput::WheelClockwise(steps) if screen_on => {
                vec![Action::WheelClockwise(steps)]
            }
            NormalizedInput::WheelCounterClockwise(steps) if screen_on => {
                vec![Action::WheelCounterClockwise(steps)]
            }
            NormalizedInput::WheelClockwise(_) | NormalizedInput::WheelCounterClockwise(_) => {
                vec![]
            }
            NormalizedInput::Press(control) => match control {
                PhysicalControl::VolumeUp => vec![Action::VolumeUp],
                PhysicalControl::VolumeDown => vec![Action::VolumeDown],
                _ => vec![],
            },
            NormalizedInput::Repeat(control) => match control {
                PhysicalControl::VolumeUp => vec![Action::VolumeUp],
                PhysicalControl::VolumeDown => vec![Action::VolumeDown],
                PhysicalControl::Previous if self.long_pressed.contains(&control) => {
                    vec![Action::SeekBackward]
                }
                PhysicalControl::Next if self.long_pressed.contains(&control) => {
                    vec![Action::SeekForward]
                }
                _ => vec![],
            },
            NormalizedInput::LongPress(control) => {
                if !screen_on {
                    return vec![];
                }
                self.long_pressed.insert(control);
                match control {
                    PhysicalControl::Select => vec![Action::ContextMenu],
                    PhysicalControl::Back => vec![Action::Home],
                    PhysicalControl::Previous => vec![Action::SeekBackward],
                    PhysicalControl::Next => vec![Action::SeekForward],
                    PhysicalControl::PlayPause => vec![Action::ShowNowPlaying],
                    PhysicalControl::Power => vec![Action::PowerMenu],
                    _ => vec![],
                }
            }
            NormalizedInput::Cancel(control) => {
                self.long_pressed.remove(&control);
                vec![]
            }
            NormalizedInput::Release(control) => {
                if self.long_pressed.remove(&control) {
                    return vec![];
                }
                match control {
                    PhysicalControl::Select if screen_on => vec![Action::Select],
                    PhysicalControl::Back if screen_on => vec![Action::Back],
                    PhysicalControl::Previous => vec![Action::PreviousTrack],
                    PhysicalControl::Next => vec![Action::NextTrack],
                    PhysicalControl::PlayPause => vec![Action::PlayPause],
                    PhysicalControl::Power => {
                        if screen_on {
                            vec![Action::ScreenSleep]
                        } else {
                            vec![Action::ScreenWake]
                        }
                    }
                    _ => vec![],
                }
            }
        }
    }

    pub fn reset(&mut self) {
        self.long_pressed.clear();
    }
}

/// Decode one Linux input record. Device identity is part of the mapping so a
/// keypad detent cannot collide with an application directional key.
pub fn map(device: &str, kind: u16, code: u16, value: i32) -> Option<NormalizedInput> {
    if kind == EV_REL && code == REL_WHEEL {
        if value == 0 {
            return None;
        }
        return Some(if value >= 0 {
            NormalizedInput::WheelClockwise(value.unsigned_abs().clamp(1, 32) as u8)
        } else {
            NormalizedInput::WheelCounterClockwise(value.unsigned_abs().clamp(1, 32) as u8)
        });
    }
    if kind != EV_KEY || !matches!(value, 0..=2) {
        return None;
    }
    if device == CLICK_WHEEL {
        let direction = match code {
            KEY_UP | KEY_PAGEUP => Some(false),
            KEY_DOWN | KEY_PAGEDOWN => Some(true),
            _ => None,
        };
        if let Some(clockwise) = direction {
            // Only the press of the driver's press/release pair is a detent.
            // Release and (never-enabled) autorepeat records are not rotation.
            return match value {
                1 => Some(if clockwise {
                    NormalizedInput::WheelClockwise(1)
                } else {
                    NormalizedInput::WheelCounterClockwise(1)
                }),
                _ => None,
            };
        }
    }
    let control = match (device, code) {
        (NAVIGATION_BUTTONS, 28) => PhysicalControl::Select,
        (NAVIGATION_BUTTONS, 158) => PhysicalControl::Back,
        (NAVIGATION_BUTTONS, 105) => PhysicalControl::Previous,
        (NAVIGATION_BUTTONS, 106) => PhysicalControl::Next,
        (NAVIGATION_BUTTONS, 164) => PhysicalControl::PlayPause,
        (PMIC_KEYS, 116) => PhysicalControl::Power,
        (_, 114) => PhysicalControl::VolumeDown,
        (_, 115) => PhysicalControl::VolumeUp,
        (_, 163) => PhysicalControl::Next,
        (_, 165) => PhysicalControl::Previous,
        (_, 57) => PhysicalControl::PlayPause,
        _ => return None,
    };
    Some(match value {
        0 => NormalizedInput::Release(control),
        1 => NormalizedInput::Press(control),
        _ => NormalizedInput::Repeat(control),
    })
}

/// Compatibility alias for the platform entrypoint; new code should use the
/// explicit InputManager name.
pub type Input = InputManager;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_devices_map_without_event_numbers() {
        assert_eq!(
            map(NAVIGATION_BUTTONS, 1, 28, 1),
            Some(NormalizedInput::Press(PhysicalControl::Select))
        );
        assert_eq!(
            map(CLICK_WHEEL, EV_KEY, KEY_DOWN, 1),
            Some(NormalizedInput::WheelClockwise(1))
        );
        assert_eq!(
            map(CLICK_WHEEL, EV_KEY, KEY_PAGEDOWN, 1),
            Some(NormalizedInput::WheelClockwise(1))
        );
        assert_eq!(
            map(CLICK_WHEEL, EV_KEY, KEY_UP, 1),
            Some(NormalizedInput::WheelCounterClockwise(1))
        );
        assert_eq!(
            map(CLICK_WHEEL, EV_KEY, KEY_PAGEUP, 1),
            Some(NormalizedInput::WheelCounterClockwise(1))
        );
        assert_eq!(map(KEYPAD, EV_KEY, KEY_DOWN, 1), None);
        assert_eq!(
            map(PMIC_KEYS, 1, 116, 1),
            Some(NormalizedInput::Press(PhysicalControl::Power))
        );
    }

    #[test]
    fn wheel_is_always_separate_from_select_and_playback() {
        assert_eq!(
            map(CLICK_WHEEL, EV_KEY, KEY_DOWN, 1),
            Some(NormalizedInput::WheelClockwise(1))
        );
        let mut router = ActionRouter::default();
        let event = InputEvent {
            device: CLICK_WHEEL.into(),
            input: NormalizedInput::WheelClockwise(1),
        };
        assert_eq!(router.route(&event, true), vec![Action::WheelClockwise(1)]);
    }

    #[test]
    fn long_press_suppresses_short_press() {
        let mut input = InputManager::empty();
        let started = std::time::Instant::now();
        let _ = input.feed(NAVIGATION_BUTTONS, 1, 28, 1, started);
        let long = input.tick(started + LONG_PRESS + Duration::from_millis(1));
        assert_eq!(long.len(), 1);
        assert_eq!(
            long[0].input,
            NormalizedInput::LongPress(PhysicalControl::Select)
        );
        let mut router = ActionRouter::default();
        assert_eq!(router.route(&long[0], true), vec![Action::ContextMenu]);
        let release = input.feed(
            NAVIGATION_BUTTONS,
            1,
            28,
            0,
            started + Duration::from_millis(700),
        );
        assert_eq!(router.route(&release[0], true), Vec::<Action>::new());
    }

    #[test]
    fn volume_repeats_without_waking_screen() {
        let mut input = InputManager::empty();
        let started = std::time::Instant::now();
        let press = input.feed(PMIC_KEYS, 1, 115, 1, started);
        let repeat = input.tick(started + REPEAT_START + REPEAT_RATE);
        let mut router = ActionRouter::default();
        assert_eq!(router.route(&press[0], false), vec![Action::VolumeUp]);
        assert_eq!(router.route(&repeat[0], false), vec![Action::VolumeUp]);
    }

    #[test]
    fn only_power_wakes_screen() {
        let mut input = InputManager::empty();
        let now = std::time::Instant::now();
        let mut router = ActionRouter::default();
        let _ = input.feed(NAVIGATION_BUTTONS, EV_KEY, 158, 1, now);
        let back = input.feed(NAVIGATION_BUTTONS, EV_KEY, 158, 0, now);
        assert_eq!(router.route(&back[0], false), Vec::<Action>::new());
        let _ = input.feed(PMIC_KEYS, EV_KEY, 116, 1, now);
        let power = input.feed(PMIC_KEYS, EV_KEY, 116, 0, now);
        assert_eq!(router.route(&power[0], false), vec![Action::ScreenWake]);
    }

    fn detent(input: &mut InputManager, code: u16, at_ms: u64) -> Option<NormalizedInput> {
        let out = input.feed_at(CLICK_WHEEL, EV_KEY, code, 1, at_ms * 1000);
        let _release = input.feed_at(CLICK_WHEEL, EV_KEY, code, 0, at_ms * 1000 + 1);
        assert!(_release.is_empty(), "release is never a detent");
        out.first().map(|e| e.input)
    }

    #[test]
    fn one_isolated_detent_is_one_step_in_both_directions() {
        let mut input = InputManager::empty();
        assert_eq!(
            detent(&mut input, KEY_DOWN, 1_000),
            Some(NormalizedInput::WheelClockwise(1))
        );
        assert_eq!(
            detent(&mut input, KEY_UP, 3_000),
            Some(NormalizedInput::WheelCounterClockwise(1))
        );
        assert_eq!(
            detent(&mut input, KEY_PAGEDOWN, 5_000),
            Some(NormalizedInput::WheelClockwise(1))
        );
    }

    #[test]
    fn duplicate_and_bounced_records_are_one_detent() {
        let mut input = InputManager::empty();
        assert_eq!(
            detent(&mut input, KEY_DOWN, 100),
            Some(NormalizedInput::WheelClockwise(1))
        );
        // Same frame re-read 2 ms later, and a bounce train within the window.
        assert_eq!(detent(&mut input, KEY_DOWN, 102), None);
        assert_eq!(detent(&mut input, KEY_PAGEDOWN, 105), None);
        assert_eq!(detent(&mut input, KEY_DOWN, 107), None);
        // Autorepeat is not rotation.
        assert!(input
            .feed_at(CLICK_WHEEL, EV_KEY, KEY_DOWN, 2, 400_000)
            .is_empty());
    }

    #[test]
    fn two_legitimate_rapid_detents_are_two_single_steps() {
        let mut input = InputManager::empty();
        assert_eq!(
            detent(&mut input, KEY_DOWN, 1_000),
            Some(NormalizedInput::WheelClockwise(1))
        );
        // 40 ms apart: a real fast flick, delivered in one poll batch.
        assert_eq!(
            detent(&mut input, KEY_DOWN, 1_040),
            Some(NormalizedInput::WheelClockwise(1))
        );
    }

    #[test]
    fn only_sustained_rotation_accelerates_and_pause_or_reversal_resets() {
        let mut input = InputManager::empty();
        let mut steps = vec![];
        for i in 0..14 {
            if let Some(NormalizedInput::WheelClockwise(n)) =
                detent(&mut input, KEY_DOWN, 10_000 + i * 40)
            {
                steps.push(n);
            }
        }
        assert_eq!(&steps[..3], &[1, 1, 1], "no early acceleration");
        assert!(steps.iter().any(|n| *n >= 2));
        assert!(steps.iter().all(|n| *n <= 4));
        assert_eq!(*steps.last().unwrap(), 4);
        // Reversal: immediately one step.
        assert_eq!(
            detent(&mut input, KEY_UP, 10_600),
            Some(NormalizedInput::WheelCounterClockwise(1))
        );
        // Build momentum again, pause, then one isolated detent.
        for i in 0..8 {
            detent(&mut input, KEY_UP, 11_000 + i * 40);
        }
        assert_eq!(
            detent(&mut input, KEY_UP, 11_600),
            Some(NormalizedInput::WheelCounterClockwise(1))
        );
    }

    #[test]
    fn slow_continuous_rotation_never_accelerates() {
        let mut input = InputManager::empty();
        for i in 0..20 {
            assert_eq!(
                detent(&mut input, KEY_DOWN, 50_000 + i * 150),
                Some(NormalizedInput::WheelClockwise(1)),
                "detent {i}"
            );
        }
    }

    #[test]
    fn backwards_timestamps_and_device_loss_forget_cadence() {
        let mut input = InputManager::empty();
        for i in 0..8 {
            detent(&mut input, KEY_DOWN, 90_000 + i * 40);
        }
        assert_eq!(
            detent(&mut input, KEY_DOWN, 80_000),
            Some(NormalizedInput::WheelClockwise(1))
        );
        for i in 0..8 {
            detent(&mut input, KEY_DOWN, 95_000 + i * 40);
        }
        input.cancel_stale_device(CLICK_WHEEL);
        assert_eq!(
            detent(&mut input, KEY_DOWN, 95_340),
            Some(NormalizedInput::WheelClockwise(1))
        );
    }

    #[test]
    fn kernel_timestamp_prefix_decodes_both_layouts() {
        let mut arm = vec![];
        arm.extend_from_slice(&7u32.to_ne_bytes());
        arm.extend_from_slice(&250_000u32.to_ne_bytes());
        assert_eq!(record_timestamp_us(&arm), Some(7_250_000));
        let mut host = vec![];
        host.extend_from_slice(&7u64.to_ne_bytes());
        host.extend_from_slice(&250_000u64.to_ne_bytes());
        assert_eq!(record_timestamp_us(&host), Some(7_250_000));
        let mut bad = vec![];
        bad.extend_from_slice(&7u32.to_ne_bytes());
        bad.extend_from_slice(&2_000_000u32.to_ne_bytes());
        assert_eq!(record_timestamp_us(&bad), None);
    }

    #[test]
    fn screen_off_drops_wheel_actions() {
        let mut router = ActionRouter::default();
        let event = InputEvent {
            device: CLICK_WHEEL.into(),
            input: NormalizedInput::WheelClockwise(1),
        };
        assert!(router.route(&event, false).is_empty());
    }

    #[test]
    fn long_previous_suppresses_track_change_and_starts_seek() {
        let mut input = InputManager::empty();
        let start = Instant::now();
        let _ = input.feed(NAVIGATION_BUTTONS, 1, 105, 1, start);
        let long = input.tick(start + LONG_PRESS + Duration::from_millis(1));
        let mut router = ActionRouter::default();
        assert_eq!(router.route(&long[0], true), vec![Action::SeekBackward]);
        let release = input.feed(
            NAVIGATION_BUTTONS,
            1,
            105,
            0,
            start + Duration::from_secs(1),
        );
        assert!(router.route(&release[0], true).is_empty());
    }

    #[test]
    fn queued_release_is_consumed_before_long_press_aging() {
        let mut input = InputManager::empty();
        let start = Instant::now();
        let _ = input.feed(NAVIGATION_BUTTONS, EV_KEY, 28, 1, start);
        let release = input.feed(
            NAVIGATION_BUTTONS,
            EV_KEY,
            28,
            0,
            start + LONG_PRESS + Duration::from_millis(1),
        );
        assert_eq!(
            release[0].input,
            NormalizedInput::Release(PhysicalControl::Select)
        );
        assert!(input
            .tick(start + LONG_PRESS + Duration::from_millis(2))
            .is_empty());
    }

    #[test]
    fn syn_dropped_cancels_select_next_and_power_without_activation() {
        let cases = [
            (NAVIGATION_BUTTONS, 28, PhysicalControl::Select),
            (NAVIGATION_BUTTONS, 106, PhysicalControl::Next),
            (PMIC_KEYS, 116, PhysicalControl::Power),
        ];
        for (device, code, control) in cases {
            let mut input = InputManager::empty();
            let mut router = ActionRouter::default();
            let now = Instant::now();
            let press = input.feed(device, EV_KEY, code, 1, now);
            assert_eq!(press[0].input, NormalizedInput::Press(control));
            assert!(router.route(&press[0], true).is_empty());

            let canceled = input.feed(device, EV_SYN, SYN_DROPPED, 0, now);
            assert_eq!(canceled.len(), 1);
            assert_eq!(canceled[0].input, NormalizedInput::Cancel(control));
            assert!(router.route(&canceled[0], true).is_empty());

            // Discard all records through the next SYN_REPORT.
            assert!(input
                .feed(device, EV_KEY, code, 0, now + Duration::from_millis(1))
                .is_empty());
            assert!(input
                .feed(device, EV_KEY, code, 1, now + Duration::from_millis(2))
                .is_empty());
            assert!(input
                .feed(
                    device,
                    EV_SYN,
                    SYN_REPORT,
                    0,
                    now + Duration::from_millis(3)
                )
                .is_empty());

            let unmatched_release =
                input.feed(device, EV_KEY, code, 0, now + Duration::from_millis(4));
            assert_eq!(unmatched_release[0].input, NormalizedInput::Cancel(control));
            assert!(router.route(&unmatched_release[0], true).is_empty());
            assert!(input
                .tick(now + LONG_PRESS + Duration::from_millis(10))
                .is_empty());
        }
    }

    #[test]
    fn device_loss_and_unknown_reconnect_state_cancel_held_actions() {
        let mut input = InputManager::empty();
        let mut router = ActionRouter::default();
        let now = Instant::now();
        let press = input.feed(NAVIGATION_BUTTONS, EV_KEY, 28, 1, now);
        assert!(router.route(&press[0], true).is_empty());

        let canceled = input.cancel_stale_device(NAVIGATION_BUTTONS);
        assert_eq!(
            canceled[0].input,
            NormalizedInput::Cancel(PhysicalControl::Select)
        );
        assert!(router.route(&canceled[0], true).is_empty());

        // A release after reconnection with an unknown initial key state is
        // not evidence of a confirmed physical release.
        let repeat = input.feed(NAVIGATION_BUTTONS, EV_KEY, 28, 2, now);
        assert_eq!(
            repeat[0].input,
            NormalizedInput::Cancel(PhysicalControl::Select)
        );
        assert!(router.route(&repeat[0], true).is_empty());
        let release = input.feed(NAVIGATION_BUTTONS, EV_KEY, 28, 0, now);
        assert_eq!(
            release[0].input,
            NormalizedInput::Cancel(PhysicalControl::Select)
        );
        assert!(router.route(&release[0], true).is_empty());
    }

    #[test]
    fn cancellation_clears_long_repeat_activation_state() {
        let mut input = InputManager::empty();
        let mut router = ActionRouter::default();
        let start = Instant::now();
        let press = input.feed(NAVIGATION_BUTTONS, EV_KEY, 106, 1, start);
        assert!(router.route(&press[0], true).is_empty());

        let long = input.tick(start + LONG_PRESS + Duration::from_millis(1));
        assert_eq!(router.route(&long[0], true), vec![Action::SeekForward]);
        let canceled = input.feed(
            NAVIGATION_BUTTONS,
            EV_SYN,
            SYN_DROPPED,
            0,
            start + LONG_PRESS + Duration::from_millis(2),
        );
        assert_eq!(
            canceled[0].input,
            NormalizedInput::Cancel(PhysicalControl::Next)
        );
        assert!(router.route(&canceled[0], true).is_empty());

        let repeat = InputEvent {
            device: "timing".into(),
            input: NormalizedInput::Repeat(PhysicalControl::Next),
        };
        assert!(router.route(&repeat, true).is_empty());
        assert!(input
            .feed(
                NAVIGATION_BUTTONS,
                EV_SYN,
                SYN_REPORT,
                0,
                start + LONG_PRESS + Duration::from_millis(3),
            )
            .is_empty());
        let release = input.feed(
            NAVIGATION_BUTTONS,
            EV_KEY,
            106,
            0,
            start + LONG_PRESS + Duration::from_millis(4),
        );
        assert_eq!(
            release[0].input,
            NormalizedInput::Cancel(PhysicalControl::Next)
        );
        assert!(router.route(&release[0], true).is_empty());
    }

    #[test]
    fn input_reconnect_matches_device_identity_when_event_node_changes() {
        let original = Device {
            path: PathBuf::from("/dev/input/event4"),
            name: NAVIGATION_BUTTONS.into(),
            identity: PathBuf::from("/sys/devices/platform/y2/buttons"),
        };
        let replacement = Device {
            path: PathBuf::from("/dev/input/event9"),
            name: NAVIGATION_BUTTONS.into(),
            identity: original.identity.clone(),
        };
        let reused_node = Device {
            path: original.path.clone(),
            name: NAVIGATION_BUTTONS.into(),
            identity: PathBuf::from("/sys/devices/platform/unrelated/buttons"),
        };

        assert_eq!(
            rediscover_device(&original, &[reused_node.clone(), replacement.clone()]),
            Some(replacement)
        );
        assert_eq!(rediscover_device(&original, &[reused_node]), None);
    }
}
