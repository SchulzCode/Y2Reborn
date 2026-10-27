//! The vibration motor (#28): the kernel's `regulator-haptic` force-feedback
//! input on the MT6323 VIBR rail. One rumble effect is uploaded and replayed
//! for every tick; the kernel's memless timer ends it.
//!
//! The driver turns the magnitude into the rail voltage between 1.8 and
//! 2.8 V, and the regulator rounds up to its next step: Light is 2.0 V,
//! Strong is the stock vibrator's fixed 2.8 V.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use reborn_core::Haptics;

const DEVICE_NAME: &str = "regulator-haptic";
const EV_FF: u16 = 0x15;
const FF_RUMBLE: u16 = 0x50;
/// One tick. The stock vibrator raises 10..49 ms requests to 50 ms; an ERM
/// motor needs some time to spin up before it is felt at all.
pub const TICK_MS: u16 = 30;
/// Ticks closer than this are dropped, so a fast spin stays a train of
/// ticks instead of one long buzz.
pub const MIN_GAP: Duration = Duration::from_millis(45);

/// `_IOW('E', 0x80, struct ff_effect)`.
const EVIOCSFF: u64 = (1 << 30)
    | ((std::mem::size_of::<libc::ff_effect>() as u64) << 16)
    | ((b'E' as u64) << 8)
    | 0x80;

/// The rumble magnitude for a strength; `None` is off.
pub fn magnitude(strength: Haptics) -> Option<u16> {
    match strength {
        Haptics::Off => None,
        // Any magnitude up to a fifth of the range lands on the 2.0 V step.
        Haptics::Light => Some(0x2000),
        Haptics::Strong => Some(0xffff),
    }
}

/// Whether a tick at `now` would play after the one at `last`.
pub fn due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| now.saturating_duration_since(last) >= MIN_GAP)
}

/// The `/dev/input/eventN` of the motor, when the kernel has one.
pub fn device() -> Option<PathBuf> {
    fs::read_dir("/sys/class/input")
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("event"))
        .find(|e| {
            fs::read_to_string(e.path().join("device/name"))
                .is_ok_and(|name| name.trim() == DEVICE_NAME)
        })
        .map(|e| PathBuf::from("/dev/input").join(e.file_name()))
}

pub struct Motor {
    file: File,
    /// The uploaded effect and the strength it was uploaded with.
    effect: Option<(i16, Haptics)>,
    last: Option<Instant>,
}

impl Motor {
    pub fn open() -> io::Result<Self> {
        let path = device().ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        Ok(Self {
            file,
            effect: None,
            last: None,
        })
    }

    /// Play one tick at `strength`. Returns whether it played: off, or too
    /// soon after the previous tick, plays nothing.
    pub fn tick(&mut self, strength: Haptics, now: Instant) -> io::Result<bool> {
        if magnitude(strength).is_none() || !due(self.last, now) {
            return Ok(false);
        }
        let id = match self.effect {
            Some((id, uploaded)) if uploaded == strength => id,
            other => self.upload(other.map(|(id, _)| id), strength)?,
        };
        // SAFETY: input_event is plain data; all-zero is a valid value.
        let mut event: libc::input_event = unsafe { std::mem::zeroed() };
        event.type_ = EV_FF;
        event.code = id as u16;
        event.value = 1;
        // SAFETY: the bytes of a fully initialized repr(C) struct.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&event as *const libc::input_event).cast::<u8>(),
                std::mem::size_of::<libc::input_event>(),
            )
        };
        self.file.write_all(bytes)?;
        self.last = Some(now);
        Ok(true)
    }

    /// Upload the rumble (a new one, or replace `id`) and return its id.
    fn upload(&mut self, id: Option<i16>, strength: Haptics) -> io::Result<i16> {
        let strong = magnitude(strength).unwrap_or(0);
        // SAFETY: ff_effect is plain data; all-zero is a valid value.
        let mut effect: libc::ff_effect = unsafe { std::mem::zeroed() };
        effect.type_ = FF_RUMBLE;
        effect.id = id.unwrap_or(-1);
        effect.replay.length = TICK_MS;
        // ff_rumble_effect { strong_magnitude, weak_magnitude } opens the union.
        effect.u[0] = u32::from(strong) as _;
        // SAFETY: EVIOCSFF reads and updates exactly one ff_effect.
        let ret = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                EVIOCSFF as _,
                &mut effect as *mut libc::ff_effect,
            )
        };
        if ret < 0 {
            self.effect = None;
            return Err(io::Error::last_os_error());
        }
        self.effect = Some((effect.id, strength));
        Ok(effect.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strengths_map_to_the_rail_steps() {
        assert_eq!(magnitude(Haptics::Off), None);
        // regulator-haptic: 1.8 V + 1.0 V * magnitude / 65536.
        let volts = |m: u16| 1.8 + f64::from(m) / 65536.0;
        assert!(volts(magnitude(Haptics::Light).unwrap()) <= 2.0);
        assert!(volts(magnitude(Haptics::Light).unwrap()) > 1.8);
        assert!(volts(magnitude(Haptics::Strong).unwrap()) > 2.0);
    }

    #[test]
    fn close_ticks_are_dropped() {
        let t = Instant::now();
        assert!(due(None, t));
        assert!(!due(Some(t), t + Duration::from_millis(20)));
        assert!(due(Some(t), t + MIN_GAP));
        assert!(TICK_MS < MIN_GAP.as_millis() as u16);
    }

    #[test]
    fn ioctl_matches_the_kernel_struct() {
        // ARM: 14 header bytes, padding, then the 28-byte union.
        #[cfg(target_pointer_width = "32")]
        assert_eq!(std::mem::size_of::<libc::ff_effect>(), 44);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<libc::ff_effect>(), 48);
        assert_eq!(EVIOCSFF & 0xffff, 0x4580);
    }
}
