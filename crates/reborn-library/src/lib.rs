#![forbid(unsafe_code)]
pub mod benchmark;
#[cfg(test)]
mod platform_tests;
pub mod playlists;
use reborn_core::{MediaSource, Playlist, Source, Track};
use reborn_media::{Cancel, MediaFile};
use reborn_observability::{HealthState, Level, Observer};
use rusqlite::{params, Connection, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender},
        Arc,
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
    pub discovered: u64,
    pub reused: u64,
    pub rescanned: u64,
    #[serde(default)]
    pub removed: u64,
    pub failures: u64,
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
    /// Playlist files found on the scanned media.
    #[serde(default)]
    pub playlists: u64,
    /// Playlists added, re-read, re-resolved or removed by this scan.
    #[serde(default)]
    pub playlists_changed: u64,
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
    Finish(Source, i64, bool, SyncSender<Result<ScanFinish, String>>),
    PlaylistProbe(
        String,
        Vec<PlaylistStamp>,
        SyncSender<Result<Vec<bool>, String>>,
    ),
    PlaylistSync(PlaylistSync, SyncSender<Result<u64, String>>),
    Playlists(SyncSender<Result<Vec<Playlist>, String>>),
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
    // Additive like the browse indexes: schema-1 readers ignore the table.
    // `locations` keeps each entry as written so a library change can
    // re-resolve without reading the playlist file again.
    c.execute_batch("CREATE TABLE IF NOT EXISTS playlists(id INTEGER PRIMARY KEY, source_id TEXT NOT NULL REFERENCES sources(id), path TEXT NOT NULL, name TEXT NOT NULL, size INTEGER NOT NULL, mtime INTEGER NOT NULL, locations TEXT NOT NULL, track_ids BLOB NOT NULL, unresolved INTEGER NOT NULL, UNIQUE(source_id,path));")
        .map_err(|error| classify_open_error(error, OpenStage::Migration))?;
    c.execute_batch("CREATE TEMP TABLE scan_seen(id INTEGER PRIMARY KEY);
        CREATE TEMP TABLE scan_candidates(position INTEGER PRIMARY KEY,path TEXT NOT NULL,size INTEGER NOT NULL,mtime INTEGER NOT NULL);")
        .map_err(|error| classify_open_error(error, OpenStage::Setup))?;
    Ok(c)
}
fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Track> {
    Ok(Track {
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
    })
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
    let join = format!("FROM scan_candidates c LEFT JOIN tracks t ON t.source_id=?1 AND t.path=c.path AND t.size=c.size AND t.mtime=c.mtime AND t.deleted=0 AND t.metadata_revision={METADATA_REVISION}");
    let matches = {
        let mut q = tx
            .prepare_cached(&format!(
                "SELECT t.id IS NOT NULL {join} ORDER BY c.position"
            ))
            .map_err(err)?;
        let rows = q
            .query_map([source], |r| r.get::<_, bool>(0))
            .map_err(err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
    };
    tx.execute(
        &format!("INSERT OR IGNORE INTO scan_seen SELECT t.id {join} WHERE t.id IS NOT NULL"),
        [source],
    )
    .map_err(err)?;
    tx.commit().map_err(err)?;
    Ok(matches)
}
#[derive(Clone)]
struct ScanFinish {
    verified: bool,
    removed: u64,
}
fn finish(c: &mut Connection, source: &Source, complete: bool) -> Result<ScanFinish, String> {
    let prune = complete && source_identity_is_current(source);
    let tx = c.transaction().map_err(err)?;
    let removed = if prune {
        tx.execute("UPDATE tracks SET deleted=1 WHERE source_id=?1 AND deleted=0 AND NOT EXISTS (SELECT 1 FROM scan_seen WHERE id=tracks.id)", [&source.id]).map_err(err)? as u64
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
/// A playlist file seen by the scanner: logical path, size and mtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistStamp {
    path: PathBuf,
    size: u64,
    mtime: i64,
}
/// A discovered playlist file; `content` is present when it was (re)read.
#[derive(Debug, Clone)]
pub(crate) struct PlaylistFile {
    source_id: String,
    stamp: PlaylistStamp,
    content: Option<Vec<u8>>,
}
#[derive(Debug, Clone, Default)]
pub(crate) struct PlaylistSync {
    files: Vec<PlaylistFile>,
    /// Sources whose traversal was complete: their vanished playlists go.
    complete_sources: Vec<String>,
    /// Tracks were added, changed or removed: re-resolve stored entries.
    library_changed: bool,
}
/// Which stamps differ from the stored playlist (or have none yet).
fn playlist_probe(
    c: &Connection,
    source: &str,
    stamps: &[PlaylistStamp],
) -> Result<Vec<bool>, String> {
    let mut q = c
        .prepare_cached(
            "SELECT 1 FROM playlists WHERE source_id=?1 AND path=?2 AND size=?3 AND mtime=?4",
        )
        .map_err(err)?;
    stamps
        .iter()
        .map(|s| {
            q.exists(params![source, s.path.to_string_lossy(), s.size, s.mtime])
                .map(|unchanged| !unchanged)
                .map_err(err)
        })
        .collect()
}
fn library_index(c: &Connection) -> Result<playlists::LibraryIndex, String> {
    let mut q = c
        // Only playable tracks: an offline source (for example the same card
        // under an older source id) holds the same paths again, which made
        // every entry ambiguous and therefore unresolved.
        .prepare_cached(
            "SELECT t.id,t.path FROM tracks t JOIN sources s ON s.id=t.source_id \
             WHERE t.deleted=0 AND s.online=1",
        )
        .map_err(err)?;
    let rows = q
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(err)?;
    Ok(playlists::LibraryIndex::new(
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?,
    ))
}
/// Store re-read playlists, re-resolve stored ones after library changes
/// and forget playlists whose files are gone. Returns how many changed.
fn sync_playlists(c: &mut Connection, sync: PlaylistSync) -> Result<u64, String> {
    let tx = c.transaction().map_err(err)?;
    let roots: std::collections::HashMap<String, PathBuf> = {
        let mut q = tx
            .prepare_cached("SELECT id,root FROM sources")
            .map_err(err)?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    PathBuf::from(r.get::<_, String>(1)?),
                ))
            })
            .map_err(err)?;
        rows.collect::<rusqlite::Result<_>>().map_err(err)?
    };
    let stored: i64 = tx
        .query_row("SELECT count(*) FROM playlists", [], |r| r.get(0))
        .map_err(err)?;
    let fresh = sync.files.iter().any(|f| f.content.is_some());
    // Entries that did not resolve are retried on every sync, so a fix to
    // the library (a source coming back online, say) heals them without
    // the playlist file itself changing.
    let missing: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM playlists WHERE unresolved>0)",
            [],
            |r| r.get(0),
        )
        .map_err(err)?;
    let recheck = sync.library_changed || missing;
    let index = if fresh || (recheck && stored > 0) {
        Some(library_index(&tx)?)
    } else {
        None
    };
    let root_of = |source: &str| roots.get(source).cloned().unwrap_or_else(|| "/".into());
    let mut changed = 0u64;
    let mut touched = std::collections::HashSet::new();
    if let Some(index) = &index {
        let mut upsert = tx.prepare_cached("INSERT INTO playlists(source_id,path,name,size,mtime,locations,track_ids,unresolved) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(source_id,path) DO UPDATE SET name=excluded.name,size=excluded.size,mtime=excluded.mtime,locations=excluded.locations,track_ids=excluded.track_ids,unresolved=excluded.unresolved").map_err(err)?;
        for file in &sync.files {
            let Some(content) = &file.content else {
                continue;
            };
            let parsed = playlists::parse(content);
            let path = &file.stamp.path;
            if parsed.is_station_list() {
                // Radio's station lists share the extension; they are not
                // playlists of songs (an earlier row for the file goes).
                let removed = tx
                    .execute(
                        "DELETE FROM playlists WHERE source_id=?1 AND path=?2",
                        params![file.source_id, path.to_string_lossy()],
                    )
                    .map_err(err)?;
                changed += removed as u64;
                continue;
            }
            let resolved = index.resolve(
                path,
                &root_of(&file.source_id),
                parsed.entries.iter().map(|e| e.location.as_str()),
            );
            let locations = parsed
                .entries
                .iter()
                .map(|e| e.location.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            upsert
                .execute(params![
                    file.source_id,
                    path.to_string_lossy(),
                    playlists::display_name(path, &parsed),
                    file.stamp.size,
                    file.stamp.mtime,
                    locations,
                    playlists::encode_ids(&resolved.track_ids),
                    resolved.unresolved
                ])
                .map_err(err)?;
            touched.insert((file.source_id.clone(), path.to_string_lossy().into_owned()));
            changed += 1;
        }
        if recheck {
            let rows = {
                let mut q = tx
                    .prepare_cached(
                        "SELECT id,source_id,path,locations,track_ids,unresolved FROM playlists",
                    )
                    .map_err(err)?;
                let rows = q
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, Vec<u8>>(4)?,
                            r.get::<_, u32>(5)?,
                        ))
                    })
                    .map_err(err)?;
                rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?
            };
            let mut update = tx
                .prepare_cached("UPDATE playlists SET track_ids=?2,unresolved=?3 WHERE id=?1")
                .map_err(err)?;
            for (id, source, path, locations, ids, unresolved) in rows {
                if touched.contains(&(source.clone(), path.clone())) {
                    continue;
                }
                let resolved = index.resolve(
                    Path::new(&path),
                    &root_of(&source),
                    locations.lines().filter(|l| !l.is_empty()),
                );
                let encoded = playlists::encode_ids(&resolved.track_ids);
                if encoded != ids || resolved.unresolved != unresolved {
                    update
                        .execute(params![id, encoded, resolved.unresolved])
                        .map_err(err)?;
                    changed += 1;
                }
            }
        }
    }
    {
        let mut existing = tx
            .prepare_cached("SELECT id,path FROM playlists WHERE source_id=?1")
            .map_err(err)?;
        let mut delete = tx
            .prepare_cached("DELETE FROM playlists WHERE id=?1")
            .map_err(err)?;
        for source in &sync.complete_sources {
            let present: std::collections::HashSet<String> = sync
                .files
                .iter()
                .filter(|f| &f.source_id == source)
                .map(|f| f.stamp.path.to_string_lossy().into_owned())
                .collect();
            let rows = existing
                .query_map([source], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(err)?;
            for (id, path) in rows {
                if !present.contains(&path) {
                    delete.execute([id]).map_err(err)?;
                    changed += 1;
                }
            }
        }
    }
    tx.commit().map_err(err)?;
    Ok(changed)
}
fn list_playlists(c: &Connection) -> Result<Vec<Playlist>, String> {
    let mut q = c
        .prepare_cached("SELECT p.id,p.source_id,p.path,p.name,p.track_ids,p.unresolved,s.online FROM playlists p JOIN sources s ON s.id=p.source_id ORDER BY p.name COLLATE NOCASE,p.path,p.id")
        .map_err(err)?;
    let rows = q
        .query_map([], |r| {
            Ok(Playlist {
                id: r.get(0)?,
                source_id: r.get(1)?,
                path: PathBuf::from(r.get::<_, String>(2)?),
                name: r.get(3)?,
                track_ids: playlists::decode_ids(&r.get::<_, Vec<u8>>(4)?),
                unresolved: r.get(5)?,
                online: r.get(6)?,
            })
        })
        .map_err(err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)
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
                            let _ = reply.try_send(catalog(&mut c));
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
                        DbCommand::PlaylistProbe(source, stamps, reply) => {
                            let r = playlist_probe(&c, &source, &stamps);
                            let _ = reply.try_send(r.clone());
                            r.map(|_| ())
                        }
                        DbCommand::PlaylistSync(sync, reply) => {
                            let r = sync_playlists(&mut c, sync);
                            let _ = reply.try_send(r.clone());
                            r.map(|_| ())
                        }
                        DbCommand::Playlists(reply) => {
                            let _ = reply.try_send(list_playlists(&c));
                            Ok(())
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
    /// Every stored playlist, sorted by name, with its source's online flag.
    pub fn playlists(&self) -> Result<Receiver<Result<Vec<Playlist>, String>>, String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .try_send(DbCommand::Playlists(tx))
            .map_err(|e| e.to_string())?;
        Ok(rx)
    }
    fn playlist_probe(
        &self,
        source: String,
        stamps: Vec<PlaylistStamp>,
    ) -> Result<Vec<bool>, String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .send(DbCommand::PlaylistProbe(source, stamps, tx))
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(15))
            .map_err(|e| e.to_string())?
    }
    fn sync_playlists(&self, sync: PlaylistSync) -> Result<u64, String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .send(DbCommand::PlaylistSync(sync, tx))
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(30))
            .map_err(|e| e.to_string())?
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
    mountinfo
        .lines()
        .filter_map(|line| {
            let (mount, _) = line.split_once(" - ")?;
            let fields = mount.split_whitespace().collect::<Vec<_>>();
            let id = fields.first()?.parse::<u64>().ok()?;
            let mount_path = fields
                .get(4)?
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\012", "\n")
                .replace("\\134", "\\");
            let mount_path = Path::new(&mount_path);
            path.starts_with(mount_path)
                .then_some((mount_path.components().count(), id))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, id)| id)
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
pub struct Scanner {
    tx: SyncSender<(Vec<Source>, bool)>,
    pub results: Receiver<Result<ScanMetrics, String>>,
    cancel: Arc<AtomicBool>,
    prepared: Arc<AtomicBool>,
    pending: Arc<AtomicUsize>,
}
impl Scanner {
    pub fn spawn(db: Database, log: Observer) -> Result<Self, String> {
        let (tx, rx) = sync_channel::<(Vec<Source>, bool)>(1);
        let (rt, rr) = sync_channel(2);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let prepared = Arc::new(AtomicBool::new(false));
        let pending = Arc::new(AtomicUsize::new(0));
        let running = pending.clone();
        thread::Builder::new()
            .name("scanner".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    log.heartbeat("scanner", 30);
                    let (sources, reuse) = match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(request) => request,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    };
                    let result = scan_sources_with_identity_and_reuse(
                        &db,
                        &sources,
                        &log,
                        &stop,
                        source_identity_is_current,
                        reuse,
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
    /// Incremental scan: unchanged files keep their indexed metadata.
    pub fn scan(&self, s: Vec<Source>) -> Result<(), String> {
        self.submit(s, true)
    }
    /// Rebuild every supported file, preserving rows until replacement publication.
    pub fn rebuild(&self, s: Vec<Source>) -> Result<(), String> {
        self.submit(s, false)
    }
    fn submit(&self, sources: Vec<Source>, reuse: bool) -> Result<(), String> {
        if self.prepared.load(Ordering::Acquire) {
            return Err("sleep_operation_in_progress".into());
        }
        self.pending.fetch_add(1, Ordering::AcqRel);
        if self.prepared.load(Ordering::Acquire) {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            return Err("sleep_operation_in_progress".into());
        }
        if self.tx.try_send((sources, reuse)).is_err() {
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
fn read_playlist(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(playlists::MAX_PLAYLIST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > playlists::MAX_PLAYLIST_BYTES {
        return Err(std::io::Error::other("playlist too large"));
    }
    Ok(bytes)
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
        for (file, reuse) in files.into_iter().zip(reused) {
            if self.stop.load(Ordering::Relaxed) {
                return Err("scan cancelled".into());
            }
            if reuse {
                self.stats.reused += 1;
                continue;
            }
            let path = self.pinned_root.join(
                file.path
                    .strip_prefix(&self.source.root)
                    .map_err(|e| e.to_string())?,
            );
            if let Some(track) = self.decode(&path, &file)? {
                changes.push(track);
            }
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
    fn decode(&mut self, path: &Path, file: &FileStamp) -> Result<Option<Track>, String> {
        let logical_path = &file.path;
        let size = file.size;
        let mtime = file.mtime;
        let started = Instant::now();
        let parsed = MediaFile::open(path, Cancel::new()?);
        self.stats.metadata_us += started.elapsed().as_micros() as u64;
        let track = match parsed {
            Ok(mut media) => {
                let m = std::mem::take(&mut media.metadata);
                self.stats.ffmpeg_open_us += m.open_us;
                self.stats.ffmpeg_probe_us += m.probe_us;
                self.stats.ffmpeg_info_runs += m.info_runs as u64;
                self.stats.ffmpeg_io_bytes += m.io_bytes;
                if !file.metadata.as_ref().is_some_and(|before| {
                    fs::metadata(path).is_ok_and(|after| same_file(before, &after))
                }) {
                    *self.complete = false;
                    self.stats.failures += 1;
                    return Ok(None);
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
                }
            }
            Err(e) => {
                *self.complete = false;
                self.stats.failures += 1;
                self.log.add("library_scan_errors", 1.);
                self.log.emit(
                    Level::Warn,
                    "scanner",
                    "file_failed",
                    &e,
                    Some(self.id),
                    json!({"path":logical_path,"recovery":"retain_source_rows"}),
                );
                return Ok(None);
            }
        };
        Ok(Some(track))
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
    scan_sources_with_identity_and_reuse(db, sources, log, stop, identity_is_current, true)
}
fn scan_sources_with_identity_and_reuse(
    db: &Database,
    sources: &[Source],
    log: &Observer,
    stop: &AtomicBool,
    identity_is_current: impl Fn(&Source) -> bool,
    reuse: bool,
) -> Result<ScanMetrics, String> {
    let mut workload = reborn_platform::workload::CpuWorkloadLease::acquire_workload_hint(
        reborn_platform::workload::Class::LibraryScan,
    );
    let start = Instant::now();
    let id = log.correlation();
    let mut stats = ScanMetrics {
        complete: true,
        ..Default::default()
    };
    let mut playlist_sync = PlaylistSync::default();
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
        json!({"sources":sources.len(),"reuse":reuse}),
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
        let seen = db.begin_scan(source.id.clone())?;
        let mut directories = match fs::read_dir(&pinned_root) {
            Ok(entries) => vec![(entries, 0usize)],
            Err(_) => {
                stats.complete = false;
                stats.failures += 1;
                continue;
            }
        };
        let mut batch_items = Vec::with_capacity(64);
        let mut complete = true;
        let mut found_playlists: Vec<(PlaylistStamp, PathBuf)> = Vec::new();
        let mut playlists_complete = true;
        while let Some((entries, depth)) = directories.last_mut() {
            workload.renew();
            log.heartbeat("scanner", 30);
            if stop.load(Ordering::Relaxed) {
                return Err("scan cancelled".into());
            }
            let depth = *depth;
            let Some(entry) = entries.next() else {
                directories.pop();
                continue;
            };
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    complete = false;
                    stats.failures += 1;
                    continue;
                }
            };
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
                match fs::read_dir(&path) {
                    Ok(entries) => directories.push((entries, depth + 1)),
                    Err(_) => {
                        complete = false;
                        stats.failures += 1;
                    }
                }
                continue;
            }
            if file_type.is_file() && playlists::is_playlist(&path) {
                if depth <= playlists::MAX_PLAYLIST_DEPTH {
                    match entry.metadata() {
                        Ok(meta) if meta.len() > playlists::MAX_PLAYLIST_BYTES => {}
                        Ok(meta)
                            if playlist_sync.files.len() + found_playlists.len()
                                < playlists::MAX_PLAYLISTS =>
                        {
                            match (file_mtime(&meta), path.strip_prefix(&pinned_root)) {
                                (Some(mtime), Ok(relative)) => found_playlists.push((
                                    PlaylistStamp {
                                        path: source.root.join(relative),
                                        size: meta.len(),
                                        mtime,
                                    },
                                    path.clone(),
                                )),
                                _ => playlists_complete = false,
                            }
                        }
                        _ => playlists_complete = false,
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
            if stats.discovered > 250000 {
                return Err("scan file limit exceeded".into());
            }
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
                }
                .process(std::mem::take(&mut batch_items))?;
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
                }
                .process(batch_items)?;
            } else {
                complete = false;
                stats.failures += 1;
            }
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
        stats.complete &= complete;
        // Playlists: only files that changed since they were stored are read,
        // through the pinned root like the music itself.
        stats.playlists += found_playlists.len() as u64;
        if !found_playlists.is_empty() && identity_is_current(source) {
            let stamps = found_playlists.iter().map(|(s, _)| s.clone()).collect();
            match db.playlist_probe(source.id.clone(), stamps) {
                Ok(stale) => {
                    for ((stamp, pinned), stale) in found_playlists.into_iter().zip(stale) {
                        let content = if stale {
                            match read_playlist(&pinned) {
                                Ok(bytes) => Some(bytes),
                                Err(_) => {
                                    playlists_complete = false;
                                    continue;
                                }
                            }
                        } else {
                            None
                        };
                        playlist_sync.files.push(PlaylistFile {
                            source_id: source.id.clone(),
                            stamp,
                            content,
                        });
                    }
                }
                Err(_) => playlists_complete = false,
            }
        } else if !found_playlists.is_empty() {
            playlists_complete = false;
        }
        if complete && playlists_complete {
            playlist_sync.complete_sources.push(source.id.clone());
        }
    }
    playlist_sync.library_changed = stats.rescanned != 0 || stats.removed != 0;
    if !playlist_sync.files.is_empty()
        || !playlist_sync.complete_sources.is_empty()
        || playlist_sync.library_changed
    {
        match db.sync_playlists(playlist_sync) {
            Ok(changed) => stats.playlists_changed = changed,
            Err(error) => log.emit(
                Level::Warn,
                "scanner",
                "playlists_failed",
                &error,
                Some(id),
                json!({"recovery":"retain_stored_playlists"}),
            ),
        }
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
        )
        .unwrap();
        assert_eq!(scan.reused, 1);
        let rebuild =
            scan_sources_with_identity_and_reuse(&db, &[source], &log, &stop, |_| true, false)
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

        assert!(!stats.complete);
        assert!(stats.failures > 0);
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
        assert_eq!(metrics.failures, 1);
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
        assert_eq!(result.failures, 1);
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
mod playlist_tests {
    use super::*;

    fn temp(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "reborn-playlist-{label}-{}-{}",
            std::process::id(),
            reborn_observability::wall_ms()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn tone(to: &Path) {
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac"),
            to,
        )
        .unwrap();
    }

    fn playlists(db: &Database) -> Vec<Playlist> {
        db.playlists()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
    }

    fn id_of(db: &Database, path: &Path) -> i64 {
        db.list(Filter {
            limit: 100,
            ..Default::default()
        })
        .unwrap()
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .into_iter()
        .find(|t| t.path == path)
        .unwrap_or_else(|| panic!("{} not in library", path.display()))
        .id
    }

    #[test]
    fn radio_station_lists_on_the_media_are_not_playlists() {
        let root = temp("stations");
        let media = root.join("media");
        let song = media.join("Music/Artist/Album/01 Track.flac");
        tone(&song);
        fs::create_dir_all(media.join("Radio")).unwrap();
        fs::write(
            media.join("Radio/Jazz.m3u"),
            "#EXTM3U\n#EXTINF:-1,Jazz FM\nhttp://jazz.example/live\n",
        )
        .unwrap();
        fs::create_dir_all(media.join("Playlists")).unwrap();
        fs::write(
            media.join("Playlists/Mix.m3u8"),
            "../Music/Artist/Album/01 Track.flac\nhttp://jazz.example/live\n",
        )
        .unwrap();
        let source = Source {
            id: "internal".into(),
            kind: MediaSource::Internal,
            root: media.clone(),
            online: true,
            mount: "fixture".into(),
            mount_id: None,
        };
        let log = Observer::new(&root.join("logs")).unwrap();
        let db = Database::spawn(root.join("library.db"), log.clone()).unwrap();
        db.sources(vec![source.clone()]).unwrap();
        let scanned = scan_sources_with_identity(
            &db,
            std::slice::from_ref(&source),
            &log,
            &AtomicBool::new(false),
            |_| true,
        )
        .unwrap();
        assert!(scanned.complete);
        let stored = playlists(&db);
        // Only the song playlist; its stream line is neither a song nor missing.
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].name, "Mix");
        assert_eq!(stored[0].track_ids, [id_of(&db, &song)]);
        assert_eq!(stored[0].unresolved, 0);
    }

    #[test]
    fn scan_imports_resolves_refreshes_and_forgets_playlists_without_writing_them() {
        let root = temp("scan");
        let media = root.join("media");
        let one = media.join("Music/Artist/Album/01 Track.flac");
        let two = media.join("Music/Artist/Album/02 Other.flac");
        tone(&one);
        tone(&two);
        let list = media.join("Playlists/Mix.m3u8");
        fs::create_dir_all(list.parent().unwrap()).unwrap();
        let text = "\u{feff}#EXTM3U\r\n#EXTINF:3,Artist - Other\r\n../Music/Artist/Album/02 Other.flac\r\n#EXTINF:3,Artist - Track\r\n..\\Music\\artist\\ALBUM\\01 TRACK.flac\r\n../Music/Artist/Album/03 Later.flac\r\n";
        fs::write(&list, text).unwrap();
        // Too deep to be looked for.
        let deep = media.join("a/b/c/d/e/f/g/h/i/Deep.m3u");
        fs::create_dir_all(deep.parent().unwrap()).unwrap();
        fs::write(&deep, "../x.flac\n").unwrap();

        let source = Source {
            id: "internal".into(),
            kind: MediaSource::Internal,
            root: media.clone(),
            online: true,
            mount: "fixture".into(),
            mount_id: None,
        };
        let log = Observer::new(&root.join("logs")).unwrap();
        let db_path = root.join("library.db");
        let db = Database::spawn(db_path.clone(), log.clone()).unwrap();
        db.sources(vec![source.clone()]).unwrap();
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

        let first = scan(&db);
        assert!(first.complete);
        assert_eq!(first.playlists, 1);
        assert_eq!(first.playlists_changed, 1);
        let (id1, id2) = (id_of(&db, &one), id_of(&db, &two));
        let stored = playlists(&db);
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].name, "Mix");
        assert_eq!(stored[0].path, list);
        assert_eq!(stored[0].track_ids, [id2, id1]);
        assert_eq!(stored[0].unresolved, 1);
        assert!(stored[0].online);

        // Nothing changed: nothing is re-read or rewritten.
        db.execute_test_sql("CREATE TRIGGER forbid_playlist_write BEFORE UPDATE ON playlists BEGIN SELECT RAISE(FAIL,'playlist rewritten'); END;".into()).unwrap();
        let again = scan(&db);
        assert_eq!(again.playlists_changed, 0);
        db.execute_test_sql("DROP TRIGGER forbid_playlist_write".into())
            .unwrap();

        // The missing song arrives: stored entries are resolved again.
        let three = media.join("Music/Artist/Album/03 Later.flac");
        tone(&three);
        let added = scan(&db);
        assert_eq!(added.playlists_changed, 1);
        let id3 = id_of(&db, &three);
        let stored = playlists(&db);
        assert_eq!(stored[0].track_ids, [id2, id1, id3]);
        assert_eq!(stored[0].unresolved, 0);

        // Survives a restart (database round trip).
        db.shutdown(Duration::from_secs(5)).unwrap();
        let db = Database::spawn(db_path.clone(), log.clone()).unwrap();
        db.sources(vec![source.clone()]).unwrap();
        assert_eq!(playlists(&db), stored);

        // The file changes: it is read again.
        std::thread::sleep(Duration::from_millis(20));
        fs::write(
            &list,
            "#EXTM3U\n#PLAYLIST:Road Trip\n../Music/Artist/Album/01 Track.flac\n",
        )
        .unwrap();
        let edited = scan(&db);
        assert_eq!(edited.playlists_changed, 1);
        let stored = playlists(&db);
        assert_eq!(stored[0].name, "Road Trip");
        assert_eq!(stored[0].track_ids, [id1]);
        let written = fs::read(&list).unwrap();

        // An SD card going offline keeps the playlist, marked offline.
        db.sources(vec![Source {
            online: false,
            ..source.clone()
        }])
        .unwrap();
        assert!(!playlists(&db)[0].online);
        db.sources(vec![source.clone()]).unwrap();

        // The file is removed: the playlist goes with it.
        fs::remove_file(&list).unwrap();
        let removed = scan(&db);
        assert_eq!(removed.playlists, 0);
        assert_eq!(removed.playlists_changed, 1);
        assert!(playlists(&db).is_empty());
        assert_eq!(
            written,
            b"#EXTM3U\n#PLAYLIST:Road Trip\n../Music/Artist/Album/01 Track.flac\n"
        );
        db.stop();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sync_keeps_playlists_of_incomplete_sources_and_counts_unresolved() {
        let p = temp("sync").join("library.db");
        let mut c = open(&p).unwrap();
        c.execute("INSERT INTO sources VALUES('sd','/media/sd',1)", [])
            .unwrap();
        batch(
            &mut c,
            vec![Track {
                source_id: "sd".into(),
                path: "/media/sd/Music/A/B/01.flac".into(),
                ..Default::default()
            }],
            1,
        )
        .unwrap();
        let file = |content: Option<&str>| PlaylistFile {
            source_id: "sd".into(),
            stamp: PlaylistStamp {
                path: "/media/sd/Playlists/One.m3u8".into(),
                size: 10,
                mtime: 20,
            },
            content: content.map(|c| c.as_bytes().to_vec()),
        };
        let changed = sync_playlists(
            &mut c,
            PlaylistSync {
                files: vec![file(Some("../Music/A/B/01.flac\nE:\\Gone\\x.flac\n"))],
                complete_sources: vec!["sd".into()],
                library_changed: false,
            },
        )
        .unwrap();
        assert_eq!(changed, 1);
        let stored = list_playlists(&c).unwrap();
        assert_eq!(stored[0].track_ids.len(), 1);
        assert_eq!(stored[0].unresolved, 1);
        assert_eq!(
            playlist_probe(&c, "sd", &[file(None).stamp]).unwrap(),
            [false]
        );
        // An incomplete traversal that did not see the file keeps it.
        sync_playlists(&mut c, PlaylistSync::default()).unwrap();
        assert_eq!(list_playlists(&c).unwrap().len(), 1);
        // A complete one forgets it.
        sync_playlists(
            &mut c,
            PlaylistSync {
                complete_sources: vec!["sd".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(list_playlists(&c).unwrap().is_empty());
        drop(c);
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn sync_ignores_tracks_of_an_offline_duplicate_source() {
        let p = temp("sync-offline").join("library.db");
        let mut c = open(&p).unwrap();
        // The same card once recorded under an older id, now offline.
        c.execute("INSERT INTO sources VALUES('old','/media/sd',0)", [])
            .unwrap();
        c.execute("INSERT INTO sources VALUES('sd','/media/sd',1)", [])
            .unwrap();
        for source in ["old", "sd"] {
            batch(
                &mut c,
                vec![Track {
                    source_id: source.into(),
                    path: "/media/sd/Music/A/B/01.flac".into(),
                    ..Default::default()
                }],
                1,
            )
            .unwrap();
        }
        sync_playlists(
            &mut c,
            PlaylistSync {
                files: vec![PlaylistFile {
                    source_id: "sd".into(),
                    stamp: PlaylistStamp {
                        path: "/media/sd/Playlists/One.m3u8".into(),
                        size: 10,
                        mtime: 20,
                    },
                    content: Some(b"../Music/A/B/01.flac\n".to_vec()),
                }],
                complete_sources: vec!["sd".into()],
                library_changed: false,
            },
        )
        .unwrap();
        let stored = list_playlists(&c).unwrap();
        assert_eq!(stored[0].track_ids.len(), 1);
        assert_eq!(stored[0].unresolved, 0);
        // A stored miss heals on a later sync even though neither the file
        // nor the scan reported a change.
        c.execute("UPDATE playlists SET track_ids=x'',unresolved=1", [])
            .unwrap();
        sync_playlists(&mut c, PlaylistSync::default()).unwrap();
        let stored = list_playlists(&c).unwrap();
        assert_eq!(stored[0].track_ids.len(), 1);
        assert_eq!(stored[0].unresolved, 0);
        drop(c);
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }
}
