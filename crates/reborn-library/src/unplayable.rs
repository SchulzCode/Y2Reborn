//! Audio files the library sees but cannot play (Y2Reborn #104).
//!
//! A card can hold songs in formats Reborn has no decoder for (WMA, DSD, Musepack, …). They are
//! never added to the library, so a listener who copied them only sees "the new songs are
//! missing". The walk counts them by type: the counts reach the scan log, the error report's
//! snapshot and Settings → Library, which says what was left out instead of staying silent.
//!
//! A quick scan does not read an unchanged folder again, so each folder's counts are kept with its
//! record in `scan_folder_unplayable`. That table is additive: an older Reborn ignores it and
//! rewrites `scan_folders` alone, which is why a row counts only while its `mtime` matches the
//! folder record (a folder whose files changed has a new time). A folder without matching counts
//! is read again.
use super::{err, FolderWalk, ScanMetrics};
use rusqlite::{params, Transaction};
use std::{collections::BTreeMap, path::Path};

/// Files per type ("wma" → 40).
pub type Counts = BTreeMap<String, u64>;

/// Audio types without a decoder in Reborn's FFmpeg (`y2_production_defconfig`), as file name
/// endings.
const KINDS: &[&str] = &[
    "wma", "dsf", "dff", "mka", "mpc", "tta", "tak", "caf", "ac3", "dts", "mp2", "amr", "m4p",
    "aa", "aax", "ra", "mid", "midi",
];

/// The type of an audio file the library cannot play, None for anything else (pictures, text,
/// playable songs, macOS `._` companions).
pub fn kind(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.as_encoded_bytes();
    if name.starts_with(b"._") {
        return None;
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    KINDS.iter().copied().find(|kind| *kind == ext)
}

/// Add `from` to `into`.
pub fn add(into: &mut Counts, from: &Counts) {
    for (kind, files) in from {
        *into.entry(kind.clone()).or_default() += files;
    }
}

impl FolderWalk {
    /// A file the walk does not add: counted when it is audio Reborn cannot play.
    pub(super) fn note_unplayable(
        &mut self,
        folder: usize,
        path: &Path,
        is_file: bool,
        stats: &mut ScanMetrics,
    ) {
        let Some(kind) = kind(path).filter(|_| is_file) else {
            return;
        };
        *stats.unplayable.entry(kind.to_owned()).or_default() += 1;
        if let Some(counts) = &mut self.walked[folder].unplayable {
            *counts.entry(kind.to_owned()).or_default() += 1;
        }
    }
}

/// Forget the counts of `source` (with its folder records).
pub(super) fn forget(tx: &Transaction<'_>, source: &str) -> Result<(), String> {
    tx.execute(
        "DELETE FROM scan_folder_unplayable WHERE source_id=?1",
        [source],
    )
    .map(drop)
    .map_err(err)
}

/// Keep the counts of one walked folder (an empty map too: it says the folder was counted).
pub(super) fn record(
    tx: &Transaction<'_>,
    source: &str,
    path: &str,
    mtime: i64,
    counts: &Counts,
) -> Result<(), String> {
    let counts = serde_json::to_string(counts).map_err(|e| e.to_string())?;
    tx.prepare_cached("INSERT INTO scan_folder_unplayable VALUES(?1,?2,?3,?4)")
        .map_err(err)?
        .execute(params![source, path, mtime, counts])
        .map(drop)
        .map_err(err)
}

/// The counts stored with a folder record (the `counts` column of the join), None when missing.
pub(super) fn parse(counts: Option<String>) -> Option<Counts> {
    serde_json::from_str(&counts?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_without_a_decoder_is_named_and_everything_else_is_not() {
        assert_eq!(kind(Path::new("/m/a/01 Song.WMA")), Some("wma"));
        assert_eq!(kind(Path::new("/m/a/track.dsf")), Some("dsf"));
        assert_eq!(
            kind(Path::new("/m/a/video.mp4")),
            None,
            "a film is read, not left out"
        );
        for other in [
            "/m/a/01.flac",
            "/m/a/cover.jpg",
            "/m/a/._01.wma",
            "/m/a/wma",
            "/m/a/x.txt",
        ] {
            assert_eq!(kind(Path::new(other)), None, "{other}");
        }
        let mut total = Counts::from([("wma".to_owned(), 2)]);
        add(
            &mut total,
            &Counts::from([("wma".to_owned(), 1), ("dsf".to_owned(), 4)]),
        );
        assert_eq!(
            total,
            Counts::from([("dsf".to_owned(), 4), ("wma".to_owned(), 3)])
        );
    }
}
