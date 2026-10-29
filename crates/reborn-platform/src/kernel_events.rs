//! Incremental diagnostic history. Only the watchdog reads the kernel ring.
use std::{collections::VecDeque, fs::File, io::Read, os::unix::fs::OpenOptionsExt};

#[derive(Default)]
pub struct KernelEvents {
    file: Option<File>,
    history: VecDeque<String>,
}
impl KernelEvents {
    /// The history after the records since the last poll, when they added
    /// to it (most polls read only records that are no faults).
    pub fn poll(&mut self) -> Option<Vec<String>> {
        if self.file.is_none() {
            self.file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(0o4000)
                .open("/dev/kmsg")
                .ok();
        }
        let file = self.file.as_mut()?;
        drain(file, &mut self.history).then(|| self.history.iter().cloned().collect())
    }
}
/// Whether a record joined the history.
fn drain(reader: &mut impl Read, history: &mut VecDeque<String>) -> bool {
    let mut added = false;
    let mut buf = [0; 4096];
    for _ in 0..2048 {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let line = String::from_utf8_lossy(&buf[..n]);
                let lower = line.to_lowercase();
                if [
                    "lima", "drm", "gpu", "alsa", "snd_", "mmc", "ext4", "suspend",
                ]
                .iter()
                .any(|s| lower.contains(s))
                    && ["error", "fault", "hang", "timeout", "corrupt"]
                        .iter()
                        .any(|s| lower.contains(s))
                    && !lower.contains("calibration")
                {
                    history.push_back(line.chars().take(512).collect());
                    added = true;
                    if history.len() > 32 {
                        history.pop_front();
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            // kmsg resets its cursor on ring overrun; the next read continues
            // from the oldest retained record. Keep the per-poll work bound.
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => continue,
            Err(_) => break,
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Records(VecDeque<Vec<u8>>);
    impl Read for Records {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let Some(record) = self.0.pop_front() else {
                return Err(std::io::ErrorKind::WouldBlock.into());
            };
            out[..record.len()].copy_from_slice(&record);
            Ok(record.len())
        }
    }
    #[test]
    fn incremental_history_retains_new_errors_without_replaying_old_records() {
        let mut records = Records(VecDeque::from([
            b"drm ready".to_vec(),
            b"mmc calibration error".to_vec(),
            b"lima fault old".to_vec(),
        ]));
        let mut history = VecDeque::new();
        assert!(drain(&mut records, &mut history));
        assert!(!drain(&mut records, &mut history));
        assert_eq!(history, ["lima fault old"]);
        records.0.push_back(b"drm ready again".to_vec());
        assert!(!drain(&mut records, &mut history));
        records.0.push_back(b"EXT4 ERROR new".to_vec());
        assert!(drain(&mut records, &mut history));
        assert_eq!(history, ["lima fault old", "EXT4 ERROR new"]);
        for n in 0..40 {
            records.0.push_back(format!("mmc timeout {n}").into_bytes());
        }
        drain(&mut records, &mut history);
        assert_eq!(history.len(), 32);
        assert_eq!(history.front().unwrap(), "mmc timeout 8");
        assert_eq!(history.back().unwrap(), "mmc timeout 39");
    }
}
