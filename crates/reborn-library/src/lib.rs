#![forbid(unsafe_code)]
use reborn_core::{MediaSource, Source, Track};
use reborn_media::{Cancel, Decoder};
use reborn_observability::{HealthState, Level, Observer};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender},
        Arc,
    },
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};
pub const SCHEMA_VERSION: i64 = 1;
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScanMetrics {
    pub discovered: u64,
    pub reused: u64,
    pub rescanned: u64,
    pub failures: u64,
    pub elapsed_ms: u64,
    pub tracks_per_sec: f64,
    pub complete: bool,
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
    Existing(String, SyncSender<Result<BTreeMap<PathBuf, Track>, String>>),
    Batch(Vec<Track>, i64, SyncSender<Result<(), String>>),
    Finish(Source, i64, bool, SyncSender<Result<bool, String>>),
    Test(SyncSender<Result<serde_json::Value, String>>),
    #[cfg(test)]
    TestSql(String, SyncSender<Result<(), String>>),
    Stop,
}
#[derive(Clone)]
pub struct Database {
    tx: SyncSender<DbCommand>,
}
fn err(e: rusqlite::Error) -> String {
    format!("SQLite: {e}")
}
fn open(path: &Path) -> Result<Connection, String> {
    let c = Connection::open(path).map_err(err)?;
    c.busy_timeout(Duration::from_secs(2)).map_err(err)?;
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")
        .map_err(err)?;
    let version: i64 = c
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(err)?;
    if version > SCHEMA_VERSION {
        return Err("database schema is newer than Reborn".into());
    }
    if version == 0 {
        c.execute_batch("BEGIN IMMEDIATE;
 CREATE TABLE sources(id TEXT PRIMARY KEY, root TEXT NOT NULL, online INTEGER NOT NULL);
 CREATE TABLE tracks(id INTEGER PRIMARY KEY, source_id TEXT NOT NULL REFERENCES sources(id),path TEXT NOT NULL,filename TEXT NOT NULL,size INTEGER NOT NULL,mtime INTEGER NOT NULL,title TEXT NOT NULL,artist TEXT NOT NULL,album TEXT NOT NULL,album_artist TEXT NOT NULL,track INTEGER NOT NULL,disc INTEGER NOT NULL,duration_ms INTEGER NOT NULL,codec TEXT NOT NULL,sample_rate INTEGER NOT NULL,channels INTEGER NOT NULL,bitrate INTEGER NOT NULL,artwork INTEGER NOT NULL,seen INTEGER NOT NULL,deleted INTEGER NOT NULL DEFAULT 0,UNIQUE(source_id,path));
 CREATE INDEX tracks_album ON tracks(album_artist,album,disc,track);CREATE INDEX tracks_artist ON tracks(artist);
 PRAGMA user_version=1;COMMIT;").map_err(err)?;
    }
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
    })
}
const SELECT:&str="SELECT t.id,t.source_id,t.path,t.filename,t.size,t.mtime,t.title,t.artist,t.album,t.album_artist,t.track,t.disc,t.duration_ms,t.codec,t.sample_rate,t.channels,t.bitrate,t.artwork,s.online FROM tracks t JOIN sources s ON s.id=t.source_id";
fn list(c: &Connection, f: &Filter) -> Result<Vec<Track>, String> {
    let sql=format!("{SELECT} WHERE t.deleted=0 AND (?1 IS NULL OR t.artist=?1) AND (?2 IS NULL OR t.album=?2) AND (?3 IS NULL OR substr(t.path,1,length(?3))=?3) ORDER BY t.artist,t.album,t.disc,t.track,t.title,t.path LIMIT ?4 OFFSET ?5");
    let mut q = c.prepare(&sql).map_err(err)?;
    let rows = q
        .query_map(
            params![
                f.artist,
                f.album,
                f.folder,
                f.limit.clamp(1, 20000),
                f.offset
            ],
            from_row,
        )
        .map_err(err)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)
}
fn batch(c: &mut Connection, tracks: Vec<Track>, seen: i64) -> Result<(), String> {
    let tx = c.transaction().map_err(err)?;
    {
        let mut q=tx.prepare_cached("INSERT INTO tracks(source_id,path,filename,size,mtime,title,artist,album,album_artist,track,disc,duration_ms,codec,sample_rate,channels,bitrate,artwork,seen) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18) ON CONFLICT(source_id,path) DO UPDATE SET filename=excluded.filename,size=excluded.size,mtime=excluded.mtime,title=excluded.title,artist=excluded.artist,album=excluded.album,album_artist=excluded.album_artist,track=excluded.track,disc=excluded.disc,duration_ms=excluded.duration_ms,codec=excluded.codec,sample_rate=excluded.sample_rate,channels=excluded.channels,bitrate=excluded.bitrate,artwork=excluded.artwork,seen=excluded.seen,deleted=0").map_err(err)?;
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
                seen
            ])
            .map_err(err)?;
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
impl Database {
    pub fn spawn(path: PathBuf, log: Observer) -> Result<Self, String> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let mut recovered = None;
        let mut c = match open(&path) {
            Ok(connection) => connection,
            Err(error) if error.contains("schema is newer") => return Err(error),
            Err(error) if path.exists() => {
                let stamp = reborn_observability::wall_ms();
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("library.db");
                let quarantined = path.with_file_name(format!("{name}.corrupt-{stamp}"));
                fs::rename(&path, &quarantined)
                    .map_err(|rename| format!("{error}; database quarantine failed: {rename}"))?;
                for suffix in ["-wal", "-shm"] {
                    let sidecar = PathBuf::from(format!("{}{}", path.display(), suffix));
                    if sidecar.exists() {
                        let sidecar_quarantine =
                            PathBuf::from(format!("{}{}", quarantined.display(), suffix));
                        fs::rename(&sidecar, sidecar_quarantine).map_err(|rename| {
                            format!("{error}; database sidecar quarantine failed: {rename}")
                        })?;
                    }
                }
                recovered = Some((error, quarantined));
                open(&path)?
            }
            Err(error) => return Err(error),
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
        thread::Builder::new().name("database".into()).spawn(move||{
 log.health_set("database",HealthState::Ok,true,"schema ready");loop{log.heartbeat("database",15);let msg=match rx.recv_timeout(Duration::from_secs(1)){Ok(m)=>m,Err(RecvTimeoutError::Timeout)=>continue,Err(_)=>break};let now=Instant::now();let result:Result<(),String>=match msg{
 DbCommand::Stop=>break,
 DbCommand::Sources(sources)=>(||{let tx=c.transaction().map_err(err)?;tx.execute("UPDATE sources SET online=0",[]).map_err(err)?;for s in sources{tx.execute("INSERT INTO sources(id,root,online) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET root=excluded.root,online=excluded.online",params![s.id,s.root.to_string_lossy(),s.online]).map_err(err)?;}tx.commit().map_err(err)})(),
 DbCommand::List(f,reply)=>{let _=reply.try_send(list(&c,&f));Ok(())},
 DbCommand::Existing(id,reply)=>{let value=(||{let mut q=c.prepare(&format!("{SELECT} WHERE t.source_id=?1 LIMIT 250000")).map_err(err)?;let rows=q.query_map([id],from_row).map_err(err)?;let tracks=rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?;Ok(tracks.into_iter().map(|t|(t.path.clone(),t)).collect())})();let _=reply.try_send(value);Ok(())},
 DbCommand::Batch(v,s,reply)=>{let r=batch(&mut c,v,s);let _=reply.try_send(r.clone());r},
 DbCommand::Finish(source,seen,complete,reply)=>{let r=if complete&&source_identity_is_current(&source){c.execute("UPDATE tracks SET deleted=1 WHERE source_id=?1 AND seen<>?2",params![source.id,seen]).map(|_|true).map_err(err)}else{Ok(false)};let _=reply.try_send(r.clone());r.map(|_|())},
 DbCommand::Test(reply)=>{let r=validate(&c);let _=reply.try_send(r);Ok(())},
 #[cfg(test)] DbCommand::TestSql(sql,reply)=>{let r=c.execute_batch(&sql).map_err(err);let _=reply.try_send(r.clone());r}};
 log.gauge("database_query_latency_ms",now.elapsed().as_secs_f64()*1000.);if let Err(e)=result{log.health_set("database",HealthState::Failed,true,&e);log.emit(Level::Error,"database","operation_failed",&e,None,json!({"recovery_attempted":false}));}
 if let Ok(n)=c.query_row("SELECT count(*) FROM tracks WHERE deleted=0",[],|r|r.get::<_,i64>(0)){log.gauge("library_tracks",n as f64);}
 }}).map_err(|e|e.to_string())?;
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
    fn existing(&self, id: String) -> Result<BTreeMap<PathBuf, Track>, String> {
        let (tx, rx) = sync_channel(1);
        self.tx
            .send(DbCommand::Existing(id, tx))
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|e| e.to_string())?
    }
    pub fn stop(&self) {
        let _ = self.tx.try_send(DbCommand::Stop);
    }
}
pub fn supported(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        matches!(
            e.to_string_lossy().to_ascii_lowercase().as_str(),
            "flac" | "mp3" | "aac" | "m4a" | "ogg" | "opus" | "wav" | "aiff" | "aif" | "ape" | "wv"
        )
    })
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
fn source_identity_matches_at(source: &Source, mountinfo: &str) -> bool {
    match &source.kind {
        MediaSource::Internal => true,
        MediaSource::SdCard(uuid) => {
            source.id == format!("uuid:{uuid}")
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
    tx: SyncSender<Vec<Source>>,
    pub results: Receiver<Result<ScanMetrics, String>>,
    cancel: Arc<AtomicBool>,
}
impl Scanner {
    pub fn spawn(db: Database, log: Observer) -> Result<Self, String> {
        let (tx, rx) = sync_channel::<Vec<Source>>(1);
        let (rt, rr) = sync_channel(2);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        thread::Builder::new()
            .name("scanner".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    log.heartbeat("scanner", 30);
                    let sources = match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(s) => s,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    };
                    let result = scan_sources(&db, &sources, &log, &stop);
                    let _ = rt.try_send(result);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            tx,
            results: rr,
            cancel,
        })
    }
    pub fn scan(&self, s: Vec<Source>) -> Result<(), String> {
        self.tx.try_send(s).map_err(|_| "scanner busy".into())
    }
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed)
    }
}
fn scan_sources(
    db: &Database,
    sources: &[Source],
    log: &Observer,
    stop: &AtomicBool,
) -> Result<ScanMetrics, String> {
    scan_sources_with_identity(db, sources, log, stop, source_identity_is_current)
}
fn scan_sources_with_identity(
    db: &Database,
    sources: &[Source],
    log: &Observer,
    stop: &AtomicBool,
    identity_is_current: impl Fn(&Source) -> bool,
) -> Result<ScanMetrics, String> {
    let start = Instant::now();
    let id = log.correlation();
    let seen = reborn_observability::wall_ms() as i64;
    let mut stats = ScanMetrics {
        complete: true,
        ..Default::default()
    };
    log.emit(
        Level::Info,
        "scanner",
        "scan_started",
        "Incremental library scan",
        Some(id),
        json!({"sources":sources.len()}),
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
        let existing = db.existing(source.id.clone())?;
        let mut directories = vec![(pinned_root.clone(), 0)];
        let mut batch_items = vec![];
        let mut complete = true;
        'source_traversal: while let Some((dir, depth)) = directories.pop() {
            log.heartbeat("scanner", 30);
            if stop.load(Ordering::Relaxed) {
                return Err("scan cancelled".into());
            }
            if !identity_is_current(source) {
                complete = false;
                stats.failures += 1;
                break;
            }
            if depth > 32 {
                complete = false;
                stats.failures += 1;
                continue;
            }
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => {
                    complete = false;
                    stats.failures += 1;
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => {
                        complete = false;
                        stats.failures += 1;
                        continue;
                    }
                };
                let path = entry.path();
                let Ok(relative_path) = path.strip_prefix(&pinned_root) else {
                    complete = false;
                    stats.failures += 1;
                    continue;
                };
                let logical_path = source.root.join(relative_path);
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => {
                        complete = false;
                        stats.failures += 1;
                        continue;
                    }
                };
                match entry.file_type() {
                    Ok(file_type) if file_type.is_symlink() => continue,
                    Ok(_) => {}
                    Err(_) => {
                        complete = false;
                        stats.failures += 1;
                        continue;
                    }
                }
                if meta.is_dir() {
                    directories.push((path, depth + 1));
                    continue;
                }
                if !meta.is_file() || !supported(&logical_path) {
                    continue;
                }
                stats.discovered += 1;
                if stats.discovered > 250000 {
                    return Err("scan file limit exceeded".into());
                }
                let Some(mtime) = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|t| t.as_nanos().min(i64::MAX as u128) as i64)
                else {
                    complete = false;
                    stats.failures += 1;
                    continue;
                };
                let track = if let Some(t) = existing
                    .get(&logical_path)
                    .filter(|t| unchanged(t, meta.len(), mtime))
                {
                    stats.reused += 1;
                    t.clone()
                } else {
                    match Decoder::open(&path, 48000, Cancel::new()?) {
                        Ok(d) => {
                            let m = d.metadata.clone();
                            stats.rescanned += 1;
                            let filename = logical_path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned();
                            Track {
                                id: 0,
                                source_id: source.id.clone(),
                                path: logical_path.clone(),
                                filename: filename.clone(),
                                size: meta.len(),
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
                            }
                        }
                        Err(e) => {
                            complete = false;
                            stats.failures += 1;
                            log.add("library_scan_errors", 1.);
                            log.emit(
                                Level::Warn,
                                "scanner",
                                "file_failed",
                                &e,
                                Some(id),
                                json!({"path":logical_path,"recovery":"retain_source_rows"}),
                            );
                            continue;
                        }
                    }
                };
                batch_items.push(track);
                if batch_items.len() == 64 {
                    if !identity_is_current(source) {
                        complete = false;
                        stats.failures += 1;
                        batch_items.clear();
                        break 'source_traversal;
                    }
                    let (reply, result) = sync_channel(1);
                    db.tx
                        .send(DbCommand::Batch(
                            std::mem::take(&mut batch_items),
                            seen,
                            reply,
                        ))
                        .map_err(|e| e.to_string())?;
                    result
                        .recv_timeout(Duration::from_secs(15))
                        .map_err(|e| e.to_string())??;
                }
            }
        }
        if !batch_items.is_empty() {
            if identity_is_current(source) {
                let (reply, result) = sync_channel(1);
                db.tx
                    .send(DbCommand::Batch(batch_items, seen, reply))
                    .map_err(|e| e.to_string())?;
                result
                    .recv_timeout(Duration::from_secs(15))
                    .map_err(|e| e.to_string())??;
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
        let pruned = result
            .recv_timeout(Duration::from_secs(15))
            .map_err(|e| e.to_string())??;
        if complete && !pruned {
            complete = false;
            stats.failures += 1;
        }
        stats.complete &= complete;
    }
    // A FIFO database barrier makes scan completion mean writes have completed.
    db.test()?
        .recv_timeout(Duration::from_secs(15))
        .map_err(|e| e.to_string())??;
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
        let p = std::env::temp_dir().join(format!("reborn-future-{}.db", std::process::id()));
        let c = Connection::open(&p).unwrap();
        c.pragma_update(None, "user_version", 999).unwrap();
        drop(c);
        assert!(open(&p).is_err());
        fs::remove_file(p).unwrap();
    }
    #[test]
    fn formats() {
        assert!(supported(Path::new("a.FLAC")));
        assert!(supported(Path::new("a.AIFF")));
        assert!(supported(Path::new("a.ape")));
        assert!(supported(Path::new("a.wv")));
        assert!(!supported(Path::new("a.txt")));
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
        assert!(root
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("library.db.corrupt-")));
        db.stop();
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
                1,
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
        assert!(tracks(&db).is_empty());
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
