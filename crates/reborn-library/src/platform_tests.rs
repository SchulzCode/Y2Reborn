//! Behavior and failure tests for the bounded scanner and resident catalog.
use super::*;
use std::process::{Command, Stdio};

fn scratch(label: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("reborn-platform-db-{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}
fn fixture(i: usize, source: &str) -> Track {
    Track {
        source_id: source.into(),
        path: format!("/media/sd/{i:05}.flac").into(),
        title: "Same title".into(),
        artist: "Same artist".into(),
        album: "Same album".into(),
        size: 123,
        mtime: 456,
        ..Default::default()
    }
}
fn source(id: &str) -> Source {
    Source {
        id: id.into(),
        root: "/media/sd".into(),
        kind: MediaSource::Internal,
        online: true,
        mount: "test".into(),
        mount_id: None,
    }
}
fn seeded(root: &Path) -> Connection {
    let c = open(&root.join("library.db")).unwrap();
    c.execute(
        "INSERT INTO sources VALUES('one','/media/sd',1),('two','/media/sd',0)",
        [],
    )
    .unwrap();
    c
}
fn count(c: &Connection) -> i64 {
    c.query_row("SELECT count(*) FROM tracks WHERE deleted=0", [], |r| {
        r.get(0)
    })
    .unwrap()
}
#[test]
fn sleep_barrier_drains_fifo_work_and_refuses_an_open_transaction() {
    let root = scratch("sleep-barrier");
    let db = Database::spawn(
        root.join("library.db"),
        Observer::new(&root.join("logs")).unwrap(),
    )
    .unwrap();
    db.sources(vec![source("sleep-source")]).unwrap();
    db.prepare_sleep(Duration::from_secs(2)).unwrap();
    let c = Connection::open(root.join("library.db")).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sources WHERE id='sleep-source'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    db.execute_test_sql("BEGIN IMMEDIATE".into()).unwrap();
    assert_eq!(
        db.prepare_sleep(Duration::from_secs(2)).unwrap_err(),
        "database_operation_active"
    );
    db.execute_test_sql("ROLLBACK".into()).unwrap();
    db.prepare_sleep(Duration::from_secs(2)).unwrap();
    db.begin_scan("sleep-source".into()).unwrap();
    assert_eq!(
        db.prepare_sleep(Duration::from_secs(2)).unwrap_err(),
        "database_operation_active"
    );
    db.shutdown(Duration::from_secs(2)).unwrap();
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn fifty_thousand_catalog_has_no_cap_or_duplicate_boundary_rows() {
    let root = scratch("large");
    let mut c = seeded(&root);
    for offset in (0..25_000).step_by(64) {
        let end = (offset + 64).min(25_000);
        // Identical sort keys and paths across media exercise the rowid cursor.
        for id in ["one", "two"] {
            batch(&mut c, (offset..end).map(|i| fixture(i, id)).collect(), 1).unwrap();
        }
    }
    let tracks = catalog(&mut c).unwrap();
    assert_eq!(tracks.len(), 50_000);
    let identities: std::collections::BTreeSet<_> = tracks.iter().map(|t| t.id).collect();
    assert_eq!(identities.len(), 50_000);
    assert_eq!(tracks.iter().filter(|t| t.online).count(), 25_000);
    assert_eq!(
        list(
            &c,
            &Filter {
                limit: usize::MAX,
                ..Default::default()
            }
        )
        .unwrap()
        .len(),
        CATALOG_PAGE_SIZE
    );
    assert_eq!(validate(&c).unwrap()["tracks"], 50_000);
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn unchanged_presence_and_sources_do_not_mutate_persistent_rows() {
    let root = scratch("read-only-reuse");
    let mut c = seeded(&root);
    let t = fixture(0, "one");
    batch(&mut c, vec![t.clone()], 1).unwrap();
    // A permanent trigger makes any persistent metadata touch observable.
    c.execute_batch("CREATE TRIGGER refuse_track_update BEFORE UPDATE ON tracks BEGIN SELECT RAISE(ABORT,'redundant track update'); END;
        CREATE TRIGGER refuse_source_update BEFORE UPDATE ON sources BEGIN SELECT RAISE(ABORT,'redundant source update'); END;").unwrap();
    update_sources(
        &mut c,
        &[
            source("one"),
            Source {
                online: false,
                ..source("two")
            },
        ],
    )
    .unwrap();
    let before = c.total_changes();
    batch(&mut c, vec![t.clone()], 2).unwrap();
    assert_eq!(c.total_changes(), before);
    let fingerprint = FileStamp {
        path: t.path,
        size: t.size,
        mtime: t.mtime,
        metadata: None,
    };
    assert_eq!(
        probe(&mut c, "one", &[fingerprint], true).unwrap(),
        vec![true]
    );
    assert!(finish(&mut c, &source("one"), true).unwrap().verified);
    assert_eq!(count(&c), 1);
    assert_eq!(
        c.query_row("SELECT seen FROM tracks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn same_path_on_replacement_media_does_not_reuse_or_prune_old_card() {
    let root = scratch("media");
    let mut c = seeded(&root);
    batch(&mut c, vec![fixture(0, "one")], 1).unwrap();
    let stamp = FileStamp {
        path: fixture(0, "two").path,
        size: 123,
        mtime: 456,
        metadata: None,
    };
    assert_eq!(probe(&mut c, "two", &[stamp], true).unwrap(), vec![false]);
    batch_tracked(&mut c, vec![fixture(0, "two")], 2, true).unwrap();
    update_sources(&mut c, &[source("two")]).unwrap();
    assert!(finish(&mut c, &source("two"), true).unwrap().verified);
    let tracks = catalog(&mut c).unwrap();
    assert_eq!(tracks.len(), 2);
    assert_ne!(tracks[0].id, tracks[1].id);
    assert!(tracks.iter().any(|t| t.source_id == "one" && !t.online));
    assert!(tracks.iter().any(|t| t.source_id == "two" && t.online));
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn sqlite_full_rolls_back_whole_batch_and_leaves_usable_library() {
    let root = scratch("sqlite-full");
    let mut c = seeded(&root);
    batch(&mut c, vec![fixture(0, "one")], 1).unwrap();
    let pages: i64 = c
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    c.pragma_update(None, "max_page_count", pages).unwrap();
    let changes = (1..65)
        .map(|i| Track {
            title: "x".repeat(16_384),
            ..fixture(i, "one")
        })
        .collect();
    let error = batch_tracked(&mut c, changes, 2, true).unwrap_err();
    assert!(error.to_lowercase().contains("full"));
    assert_eq!(count(&c), 1);
    assert_eq!(
        c.query_row("SELECT count(*) FROM scan_seen", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(catalog(&mut c).unwrap()[0].title, "Same title");
    assert_eq!(validate(&c).unwrap()["quick_check"], "ok");
    drop(c);
    let c = open(&root.join("library.db")).unwrap();
    assert_eq!(count(&c), 1);
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn additive_migration_failure_rolls_back_and_is_not_quarantined() {
    let root = scratch("migration-failure");
    let c = seeded(&root);
    c.execute_batch("DROP INDEX tracks_browse; DROP INDEX tracks_browse_album;
        CREATE TABLE tracks_browse_album(evidence TEXT); INSERT INTO tracks_browse_album VALUES('preserve');").unwrap();
    drop(c);
    assert!(matches!(
        open(&root.join("library.db")),
        Err(OpenFailure::Migration(_))
    ));
    let log = Observer::new(&root.join("logs")).unwrap();
    assert!(Database::spawn(root.join("library.db"), log).is_err());
    let c = Connection::open(root.join("library.db")).unwrap();
    assert_eq!(
        c.query_row("SELECT evidence FROM tracks_browse_album", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "preserve"
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='tracks_browse'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(!fs::read_dir(&root).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains("corrupt")));
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn malformed_numeric_row_reports_error_without_destroying_other_rows() {
    let root = scratch("malformed");
    let mut c = seeded(&root);
    batch(&mut c, vec![fixture(0, "one"), fixture(1, "one")], 1).unwrap();
    c.execute("UPDATE tracks SET size=-1 WHERE id=2", [])
        .unwrap();
    assert!(catalog(&mut c).is_err());
    assert_eq!(count(&c), 2);
    assert_eq!(validate(&c).unwrap()["quick_check"], "ok");
    // A new valid scan can repair the row without quarantine or rebuild.
    batch(&mut c, vec![fixture(1, "one")], 2).unwrap();
    assert_eq!(catalog(&mut c).unwrap().len(), 2);
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn query_plans_seek_browse_identity_and_temp_presence_indexes() {
    let root = scratch("plans");
    let c = seeded(&root);
    let plans = |sql: &str| {
        let mut q = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        q.query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n")
    };
    let ordered=plans(&format!("{SELECT} WHERE t.deleted=0 ORDER BY t.artist,t.album,t.disc,t.track,t.title,t.path LIMIT 64"));
    assert!(ordered.contains("tracks_browse"));
    assert!(!ordered.contains("TEMP B-TREE"));
    let artist=plans(&format!("{SELECT} WHERE t.deleted=0 AND t.artist='Same artist' ORDER BY t.artist,t.album,t.disc,t.track,t.title,t.path LIMIT 64"));
    assert!(artist.contains("SEARCH t USING INDEX tracks_browse"));
    assert!(!artist.contains("TEMP B-TREE"));
    let identity=plans("SELECT t.id FROM scan_candidates c LEFT JOIN tracks t ON t.source_id='one' AND t.path=c.path AND t.size=c.size AND t.mtime=c.mtime AND t.deleted=0 ORDER BY c.position");
    assert!(identity.contains("sqlite_autoindex_tracks_1"));
    let prune=plans("UPDATE tracks SET deleted=1 WHERE source_id='one' AND deleted=0 AND NOT EXISTS (SELECT 1 FROM scan_seen WHERE id=tracks.id)");
    assert!(prune.contains("sqlite_autoindex_tracks_1"));
    assert!(prune.contains("INTEGER PRIMARY KEY"));
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn crash_child() {
    let Some(root) = std::env::var_os("REBORN_DB_CRASH_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(root);
    let c = open(&root.join("library.db")).unwrap();
    c.execute_batch("BEGIN IMMEDIATE; UPDATE tracks SET title='uncommitted',deleted=1;")
        .unwrap();
    fs::write(root.join("ready"), b"ready").unwrap();
    // Parent kills this process with a transaction open; no Drop rollback.
    loop {
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn process_death_during_update_recovers_committed_library_on_restart() {
    let root = scratch("process-death");
    let mut c = seeded(&root);
    batch(&mut c, vec![fixture(0, "one")], 1).unwrap();
    drop(c);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "platform_tests::crash_child"])
        .env("REBORN_DB_CRASH_FIXTURE", &root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("ready").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let ready = root.join("ready").exists();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(ready);
    let mut c = open(&root.join("library.db")).unwrap();
    assert_eq!(count(&c), 1);
    assert_eq!(catalog(&mut c).unwrap()[0].title, "Same title");
    assert_eq!(validate(&c).unwrap()["quick_check"], "ok");
    drop(c);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn interrupted_scan_keeps_published_rows_and_retry_can_prune() {
    let root = scratch("scan-restart");
    let mut c = seeded(&root);
    batch(&mut c, vec![fixture(0, "one")], 1).unwrap();
    batch_tracked(&mut c, vec![fixture(1, "one")], 2, true).unwrap();
    // Scan disappears before Finish. Its TEMP presence cannot survive restart.
    drop(c);
    let mut c = open(&root.join("library.db")).unwrap();
    assert_eq!(count(&c), 2);
    assert_eq!(
        c.query_row("SELECT count(*) FROM scan_seen", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let t = fixture(1, "one");
    let stamp = FileStamp {
        path: t.path,
        size: t.size,
        mtime: t.mtime,
        metadata: None,
    };
    assert_eq!(probe(&mut c, "one", &[stamp], true).unwrap(), vec![true]);
    assert!(finish(&mut c, &source("one"), true).unwrap().verified);
    assert_eq!(count(&c), 1);
    drop(c);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn storage_cid_qualified_ids_keep_uuid_and_mount_identity_guards() {
    let hash = "a".repeat(64);
    assert!(sd_id_matches_uuid("uuid:volume", "volume"));
    assert!(sd_id_matches_uuid(
        &format!("uuid:cid:{hash}"),
        &format!("cid:{hash}")
    ));
    assert!(sd_id_matches_uuid(
        &format!("uuid:volume:cid:{hash}"),
        "volume"
    ));
    assert!(!sd_id_matches_uuid(
        &format!("uuid:other:cid:{hash}"),
        "volume"
    ));
    assert!(!sd_id_matches_uuid("uuid:volume:cid:bad", "volume"));
    let source = Source {
        id: format!("uuid:volume:cid:{hash}"),
        kind: MediaSource::SdCard("volume".into()),
        mount_id: Some(40),
        ..source("unused")
    };
    assert!(source_identity_matches_at(
        &source,
        "40 1 179:1 / /media/sd rw - vfat /dev/mmcblk0p1 rw\n"
    ));
    assert!(!source_identity_matches_at(
        &source,
        "58 1 179:1 / /media/sd rw - vfat /dev/mmcblk0p1 rw\n"
    ));
    let uuidless = Source {
        id: format!("uuid:cid:{hash}"),
        kind: MediaSource::SdCard(format!("cid:{hash}")),
        ..source
    };
    assert!(source_identity_matches_at(
        &uuidless,
        "40 1 179:1 / /media/sd rw - vfat /dev/mmcblk0p1 rw\n"
    ));
}

#[test]
fn schema_enforces_source_foreign_keys_and_preserves_connection_policy() {
    let root = scratch("schema-policy");
    let mut c = seeded(&root);
    assert_eq!(
        c.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    for (pragma, expected) in [
        ("foreign_keys", 1),
        ("synchronous", 1),
        ("cache_size", -2048),
        ("temp_store", 2),
        ("wal_autocheckpoint", 1000),
    ] {
        assert_eq!(
            c.pragma_query_value(None, pragma, |r| r.get::<_, i64>(0))
                .unwrap(),
            expected
        );
    }
    let error = batch(&mut c, vec![fixture(0, "missing-source")], 1).unwrap_err();
    assert!(error.to_lowercase().contains("foreign key"));
    assert_eq!(count(&c), 0);
    batch(&mut c, vec![fixture(0, "one")], 1).unwrap();
    assert!(c.execute("UPDATE tracks SET title=NULL", []).is_err());
    assert_eq!(catalog(&mut c).unwrap()[0].title, "Same title");
    drop(c);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn uuid_to_cid_upgrade_preserves_offline_rows_and_saved_queue_identity() {
    let root = scratch("cid-upgrade");
    let mut c = open(&root.join("library.db")).unwrap();
    let old = "uuid:volume";
    let new = format!("uuid:volume:cid:{}", "a".repeat(64));
    update_sources(&mut c, &[source(old)]).unwrap();
    batch(&mut c, vec![fixture(0, old)], 1).unwrap();
    let saved = catalog(&mut c).unwrap()[0].clone();
    update_sources(&mut c, &[source(&new)]).unwrap();
    batch(&mut c, vec![fixture(0, &new)], 2).unwrap();
    let tracks = catalog(&mut c).unwrap();
    assert_eq!(tracks.len(), 2);
    let old_row = tracks.iter().find(|t| t.id == saved.id).unwrap();
    assert_eq!(old_row.source_id, saved.source_id);
    assert!(!old_row.online);
    let new_row = tracks.iter().find(|t| t.source_id == new).unwrap();
    assert!(new_row.online);
    assert_ne!(new_row.id, saved.id);
    assert_ne!(new_row.source_id, saved.source_id);
    drop(c);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn resident_catalog_counts_offline_rows_towards_unchanged_capacity() {
    let root = scratch("resident-capacity");
    let mut c = seeded(&root);
    // SQL-only threshold fixture: this checks the cap, not hardware/RSS qualification.
    c.execute_batch("WITH RECURSIVE n(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i<250000)
        INSERT INTO tracks(source_id,path,filename,size,mtime,title,artist,album,album_artist,track,disc,duration_ms,codec,sample_rate,channels,bitrate,artwork,seen)
        SELECT 'two',printf('/media/sd/%06d.flac',i),'fixture',1,1,'','','','',0,0,0,'',0,0,0,0,1 FROM n;").unwrap();
    assert_eq!(count(&c), MAX_LIBRARY_TRACKS as i64 + 1);
    assert_eq!(
        catalog(&mut c).unwrap_err(),
        "library exceeds 250000-track limit"
    );
    assert_eq!(count(&c), MAX_LIBRARY_TRACKS as i64 + 1);
    drop(c);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn prepared_scanner_rejects_usb_and_rebuild_work_until_restore() {
    let root = scratch("prepared-scan");
    let log = Observer::new(&root.join("logs")).unwrap();
    let db = Database::spawn(root.join("library.db"), log.clone()).unwrap();
    let scanner = Scanner::spawn(db.clone(), log).unwrap();
    assert!(scanner.quiesce());
    assert_eq!(
        scanner.scan(vec![]).unwrap_err(),
        "sleep_operation_in_progress"
    );
    assert_eq!(
        scanner.rebuild(vec![]).unwrap_err(),
        "sleep_operation_in_progress"
    );
    assert!(matches!(
        scanner.results.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    scanner.resume();
    scanner.scan(vec![]).unwrap();
    assert!(
        scanner
            .results
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .complete
    );
    let until = Instant::now() + Duration::from_secs(1);
    while !scanner.quiesce() {
        assert!(Instant::now() < until);
        thread::yield_now();
    }
    assert!(scanner.scan(vec![]).is_err());
    scanner.stop();
    db.shutdown(Duration::from_secs(5)).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_published_catalog_lists_one_album_under_one_album_artist() {
    // Y2Reborn #45: one song of an album carries Album Artist, the others none.
    let root = scratch("album-artist");
    let mut c = seeded(&root);
    let songs: Vec<Track> = (0..3)
        .map(|i| Track {
            album: "Discovery".into(),
            album_artist: if i == 2 {
                "Daft Punk".into()
            } else {
                String::new()
            },
            ..fixture(i, "one")
        })
        .collect();
    batch(&mut c, songs, 1).unwrap();
    drop(c);
    let db = Database::spawn(
        root.join("library.db"),
        Observer::new(&root.join("logs")).unwrap(),
    )
    .unwrap();
    let tracks = db
        .catalog()
        .unwrap()
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(tracks.len(), 3);
    assert!(tracks.iter().all(|t| t.album_artist == "Daft Punk"));
    assert_eq!(
        tracks
            .iter()
            .filter(|t| t.tagged_album_artist().is_empty())
            .count(),
        2,
        "each song keeps its own tag"
    );
    // The file is unchanged.
    let c = Connection::open(root.join("library.db")).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM tracks WHERE album_artist=''",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    drop(c);
    db.shutdown(Duration::from_secs(5)).unwrap();
    fs::remove_dir_all(root).unwrap();
}
