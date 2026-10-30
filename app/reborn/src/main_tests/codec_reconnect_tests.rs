use super::*;
fn status() -> bluetooth::Status {
    let mut status = bluetooth::Status::default();
    status.available = true;
    status.powered = true;
    status.bluealsa = true;
    status.devices.push(bluetooth::Device {
        address: "AA".into(),
        path: "/peer".into(),
        connected: true,
        audio: true,
        paired: true,
        bonded: true,
        ..Default::default()
    });
    status.pcms.push(reborn_core::BluetoothPcm {
        device: "/peer".into(),
        transport: "A2DP-source".into(),
        mode: "sink".into(),
        running: Some(false),
        transport_generation: 1,
        ..Default::default()
    });
    status
}
#[test]
fn saved_codec_retries_only_on_real_reconnect_and_never_during_playback() {
    let mut guard = CodecReconnect::default();
    let mut s = status();
    assert!(guard.next(&s, PlaybackState::Playing, false).is_none());
    assert!(guard.next(&s, PlaybackState::Paused, true).is_none());
    s.pcms[0].running = None;
    assert!(guard.next(&s, PlaybackState::Paused, false).is_none());
    s.pcms[0].running = Some(false);
    assert_eq!(
        guard.next(&s, PlaybackState::Paused, false).as_deref(),
        Some("AA")
    );
    // Queue-full does not consume the sole attempt; caller confirms send.
    assert_eq!(
        guard.next(&s, PlaybackState::Paused, false).as_deref(),
        Some("AA")
    );
    guard.sent("AA".into());
    s.pcms[0].transport_generation = 2; // Codec selection may recreate PCM.
    assert!(guard.next(&s, PlaybackState::Paused, false).is_none());
    s.available = false;
    s.devices[0].connected = false;
    assert!(guard.next(&s, PlaybackState::Paused, false).is_none());
    s.available = true;
    s.devices[0].connected = true; // Failed snapshot did not prove a disconnect.
    assert!(guard.next(&s, PlaybackState::Paused, false).is_none());
    s.devices[0].connected = false;
    assert!(guard.next(&s, PlaybackState::Paused, false).is_none());
    s.devices[0].connected = true;
    assert_eq!(
        guard.next(&s, PlaybackState::Stopped, false).as_deref(),
        Some("AA")
    );
}
#[test]
fn prepared_sleep_allows_restore_and_observation_and_refuses_mutations() {
    for command in [
        Command::Status,
        Command::Audio,
        Command::Health,
        Command::Metrics,
        Command::Snapshot,
        Command::SleepRestore { dark: false },
        Command::SleepRestore { dark: true },
    ] {
        assert!(sleep_control_allowed(&command));
    }
    for command in [
        Command::SleepPrepare,
        Command::Scan,
        Command::Diagnose,
        Command::Tests,
    ] {
        assert!(!sleep_control_allowed(&command));
    }
}
