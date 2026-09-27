//! Workload hints only. The platform's expiring QoS leases own CPU policy.
#![forbid(unsafe_code)]
use std::{
    fs::{File, OpenOptions},
    io::Write,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Idle,
    PlaybackNormal,
    PlaybackHeavy,
    Interactive,
    LibraryScan,
    NetworkTransfer,
    Maintenance,
}
impl Class {
    fn name(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::PlaybackNormal => "PlaybackNormal",
            Self::PlaybackHeavy => "PlaybackHeavy",
            Self::Interactive => "Interactive",
            Self::LibraryScan => "LibraryScan",
            Self::NetworkTransfer => "NetworkTransfer",
            Self::Maintenance => "Maintenance",
        }
    }
    fn lease_ms(self) -> u32 {
        if self == Self::Interactive {
            250
        } else {
            3000
        }
    }
}
#[derive(Default)]
pub struct Hints {
    file: Option<File>,
    last: Option<(Class, Instant)>,
    retry: Option<Instant>,
}
impl Hints {
    pub fn publish(&mut self, class: Class) {
        if self
            .last
            .is_some_and(|(old, when)| old == class && when.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        if self.file.is_none() {
            if self
                .retry
                .is_some_and(|when| when.elapsed() < Duration::from_secs(10))
            {
                return;
            }
            self.retry = Some(Instant::now());
            self.file = OpenOptions::new().write(true).open("/dev/y2-workload").ok();
        }
        if let Some(file) = &mut self.file {
            if writeln!(file, "{} {}", class.name(), class.lease_ms()).is_ok() {
                self.last = Some((class, Instant::now()));
            } else {
                self.file = None;
                self.last = None;
            }
        }
    }
    pub fn interactive(&mut self) {
        // Each input gets a short lease; no continuous fixed-MHz selection.
        self.last = None;
        self.publish(Class::Interactive);
    }
}
pub fn playback_class(playing: bool, heavy: bool, scanning: bool) -> Class {
    if scanning {
        Class::LibraryScan
    } else if playing && heavy {
        Class::PlaybackHeavy
    } else if playing {
        Class::PlaybackNormal
    } else {
        Class::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hints_are_classes_with_bounded_leases() {
        assert_eq!(Class::Interactive.lease_ms(), 250);
        assert_eq!(playback_class(true, false, false), Class::PlaybackNormal);
        assert_eq!(playback_class(true, true, false), Class::PlaybackHeavy);
        assert_eq!(playback_class(false, false, true), Class::LibraryScan);
        assert_eq!(playback_class(false, true, false), Class::Idle);
    }
}
