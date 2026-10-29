#![forbid(unsafe_code)]

use reborn_core::{Action, NormalizedInput, PhysicalControl, ScreenOffButtons};
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
const RESCAN_RATE: Duration = Duration::from_secs(1);
const MAX_READS_PER_DEVICE: usize = 16;

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
        if direction != clockwise || gap >= WHEEL_RESET_US {
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
const KEY_POWER: u16 = 116;

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
            let identity = hardware_identity(fs::canonicalize(e.path().join("device")).ok()?);
            Some(Device {
                path: PathBuf::from("/dev/input").join(e.file_name()),
                name,
                identity,
            })
        })
        .collect()
}

// inputN is allocated anew after driver rebind; the physical parent is stable.
fn hardware_identity(path: PathBuf) -> PathBuf {
    if path.file_name().is_some_and(|name| {
        name.to_str().is_some_and(|name| {
            name.strip_prefix("input")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
        })
    }) && path
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == "input")
    {
        return path.parent().unwrap().parent().unwrap().to_owned();
    }
    path
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
    pub kernel_mono_us: Option<u64>,
}

#[derive(Default, serde::Serialize)]
pub struct InputCapture {
    pub records: Vec<InputTrace>,
    pub truncated: bool,
    pub raw_wheel: u64,
    pub accepted_wheel: u64,
    pub rejected_wheel: u64,
    pub syn_dropped: u64,
    pub device_losses: u64,
    pub rescans: u64,
    pub reopens: u64,
    pub read_limit_hits: u64,
}

#[derive(serde::Serialize)]
pub struct InputTrace {
    device: String,
    kernel_mono_us: Option<u64>,
    received_mono_us: Option<u64>,
    kind: u16,
    code: u16,
    value: i32,
    normalized: Vec<NormalizedInput>,
    cadence_streak: Option<u32>,
    cadence_interval_us: Option<u64>,
}

pub fn monotonic_us() -> Option<u64> {
    crate::native::monotonic_seconds().map(|s| (s * 1e6) as u64)
}

#[derive(Clone, Copy, Debug)]
struct Pressed {
    started: Instant,
    stamp_us: Option<u64>,
    last_repeat: Instant,
    long_emitted: bool,
    /// Found held after a whole-system sleep (`resumed`), never seen as a
    /// press: it can become a hold, but its short release is no action.
    resumed: bool,
}

/// The only layer that knows Linux event types, key codes, and device names.
/// It converts those events into stable physical events and owns all timing.
pub struct InputManager {
    files: Vec<(Device, File, Vec<u8>, bool)>,
    next_rescan: Instant,
    capture: Option<InputCapture>,
    pressed: HashMap<(String, PhysicalControl), Pressed>,
    wheel: HashMap<String, WheelCadence>,
    dropping: HashSet<String>,
    epoch: Instant,
}

impl InputManager {
    /// Saved press ages and wheel cadence do not cross whole-system sleep.
    /// Unmatched releases remain Cancel events; evdev overflow recovery is kept.
    pub fn resumed(&mut self) {
        // Closing evdev drops all old edges, even beyond the bounded read
        // budget. Rediscover current identities and retry if resume hides one.
        self.files.clear();
        self.pressed.clear();
        self.wheel.clear();
        self.dropping.clear();
        self.rescan(devices());
        self.next_rescan = Instant::now();
        // F57: Power held through the wake sends no press after the reopen,
        // so that hold never turned the player off; held on, the PMIC's own
        // long-press reset cuts the power (suspected for c077cc2e).
        let held = self
            .files
            .iter()
            .filter(|(device, file, _, _)| {
                device.name == PMIC_KEYS
                    && crate::native::evdev_key_down(file, KEY_POWER) == Some(true)
            })
            .map(|(device, _, _, _)| device.identity.to_string_lossy().into_owned())
            .collect();
        self.held_through_resume(held, Instant::now());
    }

    /// Power still down after a resume starts a hold from now. The wake
    /// press itself must not also act: its short release is only a Cancel.
    fn held_through_resume(&mut self, identities: Vec<String>, now: Instant) {
        for identity in identities {
            self.pressed.insert(
                (identity, PhysicalControl::Power),
                Pressed {
                    started: now,
                    stamp_us: None,
                    last_repeat: now,
                    long_emitted: false,
                    resumed: true,
                },
            );
        }
    }

    pub fn open() -> Self {
        Self {
            files: devices()
                .into_iter()
                .filter_map(|d| {
                    let (f, monotonic) = open_device(&d.path)?;
                    Some((d, f, vec![], monotonic))
                })
                .collect(),
            next_rescan: Instant::now() + RESCAN_RATE,
            capture: None,
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
            next_rescan: Instant::now() + RESCAN_RATE,
            capture: None,
            pressed: HashMap::new(),
            wheel: HashMap::new(),
            dropping: HashSet::new(),
            epoch: Instant::now(),
        }
    }

    pub fn count(&self) -> usize {
        self.files.len()
    }

    /// Opt-in, memory-bounded diagnostics for the existing control monitor.
    pub fn begin_capture(&mut self) {
        self.capture = Some(InputCapture::default());
    }

    pub fn finish_capture(&mut self) -> Option<InputCapture> {
        self.capture.take()
    }

    pub fn reset_wheel(&mut self) {
        self.wheel.clear();
    }

    /// Full suspend owners call this at their boundary, then route its Cancel
    /// records. Reopening evdev discards pre-sleep queues and unknown held state.
    pub fn reset(&mut self) -> Vec<InputEvent> {
        let identities = self
            .pressed
            .iter()
            .map(|((identity, _), _)| identity.clone())
            .collect::<Vec<_>>();
        let mut canceled = vec![];
        for identity in identities {
            canceled.extend(self.cancel_stale_device(&identity));
        }
        self.files.clear();
        self.wheel.clear();
        self.dropping.clear();
        self.rescan(devices());
        self.next_rescan = Instant::now() + RESCAN_RATE;
        canceled
    }

    pub fn poll(&mut self) -> Vec<InputEvent> {
        self.poll_at(Instant::now())
    }
    pub fn wait(&self, timeout: Duration) -> std::io::Result<()> {
        self.wait_with(timeout, None).map(|_| ())
    }
    /// A button is held: its repeat and long press need the loop every 15 ms.
    pub fn held(&self) -> bool {
        !self.pressed.is_empty()
    }
    /// Wait for input or one of `others` (the control wake) to be
    /// readable, or `timeout`; true when something is.
    pub fn wait_with<'a>(
        &'a self,
        timeout: Duration,
        others: impl IntoIterator<Item = std::os::fd::BorrowedFd<'a>>,
    ) -> std::io::Result<bool> {
        use std::os::fd::AsFd;
        // Preserve the existing repeat and long-press cadence while a button
        // is held; an idle wheel/Select/Back/Power wakes the poll immediately.
        let timeout = if self.pressed.is_empty() {
            timeout
        } else {
            timeout.min(Duration::from_millis(15))
        };
        crate::native::wait_readable(
            self.files
                .iter()
                .map(|(_, file, _, _)| file.as_fd())
                .chain(others),
            timeout,
        )
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
        if now >= self.next_rescan {
            if let Some(capture) = &mut self.capture {
                capture.rescans += 1;
            }
            self.rescan(devices());
            self.next_rescan = now + RESCAN_RATE;
        }
        let mut events = vec![];
        let size = std::mem::size_of::<libc::input_event>();
        let mut raw = Vec::new();
        let mut reopen = Vec::new();
        let mono_us = crate::native::monotonic_seconds().map(|s| (s * 1e6) as u64);
        let mut drain_incomplete = HashSet::new();
        for (index, (device, file, pending, monotonic)) in self.files.iter_mut().enumerate() {
            let mut bytes = [0u8; 1024];
            // Drain a real evdev queue, bounded so a faulty producer cannot
            // starve rendering. No separate semantic channel queues motion.
            for read in 0..MAX_READS_PER_DEVICE {
                match file.read(&mut bytes) {
                    Ok(0) => {
                        pending.clear();
                        reopen.push(index);
                        break;
                    }
                    Ok(n) => pending.extend_from_slice(&bytes[..n]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        pending.clear();
                        reopen.push(index);
                        break;
                    }
                }
                if read + 1 == MAX_READS_PER_DEVICE {
                    drain_incomplete.insert(device.identity.to_string_lossy().into_owned());
                    if let Some(capture) = &mut self.capture {
                        capture.read_limit_hits += 1;
                    }
                }
            }
            while pending.len() >= size {
                let offset = size - 8;
                let kind = u16::from_ne_bytes(pending[offset..offset + 2].try_into().unwrap());
                let code = u16::from_ne_bytes(pending[offset + 2..offset + 4].try_into().unwrap());
                let value = i32::from_ne_bytes(pending[offset + 4..offset + 8].try_into().unwrap());
                let stamp = if *monotonic {
                    record_timestamp_us(&pending[..offset])
                } else {
                    None
                };
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
        // Preserve cross-device ordering (not event-node enumeration order).
        raw.sort_by_key(|(_, _, record)| record.stamp_us);
        for (device, identity, record) in raw {
            let at = event_instant(now, mono_us, record.stamp_us);
            events.extend(self.ingest(&device, &identity, record, at));
        }
        if !reopen.is_empty() {
            reopen.sort_unstable();
            reopen.dedup();
            for index in reopen.into_iter().rev() {
                if index >= self.files.len() {
                    continue;
                }
                let device = self.files[index].0.clone();
                let identity = device.identity.to_string_lossy().into_owned();
                events.extend(self.cancel_stale_device(&identity));
                self.files.remove(index);
                if let Some(capture) = &mut self.capture {
                    capture.device_losses += 1;
                }
            }
        }
        // A queued release must be read before a held key can age/repeat.
        events.extend(self.tick_ready(now, &drain_incomplete));
        events
    }

    fn rescan(&mut self, candidates: Vec<Device>) {
        for device in candidates {
            let opened = self.files.iter().any(|(existing, _, _, _)| {
                rediscover_device(existing, std::slice::from_ref(&device)).is_some()
            });
            if !opened {
                if let Some((file, monotonic)) = open_device(&device.path) {
                    self.files.push((device, file, Vec::new(), monotonic));
                    if let Some(capture) = &mut self.capture {
                        capture.reopens += 1;
                    }
                }
            }
        }
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
        self.dropping.remove(identity);
        controls
            .into_iter()
            .map(|(_, control)| InputEvent {
                device: "input-recovery".into(),
                kernel_mono_us: None,
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
        let mut events = self.ingest_record(device, identity, record, now);
        for event in &mut events {
            event.kernel_mono_us = record.stamp_us;
        }
        if let Some(capture) = &mut self.capture {
            if capture.records.len() < 2048 {
                capture.records.push(InputTrace {
                    device: device.to_owned(),
                    kernel_mono_us: record.stamp_us,
                    received_mono_us: monotonic_us(),
                    kind: record.kind,
                    code: record.code,
                    value: record.value,
                    normalized: events.iter().map(|e| e.input).collect(),
                    cadence_streak: self.wheel.get(identity).map(|c| c.streak),
                    cadence_interval_us: self.wheel.get(identity).map(|c| c.interval_us),
                });
            } else {
                capture.truncated = true;
            }
            if matches!(
                map(device, record.kind, record.code, record.value),
                Some(
                    NormalizedInput::WheelClockwise(_) | NormalizedInput::WheelCounterClockwise(_)
                )
            ) {
                capture.raw_wheel += 1;
                if events.is_empty() {
                    capture.rejected_wheel += 1;
                } else {
                    capture.accepted_wheel += 1;
                }
            }
            if record.kind == EV_SYN && record.code == SYN_DROPPED {
                capture.syn_dropped += 1;
            }
        }
        events
    }

    fn ingest_record(
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
                let canceled = self.cancel_stale_device(identity);
                self.dropping.insert(identity.to_owned());
                return canceled;
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
        if kind == EV_KEY && !matches!(value, 0..=2) {
            // A malformed key record cannot leave a previous press aging or
            // repeating indefinitely. Recovery never synthesizes activation.
            return self.cancel_stale_device(identity);
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
            let steps = if stamp_us.is_none() {
                // Receipt time cannot distinguish separate queued detents.
                cadence.reset();
                raw_detents.min(4)
            } else if raw_detents > 1 {
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
                kernel_mono_us: None,
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
                        stamp_us,
                        last_repeat: now,
                        long_emitted: false,
                        resumed: false,
                    },
                );
                vec![InputEvent {
                    device: device.to_owned(),
                    kernel_mono_us: None,
                    input: NormalizedInput::Press(control),
                }]
            }
            NormalizedInput::Release(control) => {
                let state = self.pressed.remove(&(identity.to_owned(), control));
                let mut out = vec![];
                let mut long = false;
                if let Some(state) = state {
                    let held = stamp_us
                        .zip(state.stamp_us)
                        .and_then(|(end, start)| end.checked_sub(start))
                        .map(Duration::from_micros)
                        .unwrap_or_else(|| now.saturating_duration_since(state.started));
                    long = state.long_emitted;
                    if !state.long_emitted && held >= LONG_PRESS && is_long_press_control(control) {
                        long = true;
                        out.push(InputEvent {
                            device: device.to_owned(),
                            kernel_mono_us: None,
                            input: NormalizedInput::LongPress(control),
                        });
                    }
                }
                // A wake press released before its hold began did its job.
                let short_wake = state.is_some_and(|state| state.resumed) && !long;
                out.push(InputEvent {
                    device: device.to_owned(),
                    kernel_mono_us: None,
                    input: if state.is_some() && !short_wake {
                        NormalizedInput::Release(control)
                    } else {
                        NormalizedInput::Cancel(control)
                    },
                });
                out
            }
            NormalizedInput::Repeat(control)
                if self.pressed.contains_key(&(identity.to_owned(), control)) =>
            {
                // InputManager is the sole repeat authority, including when a
                // kernel device happens to advertise EV_REP.
                vec![]
            }
            NormalizedInput::Repeat(control) => vec![InputEvent {
                device: device.to_owned(),
                kernel_mono_us: None,
                input: NormalizedInput::Cancel(control),
            }],
            _ => vec![],
        }
    }

    #[cfg(test)]
    fn tick_at(&mut self, now: Instant) -> Vec<InputEvent> {
        self.tick_ready(now, &HashSet::new())
    }

    fn tick_ready(&mut self, now: Instant, blocked: &HashSet<String>) -> Vec<InputEvent> {
        let mut events = vec![];
        for (key, state) in &mut self.pressed {
            if blocked.contains(&key.0) {
                continue;
            }
            let control = key.1;
            let held = now.saturating_duration_since(state.started);
            if !state.long_emitted && held >= LONG_PRESS && is_long_press_control(control) {
                state.long_emitted = true;
                state.last_repeat = now;
                events.push(InputEvent {
                    device: "timing".into(),
                    kernel_mono_us: None,
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
                    kernel_mono_us: None,
                    input: NormalizedInput::Repeat(control),
                });
            }
        }
        events
    }
}

fn event_instant(now: Instant, mono_us: Option<u64>, stamp_us: Option<u64>) -> Instant {
    mono_us
        .zip(stamp_us)
        .and_then(|(mono, stamp)| mono.checked_sub(stamp))
        .and_then(|age| now.checked_sub(Duration::from_micros(age)))
        .unwrap_or(now)
}

fn open_device(path: &std::path::Path) -> Option<(File, bool)> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let monotonic = crate::evdev_monotonic_clock(&file);
    Some((file, monotonic))
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
    dark_navigation: HashSet<PhysicalControl>,
    power_started_on: Option<bool>,
    screen_on: Option<bool>,
    precise_wheel: bool,
    /// The on-screen keyboard is open: the wheel's four buttons are arrow keys.
    keyboard: bool,
    /// The open keyboard has a list to browse (the library search): the
    /// wheel turns reach it.
    keyboard_wheel: bool,
    /// Buttons pressed as arrow keys; their hold and release belong to the
    /// keyboard even when it closed meanwhile.
    keyboard_held: HashSet<PhysicalControl>,
    /// Held Previous/Next move every other repeat (180 ms), not every 90 ms.
    keyboard_repeat: u32,
    /// Back was pressed as the Up arrow on the library search's keys: held,
    /// it leaves the search even when that press folded the keys (#51).
    keyboard_search_back: bool,
    /// What the wheel and its buttons do while the screen is off (a setting).
    screen_off_buttons: ScreenOffButtons,
}

impl ActionRouter {
    /// The on-screen keyboard opened or closed. While it is open Back,
    /// Play/Pause, Previous and Next move the focus (up, down, left, right)
    /// as soon as they are pressed; holding Back cancels, holding Previous
    /// or Next keeps moving; the wheel does nothing. Select is unchanged
    /// (hold: done). Volume and Power keep their meaning.
    ///
    /// The library search's Up from the top row folds its keys into the
    /// results at the press; holding that Back still leaves the search: it
    /// goes Back (from the results to the keys) and then Home (Cancel).
    pub fn set_keyboard(&mut self, open: bool) {
        self.keyboard = open;
    }

    /// The open keyboard's page has results to browse (the library search,
    /// Beta 8.32): its wheel turns are wheel turns. The buttons stay arrows.
    pub fn set_keyboard_wheel(&mut self, browses: bool) {
        self.keyboard_wheel = browses;
    }

    /// The setting for the wheel and its buttons while the screen is off.
    pub fn set_screen_off_buttons(&mut self, mode: ScreenOffButtons) {
        self.screen_off_buttons = mode;
    }

    /// A button that does nothing while the screen is off: its press, hold
    /// and release are all swallowed, even when the screen wakes meanwhile.
    fn rests_in_the_dark(&self, control: PhysicalControl) -> bool {
        use PhysicalControl::*;
        match self.screen_off_buttons {
            ScreenOffButtons::Locked => {
                matches!(control, Select | Back | Previous | Next | PlayPause)
            }
            ScreenOffButtons::Buttons => matches!(control, Select | Back),
            ScreenOffButtons::WheelAndButtons => control == Back,
        }
    }

    /// A screen transition inside an already-decoded batch must not carry
    /// invisible wheel cadence into the newly visible UI.
    pub fn begin_batch(&mut self) {
        self.precise_wheel = false;
    }

    pub fn route(&mut self, event: &InputEvent, screen_on: bool) -> Vec<Action> {
        if self.screen_on != Some(screen_on) {
            self.screen_on = Some(screen_on);
            self.precise_wheel = true;
        }
        if let Some(actions) = self.keyboard_route(event.input, screen_on) {
            return actions;
        }
        match event.input {
            NormalizedInput::WheelClockwise(steps) if screen_on => {
                vec![Action::WheelClockwise(if self.precise_wheel {
                    1
                } else {
                    steps
                })]
            }
            NormalizedInput::WheelCounterClockwise(steps) if screen_on => {
                vec![Action::WheelCounterClockwise(if self.precise_wheel {
                    1
                } else {
                    steps
                })]
            }
            // In the dark the wheel is volume, one step a detent, if allowed.
            NormalizedInput::WheelClockwise(_)
                if self.screen_off_buttons == ScreenOffButtons::WheelAndButtons =>
            {
                vec![Action::VolumeUp]
            }
            NormalizedInput::WheelCounterClockwise(_)
                if self.screen_off_buttons == ScreenOffButtons::WheelAndButtons =>
            {
                vec![Action::VolumeDown]
            }
            NormalizedInput::WheelClockwise(_) | NormalizedInput::WheelCounterClockwise(_) => {
                vec![]
            }
            NormalizedInput::Press(control) => {
                if control == PhysicalControl::Power {
                    self.power_started_on = Some(screen_on);
                }
                if !screen_on && self.rests_in_the_dark(control) {
                    self.dark_navigation.insert(control);
                }
                match control {
                    PhysicalControl::VolumeUp => vec![Action::VolumeUp],
                    PhysicalControl::VolumeDown => vec![Action::VolumeDown],
                    _ => vec![],
                }
            }
            NormalizedInput::Repeat(control) => match control {
                PhysicalControl::VolumeUp => vec![Action::VolumeUp],
                PhysicalControl::VolumeDown => vec![Action::VolumeDown],
                _ if self.dark_navigation.contains(&control) => vec![],
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
                if self.dark_navigation.contains(&control)
                    || (!screen_on
                        && !matches!(
                            control,
                            PhysicalControl::Previous
                                | PhysicalControl::Next
                                | PhysicalControl::Power
                        ))
                {
                    return vec![];
                }
                match control {
                    PhysicalControl::Select => vec![Action::ContextMenu],
                    PhysicalControl::Back => vec![Action::Home],
                    PhysicalControl::Previous => vec![Action::SeekBackward],
                    PhysicalControl::Next => vec![Action::SeekForward],
                    PhysicalControl::PlayPause => vec![Action::ShowNowPlaying],
                    PhysicalControl::Power if screen_on => vec![Action::PowerHold],
                    PhysicalControl::Power => vec![Action::ScreenWake, Action::PowerHold],
                    _ => vec![],
                }
            }
            NormalizedInput::Cancel(control) => {
                let held = self.long_pressed.remove(&control);
                self.dark_navigation.remove(&control);
                if control == PhysicalControl::Power {
                    self.power_started_on = None;
                    if held {
                        return vec![Action::PowerHoldCancel];
                    }
                }
                vec![]
            }
            NormalizedInput::Release(control) => {
                let power_started_on = if control == PhysicalControl::Power {
                    self.power_started_on.take()
                } else {
                    None
                };
                if self.dark_navigation.remove(&control) {
                    self.long_pressed.remove(&control);
                    return vec![];
                }
                if self.long_pressed.remove(&control) {
                    return if control == PhysicalControl::Power {
                        vec![Action::PowerHoldCancel]
                    } else {
                        vec![]
                    };
                }
                match control {
                    PhysicalControl::Select if screen_on => vec![Action::Select],
                    // The centre plays and pauses in the dark, as on Now Playing.
                    PhysicalControl::Select
                        if self.screen_off_buttons == ScreenOffButtons::WheelAndButtons =>
                    {
                        vec![Action::PlayPause]
                    }
                    PhysicalControl::Back if screen_on => vec![Action::Back],
                    PhysicalControl::Previous => vec![Action::PreviousTrack],
                    PhysicalControl::Next => vec![Action::NextTrack],
                    PhysicalControl::PlayPause => vec![Action::PlayPause],
                    PhysicalControl::Power => {
                        if power_started_on.is_some_and(|started| started != screen_on) {
                            vec![]
                        } else if screen_on {
                            vec![Action::PowerShort]
                        } else {
                            vec![Action::ScreenWake]
                        }
                    }
                    _ => vec![],
                }
            }
        }
    }

    fn keyboard_route(&mut self, input: NormalizedInput, screen_on: bool) -> Option<Vec<Action>> {
        let arrow = |control| match control {
            PhysicalControl::Back => Some(Action::NavigateUp),
            PhysicalControl::PlayPause => Some(Action::NavigateDown),
            PhysicalControl::Previous => Some(Action::NavigateLeft),
            PhysicalControl::Next => Some(Action::NavigateRight),
            _ => None,
        };
        match input {
            NormalizedInput::Press(control) if self.keyboard && screen_on => {
                let action = arrow(control)?;
                self.keyboard_held.insert(control);
                self.keyboard_repeat = 0;
                if control == PhysicalControl::Back {
                    self.keyboard_search_back = self.keyboard_wheel;
                }
                Some(vec![action])
            }
            NormalizedInput::Repeat(control) if self.keyboard_held.contains(&control) => {
                self.keyboard_repeat += 1;
                Some(match arrow(control) {
                    Some(action) if self.keyboard && self.keyboard_repeat.is_multiple_of(2) => {
                        vec![action]
                    }
                    _ => vec![],
                })
            }
            NormalizedInput::LongPress(control) if self.keyboard_held.contains(&control) => {
                Some(match control {
                    PhysicalControl::Back if self.keyboard => vec![Action::Home],
                    // The press folded the search's keys into its results.
                    PhysicalControl::Back if self.keyboard_search_back && screen_on => {
                        vec![Action::Back, Action::Home]
                    }
                    _ => vec![],
                })
            }
            NormalizedInput::Release(control) | NormalizedInput::Cancel(control)
                if self.keyboard_held.remove(&control) =>
            {
                Some(vec![])
            }
            NormalizedInput::WheelClockwise(_) | NormalizedInput::WheelCounterClockwise(_)
                if self.keyboard && !self.keyboard_wheel =>
            {
                Some(vec![])
            }
            _ => None,
        }
    }

    pub fn reset(&mut self) {
        self.keyboard_held.clear();
        self.long_pressed.clear();
        self.dark_navigation.clear();
        self.power_started_on = None;
        self.screen_on = None;
        self.precise_wheel = false;
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
            kernel_mono_us: None,
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

    /// Keyboard mode: the four wheel buttons are arrow keys at once on press,
    /// hold Back cancels, held Previous/Next keep moving, the wheel is inert,
    /// Select keeps its meaning and nothing reaches playback.
    #[test]
    fn keyboard_mode_turns_wheel_buttons_into_arrow_keys() {
        let mut input = InputManager::empty();
        let now = std::time::Instant::now();
        let mut router = ActionRouter::default();
        router.set_keyboard(true);
        let mut route = |input: &mut InputManager, code: u16, value: i32| -> Vec<Action> {
            input
                .feed(NAVIGATION_BUTTONS, EV_KEY, code, value, now)
                .iter()
                .flat_map(|e| router.route(e, true))
                .collect()
        };
        for (code, arrow) in [
            (158, Action::NavigateUp),
            (164, Action::NavigateDown),
            (105, Action::NavigateLeft),
            (106, Action::NavigateRight),
        ] {
            assert_eq!(route(&mut input, code, 1), vec![arrow]);
            assert_eq!(
                route(&mut input, code, 0),
                Vec::<Action>::new(),
                "no playback on release"
            );
        }
        assert_eq!(route(&mut input, 28, 1), Vec::<Action>::new());
        assert_eq!(route(&mut input, 28, 0), vec![Action::Select]);
        // Held: Back cancels at the long press; Next repeats every other tick.
        let mut router = ActionRouter::default();
        router.set_keyboard(true);
        let mut input = InputManager::empty();
        let start = std::time::Instant::now();
        let mut actions = vec![];
        for (code, value) in [(158, 1), (106, 1)] {
            for e in input.feed(NAVIGATION_BUTTONS, EV_KEY, code, value, start) {
                actions.extend(router.route(&e, true));
            }
        }
        for ms in (0..=1400).step_by(10) {
            for e in input.tick_at(start + Duration::from_millis(ms)) {
                actions.extend(router.route(&e, true));
            }
        }
        assert_eq!(actions.iter().filter(|a| **a == Action::Home).count(), 1);
        let rights = actions
            .iter()
            .filter(|a| **a == Action::NavigateRight)
            .count();
        // One on press, then from 400 ms every 180 ms up to 1.4 s.
        assert!((5..=8).contains(&rights), "{actions:?}");
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::SeekForward | Action::NextTrack)));
        // The keyboard closed while Next was held: its release stays silent.
        router.set_keyboard(false);
        for e in input.feed(
            NAVIGATION_BUTTONS,
            EV_KEY,
            106,
            0,
            start + Duration::from_millis(1500),
        ) {
            assert_eq!(router.route(&e, true), Vec::<Action>::new());
        }
        // The wheel does nothing while the keyboard is open.
        router.set_keyboard(true);
        let wheel = InputEvent {
            device: CLICK_WHEEL.into(),
            kernel_mono_us: None,
            input: NormalizedInput::WheelClockwise(1),
        };
        assert_eq!(router.route(&wheel, true), Vec::<Action>::new());
        // The library search's keyboard browses its results with the wheel;
        // its buttons stay arrow keys.
        router.set_keyboard_wheel(true);
        assert_eq!(router.route(&wheel, true), vec![Action::WheelClockwise(1)]);
        let mut input = InputManager::empty();
        let press = input.feed(NAVIGATION_BUTTONS, EV_KEY, 158, 1, now);
        assert_eq!(router.route(&press[0], true), vec![Action::NavigateUp]);
        let release = input.feed(NAVIGATION_BUTTONS, EV_KEY, 158, 0, now);
        assert_eq!(router.route(&release[0], true), Vec::<Action>::new());
        router.set_keyboard_wheel(false);
        // Closed again, the buttons are playback controls as before.
        router.set_keyboard(false);
        let mut input = InputManager::empty();
        let _ = input.feed(NAVIGATION_BUTTONS, EV_KEY, 106, 1, now);
        let release = input.feed(NAVIGATION_BUTTONS, EV_KEY, 106, 0, now);
        assert_eq!(router.route(&release[0], true), vec![Action::NextTrack]);
    }

    /// #51: holding Back on the library search's keys leaves the search,
    /// also when its press (the Up arrow off the top row) already folded
    /// the keys into the results: Back reopens them, Home cancels. Other
    /// keyboards keep a hold of Back that only cancels while they are open.
    #[test]
    fn held_back_leaves_the_search_even_when_its_press_folded_the_keys() {
        let hold = |search: bool, folds: bool| -> Vec<Action> {
            let mut input = InputManager::empty();
            let mut router = ActionRouter::default();
            router.set_keyboard(true);
            router.set_keyboard_wheel(search);
            let start = std::time::Instant::now();
            let mut actions = vec![];
            for e in input.feed(NAVIGATION_BUTTONS, EV_KEY, 158, 1, start) {
                actions.extend(router.route(&e, true));
            }
            if folds {
                router.set_keyboard(false);
                router.set_keyboard_wheel(false);
            }
            for ms in (0..=800).step_by(10) {
                for e in input.tick_at(start + Duration::from_millis(ms)) {
                    actions.extend(router.route(&e, true));
                }
            }
            let at = start + Duration::from_millis(900);
            for e in input.feed(NAVIGATION_BUTTONS, EV_KEY, 158, 0, at) {
                actions.extend(router.route(&e, true));
            }
            actions
        };
        assert_eq!(hold(true, false), vec![Action::NavigateUp, Action::Home]);
        assert_eq!(
            hold(true, true),
            vec![Action::NavigateUp, Action::Back, Action::Home]
        );
        assert_eq!(hold(false, false), vec![Action::NavigateUp, Action::Home]);
        assert_eq!(hold(false, true), vec![Action::NavigateUp]);
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
            kernel_mono_us: None,
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
        let record = |value, stamp_us| Record {
            kind: EV_KEY,
            code: 28,
            value,
            stamp_us: Some(stamp_us),
        };
        let _ = input.ingest(
            NAVIGATION_BUTTONS,
            NAVIGATION_BUTTONS,
            record(1, 1_000_000),
            start,
        );
        // A physical 100-ms press read late must still be short.
        let release = input.ingest(
            NAVIGATION_BUTTONS,
            NAVIGATION_BUTTONS,
            record(0, 1_100_000),
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
            kernel_mono_us: None,
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
    #[test]
    fn queued_kernel_timestamps_preserve_a_real_long_hold() {
        let mut input = InputManager::empty();
        let now = Instant::now();
        for (value, at_us) in [(1, 1_000_000), (0, 1_700_000)] {
            let events = input.ingest(
                NAVIGATION_BUTTONS,
                NAVIGATION_BUTTONS,
                Record {
                    kind: EV_KEY,
                    code: 28,
                    value,
                    stamp_us: Some(at_us),
                },
                now,
            );
            if value == 0 {
                assert_eq!(
                    events.iter().map(|e| e.input).collect::<Vec<_>>(),
                    vec![
                        NormalizedInput::LongPress(PhysicalControl::Select),
                        NormalizedInput::Release(PhysicalControl::Select)
                    ]
                );
            }
        }
    }

    #[test]
    fn hardware_repeat_cannot_double_the_owned_volume_timer() {
        let mut input = InputManager::empty();
        let now = Instant::now();
        input.feed(KEYPAD, EV_KEY, 115, 1, now);
        assert!(input
            .feed(KEYPAD, EV_KEY, 115, 2, now + REPEAT_START)
            .is_empty());
        assert_eq!(input.tick(now + REPEAT_START).len(), 1);
        input.feed(KEYPAD, EV_KEY, 115, 0, now + REPEAT_START + REPEAT_RATE);
        assert!(input.tick(now + Duration::from_secs(10)).is_empty());
    }

    #[test]
    fn screen_off_long_transport_seeks_without_a_short_track_skip() {
        let mut router = ActionRouter::default();
        let event = |input| InputEvent {
            device: NAVIGATION_BUTTONS.into(),
            kernel_mono_us: None,
            input,
        };
        router.route(&event(NormalizedInput::Press(PhysicalControl::Next)), false);
        assert_eq!(
            router.route(
                &event(NormalizedInput::LongPress(PhysicalControl::Next)),
                false
            ),
            vec![Action::SeekForward]
        );
        assert!(router
            .route(
                &event(NormalizedInput::Release(PhysicalControl::Next)),
                false
            )
            .is_empty());
    }

    #[test]
    fn power_wake_gesture_does_not_sleep_an_externally_woken_display() {
        let mut router = ActionRouter::default();
        let event = |input| InputEvent {
            device: PMIC_KEYS.into(),
            kernel_mono_us: None,
            input,
        };
        router.route(
            &event(NormalizedInput::Press(PhysicalControl::Power)),
            false,
        );
        assert!(router
            .route(
                &event(NormalizedInput::Release(PhysicalControl::Power)),
                true
            )
            .is_empty());
    }
    fn record_bytes(kind: u16, code: u16, value: i32, at_us: u64) -> Vec<u8> {
        let mut out = vec![];
        let (sec, usec) = (at_us / 1_000_000, at_us % 1_000_000);
        if std::mem::size_of::<libc::input_event>() == 24 {
            out.extend_from_slice(&sec.to_ne_bytes());
            out.extend_from_slice(&usec.to_ne_bytes());
        } else {
            out.extend_from_slice(&(sec as u32).to_ne_bytes());
            out.extend_from_slice(&(usec as u32).to_ne_bytes());
        }
        out.extend_from_slice(&kind.to_ne_bytes());
        out.extend_from_slice(&code.to_ne_bytes());
        out.extend_from_slice(&value.to_ne_bytes());
        out
    }

    fn queued_input(name: &str) -> (InputManager, std::os::unix::net::UnixStream) {
        let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let fd: std::os::fd::OwnedFd = reader.into();
        let mut input = InputManager::empty();
        input.files.push((
            Device {
                path: PathBuf::from("test-only"),
                name: name.into(),
                identity: PathBuf::from(name),
            },
            File::from(fd),
            vec![],
            true,
        ));
        (input, writer)
    }

    #[test]
    fn a_queued_wheel_burst_is_drained_without_replay_or_coalescing_buttons() {
        use std::io::Write;
        let (mut input, mut writer) = queued_input(CLICK_WHEEL);
        let mut bytes = vec![];
        for i in 0..100 {
            for (kind, code, value) in [
                (EV_KEY, KEY_DOWN, 1),
                (EV_SYN, SYN_REPORT, 0),
                (EV_KEY, KEY_DOWN, 0),
                (EV_SYN, SYN_REPORT, 0),
            ] {
                bytes.extend(record_bytes(kind, code, value, 1_000_000 + i * 250_000));
            }
        }
        writer.write_all(&bytes).unwrap();
        let events = input.poll();
        assert_eq!(events.len(), 100);
        assert!(events
            .iter()
            .all(|e| e.input == NormalizedInput::WheelClockwise(1)));
        assert!(
            input.poll().is_empty(),
            "nothing remains to replay after the spin"
        );
    }

    #[test]
    fn draining_a_cross_device_batch_preserves_kernel_order() {
        use std::io::Write;
        let (mut input, mut nav) = queued_input(NAVIGATION_BUTTONS);
        let (power_input, mut power) = queued_input(PMIC_KEYS);
        input.files.extend(power_input.files);
        nav.write_all(&record_bytes(EV_KEY, 28, 1, 1_100_000))
            .unwrap();
        nav.write_all(&record_bytes(EV_KEY, 28, 0, 1_200_000))
            .unwrap();
        power
            .write_all(&record_bytes(EV_KEY, 116, 1, 1_000_000))
            .unwrap();
        power
            .write_all(&record_bytes(EV_KEY, 116, 0, 1_300_000))
            .unwrap();
        let events = input.poll();
        assert_eq!(
            events.iter().map(|e| e.input).collect::<Vec<_>>(),
            vec![
                NormalizedInput::Press(PhysicalControl::Power),
                NormalizedInput::Press(PhysicalControl::Select),
                NormalizedInput::Release(PhysicalControl::Select),
                NormalizedInput::Release(PhysicalControl::Power)
            ]
        );
    }

    #[test]
    fn split_records_and_device_eof_do_not_leave_a_key_stuck() {
        use std::io::Write;
        let (mut input, mut writer) = queued_input(NAVIGATION_BUTTONS);
        let bytes = record_bytes(EV_KEY, 28, 1, monotonic_us().unwrap());
        writer.write_all(&bytes[..7]).unwrap();
        assert!(input.poll().is_empty());
        writer.write_all(&bytes[7..]).unwrap();
        assert_eq!(
            input.poll()[0].input,
            NormalizedInput::Press(PhysicalControl::Select)
        );
        drop(writer);
        assert_eq!(
            input.poll()[0].input,
            NormalizedInput::Cancel(PhysicalControl::Select)
        );
        assert_eq!(input.count(), 0);
        assert!(input.tick(Instant::now() + LONG_PRESS).is_empty());
    }

    #[test]
    fn read_budget_defers_timers_until_a_queued_release_is_consumed() {
        use std::io::Write;
        let (mut input, mut writer) = queued_input(NAVIGATION_BUTTONS);
        let now = Instant::now();
        input.feed(NAVIGATION_BUTTONS, EV_KEY, 28, 1, now);
        let mut bytes = vec![];
        for _ in 0..800 {
            bytes.extend(record_bytes(EV_SYN, SYN_REPORT, 0, 1_000_000));
        }
        bytes.extend(record_bytes(EV_KEY, 28, 0, 1_100_000));
        writer.write_all(&bytes).unwrap();
        assert!(input.poll_at(now + LONG_PRESS).is_empty());
        let events = input.poll_at(now + LONG_PRESS);
        assert!(matches!(
            events.last().unwrap().input,
            NormalizedInput::Release(PhysicalControl::Select)
        ));
        assert!(input.tick(now + Duration::from_secs(10)).is_empty());
    }

    #[test]
    fn rebind_identity_uses_physical_parent_and_startup_open_failures_retry() {
        assert_eq!(
            hardware_identity(PathBuf::from(
                "/sys/devices/platform/navigation-keys/input/input2"
            )),
            hardware_identity(PathBuf::from(
                "/sys/devices/platform/navigation-keys/input/input29"
            ))
        );
        let (mut input, writer) = queued_input(NAVIGATION_BUTTONS);
        let candidate = input.files[0].0.clone();
        input.rescan(vec![candidate]);
        assert_eq!(
            input.count(),
            1,
            "never open a second reader for an existing identity"
        );
        drop(writer);
        input.poll();
        let path = std::env::temp_dir().join(format!("reborn-input-rescan-{}", std::process::id()));
        let candidate = Device {
            path: path.clone(),
            name: NAVIGATION_BUTTONS.into(),
            identity: PathBuf::from("physical-test-parent"),
        };
        input.rescan(vec![candidate.clone()]);
        assert_eq!(input.count(), 0);
        File::create(&path).unwrap();
        input.rescan(vec![candidate]);
        assert_eq!(
            input.count(),
            1,
            "a failed initial open is retried on rescan"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn exact_pause_threshold_and_unstamped_batches_keep_fine_control() {
        let mut cadence = WheelCadence::default();
        for i in 0..10 {
            cadence.detent(true, i * 40_000);
        }
        assert_eq!(cadence.detent(true, 360_000 + WHEEL_RESET_US), Some(1));
        let mut input = InputManager::empty();
        let now = Instant::now();
        for _ in 0..10 {
            assert_eq!(
                input.feed(CLICK_WHEEL, EV_KEY, KEY_DOWN, 1, now)[0].input,
                NormalizedInput::WheelClockwise(1)
            );
        }
    }

    #[test]
    fn volume_repeat_is_bounded_and_seek_hold_emits_once_at_threshold() {
        let mut input = InputManager::empty();
        let now = Instant::now();
        input.feed(KEYPAD, EV_KEY, 115, 1, now);
        assert!(input
            .tick(now + REPEAT_START - Duration::from_millis(1))
            .is_empty());
        assert_eq!(input.tick(now + REPEAT_START).len(), 1);
        assert!(input
            .tick(now + REPEAT_START + Duration::from_millis(1))
            .is_empty());
        assert_eq!(
            input.tick(now + Duration::from_secs(5)).len(),
            1,
            "no catch-up burst"
        );
        let mut input = InputManager::empty();
        input.feed(NAVIGATION_BUTTONS, EV_KEY, 106, 1, now);
        assert_eq!(
            input
                .tick(now + LONG_PRESS)
                .iter()
                .map(|e| e.input)
                .collect::<Vec<_>>(),
            vec![NormalizedInput::LongPress(PhysicalControl::Next)]
        );
        assert_eq!(
            input
                .tick(now + LONG_PRESS + REPEAT_RATE)
                .iter()
                .map(|e| e.input)
                .collect::<Vec<_>>(),
            vec![NormalizedInput::Repeat(PhysicalControl::Next)]
        );
    }

    #[test]
    fn screen_off_buttons_follow_the_setting_and_volume_and_power_always_work() {
        use PhysicalControl::*;
        let nav = |input| InputEvent {
            device: NAVIGATION_BUTTONS.into(),
            input,
            kernel_mono_us: None,
        };
        let wheel = |input| InputEvent {
            device: CLICK_WHEEL.into(),
            input,
            kernel_mono_us: None,
        };
        // A short press of `control` in the dark.
        let tap = |router: &mut ActionRouter, control| {
            let mut out = router.route(&nav(NormalizedInput::Press(control)), false);
            out.extend(router.route(&nav(NormalizedInput::Release(control)), false));
            out
        };
        let turn = |router: &mut ActionRouter| {
            let mut out = router.route(&wheel(NormalizedInput::WheelClockwise(3)), false);
            out.extend(router.route(&wheel(NormalizedInput::WheelCounterClockwise(1)), false));
            out
        };
        let hold = |router: &mut ActionRouter, control| {
            let mut out = router.route(&nav(NormalizedInput::Press(control)), false);
            out.extend(router.route(&nav(NormalizedInput::LongPress(control)), false));
            out.extend(router.route(&nav(NormalizedInput::Repeat(control)), false));
            out.extend(router.route(&nav(NormalizedInput::Release(control)), false));
            out
        };
        // Buttons (the default): transport buttons work, wheel and centre rest.
        let mut router = ActionRouter::default();
        assert_eq!(tap(&mut router, Next), vec![Action::NextTrack]);
        assert_eq!(tap(&mut router, PlayPause), vec![Action::PlayPause]);
        assert_eq!(
            hold(&mut router, Previous),
            vec![Action::SeekBackward, Action::SeekBackward]
        );
        assert!(tap(&mut router, Select).is_empty());
        assert!(turn(&mut router).is_empty());
        // Locked: nothing on the wheel, a hold does not seek either.
        router.set_screen_off_buttons(ScreenOffButtons::Locked);
        for control in [Select, Back, Previous, Next, PlayPause] {
            assert!(tap(&mut router, control).is_empty(), "{control:?}");
            assert!(hold(&mut router, control).is_empty(), "{control:?} held");
        }
        assert!(turn(&mut router).is_empty());
        // A locked press stays swallowed when the screen wakes before release.
        router.route(&nav(NormalizedInput::Press(Next)), false);
        assert!(router
            .route(&nav(NormalizedInput::Release(Next)), true)
            .is_empty());
        // Wheel and Buttons: the wheel is volume (one step a detent), the
        // centre plays and pauses; Back still rests.
        router.set_screen_off_buttons(ScreenOffButtons::WheelAndButtons);
        assert_eq!(
            turn(&mut router),
            vec![Action::VolumeUp, Action::VolumeDown]
        );
        assert_eq!(tap(&mut router, Select), vec![Action::PlayPause]);
        assert_eq!(tap(&mut router, Next), vec![Action::NextTrack]);
        assert!(tap(&mut router, Back).is_empty());
        assert!(hold(&mut router, Select).is_empty(), "no menu in the dark");
        // Every mode: volume keys and Power.
        for mode in [
            ScreenOffButtons::Locked,
            ScreenOffButtons::Buttons,
            ScreenOffButtons::WheelAndButtons,
        ] {
            router.set_screen_off_buttons(mode);
            assert_eq!(
                router.route(&nav(NormalizedInput::Press(VolumeUp)), false),
                vec![Action::VolumeUp],
                "{mode:?}"
            );
            router.route(&nav(NormalizedInput::Release(VolumeUp)), false);
            assert_eq!(
                tap(&mut router, Power),
                vec![Action::ScreenWake],
                "{mode:?}"
            );
        }
        // With the screen on nothing changes.
        router.set_screen_off_buttons(ScreenOffButtons::Locked);
        router.route(&nav(NormalizedInput::Press(Select)), true);
        assert_eq!(
            router.route(&nav(NormalizedInput::Release(Select)), true),
            vec![Action::Select]
        );
    }

    #[test]
    fn dark_navigation_stays_suppressed_through_wake_and_power_cancel() {
        let mut router = ActionRouter::default();
        let event = |input| InputEvent {
            device: NAVIGATION_BUTTONS.into(),
            input,
            kernel_mono_us: None,
        };
        router.route(
            &event(NormalizedInput::Press(PhysicalControl::Select)),
            false,
        );
        assert!(router
            .route(
                &event(NormalizedInput::Release(PhysicalControl::Select)),
                true
            )
            .is_empty());
        router.route(
            &event(NormalizedInput::Press(PhysicalControl::Power)),
            false,
        );
        router.route(
            &event(NormalizedInput::Cancel(PhysicalControl::Power)),
            false,
        );
        router.route(&event(NormalizedInput::Press(PhysicalControl::Power)), true);
        assert_eq!(
            router.route(
                &event(NormalizedInput::Release(PhysicalControl::Power)),
                true
            ),
            vec![Action::PowerShort]
        );
    }

    #[test]
    fn diagnostic_capture_is_opt_in_and_memory_bounded() {
        let mut input = InputManager::empty();
        assert!(input.finish_capture().is_none());
        input.begin_capture();
        for i in 0..2200 {
            detent(&mut input, KEY_DOWN, 1000 + i * 250);
        }
        let capture = input.finish_capture().unwrap();
        assert_eq!(capture.records.len(), 2048);
        assert!(capture.truncated);
        assert_eq!(capture.accepted_wheel, 2200);
        assert_eq!(capture.rejected_wheel, 0);
        assert!(input.finish_capture().is_none());
    }
    #[test]
    fn wheel_cadence_from_a_dark_batch_cannot_accelerate_after_wake() {
        let mut router = ActionRouter::default();
        let event = |input| InputEvent {
            device: CLICK_WHEEL.into(),
            input,
            kernel_mono_us: None,
        };
        router.begin_batch();
        assert!(router
            .route(&event(NormalizedInput::WheelClockwise(4)), false)
            .is_empty());
        assert_eq!(
            router.route(&event(NormalizedInput::WheelClockwise(4)), true),
            vec![Action::WheelClockwise(1)]
        );
        assert_eq!(
            router.route(&event(NormalizedInput::WheelClockwise(4)), true),
            vec![Action::WheelClockwise(1)]
        );
        router.begin_batch();
        assert_eq!(
            router.route(&event(NormalizedInput::WheelClockwise(2)), true),
            vec![Action::WheelClockwise(2)]
        );
    }

    #[test]
    fn every_short_and_long_button_is_exclusive_and_duplicate_downs_are_ignored() {
        for (device, code, short, long) in [
            (NAVIGATION_BUTTONS, 28, Action::Select, Action::ContextMenu),
            (NAVIGATION_BUTTONS, 158, Action::Back, Action::Home),
            (
                NAVIGATION_BUTTONS,
                105,
                Action::PreviousTrack,
                Action::SeekBackward,
            ),
            (
                NAVIGATION_BUTTONS,
                106,
                Action::NextTrack,
                Action::SeekForward,
            ),
            (
                NAVIGATION_BUTTONS,
                164,
                Action::PlayPause,
                Action::ShowNowPlaying,
            ),
            (PMIC_KEYS, 116, Action::PowerShort, Action::PowerHold),
        ] {
            for held in [false, true] {
                let mut input = InputManager::empty();
                let mut router = ActionRouter::default();
                let now = Instant::now();
                for event in input.feed(device, EV_KEY, code, 1, now) {
                    assert!(router.route(&event, true).is_empty());
                }
                assert!(input
                    .feed(device, EV_KEY, code, 1, now + Duration::from_millis(1))
                    .is_empty());
                let mut actions = vec![];
                if held {
                    for event in input.tick(now + LONG_PRESS) {
                        actions.extend(router.route(&event, true));
                    }
                }
                for event in input.feed(
                    device,
                    EV_KEY,
                    code,
                    0,
                    now + if held {
                        LONG_PRESS + Duration::from_millis(1)
                    } else {
                        Duration::from_millis(50)
                    },
                ) {
                    actions.extend(router.route(&event, true));
                }
                let mut expected = vec![if held { long } else { short }];
                if held && long == Action::PowerHold {
                    // Releasing a Power hold ends the turn-off countdown.
                    expected.push(Action::PowerHoldCancel);
                }
                assert_eq!(actions, expected);
            }
        }
    }
    #[test]
    fn full_reset_cancels_holds_and_forgets_a_drop_before_reopen() {
        let mut input = InputManager::empty();
        let now = Instant::now();
        input.feed(PMIC_KEYS, EV_KEY, 116, 1, now);
        input
            .wheel
            .entry(CLICK_WHEEL.into())
            .or_default()
            .detent(true, 1_000_000);
        input.dropping.insert(CLICK_WHEEL.into());
        let canceled = input.reset();
        assert_eq!(
            canceled.iter().map(|e| e.input).collect::<Vec<_>>(),
            vec![NormalizedInput::Cancel(PhysicalControl::Power)]
        );
        assert!(input.pressed.is_empty());
        assert!(input.wheel.is_empty());
        assert!(input.dropping.is_empty());
        assert!(input.tick(now + Duration::from_secs(10)).is_empty());
    }

    #[test]
    fn evdev_overflow_forgets_wheel_acceleration_until_a_fresh_detent() {
        let mut input = InputManager::empty();
        for i in 0..12 {
            detent(&mut input, KEY_DOWN, 1_000 + i * 40);
        }
        let now = Instant::now();
        input.feed(CLICK_WHEEL, EV_SYN, SYN_DROPPED, 0, now);
        assert!(input.feed(CLICK_WHEEL, EV_KEY, KEY_DOWN, 1, now).is_empty());
        input.feed(CLICK_WHEEL, EV_SYN, SYN_REPORT, 0, now);
        assert_eq!(
            detent(&mut input, KEY_DOWN, 1_490),
            Some(NormalizedInput::WheelClockwise(1))
        );
    }
    #[test]
    fn malformed_key_values_cancel_pressed_timers_without_activation() {
        for (device, code, control) in [
            (NAVIGATION_BUTTONS, 28, PhysicalControl::Select),
            (NAVIGATION_BUTTONS, 158, PhysicalControl::Back),
            (KEYPAD, 115, PhysicalControl::VolumeUp),
        ] {
            for value in [-1, 3] {
                let mut input = InputManager::empty();
                let now = Instant::now();
                input.feed(device, EV_KEY, code, 1, now);
                let events = input.feed(
                    device,
                    EV_KEY,
                    code,
                    value,
                    now + Duration::from_millis(100),
                );
                assert_eq!(
                    events.iter().map(|e| e.input).collect::<Vec<_>>(),
                    vec![NormalizedInput::Cancel(control)]
                );
                assert!(input.tick(now + Duration::from_secs(10)).is_empty());
            }
        }
    }

    #[test]
    fn power_hold_works_with_the_screen_off_and_its_release_is_consumed() {
        let mut router = ActionRouter::default();
        let event = |input| InputEvent {
            device: PMIC_KEYS.into(),
            input,
            kernel_mono_us: None,
        };
        assert_eq!(
            router.route(
                &event(NormalizedInput::LongPress(PhysicalControl::Power)),
                false
            ),
            vec![Action::ScreenWake, Action::PowerHold]
        );
        assert_eq!(
            router.route(
                &event(NormalizedInput::Release(PhysicalControl::Power)),
                true
            ),
            vec![Action::PowerHoldCancel]
        );
    }

    #[test]
    fn resume_drops_old_press_age_and_wheel_cadence() {
        let mut input = InputManager::empty();
        let now = Instant::now();
        input.feed(PMIC_KEYS, EV_KEY, 116, 1, now);
        input.resumed();
        assert!(input.tick(now + Duration::from_secs(60)).is_empty());
        assert_eq!(
            input.feed(PMIC_KEYS, EV_KEY, 116, 0, now)[0].input,
            NormalizedInput::Cancel(PhysicalControl::Power)
        );
    }

    #[test]
    fn power_held_through_a_wake_turns_the_player_off_and_a_short_wake_press_does_nothing() {
        // F57 (c077cc2e): the wake press, still down after the reopen, is a
        // hold from the resume on; it never becomes a short press.
        let route = |input: &mut InputManager, router: &mut ActionRouter, at, lit| {
            input
                .tick(at)
                .iter()
                .chain(input.feed(PMIC_KEYS, EV_KEY, 116, 0, at).iter())
                .flat_map(|event| router.route(event, lit))
                .collect::<Vec<_>>()
        };
        for lit in [true, false] {
            let mut input = InputManager::empty();
            let mut router = ActionRouter::default();
            let now = Instant::now();
            input.held_through_resume(vec![PMIC_KEYS.into()], now);
            let hold = input
                .tick(now + LONG_PRESS)
                .iter()
                .flat_map(|event| router.route(event, lit))
                .collect::<Vec<_>>();
            assert_eq!(
                hold,
                if lit {
                    vec![Action::PowerHold]
                } else {
                    vec![Action::ScreenWake, Action::PowerHold]
                }
            );
            assert_eq!(
                route(&mut input, &mut router, now + Duration::from_secs(1), true),
                vec![Action::PowerHoldCancel]
            );

            let mut input = InputManager::empty();
            let mut router = ActionRouter::default();
            input.held_through_resume(vec![PMIC_KEYS.into()], now);
            let short = now + LONG_PRESS / 2;
            assert!(input.tick(short).is_empty());
            assert_eq!(
                input.feed(PMIC_KEYS, EV_KEY, 116, 0, short)[0].input,
                NormalizedInput::Cancel(PhysicalControl::Power)
            );
            input.held_through_resume(vec![PMIC_KEYS.into()], now);
            assert!(route(&mut input, &mut router, short, lit).is_empty());
            // A repeated press record while held changes nothing.
            input.held_through_resume(vec![PMIC_KEYS.into()], now);
            assert!(input.feed(PMIC_KEYS, EV_KEY, 116, 1, short).is_empty());
            assert!(route(&mut input, &mut router, short, lit).is_empty());
            // The next press is an ordinary press again.
            assert_eq!(
                input.feed(PMIC_KEYS, EV_KEY, 116, 1, short)[0].input,
                NormalizedInput::Press(PhysicalControl::Power)
            );
        }
    }

    #[test]
    fn resume_does_not_carry_an_overflow_drop_into_new_input() {
        let mut input = InputManager::empty();
        let now = Instant::now();
        input.feed(PMIC_KEYS, EV_KEY, 116, 1, now);
        input.feed(PMIC_KEYS, EV_SYN, SYN_DROPPED, 0, now);
        input.resumed();
        assert_eq!(
            input.feed(PMIC_KEYS, EV_KEY, 116, 1, now)[0].input,
            NormalizedInput::Press(PhysicalControl::Power)
        );
    }
    #[test]
    fn resume_closes_a_pre_sleep_queue_larger_than_the_read_budget() {
        use std::io::Write;
        let (mut input, mut writer) = queued_input(PMIC_KEYS);
        let mut bytes = vec![];
        for _ in 0..800 {
            bytes.extend(record_bytes(EV_SYN, SYN_REPORT, 0, 1_000_000));
        }
        bytes.extend(record_bytes(EV_KEY, 116, 1, 1_000_000));
        bytes.extend(record_bytes(EV_KEY, 116, 0, 1_100_000));
        writer.write_all(&bytes).unwrap();
        input.resumed();
        assert!(input.poll().is_empty());
        assert!(input
            .tick(Instant::now() + Duration::from_secs(60))
            .is_empty());
    }
}
