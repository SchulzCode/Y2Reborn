#![forbid(unsafe_code)]
use reborn_core::Action;
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
};
#[derive(Clone, Serialize)]
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
            if ![
                "Y2 navigation buttons",
                "mtk-pmic-keys",
                "mt6582-keypad",
                "APT32F click-wheel",
            ]
            .contains(&name.as_str())
            {
                return None;
            }
            Some(Device {
                path: PathBuf::from("/dev/input").join(e.file_name()),
                name,
            })
        })
        .collect()
}
pub fn map(kind: u16, code: u16, value: i32) -> Option<Action> {
    if kind == 2 && code == 8 {
        return Some(Action::Wheel(value));
    }
    if kind != 1 || !(value == 1 || value == 2) {
        return None;
    }
    // Navigation/volume may repeat while held. One press must not repeatedly
    // activate a row, reverse a radio toggle, or start/stop discovery.
    if value == 2 && !matches!(code, 103 | 108 | 115 | 114) {
        return None;
    }
    match code {
        103 => Some(Action::Up),
        108 => Some(Action::Down),
        105 => Some(Action::Left),
        106 => Some(Action::Right),
        28 => Some(Action::Select),
        158 => Some(Action::Back),
        139 => Some(Action::Menu),
        164 | 57 => Some(Action::PlayPause),
        165 => Some(Action::Previous),
        163 => Some(Action::Next),
        115 => Some(Action::VolumeUp),
        114 => Some(Action::VolumeDown),
        116 => Some(Action::ScreenToggle),
        _ => None,
    }
}
pub struct Input {
    files: Vec<(Device, File, Vec<u8>)>,
}
impl Input {
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
        }
    }
    pub fn count(&self) -> usize {
        self.files.len()
    }
    pub fn poll(&mut self) -> Vec<(String, Action)> {
        let size = std::mem::size_of::<libc::input_event>();
        let mut events = vec![];
        for (d, f, pending) in &mut self.files {
            let mut b = [0u8; 1024];
            if let Ok(n) = f.read(&mut b) {
                pending.extend_from_slice(&b[..n]);
            }
            while pending.len() >= size {
                let offset = size - 8;
                let kind = u16::from_ne_bytes(pending[offset..offset + 2].try_into().unwrap());
                let code = u16::from_ne_bytes(pending[offset + 2..offset + 4].try_into().unwrap());
                let val = i32::from_ne_bytes(pending[offset + 4..offset + 8].try_into().unwrap());
                if let Some(a) = map(kind, code, val) {
                    events.push((d.name.clone(), a))
                }
                pending.drain(..size);
            }
        }
        events
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn holding_select_cannot_toggle_a_radio_back_off() {
        assert_eq!(map(1, 28, 1), Some(Action::Select));
        for code in [28, 106, 116, 164] {
            assert_eq!(map(1, code, 2), None);
        }
        assert_eq!(map(1, 108, 2), Some(Action::Down));
        assert_eq!(map(1, 115, 2), Some(Action::VolumeUp));
    }
    #[test]
    fn physical_keys() {
        for c in [
            103, 108, 105, 106, 28, 158, 139, 164, 165, 163, 115, 114, 116,
        ] {
            assert!(map(1, c, 1).is_some());
            assert!(map(1, c, 0).is_none());
        }
        assert_eq!(map(2, 8, -2), Some(Action::Wheel(-2)));
        assert!(map(0, 0, 0).is_none());
    }
}
