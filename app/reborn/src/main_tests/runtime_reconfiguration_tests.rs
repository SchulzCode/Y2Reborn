use super::{failed_reconfiguration_model, keeps_wired_sink, run_sink_reconfiguration};
use crate::playback::Playback;
use reborn_audio::{AudioSink, Parameters, SinkSpec};
use reborn_core::{
    AppModel, AudioOutput, EqBand, PcmFormat, PlaybackState, QueueEntryId, ReplayGainMode, Track,
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
    let wired = AudioOutput::Wired;
    assert!(keeps_wired_sink(&kept, &wired));
    // A sink some other plan gave (not the 44.1 kHz all wired plans give).
    let mut other = test_spec();
    other.rate = 48_000;
    assert!(!keeps_wired_sink(&other, &wired));
    let bluetooth = AudioOutput::Bluetooth("12:34:56:78:90:AB".into());
    assert!(!keeps_wired_sink(&kept, &bluetooth));
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
