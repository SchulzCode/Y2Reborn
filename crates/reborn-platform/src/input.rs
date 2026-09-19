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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Device {
    pub path: PathBuf,
    pub name: String,
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
            Some(Device {
                path: PathBuf::from("/dev/input").join(e.file_name()),
                name,
            })
        })
        .collect()
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
    pressed: HashMap<PhysicalControl, Pressed>,
    wheel: Option<(bool, Instant, u8)>,
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
            pressed: HashMap::new(),
            wheel: None,
        }
    }

    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            files: vec![],
            pressed: HashMap::new(),
            wheel: None,
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
        self.ingest(device, kind, code, value, now)
    }

    #[cfg(test)]
    pub fn tick(&mut self, now: Instant) -> Vec<InputEvent> {
        self.tick_at(now)
    }

    fn poll_at(&mut self, now: Instant) -> Vec<InputEvent> {
        let mut events = self.tick_at(now);
        let size = std::mem::size_of::<libc::input_event>();
        let mut raw = Vec::new();
        for (device, file, pending) in &mut self.files {
            let mut bytes = [0u8; 1024];
            if let Ok(n) = file.read(&mut bytes) {
                pending.extend_from_slice(&bytes[..n]);
            }
            while pending.len() >= size {
                let offset = size - 8;
                let kind = u16::from_ne_bytes(pending[offset..offset + 2].try_into().unwrap());
                let code = u16::from_ne_bytes(pending[offset + 2..offset + 4].try_into().unwrap());
                let value = i32::from_ne_bytes(pending[offset + 4..offset + 8].try_into().unwrap());
                pending.drain(..size);
                raw.push((device.name.clone(), kind, code, value));
            }
        }
        for (device, kind, code, value) in raw {
            events.extend(self.ingest(&device, kind, code, value, now));
        }
        events.extend(self.tick_at(now));
        events
    }

    fn ingest(
        &mut self,
        device: &str,
        kind: u16,
        code: u16,
        value: i32,
        now: Instant,
    ) -> Vec<InputEvent> {
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
            let steps = match self.wheel {
                Some((last_direction, previous, previous_steps))
                    if last_direction == clockwise
                        && now.saturating_duration_since(previous)
                            <= Duration::from_millis(180) =>
                {
                    raw_steps.max(previous_steps.saturating_add(1)).min(6)
                }
                _ => raw_steps.clamp(1, 2),
            };
            self.wheel = Some((clockwise, now, steps));
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
                if self.pressed.contains_key(&control) {
                    return vec![];
                }
                self.pressed.insert(
                    control,
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
                self.pressed.remove(&control);
                vec![InputEvent {
                    device: device.to_owned(),
                    input: NormalizedInput::Release(control),
                }]
            }
            NormalizedInput::Repeat(control) => vec![InputEvent {
                device: device.to_owned(),
                input: NormalizedInput::Repeat(control),
            }],
            _ => vec![],
        }
    }

    fn tick_at(&mut self, now: Instant) -> Vec<InputEvent> {
        let mut events = vec![];
        let mut remove = vec![];
        for (&control, state) in &mut self.pressed {
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
            if held > Duration::from_secs(60) {
                remove.push(control);
            }
        }
        for control in remove {
            self.pressed.remove(&control);
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
                self.long_pressed.insert(control);
                if !screen_on {
                    return vec![];
                }
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
    if kind == 2 && code == 8 {
        if value == 0 {
            return None;
        }
        return Some(if value >= 0 {
            NormalizedInput::WheelClockwise(value.unsigned_abs().clamp(1, 32) as u8)
        } else {
            NormalizedInput::WheelCounterClockwise(value.unsigned_abs().clamp(1, 32) as u8)
        });
    }
    if kind != 1 || !matches!(value, 0..=2) {
        return None;
    }
    if device == KEYPAD && code == 103 {
        return match value {
            0 => None,
            1 | 2 => Some(NormalizedInput::WheelCounterClockwise(1)),
            _ => None,
        };
    }
    if device == KEYPAD && code == 108 {
        return match value {
            0 => None,
            1 | 2 => Some(NormalizedInput::WheelClockwise(1)),
            _ => None,
        };
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
            map(KEYPAD, 1, 108, 1),
            Some(NormalizedInput::WheelClockwise(1))
        );
        assert_eq!(map("event3", 1, 108, 1), None);
        assert_eq!(
            map(PMIC_KEYS, 1, 116, 1),
            Some(NormalizedInput::Press(PhysicalControl::Power))
        );
    }

    #[test]
    fn wheel_is_always_separate_from_select_and_playback() {
        assert_eq!(
            map(KEYPAD, 1, 108, 1),
            Some(NormalizedInput::WheelClockwise(1))
        );
        let mut router = ActionRouter::default();
        let event = InputEvent {
            device: KEYPAD.into(),
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
        let back = input.feed(NAVIGATION_BUTTONS, 1, 158, 0, now);
        assert_eq!(router.route(&back[0], false), Vec::<Action>::new());
        let power = input.feed(PMIC_KEYS, 1, 116, 0, now);
        assert_eq!(router.route(&power[0], false), vec![Action::ScreenWake]);
    }

    #[test]
    fn sustained_wheel_rotation_accelerates_without_becoming_select() {
        let mut input = InputManager::empty();
        let start = Instant::now();
        let first = input.feed(KEYPAD, 1, 108, 1, start);
        let second = input.feed(KEYPAD, 1, 108, 1, start + Duration::from_millis(90));
        let reset = input.feed(KEYPAD, 1, 108, 1, start + Duration::from_millis(500));
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
}
