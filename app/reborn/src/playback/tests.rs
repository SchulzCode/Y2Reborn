use super::*;
use reborn_audio::Parameters;
use reborn_core::{AudioOutput, PcmFormat};

#[test]
fn an_opened_sink_fades_for_a_rise_and_otherwise_continues_an_unfinished_fade() {
    let event = |raised, dsp, carried| opening_gain(100, 1000, raised, dsp, carried).1.map(|e| e.0);
    assert_eq!(event(true, true, None), Some("level_raise_fade"));
    assert_eq!(event(false, true, Some((0.5, 10))), Some("dsp_rise_fade"));
    assert_eq!(
        event(false, false, Some((0.5, 1000))),
        Some("fade_continued")
    );
    assert_eq!(event(false, false, None), None);
    // A fresh fade starts silent; a continued one where the last one was.
    let (fresh, _) = opening_gain(100, 1000, true, false, Some((0.5, 1000)));
    assert_eq!(fresh.fade_progress(1000), Some((0.0, LEVEL_RAISE_FADE_MS)));
    let (continued, _) = opening_gain(100, 1000, false, false, Some((0.5, 1000)));
    assert_eq!(continued.fade_progress(1000), Some((0.5, 1000)));
    assert!(opening_gain(100, 1000, false, false, None)
        .0
        .fade_progress(1000)
        .is_none());
}

struct Sink {
    frames: Arc<AtomicU64>,
    fail: bool,
    rate: u32,
    samples: Option<Arc<Mutex<Vec<i32>>>>,
}
impl AudioSink for Sink {
    fn parameters(&self) -> Parameters {
        Parameters {
            rate: self.rate,
            period: 512,
            buffer: 4096,
            format: PcmFormat::S32LE,
            channels: 2,
            hardware_mixer_gain_db: None,
            device: "test".into(),
            fallback: false,
            fallback_reason: String::new(),
            hp_level_live: None,
            level_raised: false,
        }
    }
    fn write(&mut self, s: &[u8]) -> Result<usize, String> {
        if self.fail {
            return Err("injected sink disconnect".into());
        }
        thread::sleep(Duration::from_millis(2));
        let n = (s.len() / 8).min(128);
        if let Some(samples) = &self.samples {
            let mut samples = samples.lock().unwrap();
            for frame in s.as_chunks::<8>().0.iter().take(n) {
                samples.push(i32::from_le_bytes(frame[..4].try_into().unwrap()));
            }
        }
        self.frames.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
    fn discard(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn delay(&self) -> u64 {
        0
    }
}
/// Runs `open_with_retries` on a virtual clock that only the pauses move
/// (plus `open_ms` per open); returns the result, opens and pauses (ms).
fn retried(
    mut outcomes: Vec<Result<(), &'static str>>,
    open_ms: u64,
) -> (Result<(), String>, usize, Vec<u64>) {
    use std::cell::Cell;
    let clock = Cell::new(Duration::ZERO);
    let opens = Cell::new(0);
    let pauses = std::cell::RefCell::new(Vec::new());
    outcomes.reverse();
    let result = open_with_retries(
        || {
            opens.set(opens.get() + 1);
            clock.set(clock.get() + Duration::from_millis(open_ms));
            outcomes.pop().unwrap_or(Ok(())).map_err(str::to_owned)
        },
        |_, attempt, _| assert_eq!(attempt as usize, opens.get()),
        |delay| {
            pauses.borrow_mut().push(delay.as_millis() as u64);
            clock.set(clock.get() + delay);
        },
        || clock.get(),
    );
    (result, opens.get(), pauses.into_inner())
}
const BUSY: &str = "ALSA -16: Device or resource busy";
const AGAIN: &str = "ALSA -11: Resource temporarily unavailable";
const CRYSTAL: &str = "ALSA -5: Input/output error";

#[test]
fn a_busy_output_is_tried_again_for_two_seconds_with_short_pauses() {
    // Busy for ≈1.5 s after a wake, then free: it plays.
    let (result, opens, pauses) = retried(vec![Err(BUSY); 7], 1);
    assert!(result.is_ok());
    assert_eq!(opens, 8);
    assert_eq!(pauses, [100, 150, 200, 250, 250, 250, 250]);
    let (result, _, _) = retried(vec![Err(AGAIN), Err(BUSY)], 1);
    assert!(result.is_ok());
    // Never free: the busy error after ≈2 s, well inside the open deadline.
    let (result, opens, pauses) = retried(vec![Err(BUSY); 100], 1);
    assert_eq!(result.unwrap_err(), BUSY);
    let waited = pauses.iter().sum::<u64>() + opens as u64;
    assert!(waited <= BUSY_OPEN_FOR.as_millis() as u64, "{waited}");
    assert!(waited >= 1800, "{waited}");
}

#[test]
fn any_other_refusal_is_tried_once_more_and_never_past_the_deadline() {
    // The crystal case (#48, #52): ≈1.9 s per open, one retry plays.
    let (result, opens, pauses) = retried(vec![Err(CRYSTAL)], 1900);
    assert!(result.is_ok());
    assert_eq!((opens, pauses), (2, vec![250]));
    let (result, opens, _) = retried(vec![Err(CRYSTAL), Err(CRYSTAL)], 1);
    assert_eq!(result.unwrap_err(), CRYSTAL);
    assert_eq!(opens, 2);
    // A refusal too late for one more slow open inside the 5 s deadline.
    let (result, opens, _) = retried(vec![Err(CRYSTAL)], 3000);
    assert_eq!(result.unwrap_err(), CRYSTAL);
    assert_eq!(opens, 1);
    // Busy at first, then the crystal: still its one retry.
    let (result, opens, _) = retried(vec![Err(BUSY), Err(CRYSTAL)], 1);
    assert!(result.is_ok());
    assert_eq!(opens, 3);
    assert!(!open_busy(CRYSTAL));
    assert!(!open_busy(
        "Bluetooth sink spec has no observed transport epoch"
    ));
}

#[test]
fn a_cancelled_job_opens_no_file_and_is_not_a_file_failure() {
    let cancel = Cancel::new().unwrap();
    cancel.cancel();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
    let output = OutputSpec {
        rate: 44_100,
        format: PcmFormat::S32LE,
    };
    let error = BoundDecoder::open(
        &gapless_track("gapless-a.flac", 1),
        &root,
        output,
        DspConfig::default(),
        cancel,
    )
    .err()
    .unwrap();
    assert!(open_cancelled(&error), "{error}");
    assert!(!open_cancelled(
        "FFmpeg -1094995529: Invalid data found when processing input"
    ));
}
fn track() -> Track {
    Track {
        source_id: "internal".into(),
        path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac"),
        ..Default::default()
    }
}
fn gapless_track(name: &str, id: i64) -> Track {
    Track {
        source_id: "internal".into(),
        id,
        path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/fixtures")
            .join(name),
        ..Default::default()
    }
}
#[test]
fn pinned_file_probe_preserves_fixture_metadata_and_pcm_without_extension() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let spec = OutputSpec {
        rate: 44_100,
        format: PcmFormat::S32LE,
    };
    let mut tested = 0;
    for name in manifest["files"].as_object().unwrap().keys() {
        if name.starts_with("malformed")
            || name.starts_with("truncated")
            || matches!(
                std::path::Path::new(name)
                    .extension()
                    .and_then(|s| s.to_str()),
                Some("jpg" | "png" | "webp")
            )
        {
            continue;
        }
        let track = gapless_track(name, 1);
        let mut direct = Decoder::open_with(
            &track.path,
            spec,
            DspConfig::default(),
            Cancel::new().unwrap(),
        )
        .unwrap();
        let mut pinned = BoundDecoder::open(
            &track,
            &root,
            spec,
            DspConfig::default(),
            Cancel::new().unwrap(),
        )
        .unwrap();
        let semantic = |metadata: &reborn_media::Metadata| {
            let mut value = serde_json::to_value(metadata).unwrap();
            for key in ["open_us", "probe_us", "io_bytes", "info_runs"] {
                value.as_object_mut().unwrap().remove(key);
            }
            value
        };
        assert_eq!(
            semantic(&direct.metadata),
            semantic(&pinned.metadata),
            "{name}"
        );
        assert!(direct.metadata.io_bytes > 0 && pinned.metadata.io_bytes > 0);
        let expected = direct.read().unwrap().unwrap();
        let actual = pinned.read().unwrap().unwrap();
        assert_eq!(actual.data, expected.data, "{name}");
        assert_eq!(actual.position_ms, expected.position_ms, "{name}");
        tested += 1;
    }
    // 36 music fixtures and the three audiobook specimens (book-*).
    assert_eq!(tested, 39);
    let pinned = reborn_platform::storage::pin_track(&track(), &root).unwrap();
    assert!(external_artwork(pinned.path(), ART_SIZE as u32).is_err());
    for name in [
        "artwork-external.jpg",
        "artwork-external.png",
        "artwork-external.webp",
    ] {
        let file = pinned.open_sidecar(name).unwrap();
        assert_eq!(
            external_artwork(file.path(), ART_SIZE as u32).unwrap(),
            external_artwork(&root.join(name), ART_SIZE as u32).unwrap(),
            "{name}"
        );
    }
}
#[test]
fn pinned_sidecars_keep_case_fallback_and_parent_identity_without_following_links() {
    use std::os::unix::fs::symlink;
    let root = std::env::temp_dir().join(format!("reborn-pinned-art-{}", std::process::id()));
    let moved = root.with_extension("moved");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&moved);
    std::fs::create_dir(&root).unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
    std::fs::copy(fixtures.join("tone.flac"), root.join("song.flac")).unwrap();
    std::fs::write(root.join("CoVeR.jpg"), b"corrupt cover").unwrap();
    std::fs::copy(
        fixtures.join("artwork-external.png"),
        root.join("FoLdEr.PNG"),
    )
    .unwrap();
    symlink(
        fixtures.join("artwork-external.jpg"),
        root.join("album.jpg"),
    )
    .unwrap();
    let track = Track {
        source_id: "internal".into(),
        path: root.join("song.flac"),
        ..Default::default()
    };
    let pinned = reborn_platform::storage::pin_track(&track, &root).unwrap();
    let expected = external_artwork(&root.join("FoLdEr.PNG"), ART_SIZE as u32).unwrap();
    assert_eq!(pinned_folder_artwork(&pinned).unwrap(), expected);
    assert!(pinned.open_sidecar("album.jpg").is_err());
    std::fs::rename(&root, &moved).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::copy(
        fixtures.join("artwork-external.jpg"),
        root.join("cover.jpg"),
    )
    .unwrap();
    assert_eq!(pinned_folder_artwork(&pinned).unwrap(), expected);
    std::fs::remove_file(moved.join("FoLdEr.PNG")).unwrap();
    assert!(pinned_folder_artwork(&pinned).is_err());
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(moved).unwrap();
}
fn spec(output: AudioOutput, rate: u32) -> SinkSpec {
    SinkSpec {
        output,
        rate,
        format: PcmFormat::S32LE,
        physical_bits: 32,
        valid_bits: 32,
        channels: 2,
        layout: "stereo".into(),
        device: "test".into(),
        codec: None,
        transport_object: None,
        transport_device: None,
        transport: None,
        mode: None,
        transport_generation: 0,
        fallback: false,
        fallback_reason: String::new(),
        speaker: false,
    }
}
struct VolumeTestSink {
    captured: Arc<Mutex<Vec<i32>>>,
    allowed: Arc<AtomicU64>,
    frames: Arc<AtomicU64>,
}
impl AudioSink for VolumeTestSink {
    fn parameters(&self) -> Parameters {
        Sink {
            frames: self.frames.clone(),
            fail: false,
            rate: 44_100,
            samples: None,
        }
        .parameters()
    }
    fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        let written = self.frames.load(Ordering::Acquire);
        let room = self.allowed.load(Ordering::Acquire).saturating_sub(written) as usize;
        let count = (data.len() / 8).min(73).min(room);
        if count == 0 {
            thread::sleep(Duration::from_millis(1));
        }
        let mut captured = self.captured.lock().unwrap();
        captured.extend(data.as_chunks::<8>().0.iter().take(count).map(|frame| {
            let left = i32::from_le_bytes(frame[..4].try_into().unwrap());
            let right = i32::from_le_bytes(frame[4..].try_into().unwrap());
            assert_eq!(left, right);
            left
        }));
        self.frames.fetch_add(count as u64, Ordering::Release);
        Ok(count)
    }
    fn discard(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn delay(&self) -> u64 {
        0
    }
}
#[test]
fn live_volume_changes_queued_pcm_without_reopening_or_losing_frames() {
    let root = std::env::temp_dir().join(format!("reborn-live-volume-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("constant.wav");
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36u32 + 4096 * 8).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&44_100u32.to_le_bytes());
    wav.extend_from_slice(&(44_100u32 * 8).to_le_bytes());
    wav.extend_from_slice(&8u16.to_le_bytes());
    wav.extend_from_slice(&32u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(4096u32 * 8).to_le_bytes());
    wav.extend_from_slice(&(1i32 << 28).to_le_bytes().repeat(4096 * 2));
    std::fs::write(&source, wav).unwrap();
    let frames = Arc::new(AtomicU64::new(0));
    let allowed = Arc::new(AtomicU64::new(512));
    let samples = Arc::new(Mutex::new(Vec::new()));
    let opens = Arc::new(AtomicU64::new(0));
    let (f, a, c, o) = (
        frames.clone(),
        allowed.clone(),
        samples.clone(),
        opens.clone(),
    );
    let log = Observer::new(&root).unwrap();
    let mut playback = Playback::with_sink(
        log.clone(),
        root.clone(),
        Box::new(move |_, _, _| {
            o.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(VolumeTestSink {
                captured: c.clone(),
                allowed: a.clone(),
                frames: f.clone(),
            }))
        }),
    )
    .unwrap();
    playback
        .load(
            Track {
                source_id: "internal".into(),
                path: source,
                ..Default::default()
            },
            vec![],
            0,
            spec(AudioOutput::Bluetooth("test-peer".into()), 44_100),
            81,
            DspConfig::with_volume(0),
            81,
        )
        .unwrap();
    let mut started = 0;
    for (limit, volume) in [(512, 100), (1024, 0), (1536, 25), (2048, 100)] {
        let deadline = Instant::now() + Duration::from_secs(3);
        while frames.load(Ordering::Acquire) < limit {
            for event in playback.events.try_iter() {
                match event {
                    PlaybackEvent::Core(Event::TrackStarted { generation: 81 }) => started += 1,
                    PlaybackEvent::Core(Event::PlaybackError { message, .. }) => {
                        panic!("{message}")
                    }
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline, "PCM progress stopped");
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(playback.epoch.load(Ordering::Acquire), 81);
        playback.set_volume(volume);
        allowed.store(
            if limit == 2048 { 4096 } else { limit + 512 },
            Ordering::Release,
        );
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match playback.events.recv_timeout(Duration::from_millis(50)) {
            Ok(PlaybackEvent::Core(Event::TrackStarted { generation: 81 })) => started += 1,
            Ok(PlaybackEvent::Core(Event::TrackEnded { generation: 81 })) => break,
            Ok(PlaybackEvent::Core(Event::PlaybackError { message, .. })) => {
                panic!("{message}")
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "stream did not finish");
    }
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    assert_eq!(started, 1);
    assert_eq!(frames.load(Ordering::Acquire), 4096);
    let samples = samples.lock().unwrap();
    assert!(samples[..512].iter().all(|s| *s == 0));
    // A volume update can race one already rendered 73-frame partial
    // write. After that write plus the five-millisecond ramp, the exact
    // target must hold; this bound is independent of host scheduling.
    let settled_after = 73 + 44_100 / 200;
    assert!(samples[512 + settled_after..1024]
        .iter()
        .all(|s| *s == 1 << 28));
    assert!(samples[1024 + settled_after..1536].iter().all(|s| *s == 0));
    let quarter = (f64::from(1 << 28) * reborn_core::volume_gain(25)).round() as i32;
    assert!(samples[1536 + settled_after..2048]
        .iter()
        .all(|s| *s == quarter));
    assert!(samples[2048 + settled_after..]
        .iter()
        .all(|s| *s == 1 << 28));
    drop(samples);
    playback.shutdown(82).unwrap();
}

#[test]
fn boundary_event_precedes_its_queue_entry_artwork() {
    let (tx, rx) = sync_channel(4);
    let entry_id = QueueEntryId(22);
    emit(
        &tx,
        Event::TrackBoundary {
            generation: 7,
            next_entry_id: Some(entry_id),
            next_track_id: Some(4),
            output_position_ms: 0,
        },
    );
    emit_artwork(&tx, 7, entry_id, 4, vec![1, 2, 3]);

    assert!(matches!(
        rx.recv().unwrap(),
        PlaybackEvent::Core(Event::TrackBoundary {
            next_entry_id: Some(id),
            ..
        }) if id == entry_id
    ));
    assert!(matches!(
        rx.recv().unwrap(),
        PlaybackEvent::Artwork {
            entry_id: id,
            track_id: 4,
            bytes,
            ..
        } if id == entry_id && bytes == [1, 2, 3]
    ));
}
fn indexed_pcm(first: usize, frames: usize, rate: u32) -> Pcm {
    let mut data = Vec::with_capacity(frames * 8);
    for frame in first..first + frames {
        let sample = frame as i32;
        data.extend_from_slice(&sample.to_le_bytes());
        data.extend_from_slice(&(-sample).to_le_bytes());
    }
    Pcm {
        data,
        position_ms: first as u64 * 1000 / u64::from(rate),
        packets: 0,
        frames: frames as u64,
        rate,
        format: PcmFormat::S32LE,
    }
}
fn indexed_samples(pcm: &Pcm) -> Vec<i32> {
    pcm.data
        .as_chunks::<8>()
        .0
        .iter()
        .map(|frame| i32::from_le_bytes(frame[..4].try_into().unwrap()))
        .collect()
}

#[test]
fn partial_crossfade_windows_preserve_consumed_input_at_all_durations() {
    let rate = 1000;
    for seconds in [5usize, 10, 15] {
        let wanted = seconds * rate as usize;
        let near_end_a = wanted / 2;
        let near_end_b = wanted / 3;
        let cases = [
            Vec::new(),
            vec![1],
            vec![3, 7, 13],
            vec![near_end_a, near_end_b, wanted - 1 - near_end_a - near_end_b],
        ];
        for block_sizes in cases {
            let consumed: usize = block_sizes.iter().sum();
            let mut next = 0;
            let mut blocks = VecDeque::new();
            for size in block_sizes {
                blocks.push_back(indexed_pcm(next, size, rate));
                next += size;
            }
            let mut pending = VecDeque::new();
            let result = read_window_with(&mut pending, wanted, || {
                blocks
                    .pop_front()
                    .map(|block| Ok(Some(block)))
                    .unwrap_or_else(|| Err("injected decoder read failure".into()))
            });
            let failure = match result {
                Err(failure) => failure,
                Ok(_) => panic!("decoder failure after each injected prefix"),
            };

            assert_eq!(failure.error, "injected decoder read failure");
            match (consumed, failure.partial) {
                (0, None) => {}
                (count, Some(partial)) => {
                    assert_eq!(partial.frames, count as u64);
                    assert_eq!(
                        indexed_samples(&partial),
                        (0..count as i32).collect::<Vec<_>>()
                    );
                    assert_eq!(partial.position_ms, 0);
                }
                _ => panic!("partial consumed PCM was not returned"),
            }
        }
    }
}

#[test]
fn failed_window_recovery_keeps_old_suffix_before_next_prefix() {
    let mut tail = TailWindow::new(1000, PcmFormat::S32LE);
    assert!(tail.push(indexed_pcm(1000, 4, 1000), 4).is_none());
    let failure = WindowReadFailure {
        partial: Some(indexed_pcm(0, 3, 1000)),
        error: "injected read failure".into(),
    };

    let recovered = recover_window_failure(&mut tail, failure);

    assert_eq!(recovered.len(), 2);
    assert_eq!(indexed_samples(&recovered[0]), vec![1000, 1001, 1002, 1003]);
    assert_eq!(indexed_samples(&recovered[1]), vec![0, 1, 2]);
}

#[test]
fn partial_window_recovery_is_chunked_under_sink_write_limit() {
    let root = std::env::temp_dir().join(format!("reborn-window-chunks-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let (tx, rx) = sync_channel(4);
    let buffered = AtomicU64::new(0);
    let epoch = AtomicU64::new(9);
    let frames = MAX_SINK_WRITE_BYTES / 8 + 11;
    let input = indexed_pcm(0, frames, 1000);
    let input_samples = indexed_samples(&input);
    let recovered = recover_window_failure(
        &mut TailWindow::new(1000, PcmFormat::S32LE),
        WindowReadFailure {
            partial: Some(input),
            error: "injected read failure".into(),
        },
    );
    for block in recovered {
        send_stream(
            &tx,
            &buffered,
            &epoch,
            9,
            Stream::Pcm {
                generation: 9,
                block,
            },
            frames as u64,
            &log,
        )
        .unwrap();
    }
    let mut output = Vec::new();
    while let Ok(Stream::Pcm { block, .. }) = rx.try_recv() {
        assert!(block.data.len() <= MAX_SINK_WRITE_BYTES);
        output.extend(indexed_samples(&block));
    }
    assert_eq!(output, input_samples);
}
#[test]
fn async_open_failure_is_reported_without_waiting_or_starting_decode() {
    let root = std::env::temp_dir().join(format!(
        "reborn-open-failure-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let log = Observer::new(&root).unwrap();
    let (entered_tx, entered_rx) = sync_channel(1);
    let (release_tx, release_rx) = sync_channel(1);
    let mut playback = Playback::with_sink(
        log,
        root,
        Box::new(move |_, _, _| {
            let _ = entered_tx.try_send(());
            let _ = release_rx.recv();
            Err("injected sink open failure".into())
        }),
    )
    .unwrap();
    playback.stop_and_wait(2).unwrap();
    playback
        .begin_start_with_gapless(LoadRequest {
            track: track(),
            music_root: track().path.parent().unwrap().into(),
            entry_id: QueueEntryId(1),
            queue: vec![],
            queue_entry_ids: vec![],
            position: 0,
            spec: spec(AudioOutput::Wired, 44_100),
            generation: 2,
            dsp: DspConfig::with_volume(35),
            gapless_enabled: true,
            id: 1,
            fade_in: false,
        })
        .unwrap();
    // The opener is still held behind `release_rx`; reaching this point
    // proves begin_start returned without waiting for the worker result.
    assert!(entered_rx.recv_timeout(Duration::from_millis(100)).is_ok());
    assert!(playback.has_pending_start());
    assert!(playback.poll_start().is_none());
    release_tx.try_send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    let error = loop {
        if let Some(result) = playback.poll_start() {
            break result.unwrap_err();
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    };
    assert!(error.contains("injected sink open failure"));
    assert!(playback.events.try_recv().is_err());
    playback.shutdown(3).unwrap();
}

#[test]
fn stop_send_deadline_covers_a_full_command_queue() {
    let root = std::env::temp_dir().join(format!(
        "reborn-stop-full-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let log = Observer::new(&root).unwrap();
    let (sink, sink_rx) = sync_channel(1);
    sink.try_send(SinkCommand::Stop(None)).unwrap();
    let (decode, _decode_rx) = sync_channel(1);
    let (event_sender, events) = sync_channel(64);
    let mut playback = Playback {
        decode,
        sink,
        events,
        #[cfg(test)]
        event_sender,
        epoch: Arc::new(AtomicU64::new(0)),
        volume: Arc::new(AtomicU8::new(100)),
        balance: Arc::new(AtomicI8::new(0)),
        hold: Arc::new(AtomicBool::new(false)),
        upcoming: Arc::new(Mutex::new(None)),
        cancel: None,
        pending_start: None,
        sink_released: false,
        state: Arc::new(Mutex::new(json!({}))),
        observer: log,
        levels: Default::default(),
    };
    let start = Instant::now();
    let error = playback
        .stop_and_wait_with_timeout(1, Duration::from_millis(40))
        .unwrap_err();

    assert!(error.contains("deadline"));
    assert!(start.elapsed() < Duration::from_millis(250));
    drop(sink_rx);
}

#[test]
fn stop_with_unavailable_worker_fails_without_blocking() {
    let root = std::env::temp_dir().join(format!("reborn-stop-gone-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let (sink, sink_rx) = sync_channel(1);
    drop(sink_rx);
    let (decode, _decode_rx) = sync_channel(1);
    let (event_sender, events) = sync_channel(64);
    let mut playback = Playback {
        decode,
        sink,
        events,
        #[cfg(test)]
        event_sender,
        epoch: Arc::new(AtomicU64::new(0)),
        volume: Arc::new(AtomicU8::new(100)),
        balance: Arc::new(AtomicI8::new(0)),
        hold: Arc::new(AtomicBool::new(false)),
        upcoming: Arc::new(Mutex::new(None)),
        cancel: None,
        pending_start: None,
        sink_released: false,
        state: Arc::new(Mutex::new(json!({}))),
        observer: log,
        levels: Default::default(),
    };
    let start = Instant::now();

    assert_eq!(
        playback
            .stop_and_wait_with_timeout(1, Duration::from_secs(1))
            .unwrap_err(),
        "audio worker unavailable"
    );
    assert!(start.elapsed() < Duration::from_millis(250));
}

struct SignalingFailureSink(Arc<AtomicBool>);
impl AudioSink for SignalingFailureSink {
    fn parameters(&self) -> Parameters {
        Parameters {
            rate: 44_100,
            period: 512,
            buffer: 4096,
            format: PcmFormat::S32LE,
            channels: 2,
            hardware_mixer_gain_db: None,
            device: "test".into(),
            fallback: false,
            fallback_reason: String::new(),
            hp_level_live: None,
            level_raised: false,
        }
    }
    fn write(&mut self, _pcm: &[u8]) -> Result<usize, String> {
        self.0.store(true, Ordering::Release);
        Err("injected sink write failure".into())
    }
    fn discard(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn delay(&self) -> u64 {
        0
    }
}

#[test]
fn full_event_queue_cannot_extend_the_release_deadline() {
    let root = std::env::temp_dir().join(format!("reborn-events-full-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let attempted = Arc::new(AtomicBool::new(false));
    let sink_attempted = attempted.clone();
    let mut playback = Playback::with_sink(
        log,
        root,
        Box::new(move |_, _, _| Ok(Box::new(SignalingFailureSink(sink_attempted.clone())))),
    )
    .unwrap();
    for _ in 0..64 {
        playback
            .event_sender
            .try_send(PlaybackEvent::Core(Event::Position {
                generation: 0,
                ms: 0,
            }))
            .unwrap();
    }
    playback
        .load(
            track(),
            vec![],
            0,
            spec(AudioOutput::Wired, 44_100),
            1,
            DspConfig::with_volume(35),
            1,
        )
        .unwrap();
    let wait_until = Instant::now() + Duration::from_secs(3);
    while !attempted.load(Ordering::Acquire) {
        assert!(Instant::now() < wait_until, "sink write did not start");
        thread::sleep(Duration::from_millis(2));
    }

    let start = Instant::now();
    let error = playback
        .stop_and_wait_with_timeout(2, Duration::from_millis(40))
        .unwrap_err();
    assert!(error.contains("deadline"));
    assert!(start.elapsed() < Duration::from_millis(250));

    while playback.events.try_recv().is_ok() {}
    playback
        .stop_and_wait_with_timeout(3, Duration::from_secs(1))
        .unwrap();
    playback.shutdown(4).unwrap();
}

/// Pause holds the open pipeline: the device is paused, nothing is
/// written, and resume continues the same stream without a reopen.
#[test]
fn a_held_pause_stops_writing_and_resumes_without_reopening() {
    struct HoldSink {
        frames: Arc<AtomicU64>,
        pauses: Arc<Mutex<Vec<bool>>>,
    }
    impl AudioSink for HoldSink {
        fn parameters(&self) -> Parameters {
            Parameters {
                rate: 44100,
                period: 512,
                buffer: 4096,
                format: PcmFormat::S32LE,
                channels: 2,
                hardware_mixer_gain_db: None,
                device: "test".into(),
                fallback: false,
                fallback_reason: String::new(),
                hp_level_live: None,
                level_raised: false,
            }
        }
        fn write(&mut self, s: &[u8]) -> Result<usize, String> {
            // Real time: 128 frames take ~2.9 ms at 44.1 kHz.
            thread::sleep(Duration::from_millis(3));
            let n = (s.len() / 8).min(128);
            self.frames.fetch_add(n as u64, Ordering::Relaxed);
            Ok(n)
        }
        fn discard(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn delay(&self) -> u64 {
            0
        }
        fn pause(&mut self, on: bool) -> Result<(), String> {
            self.pauses.lock().unwrap().push(on);
            Ok(())
        }
    }
    let frames = Arc::new(AtomicU64::new(0));
    let pauses = Arc::new(Mutex::new(vec![]));
    let opened = Arc::new(AtomicU64::new(0));
    let (f, ps, o) = (frames.clone(), pauses.clone(), opened.clone());
    let root = std::env::temp_dir().join(format!("reborn-hold-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let mut p = Playback::with_sink(
        log,
        root,
        Box::new(move |_, _, _| {
            o.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(HoldSink {
                frames: f.clone(),
                pauses: ps.clone(),
            }))
        }),
    )
    .unwrap();
    p.load(
        track(),
        vec![],
        0,
        spec(AudioOutput::Wired, 44100),
        1,
        DspConfig::with_volume(80),
        1,
    )
    .unwrap();
    let wait_for = |what: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !what() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        what()
    };
    assert!(
        wait_for(&|| frames.load(Ordering::Relaxed) > 2_000),
        "playback started"
    );
    p.hold(true);
    assert!(p.is_held());
    assert!(
        wait_for(&|| pauses.lock().unwrap().as_slice() == [true]),
        "device paused"
    );
    let at_pause = frames.load(Ordering::Relaxed);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        frames.load(Ordering::Relaxed),
        at_pause,
        "nothing written while held"
    );
    p.hold(false);
    assert!(
        wait_for(&|| frames.load(Ordering::Relaxed) > at_pause + 2_000),
        "continues"
    );
    assert_eq!(pauses.lock().unwrap().as_slice(), [true, false]);
    assert_eq!(
        opened.load(Ordering::Relaxed),
        1,
        "the device was never reopened"
    );
    // Any stop ends the hold, so a later Play cannot resume a stopped pipeline.
    p.hold(true);
    p.stop(2);
    assert!(!p.is_held());
}

#[test]
fn stop_invalidates_pending_pcm_and_output_switch_reopens() {
    let frames = Arc::new(AtomicU64::new(0));
    let count = frames.clone();
    let opened = Arc::new(AtomicU64::new(0));
    let calls = opened.clone();
    let root = std::env::temp_dir().join(format!("reborn-pipeline-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let mut p = Playback::with_sink(
        log,
        root,
        Box::new(move |requested, _, _| {
            let previous = calls.fetch_add(1, Ordering::Relaxed);
            let rate = requested.rate;
            assert_eq!(rate, if previous == 0 { 44100 } else { 48000 });
            Ok(Box::new(Sink {
                frames: count.clone(),
                fail: false,
                rate,
                samples: None,
            }))
        }),
    )
    .unwrap();
    p.load(
        track(),
        vec![],
        0,
        spec(AudioOutput::Wired, 44100),
        1,
        DspConfig::with_volume(35),
        1,
    )
    .unwrap();
    let second_open_without_release = p
        .start_with_gapless(LoadRequest {
            track: track(),
            music_root: track().path.parent().unwrap().into(),
            entry_id: QueueEntryId(2),
            queue: vec![],
            queue_entry_ids: vec![],
            position: 0,
            spec: spec(AudioOutput::Bluetooth("12:34:56:78:90:AB".into()), 48_000),
            generation: 2,
            dsp: DspConfig::with_volume(35),
            gapless_enabled: true,
            id: 2,
            fade_in: false,
        })
        .unwrap_err();
    assert!(second_open_without_release.contains("release acknowledgement"));
    assert_eq!(opened.load(Ordering::Relaxed), 1);
    thread::sleep(Duration::from_millis(60));
    assert!(frames.load(Ordering::Relaxed) > 0);
    p.stop(2);
    thread::sleep(Duration::from_millis(40));
    let before = frames.load(Ordering::Relaxed);
    thread::sleep(Duration::from_millis(40));
    assert_eq!(before, frames.load(Ordering::Relaxed));
    while p.events.try_recv().is_ok() {}
    p.load(
        track(),
        vec![],
        500,
        spec(AudioOutput::Bluetooth("12:34:56:78:90:AB".into()), 48000),
        3,
        DspConfig::with_volume(35),
        2,
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(PlaybackEvent::Core(Event::TrackStarted { generation })) =
            p.events.recv_timeout(Duration::from_millis(100))
        {
            assert_eq!(generation, 3);
            break;
        }
        assert!(Instant::now() < until)
    }
    assert_eq!(opened.load(Ordering::Relaxed), 2);
    p.stop(4);
}
#[test]
fn a_kept_sink_plays_on_only_for_exactly_its_spec_and_wired_settings() {
    let wired = spec(AudioOutput::Wired, 44_100);
    assert_eq!(sink_change(&wired, 3, &wired.clone(), 3), None);
    assert_eq!(
        sink_change(&wired, 3, &spec(AudioOutput::Wired, 48_000), 3),
        Some("rate")
    );
    let mut s24 = wired.clone();
    s24.format = PcmFormat::S24LE;
    assert_eq!(sink_change(&wired, 3, &s24, 3), Some("format"));
    let bluetooth = spec(AudioOutput::Bluetooth("12:34:56:78:90:AB".into()), 44_100);
    assert_eq!(sink_change(&wired, 3, &bluetooth, 3), Some("output"));
    // A Bluetooth sink is never kept, even for the same transport.
    assert_eq!(
        sink_change(&bluetooth, 3, &bluetooth.clone(), 3),
        Some("output")
    );
    let mut other_device = wired.clone();
    other_device.device = "other".into();
    assert_eq!(sink_change(&wired, 3, &other_device, 3), Some("spec"));
    // A new headphone level or DAC filter needs the amplifier powered up.
    assert_eq!(
        sink_change(&wired, 3, &wired.clone(), 4),
        Some("wired_settings")
    );
}
/// A track change keeps the wired sink open when the next track wants
/// the same one (#52): its queued audio is dropped and nothing reopens.
/// Another rate or output opens a new sink after the kept one closed; a
/// kept sink no stream asks for closes on its own.
#[test]
fn consecutive_tracks_with_one_wired_spec_open_the_sink_once() {
    struct KeptTestSink {
        open: Arc<AtomicU64>,
        discards: Arc<AtomicU64>,
        rate: u32,
    }
    impl AudioSink for KeptTestSink {
        fn parameters(&self) -> Parameters {
            Parameters {
                rate: self.rate,
                period: 512,
                buffer: 4096,
                format: PcmFormat::S32LE,
                channels: 2,
                device: "test".into(),
                ..Default::default()
            }
        }
        fn write(&mut self, s: &[u8]) -> Result<usize, String> {
            thread::sleep(Duration::from_millis(2));
            Ok((s.len() / 8).min(128))
        }
        fn discard(&mut self) -> Result<(), String> {
            self.discards.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        fn delay(&self) -> u64 {
            0
        }
    }
    impl Drop for KeptTestSink {
        fn drop(&mut self) {
            self.open.fetch_sub(1, Ordering::AcqRel);
        }
    }
    let opens = Arc::new(AtomicU64::new(0));
    let open = Arc::new(AtomicU64::new(0));
    let peak = Arc::new(AtomicU64::new(0));
    let discards = Arc::new(AtomicU64::new(0));
    let (factory_opens, factory_open, factory_peak, factory_discards) =
        (opens.clone(), open.clone(), peak.clone(), discards.clone());
    let root = std::env::temp_dir().join(format!("reborn-kept-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let mut p = Playback::with_sink(
        log,
        root,
        Box::new(move |requested, _, _| {
            factory_opens.fetch_add(1, Ordering::Relaxed);
            let now = factory_open.fetch_add(1, Ordering::AcqRel) + 1;
            factory_peak.fetch_max(now, Ordering::AcqRel);
            Ok(Box::new(KeptTestSink {
                open: factory_open.clone(),
                discards: factory_discards.clone(),
                rate: requested.rate,
            }))
        }),
    )
    .unwrap();
    let start = |p: &mut Playback, generation: u64, spec: SinkSpec| {
        let kept = p.switch_and_wait(generation).unwrap();
        p.start_with_gapless(LoadRequest {
            track: track(),
            music_root: track().path.parent().unwrap().into(),
            entry_id: QueueEntryId(generation),
            queue: vec![],
            queue_entry_ids: vec![],
            position: 0,
            spec,
            generation,
            dsp: DspConfig::with_volume(35),
            gapless_enabled: true,
            id: generation,
            fade_in: false,
        })
        .unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(PlaybackEvent::Core(Event::TrackStarted { generation: g })) =
                p.events.recv_timeout(Duration::from_millis(100))
            {
                if g == generation {
                    break;
                }
            }
            assert!(Instant::now() < until, "track {generation} never started");
        }
        kept
    };
    let wired = spec(AudioOutput::Wired, 44_100);
    assert_eq!(start(&mut p, 1, wired.clone()), None);
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    // Next: the same sink, its old audio dropped.
    assert_eq!(start(&mut p, 2, wired.clone()), Some(wired.clone()));
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    assert!(discards.load(Ordering::Relaxed) >= 1);
    assert_eq!(open.load(Ordering::Acquire), 1);
    // Another rate: the kept sink closes before the new one opens.
    assert_eq!(
        start(&mut p, 3, spec(AudioOutput::Wired, 48_000)),
        Some(wired)
    );
    assert_eq!(opens.load(Ordering::Relaxed), 2);
    assert_eq!(peak.load(Ordering::Acquire), 1);
    // Kept for a stream that never comes: it closes on its own.
    assert!(p.switch_and_wait(4).unwrap().is_some());
    assert_eq!(open.load(Ordering::Acquire), 1);
    let until = Instant::now() + KEEP_SINK_FOR + Duration::from_secs(1);
    while open.load(Ordering::Acquire) != 0 {
        assert!(Instant::now() < until, "the kept sink stayed open");
        thread::sleep(Duration::from_millis(20));
    }
    // Another output opens its own sink, which is not kept.
    let bluetooth = spec(AudioOutput::Bluetooth("12:34:56:78:90:AB".into()), 48_000);
    assert_eq!(start(&mut p, 5, bluetooth), None);
    assert_eq!(opens.load(Ordering::Relaxed), 3);
    assert_eq!(p.switch_and_wait(6).unwrap(), None);
    assert_eq!(open.load(Ordering::Acquire), 0);
    // A stop closes a kept wired sink at once.
    assert_eq!(start(&mut p, 7, spec(AudioOutput::Wired, 48_000)), None);
    p.stop_and_wait(8).unwrap();
    assert_eq!(open.load(Ordering::Acquire), 0);
    assert_eq!(opens.load(Ordering::Relaxed), 4);
    p.shutdown(9).unwrap();
}
#[test]
fn decoder_and_sink_agree_on_both_baseline_rates() {
    for rate in [44100, 48000] {
        let frames = Arc::new(AtomicU64::new(0));
        let count = frames.clone();
        let root = std::env::temp_dir().join(format!("reborn-rate-{}-{rate}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let mut p = Playback::with_sink(
            log,
            root,
            Box::new(move |requested, _, _| {
                assert_eq!(requested.rate, rate);
                Ok(Box::new(Sink {
                    frames: count.clone(),
                    fail: false,
                    rate,
                    samples: None,
                }))
            }),
        )
        .unwrap();
        p.load(
            track(),
            vec![],
            0,
            spec(AudioOutput::Wired, rate),
            1,
            DspConfig::with_volume(35),
            1,
        )
        .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(PlaybackEvent::Core(event)) =
                p.events.recv_timeout(Duration::from_millis(100))
            {
                match event {
                    Event::TrackEnded { generation: 1 } => break,
                    Event::PlaybackError { message, .. } => panic!("{message}"),
                    _ => {}
                }
            }
            assert!(Instant::now() < until, "PCM drain deadline");
        }
        assert_eq!(frames.load(Ordering::Relaxed), u64::from(rate));
        p.shutdown(2).unwrap();
    }
}
#[test]
fn sink_failure_reaches_authoritative_model() {
    let root = std::env::temp_dir().join(format!("reborn-sink-fault-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let mut p = Playback::with_sink(
        log,
        root,
        Box::new(|requested, _, _| {
            Ok(Box::new(Sink {
                frames: Arc::new(AtomicU64::new(0)),
                fail: true,
                rate: requested.rate,
                samples: None,
            }))
        }),
    )
    .unwrap();
    p.load(
        track(),
        vec![],
        0,
        spec(AudioOutput::Wired, 44100),
        9,
        DspConfig::with_volume(35),
        1,
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(PlaybackEvent::Core(Event::PlaybackError {
            generation,
            message,
        })) = p.events.recv_timeout(Duration::from_millis(100))
        {
            assert_eq!(generation, 9);
            assert!(message.contains("disconnect"));
            break;
        }
        assert!(Instant::now() < until)
    }
    p.stop(10);
}
#[test]
fn gapless_queue_is_contiguous_when_crossfade_is_off() {
    let frames = Arc::new(AtomicU64::new(0));
    let count = frames.clone();
    let samples = Arc::new(Mutex::new(Vec::new()));
    let captured = samples.clone();
    let root = std::env::temp_dir().join(format!("reborn-gapless-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let mut p = Playback::with_sink(
        log,
        root,
        Box::new(move |requested, _, _| {
            Ok(Box::new(Sink {
                frames: count.clone(),
                fail: false,
                rate: requested.rate,
                samples: Some(captured.clone()),
            }))
        }),
    )
    .unwrap();
    p.load(
        gapless_track("gapless-a.flac", 1),
        vec![gapless_track("gapless-b.flac", 2)],
        0,
        spec(AudioOutput::Wired, 44100),
        21,
        DspConfig::with_volume(100),
        21,
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    let mut boundary = false;
    loop {
        if let Ok(PlaybackEvent::Core(event)) = p.events.recv_timeout(Duration::from_millis(100)) {
            match event {
                Event::TrackBoundary { generation: 21, .. } => boundary = true,
                Event::TrackEnded { generation: 21 } => break,
                Event::PlaybackError { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        assert!(Instant::now() < until, "gapless queue drain deadline");
    }
    assert!(boundary);
    assert_eq!(frames.load(Ordering::Relaxed), 8192);
    let samples = samples.lock().unwrap();
    assert_eq!(samples.len(), 8192);
    let previous_step = samples[4095] - samples[4094];
    let boundary_step = samples[4096] - samples[4095];
    assert!(previous_step > 0);
    assert!((boundary_step - previous_step).abs() <= 2);
    p.shutdown(22).unwrap();
}
/// A queue edit while a song plays (Beta 8.33): the song after it
/// changes without the song starting over.
#[test]
fn a_new_upcoming_list_changes_the_next_song_without_a_restart() {
    let frames = Arc::new(AtomicU64::new(0));
    let count = frames.clone();
    let root = std::env::temp_dir().join(format!("reborn-upcoming-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let mut p = Playback::with_sink(
        log,
        root,
        Box::new(move |requested, _, _| {
            Ok(Box::new(Sink {
                frames: count.clone(),
                fail: false,
                rate: requested.rate,
                samples: None,
            }))
        }),
    )
    .unwrap();
    // Waiting for job 31 before it starts: taken at its first block.
    p.set_upcoming(
        31,
        vec![gapless_track("gapless-a.flac", 3)],
        vec![QueueEntryId(7)],
    );
    p.load(
        gapless_track("gapless-a.flac", 1),
        vec![gapless_track("gapless-b.flac", 2)],
        0,
        spec(AudioOutput::Wired, 44100),
        31,
        DspConfig::with_volume(100),
        31,
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    let mut boundaries = vec![];
    loop {
        if let Ok(PlaybackEvent::Core(event)) = p.events.recv_timeout(Duration::from_millis(100)) {
            match event {
                Event::TrackBoundary {
                    generation: 31,
                    next_entry_id,
                    next_track_id,
                    ..
                } => boundaries.push((next_entry_id, next_track_id)),
                Event::TrackEnded { generation: 31 } => break,
                Event::PlaybackError { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        assert!(Instant::now() < until, "upcoming drain deadline");
    }
    assert_eq!(
        boundaries,
        [(Some(QueueEntryId(7)), Some(3))],
        "the new next song, not the one the job started with"
    );
    assert_eq!(
        frames.load(Ordering::Relaxed),
        8192,
        "both songs whole, once"
    );
    // Another job's list is never taken.
    p.set_upcoming(99, vec![], vec![]);
    p.shutdown(32).unwrap();
}
#[test]
fn failed_next_track_is_skipped_to_the_following_decodable_track() {
    let frames = Arc::new(AtomicU64::new(0));
    let count = frames.clone();
    let root = std::env::temp_dir().join(format!("reborn-skip-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let mut p = Playback::with_sink(
        log,
        root,
        Box::new(move |requested, _, _| {
            Ok(Box::new(Sink {
                frames: count.clone(),
                fail: false,
                rate: requested.rate,
                samples: None,
            }))
        }),
    )
    .unwrap();
    let mut missing = gapless_track("missing.flac", 2);
    missing.path = PathBuf::from("/missing/reborn-next-track.flac");
    p.load(
        gapless_track("gapless-a.flac", 1),
        vec![missing, gapless_track("gapless-b.flac", 3)],
        0,
        spec(AudioOutput::Wired, 44100),
        41,
        DspConfig::with_volume(100),
        41,
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    let mut skipped_to = None;
    loop {
        if let Ok(PlaybackEvent::Core(event)) = p.events.recv_timeout(Duration::from_millis(100)) {
            match event {
                Event::TrackBoundary {
                    generation: 41,
                    next_track_id,
                    ..
                } => skipped_to = next_track_id,
                Event::TrackEnded { generation: 41 } => break,
                Event::PlaybackError { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        assert!(Instant::now() < until, "skip queue drain deadline");
    }
    assert_eq!(skipped_to, Some(3));
    assert_eq!(frames.load(Ordering::Relaxed), 8192);
    p.shutdown(42).unwrap();
}
#[test]
fn crossfade_is_separate_from_gapless_and_reduces_the_overlap_window() {
    let frames = Arc::new(AtomicU64::new(0));
    let count = frames.clone();
    let root = std::env::temp_dir().join(format!("reborn-crossfade-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let mut p = Playback::with_sink(
        log,
        root,
        Box::new(move |requested, _, _| {
            Ok(Box::new(Sink {
                frames: count.clone(),
                fail: false,
                rate: requested.rate,
                samples: None,
            }))
        }),
    )
    .unwrap();
    p.load(
        gapless_track("gapless-a.flac", 1),
        vec![gapless_track("gapless-b.flac", 2)],
        0,
        spec(AudioOutput::Wired, 44100),
        31,
        DspConfig {
            volume: 100,
            crossfade_ms: 50,
            ..Default::default()
        },
        31,
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    let mut boundary = false;
    loop {
        if let Ok(PlaybackEvent::Core(event)) = p.events.recv_timeout(Duration::from_millis(100)) {
            match event {
                Event::TrackBoundary { generation: 31, .. } => boundary = true,
                Event::TrackEnded { generation: 31 } => break,
                Event::PlaybackError { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        assert!(Instant::now() < until, "crossfade queue drain deadline");
    }
    assert!(boundary);
    assert_eq!(frames.load(Ordering::Relaxed), 5987);
    p.shutdown(32).unwrap();
}

/// Live end-to-end check (needs the internet): a real station through
/// the fetcher and the stream decoder, as playback opens it.
#[test]
#[ignore]
fn live_radio_station_decodes() {
    let directory = reborn_platform::radio::Directory::discover();
    let popular = directory.popular().expect("directory reachable");
    for codec in ["MP3", "AAC"] {
        let station = popular
            .iter()
            .find(|s| s.codec == codec)
            .expect("station")
            .clone();
        let track = reborn_core::Track::radio(&station);
        let spec = OutputSpec {
            rate: 48_000,
            format: PcmFormat::S16LE,
        };
        let mut decoder = BoundDecoder::open(
            &track,
            std::path::Path::new("/"),
            spec,
            DspConfig::with_volume(100),
            Cancel::new().unwrap(),
        )
        .unwrap();
        let (mut frames, mut energy, mut peak) = (0u64, 0f64, 0i32);
        while frames < 48_000 * 3 {
            let pcm = decoder.read().unwrap().expect("live stream does not end");
            frames += pcm.frames;
            for sample in pcm.data.as_chunks::<2>().0 {
                let v = i16::from_le_bytes(*sample) as i32;
                energy += (v * v) as f64;
                peak = peak.max(v.abs());
            }
        }
        let rms = (energy / (frames * 2) as f64).sqrt();
        eprintln!(
            "{codec}: {} decoded 3 s, rms {rms:.0}, peak {peak}",
            station.name
        );
        assert!(rms > 100.0, "decoded audio is silent");
    }
}
