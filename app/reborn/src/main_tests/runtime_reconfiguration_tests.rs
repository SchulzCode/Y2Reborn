use super::{
    failed_reconfiguration_model, keeps_bluetooth_sink, keeps_wired_sink, run_sink_reconfiguration,
};
use crate::playback::Playback;
use reborn_audio::{AudioSink, Parameters, SinkSpec};
use reborn_core::{
    AppModel, AudioOutput, BluetoothPcm, EqBand, PcmFormat, PlaybackState, QueueEntryId,
    ReplayGainMode, Track,
};
use reborn_media::DspConfig;
use reborn_observability::Observer;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

struct CountingSink {
    active: Arc<AtomicUsize>,
}
impl AudioSink for CountingSink {
    fn parameters(&self) -> Parameters {
        Parameters {
            rate: 44_100,
            period: 512,
            buffer: 4096,
            format: PcmFormat::S32LE,
            channels: 2,
            hardware_mixer_gain_db: None,
            device: "transaction-test".into(),
            fallback: false,
            fallback_reason: String::new(),
            hp_level_live: None,
            level_raised: false,
        }
    }
    fn write(&mut self, pcm: &[u8]) -> Result<usize, String> {
        Ok(pcm.len() / 8)
    }
    fn discard(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn delay(&self) -> u64 {
        0
    }
}
impl Drop for CountingSink {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn active_model() -> AppModel {
    let first = Track {
        id: 1,
        path: PathBuf::from("/music/first.flac"),
        ..Default::default()
    };
    let second = Track {
        id: 2,
        path: PathBuf::from("/music/second.flac"),
        ..Default::default()
    };
    let mut model = AppModel {
        playback: PlaybackState::Playing,
        queue: vec![first, second],
        queue_entry_ids: vec![QueueEntryId(11), QueueEntryId(12)],
        queue_position: 0,
        position_ms: 12_345,
        generation: 7,
        output: AudioOutput::Wired,
        ..Default::default()
    };
    model.settings.volume = 42;
    model.settings.replay_gain = ReplayGainMode::Off;
    model.settings.eq_enabled = false;
    model.settings.eq_bands = vec![EqBand::default()];
    model
}

fn test_spec() -> SinkSpec {
    SinkSpec {
        output: AudioOutput::Wired,
        rate: 44_100,
        format: PcmFormat::S32LE,
        physical_bits: 32,
        valid_bits: 32,
        channels: 2,
        layout: "stereo".into(),
        device: "transaction-test".into(),
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

#[test]
fn plan_failure_after_release_never_starts_or_keeps_the_old_sink() {
    let root = std::env::temp_dir().join(format!("reborn-plan-failure-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let open_count = Arc::new(AtomicUsize::new(0));
    let factory_count = open_count.clone();
    let peak = Arc::new(AtomicUsize::new(0));
    let factory_peak = peak.clone();
    let mut playback = Playback::with_sink(
        log,
        root,
        Box::new(move |_, _, _| {
            let active = factory_count.fetch_add(1, Ordering::AcqRel) + 1;
            factory_peak.fetch_max(active, Ordering::AcqRel);
            Ok(Box::new(CountingSink {
                active: factory_count.clone(),
            }))
        }),
    )
    .unwrap();
    playback
        .load(
            Track::default(),
            vec![],
            0,
            test_spec(),
            1,
            DspConfig::with_volume(42),
            1,
        )
        .unwrap();
    assert_eq!(open_count.load(Ordering::Acquire), 1);
    let mut started = false;
    let error = run_sink_reconfiguration(
        &mut playback,
        2,
        |_| None,
        || Err::<SinkSpec, _>("injected sink plan failure".into()),
        |_, _| {
            started = true;
            Ok(())
        },
    )
    .unwrap_err();

    assert!(error.contains("injected sink plan failure"));
    assert!(!started);
    assert_eq!(open_count.load(Ordering::Acquire), 0);
    playback.shutdown(3).unwrap();
}

#[test]
fn new_sink_opens_only_after_old_sink_release_acknowledges() {
    let root = std::env::temp_dir().join(format!("reborn-plan-start-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let factory_active = active.clone();
    let peak = Arc::new(AtomicUsize::new(0));
    let factory_peak = peak.clone();
    let mut playback = Playback::with_sink(
        log,
        root,
        Box::new(move |_, _, _| {
            let count = factory_active.fetch_add(1, Ordering::AcqRel) + 1;
            factory_peak.fetch_max(count, Ordering::AcqRel);
            Ok(Box::new(CountingSink {
                active: factory_active.clone(),
            }))
        }),
    )
    .unwrap();
    playback
        .load(
            Track::default(),
            vec![],
            0,
            test_spec(),
            1,
            DspConfig::with_volume(42),
            1,
        )
        .unwrap();
    assert_eq!(active.load(Ordering::Acquire), 1);
    let track = Track {
        id: 3,
        source_id: "internal".into(),
        path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac"),
        ..Default::default()
    };

    run_sink_reconfiguration(
        &mut playback,
        2,
        |_| None,
        || Ok(test_spec()),
        move |playback, spec| {
            playback.start_with_gapless(crate::playback::LoadRequest {
                music_root: track.path.parent().unwrap().into(),
                track,
                entry_id: QueueEntryId(3),
                queue: vec![],
                queue_entry_ids: vec![],
                position: 0,
                spec,
                generation: 2,
                dsp: DspConfig::with_volume(42),
                gapless_enabled: true,
                id: 2,
                fade_in: false,
            })
        },
    )
    .unwrap();

    assert_eq!(active.load(Ordering::Acquire), 1);
    assert_eq!(peak.load(Ordering::Acquire), 1);
    playback.shutdown(3).unwrap();
    assert_eq!(active.load(Ordering::Acquire), 0);
}

#[test]
fn a_kept_wired_sink_is_taken_only_where_the_plan_would_give_it() {
    let kept = test_spec();
    assert_eq!(kept.rate, 44_100);
    let wired = AudioOutput::Wired;
    // A 44.1 kHz-family track plays on in the 44.1 kHz sink, a 48 kHz-family one reopens it.
    assert!(keeps_wired_sink(&kept, &wired, 44_100));
    assert!(keeps_wired_sink(&kept, &wired, 88_200));
    assert!(!keeps_wired_sink(&kept, &wired, 96_000));
    // And the other way round: a 48 kHz sink keeps the 48 kHz family only.
    let mut at_48k = test_spec();
    at_48k.rate = 48_000;
    assert!(keeps_wired_sink(&at_48k, &wired, 48_000));
    assert!(keeps_wired_sink(&at_48k, &wired, 192_000));
    assert!(!keeps_wired_sink(&at_48k, &wired, 44_100));
    // A sink no wired plan gives.
    let mut other = test_spec();
    other.rate = 32_000;
    assert!(!keeps_wired_sink(&other, &wired, 32_000));
    let bluetooth = AudioOutput::Bluetooth("12:34:56:78:90:AB".into());
    assert!(!keeps_wired_sink(&kept, &bluetooth, 44_100));
}

/// F61: the next station or song plays on in the kept Bluetooth sink while
/// the headphones' observed transport is still the one it opened for.
#[test]
fn a_kept_bluetooth_sink_is_taken_only_for_the_same_observed_transport() {
    let output = AudioOutput::Bluetooth("01:02:03:04:05:06".into());
    let pcm = BluetoothPcm {
        object: "/org/bluealsa/hci0/dev_01_02_03_04_05_06/a2dp".into(),
        device: "/org/bluez/hci0/dev_01_02_03_04_05_06".into(),
        transport: "A2DP-source".into(),
        mode: "sink".into(),
        codec: Some("SBC".into()),
        format: Some(0x8210),
        rate: Some(44_100),
        channels: Some(2),
        transport_generation: 7,
        ..Default::default()
    };
    let mut kept = test_spec();
    kept.output = output.clone();
    kept.format = PcmFormat::S16LE;
    kept.physical_bits = 16;
    kept.valid_bits = 16;
    kept.codec = pcm.codec.clone();
    kept.transport_object = Some(pcm.object.clone());
    kept.transport_device = Some(pcm.device.clone());
    kept.transport = Some(pcm.transport.clone());
    kept.mode = Some(pcm.mode.clone());
    kept.transport_generation = pcm.transport_generation;
    assert!(keeps_bluetooth_sink(&kept, &output, &pcm));
    // A reconnect, another codec or another rate: a new sink.
    let reconnected = BluetoothPcm {
        transport_generation: 8,
        ..pcm.clone()
    };
    assert!(!keeps_bluetooth_sink(&kept, &output, &reconnected));
    let aac = BluetoothPcm {
        codec: Some("AAC".into()),
        ..pcm.clone()
    };
    assert!(!keeps_bluetooth_sink(&kept, &output, &aac));
    let faster = BluetoothPcm {
        rate: Some(48_000),
        ..pcm.clone()
    };
    assert!(!keeps_bluetooth_sink(&kept, &output, &faster));
    // Other headphones, or the jack.
    let other = AudioOutput::Bluetooth("0A:0B:0C:0D:0E:0F".into());
    assert!(!keeps_bluetooth_sink(&kept, &other, &pcm));
    assert!(!keeps_bluetooth_sink(&kept, &AudioOutput::Wired, &pcm));
    assert!(!keeps_bluetooth_sink(&test_spec(), &output, &pcm));
}

#[test]
fn a_track_change_for_the_same_wired_sink_starts_in_it_without_a_plan() {
    let root = std::env::temp_dir().join(format!("reborn-plan-kept-{}", std::process::id()));
    let log = Observer::new(&root).unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let opens = Arc::new(AtomicUsize::new(0));
    let (factory_active, factory_opens) = (active.clone(), opens.clone());
    let mut playback = Playback::with_sink(
        log,
        root,
        Box::new(move |_, _, _| {
            factory_opens.fetch_add(1, Ordering::AcqRel);
            factory_active.fetch_add(1, Ordering::AcqRel);
            Ok(Box::new(CountingSink {
                active: factory_active.clone(),
            }))
        }),
    )
    .unwrap();
    playback
        .load(
            Track::default(),
            vec![],
            0,
            test_spec(),
            1,
            DspConfig::with_volume(42),
            1,
        )
        .unwrap();
    let track = Track {
        id: 3,
        source_id: "internal".into(),
        path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac"),
        ..Default::default()
    };
    run_sink_reconfiguration(
        &mut playback,
        2,
        |kept| Some(kept.clone()),
        || Err::<SinkSpec, _>("the device is open: no probe".into()),
        move |playback, spec| {
            assert_eq!(spec, test_spec());
            playback.start_with_gapless(crate::playback::LoadRequest {
                music_root: track.path.parent().unwrap().into(),
                track,
                entry_id: QueueEntryId(3),
                queue: vec![],
                queue_entry_ids: vec![],
                position: 0,
                spec,
                generation: 2,
                dsp: DspConfig::with_volume(42),
                gapless_enabled: true,
                id: 2,
                fade_in: false,
            })
        },
    )
    .unwrap();

    assert_eq!(opens.load(Ordering::Acquire), 1);
    assert_eq!(active.load(Ordering::Acquire), 1);
    playback.shutdown(3).unwrap();
    assert_eq!(active.load(Ordering::Acquire), 0);
}

#[test]
fn output_seek_volume_replaygain_and_eq_failures_restore_prior_model() {
    let previous = active_model();
    let transition_generation = 10;
    let mut intended = previous.clone();
    intended.output = AudioOutput::Bluetooth("12:34:56:78:90:AB".into());
    intended.queue_position = 1;
    intended.position_ms = 54_321;
    intended.settings.volume = 80;
    intended.settings.replay_gain = ReplayGainMode::Track;
    intended.settings.eq_enabled = true;
    assert_ne!(intended.output, previous.output);
    assert_ne!(intended.settings.volume, previous.settings.volume);

    let restored = failed_reconfiguration_model(
        previous.clone(),
        "injected start failure",
        true,
        Some(transition_generation),
    );
    assert_eq!(restored.output, previous.output);
    assert_eq!(restored.settings.volume, previous.settings.volume);
    assert_eq!(restored.settings.replay_gain, previous.settings.replay_gain);
    assert_eq!(restored.settings.eq_enabled, previous.settings.eq_enabled);
    assert_eq!(restored.queue_position, previous.queue_position);
    assert_eq!(restored.position_ms, previous.position_ms);
    assert_eq!(restored.queue[0].id, previous.queue[0].id);
    assert_eq!(restored.playback, PlaybackState::Error);
    assert_eq!(
        restored.last_error.as_deref(),
        Some("injected start failure")
    );
    assert_eq!(restored.generation, transition_generation + 1);
}

#[test]
fn pre_release_failure_preserves_the_still_running_model() {
    let previous = active_model();
    let restored = failed_reconfiguration_model(
        previous.clone(),
        "transport observation unavailable",
        false,
        None,
    );

    assert_eq!(restored.playback, PlaybackState::Playing);
    assert_eq!(restored.output, previous.output);
    assert_eq!(restored.settings.volume, previous.settings.volume);
    assert_eq!(restored.generation, previous.generation);
    assert_eq!(restored.last_error, previous.last_error);
}

#[test]
fn a_rolled_back_output_change_keeps_the_library_and_book_progress() {
    // Y2Reborn #92: a Bluetooth loss while a sink opened restored the snapshot taken without the
    // library; Audiobooks vanished and the next checkpoint wrote an empty shelf.
    let mut live = active_model();
    live.library.tracks = vec![Track {
        id: 5,
        path: PathBuf::from("/music/book.m4b"),
        ..Default::default()
    }];
    live.library.scanning = false;
    let mut snapshot = live.clone();
    snapshot.library = Default::default();
    snapshot.library.scanning = true;
    snapshot.output = AudioOutput::Wired;
    live.output = AudioOutput::Bluetooth("12:34:56:78:90:AB".into());
    crate::runtime::keep_live_parts(&mut live, &mut snapshot);
    assert_eq!(snapshot.library.tracks.len(), 1);
    assert!(!snapshot.library.scanning);
    assert_eq!(snapshot.output, AudioOutput::Wired);
}
