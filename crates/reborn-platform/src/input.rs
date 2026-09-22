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
    wheel: Option<(String, bool, Instant, u8)>,
    dropping: HashSet<String>,
}

impl InputManager {
    pub fn open() -> Self {
        Self {
            files: devices()
                .into_iter()
                .filter_map(|d| {
                    let f = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                        .open(&d.path)
                        .ok()?;
                    Some((d, f, vec![]))
                })
                .collect(),
            missing: Vec::new(),
            pressed: HashMap::new(),
            wheel: None,
            dropping: HashSet::new(),
        }
    }

    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            files: vec![],
            missing: Vec::new(),
            pressed: HashMap::new(),
            wheel: None,
            dropping: HashSet::new(),
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
        self.ingest(device, device, kind, code, value, now)
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
                pending.drain(..size);
                raw.push((
                    device.name.clone(),
                    device.identity.to_string_lossy().into_owned(),
                    kind,
                    code,
                    value,
                ));
            }
        }
        for (device, identity, kind, code, value) in raw {
            events.extend(self.ingest(&device, &identity, kind, code, value, now));
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
                    if let Ok(file) = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                        .open(&replacement.path)
                    {
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
            match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&replacement.path)
            {
                Ok(file) => self.files.push((replacement, file, Vec::new())),
                Err(_) => still_missing.push(device),
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
        if self
            .wheel
            .as_ref()
            .is_some_and(|(device, _, _, _)| device == identity)
        {
            self.wheel = None;
        }
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
        kind: u16,
        code: u16,
        value: i32,
        now: Instant,
    ) -> Vec<InputEvent> {
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
            let raw_steps = match input {
                NormalizedInput::WheelClockwise(steps)
                | NormalizedInput::WheelCounterClockwise(steps) => steps,
                _ => 1,
            };
            let steps = match self.wheel.as_ref() {
                Some((last_device, last_direction, previous, previous_steps))
                    if last_device == identity
                        && *last_direction == clockwise
                        && now.saturating_duration_since(*previous)
                            <= Duration::from_millis(180) =>
                {
                    raw_steps.max(previous_steps.saturating_add(1)).min(6)
                }
                _ => raw_steps.clamp(1, 2),
            };
            self.wheel = Some((identity.to_owned(), clockwise, now, steps));
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
            return match value {
                0 => None,
                1 | 2 => Some(if clockwise {
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

    #[test]
    fn sustained_wheel_rotation_accelerates_without_becoming_select() {
        let mut input = InputManager::empty();
        let start = Instant::now();
        let first = input.feed(CLICK_WHEEL, EV_KEY, KEY_DOWN, 1, start);
        let second = input.feed(
            CLICK_WHEEL,
            EV_KEY,
            KEY_DOWN,
            1,
            start + Duration::from_millis(90),
        );
        let reset = input.feed(
            CLICK_WHEEL,
            EV_KEY,
            KEY_DOWN,
            1,
            start + Duration::from_millis(500),
        );
        assert_eq!(first[0].input, NormalizedInput::WheelClockwise(1));
        assert!(matches!(second[0].input, NormalizedInput::WheelClockwise(steps) if steps >= 2));
        assert_eq!(reset[0].input, NormalizedInput::WheelClockwise(1));
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
