use super::*;
fn status(address: &str, path: &str) -> bluetooth::Status {
    let mut status = bluetooth::Status::default();
    status.available = true;
    status.powered = true;
    status.bluealsa = true;
    add(&mut status, address, path);
    status
}
fn add(status: &mut bluetooth::Status, address: &str, path: &str) {
    status.devices.push(bluetooth::Device {
        address: address.into(),
        path: path.into(),
        name: "AirPods Pro".into(),
        connected: true,
        audio: true,
        paired: true,
        bonded: true,
        ..Default::default()
    });
    status.pcms.push(reborn_core::BluetoothPcm {
        device: path.into(),
        transport: "A2DP-source".into(),
        mode: "sink".into(),
        running: Some(false),
        transport_generation: 1,
        ..Default::default()
    });
}
#[test]
fn play_waits_briefly_for_bluetooth_headphones_just_lost() {
    let t0 = Instant::now();
    let mut lost = LostPeer::new("AA:BB".into(), t0);
    let wired = AudioOutput::Wired;
    // Jack empty or not read yet: wait. Headphones in the jack: play there.
    assert!(lost.applies(&wired, Some(false), t0));
    assert!(lost.applies(&wired, None, t0));
    assert!(!lost.applies(&wired, Some(true), t0));
    // The headphones are the output again: nothing to wait for.
    assert!(!lost.applies(&AudioOutput::Bluetooth("AA:BB".into()), Some(false), t0));
    assert!(lost.is(&AudioOutput::Bluetooth("aa:bb".into())));
    assert!(!lost.is(&AudioOutput::Bluetooth("CC:DD".into())));
    // Long after the loss (or the wake after it) the jack plays again.
    let late = t0 + LOST_PEER_WINDOW;
    assert!(lost.expired(late));
    assert!(!lost.applies(&wired, Some(false), late));
    // A Play waits its own time, also past the window, then gives up.
    lost.play = Some(t0 + Duration::from_secs(110));
    assert!(!lost.expired(late));
    assert!(lost.applies(&wired, Some(false), late));
    let after = t0 + Duration::from_secs(110) + LOST_PEER_PLAY_WAIT;
    assert!(lost.gave_up(after));
    assert!(!lost.applies(&wired, Some(false), after));
}
#[test]
fn bluetooth_headphones_win_over_the_jack_once_per_connection() {
    let mut auto = AutoOutput::default();
    let mut s = status("AA", "/a");
    let pcm = s.pcms.clone();
    // Connected but BlueALSA has no playback PCM yet: wait.
    s.pcms.clear();
    assert!(auto.next(&s, &AudioOutput::Wired).is_none());
    s.pcms = pcm.clone();
    assert_eq!(auto.next(&s, &AudioOutput::Wired).as_deref(), Some("AA"));
    auto.routed("AA".into());
    // The owner then picks the jack by hand: that choice sticks.
    assert!(auto.next(&s, &AudioOutput::Wired).is_none());
    // A codec change recreates the PCM; still the same connection.
    s.pcms.clear();
    assert!(auto.next(&s, &AudioOutput::Wired).is_none());
    s.pcms = pcm.clone();
    assert!(auto.next(&s, &AudioOutput::Wired).is_none());
    // A failed observation proves nothing.
    s.available = false;
    s.devices[0].connected = false;
    assert!(auto.next(&s, &AudioOutput::Wired).is_none());
    s.available = true;
    s.devices[0].connected = true;
    assert!(auto.next(&s, &AudioOutput::Wired).is_none());
    // A real reconnect routes again.
    s.devices[0].connected = false;
    assert!(auto.next(&s, &AudioOutput::Wired).is_none());
    s.devices[0].connected = true;
    assert_eq!(auto.next(&s, &AudioOutput::Wired).as_deref(), Some("AA"));
    // Radio off and on again is a new connection too.
    auto.routed("AA".into());
    s.powered = false;
    assert!(auto.next(&s, &AudioOutput::Wired).is_none());
    s.powered = true;
    assert_eq!(auto.next(&s, &AudioOutput::Wired).as_deref(), Some("AA"));
}
#[test]
fn already_on_bluetooth_is_never_displaced() {
    let mut auto = AutoOutput::default();
    let mut s = status("AA", "/a");
    // Restored or chosen by hand: nothing to do, and none later.
    assert!(auto
        .next(&s, &AudioOutput::Bluetooth("aa".into()))
        .is_none());
    assert!(auto.next(&s, &AudioOutput::Wired).is_none());
    // A second peer does not take over a working first one...
    add(&mut s, "BB", "/b");
    assert!(auto
        .next(&s, &AudioOutput::Bluetooth("AA".into()))
        .is_none());
    // ...but takes over once the first is gone and the jack is back.
    s.devices[0].connected = false;
    assert_eq!(auto.next(&s, &AudioOutput::Wired).as_deref(), Some("BB"));
    // A peer without the audio sink profile is never an output.
    let mut keyboard = status("CC", "/c");
    keyboard.devices[0].audio = false;
    assert!(AutoOutput::default()
        .next(&keyboard, &AudioOutput::Wired)
        .is_none());
}
