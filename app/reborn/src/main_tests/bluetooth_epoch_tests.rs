use super::{transport_epoch_state, TransportEpochState};
use crate::bluetooth::{self, Device};
use reborn_core::{AudioOutput, BluetoothPcm, PlaybackState};

fn status(generation: u64) -> bluetooth::Status {
    let mut status = bluetooth::Status::default();
    status.bluealsa = true;
    status.devices = vec![Device {
        path: "/org/bluez/hci0/dev_01_02_03_04_05_06".into(),
        address: "01:02:03:04:05:06".into(),
        connected: true,
        audio: true,
        ..Default::default()
    }];
    status.pcms = vec![BluetoothPcm {
        object: "/org/bluealsa/hci0/dev_01_02_03_04_05_06/a2dp".into(),
        device: status.devices[0].path.clone(),
        transport: "A2DP-source".into(),
        mode: "sink".into(),
        codec: Some("SBC".into()),
        format: Some(0x8210),
        rate: Some(44_100),
        channels: Some(2),
        transport_generation: generation,
        ..Default::default()
    }];
    status
}

#[test]
fn a_start_refused_for_a_missing_peer_counts_as_a_disconnect() {
    let output = AudioOutput::Bluetooth("01:02:03:04:05:06".into());
    let refused = "connected A2DP peer required";
    let mut gone = status(10);
    gone.available = true;
    gone.devices[0].connected = false;
    assert!(super::selected_peer_gone(&output, &gone, refused));
    // Still connected (audio not ready yet), another error, the jack, or no
    // observation at all: not a disconnect.
    let mut connected = status(10);
    connected.available = true;
    assert!(!super::selected_peer_gone(&output, &connected, refused));
    assert!(!super::selected_peer_gone(
        &output,
        &gone,
        "BlueALSA unavailable"
    ));
    assert!(!super::selected_peer_gone(
        &AudioOutput::Wired,
        &gone,
        refused
    ));
    gone.available = false;
    assert!(!super::selected_peer_gone(&output, &gone, refused));
    assert!(super::friendly_error(refused).starts_with("Bluetooth headphones aren't connected"));
}

#[test]
fn a_song_on_a_refused_card_does_not_ask_for_a_reinsert() {
    use reborn_core::platform::SdCard;
    let missing = "track source unavailable";
    assert!(super::refused_card_error(missing, SdCard::NeedsRepair)
        .is_some_and(|m| m.contains("needs repair")));
    assert!(super::refused_card_error(missing, SdCard::CheckTimedOut)
        .is_some_and(|m| m.contains("too long")));
    assert_eq!(super::refused_card_error(missing, SdCard::Absent), None);
    assert_eq!(
        super::refused_card_error("queue is empty", SdCard::NeedsRepair),
        None
    );
}

#[test]
fn active_sink_is_reopened_or_stopped_when_transport_epoch_is_stale() {
    let output = AudioOutput::Bluetooth("01:02:03:04:05:06".into());
    let current = status(10);
    assert_eq!(
        transport_epoch_state(&output, PlaybackState::Playing, Some(10), &current),
        TransportEpochState::Current
    );
    assert_eq!(
        transport_epoch_state(&output, PlaybackState::Playing, Some(10), &status(11)),
        TransportEpochState::Changed
    );

    let mut disappeared = status(10);
    disappeared.pcms.clear();
    assert_eq!(
        transport_epoch_state(&output, PlaybackState::Playing, Some(10), &disappeared),
        TransportEpochState::Unavailable
    );
    assert_eq!(
        transport_epoch_state(&output, PlaybackState::Paused, Some(10), &status(11)),
        TransportEpochState::Current
    );
}
