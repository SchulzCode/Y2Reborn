use super::*;
use crate::RADIO_SOURCE;

fn song(name: &str, mtime: i64) -> Track {
    Track {
        source_id: "internal".into(),
        path: format!("/music/{name}.flac").into(),
        filename: format!("{name}.flac"),
        title: name.into(),
        duration_ms: 200_000,
        mtime,
        online: true,
        ..Default::default()
    }
}

#[test]
fn most_played_by_count_and_recently_played_by_last_play() {
    let tracks = vec![song("a", 1), song("b", 2), song("c", 3)];
    let mut stats = PlayStats::default();
    stats.record(&tracks[0]);
    stats.record(&tracks[0]);
    stats.record(&tracks[2]);
    stats.record(&tracks[1]);
    assert_eq!(
        stats.most_played(&tracks),
        vec![0, 1, 2],
        "a twice, then the later one"
    );
    assert_eq!(stats.recently_played(&tracks), vec![1, 2, 0]);
    assert_eq!(stats.plays(&tracks[0]), 2);
}

#[test]
fn only_songs_count() {
    let mut station = song("radio", 1);
    station.source_id = RADIO_SOURCE.into();
    let mut book = song("book", 1);
    book.book = true;
    let mut short = song("jingle", 1);
    short.duration_ms = 10_000;
    let mut stats = PlayStats::default();
    for t in [&station, &book, &short] {
        stats.record(t);
    }
    assert_eq!(stats.generation(), 0);
    assert!(stats.recently_played(&[station, book, short]).is_empty());
}

#[test]
fn a_rescan_keeps_the_record_and_a_missing_song_is_left_out() {
    let mut stats = PlayStats::default();
    stats.record(&song("a", 1));
    stats.record(&song("b", 1));
    // A rebuilt library: new ids and order, b gone.
    let mut a = song("a", 1);
    a.id = 99;
    let rebuilt = vec![song("c", 1), a];
    assert_eq!(stats.recently_played(&rebuilt), vec![1]);
}

#[test]
fn the_record_is_bounded_and_survives_a_save() {
    let mut stats = PlayStats::default();
    for i in 0..KEEP + 10 {
        stats.record(&song(&i.to_string(), 1));
    }
    assert_eq!(stats.songs.len(), KEEP);
    assert_eq!(stats.plays(&song("0", 1)), 0, "the longest unplayed went");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("play-stats-{nanos}"));
    let path = dir.join("play-stats.json");
    stats.save(&path).unwrap();
    let loaded = PlayStats::load(&path);
    assert_eq!(loaded.plays(&song("20", 1)), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn recently_added_is_newest_file_first() {
    let tracks = vec![song("old", 10), song("new", 30), song("mid", 20)];
    assert_eq!(recently_added(&tracks), vec![1, 2, 0]);
}
