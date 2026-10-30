use super::{dsp_rise_fades, LoadedDsp};
use reborn_core::{EqBand, EqKind, ReplayGainMode};

#[test]
fn a_louder_sound_setting_fades_in_and_the_same_or_quieter_does_not() {
    let profile = reborn_core::eq::parse(
        "x",
        "Preamp: -6 dB\nFilter 1: ON LSC Fc 105 Hz Gain 6 dB Q 0.7\n",
    )
    .unwrap();
    let with = |rg, eq: Option<(f32, Vec<EqBand>)>| -> LoadedDsp { (rg, eq) };
    let on = with(
        ReplayGainMode::Off,
        Some((profile.preamp_db, profile.bands)),
    );
    let off = with(ReplayGainMode::Off, None);
    assert!(
        !dsp_rise_fades(None, &off),
        "the first load plays what was saved"
    );
    assert!(
        !dsp_rise_fades(Some(&off), &off),
        "a new song, same settings"
    );
    assert!(
        !dsp_rise_fades(Some(&off), &on),
        "a clip-safe profile is never louder"
    );
    assert!(
        dsp_rise_fades(Some(&on), &off),
        "profile off: its preamp comes back"
    );
    let bass = vec![EqBand {
        frequency_hz: 60.,
        gain_db: 0.5,
        q: 1.,
        kind: EqKind::Peak,
    }];
    let small = with(ReplayGainMode::Off, Some((0.0, bass.clone())));
    assert!(!dsp_rise_fades(Some(&off), &small), "below 1 dB");
    let mut big = bass;
    big[0].gain_db = 6.0;
    assert!(dsp_rise_fades(
        Some(&small),
        &with(ReplayGainMode::Off, Some((0.0, big)))
    ));
    // Normalize's gain depends on each song: any change counts.
    assert!(dsp_rise_fades(
        Some(&off),
        &with(ReplayGainMode::Track, None)
    ));
    assert!(dsp_rise_fades(
        Some(&with(ReplayGainMode::Album, None)),
        &off
    ));
}
