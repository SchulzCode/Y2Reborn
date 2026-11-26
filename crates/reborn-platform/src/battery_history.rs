//! The battery history on disk (`/data/reborn/state/battery-history.jsonl`): one JSON line per
//! record, appended in batches. Only the caller decides when a batch is written and whether it
//! is synced: the history may lose its last minutes in a crash, never the player's sleep.
//! A torn last line (power lost mid-write) is ignored and the next append starts a new line.
use reborn_core::battery_history::Record;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// The file name under `state/`.
pub const FILE: &str = "battery-history.jsonl";
/// At most this many records are kept (the oldest go first); a month is far below it.
pub const RECORDS_MAX: usize = 20_000;

#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Every complete record, oldest first.
    pub fn load(&self) -> Vec<Record> {
        parse(&fs::read(&self.path).unwrap_or_default())
    }

    /// Add `records` with one write. `durable` also syncs them to flash.
    pub fn append(&self, records: &[Record], durable: bool) -> io::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut text = String::new();
        // A torn last line would glue onto the first new one.
        if fs::read(&self.path).is_ok_and(|b| b.last().is_some_and(|&c| c != b'\n')) {
            text.push('\n');
        }
        text.push_str(&lines(records)?);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)?;
        file.write_all(text.as_bytes())?;
        if durable {
            file.sync_data()?;
        }
        Ok(())
    }

    /// Replace the file by `records` (a prune), atomically.
    pub fn rewrite(&self, records: &[Record]) -> io::Result<()> {
        reborn_core::atomic_write(&self.path, lines(records)?.as_bytes())
    }
}

fn lines(records: &[Record]) -> io::Result<String> {
    let mut text = String::new();
    for record in records {
        text.push_str(&serde_json::to_string(record).map_err(io::Error::other)?);
        text.push('\n');
    }
    Ok(text)
}

fn parse(bytes: &[u8]) -> Vec<Record> {
    bytes
        .split(|&b| b == b'\n')
        .filter_map(|line| serde_json::from_slice::<Record>(line).ok())
        .collect()
}

/// The kernel's id of this boot: an app restart is not a boot.
pub fn boot_id() -> String {
    fs::read_to_string(Path::new("/proc/sys/kernel/random/boot_id"))
        .map(|id| id.trim().replace('-', "").chars().take(12).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reborn_core::battery_history::Activity;

    fn dir(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("reborn-bathist-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn level(t: i64, pct: u8) -> Record {
        Record::Level {
            t,
            pct,
            activity: Activity::Awake,
            bt: false,
            est: false,
        }
    }

    #[test]
    fn appends_in_batches_and_reads_back() {
        let store = Store::new(dir("batch").join("state").join(FILE));
        assert!(store.load().is_empty());
        store.append(&[level(1, 90), level(2, 89)], false).unwrap();
        store.append(&[], true).unwrap();
        store.append(&[Record::Down { t: 3 }], true).unwrap();
        assert_eq!(
            store.load(),
            vec![level(1, 90), level(2, 89), Record::Down { t: 3 }]
        );
    }

    #[test]
    fn a_torn_last_line_is_ignored_and_not_glued() {
        let root = dir("torn");
        let path = root.join(FILE);
        let store = Store::new(&path);
        store.append(&[level(1, 90)], false).unwrap();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"k\":\"level\",\"t\":2,\"pc").unwrap();
        assert_eq!(store.load(), vec![level(1, 90)]);
        store.append(&[level(3, 88)], false).unwrap();
        assert_eq!(store.load(), vec![level(1, 90), level(3, 88)]);
    }

    #[test]
    fn rewrite_replaces_the_file_and_it_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = dir("rewrite");
        let path = root.join(FILE);
        let store = Store::new(&path);
        store.append(&[level(1, 90), level(2, 89)], false).unwrap();
        store.rewrite(&[level(2, 89)]).unwrap();
        assert_eq!(store.load(), vec![level(2, 89)]);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn boot_id_has_no_dashes_and_is_short() {
        let id = boot_id();
        assert!(id.len() <= 12 && !id.contains('-'));
    }
}
