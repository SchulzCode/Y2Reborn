#![forbid(unsafe_code)]
pub mod benchmark;
#[cfg(test)]
mod platform_tests;
use reborn_core::{MediaSource, Source, Track};
use reborn_media::{Cancel, MediaFile};
use reborn_observability::{HealthState, Level, Observer};
use rusqlite::{params, Connection, ErrorCode, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    fs,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};
const SQLITE_CANTOPEN_PERM_EXTENDED: i32 = rusqlite::ffi::SQLITE_CANTOPEN | (3 << 8);
pub const SCHEMA_VERSION: i64 = 1;
/// 3: genre is read and stored; 4: Vorbis/APE "ALBUM ARTIST" (with a space) is
/// read as the album artist. Every older row re-reads its tags once.
const METADATA_REVISION: i64 = 4;
pub const MAX_LIBRARY_TRACKS: usize = 250_000;
pub const CATALOG_PAGE_SIZE: usize = 512;
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScanMetrics {
    /// Playlist files met on the way, per storage walked whole (its root):
    /// Playlists need not walk a 120 000-song card again (Beta 8.39).
    #[serde(skip)]
    pub playlists: Vec<(PathBuf, Vec<PathBuf>)>,
    pub discovered: u64,
    pub reused: u64,
    pub rescanned: u64,
    #[serde(default)]
    pub removed: u64,
    pub failures: u64,
    /// Supported files FFmpeg could not open. Their rows (if any) are kept and
    /// they do not make a scan incomplete: one broken file must not stop
    /// pruning of the files that were really deleted.
    #[serde(default)]
    pub unreadable: u64,
    pub elapsed_ms: u64,
    pub tracks_per_sec: f64,
    pub complete: bool,
    #[serde(default)]
    pub db_us: u64,
    #[serde(default)]
    pub metadata_us: u64,
    #[serde(default)]
    pub ffmpeg_open_us: u64,
    #[serde(default)]
    pub ffmpeg_probe_us: u64,
    #[serde(default)]
    pub ffmpeg_info_runs: u64,
    #[serde(default)]
    pub ffmpeg_io_bytes: u64,
    /// The walk trusted the folders of the last complete walk (`Folder`).
    #[serde(default)]
    pub quick: bool,
    /// Folders not read again: their modification time had not changed.
    #[serde(default)]
    pub folders_skipped: u64,
}
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub artist: Option<String>,
    pub album: Option<String>,
    pub folder: Option<String>,
    pub offset: usize,
    pub limit: usize,
}
enum DbCommand {
    Sources(Vec<Source>),
    List(Filter, SyncSender<Result<Vec<Track>, String>>),
    Catalog(SyncSender<Result<Vec<Track>, String>>),
    Begin(String, SyncSender<Result<i64, String>>),
    Probe(
        String,
        i64,
        Vec<FileStamp>,
        bool,
        SyncSender<Result<Vec<bool>, String>>,
    ),
    Batch(Vec<Track>, i64, SyncSender<Result<(), String>>),
    Retain(String, i64, Vec<PathBuf>, SyncSender<Result<(), String>>),
    Finish(Source, i64, bool, SyncSender<Result<ScanFinish, String>>),
    Folders(String, u64, SyncSender<Result<Option<Vec<Folder>>, String>>),
    SeenFolders(String, i64, Vec<String>, SyncSender<Result<(), String>>),
    SaveFolders(String, Option<(u64, Vec<Folder>)>),
    Test(SyncSender<Result<serde_json::Value, String>>),
    #[cfg(test)]
    TestSql(String, SyncSender<Result<(), String>>),
    Stop,
    PrepareSleep(SyncSender<Result<(), String>>),
    Shutdown(SyncSender<Result<(), String>>),
}
#[derive(Clone)]
pub struct Database {
    tx: SyncSender<DbCommand>,
}
fn err(e: rusqlite::Error) -> String {
    format!("SQLite: {e}")
}
#[derive(Debug, PartialEq, Eq)]
enum OpenFailure {
    Corrupt(String),
    FutureSchema(i64),
    Busy(String),
    ReadOnly(String),
    DiskFull(String),
    Io(String),
    Permission(String),
    WalShm(String),
    Migration(String),
    Setup(String),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenStage {
    Open,
    Setup,
    WalShm,
    Migration,
    Integrity,
}
fn classify_open_error(error: rusqlite::Error, stage: OpenStage) -> OpenFailure {
    let code = error.sqlite_error_code();
    let extended_code = match &error {
        rusqlite::Error::SqliteFailure(code, _) => Some(code.extended_code),
        _ => None,
    };
    let message = err(error);
    match code {
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => OpenFailure::Corrupt(message),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => OpenFailure::Busy(message),
        Some(ErrorCode::ReadOnly) => OpenFailure::ReadOnly(message),
        Some(ErrorCode::DiskFull) => OpenFailure::DiskFull(message),
        Some(ErrorCode::PermissionDenied) => OpenFailure::Permission(message),
        Some(ErrorCode::CannotOpen) if extended_code == Some(SQLITE_CANTOPEN_PERM_EXTENDED) => {
            OpenFailure::Permission(message)
        }
        Some(ErrorCode::SystemIoFailure) if stage == OpenStage::WalShm => {
            OpenFailure::WalShm(message)
        }
        Some(ErrorCode::SystemIoFailure) => OpenFailure::Io(message),
        Some(ErrorCode::CannotOpen) if stage == OpenStage::WalShm => OpenFailure::WalShm(message),
        _ if stage == OpenStage::Migration => OpenFailure::Migration(message),
        _ => OpenFailure::Setup(message),
    }
}
fn open_message(error: OpenFailure) -> String {
    match error {
        OpenFailure::Corrupt(message)
        | OpenFailure::Busy(message)
        | OpenFailure::ReadOnly(message)
        | OpenFailure::DiskFull(message)
        | OpenFailure::Io(message)
        | OpenFailure::Permission(message)
        | OpenFailure::WalShm(message)
        | OpenFailure::Migration(message)
        | OpenFailure::Setup(message) => message,
        OpenFailure::FutureSchema(version) => {
            format!("database schema {version} is newer than Reborn")
        }
    }
}
fn quick_check(connection: &Connection) -> Result<(), OpenFailure> {
    let result: String = connection
        .query_row("PRAGMA quick_check(10)", [], |row| row.get(0))
        .map_err(|error| classify_open_error(error, OpenStage::Integrity))?;
    if result == "ok" {
        Ok(())
    } else {
        Err(OpenFailure::Corrupt(result))
    }
}
fn open(path: &Path) -> Result<Connection, OpenFailure> {
    let c = Connection::open(path).map_err(|error| classify_open_error(error, OpenStage::Open))?;
    c.busy_timeout(Duration::from_secs(2))
        .map_err(|error| classify_open_error(error, OpenStage::Setup))?;
    let version: i64 = c
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|error| classify_open_error(error, OpenStage::Setup))?;
    if version > SCHEMA_VERSION {
        return Err(OpenFailure::FutureSchema(version));
    }
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA cache_size=-2048; PRAGMA temp_store=MEMORY;")
        .map_err(|error| classify_open_error(error, OpenStage::WalShm))?;
    if version == 0 {
        c.execute_batch("BEGIN IMMEDIATE;
 CREATE TABLE sources(id TEXT PRIMARY KEY, root TEXT NOT NULL, online INTEGER NOT NULL);
 CREATE TABLE tracks(id INTEGER PRIMARY KEY, source_id TEXT NOT NULL REFERENCES sources(id),path TEXT NOT NULL,filename TEXT NOT NULL,size INTEGER NOT NULL,mtime INTEGER NOT NULL,title TEXT NOT NULL,artist TEXT NOT NULL,album TEXT NOT NULL,album_artist TEXT NOT NULL,track INTEGER NOT NULL,disc INTEGER NOT NULL,duration_ms INTEGER NOT NULL,codec TEXT NOT NULL,sample_rate INTEGER NOT NULL,channels INTEGER NOT NULL,bitrate INTEGER NOT NULL,artwork INTEGER NOT NULL,seen INTEGER NOT NULL,deleted INTEGER NOT NULL DEFAULT 0,UNIQUE(source_id,path));
 CREATE INDEX tracks_album ON tracks(album_artist,album,disc,track);CREATE INDEX tracks_artist ON tracks(artist);
 PRAGMA user_version=1;COMMIT;")
            .map_err(|error| classify_open_error(error, OpenStage::Migration))?;
    }
    // An additive cache stamp keeps schema-1 consumers/row IDs readable.
    // Each successful old row refreshes once; failed files do not invalidate
    // already upgraded rows elsewhere in the library.
    let has_revision = c
        .prepare("PRAGMA table_info(tracks)")
        .map_err(|e| classify_open_error(e, OpenStage::Migration))?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| classify_open_error(e, OpenStage::Migration))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| classify_open_error(e, OpenStage::Migration))?
        .iter()
        .any(|name| name == "metadata_revision");
    if !has_revision {
        c.execute_batch(
            "ALTER TABLE tracks ADD COLUMN metadata_revision INTEGER NOT NULL DEFAULT 0;",
        )
        .map_err(|e| classify_open_error(e, OpenStage::Migration))?;
    }
    // Additive like metadata_revision: older rows keep '' until their tags are
    // re-read (METADATA_REVISION 3), schema-1 readers ignore the column.
    let has_genre = c
        .prepare("PRAGMA table_info(tracks)")
        .map_err(|e| classify_open_error(e, OpenStage::Migration))?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| classify_open_error(e, OpenStage::Migration))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| classify_open_error(e, OpenStage::Migration))?
        .iter()
        .any(|name| name == "genre");
    if !has_genre {
        c.execute_batch("ALTER TABLE tracks ADD COLUMN genre TEXT NOT NULL DEFAULT '';")
            .map_err(|e| classify_open_error(e, OpenStage::Migration))?;
    }
    quick_check(&c)?;
    // Additive indexes preserve the v1 file format and rollback compatibility.
    // Y2 20k-track measurements: ordered pages ~95 -> 5 ms, artist ~42 -> 5 ms,
    // album ~38 -> 1.4 ms. The small album index avoids another ~1.5 MiB and
    // extra title-index maintenance compared with a second covering index.
    let browse_indexes: i64 = c
        .query_row("SELECT count(*) FROM sqlite_schema WHERE type='index' AND name IN ('tracks_browse', 'tracks_browse_album')", [], |row| row.get(0))
        .map_err(|error| classify_open_error(error, OpenStage::Migration))?;
    if browse_indexes != 2 {
        c.execute_batch("BEGIN IMMEDIATE;
 CREATE INDEX IF NOT EXISTS tracks_browse ON tracks(artist,album,disc,track,title,path) WHERE deleted=0;
 CREATE INDEX IF NOT EXISTS tracks_browse_album ON tracks(album) WHERE deleted=0;
 COMMIT;")
            .map_err(|error| classify_open_error(error, OpenStage::Migration))?;
    }
    // Additive (quick scan, Beta 8.42): older Reborn ignores both tables.
    c.execute_batch("CREATE TABLE IF NOT EXISTS scan_folders(source_id TEXT NOT NULL,path TEXT NOT NULL,parent TEXT NOT NULL,mtime INTEGER NOT NULL,files INTEGER NOT NULL,unreadable INTEGER NOT NULL,playlists TEXT NOT NULL,PRIMARY KEY(source_id,path)) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS scan_volumes(source_id TEXT PRIMARY KEY,free_bytes INTEGER NOT NULL,revision INTEGER NOT NULL);")
        .map_err(|error| classify_open_error(error, OpenStage::Migration))?;
    c.execute_batch("CREATE TEMP TABLE scan_seen(id INTEGER PRIMARY KEY);
        CREATE TEMP TABLE scan_candidates(position INTEGER PRIMARY KEY,path TEXT NOT NULL,size INTEGER NOT NULL,mtime INTEGER NOT NULL);")
        .map_err(|error| classify_open_error(error, OpenStage::Setup))?;
    Ok(c)
}
fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Track> {
    let mut track = Track {
        id: r.get(0)?,
        source_id: r.get(1)?,
        path: PathBuf::from(r.get::<_, String>(2)?),
        filename: r.get(3)?,
        size: r.get(4)?,
        mtime: r.get(5)?,
        title: r.get(6)?,
        artist: r.get(7)?,
        album: r.get(8)?,
        album_artist: r.get(9)?,
        track: r.get(10)?,
        disc: r.get(11)?,
        duration_ms: r.get(12)?,
        codec: r.get(13)?,
        sample_rate: r.get(14)?,
        channels: r.get(15)?,
        bitrate: r.get(16)?,
        artwork: r.get(17)?,
        online: r.get(18)?,
        genre: r.get(19)?,
        book: false,
        own_album_artist: None,
    };
    track.book = reborn_core::books::is_book(&track);
    Ok(track)
}
const SELECT:&str="SELECT t.id,t.source_id,t.path,t.filename,t.size,t.mtime,t.title,t.artist,t.album,t.album_artist,t.track,t.disc,t.duration_ms,t.codec,t.sample_rate,t.channels,t.bitrate,t.artwork,s.online,t.genre FROM tracks t JOIN sources s ON s.id=t.source_id";
fn list(c: &Connection, f: &Filter) -> Result<Vec<Track>, String> {
    let mut sql = format!("{SELECT} WHERE t.deleted=0");
    // Keep values bound, but expose actual predicates to SQLite so it can seek
    // to an artist/album instead of scanning optional-parameter OR branches.
    if f.artist.is_some() {
        sql.push_str(" AND t.artist=?1");
    }
    if f.album.is_some() {
        sql.push_str(" AND t.album=?2");
    }
    if f.folder.is_some() {
        sql.push_str(" AND substr(t.path,1,length(?3))=?3");
    }
    sql.push_str(" ORDER BY t.artist,t.album,t.disc,t.track,t.title,t.path LIMIT ?4 OFFSET ?5");
    let mut q = c.prepare_cached(&sql).map_err(err)?;
    let rows = q
        .query_map(
            params![
                f.artist,
                f.album,
                f.folder,
                f.limit.clamp(1, CATALOG_PAGE_SIZE),
                f.offset
            ],
            from_row,
        )
        .map_err(err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)
}
#[derive(Clone)]
struct FileStamp {
    path: PathBuf,
    size: u64,
    mtime: i64,
    metadata: Option<fs::Metadata>,
}
// The scanner sends at most 64 fingerprints, never a scan-wide Track map.
fn probe(
    c: &mut Connection,
    source: &str,
    files: &[FileStamp],
    reuse: bool,
) -> Result<Vec<bool>, String> {
    if files.len() > 64 {
        return Err("scan probe exceeds batch limit".into());
    }
    if !reuse {
        return Ok(vec![false; files.len()]);
    }
    let tx = c.transaction().map_err(err)?;
    tx.execute("DELETE FROM scan_candidates", []).map_err(err)?;
    {
        let mut q = tx
            .prepare_cached("INSERT INTO scan_candidates VALUES(?1,?2,?3,?4)")
            .map_err(err)?;
        for (position, f) in files.iter().enumerate() {
            q.execute(params![position, f.path.to_string_lossy(), f.size, f.mtime])
                .map_err(err)?;
        }
    }
    // One lookup per candidate through UNIQUE(source_id,path). The matches
    // are then marked seen by id: an `INSERT … SELECT … WHERE t.id IS NOT
    // NULL` over the same join was planned as a walk over every track of the
    // source per batch (28 600 songs: 1.7 s per 64 files, 12.6 min for a
    // rescan that found no change).
    let ids = {
        let mut q = tx
            .prepare_cached(&format!(
                "SELECT t.id FROM scan_candidates c LEFT JOIN tracks t ON t.source_id=?1 AND t.path=c.path AND t.size=c.size AND t.mtime=c.mtime AND t.deleted=0 AND t.metadata_revision={METADATA_REVISION} ORDER BY c.position"
            ))
            .map_err(err)?;
        let rows = q
            .query_map([source], |r| r.get::<_, Option<i64>>(0))
            .map_err(err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
    };
    {
        let mut q = tx
            .prepare_cached("INSERT OR IGNORE INTO scan_seen VALUES(?1)")
            .map_err(err)?;
        for id in ids.iter().flatten() {
            q.execute([id]).map_err(err)?;
        }
    }
    tx.commit().map_err(err)?;
    Ok(ids.iter().map(Option::is_some).collect())
}
#[derive(Clone)]
struct ScanFinish {
    verified: bool,
    removed: u64,
}
/// A card fully scanned under its CID id: its rows under the bare `uuid:` id
/// from before card ids carried the CID (an earlier image) are a second,
/// never online copy of every song (Y2Reborn issue #15). Only that bare id,
/// offline at the same root, is retired; an id with another CID is another
/// card and stays.
fn retire_pre_cid_copy(c: &Connection, source: &Source) -> Result<u64, String> {
    let MediaSource::SdCard(uuid) = &source.kind else {
        return Ok(0);
    };
    let bare = format!("uuid:{uuid}");
    if source.id == bare {
        return Ok(0);
    }
    c.execute(
        "UPDATE tracks SET deleted=1 WHERE source_id=?1 AND deleted=0 AND EXISTS (SELECT 1 FROM sources WHERE id=?1 AND root=?2 AND online=0)",
        params![bare, source.root.to_string_lossy()],
    )
    .map(|n| n as u64)
    .map_err(err)
}
/// Rows of files that are present but could not be read stay in the library:
/// they count as seen, so a complete scan prunes only files that are gone.
fn retain(c: &mut Connection, source: &str, paths: &[PathBuf]) -> Result<(), String> {
    let tx = c.transaction().map_err(err)?;
    {
        let mut q = tx
            .prepare_cached("INSERT OR IGNORE INTO scan_seen SELECT id FROM tracks WHERE source_id=?1 AND path=?2")
            .map_err(err)?;
        for path in paths {
            q.execute(params![source, path.to_string_lossy()])
                .map_err(err)?;
        }
    }
    tx.commit().map_err(err)
}
fn finish(c: &mut Connection, source: &Source, complete: bool) -> Result<ScanFinish, String> {
    let prune = complete && source_identity_is_current(source);
    let tx = c.transaction().map_err(err)?;
    let removed = if prune {
        tx.execute("UPDATE tracks SET deleted=1 WHERE source_id=?1 AND deleted=0 AND NOT EXISTS (SELECT 1 FROM scan_seen WHERE id=tracks.id)", [&source.id]).map_err(err)? as u64
            + retire_pre_cid_copy(&tx, source)?
    } else {
        0
    };
    tx.execute("DELETE FROM scan_seen", []).map_err(err)?;
    tx.execute("DELETE FROM scan_candidates", []).map_err(err)?;
    tx.commit().map_err(err)?;
    Ok(ScanFinish {
        verified: prune,
        removed,
    })
}
/// A folder as the last complete walk of its storage found it. A quick scan
/// (`Scanner::scan`) does not read a folder again while its modification time
/// is unchanged: its songs, subfolders and playlists are taken from here.
#[derive(Clone, Debug, Default, PartialEq)]
struct Folder {
    path: String,
    parent: String,
    mtime: i64,
    /// Supported files directly inside, and how many of them were unreadable.
    files: u64,
    unreadable: u64,
    playlists: Vec<String>,
}
/// The folders of `source` if its storage still has the free space it had
/// after the walk that recorded them (and tags are read as then).
fn folders(c: &Connection, source: &str, free: u64) -> Result<Option<Vec<Folder>>, String> {
    let volume = c
        .query_row(
            "SELECT free_bytes,revision FROM scan_volumes WHERE source_id=?1",
            [source],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(err)?;
    if volume != Some((free as i64, METADATA_REVISION)) {
        return Ok(None);
    }
    let mut q = c
        .prepare_cached("SELECT path,parent,mtime,files,unreadable,playlists FROM scan_folders WHERE source_id=?1")
        .map_err(err)?;
    let rows = q
        .query_map([source], |r| {
            Ok(Folder {
                path: r.get(0)?,
                parent: r.get(1)?,
                mtime: r.get(2)?,
                files: r.get::<_, i64>(3)? as u64,
                unreadable: r.get::<_, i64>(4)? as u64,
                playlists: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
            })
        })
        .map_err(err)?;
    let found = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?;
    Ok((!found.is_empty()).then_some(found))
}
/// The rows of the files directly inside skipped folders count as seen.
fn seen_folders(c: &mut Connection, source: &str, folders: &[String]) -> Result<(), String> {
    let tx = c.transaction().map_err(err)?;
    {
        // A range on UNIQUE(source_id,path): '/' sorts just before '0'.
        let mut q = tx
            .prepare_cached("INSERT OR IGNORE INTO scan_seen SELECT id FROM tracks WHERE source_id=?1 AND path>?2 AND path<?3 AND instr(substr(path,length(?2)+1),'/')=0")
            .map_err(err)?;
        for folder in folders {
            q.execute(params![source, format!("{folder}/"), format!("{folder}0")])
                .map_err(err)?;
        }
    }
    tx.commit().map_err(err)
}
/// Replace the folders of `source` after a complete walk, or forget them.
fn save_folders(
    c: &mut Connection,
    source: &str,
    walked: Option<(u64, Vec<Folder>)>,
) -> Result<(), String> {
    let tx = c.transaction().map_err(err)?;
    tx.execute("DELETE FROM scan_folders WHERE source_id=?1", [source])
        .map_err(err)?;
    tx.execute("DELETE FROM scan_volumes WHERE source_id=?1", [source])
        .map_err(err)?;
    if let Some((free, folders)) = walked {
        {
            let mut q = tx
                .prepare_cached("INSERT INTO scan_folders VALUES(?1,?2,?3,?4,?5,?6,?7)")
                .map_err(err)?;
            for f in &folders {
                q.execute(params![
                    source,
                    f.path,
                    f.parent,
                    f.mtime,
                    f.files as i64,
                    f.unreadable as i64,
                    serde_json::to_string(&f.playlists).map_err(|e| e.to_string())?
                ])
                .map_err(err)?;
            }
        }
        tx.execute(
            "INSERT INTO scan_volumes VALUES(?1,?2,?3)",
            params![source, free as i64, METADATA_REVISION],
        )
        .map_err(err)?;
    }
    tx.commit().map_err(err)
}
fn update_sources(c: &mut Connection, sources: &[Source]) -> Result<(), String> {
    let tx = c.transaction().map_err(err)?;
    // A tiny source list is the only materialized identity set here.
    let offline = {
        let mut q = tx
            .prepare_cached("SELECT id FROM sources WHERE online<>0")
            .map_err(err)?;
        let rows = q.query_map([], |r| r.get::<_, String>(0)).map_err(err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
    };
    for id in offline
        .iter()
        .filter(|id| !sources.iter().any(|s| &s.id == *id && s.online))
    {
        tx.execute("UPDATE sources SET online=0 WHERE id=?1", [id])
            .map_err(err)?;
    }
    {
        let mut q = tx.prepare_cached("INSERT INTO sources(id,root,online) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET root=excluded.root,online=excluded.online WHERE sources.root IS NOT excluded.root OR sources.online IS NOT excluded.online").map_err(err)?;
        for s in sources {
            q.execute(params![s.id, s.root.to_string_lossy(), s.online])
                .map_err(err)?;
        }
    }
    tx.commit().map_err(err)
}
fn catalog(c: &mut Connection) -> Result<Vec<Track>, String> {
    let tx = c.transaction().map_err(err)?;
    let mut tracks: Vec<Track> = Vec::new();
    // The rowid tie-breaker handles identical paths/tags on different media.
    // Keyset pages avoid walking all preceding rows for every next page.
    loop {
        let cursor = tracks.last();
        let predicate = if cursor.is_some() {
            " AND (t.artist,t.album,t.disc,t.track,t.title,t.path,t.id)>(?1,?2,?3,?4,?5,?6,?7)"
        } else {
            ""
        };
        let sql = format!("{SELECT} WHERE t.deleted=0{predicate} ORDER BY t.artist,t.album,t.disc,t.track,t.title,t.path,t.id LIMIT {CATALOG_PAGE_SIZE}");
        let page = {
            let mut q = tx.prepare_cached(&sql).map_err(err)?;
            let rows = if let Some(t) = cursor {
                q.query_map(
                    params![
                        t.artist,
                        t.album,
                        t.disc,
                        t.track,
                        t.title,
                        t.path.to_string_lossy(),
                        t.id
                    ],
                    from_row,
                )
            } else {
                q.query_map([], from_row)
            }
            .map_err(err)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
        };
        let done = page.len() < CATALOG_PAGE_SIZE;
        if tracks.len() + page.len() > MAX_LIBRARY_TRACKS {
            return Err("library exceeds 250000-track limit".into());
        }
        tracks.extend(page);
        if done {
            break;
        }
    }
    tx.commit().map_err(err)?;
    Ok(tracks)
}
fn batch(c: &mut Connection, tracks: Vec<Track>, seen: i64) -> Result<(), String> {
    batch_tracked(c, tracks, seen, false)
}
fn batch_tracked(
    c: &mut Connection,
    tracks: Vec<Track>,
    seen: i64,
    mark_seen: bool,
) -> Result<(), String> {
    let tx = c.transaction().map_err(err)?;
    {
        let mut q=tx.prepare_cached("INSERT INTO tracks(source_id,path,filename,size,mtime,title,artist,album,album_artist,track,disc,duration_ms,codec,sample_rate,channels,bitrate,artwork,seen,metadata_revision,genre) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20) ON CONFLICT(source_id,path) DO UPDATE SET filename=excluded.filename,size=excluded.size,mtime=excluded.mtime,title=excluded.title,artist=excluded.artist,album=excluded.album,album_artist=excluded.album_artist,track=excluded.track,disc=excluded.disc,duration_ms=excluded.duration_ms,codec=excluded.codec,sample_rate=excluded.sample_rate,channels=excluded.channels,bitrate=excluded.bitrate,artwork=excluded.artwork,seen=excluded.seen,metadata_revision=excluded.metadata_revision,genre=excluded.genre,deleted=0 WHERE tracks.deleted<>0 OR tracks.metadata_revision IS NOT excluded.metadata_revision OR
 tracks.filename IS NOT excluded.filename OR tracks.size IS NOT excluded.size OR tracks.mtime IS NOT excluded.mtime OR
 tracks.title IS NOT excluded.title OR tracks.artist IS NOT excluded.artist OR tracks.album IS NOT excluded.album OR
 tracks.album_artist IS NOT excluded.album_artist OR tracks.track IS NOT excluded.track OR tracks.disc IS NOT excluded.disc OR
 tracks.duration_ms IS NOT excluded.duration_ms OR tracks.codec IS NOT excluded.codec OR tracks.sample_rate IS NOT excluded.sample_rate OR
 tracks.channels IS NOT excluded.channels OR tracks.bitrate IS NOT excluded.bitrate OR tracks.artwork IS NOT excluded.artwork OR tracks.genre IS NOT excluded.genre").map_err(err)?;
        let mut presence = tx.prepare_cached("INSERT OR IGNORE INTO scan_seen SELECT id FROM tracks WHERE source_id=?1 AND path=?2").map_err(err)?;
        for t in tracks {
            q.execute(params![
                t.source_id,
                t.path.to_string_lossy(),
                t.filename,
                t.size,
                t.mtime,
                t.title,
                t.artist,
                t.album,
                t.album_artist,
                t.track,
                t.disc,
                t.duration_ms,
                t.codec,
                t.sample_rate,
                t.channels,
                t.bitrate,
                t.artwork,
                seen,
                METADATA_REVISION,
                t.genre
            ])
            .map_err(err)?;
            if mark_seen {
                presence
                    .execute(params![t.source_id, t.path.to_string_lossy()])
                    .map_err(err)?;
            }
        }
    }
    tx.commit().map_err(err)
}
fn validate(c: &Connection) -> Result<serde_json::Value, String> {
    let result: String = c
        .query_row("PRAGMA quick_check(10)", [], |r| r.get(0))
        .map_err(err)?;
    if result != "ok" {
        return Err(result);
    }
    let count: i64 = c
        .query_row("SELECT count(*) FROM tracks WHERE deleted=0", [], |r| {
            r.get(0)
        })
        .map_err(err)?;
    c.execute_batch("SAVEPOINT reborn_diagnostic; CREATE TEMP TABLE reborn_write_test(value INTEGER); INSERT INTO reborn_write_test VALUES(42); ROLLBACK TO reborn_diagnostic; RELEASE reborn_diagnostic;").map_err(err)?;
    Ok(
        json!({"passed":true,"schema":SCHEMA_VERSION,"quick_check":result,"tracks":count,"write_test":"rolled_back"}),
    )
}
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}
fn restore_quarantine_moves(
    moved: &[(PathBuf, PathBuf)],
    rename: &mut impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Vec<String> {
    moved
        .iter()
        .rev()
        .filter_map(|(original, archived)| {
            rename(archived, original)
                .err()
                .map(|error| format!("{} -> {}: {error}", archived.display(), original.display()))
        })
        .collect()
}
fn quarantine_marker(path: &Path) -> PathBuf {
    path.join("INCOMPLETE")
}
fn recover_interrupted_quarantines(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("library.db");
    let prefix = format!("{name}.corrupt-");
    for entry in fs::read_dir(parent).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let candidate = entry.path();
        if !entry.file_name().to_string_lossy().starts_with(&prefix)
            || !candidate.is_dir()
            || !quarantine_marker(&candidate).exists()
        {
            continue;
        }
        for filename in [
            name.to_string(),
            format!("{name}-wal"),
            format!("{name}-shm"),
        ] {
            let archived = candidate.join(&filename);
            if !archived.exists() {
                continue;
            }
            let original = parent.join(&filename);
            if original.exists() {
                return Err(format!(
                    "interrupted database quarantine has conflicting copies of {}; preserved at {}",
                    original.display(),
                    candidate.display()
                ));
            }
            fs::rename(&archived, &original).map_err(|error| {
                format!(
                    "cannot recover interrupted database quarantine {}: {error}",
                    candidate.display()
                )
            })?;
        }
        fs::remove_file(quarantine_marker(&candidate)).map_err(|error| {
            format!(
                "cannot clear recovered database quarantine marker {}: {error}",
                candidate.display()
            )
        })?;
        fs::remove_dir(&candidate).map_err(|error| {
            format!(
                "cannot remove recovered database quarantine directory {}: {error}",
                candidate.display()
            )
        })?;
    }
    Ok(())
}
fn quarantine_database(path: &Path) -> Result<PathBuf, String> {
    quarantine_database_with(path, |source, destination| fs::rename(source, destination))
}
fn quarantine_database_with(
    path: &Path,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("library.db");
    let stamp = reborn_observability::wall_ms();
    let mut quarantine = None;
    for suffix in 0..100 {
        let candidate = parent.join(format!("{name}.corrupt-{stamp}-{suffix}"));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                quarantine = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!("cannot create quarantine directory: {error}"));
            }
        }
    }
    let quarantine = quarantine.ok_or("cannot allocate unique quarantine directory")?;
    let marker = quarantine_marker(&quarantine);
    if let Err(error) = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
        .and_then(|mut file| {
            use std::io::Write;
            file.write_all(b"database quarantine is incomplete")?;
            file.sync_all()
        })
    {
        let _ = fs::remove_dir_all(&quarantine);
        return Err(format!("cannot mark quarantine transaction: {error}"));
    }
    let artifacts = [
        path.to_path_buf(),
        sidecar(path, "-wal"),
        sidecar(path, "-shm"),
    ];
    let mut moved = Vec::<(PathBuf, PathBuf)>::new();
    for source in artifacts {
        match fs::symlink_metadata(&source) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                let reason = format!(
                    "cannot inspect database artifact {}: {error}",
                    source.display()
                );
                let rollback_errors = restore_quarantine_moves(&moved, &mut rename);
                if rollback_errors.is_empty() {
                    let _ = fs::remove_dir_all(&quarantine);
                    return Err(format!(
                        "{reason}; previously moved artifacts were restored"
                    ));
                }
                return Err(format!(
                    "{reason}; rollback incomplete, evidence remains at {}: {}",
                    quarantine.display(),
                    rollback_errors.join("; ")
                ));
            }
        }
        let destination = quarantine.join(
            source
                .file_name()
                .ok_or("database artifact has no filename")?,
        );
        if let Err(error) = rename(&source, &destination) {
            let rollback_errors = restore_quarantine_moves(&moved, &mut rename);
            if rollback_errors.is_empty() {
                let _ = fs::remove_dir_all(&quarantine);
                return Err(format!(
                    "cannot quarantine {}: {error}; previously moved artifacts were restored",
                    source.display()
                ));
            }
            return Err(format!(
                "cannot quarantine {}: {error}; rollback incomplete, evidence remains at {}: {}",
                source.display(),
                quarantine.display(),
                rollback_errors.join("; ")
            ));
        }
        moved.push((source, destination));
    }
    if let Err(error) = fs::remove_file(&marker) {
        let rollback_errors = restore_quarantine_moves(&moved, &mut rename);
        if rollback_errors.is_empty() {
            let _ = fs::remove_dir_all(&quarantine);
            return Err(format!(
                "cannot finalize database quarantine: {error}; original artifacts were restored"
            ));
        }
        return Err(format!(
            "cannot finalize database quarantine: {error}; rollback incomplete, recoverable evidence remains at {}: {}",
            quarantine.display(),
            rollback_errors.join("; ")
        ));
    }
    Ok(quarantine)
}
impl Database {
    pub fn spawn(path: PathBuf, log: Observer) -> Result<Self, String> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        recover_interrupted_quarantines(&path)?;
        let mut recovered = None;
        let mut c = match open(&path) {
            Ok(connection) => connection,
            Err(error @ OpenFailure::Corrupt(_)) if path.exists() => {
                let error_message = open_message(error);
                let quarantined = quarantine_database(&path).map_err(|quarantine_error| {
                    format!("{error_message}; database quarantine failed: {quarantine_error}")
                })?;
                recovered = Some((error_message, quarantined.clone()));
                open(&path).map_err(|rebuild| {
                    format!(
                        "corrupt database was preserved at {}; clean database creation failed: {}",
                        quarantined.display(),
                        open_message(rebuild)
                    )
                })?
            }
            Err(error) => return Err(open_message(error)),
        };
        if let Some((error, quarantined)) = recovered {
            log.emit(
                Level::Warn,
                "database",
                "corrupt_quarantined",
                "Database was quarantined and recreated; media will be rescanned",
                None,
                json!({"error":error,"quarantined":quarantined}),
            );
        }
        let (tx, rx) = sync_channel(32);
        thread::Builder::new()
            .name("database".into())
            .spawn(move || {
                let mut generation = 0_i64;
                let mut active_scan: Option<(String, i64)> = None;
                log.health_set("database", HealthState::Ok, true, "schema ready");
                loop {
                    log.heartbeat("database", 15);
                    let msg = match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(m) => m,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    };
                    let now = Instant::now();
                    let result: Result<(), String> = match msg {
                        DbCommand::Stop => break,
                        DbCommand::PrepareSleep(reply) => {
                            // FIFO delivery drains earlier commands. The caller
                            // already quiesced producers; a scan/transaction
                            // must explicitly finish before whole-system sleep.
                            let ready = if active_scan.is_none() && c.is_autocommit() {
                                Ok(())
                            } else {
                                Err("database_operation_active".into())
                            };
                            let _ = reply.try_send(ready);
                            Ok(())
                        }
                        DbCommand::Shutdown(reply) => {
                            let checkpoint = c
                                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                                    r.get::<_, i64>(0)
                                })
                                .map_err(err)
                                .and_then(|busy| {
                                    if busy == 0 {
                                        Ok(())
                                    } else {
                                        Err("checkpoint busy".into())
                                    }
                                });
                            let closed = c.close().map_err(|(_, e)| err(e));
                            let _ = reply.try_send(checkpoint.and(closed));
                            break;
                        }
                        DbCommand::Sources(sources) => update_sources(&mut c, &sources),
                        DbCommand::List(f, reply) => {
                            let _ = reply.try_send(list(&c, &f));
                            Ok(())
                        }
                        DbCommand::Catalog(reply) => {
                            // One album, one album artist in the lists (#45),
                            // worked out here, off the UI thread; the file
                            // keeps every song's own tags.
                            let mut tracks = catalog(&mut c);
                            if let Ok(tracks) = &mut tracks {
                                reborn_core::albums::unify_album_artists(tracks);
                            }
                            let _ = reply.try_send(tracks);
                            Ok(())
                        }
                        DbCommand::Begin(source, reply) => {
                            generation += 1;
                            active_scan = None;
                            let r = c
                                .execute_batch(
                                    "DELETE FROM scan_seen; DELETE FROM scan_candidates;",
                                )
                                .map_err(err);
                            if r.is_ok() {
                                active_scan = Some((source, generation));
                            }
                            let _ = reply.try_send(r.clone().map(|()| generation));
                            r
                        }
                        DbCommand::Probe(source, token, files, reuse, reply) => {
                            let r = if active_scan.as_ref() == Some(&(source.clone(), token)) {
                                probe(&mut c, &source, &files, reuse)
                            } else {
                                Err("scan superseded".into())
                            };
                            let _ = reply.try_send(r.clone());
                            r.map(|_| ())
                        }
                        DbCommand::Batch(v, token, reply) => {
                            let r = if active_scan.as_ref().is_some_and(|(source, active)| {
                                *active == token && v.iter().all(|t| &t.source_id == source)
                            }) {
                                batch_tracked(&mut c, v, token, true)
                            } else {
                                Err("scan superseded".into())
                            };
                            let _ = reply.try_send(r.clone());
                            r
                        }
                        DbCommand::Retain(source, token, paths, reply) => {
                            let r = if active_scan.as_ref() == Some(&(source.clone(), token)) {
                                retain(&mut c, &source, &paths)
                            } else {
                                Err("scan superseded".into())
                            };
                            let _ = reply.try_send(r.clone());
                            r
                        }
                        DbCommand::Finish(source, token, complete, reply) => {
                            let r = if active_scan.as_ref() == Some(&(source.id.clone(), token)) {
                                finish(&mut c, &source, complete)
                            } else {
                                Err("scan superseded".into())
                            };
                            active_scan = None;
                            let _ = reply.try_send(r.clone());
                            if let Ok(n) = c.query_row(
                                "SELECT count(*) FROM tracks WHERE deleted=0",
                                [],
                                |r| r.get::<_, i64>(0),
                            ) {
                                log.gauge("library_tracks", n as f64);
                            }
                            r.map(|_| ())
                        }
                        DbCommand::Folders(source, free, reply) => {
                            let r = folders(&c, &source, free);
                            let _ = reply.try_send(r.clone());
                            r.map(|_| ())
                        }
                        DbCommand::SeenFolders(source, token, paths, reply) => {
                            let r = if active_scan.as_ref() == Some(&(source.clone(), token)) {
                                seen_folders(&mut c, &source, &paths)
                            } else {
                                Err("scan superseded".into())
                            };
                            let _ = reply.try_send(r.clone());
                            r
                        }
                        DbCommand::SaveFolders(source, walked) => {
                            save_folders(&mut c, &source, walked)
                        }
                        DbCommand::Test(reply) => {
                            let _ = reply.try_send(validate(&c));
                            Ok(())
                        }
                        #[cfg(test)]
                        DbCommand::TestSql(sql, reply) => {
                            let r = c.execute_batch(&sql).map_err(err);
                            let _ = reply.try_send(r.clone());
                            r
                        }
                    };
                    log.gauge(
                        "database_query_latency_ms",
                        now.elapsed().as_secs_f64() * 1000.,
                    );
                    if let Err(e) = result {
                        log.health_set("database", HealthState::Failed, true, &e);
                        log.emit(
                            Level::Error,
                            "database",
                            "operation_failed",
                            &e,
                            None,
                            json!({"recovery_attempted":false}),
                        );
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { tx })
    }
    pub fn sources(&self, s: Vec<Source>) -> Result<(), String> {
        self.tx
            .try_send(DbCommand::Sources(s))
            .map_err(|e| e.to_string())
    }
    pub fn list(&self, f: Filter) -> Result<Receiver<Result<Vec<Track>, String>>, String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .try_send(DbCommand::List(f, tx))
            .map_err(|e| e.to_string())?;
        Ok(rx)
    }
    pub fn test(&self) -> Result<Receiver<Result<serde_json::Value, String>>, String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .try_send(DbCommand::Test(tx))
            .map_err(|e| e.to_string())?;
        Ok(rx)
    }
    #[cfg(test)]
    fn execute_test_sql(&self, sql: String) -> Result<(), String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .send(DbCommand::TestSql(sql, tx))
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|e| e.to_string())?
    }
    /// Product v2 owns one resident catalog; SQL and its row batches remain on
    /// the existing worker. Return a coherent snapshot, never a silent 20k slice.
    pub fn catalog(&self) -> Result<Receiver<Result<Vec<Track>, String>>, String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .try_send(DbCommand::Catalog(tx))
            .map_err(|e| e.to_string())?;
        Ok(rx)
    }
    fn begin_scan(&self, source: String) -> Result<i64, String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .send(DbCommand::Begin(source, tx))
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(15))
            .map_err(|e| e.to_string())?
    }
    fn probe(
        &self,
        source: String,
        token: i64,
        files: Vec<FileStamp>,
        reuse: bool,
    ) -> Result<Vec<bool>, String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .send(DbCommand::Probe(source, token, files, reuse, tx))
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(15))
            .map_err(|e| e.to_string())?
    }
    fn folders(&self, source: String, free: u64) -> Result<Option<Vec<Folder>>, String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .send(DbCommand::Folders(source, free, tx))
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(15))
            .map_err(|e| e.to_string())?
    }
    fn seen_folders(&self, source: String, token: i64, paths: Vec<String>) -> Result<(), String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .send(DbCommand::SeenFolders(source, token, paths, tx))
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(15))
            .map_err(|e| e.to_string())?
    }
    fn save_folders(
        &self,
        source: String,
        walked: Option<(u64, Vec<Folder>)>,
    ) -> Result<(), String> {
        self.tx
            .send(DbCommand::SaveFolders(source, walked))
            .map_err(|e| e.to_string())
    }
    pub fn stop(&self) {
        let _ = self.tx.try_send(DbCommand::Stop);
    }
    /// Drain earlier work after producers have stopped submitting mutations.
    /// The normal filesystem sync preserves SQLite's committed WAL; no extra
    /// checkpoint or recurring disk write is introduced merely to sleep.
    pub fn prepare_sleep(&self, timeout: Duration) -> Result<(), String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .try_send(DbCommand::PrepareSleep(tx))
            .map_err(|e| format!("database sleep queue: {e}"))?;
        rx.recv_timeout(timeout)
            .map_err(|e| format!("database sleep barrier: {e}"))?
    }
    pub fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .try_send(DbCommand::Shutdown(tx))
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(timeout)
            .map_err(|e| format!("database shutdown: {e}"))?
    }
}
/// Audio files the library reads. macOS writes an AppleDouble companion
/// `._name.mp3` next to every file it copies to a FAT card; those hold
/// Finder metadata, not audio.
pub fn supported(path: &Path) -> bool {
    if path
        .file_name()
        .is_some_and(|n| n.as_encoded_bytes().starts_with(b"._"))
    {
        return false;
    }
    path.extension().is_some_and(|e| {
        matches!(
            e.to_string_lossy().to_ascii_lowercase().as_str(),
            "flac"
                | "mp3"
                | "aac"
                | "m4a"
                | "m4b"
                | "ogg"
                | "oga"
                | "opus"
                | "wav"
                | "aiff"
                | "aif"
                | "aifc"
                | "ape"
                | "wv"
        )
    })
}
/// Folders macOS keeps on removable media (search index, change log, bin).
/// They never hold the owner's music and are often unreadable.
pub fn system_folder(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.as_encoded_bytes(),
        b".Spotlight-V100" | b".fseventsd" | b".Trashes" | b".TemporaryItems"
    )
}
pub fn unchanged(old: &Track, size: u64, mtime: i64) -> bool {
    old.size == size && old.mtime == mtime
}
fn mount_id_for_path(path: &Path, mountinfo: &str) -> Option<u64> {
    mount_for_path(path, mountinfo).map(|(id, _)| id)
}
/// The mount holding `path`: its id and filesystem type.
fn mount_for_path(path: &Path, mountinfo: &str) -> Option<(u64, String)> {
    mountinfo
        .lines()
        .filter_map(|line| {
            let (mount, rest) = line.split_once(" - ")?;
            let filesystem = rest.split_whitespace().next()?.to_owned();
            let fields = mount.split_whitespace().collect::<Vec<_>>();
            let id = fields.first()?.parse::<u64>().ok()?;
            let mount_path = fields
                .get(4)?
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\012", "\n")
                .replace("\\134", "\\");
            let mount_path = Path::new(&mount_path);
            path.starts_with(mount_path).then_some((
                mount_path.components().count(),
                id,
                filesystem,
            ))
        })
        .max_by_key(|(length, _, _)| *length)
        .map(|(_, id, filesystem)| (id, filesystem))
}
/// Free bytes of the storage at `root` when a quick scan may trust its folder
/// times, else None (every folder is read). Linux updates a folder's time when
/// an entry is added, removed or renamed in it; other systems writing to a
/// card may not, so the free space must be unchanged as well. exFAT counts it
/// from the card's own allocation bitmap at mount and ext4 keeps exact
/// counters, whoever wrote last. FAT32 keeps a free count (FSInfo) that other
/// systems often leave stale, so a FAT32 card is always walked whole.
fn quick_fingerprint(root: &Path) -> Option<u64> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").ok()?;
    let (_, filesystem) = mount_for_path(root, &mountinfo)?;
    if !matches!(filesystem.as_str(), "exfat" | "ext4") {
        return None;
    }
    reborn_platform::free_bytes(root).ok()
}
fn sd_id_matches_uuid(id: &str, uuid: &str) -> bool {
    let base = format!("uuid:{uuid}");
    id == base
        || id
            .strip_prefix(&format!("{base}:cid:"))
            .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()))
}
fn source_identity_matches_at(source: &Source, mountinfo: &str) -> bool {
    match &source.kind {
        MediaSource::Internal => true,
        MediaSource::SdCard(uuid) => {
            sd_id_matches_uuid(&source.id, uuid)
                && !uuid.starts_with("device:")
                && source.mount_id.is_some_and(|expected| {
                    mount_id_for_path(&source.root, mountinfo) == Some(expected)
                })
        }
    }
}
fn source_identity_is_current(source: &Source) -> bool {
    if matches!(&source.kind, MediaSource::Internal) {
        return true;
    }
    fs::read_to_string("/proc/self/mountinfo")
        .is_ok_and(|mountinfo| source_identity_matches_at(source, &mountinfo))
}
/// How a scan treats what is already indexed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanMode {
    /// Every file is read again (Rebuild Library).
    Rebuild,
    /// Every folder is read; unchanged files keep their rows (Scan for Music).
    Walk,
    /// Like Walk, but a folder whose modification time is unchanged since the
    /// last complete walk is not read again while the storage's free space is
    /// exactly as then (`quick_fingerprint`). Edits on a computer that keep
    /// a file within its clusters (most tag edits) show after Scan for Music.
    Quick,
}
pub struct Scanner {
    tx: SyncSender<(Vec<Source>, ScanMode)>,
    pub results: Receiver<Result<ScanMetrics, String>>,
    cancel: Arc<AtomicBool>,
    prepared: Arc<AtomicBool>,
    pending: Arc<AtomicUsize>,
}
impl Scanner {
    pub fn spawn(db: Database, log: Observer) -> Result<Self, String> {
        let (tx, rx) = sync_channel::<(Vec<Source>, ScanMode)>(1);
        let (rt, rr) = sync_channel(2);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let prepared = Arc::new(AtomicBool::new(false));
        let pending = Arc::new(AtomicUsize::new(0));
        let running = pending.clone();
        thread::Builder::new()
            .name("scanner".into())
            .spawn(move || {
                let mut unreadable = Unreadable::default();
                while !stop.load(Ordering::Relaxed) {
                    log.heartbeat("scanner", 30);
                    let (sources, mode) = match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(request) => request,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    };
                    let result = scan_sources(
                        &db,
                        &sources,
                        &log,
                        &stop,
                        source_identity_is_current,
                        mode,
                        quick_fingerprint,
                        &mut unreadable,
                    );
                    let _ = rt.try_send(result);
                    running.fetch_sub(1, Ordering::AcqRel);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            tx,
            results: rr,
            cancel,
            prepared,
            pending,
        })
    }
    /// Automatic incremental scan (start, storage change, after a USB copy):
    /// unchanged folders and files keep their indexed metadata (`ScanMode::Quick`).
    pub fn scan(&self, s: Vec<Source>) -> Result<(), String> {
        self.submit(s, ScanMode::Quick)
    }
    /// Scan for Music: every folder is read; unchanged files keep their metadata.
    pub fn scan_all(&self, s: Vec<Source>) -> Result<(), String> {
        self.submit(s, ScanMode::Walk)
    }
    /// Rebuild every supported file, preserving rows until replacement publication.
    pub fn rebuild(&self, s: Vec<Source>) -> Result<(), String> {
        self.submit(s, ScanMode::Rebuild)
    }
    fn submit(&self, sources: Vec<Source>, mode: ScanMode) -> Result<(), String> {
        if self.prepared.load(Ordering::Acquire) {
            return Err("sleep_operation_in_progress".into());
        }
        self.pending.fetch_add(1, Ordering::AcqRel);
        if self.prepared.load(Ordering::Acquire) {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            return Err("sleep_operation_in_progress".into());
        }
        if self.tx.try_send((sources, mode)).is_err() {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            return Err("scanner busy".into());
        }
        Ok(())
    }
    pub fn quiesce(&self) -> bool {
        self.prepared.store(true, Ordering::Release);
        self.pending.load(Ordering::Acquire) == 0
    }
    pub fn resume(&self) {
        self.prepared.store(false, Ordering::Release);
    }
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed)
    }
}
fn file_mtime(meta: &fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|t| t.as_nanos().min(i64::MAX as u128) as i64)
}
fn same_file(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}
/// Files (source, path, size, mtime) that FFmpeg could not open during this
/// process: an incremental scan does not open them again until they change.
/// A rebuild retries every file.
type UnreadableKey = (String, PathBuf, u64, i64);
type Unreadable = HashSet<UnreadableKey>;
enum Decoded {
    Track(Box<Track>),
    /// The file changed while it was read: try again on the next scan.
    Changed,
    Unreadable,
}
/// One file's tags, read off the scanner thread (`read_parallel`).
enum TagRead {
    Parsed {
        metadata: Box<reborn_media::Metadata>,
        elapsed_us: u64,
        /// The file changed while it was read.
        changed: bool,
    },
    Failed {
        error: String,
        elapsed_us: u64,
    },
    /// Not about this file (no cancel token): ends the batch, as before.
    Fatal(String),
    /// A bug while reading this file: it counts as unreadable.
    Panicked,
}

/// Open `path`, take its tags and close it; touches nothing shared.
fn read_tags(path: &Path, before: Option<&fs::Metadata>) -> TagRead {
    let cancel = match Cancel::new() {
        Ok(cancel) => cancel,
        Err(error) => return TagRead::Fatal(error),
    };
    let started = Instant::now();
    let parsed = MediaFile::open(path, cancel);
    let elapsed_us = started.elapsed().as_micros() as u64;
    match parsed {
        Ok(mut media) => TagRead::Parsed {
            metadata: Box::new(std::mem::take(&mut media.metadata)),
            elapsed_us,
            changed: !before.is_some_and(|before| {
                fs::metadata(path).is_ok_and(|after| same_file(before, &after))
            }),
        },
        Err(error) => TagRead::Failed { error, elapsed_us },
    }
}

/// Tag reads for a new library (28 600 songs: about 35 ms each, 12 minutes on
/// one core) spread over the cores the scan lease brought online, one left
/// for the UI and playback. Read after the lease: an idle player has parked
/// every core but CPU0.
fn scan_workers() -> usize {
    thread::available_parallelism()
        .map_or(1, |n| n.get())
        .saturating_sub(1)
        .clamp(1, 3)
}

/// An MP4 (a long M4B audiobook) at least this large builds a frame index of
/// tens of MB while its tags are read (24 bytes a frame, ≈140 MB at peak for
/// 10 h); such files are read one at a time, never three at once.
const HEAVY_MP4_BYTES: u64 = 64 * 1024 * 1024;
fn heavy_mp4(path: &Path, bytes: u64) -> bool {
    bytes >= HEAVY_MP4_BYTES
        && path.extension().is_some_and(|e| {
            ["m4a", "m4b", "mp4"]
                .iter()
                .any(|x| e.eq_ignore_ascii_case(x))
        })
}

/// `read_tags` for every pending file on up to `workers` threads, results in
/// input order. Workers run at a lower priority than playback and the UI.
fn read_parallel(
    pending: &[(FileStamp, UnreadableKey, PathBuf)],
    workers: usize,
    stop: &AtomicBool,
) -> Vec<TagRead> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<TagRead>>> = pending.iter().map(|_| Mutex::new(None)).collect();
    let heavy = Mutex::new(());
    let work = || loop {
        let i = next.fetch_add(1, Ordering::Relaxed);
        if i >= pending.len() || stop.load(Ordering::Relaxed) {
            break;
        }
        let (file, _, path) = &pending[i];
        let _alone =
            heavy_mp4(path, file.size).then(|| heavy.lock().unwrap_or_else(|e| e.into_inner()));
        let read = reborn_observability::isolated(|| read_tags(path, file.metadata.as_ref()))
            .unwrap_or(TagRead::Panicked);
        *slots[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(read);
    };
    match workers.min(pending.len()) {
        0 => {}
        1 => work(),
        n => thread::scope(|scope| {
            for _ in 0..n {
                scope.spawn(|| {
                    reborn_platform::lower_thread_priority();
                    work()
                });
            }
        }),
    }
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap_or_else(|e| e.into_inner())
                .unwrap_or(TagRead::Panicked)
        })
        .collect()
}

struct ScanBatch<'a> {
    db: &'a Database,
    source: &'a Source,
    token: i64,
    pinned_root: &'a Path,
    reuse: bool,
    stats: &'a mut ScanMetrics,
    complete: &'a mut bool,
    log: &'a Observer,
    id: u64,
    stop: &'a AtomicBool,
    unreadable: &'a mut Unreadable,
    /// Unreadable files per folder, for the folder records (`Folder`).
    unreadable_in: &'a mut HashMap<PathBuf, u64>,
    /// Threads reading tags (`scan_workers`).
    workers: usize,
}
impl ScanBatch<'_> {
    fn process(&mut self, files: Vec<FileStamp>) -> Result<(), String> {
        let started = Instant::now();
        let reused = self.db.probe(
            self.source.id.clone(),
            self.token,
            files.clone(),
            self.reuse,
        )?;
        self.stats.db_us += started.elapsed().as_micros() as u64;
        let mut changes = Vec::new();
        let mut retained = Vec::new();
        let mut pending = Vec::new();
        for (file, reuse) in files.into_iter().zip(reused) {
            if self.stop.load(Ordering::Relaxed) {
                return Err("scan cancelled".into());
            }
            if reuse {
                self.stats.reused += 1;
                continue;
            }
            let key = (
                self.source.id.clone(),
                file.path.clone(),
                file.size,
                file.mtime,
            );
            if self.reuse && self.unreadable.contains(&key) {
                self.stats.unreadable += 1;
                self.count_unreadable(&file.path);
                retained.push(file.path);
                continue;
            }
            let path = self.pinned_root.join(
                file.path
                    .strip_prefix(&self.source.root)
                    .map_err(|e| e.to_string())?,
            );
            pending.push((file, key, path));
        }
        // Tags are read on several cores; the results are applied in file order.
        let reads = read_parallel(&pending, self.workers, self.stop);
        if self.stop.load(Ordering::Relaxed) {
            return Err("scan cancelled".into());
        }
        for ((file, key, _), read) in pending.into_iter().zip(reads) {
            // A bug on one odd file makes it unreadable, not the player.
            let decoded = match read {
                TagRead::Panicked => Ok(Decoded::Unreadable),
                read => self.apply(&file, read),
            };
            match decoded? {
                Decoded::Track(track) => {
                    self.unreadable.remove(&key);
                    changes.push(*track);
                }
                Decoded::Changed => {}
                Decoded::Unreadable => {
                    self.stats.unreadable += 1;
                    self.count_unreadable(&file.path);
                    self.unreadable.insert(key);
                    retained.push(file.path);
                }
            }
        }
        if !retained.is_empty() {
            let (reply, result) = sync_channel(1);
            self.db
                .tx
                .send(DbCommand::Retain(
                    self.source.id.clone(),
                    self.token,
                    retained,
                    reply,
                ))
                .map_err(|e| e.to_string())?;
            result
                .recv_timeout(Duration::from_secs(15))
                .map_err(|e| e.to_string())??;
        }
        if !changes.is_empty() {
            // Decoder reads may outlive media removal. Recheck immediately
            // before publishing; Finish independently rechecks before pruning.
            if !source_identity_is_current(self.source) {
                *self.complete = false;
                self.stats.failures += 1;
                return Ok(());
            }
            let started = Instant::now();
            let (reply, result) = sync_channel(1);
            self.db
                .tx
                .send(DbCommand::Batch(changes, self.token, reply))
                .map_err(|e| e.to_string())?;
            result
                .recv_timeout(Duration::from_secs(15))
                .map_err(|e| e.to_string())??;
            self.stats.db_us += started.elapsed().as_micros() as u64;
        }
        Ok(())
    }
    fn count_unreadable(&mut self, path: &Path) {
        if let Some(folder) = path.parent() {
            *self.unreadable_in.entry(folder.to_path_buf()).or_default() += 1;
        }
    }
    /// Account for one file's `read_tags` result and turn it into a track.
    fn apply(&mut self, file: &FileStamp, read: TagRead) -> Result<Decoded, String> {
        let logical_path = &file.path;
        let size = file.size;
        let mtime = file.mtime;
        let (parsed, elapsed_us) = match read {
            TagRead::Parsed {
                metadata,
                elapsed_us,
                changed,
            } => (Ok((metadata, changed)), elapsed_us),
            TagRead::Failed { error, elapsed_us } => (Err(error), elapsed_us),
            TagRead::Fatal(error) => return Err(error),
            TagRead::Panicked => return Ok(Decoded::Unreadable),
        };
        self.stats.metadata_us += elapsed_us;
        let track = match parsed {
            Ok((m, changed)) => {
                self.stats.ffmpeg_open_us += m.open_us;
                self.stats.ffmpeg_probe_us += m.probe_us;
                self.stats.ffmpeg_info_runs += m.info_runs as u64;
                self.stats.ffmpeg_io_bytes += m.io_bytes;
                if changed {
                    *self.complete = false;
                    self.stats.failures += 1;
                    return Ok(Decoded::Changed);
                }
                self.stats.rescanned += 1;
                let filename = logical_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                Track {
                    id: 0,
                    source_id: self.source.id.clone(),
                    path: logical_path.to_path_buf(),
                    filename: filename.clone(),
                    size,
                    mtime,
                    title: if m.title.is_empty() {
                        filename
                    } else {
                        m.title
                    },
                    artist: m.artist,
                    album: m.album,
                    album_artist: m.album_artist,
                    track: m.track,
                    disc: m.disc,
                    duration_ms: m.duration_ms,
                    codec: m.codec,
                    sample_rate: m.sample_rate,
                    channels: m.channels,
                    bitrate: m.bitrate,
                    artwork: m.artwork,
                    online: true,
                    genre: m.genre.trim().to_owned(),
                    // Not stored: `from_row` derives it when the catalog loads.
                    book: false,
                    own_album_artist: None,
                }
            }
            Err(e) => {
                self.log.add("library_scan_errors", 1.);
                self.log.emit(
                    Level::Warn,
                    "scanner",
                    "file_failed",
                    &e,
                    Some(self.id),
                    json!({"path":logical_path,"recovery":"retain_source_rows"}),
                );
                return Ok(Decoded::Unreadable);
            }
        };
        Ok(Decoded::Track(Box::new(track)))
    }
}
#[cfg(test)]
fn scan_sources_with_identity(
    db: &Database,
    sources: &[Source],
    log: &Observer,
    stop: &AtomicBool,
    identity_is_current: impl Fn(&Source) -> bool,
) -> Result<ScanMetrics, String> {
    scan_sources_with_identity_and_reuse(
        db,
        sources,
        log,
        stop,
        identity_is_current,
        true,
        &mut Unreadable::default(),
    )
}
#[cfg(test)]
fn scan_sources_with_identity_and_reuse(
    db: &Database,
    sources: &[Source],
    log: &Observer,
    stop: &AtomicBool,
    identity_is_current: impl Fn(&Source) -> bool,
    reuse: bool,
    unreadable: &mut Unreadable,
) -> Result<ScanMetrics, String> {
    let mode = if reuse {
        ScanMode::Walk
    } else {
        ScanMode::Rebuild
    };
    scan_sources(
        db,
        sources,
        log,
        stop,
        identity_is_current,
        mode,
        |_| None,
        unreadable,
    )
}
/// A directory being walked: its entries, or (unchanged since the last
/// complete walk) its recorded subfolders.
enum Listing {
    Read(fs::ReadDir),
    Known(std::vec::IntoIter<String>),
}
struct Frame {
    listing: Listing,
    depth: usize,
    /// Its record in `FolderWalk::walked`.
    folder: usize,
}
/// The folder records of one storage: those the last complete walk left
/// (quick scan only) and those of this walk.
#[derive(Default)]
struct FolderWalk {
    known: HashMap<String, Folder>,
    children: HashMap<String, Vec<String>>,
    walked: Vec<Folder>,
    /// Skipped folders whose rows are not yet marked seen.
    skipped: Vec<String>,
}
impl FolderWalk {
    fn new(known: Vec<Folder>) -> Self {
        let mut walk = Self::default();
        for folder in known {
            walk.children
                .entry(folder.parent.clone())
                .or_default()
                .push(folder.path.clone());
            walk.known.insert(folder.path.clone(), folder);
        }
        walk
    }
    /// Start the folder `logical` (at `physical`): its time first, then its
    /// record when the time is unchanged, else its entries. None: unreadable.
    fn enter(
        &mut self,
        physical: &Path,
        logical: &Path,
        parent: &str,
        depth: usize,
        stats: &mut ScanMetrics,
        playlists: &mut Vec<PathBuf>,
    ) -> Option<Frame> {
        let mtime = file_mtime(&fs::metadata(physical).ok()?)?;
        let path = logical.to_string_lossy().into_owned();
        let folder = self.walked.len();
        if let Some(known) = self.known.get(&path).filter(|known| known.mtime == mtime) {
            stats.discovered += known.files;
            stats.reused += known.files.saturating_sub(known.unreadable);
            stats.unreadable += known.unreadable;
            stats.folders_skipped += 1;
            for playlist in &known.playlists {
                if playlists.len() < reborn_platform::playlists::MAX_PLAYLISTS {
                    playlists.push(PathBuf::from(playlist));
                }
            }
            let children = self.children.get(&path).cloned().unwrap_or_default();
            self.walked.push(Folder {
                parent: parent.to_owned(),
                ..known.clone()
            });
            self.skipped.push(path);
            return Some(Frame {
                listing: Listing::Known(children.into_iter()),
                depth,
                folder,
            });
        }
        let entries = fs::read_dir(physical).ok()?;
        self.walked.push(Folder {
            path,
            parent: parent.to_owned(),
            mtime,
            ..Default::default()
        });
        Some(Frame {
            listing: Listing::Read(entries),
            depth,
            folder,
        })
    }
}
enum Entry {
    /// A recorded subfolder of an unchanged folder (logical path).
    Known(String),
    Read(fs::DirEntry),
}
#[allow(clippy::too_many_arguments)]
fn scan_sources(
    db: &Database,
    sources: &[Source],
    log: &Observer,
    stop: &AtomicBool,
    identity_is_current: impl Fn(&Source) -> bool,
    mode: ScanMode,
    fingerprint: impl Fn(&Path) -> Option<u64>,
    unreadable: &mut Unreadable,
) -> Result<ScanMetrics, String> {
    let reuse = mode != ScanMode::Rebuild;
    let mut workload = reborn_platform::workload::CpuWorkloadLease::acquire_workload_hint(
        reborn_platform::workload::Class::LibraryScan,
    );
    let workers = scan_workers();
    let start = Instant::now();
    let id = log.correlation();
    let mut stats = ScanMetrics {
        complete: true,
        ..Default::default()
    };
    log.emit(
        Level::Info,
        "scanner",
        "scan_started",
        if reuse {
            "Incremental library scan"
        } else {
            "Full library rebuild"
        },
        Some(id),
        json!({"sources":sources.len(),"reuse":reuse,"mode":format!("{mode:?}")}),
    );
    for source in sources.iter().filter(|s| s.online) {
        if !identity_is_current(source) {
            stats.complete = false;
            stats.failures += 1;
            continue;
        }
        let root_handle = match fs::File::open(&source.root) {
            Ok(handle) => handle,
            Err(_) => {
                stats.complete = false;
                stats.failures += 1;
                continue;
            }
        };
        // Traverse through the open directory descriptor so a path reused by
        // a replacement mount cannot redirect in-flight reads to another FS.
        let pinned_root = PathBuf::from(format!("/proc/self/fd/{}", root_handle.as_raw_fd()));
        if !identity_is_current(source) {
            stats.complete = false;
            stats.failures += 1;
            continue;
        }
        // Taken before the walk: a change while it runs makes the next scan a walk.
        let free = fingerprint(&source.root);
        let mut walk = match (mode, free) {
            // Without a usable record the walk reads every folder.
            (ScanMode::Quick, Some(free)) => FolderWalk::new(
                db.folders(source.id.clone(), free)
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
            ),
            _ => FolderWalk::default(),
        };
        stats.quick |= !walk.known.is_empty();
        let seen = db.begin_scan(source.id.clone())?;
        let mut unreadable_in = HashMap::new();
        let mut batch_items = Vec::with_capacity(64);
        let mut complete = true;
        let mut playlists = vec![];
        let mut directories = match walk.enter(
            &pinned_root,
            &source.root,
            "",
            0,
            &mut stats,
            &mut playlists,
        ) {
            Some(frame) => vec![frame],
            None => {
                stats.complete = false;
                stats.failures += 1;
                continue;
            }
        };
        while let Some(frame) = directories.last_mut() {
            workload.renew();
            log.heartbeat("scanner", 30);
            if stop.load(Ordering::Relaxed) {
                return Err("scan cancelled".into());
            }
            let depth = frame.depth;
            let folder = frame.folder;
            let next = match &mut frame.listing {
                Listing::Known(children) => children.next().map(Entry::Known),
                Listing::Read(entries) => match entries.next() {
                    Some(Ok(entry)) => Some(Entry::Read(entry)),
                    Some(Err(_)) => {
                        complete = false;
                        stats.failures += 1;
                        continue;
                    }
                    None => None,
                },
            };
            let Some(next) = next else {
                directories.pop();
                continue;
            };
            if walk.skipped.len() >= 64 {
                db.seen_folders(source.id.clone(), seen, std::mem::take(&mut walk.skipped))?;
            }
            if stats.discovered > 250000 {
                return Err("scan file limit exceeded".into());
            }
            let (path, logical_dir) = match next {
                Entry::Known(child) => {
                    let logical = PathBuf::from(child);
                    let Ok(inside) = logical.strip_prefix(&source.root) else {
                        complete = false;
                        stats.failures += 1;
                        continue;
                    };
                    (pinned_root.join(inside), Some(logical))
                }
                Entry::Read(entry) => {
                    let file_type = match entry.file_type() {
                        Ok(kind) => kind,
                        Err(_) => {
                            complete = false;
                            stats.failures += 1;
                            continue;
                        }
                    };
                    if file_type.is_symlink() {
                        continue;
                    }
                    let path = entry.path();
                    if file_type.is_dir() {
                        if system_folder(&entry.file_name()) {
                            continue;
                        }
                        let logical = source
                            .root
                            .join(path.strip_prefix(&pinned_root).map_err(|e| e.to_string())?);
                        (path, Some(logical))
                    } else {
                        if file_type.is_file()
                            && reborn_platform::playlists::is_playlist(&path)
                            && playlists.len() < reborn_platform::playlists::MAX_PLAYLISTS
                        {
                            if let Ok(inside) = path.strip_prefix(&pinned_root) {
                                let logical = source.root.join(inside);
                                if reborn_platform::playlists::listed(&source.root, &logical) {
                                    walk.walked[folder]
                                        .playlists
                                        .push(logical.to_string_lossy().into_owned());
                                    playlists.push(logical);
                                }
                            }
                            continue;
                        }
                        if !file_type.is_file() || !supported(&path) {
                            continue;
                        }
                        let logical_path = source
                            .root
                            .join(path.strip_prefix(&pinned_root).map_err(|e| e.to_string())?);
                        let meta = match entry.metadata() {
                            Ok(meta) => meta,
                            Err(_) => {
                                complete = false;
                                stats.failures += 1;
                                continue;
                            }
                        };
                        stats.discovered += 1;
                        walk.walked[folder].files += 1;
                        let Some(mtime) = file_mtime(&meta) else {
                            complete = false;
                            stats.failures += 1;
                            continue;
                        };
                        batch_items.push(FileStamp {
                            path: logical_path,
                            size: meta.len(),
                            mtime,
                            metadata: Some(meta),
                        });
                        if batch_items.len() == 64 {
                            if !identity_is_current(source) {
                                complete = false;
                                stats.failures += 1;
                                break;
                            }
                            ScanBatch {
                                db,
                                source,
                                token: seen,
                                pinned_root: &pinned_root,
                                reuse,
                                stats: &mut stats,
                                complete: &mut complete,
                                log,
                                id,
                                stop,
                                unreadable: &mut *unreadable,
                                unreadable_in: &mut unreadable_in,
                                workers,
                            }
                            .process(std::mem::take(&mut batch_items))?;
                        }
                        continue;
                    }
                }
            };
            let Some(logical) = logical_dir else {
                continue;
            };
            if !identity_is_current(source) {
                complete = false;
                stats.failures += 1;
                break;
            }
            if depth >= 32 {
                complete = false;
                stats.failures += 1;
                continue;
            }
            let parent = walk.walked[folder].path.clone();
            match walk.enter(
                &path,
                &logical,
                &parent,
                depth + 1,
                &mut stats,
                &mut playlists,
            ) {
                Some(frame) => directories.push(frame),
                None => {
                    complete = false;
                    stats.failures += 1;
                }
            }
        }
        if !batch_items.is_empty() {
            if identity_is_current(source) {
                ScanBatch {
                    db,
                    source,
                    token: seen,
                    pinned_root: &pinned_root,
                    reuse,
                    stats: &mut stats,
                    complete: &mut complete,
                    log,
                    id,
                    stop,
                    unreadable: &mut *unreadable,
                    unreadable_in: &mut unreadable_in,
                    workers,
                }
                .process(batch_items)?;
            } else {
                complete = false;
                stats.failures += 1;
            }
        }
        if !walk.skipped.is_empty() {
            db.seen_folders(source.id.clone(), seen, std::mem::take(&mut walk.skipped))?;
        }
        if !identity_is_current(source) {
            complete = false;
            stats.failures += 1;
        }
        let (reply, result) = sync_channel(1);
        db.tx
            .send(DbCommand::Finish(source.clone(), seen, complete, reply))
            .map_err(|e| e.to_string())?;
        let finished = result
            .recv_timeout(Duration::from_secs(15))
            .map_err(|e| e.to_string())??;
        stats.removed += finished.removed;
        if complete && !finished.verified {
            complete = false;
            stats.failures += 1;
        }
        // Only a complete walk is a base for the next quick scan.
        let walked = free.filter(|_| complete).map(|free| {
            for record in &mut walk.walked {
                record.unreadable += unreadable_in
                    .get(Path::new(&record.path))
                    .copied()
                    .unwrap_or(0);
            }
            (free, std::mem::take(&mut walk.walked))
        });
        db.save_folders(source.id.clone(), walked)?;
        if complete {
            stats.playlists.push((source.root.clone(), playlists));
        }
        stats.complete &= complete;
    }
    // Each batch and Finish is acknowledged by the single SQLite worker.
    // Integrity diagnostics are explicit; unchanged scans do not repeat quick_check.
    stats.elapsed_ms = start.elapsed().as_millis() as u64;
    stats.tracks_per_sec = stats.discovered as f64 / start.elapsed().as_secs_f64().max(0.001);
    log.gauge("library_scan_files_per_sec", stats.tracks_per_sec);
    log.health_set(
        "scanner",
        if stats.complete {
            HealthState::Ok
        } else {
            HealthState::Degraded
        },
        true,
        if stats.complete {
            "scan complete"
        } else {
            "incomplete source traversal; entries retained"
        },
    );
    log.emit(
        Level::Info,
        "scanner",
        "scan_completed",
        "Incremental scan complete",
        Some(id),
        json!(stats),
    );
    Ok(stats)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn large_mp4_audiobooks_are_read_one_at_a_time() {
        let big = HEAVY_MP4_BYTES;
        assert!(heavy_mp4(Path::new("/m/Audiobooks/Dune.M4B"), big));
        assert!(heavy_mp4(Path::new("/m/Long.m4a"), big + 1));
        assert!(!heavy_mp4(Path::new("/m/Short.m4b"), big - 1));
        assert!(
            !heavy_mp4(Path::new("/m/Concert.flac"), big * 4),
            "only MP4 builds the index"
        );
    }
    #[test]
    fn schema_and_rollback() {
        let p = std::env::temp_dir().join(format!("reborn-db-{}.sqlite", std::process::id()));
        let _ = fs::remove_file(&p);
        let c = open(&p).unwrap();
        assert!(validate(&c).unwrap()["passed"].as_bool().unwrap());
        assert!(c.prepare("SELECT * FROM reborn_write_test").is_err());
        drop(c);
        let c = open(&p).unwrap();
        assert_eq!(
            c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(c);
        fs::remove_file(p).unwrap();
    }
    #[test]
    fn diff_size_and_mtime() {
        let t = Track {
            size: 5,
            mtime: 9,
            ..Default::default()
        };
        assert!(unchanged(&t, 5, 9));
        assert!(!unchanged(&t, 6, 9));
        assert!(!unchanged(&t, 5, 10));
    }
    #[test]
    fn future_schema_rejected() {
        let root = std::env::temp_dir().join(format!("reborn-future-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let p = root.join("library.db");
        let c = Connection::open(&p).unwrap();
        c.pragma_update(None, "user_version", 999).unwrap();
        drop(c);
        assert!(matches!(open(&p), Err(OpenFailure::FutureSchema(999))));
        let log = Observer::new(&root.join("logs")).unwrap();
        assert!(Database::spawn(p.clone(), log)
            .err()
            .expect("future schema must refuse startup")
            .contains("newer than Reborn"));
        assert_eq!(
            Connection::open(&p)
                .unwrap()
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            999
        );
        assert!(!root
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains("corrupt-")));
    }
    #[test]
    fn formats() {
        assert!(supported(Path::new("a.FLAC")));
        assert!(supported(Path::new("a.AIFF")));
        assert!(supported(Path::new("a.ape")));
        assert!(supported(Path::new("a.wv")));
        // 31: containers the image's FFmpeg already demuxes.
        assert!(supported(Path::new("book.m4b")));
        assert!(supported(Path::new("a.oga")));
        assert!(supported(Path::new("a.AIFC")));
        // Video containers stay out: a music video would fail every scan.
        assert!(!supported(Path::new("clip.mp4")));
        assert!(!supported(Path::new("a.txt")));
        assert!(!supported(Path::new("/media/sd/Album/._01 Song.mp3")));
        assert!(supported(Path::new("/media/sd/Album/_01 Song.mp3")));
        assert!(system_folder(std::ffi::OsStr::new(".Spotlight-V100")));
        assert!(system_folder(std::ffi::OsStr::new(".fseventsd")));
        assert!(system_folder(std::ffi::OsStr::new(".Trashes")));
        assert!(!system_folder(std::ffi::OsStr::new("Music")));
    }
    #[test]
    fn corrupt_database_is_quarantined_before_recreation() {
        let root = std::env::temp_dir().join(format!("reborn-recovery-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("library.db");
        fs::write(&path, b"not a sqlite database").unwrap();
        let log = Observer::new(&root.join("logs")).unwrap();
        let db = Database::spawn(path.clone(), log).unwrap();
        let reply = db
            .test()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(reply.is_ok());
        let quarantine = root
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("library.db.corrupt-")
            })
            .expect("quarantine directory");
        let quarantine = quarantine.path();
        assert_eq!(
            fs::read(quarantine.join("library.db")).unwrap(),
            b"not a sqlite database"
        );
        db.stop();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn quarantine_moves_database_and_sidecars_as_one_recoverable_set() {
        let root =
            std::env::temp_dir().join(format!("reborn-quarantine-set-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("library.db");
        fs::write(&path, b"db evidence").unwrap();
        fs::write(sidecar(&path, "-wal"), b"wal evidence").unwrap();
        fs::write(sidecar(&path, "-shm"), b"shm evidence").unwrap();

        let quarantine = quarantine_database(&path).unwrap();
        assert!(!path.exists());
        for (name, contents) in [
            ("library.db", b"db evidence".as_slice()),
            ("library.db-wal", b"wal evidence".as_slice()),
            ("library.db-shm", b"shm evidence".as_slice()),
        ] {
            assert_eq!(fs::read(quarantine.join(name)).unwrap(), contents);
        }
        assert!(!quarantine_marker(&quarantine).exists());
        let _ = fs::remove_dir_all(root);
    }

    fn sqlite_error(code: i32) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
    }

    #[test]
    fn sqlite_failures_keep_corruption_separate_from_operations() {
        assert!(matches!(
            classify_open_error(
                sqlite_error(rusqlite::ffi::SQLITE_CORRUPT),
                OpenStage::Setup
            ),
            OpenFailure::Corrupt(_)
        ));
        assert!(matches!(
            classify_open_error(sqlite_error(rusqlite::ffi::SQLITE_NOTADB), OpenStage::Open),
            OpenFailure::Corrupt(_)
        ));
        assert!(matches!(
            classify_open_error(sqlite_error(rusqlite::ffi::SQLITE_BUSY), OpenStage::Setup),
            OpenFailure::Busy(_)
        ));
        assert!(matches!(
            classify_open_error(sqlite_error(rusqlite::ffi::SQLITE_LOCKED), OpenStage::Setup),
            OpenFailure::Busy(_)
        ));
        assert!(matches!(
            classify_open_error(
                sqlite_error(rusqlite::ffi::SQLITE_READONLY),
                OpenStage::Setup
            ),
            OpenFailure::ReadOnly(_)
        ));
        assert!(matches!(
            classify_open_error(sqlite_error(rusqlite::ffi::SQLITE_FULL), OpenStage::Setup),
            OpenFailure::DiskFull(_)
        ));
        assert!(matches!(
            classify_open_error(sqlite_error(rusqlite::ffi::SQLITE_PERM), OpenStage::Setup),
            OpenFailure::Permission(_)
        ));
        let cantopen_permission = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::CannotOpen,
                extended_code: SQLITE_CANTOPEN_PERM_EXTENDED,
            },
            None,
        );
        assert!(matches!(
            classify_open_error(cantopen_permission, OpenStage::Open),
            OpenFailure::Permission(_)
        ));
        assert!(matches!(
            classify_open_error(sqlite_error(rusqlite::ffi::SQLITE_IOERR), OpenStage::Setup),
            OpenFailure::Io(_)
        ));
        assert!(matches!(
            classify_open_error(sqlite_error(rusqlite::ffi::SQLITE_IOERR), OpenStage::WalShm),
            OpenFailure::WalShm(_)
        ));
        assert!(matches!(
            classify_open_error(
                sqlite_error(rusqlite::ffi::SQLITE_CONSTRAINT),
                OpenStage::Migration
            ),
            OpenFailure::Migration(_)
        ));
    }

    #[test]
    fn locked_database_is_preserved_without_quarantine() {
        let root = std::env::temp_dir().join(format!("reborn-busy-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("library.db");
        let lock = Connection::open(&path).unwrap();
        lock.execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
            .unwrap();
        let log = Observer::new(&root.join("logs")).unwrap();

        let error = Database::spawn(path.clone(), log)
            .err()
            .expect("locked database must refuse startup");

        assert!(error.to_ascii_lowercase().contains("locked"));
        assert!(path.exists());
        assert!(!root
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains("corrupt-")));
        lock.execute_batch("ROLLBACK;").unwrap();
    }

    #[test]
    fn shutdown_ack_follows_checkpoint_and_connection_close() {
        let root = std::env::temp_dir().join(format!("reborn-shutdown-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("library.db");
        let log = Observer::new(&root.join("logs")).unwrap();
        let db = Database::spawn(path.clone(), log).unwrap();
        db.execute_test_sql("INSERT OR REPLACE INTO sources VALUES('one','/scratch',1)".into())
            .unwrap();
        db.shutdown(Duration::from_secs(3)).unwrap();
        if let Ok(reply) = db.test() {
            assert!(reply.recv_timeout(Duration::from_secs(1)).is_err());
        }
        let connection = Connection::open(path).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT root FROM sources WHERE id='one'", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "/scratch"
        );
        assert_eq!(
            connection
                .query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }

    #[test]
    fn read_only_disk_full_permission_and_io_codes_are_operational() {
        let root = std::env::temp_dir().join(format!("reborn-operational-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("readonly.db");
        drop(Connection::open(&path).unwrap());
        let readonly =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let read_only_error = readonly
            .execute("CREATE TABLE denied(value)", [])
            .unwrap_err();
        assert!(matches!(
            classify_open_error(read_only_error, OpenStage::Migration),
            OpenFailure::ReadOnly(_)
        ));

        let disk_full = root.join("full.db");
        let connection = Connection::open(&disk_full).unwrap();
        connection
            .execute_batch("PRAGMA page_size=512; CREATE TABLE payload(value BLOB);")
            .unwrap();
        let pages: u32 = connection
            .pragma_query_value(None, "page_count", |row| row.get(0))
            .unwrap();
        connection
            .pragma_update(None, "max_page_count", pages)
            .unwrap();
        let full_error = connection
            .execute("INSERT INTO payload VALUES(zeroblob(8192))", [])
            .unwrap_err();
        assert!(matches!(
            classify_open_error(full_error, OpenStage::Migration),
            OpenFailure::DiskFull(_)
        ));
        drop(connection);

        assert!(matches!(
            classify_open_error(sqlite_error(rusqlite::ffi::SQLITE_PERM), OpenStage::Open),
            OpenFailure::Permission(_)
        ));
        assert!(matches!(
            classify_open_error(sqlite_error(rusqlite::ffi::SQLITE_IOERR), OpenStage::Setup),
            OpenFailure::Io(_)
        ));
    }

    #[test]
    fn checksum_page_corruption_is_detected_and_quarantined() {
        let root = std::env::temp_dir().join(format!("reborn-checksum-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("library.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA page_size=512; PRAGMA journal_mode=DELETE; PRAGMA user_version=1; CREATE TABLE payload(value BLOB);",
            )
            .unwrap();
        connection
            .execute("INSERT INTO payload VALUES(zeroblob(12000))", [])
            .unwrap();
        let page_count: u64 = connection
            .pragma_query_value(None, "page_count", |row| row.get(0))
            .unwrap();
        let page_size: u64 = connection
            .pragma_query_value(None, "page_size", |row| row.get(0))
            .unwrap();
        drop(connection);
        assert!(page_count > 2);
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len((page_count - 1) * page_size)
            .unwrap();
        assert!(matches!(open(&path), Err(OpenFailure::Corrupt(_))));

        let log = Observer::new(&root.join("logs")).unwrap();
        let db = Database::spawn(path.clone(), log).unwrap();
        let test = db
            .test()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(test.is_ok());
        assert!(root
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("library.db.corrupt-")));
        db.stop();
    }

    #[test]
    fn wal_or_shm_rename_failure_restores_the_whole_artifact_set() {
        for fail_at in [2, 3] {
            let root = std::env::temp_dir().join(format!(
                "reborn-quarantine-fail-{fail_at}-{}",
                std::process::id()
            ));
            fs::create_dir_all(&root).unwrap();
            let path = root.join("library.db");
            fs::write(&path, b"db").unwrap();
            fs::write(sidecar(&path, "-wal"), b"wal").unwrap();
            fs::write(sidecar(&path, "-shm"), b"shm").unwrap();
            let call_count = Cell::new(0usize);
            let error = quarantine_database_with(&path, |from, to| {
                let call = call_count.get() + 1;
                call_count.set(call);
                if call == fail_at {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected rename failure",
                    ))
                } else {
                    fs::rename(from, to)
                }
            })
            .unwrap_err();

            assert!(error.contains("previously moved artifacts were restored"));
            assert_eq!(fs::read(&path).unwrap(), b"db");
            assert_eq!(fs::read(sidecar(&path, "-wal")).unwrap(), b"wal");
            assert_eq!(fs::read(sidecar(&path, "-shm")).unwrap(), b"shm");
            assert!(!root
                .read_dir()
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().contains("corrupt-")));
        }
    }

    #[test]
    fn interrupted_quarantine_restores_archived_artifacts_before_open() {
        let root =
            std::env::temp_dir().join(format!("reborn-quarantine-recovery-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("library.db");
        let quarantine = root.join("library.db.corrupt-test");
        fs::create_dir(&quarantine).unwrap();
        fs::write(quarantine_marker(&quarantine), b"incomplete").unwrap();
        fs::write(quarantine.join("library.db"), b"db").unwrap();
        fs::write(quarantine.join("library.db-wal"), b"wal").unwrap();
        fs::write(sidecar(&path, "-shm"), b"shm").unwrap();

        recover_interrupted_quarantines(&path).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"db");
        assert_eq!(fs::read(sidecar(&path, "-wal")).unwrap(), b"wal");
        assert_eq!(fs::read(sidecar(&path, "-shm")).unwrap(), b"shm");
        assert!(!quarantine.exists());
        let _ = fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod consistency_tests {
    use super::*;
    fn setup(name: &str) -> (PathBuf, Connection) {
        let p = std::env::temp_dir().join(format!("reborn-{name}-{}.db", std::process::id()));
        let _ = fs::remove_file(&p);
        let c = open(&p).unwrap();
        c.execute("INSERT INTO sources VALUES('sd','/media/sd',1)", [])
            .unwrap();
        (p, c)
    }
    #[test]
    fn a_card_scanned_under_its_cid_id_retires_its_pre_cid_copy() {
        let (path, mut c) = setup("pre-cid");
        let cid = format!("uuid:0CDF-1366:cid:{}", "b".repeat(64));
        let other = format!("uuid:0CDF-1366:cid:{}", "c".repeat(64));
        for (id, online) in [
            ("uuid:0CDF-1366", 0),
            (cid.as_str(), 1),
            (other.as_str(), 0),
        ] {
            c.execute(
                "INSERT INTO sources VALUES(?1,'/media/sd',?2)",
                params![id, online],
            )
            .unwrap();
        }
        let song = |source: &str, n: u32| Track {
            source_id: source.into(),
            path: PathBuf::from(format!("/media/sd/Album/{n}.flac")),
            title: format!("Song {n}"),
            ..Default::default()
        };
        let mut rows = vec![];
        for source in ["uuid:0CDF-1366", cid.as_str(), other.as_str()] {
            rows.extend((0..3).map(|n| song(source, n)));
        }
        batch(&mut c, rows, 1).unwrap();
        let card = Source {
            id: cid.clone(),
            kind: MediaSource::SdCard("0CDF-1366".into()),
            root: "/media/sd".into(),
            online: true,
            mount: "/dev/mmcblk1p1".into(),
            mount_id: None,
        };
        // The bare id, offline at the same root: retired, once.
        assert_eq!(retire_pre_cid_copy(&c, &card).unwrap(), 3);
        assert_eq!(retire_pre_cid_copy(&c, &card).unwrap(), 0);
        let live = |c: &Connection, source: &str| -> i64 {
            c.query_row(
                "SELECT count(*) FROM tracks WHERE source_id=?1 AND deleted=0",
                [source],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(live(&c, "uuid:0CDF-1366"), 0);
        // The card itself and another card with the same volume id stay.
        assert_eq!(live(&c, &cid), 3);
        assert_eq!(live(&c, &other), 3);
        // A card known only by its bare id, or internal storage, retires nothing.
        let bare = Source {
            id: "uuid:0CDF-1366".into(),
            ..card.clone()
        };
        assert_eq!(retire_pre_cid_copy(&c, &bare).unwrap(), 0);
        let internal = Source {
            id: "internal".into(),
            kind: MediaSource::Internal,
            ..card
        };
        assert_eq!(retire_pre_cid_copy(&c, &internal).unwrap(), 0);
        drop(c);
        let _ = fs::remove_file(path);
    }
    #[test]
    fn v1_index_upgrade_preserves_all_filter_combinations_and_rows() {
        let (path, mut c) = setup("browse-upgrade");
        let artist = "Artist ' OR 1=1 --";
        let album = "Album %_";
        let folder = "/media/sd/音楽%_/";
        let tracks = (0..24)
            .map(|i| Track {
                source_id: "sd".into(),
                path: PathBuf::from(format!(
                    "{}{i}.flac",
                    if i / 4 % 2 == 0 {
                        folder
                    } else {
                        "/media/sd/other/"
                    }
                )),
                artist: if i % 2 == 0 {
                    artist.into()
                } else {
                    "Another".into()
                },
                album: if i / 2 % 2 == 0 {
                    album.into()
                } else {
                    "Other album".into()
                },
                title: format!("Track {i:02}"),
                track: i + 1,
                ..Default::default()
            })
            .collect();
        batch(&mut c, tracks, 1).unwrap();
        // Reproduce an existing schema-v1 database, including offline/deleted
        // state. Upgrade must not discard state or require a schema bump.
        c.execute_batch(
            "DROP INDEX tracks_browse; DROP INDEX tracks_browse_album;
                         UPDATE sources SET online=0; UPDATE tracks SET deleted=1 WHERE id=1;",
        )
        .unwrap();
        drop(c);
        let c = open(&path).unwrap();
        assert_eq!(
            c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        for mask in 0..8 {
            let f = Filter {
                artist: (mask & 1 != 0).then(|| artist.into()),
                album: (mask & 2 != 0).then(|| album.into()),
                folder: (mask & 4 != 0).then(|| folder.into()),
                offset: 1,
                limit: 2,
            };
            let old = format!("{SELECT} WHERE t.deleted=0 AND (?1 IS NULL OR t.artist=?1) AND (?2 IS NULL OR t.album=?2) AND (?3 IS NULL OR substr(t.path,1,length(?3))=?3) ORDER BY t.artist,t.album,t.disc,t.track,t.title,t.path LIMIT ?4 OFFSET ?5");
            let mut statement = c.prepare(&old).unwrap();
            let expected = statement
                .query_map(
                    params![f.artist, f.album, f.folder, f.limit, f.offset],
                    from_row,
                )
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            let actual = list(&c, &f).unwrap();
            assert!(!actual.is_empty());
            assert!(actual.iter().all(|track| !track.online));
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(expected).unwrap(),
                "filter mask {mask}"
            );
        }
        assert_eq!(
            c.query_row("SELECT count(*) FROM tracks", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            24
        );
        drop(c);
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn offline_retains_tracks() {
        let (p, mut c) = setup("offline");
        batch(
            &mut c,
            vec![Track {
                source_id: "sd".into(),
                path: "/media/sd/one.flac".into(),
                title: "One".into(),
                ..Default::default()
            }],
            1,
        )
        .unwrap();
        c.execute("UPDATE sources SET online=0", []).unwrap();
        let rows = list(
            &c,
            &Filter {
                limit: 64,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].online);
        drop(c);
        fs::remove_file(p).unwrap();
    }
    #[test]
    fn upsert_preserves_id_and_reuses_metadata() {
        let (p, mut c) = setup("upsert");
        let mut track = Track {
            source_id: "sd".into(),
            path: "/media/sd/one.flac".into(),
            title: "One".into(),
            ..Default::default()
        };
        batch(&mut c, vec![track.clone()], 1).unwrap();
        let first = list(
            &c,
            &Filter {
                limit: 64,
                ..Default::default()
            },
        )
        .unwrap()[0]
            .id;
        track.title = "Changed".into();
        track.size = 123;
        batch(&mut c, vec![track], 2).unwrap();
        let rows = list(
            &c,
            &Filter {
                limit: 64,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, first);
        assert_eq!(rows[0].title, "Changed");
        drop(c);
        fs::remove_file(p).unwrap();
    }
    #[test]
    fn queries_treat_metadata_as_data() {
        let (p, mut c) = setup("sql");
        let artist = "' OR 1=1 --";
        batch(
            &mut c,
            vec![Track {
                source_id: "sd".into(),
                path: "/media/sd/a.wav".into(),
                artist: artist.into(),
                ..Default::default()
            }],
            1,
        )
        .unwrap();
        assert_eq!(
            list(
                &c,
                &Filter {
                    artist: Some(artist.into()),
                    limit: 64,
                    ..Default::default()
                }
            )
            .unwrap()
            .len(),
            1
        );
        assert!(list(
            &c,
            &Filter {
                artist: Some("different".into()),
                limit: 64,
                ..Default::default()
            }
        )
        .unwrap()
        .is_empty());
        drop(c);
        fs::remove_file(p).unwrap();
    }
}

#[cfg(test)]
mod scan_safety_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    fn root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "reborn-scan-{label}-{}-{}",
            std::process::id(),
            reborn_observability::wall_ms()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn source(id: &str, path: &Path) -> Source {
        Source {
            id: id.into(),
            kind: MediaSource::Internal,
            root: path.into(),
            online: true,
            mount: "test".into(),
            mount_id: None,
        }
    }

    fn database(root: &Path) -> (Database, Observer) {
        let log = Observer::new(&root.join("logs")).unwrap();
        let db = Database::spawn(root.join("library.db"), log.clone()).unwrap();
        (db, log)
    }

    fn seed(db: &Database, source: &Source, path: &Path) {
        db.sources(vec![source.clone()]).unwrap();
        let token = db.begin_scan(source.id.clone()).unwrap();
        let (reply, result) = sync_channel(1);
        db.tx
            .send(DbCommand::Batch(
                vec![Track {
                    source_id: source.id.clone(),
                    path: path.into(),
                    filename: "old.flac".into(),
                    size: 1,
                    mtime: 1,
                    title: "Prior Valid Row".into(),
                    ..Default::default()
                }],
                token,
                reply,
            ))
            .unwrap();
        result
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap();
    }

    fn tracks(db: &Database) -> Vec<Track> {
        db.list(Filter {
            limit: 100,
            ..Default::default()
        })
        .unwrap()
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap()
    }

    #[test]
    fn rebuild_never_reuses_unchanged_rows_while_scan_does() {
        let root = root("rebuild-reuse");
        let path = root.join("same.flac");
        fs::write(&path, b"not decodable").unwrap();
        let meta = fs::metadata(&path).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        let source = source("internal", &root);
        let (db, log) = database(&root);
        db.sources(vec![source.clone()]).unwrap();
        let token = db.begin_scan(source.id.clone()).unwrap();
        let (reply, result) = sync_channel(1);
        db.tx
            .send(DbCommand::Batch(
                vec![Track {
                    source_id: source.id.clone(),
                    path: path.clone(),
                    filename: "same.flac".into(),
                    size: meta.len(),
                    mtime,
                    title: "Indexed".into(),
                    ..Default::default()
                }],
                token,
                reply,
            ))
            .unwrap();
        result
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap();
        let stop = AtomicBool::new(false);
        let scan = scan_sources_with_identity_and_reuse(
            &db,
            std::slice::from_ref(&source),
            &log,
            &stop,
            |_| true,
            true,
            &mut Unreadable::default(),
        )
        .unwrap();
        assert_eq!(scan.reused, 1);
        let rebuild = scan_sources_with_identity_and_reuse(
            &db,
            &[source],
            &log,
            &stop,
            |_| true,
            false,
            &mut Unreadable::default(),
        )
        .unwrap();
        assert_eq!(rebuild.reused, 0);
        // A file that cannot be re-read keeps its prior row: rebuild is not a wipe.
        assert_eq!(tracks(&db)[0].title, "Indexed");
        db.stop();
    }

    #[test]
    fn changed_file_decoder_open_failure_retains_prior_row() {
        let root = root("changed-open-failure");
        let path = root.join("changed.flac");
        fs::write(&path, b"changed file with unreadable transient bytes").unwrap();
        let source = source("internal", &root);
        let (db, log) = database(&root);
        seed(&db, &source, &path);

        let stats =
            scan_sources_with_identity(&db, &[source], &log, &AtomicBool::new(false), |_| true)
                .unwrap();

        // Unreadable is not incomplete (B3): the row stays, the scan completes.
        assert!(stats.complete);
        assert_eq!(stats.failures, 0);
        assert_eq!(stats.unreadable, 1);
        let rows = tracks(&db);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Prior Valid Row");
        assert_eq!(rows[0].size, 1);
        db.stop();
    }

    #[test]
    fn sd_identity_rejects_unmounted_and_reused_mount_paths() {
        let source = Source {
            id: "uuid:old-volume".into(),
            kind: MediaSource::SdCard("old-volume".into()),
            root: "/media/sd".into(),
            online: true,
            mount: "/dev/mmcblk0p1".into(),
            mount_id: Some(40),
        };
        let original = "40 1 179:1 / /media/sd rw - vfat /dev/mmcblk0p1 rw\n";
        let empty_mountpoint = "1 0 8:1 / / rw - ext4 /dev/root rw\n";
        let replacement = "58 1 179:2 / /media/sd rw - ext4 /dev/mmcblk1p1 rw\n";

        assert!(source_identity_matches_at(&source, original));
        assert!(!source_identity_matches_at(&source, empty_mountpoint));
        assert!(!source_identity_matches_at(&source, replacement));
        let replacement_source = Source {
            id: "uuid:new-volume".into(),
            kind: MediaSource::SdCard("new-volume".into()),
            mount_id: Some(58),
            ..source.clone()
        };
        assert!(source_identity_matches_at(&replacement_source, replacement));
    }

    #[test]
    fn identity_loss_mid_traversal_disables_pruning() {
        let root = root("identity-loss");
        let source = Source {
            id: "uuid:old-volume".into(),
            kind: MediaSource::SdCard("old-volume".into()),
            root: root.clone(),
            online: true,
            mount: "/dev/mmcblk0p1".into(),
            mount_id: Some(40),
        };
        let old_path = root.join("removed.flac");
        let (db, log) = database(&root);
        seed(&db, &source, &old_path);
        let checks = AtomicUsize::new(0);

        let stats =
            scan_sources_with_identity(&db, &[source], &log, &AtomicBool::new(false), |_| {
                checks.fetch_add(1, Ordering::Relaxed) < 3
            })
            .unwrap();

        assert!(!stats.complete);
        assert_eq!(tracks(&db).len(), 1);
        db.stop();
    }

    #[test]
    fn writer_rechecks_mount_identity_at_finish_before_pruning() {
        let root = root("identity-at-finish");
        let source = Source {
            id: "uuid:old-volume".into(),
            kind: MediaSource::SdCard("old-volume".into()),
            root: root.clone(),
            online: true,
            mount: "/dev/mmcblk0p1".into(),
            mount_id: Some(u64::MAX),
        };
        let old_path = root.join("removed.flac");
        let (db, log) = database(&root);
        seed(&db, &source, &old_path);

        let stats =
            scan_sources_with_identity(&db, &[source], &log, &AtomicBool::new(false), |_| true)
                .unwrap();

        assert!(!stats.complete);
        assert_eq!(tracks(&db).len(), 1);
        db.stop();
    }

    #[test]
    fn complete_identity_consistent_scan_still_prunes_missing_rows() {
        let root = root("safe-prune");
        let source = source("internal", &root);
        let old_path = root.join("removed.flac");
        let (db, log) = database(&root);
        seed(&db, &source, &old_path);

        let stats =
            scan_sources_with_identity(&db, &[source], &log, &AtomicBool::new(false), |_| true)
                .unwrap();

        assert!(stats.complete);
        assert_eq!(stats.removed, 1);
        assert!(tracks(&db).is_empty());
        let unchanged = scan_sources_with_identity(
            &db,
            &[Source {
                id: "internal".into(),
                kind: MediaSource::Internal,
                root: root.clone(),
                online: true,
                mount: "test".into(),
                mount_id: None,
            }],
            &log,
            &AtomicBool::new(false),
            |_| true,
        )
        .unwrap();
        assert_eq!(unchanged.removed, 0);
        assert_eq!(unchanged.rescanned, 0);
        db.stop();
    }

    #[test]
    fn unreadable_file_keeps_its_row_and_deleted_files_are_still_pruned() {
        let root = root("unreadable-prune");
        let source = source("internal", &root);
        let broken = root.join("broken.mp3");
        let removed = root.join("removed.flac");
        fs::write(&broken, b"this is not audio").unwrap();
        let (db, log) = database(&root);
        seed(&db, &source, &broken);
        seed(&db, &source, &removed);

        let stats = scan_sources_with_identity(
            &db,
            std::slice::from_ref(&source),
            &log,
            &AtomicBool::new(false),
            |_| true,
        )
        .unwrap();

        assert!(stats.complete);
        assert_eq!(stats.unreadable, 1);
        assert_eq!(stats.removed, 1);
        let rows = tracks(&db);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, broken);

        // Within one process the failure is remembered: not opened again.
        let mut unreadable = Unreadable::default();
        let stop = AtomicBool::new(false);
        let first = scan_sources_with_identity_and_reuse(
            &db,
            std::slice::from_ref(&source),
            &log,
            &stop,
            |_| true,
            true,
            &mut unreadable,
        )
        .unwrap();
        let again = scan_sources_with_identity_and_reuse(
            &db,
            std::slice::from_ref(&source),
            &log,
            &stop,
            |_| true,
            true,
            &mut unreadable,
        )
        .unwrap();
        assert_eq!((first.unreadable, again.unreadable), (1, 1));
        assert_eq!(unreadable.len(), 1);
        assert!(again.complete);
        assert_eq!(tracks(&db).len(), 1);
        db.stop();
    }

    #[test]
    fn macos_companions_and_system_folders_never_make_a_scan_incomplete() {
        use std::os::unix::fs::PermissionsExt;
        let root = root("macos-card");
        let source = source("internal", &root);
        let album = root.join("Album");
        fs::create_dir(&album).unwrap();
        let tone = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac");
        fs::copy(tone, album.join("01 Song.flac")).unwrap();
        // AppleDouble companion: Finder metadata under an audio name.
        fs::write(album.join("._01 Song.flac"), b"\0\x05\x16\x07Mac OS X").unwrap();
        let spotlight = root.join(".Spotlight-V100");
        fs::create_dir(&spotlight).unwrap();
        fs::write(spotlight.join("store.flac"), b"index").unwrap();
        fs::set_permissions(&spotlight, fs::Permissions::from_mode(0o000)).unwrap();
        let (db, log) = database(&root);
        db.sources(vec![source.clone()]).unwrap();
        let stats =
            scan_sources_with_identity(&db, &[source], &log, &AtomicBool::new(false), |_| true)
                .unwrap();
        fs::set_permissions(&spotlight, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(stats.complete);
        assert_eq!(stats.failures, 0);
        assert_eq!(stats.discovered, 1);
        let rows = tracks(&db);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].path.ends_with("Album/01 Song.flac"));
        db.stop();
    }

    fn seed_changed_audio(root: &Path) -> (Database, Observer, Source, PathBuf) {
        let source = source("internal", root);
        let path = root.join("changed.flac");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac");
        fs::copy(fixture, &path).unwrap();
        let (db, log) = database(root);
        seed(&db, &source, &path);
        (db, log, source, path)
    }

    #[test]
    fn batch_failure_keeps_prior_rows_and_never_finishes_scan() {
        let root = root("batch-failure");
        let (db, log, source, _) = seed_changed_audio(&root);
        db.execute_test_sql(
            "CREATE TRIGGER fail_batch BEFORE INSERT ON tracks WHEN NEW.path LIKE '%changed.flac' BEGIN SELECT RAISE(FAIL, 'injected batch failure'); END;".into(),
        )
        .unwrap();

        let error =
            scan_sources_with_identity(&db, &[source], &log, &AtomicBool::new(false), |_| true)
                .unwrap_err();

        assert!(error.contains("injected batch failure"));
        let rows = tracks(&db);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Prior Valid Row");
        db.stop();
    }

    #[test]
    fn simulated_enospc_batch_failure_keeps_prior_rows() {
        let root = root("enospc-batch");
        let (db, log, source, _) = seed_changed_audio(&root);
        db.execute_test_sql(
            "CREATE TRIGGER fail_full BEFORE INSERT ON tracks WHEN NEW.path LIKE '%changed.flac' BEGIN SELECT RAISE(FAIL, 'database or disk is full'); END;".into(),
        )
        .unwrap();

        let error =
            scan_sources_with_identity(&db, &[source], &log, &AtomicBool::new(false), |_| true)
                .unwrap_err();

        assert!(error.to_ascii_lowercase().contains("full"));
        assert_eq!(tracks(&db).len(), 1);
        db.stop();
    }

    #[test]
    fn finish_failure_does_not_prune_prior_rows() {
        let root = root("finish-failure");
        let source = source("internal", &root);
        let old_path = root.join("removed.flac");
        let (db, log) = database(&root);
        seed(&db, &source, &old_path);
        db.execute_test_sql(
            "CREATE TRIGGER fail_finish BEFORE UPDATE OF deleted ON tracks WHEN NEW.deleted=1 BEGIN SELECT RAISE(FAIL, 'injected finish failure'); END;".into(),
        )
        .unwrap();

        let error =
            scan_sources_with_identity(&db, &[source], &log, &AtomicBool::new(false), |_| true)
                .unwrap_err();

        assert!(error.contains("injected finish failure"));
        assert_eq!(tracks(&db).len(), 1);
        db.stop();
    }
}

#[cfg(test)]
mod sd_qualified_source_id_tests {
    use super::*;
    #[test]
    fn qualified_and_uuidless_ids_preserve_mount_guard() {
        let hash = "a".repeat(64);
        for (id, uuid) in [
            (format!("uuid:volume:cid:{hash}"), "volume".to_owned()),
            (format!("uuid:cid:{hash}"), format!("cid:{hash}")),
            ("uuid:volume".to_owned(), "volume".to_owned()),
        ] {
            let source = Source {
                id,
                kind: MediaSource::SdCard(uuid),
                root: "/media/sd".into(),
                online: true,
                mount: "/dev/mmcblk0p1".into(),
                mount_id: Some(40),
            };
            assert!(source_identity_matches_at(
                &source,
                "40 1 179:1 / /media/sd rw - vfat /dev/mmcblk0p1 rw\n"
            ));
            assert!(!source_identity_matches_at(
                &source,
                "58 1 179:1 / /media/sd rw - vfat /dev/mmcblk0p1 rw\n"
            ));
        }
        assert!(!sd_id_matches_uuid("uuid:other:cid:bad", "volume"));
        assert!(!sd_id_matches_uuid("uuid:volume:cid:bad", "volume"));
    }
}

#[cfg(test)]
mod ingest_regression_tests {
    use super::*;
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/fixtures")
            .join(name)
    }
    #[test]
    fn incremental_scan_touches_only_identity_and_tracks_edits_moves_and_removals() {
        let root = std::env::temp_dir().join(format!("reborn-ingest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let log = Observer::new(&root.join("logs")).unwrap();
        let db = Database::spawn(root.join("library.sqlite"), log.clone()).unwrap();
        let source = Source {
            id: "internal".into(),
            kind: MediaSource::Internal,
            root: root.clone(),
            online: true,
            mount: "fixture".into(),
            mount_id: None,
        };
        db.sources(vec![source.clone()]).unwrap();
        let file = root.join("a.flac");
        fs::copy(fixture("tone.flac"), &file).unwrap();
        let scan = || {
            scan_sources_with_identity(
                &db,
                std::slice::from_ref(&source),
                &log,
                &AtomicBool::new(false),
                |_| true,
            )
            .unwrap()
        };
        assert_eq!(scan().rescanned, 1);
        let rows = || {
            db.list(Filter {
                limit: 100,
                ..Filter::default()
            })
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
        };
        let id = rows()[0].id;
        db.execute_test_sql("CREATE TRIGGER forbid_metadata_rewrite BEFORE UPDATE OF title ON tracks BEGIN SELECT RAISE(FAIL,'reused metadata rewritten'); END;".into()).unwrap();
        let metrics = scan();
        assert_eq!(metrics.reused, 1);
        assert_eq!(metrics.ffmpeg_info_runs, 0);
        assert_eq!(metrics.metadata_us, 0);
        assert_eq!(rows()[0].id, id);
        db.execute_test_sql("DROP TRIGGER forbid_metadata_rewrite".into())
            .unwrap();
        fs::copy(fixture("metadata/tags.flac"), &file).unwrap();
        assert_eq!(scan().rescanned, 1);
        assert_eq!(rows()[0].title, "日本語 – café");
        assert_eq!(rows()[0].id, id);
        fs::rename(&file, root.join("moved.flac")).unwrap();
        assert!(scan().complete);
        assert_eq!(rows().len(), 1);
        assert!(rows()[0].path.ends_with("moved.flac"));
        fs::remove_file(root.join("moved.flac")).unwrap();
        assert!(scan().complete);
        assert!(rows().is_empty());
        // A bad file never aborts ingestion of a separate good file.
        fs::write(&file, b"bad media").unwrap();
        fs::copy(fixture("tone.flac"), root.join("good.flac")).unwrap();
        let metrics = scan();
        assert_eq!((metrics.failures, metrics.unreadable), (0, 1));
        assert!(metrics.complete);
        assert_eq!(metrics.rescanned, 1);
        assert_eq!(rows().len(), 1);
        db.shutdown(Duration::from_secs(5)).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn genre_is_stored_and_a_pre_genre_library_reads_it_once() {
        let root = std::env::temp_dir().join(format!("reborn-genre-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let log = Observer::new(&root.join("logs")).unwrap();
        let source = Source {
            id: "internal".into(),
            kind: MediaSource::Internal,
            root: root.clone(),
            online: true,
            mount: "fixture".into(),
            mount_id: None,
        };
        fs::copy(fixture("metadata/tags.flac"), root.join("song.flac")).unwrap();
        let genres = |db: &Database| -> Vec<String> {
            db.list(Filter {
                limit: 10,
                ..Filter::default()
            })
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|t| t.genre)
            .collect()
        };
        let scan = |db: &Database| {
            scan_sources_with_identity(
                db,
                std::slice::from_ref(&source),
                &log,
                &AtomicBool::new(false),
                |_| true,
            )
            .unwrap()
        };
        let db = Database::spawn(root.join("library.sqlite"), log.clone()).unwrap();
        db.sources(vec![source.clone()]).unwrap();
        assert_eq!(scan(&db).rescanned, 1);
        assert_eq!(genres(&db), ["Ambient"]);
        // A revision-2 library: no genre column yet.
        db.execute_test_sql(
            "UPDATE tracks SET metadata_revision=2; ALTER TABLE tracks DROP COLUMN genre;".into(),
        )
        .unwrap();
        db.shutdown(Duration::from_secs(5)).unwrap();
        let db = Database::spawn(root.join("library.sqlite"), log.clone()).unwrap();
        db.sources(vec![source.clone()]).unwrap();
        assert_eq!(genres(&db), [""]);
        assert_eq!(scan(&db).rescanned, 1);
        assert_eq!(scan(&db).reused, 1);
        assert_eq!(genres(&db), ["Ambient"]);
        db.shutdown(Duration::from_secs(5)).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn extractor_revision_refreshes_old_rows_once_without_a_library_rebuild() {
        let root =
            std::env::temp_dir().join(format!("reborn-ingest-upgrade-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let log = Observer::new(&root.join("logs")).unwrap();
        let db = Database::spawn(root.join("library.sqlite"), log.clone()).unwrap();
        let source = Source {
            id: "internal".into(),
            kind: MediaSource::Internal,
            root: root.clone(),
            online: true,
            mount: "fixture".into(),
            mount_id: None,
        };
        db.sources(vec![source.clone()]).unwrap();
        fs::copy(fixture("metadata/tags.aiff"), root.join("song.aiff")).unwrap();
        let scan = || {
            scan_sources_with_identity(
                &db,
                std::slice::from_ref(&source),
                &log,
                &AtomicBool::new(false),
                |_| true,
            )
            .unwrap()
        };
        assert_eq!(scan().rescanned, 1);
        db.execute_test_sql("UPDATE tracks SET metadata_revision=0,artist='old extractor'; ALTER TABLE tracks DROP COLUMN metadata_revision;".into()).unwrap();
        db.shutdown(Duration::from_secs(5)).unwrap();
        // Reopening performs the additive migration on an actual schema-1 DB.
        let db = Database::spawn(root.join("library.sqlite"), log.clone()).unwrap();
        db.sources(vec![source.clone()]).unwrap();
        let scan = || {
            scan_sources_with_identity(
                &db,
                std::slice::from_ref(&source),
                &log,
                &AtomicBool::new(false),
                |_| true,
            )
            .unwrap()
        };
        assert_eq!(scan().rescanned, 1);
        assert_eq!(scan().reused, 1);
        let rows = db
            .list(Filter {
                limit: 10,
                ..Filter::default()
            })
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(rows[0].artist, "Björk");
        fs::write(root.join("bad.flac"), b"bad media").unwrap();
        let result = scan();
        assert_eq!(result.reused, 1);
        assert_eq!(result.rescanned, 0);
        assert_eq!((result.failures, result.unreadable), (0, 1));
        db.shutdown(Duration::from_secs(5)).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn same_size_and_mtime_replacement_is_rejected_during_parse() {
        let root =
            std::env::temp_dir().join(format!("reborn-ingest-revision-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("track");
        fs::write(&path, b"old").unwrap();
        let before = fs::metadata(&path).unwrap();
        assert!(same_file(&before, &fs::metadata(&path).unwrap()));
        let replacement = root.join("replacement");
        fs::write(&replacement, b"new").unwrap();
        fs::File::open(&replacement)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(before.modified().unwrap()))
            .unwrap();
        fs::rename(replacement, &path).unwrap();
        let after = fs::metadata(&path).unwrap();
        assert_eq!(before.len(), after.len());
        assert_eq!(file_mtime(&before), file_mtime(&after));
        assert!(!same_file(&before, &after));
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod parallel_read_tests {
    use super::{read_parallel, read_tags, FileStamp, TagRead};
    use std::{fs, path::PathBuf, sync::atomic::AtomicBool};

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/fixtures")
            .join(name)
    }

    fn summary(read: &TagRead) -> String {
        match read {
            TagRead::Parsed {
                metadata, changed, ..
            } => format!("{}:{}:{changed}", metadata.codec, metadata.sample_rate),
            TagRead::Failed { .. } => "failed".into(),
            TagRead::Fatal(e) => format!("fatal {e}"),
            TagRead::Panicked => "panicked".into(),
        }
    }

    #[test]
    fn several_workers_read_every_file_and_keep_the_order() {
        let garbage =
            std::env::temp_dir().join(format!("reborn-garbage-{}.flac", std::process::id()));
        fs::write(&garbage, b"not audio").unwrap();
        let names = [
            "flac-16-44100.flac",
            "flac-24-96000.flac",
            "mp3.mp3",
            "opus.opus",
            "flac-16-48000.flac",
        ];
        let mut paths: Vec<PathBuf> = (0..6)
            .flat_map(|_| names.iter().map(|n| fixture(n)))
            .collect();
        paths.insert(7, garbage.clone());
        let pending: Vec<_> = paths
            .iter()
            .map(|path| {
                let metadata = fs::metadata(path).unwrap();
                let stamp = FileStamp {
                    path: path.clone(),
                    size: metadata.len(),
                    mtime: 0,
                    metadata: Some(metadata),
                };
                (stamp, (String::new(), path.clone(), 0, 0), path.clone())
            })
            .collect();
        let expected: Vec<String> = pending
            .iter()
            .map(|(file, _, path)| summary(&read_tags(path, file.metadata.as_ref())))
            .collect();
        assert_eq!(expected[7], "failed");
        assert!(
            expected
                .iter()
                .filter(|e| e.starts_with("flac:96000"))
                .count()
                == 6
        );
        let stop = AtomicBool::new(false);
        let parallel: Vec<String> = read_parallel(&pending, 3, &stop)
            .iter()
            .map(summary)
            .collect();
        fs::remove_file(garbage).unwrap();
        assert_eq!(parallel, expected);
    }
}
#[cfg(test)]
mod quick_scan_tests {
    use super::*;

    struct Library {
        media: PathBuf,
        source: Source,
        db: Database,
        log: Observer,
    }
    impl Library {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "reborn-quick-{label}-{}-{}",
                std::process::id(),
                reborn_observability::wall_ms()
            ));
            // The database lives beside the music: its writes must not touch the scanned folders.
            let media = root.join("media");
            let album = media.join("Artist/Album");
            fs::create_dir_all(&album).unwrap();
            fs::create_dir_all(media.join("Other")).unwrap();
            let tone =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac");
            fs::copy(&tone, album.join("1.flac")).unwrap();
            fs::copy(&tone, album.join("2.flac")).unwrap();
            fs::copy(&tone, media.join("top.flac")).unwrap();
            fs::write(media.join("Other/broken.mp3"), b"this is not audio").unwrap();
            fs::write(media.join("Other/mix.m3u"), b"../top.flac\n").unwrap();
            let log = Observer::new(&root.join("logs")).unwrap();
            let db = Database::spawn(root.join("library.db"), log.clone()).unwrap();
            let source = Source {
                id: "internal".into(),
                kind: MediaSource::Internal,
                root: media.clone(),
                online: true,
                mount: "test".into(),
                mount_id: None,
            };
            db.sources(vec![source.clone()]).unwrap();
            Self {
                media,
                source,
                db,
                log,
            }
        }
        fn scan(&self, mode: ScanMode, free: Option<u64>) -> ScanMetrics {
            let stats = scan_sources(
                &self.db,
                std::slice::from_ref(&self.source),
                &self.log,
                &AtomicBool::new(false),
                |_| true,
                mode,
                |_| free,
                &mut Unreadable::default(),
            )
            .unwrap();
            assert!(stats.complete, "{stats:?}");
            stats
        }
        fn paths(&self) -> Vec<PathBuf> {
            let mut paths: Vec<_> = self
                .db
                .list(Filter {
                    limit: 100,
                    ..Default::default()
                })
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap()
                .into_iter()
                .map(|t| t.path.strip_prefix(&self.media).unwrap().to_path_buf())
                .collect();
            paths.sort();
            paths
        }
    }

    #[test]
    fn unchanged_folders_are_not_read_again_and_count_as_before() {
        let lib = Library::new("unchanged");
        let first = lib.scan(ScanMode::Quick, Some(7));
        // Nothing recorded yet: a whole walk.
        assert!(!first.quick);
        assert_eq!(
            (first.discovered, first.rescanned, first.unreadable),
            (4, 3, 1)
        );
        let again = lib.scan(ScanMode::Quick, Some(7));
        assert!(again.quick);
        assert_eq!(again.folders_skipped, 4);
        assert_eq!(
            (
                again.discovered,
                again.reused,
                again.unreadable,
                again.rescanned,
                again.removed
            ),
            (4, 3, 1, 0, 0)
        );
        assert_eq!(again.playlists, first.playlists);
        assert_eq!(again.playlists[0].1, vec![lib.media.join("Other/mix.m3u")]);
        assert_eq!(lib.paths().len(), 3, "rows of skipped folders are kept");
        // And the record carries over to the scan after.
        assert_eq!(lib.scan(ScanMode::Quick, Some(7)).folders_skipped, 4);
        lib.db.stop();
    }

    #[test]
    fn a_folder_with_a_new_time_is_read_and_its_deletions_pruned() {
        let lib = Library::new("changed");
        lib.scan(ScanMode::Quick, Some(7));
        fs::remove_file(lib.media.join("Artist/Album/2.flac")).unwrap();
        // Same free space (a rename on the player, say): only the album is read.
        let stats = lib.scan(ScanMode::Quick, Some(7));
        assert!(stats.quick);
        assert_eq!(stats.folders_skipped, 3);
        assert_eq!((stats.removed, stats.discovered), (1, 3));
        assert_eq!(
            lib.paths(),
            vec![
                PathBuf::from("Artist/Album/1.flac"),
                PathBuf::from("top.flac")
            ]
        );
        lib.db.stop();
    }

    #[test]
    fn other_free_space_or_scan_for_music_reads_every_folder() {
        let lib = Library::new("free");
        lib.scan(ScanMode::Quick, Some(7));
        let tone = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac");
        // A computer adds a song without changing the album's time: the free space tells.
        let album = lib.media.join("Artist/Album");
        let time = fs::metadata(&album).unwrap().modified().unwrap();
        fs::copy(&tone, album.join("3.flac")).unwrap();
        fs::File::open(&album).unwrap().set_modified(time).unwrap();
        let unchanged_free = lib.scan(ScanMode::Quick, Some(7));
        assert_eq!(
            unchanged_free.rescanned, 0,
            "the trade-off: trusted folder time"
        );
        let stats = lib.scan(ScanMode::Quick, Some(8));
        assert!(!stats.quick);
        assert_eq!((stats.folders_skipped, stats.rescanned), (0, 1));
        // Scan for Music never trusts the record, but leaves one for the next scan.
        let all = lib.scan(ScanMode::Walk, Some(8));
        assert_eq!(
            (all.quick, all.folders_skipped, all.discovered),
            (false, 0, 5)
        );
        assert!(lib.scan(ScanMode::Quick, Some(8)).quick);
        // No fingerprint (a FAT32 card): every folder, and the record is dropped.
        assert!(!lib.scan(ScanMode::Quick, None).quick);
        assert!(!lib.scan(ScanMode::Quick, Some(8)).quick);
        lib.db.stop();
    }

    #[test]
    fn mount_type_comes_from_the_longest_matching_mount() {
        let info = "22 1 179:5 / / ro - ext4 /dev/mmcblk0p5 ro\n\
                    34 22 179:9 / /media/sd ro,noatime - exfat /dev/mmcblk1p1 ro\n";
        assert_eq!(
            mount_for_path(Path::new("/media/sd/Music"), info),
            Some((34, "exfat".into()))
        );
        assert_eq!(mount_id_for_path(Path::new("/data"), info), Some(22));
    }
}
