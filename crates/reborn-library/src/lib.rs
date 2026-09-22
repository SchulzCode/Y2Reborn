#![forbid(unsafe_code)]
use reborn_core::{Source, Track};
use reborn_media::{Cancel, Decoder};
use reborn_observability::{HealthState, Level, Observer};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
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
    Finish(String, i64, bool, SyncSender<Result<(), String>>),
    Test(SyncSender<Result<serde_json::Value, String>>),
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
 DbCommand::Finish(id,seen,complete,reply)=>{let r=if complete{c.execute("UPDATE tracks SET deleted=1 WHERE source_id=?1 AND seen<>?2",params![id,seen]).map(|_|()).map_err(err)}else{Ok(())};let _=reply.try_send(r.clone());r},
 DbCommand::Test(reply)=>{let r=validate(&c);let _=reply.try_send(r);Ok(())}};
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
        let existing = db.existing(source.id.clone())?;
        let mut directories = vec![(source.root.clone(), 0)];
        let mut batch_items = vec![];
        let mut complete = true;
        while let Some((dir, depth)) = directories.pop() {
            log.heartbeat("scanner", 30);
            if stop.load(Ordering::Relaxed) {
                return Err("scan cancelled".into());
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
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => {
                        complete = false;
                        stats.failures += 1;
                        continue;
                    }
                };
                if entry.file_type().map(|t| t.is_symlink()).unwrap_or(true) {
                    continue;
                }
                if meta.is_dir() {
                    directories.push((path, depth + 1));
                    continue;
                }
                if !meta.is_file() || !supported(&path) {
                    continue;
                }
                stats.discovered += 1;
                if stats.discovered > 250000 {
                    return Err("scan file limit exceeded".into());
                }
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|t| t.as_nanos().min(i64::MAX as u128) as i64)
                    .unwrap_or(0);
                let track = if let Some(t) = existing
                    .get(&path)
                    .filter(|t| unchanged(t, meta.len(), mtime))
                {
                    stats.reused += 1;
                    t.clone()
                } else {
                    match Decoder::open(&path, 48000, Cancel::new()?) {
                        Ok(d) => {
                            let m = d.metadata.clone();
                            stats.rescanned += 1;
                            let filename = path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned();
                            Track {
                                id: 0,
                                source_id: source.id.clone(),
                                path: path.clone(),
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
                            stats.failures += 1;
                            log.add("library_scan_errors", 1.);
                            log.emit(
                                Level::Warn,
                                "scanner",
                                "file_failed",
                                &e,
                                Some(id),
                                json!({"path":path,"recovery":"skip_file"}),
                            );
                            continue;
                        }
                    }
                };
                batch_items.push(track);
                if batch_items.len() == 64 {
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
            let (reply, result) = sync_channel(1);
            db.tx
                .send(DbCommand::Batch(batch_items, seen, reply))
                .map_err(|e| e.to_string())?;
            result
                .recv_timeout(Duration::from_secs(15))
                .map_err(|e| e.to_string())??;
        }
        // Never mark files deleted after incomplete traversal or physical removal.
        complete &= source.root.is_dir();
        let (reply, result) = sync_channel(1);
        db.tx
            .send(DbCommand::Finish(source.id.clone(), seen, complete, reply))
            .map_err(|e| e.to_string())?;
        result
            .recv_timeout(Duration::from_secs(15))
            .map_err(|e| e.to_string())??;
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
