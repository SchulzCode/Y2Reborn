//! Disposable measurements through the same schema, SQL and scanner as Reborn.
//! Caller must supply a newly created, private scratch directory.
use super::*;
use serde_json::Value;
use std::io::Write;

pub fn summary(name: &str, times: &[u64]) -> Value {
    let mut sorted = times.to_vec();
    sorted.sort_unstable();
    let percentile = |p: usize| {
        sorted
            .get((sorted.len() * p).div_ceil(100).saturating_sub(1))
            .map(|v| *v as f64 / 1_000_000.)
    };
    json!({"operation":name,"samples":times.len(),"p50_ms":percentile(50),
        "p95_ms":percentile(95),"p99_ms":percentile(99),"max_ms":percentile(100),
        "elapsed_ms":times.iter().sum::<u64>() as f64/1_000_000.,
        "operations_per_second":if times.iter().sum::<u64>()>0 {Some(times.len() as f64*1e9/times.iter().sum::<u64>() as f64)} else {None}})
}

pub fn timed<T>(
    samples: &mut Vec<u64>,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let start = Instant::now();
    let value = f()?;
    samples.push(start.elapsed().as_nanos().min(u64::MAX as u128) as u64);
    Ok(value)
}

pub fn database(root: &Path, count: usize) -> Result<(Value, Vec<Track>), String> {
    if !(1..=20_000).contains(&count) {
        return Err("dataset must be 1..20000 tracks".into());
    }
    let path = root.join("library.sqlite");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let mut measurements = vec![];
    let mut times = vec![];
    let mut c = timed(&mut times, || open(&path).map_err(open_message))?;
    measurements.push(summary("database_create_open", &times));
    c.execute(
        "INSERT INTO sources(id,root,online) VALUES('benchmark',?1,1)",
        [root.to_string_lossy()],
    )
    .map_err(err)?;
    let make_track = |i: usize| Track {
        source_id: "benchmark".into(),
        path: root.join(format!(
            "media/artist{:04}/album{:04}/{i:05}.wav",
            i / 200,
            i / 10
        )),
        filename: format!("{i:05}.wav"),
        size: 4096,
        mtime: 1,
        title: format!("Track {i:05}"),
        artist: format!("Artist {:04}", i / 200),
        album: format!("Album {:04}", i / 10),
        album_artist: format!("Artist {:04}", i / 200),
        track: (i % 10 + 1) as u32,
        disc: 1,
        duration_ms: 180_000,
        codec: "pcm_s16le".into(),
        sample_rate: 44100,
        channels: 2,
        bitrate: 1411200,
        online: true,
        ..Track::default()
    };
    times.clear();
    let population = Instant::now();
    for offset in (0..count).step_by(64) {
        let tracks = (offset..(offset + 64).min(count)).map(make_track).collect();
        timed(&mut times, || batch(&mut c, tracks, 1))?;
    }
    measurements.push(summary("batch_commit_64", &times));
    measurements.push(summary(
        "initial_population",
        &[population.elapsed().as_nanos() as u64],
    ));
    times.clear();
    for offset in (0..count).step_by(64) {
        let tracks = (offset..(offset + 64).min(count))
            .map(|i| {
                let mut t = make_track(i);
                t.mtime = 2;
                t
            })
            .collect();
        timed(&mut times, || batch(&mut c, tracks, 2))?;
    }
    measurements.push(summary("incremental_upsert_batch_64", &times));
    for (name, filter) in [
        (
            "list_page",
            Filter {
                limit: 64,
                ..Filter::default()
            },
        ),
        (
            "artist_lookup",
            Filter {
                artist: Some("Artist 0000".into()),
                limit: 64,
                ..Filter::default()
            },
        ),
        (
            "album_lookup",
            Filter {
                album: Some("Album 0000".into()),
                limit: 64,
                ..Filter::default()
            },
        ),
        (
            "folder_lookup",
            Filter {
                folder: Some(root.join("media/artist0000").to_string_lossy().into()),
                limit: 64,
                ..Filter::default()
            },
        ),
    ] {
        times.clear();
        for _ in 0..32 {
            timed(&mut times, || list(&c, &filter))?;
        }
        measurements.push(summary(name, &times));
    }
    times.clear();
    for n in 0..32 {
        timed(&mut times, || {
            c.query_row(
                "SELECT title FROM tracks WHERE source_id=?1 AND path=?2",
                params!["benchmark", make_track(n % count).path.to_string_lossy()],
                |r| r.get::<_, String>(0),
            )
            .map_err(err)
        })?;
    }
    measurements.push(summary("indexed_identity_lookup", &times));
    times.clear();
    for _ in 0..32 {
        timed(&mut times, || {
            let mut q = c
                .prepare("SELECT id FROM tracks WHERE deleted=0 AND title LIKE ?1 LIMIT 64")
                .map_err(err)?;
            let rows = q
                .query_map(["%001%"], |r| r.get::<_, i64>(0))
                .map_err(err)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)
        })?;
    }
    measurements.push(summary("search_like_query", &times));
    let wal_size = fs::metadata(sidecar(&path, "-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    times.clear();
    timed(&mut times, || {
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(err)
        .and_then(|busy| {
            if busy == 0 {
                Ok(())
            } else {
                Err("checkpoint busy".into())
            }
        })
    })?;
    measurements.push(summary("wal_checkpoint_truncate", &times));
    drop(c);
    // No global drop_caches or claim of electrically cold storage.
    for name in ["reopen_first_connection", "reopen_warm_connection"] {
        times.clear();
        c = timed(&mut times, || open(&path).map_err(open_message))?;
        measurements.push(summary(name, &times));
        drop(c);
    }
    c = open(&path).map_err(open_message)?;
    times.clear();
    let tracks = timed(&mut times, || {
        list(
            &c,
            &Filter {
                limit: count,
                ..Filter::default()
            },
        )
    })?;
    measurements.push(summary("startup_full_library_load", &times));
    let check = validate(&c)?;
    if check["tracks"].as_u64() != Some(count as u64) {
        return Err("population_count_mismatch".into());
    }
    let report = json!({"schema_version":SCHEMA_VERSION,"sqlite_version":rusqlite::version(),
        "tracks":count,"measurements":measurements,"db_bytes":fs::metadata(&path).map_err(|e|e.to_string())?.len(),
        "wal_bytes_before_checkpoint":wal_size,"cache_state":"OS page cache uncontrolled; reopen is not cold-device evidence",
        "journal_mode":"WAL","synchronous":"NORMAL","batch_tracks":64,"quick_check":check,
        "memory":fs::read_to_string("/proc/self/status").ok()});
    Ok((report, tracks))
}

fn wave() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&100u32.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&44100u32.to_le_bytes());
    bytes.extend_from_slice(&176400u32.to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&64u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 64]);
    bytes
}

pub fn scanner(root: &Path, count: usize) -> Result<Value, String> {
    if !(1..=20_000).contains(&count) {
        return Err("dataset out of range".into());
    }
    let media = root.join("scan-media");
    fs::create_dir(&media).map_err(|e| e.to_string())?;
    let bytes = wave();
    for i in 0..count {
        let path = media.join(format!("{i:05}.wav"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
    }
    let log = Observer::new(&root.join("logs")).map_err(|e| e.to_string())?;
    let db = Database::spawn(root.join("scan.sqlite"), log.clone())?;
    let source = Source {
        id: "benchmark-scan".into(),
        kind: MediaSource::Internal,
        root: media.clone(),
        online: true,
        mount: "scratch fixture".into(),
        mount_id: None,
    };
    db.sources(vec![source.clone()])?;
    let scanner = Scanner::spawn(db.clone(), log)?;
    let mut results = vec![];
    for name in ["initial_scan", "incremental_scan"] {
        scanner.scan(vec![source.clone()])?;
        let result = scanner
            .results
            .recv_timeout(Duration::from_secs(900))
            .map_err(|e| e.to_string())??;
        if !result.complete || result.discovered != count as u64 {
            return Err("scan_incomplete".into());
        }
        results.push(json!({"operation":name,"result":result}));
    }
    scanner.stop();
    db.stop();
    Ok(
        json!({"workload":"synthetic short WAV metadata, not audio decode throughput","results":results}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disposable_benchmark_exercises_current_schema_and_refuses_existing_database() {
        let root =
            std::env::temp_dir().join(format!("reborn-platform-bench-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let (report, tracks) = database(&root, 128).unwrap();
        assert_eq!(tracks.len(), 128);
        assert_eq!(report["quick_check"]["tracks"], 128);
        assert_eq!(report["batch_tracks"], 64);
        assert!(database(&root, 128).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
