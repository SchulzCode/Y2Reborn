//! Bounded, dependency-free simple LRC parsing and audio-position lookup.
use crate::{Source, Track};
use std::{path::PathBuf, sync::Arc};

pub const MAX_BYTES: usize = 256 * 1024;
pub const MAX_INPUT_LINES: usize = 8192;
pub const MAX_ENTRIES: usize = 4096;
pub const MAX_LINE_BYTES: usize = 4096;
pub const MAX_EXPANDED_TEXT: usize = 256 * 1024;
const MAX_TIMESTAMPS: usize = 32;

pub fn available(track: &Track, sources: &[Source]) -> bool {
    !track.is_radio()
        && track.online
        && (sources.is_empty() || sources.iter().any(|s| s.id == track.source_id && s.online))
}

/// A source revision, not a queue slot: shuffle/repeat retain a valid current-track cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub source: String,
    pub path: PathBuf,
    pub size: u64,
    pub modified: i64,
    pub mount: Option<u64>,
    pub root: Option<PathBuf>,
}
impl Key {
    pub fn new(track: &Track, sources: &[Source]) -> Self {
        let source = sources.iter().find(|s| s.id == track.source_id);
        Self {
            source: track.source_id.clone(),
            path: track.path.clone(),
            size: track.size,
            modified: track.mtime,
            mount: source.and_then(|s| s.mount_id),
            root: source.map(|s| s.root.clone()),
        }
    }
    pub fn matches(&self, track: &Track, sources: &[Source]) -> bool {
        let source = sources.iter().find(|s| s.id == track.source_id);
        self.source == track.source_id
            && self.path == track.path
            && self.size == track.size
            && self.modified == track.mtime
            && self.mount == source.and_then(|s| s.mount_id)
            && self.root.as_ref() == source.map(|s| &s.root)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub at_ms: u64,
    pub text: Arc<str>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lyrics {
    pub lines: Vec<Line>,
}
impl Lyrics {
    /// Last entry at or before the audible position; equal times keep their file ordering.
    pub fn active(&self, position_ms: u64) -> Option<usize> {
        self.lines
            .partition_point(|line| line.at_ms <= position_ms)
            .checked_sub(1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    TooLarge,
    TooManyLines,
    TooManyEntries,
    LineTooLong,
    TextLimit,
    InvalidUtf8,
}
impl Error {
    pub fn message(self) -> &'static str {
        match self {
            Self::TooLarge => "Lyrics exceed the 256 KiB file limit",
            Self::TooManyLines => "Lyrics exceed the 8192 input-line limit",
            Self::TooManyEntries => "Lyrics exceed the timed-entry limit",
            Self::LineTooLong => "An LRC line exceeds 4096 bytes",
            Self::TextLimit => "Repeated lyric text exceeds 256 KiB",
            Self::InvalidUtf8 => "Lyrics must be UTF-8 text",
        }
    }
}

/// Simple LRC, including a final global offset. Positive offsets advance the lyrics.
pub fn parse(bytes: &[u8]) -> Result<Lyrics, Error> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Error::InvalidUtf8)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (mut lines, mut offset, mut expanded) = (Vec::new(), 0i64, 0usize);
    for (index, raw) in text.lines().enumerate() {
        if index >= MAX_INPUT_LINES {
            return Err(Error::TooManyLines);
        }
        if raw.len() > MAX_LINE_BYTES {
            return Err(Error::LineTooLong);
        }
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some((tag, after)) = rest.strip_prefix('[').and_then(|s| s.split_once(']')) {
            if let Some(time) = timestamp(tag) {
                if times.len() >= MAX_TIMESTAMPS {
                    return Err(Error::TooManyEntries);
                }
                times.push(time);
            } else if let Some(value) = tag.strip_prefix("offset:") {
                if let Ok(value) = value.trim().parse::<i64>() {
                    offset = value;
                }
            } else if !times.is_empty() {
                break;
            }
            rest = after.trim_start();
        }
        if times.is_empty() {
            continue;
        }
        if lines.len() + times.len() > MAX_ENTRIES {
            return Err(Error::TooManyEntries);
        }
        expanded = expanded.saturating_add(rest.len().saturating_mul(times.len()));
        if expanded > MAX_EXPANDED_TEXT {
            return Err(Error::TextLimit);
        }
        let text: Arc<str> = rest.trim_end().into();
        lines.extend(times.into_iter().map(|at_ms| Line {
            at_ms,
            text: text.clone(),
        }));
    }
    for line in &mut lines {
        line.at_ms = if offset >= 0 {
            line.at_ms.saturating_sub(offset as u64)
        } else {
            line.at_ms.saturating_add(offset.unsigned_abs())
        };
    }
    lines.sort_by_key(|line| line.at_ms);
    Ok(Lyrics { lines })
}

fn timestamp(tag: &str) -> Option<u64> {
    let (minutes, seconds) = tag.split_once(':')?;
    if minutes.is_empty() || !minutes.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    if seconds.len() != 2 || !seconds.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if fraction.len() > 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if tag.ends_with('.') {
        return None;
    }
    let seconds = seconds.parse::<u64>().ok().filter(|&s| s < 60)?;
    let millis = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u64>().ok()? * 10u64.pow(3 - fraction.len() as u32)
    };
    minutes
        .parse::<u64>()
        .ok()?
        .checked_mul(60_000)?
        .checked_add(seconds * 1000)?
        .checked_add(millis)
}

#[cfg(test)]
#[path = "lyrics_tests.rs"]
mod tests;
