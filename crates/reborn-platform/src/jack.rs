//! The headphone jack (#28 speaker). Y2Linux polls the CS43131's plug
//! detector while the DAC is powered (screen on, or playing) and reports it on
//! the card's jack input, "Y2Audio Headphone" (SW_HEADPHONE_INSERT). A plug
//! turns a playing speaker off in the kernel already; Reborn follows with the
//! headphone route, and pauses on an unplug as the stock player does.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

const DEVICE_NAME: &str = "Y2Audio Headphone";
/// The kernel's poll state, `<bus>-0030/y2_hp/jack` ("state=-1" until it has
/// read the detector once). The DAC sits on bus 0 or 1, by probe order.
const DEVICES: &str = "/sys/bus/i2c/devices";
const EV_SW: u16 = 0x05;
const SW_HEADPHONE_INSERT: u16 = 0x02;
/// `_IOC(_IOC_READ, 'E', 0x1b, 8)`: EVIOCGSW into an 8-byte bitmap.
const EVIOCGSW_8: u64 = (2 << 30) | (8 << 16) | ((b'E' as u64) << 8) | 0x1b;

pub struct Jack {
    file: File,
}

/// The `/dev/input/eventN` of the jack, when the kernel has one.
fn device() -> Option<PathBuf> {
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

/// The DAC's jack state file on whichever bus it probed.
fn state_path(devices: &Path) -> Option<PathBuf> {
    let mut found = fs::read_dir(devices)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.ends_with("-0030")))
        .map(|e| e.path().join("y2_hp/jack"))
        .filter(|p| p.is_file())
        .collect::<Vec<_>>();
    found.sort();
    found.into_iter().next()
}

/// Whether the kernel's poll has read the detector yet (`state=` 0 or 1).
pub fn known(state: &str) -> bool {
    state
        .split_whitespace()
        .find_map(|field| field.strip_prefix("state="))
        .and_then(|value| value.parse::<i32>().ok())
        .is_some_and(|value| value >= 0)
}

/// The last SW_HEADPHONE_INSERT value in a run of raw `input_event`s.
pub fn last_switch(events: &[u8]) -> Option<bool> {
    let size = std::mem::size_of::<libc::input_event>();
    events.chunks_exact(size).rev().find_map(|raw| {
        // SAFETY: a whole input_event of plain data, read unaligned.
        let event: libc::input_event = unsafe { std::ptr::read_unaligned(raw.as_ptr().cast()) };
        (event.type_ == EV_SW && event.code == SW_HEADPHONE_INSERT).then_some(event.value != 0)
    })
}

impl Jack {
    pub fn open() -> io::Result<Self> {
        let path = device().ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        Ok(Self { file })
    }

    /// Headphones in now; None until the kernel has read the detector.
    pub fn current(&self) -> Option<bool> {
        let state = state_path(Path::new(DEVICES)).and_then(|p| fs::read_to_string(p).ok());
        if !state.is_some_and(|s| known(&s)) {
            return None;
        }
        let mut bits = [0u8; 8];
        // SAFETY: EVIOCGSW writes at most the 8 bytes its size names.
        let ret = unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGSW_8 as _, bits.as_mut_ptr()) };
        (ret >= 0).then_some(bits[0] & (1 << SW_HEADPHONE_INSERT) != 0)
    }

    /// The newest reported state since the last call, if one came.
    pub fn poll(&mut self) -> Option<bool> {
        let mut buffer = [0u8; 24 * 16];
        let mut latest = None;
        loop {
            match self.file.read(&mut buffer) {
                Ok(0) | Err(_) => return latest,
                Ok(n) => latest = last_switch(&buffer[..n]).or(latest),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: u16, code: u16, value: i32) -> Vec<u8> {
        // SAFETY: plain data; all-zero is valid.
        let mut e: libc::input_event = unsafe { std::mem::zeroed() };
        e.type_ = kind;
        e.code = code;
        e.value = value;
        let size = std::mem::size_of::<libc::input_event>();
        // SAFETY: the bytes of a fully initialized repr(C) struct.
        unsafe { std::slice::from_raw_parts((&e as *const libc::input_event).cast::<u8>(), size) }
            .to_vec()
    }

    #[test]
    fn the_last_headphone_switch_wins() {
        let mut run = event(EV_SW, SW_HEADPHONE_INSERT, 1);
        run.extend(event(0, 0, 0)); // SYN_REPORT
        assert_eq!(last_switch(&run), Some(true));
        run.extend(event(EV_SW, SW_HEADPHONE_INSERT, 0));
        assert_eq!(last_switch(&run), Some(false));
        // Other switches (line out, mechanical) say nothing about headphones.
        assert_eq!(last_switch(&event(EV_SW, 0x07, 1)), None);
    }

    #[test]
    fn the_state_is_found_on_either_bus() {
        let root = std::env::temp_dir().join(format!("reborn-jack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("1-0051")).unwrap();
        fs::create_dir_all(root.join("0-0030")).unwrap(); // a DAC without the poll
        assert_eq!(state_path(&root), None);
        fs::create_dir_all(root.join("0-0030/y2_hp")).unwrap();
        fs::write(root.join("0-0030/y2_hp/jack"), "state=1\n").unwrap();
        assert_eq!(state_path(&root), Some(root.join("0-0030/y2_hp/jack")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_until_the_kernel_read_the_detector() {
        assert!(!known("state=-1 empty_reads=0 speaker_offs=0\n"));
        assert!(known("state=0 empty_reads=3 speaker_offs=0\n"));
        assert!(known("state=1 empty_reads=0 speaker_offs=1\n"));
        assert!(!known(""));
    }
}
