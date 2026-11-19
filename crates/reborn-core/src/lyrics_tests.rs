use super::*;

#[test]
fn milliseconds_metadata_bom_crlf_unicode_and_empty_breaks() {
    let doc = parse("\u{feff}[ar:Original Artist]\r\n[ti:Homeward]\r\n[al:Examples]\r\n[00:01.2]First\r\n[01:23.45]灯り\r\n[01:24.456]\r\n".as_bytes()).unwrap();
    assert_eq!(
        doc.lines.iter().map(|l| l.at_ms).collect::<Vec<_>>(),
        [1200, 83_450, 84_456]
    );
    assert_eq!(&*doc.lines[1].text, "灯り");
    assert!(doc.lines[2].text.is_empty());
}

#[test]
fn unsorted_multiple_and_duplicate_times_keep_stable_order() {
    let doc = parse(b"[00:03][00:01]A\n[00:01.00]B\n[00:02]C").unwrap();
    assert_eq!(
        doc.lines.iter().map(|l| &*l.text).collect::<Vec<_>>(),
        ["A", "B", "C", "A"]
    );
    assert!(Arc::ptr_eq(&doc.lines[0].text, &doc.lines[3].text));
    assert_eq!(doc.active(1000), Some(1));
}

#[test]
fn final_offset_applies_globally_and_positive_advances() {
    let doc = parse(b"[00:00.1]A\n[offset:-100]\n[00:01]B\n[offset:+500]\n[offset:oops]").unwrap();
    assert_eq!(
        doc.lines.iter().map(|l| l.at_ms).collect::<Vec<_>>(),
        [0, 500]
    );
    assert_eq!(
        parse(b"[offset:-1500]\n[00:01]A").unwrap().lines[0].at_ms,
        2500
    );
    assert_eq!(
        parse(b"[offset:-9223372036854775808]\n[00:01]A")
            .unwrap()
            .lines[0]
            .at_ms,
        1000 + 9_223_372_036_854_775_808
    );
}

#[test]
fn malformed_times_do_not_overflow_or_become_timed_lyrics() {
    let doc = parse(b"[99:99]A\n[-1:02]B\n[1:2]C\n[00:01.]D\n[00:02.1234]E\n[18446744073709551615:00]F\n[00:00.001]Good").unwrap();
    assert_eq!(doc.lines.len(), 1);
    assert_eq!(doc.lines[0].at_ms, 1);
    assert_eq!(&*doc.lines[0].text, "Good");
    assert!(parse(b"[ar:Example]\nplain unsynchronized text")
        .unwrap()
        .lines
        .is_empty());
    assert_eq!(parse(&[0xff]), Err(Error::InvalidUtf8));
}

#[test]
fn active_position_follows_pause_seek_repeat_and_track_boundaries() {
    let doc = parse(b"[00:01]A\n[00:02]B\n[00:05]C").unwrap();
    for (position, expected) in [
        (0, None),
        (1000, Some(0)),
        (2500, Some(1)),
        (2500, Some(1)),
        (6000, Some(2)),
        (500, None),
        (2000, Some(1)),
        (0, None),
    ] {
        assert_eq!(doc.active(position), expected);
    }
}

#[test]
fn limits_cover_raw_bytes_lines_timestamps_and_expanded_text() {
    assert_eq!(parse(&vec![b'x'; MAX_BYTES + 1]), Err(Error::TooLarge));
    assert_eq!(
        parse("\n".repeat(MAX_INPUT_LINES + 1).as_bytes()),
        Err(Error::TooManyLines)
    );
    assert_eq!(
        parse("x".repeat(MAX_LINE_BYTES + 1).as_bytes()),
        Err(Error::LineTooLong)
    );
    assert_eq!(
        parse(format!("{}word", "[00:01]".repeat(33)).as_bytes()),
        Err(Error::TooManyEntries)
    );
    assert_eq!(
        parse("[00:01]x\n".repeat(MAX_ENTRIES + 1).as_bytes()),
        Err(Error::TooManyEntries)
    );
    let repeated = format!("{}{}\n", "[00:01]".repeat(32), "x".repeat(2048)).repeat(5);
    assert_eq!(parse(repeated.as_bytes()), Err(Error::TextLimit));
}

#[test]
fn cache_identity_includes_source_path_revision_and_mount() {
    let mut track = Track {
        source_id: "card".into(),
        path: "/music/A.mp3".into(),
        size: 100,
        mtime: 1,
        ..Track::default()
    };
    let mut sources = vec![Source {
        id: "card".into(),
        mount_id: Some(10),
        kind: crate::MediaSource::SdCard("card".into()),
        root: "/music".into(),
        online: true,
        mount: "test".into(),
    }];
    let key = Key::new(&track, &sources);
    assert!(key.matches(&track, &sources));
    sources[0].mount_id = Some(11);
    assert!(!key.matches(&track, &sources));
    sources[0].mount_id = Some(10);
    track.mtime = 2;
    assert!(!key.matches(&track, &sources));
    track.mtime = 1;
    track.path = "/music/B.mp3".into();
    assert!(!key.matches(&track, &sources));
    track.path = "/music/A.mp3".into();
    sources[0].root = "/another-source".into();
    assert!(!key.matches(&track, &sources));
}
