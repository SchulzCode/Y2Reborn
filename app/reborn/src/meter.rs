//! What the music sounds like now, for the playing wave (Beta 8.38): its
//! loudness in three bands, measured from the samples the output is given,
//! before the volume (the wave follows the music, not the volume), and timed
//! by the output's delay so it moves with what is heard (Bluetooth lags by a
//! fifth of a second). Three one-pole filters per frame: no spectrum.
use reborn_core::PcmFormat;
use reborn_ui::Levels;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Levels measured, each with when it is heard.
pub(crate) type Shared = Arc<Mutex<VecDeque<(Instant, Levels)>>>;

/// Measurements kept ahead of the ear (Bluetooth: ≈ 40 blocks).
const KEPT: usize = 96;
/// Newer than this or the music stopped: no levels (the wave lies calm).
const STALE: Duration = Duration::from_millis(300);
/// Where bass and highs part from the mids (Hz).
const BASS_HZ: f32 = 200.;
const HIGH_HZ: f32 = 3000.;

pub(crate) struct Meter {
    shared: Shared,
    rate: u32,
    bass_a: f32,
    high_a: f32,
    low: f32,
    split: f32,
}

impl Meter {
    pub(crate) fn new(shared: Shared) -> Self {
        Self {
            shared,
            rate: 0,
            bass_a: 0.,
            high_a: 0.,
            low: 0.,
            split: 0.,
        }
    }

    /// Measure frames just written; they are heard `delay` frames from now.
    pub(crate) fn measure(&mut self, data: &[u8], format: PcmFormat, rate: u32, delay: u64) {
        if rate == 0 || data.is_empty() {
            return;
        }
        if rate != self.rate {
            let pole = |hz: f32| 1. - (-std::f32::consts::TAU * hz / rate as f32).exp();
            (self.rate, self.bass_a, self.high_a) = (rate, pole(BASS_HZ), pole(HIGH_HZ));
        }
        let (mut bass, mut mid, mut high, mut frames) = (0f32, 0f32, 0f32, 0f32);
        for frame in data.chunks_exact(format.bytes_per_frame()) {
            let mut sum = 0.;
            for sample in frame.chunks_exact(format.bytes_per_sample()) {
                sum += match format {
                    PcmFormat::S16LE => {
                        f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32_768.
                    }
                    PcmFormat::S24LE => {
                        let v = i32::from_le_bytes(sample.try_into().unwrap_or_default());
                        ((v << 8) >> 8) as f32 / 8_388_608.
                    }
                    PcmFormat::S32LE => {
                        i32::from_le_bytes(sample.try_into().unwrap_or_default()) as f32
                            / 2_147_483_648.
                    }
                };
            }
            let x = sum / 2.;
            self.low += self.bass_a * (x - self.low);
            self.split += self.high_a * (x - self.split);
            bass += self.low * self.low;
            mid += (self.split - self.low) * (self.split - self.low);
            high += (x - self.split) * (x - self.split);
            frames += 1.;
        }
        // Loudness from quiet to loud as 0..1; the highs carry less energy.
        let level = |power: f32, floor: f32| {
            let db = 10. * (power / frames).max(1e-12).log10();
            ((db - floor) / 36.).clamp(0., 1.)
        };
        let levels = Levels {
            bass: level(bass, -45.),
            mid: level(mid, -45.),
            high: level(high, -54.),
        };
        let heard = Instant::now() + Duration::from_secs_f64(delay as f64 / f64::from(rate));
        if let Ok(mut kept) = self.shared.lock() {
            if kept.len() >= KEPT {
                kept.pop_front();
            }
            kept.push_back((heard, levels));
        }
    }
}

/// The levels heard now; none while nothing plays.
pub(crate) fn heard_now(shared: &Shared) -> Option<Levels> {
    let now = Instant::now();
    let mut kept = shared.lock().ok()?;
    // Keep the newest measurement already heard and those still to come.
    while kept.len() > 1 && kept[1].0 <= now {
        kept.pop_front();
    }
    let &(at, levels) = kept.front()?;
    (at <= now && now - at < STALE).then_some(levels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, amp: f32, rate: u32, frames: usize) -> Vec<u8> {
        (0..frames)
            .flat_map(|i| {
                let v = (amp
                    * (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin()
                    * 32_767.) as i16;
                [v.to_le_bytes(), v.to_le_bytes()].concat()
            })
            .collect()
    }

    #[test]
    fn bands_follow_the_music_and_loudness() {
        let shared: Shared = Default::default();
        let mut meter = Meter::new(shared.clone());
        let level = |meter: &mut Meter, hz: f32, amp: f32| {
            // Settle the filters, then measure.
            meter.measure(&tone(hz, amp, 44_100, 4096), PcmFormat::S16LE, 44_100, 0);
            meter.measure(&tone(hz, amp, 44_100, 2048), PcmFormat::S16LE, 44_100, 0);
            heard_now(&shared).unwrap()
        };
        let bass = level(&mut meter, 60., 0.5);
        assert!(bass.bass > bass.mid && bass.bass > bass.high, "{bass:?}");
        let mid = level(&mut meter, 800., 0.5);
        assert!(mid.mid > mid.bass && mid.mid > mid.high, "{mid:?}");
        let high = level(&mut meter, 8000., 0.5);
        assert!(high.high > high.bass, "{high:?}");
        // Quieter music, lower levels; silence, none at all.
        let quiet = level(&mut meter, 800., 0.02);
        assert!(quiet.mid < mid.mid);
        let silent = level(&mut meter, 800., 0.);
        assert_eq!(silent, Levels::default());
    }

    #[test]
    fn levels_wait_for_the_output_delay_and_go_stale() {
        let shared: Shared = Default::default();
        let mut meter = Meter::new(shared.clone());
        // Heard half a second from now (a Bluetooth-like delay).
        meter.measure(
            &tone(800., 0.5, 44_100, 1024),
            PcmFormat::S16LE,
            44_100,
            22_050,
        );
        assert!(heard_now(&shared).is_none(), "not heard yet");
        let past = Instant::now() - Duration::from_secs(1);
        shared.lock().unwrap().push_front((past, Levels::default()));
        assert!(heard_now(&shared).is_none(), "an old one is stale");
    }
}
