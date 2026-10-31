//! The wheel click (#67, #77): a few milliseconds of tick on the wired output while the music is
//! paused: the stream plays the click over silence and stops again.
use crate::volume::{decode, encode};
use reborn_core::PcmFormat;
use std::{f64::consts::PI, time::Duration};

/// The click's peak at full volume, as a share of full scale; it follows the volume.
const LEVEL: f64 = 0.3;
/// How long the click sounds.
const LENGTH_MS: u32 = 4;
/// The tick's pitch and how fast it dies away.
const PITCH_HZ: f64 = 3000.0;
const DECAY_S: f64 = 0.0008;
/// A paused stream that clicked plays silence this long after the last click, then pauses again.
pub(super) const LINGER: Duration = Duration::from_secs(1);

/// The click's samples at `rate`, from -1 to 1.
fn wave(rate: u32) -> Vec<f64> {
    let frames = (rate * LENGTH_MS / 1000) as usize;
    (0..frames)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            (-t / DECAY_S).exp() * (2.0 * PI * PITCH_HZ * t).sin()
        })
        .collect()
}

fn full_scale(format: PcmFormat) -> f64 {
    match format {
        PcmFormat::S16LE => f64::from(i16::MAX),
        PcmFormat::S24LE => 8_388_607.0,
        PcmFormat::S32LE => f64::from(i32::MAX),
    }
}

/// The click sounding in the output, as far as the sink took it.
#[derive(Default)]
pub(super) struct Click {
    wave: Vec<f64>,
    rate: u32,
    at: Option<usize>,
}

impl Click {
    pub(super) fn start(&mut self, rate: u32) {
        if rate != self.rate {
            self.wave = wave(rate);
            self.rate = rate;
        }
        self.at = Some(0);
    }
    pub(super) fn stop(&mut self) {
        self.at = None;
    }
    /// Add the rest of the click to `pcm` (stereo frames) at the volume's `gain`. Like the gain's
    /// render, speculative: `advance` follows with the frames the sink took.
    pub(super) fn mix(&self, pcm: &mut [u8], format: PcmFormat, gain: f64) {
        let Some(at) = self.at else { return };
        let peak = LEVEL * gain * full_scale(format);
        let width = format.bytes_per_sample();
        let frames = pcm.chunks_exact_mut(format.bytes_per_frame());
        let mut sample = Vec::with_capacity(width);
        for (frame, value) in frames.zip(&self.wave[at..]) {
            for slot in frame.chunks_exact_mut(width) {
                sample.clear();
                encode(decode(slot, format) + value * peak, format, &mut sample);
                slot.copy_from_slice(&sample);
            }
        }
    }
    pub(super) fn advance(&mut self, frames: usize) {
        self.at = self
            .at
            .map(|at| at + frames)
            .filter(|at| *at < self.wave.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo16(values: &[i16]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|v| [*v, *v])
            .flat_map(i16::to_le_bytes)
            .collect()
    }

    #[test]
    fn a_click_rides_on_the_music_across_writes_and_follows_the_volume() {
        let mut click = Click::default();
        click.start(44_100);
        let length = click.wave.len();
        assert_eq!(length, 176);
        let music = stereo16(&[1000; 200]);
        let mut first = music.clone();
        click.mix(&mut first, PcmFormat::S16LE, 1.0);
        assert_ne!(first, music, "the click sounds over the music");
        assert_eq!(first[..4], music[..4], "it starts at zero");
        click.advance(100);
        let mut second = music.clone();
        click.mix(&mut second, PcmFormat::S16LE, 0.0);
        assert_eq!(second, music, "no volume, no click");
        let mut quiet = music.clone();
        click.mix(&mut quiet, PcmFormat::S16LE, 0.5);
        assert_ne!(quiet, music);
        assert_eq!(
            quiet[76 * 4..],
            music[76 * 4..],
            "the rest after its end is untouched"
        );
        click.advance(100);
        assert_eq!(click.at, None, "the click is over");
    }
}
