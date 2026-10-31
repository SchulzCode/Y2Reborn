//! The wheel click (#67): a few milliseconds of tick mixed into the wired output. While music plays,
//! the queued music is taken back and written again with the click at its start, so the click
//! sounds at once; while paused, the stream plays the click over silence and pauses again.
use crate::volume::{decode, encode};
use reborn_core::PcmFormat;
use reborn_media::Pcm;
use std::{collections::VecDeque, f64::consts::PI, time::Duration};

/// The click's peak at full volume, as a share of full scale; it follows the volume.
const LEVEL: f64 = 0.3;
/// How long the click sounds.
const LENGTH_MS: u32 = 4;
/// The tick's pitch and how fast it dies away.
const PITCH_HZ: f64 = 3000.0;
const DECAY_S: f64 = 0.0008;
/// A paused stream that clicked plays silence this long after the last click, then pauses again.
pub(super) const LINGER: Duration = Duration::from_secs(1);
/// The music kept after it is written: more than the wired sink's 4096-frame buffer.
const TAIL_FRAMES: usize = 8192;

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

/// The music last written to a wired sink, as (position of the first frame in ms, PCM) pieces, so
/// frames the sink gives back can be written again.
#[derive(Default)]
pub(super) struct Tail {
    pieces: VecDeque<(u64, Vec<u8>)>,
    frames: usize,
}

impl Tail {
    pub(super) fn frames(&self) -> usize {
        self.frames
    }
    pub(super) fn clear(&mut self) {
        self.pieces.clear();
        self.frames = 0;
    }
    pub(super) fn push(&mut self, ms: u64, pcm: &[u8], frame_bytes: usize) {
        self.pieces.push_back((ms, pcm.to_vec()));
        self.frames += pcm.len() / frame_bytes;
        while let Some((_, front)) = self.pieces.front() {
            let front = front.len() / frame_bytes;
            if self.frames - front < TAIL_FRAMES {
                break;
            }
            self.frames -= front;
            self.pieces.pop_front();
        }
    }
    /// Take the last `frames` frames out, as one block starting at its first frame's position
    /// (`tempo`: the speed × 1000, as positions count).
    pub(super) fn take_last(
        &mut self,
        frames: usize,
        format: PcmFormat,
        rate: u32,
        tempo: u64,
    ) -> Option<Pcm> {
        let frame_bytes = format.bytes_per_frame();
        let mut need = frames.min(self.frames);
        if need == 0 {
            return None;
        }
        self.frames -= need;
        let mut taken = VecDeque::new();
        let mut position_ms = 0;
        while need > 0 {
            let (ms, mut data) = self.pieces.pop_back()?;
            let have = data.len() / frame_bytes;
            if have > need {
                let keep = have - need;
                let back = data.split_off(keep * frame_bytes);
                position_ms = ms + keep as u64 * tempo / u64::from(rate);
                self.pieces.push_back((ms, data));
                taken.push_front(back);
                need = 0;
            } else {
                position_ms = ms;
                taken.push_front(data);
                need -= have;
            }
        }
        let data: Vec<u8> = taken.into_iter().flatten().collect();
        Some(Pcm {
            frames: (data.len() / frame_bytes) as u64,
            data,
            position_ms,
            packets: 0,
            rate,
            format,
        })
    }
}

/// `before` and then the unwritten rest of `block` from `offset`: music the sink gave back goes
/// again ahead of what was still to be written.
pub(super) fn joined(before: Pcm, rest: Option<(Pcm, usize)>) -> Pcm {
    let Some((block, offset)) = rest else {
        return before;
    };
    let mut data = before.data;
    data.extend_from_slice(&block.data[offset..]);
    Pcm {
        frames: (data.len() / block.format.bytes_per_frame()) as u64,
        data,
        ..before
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

    #[test]
    fn music_given_back_comes_out_of_the_tail_with_its_position() {
        let mut tail = Tail::default();
        // 1000 frames per second, speed 1: one frame is one millisecond.
        let fb = PcmFormat::S16LE.bytes_per_frame();
        tail.push(0, &stereo16(&[1; 100]), fb);
        tail.push(100, &stereo16(&[2; 100]), fb);
        let back = tail.take_last(150, PcmFormat::S16LE, 1000, 1000).unwrap();
        assert_eq!((back.position_ms, back.frames), (50, 150));
        assert_eq!(back.data[..4], stereo16(&[1])[..]);
        assert_eq!(back.data[back.data.len() - 4..], stereo16(&[2])[..]);
        assert_eq!(tail.frames(), 50);
        // The block still to be written, 10 frames of it written (the offset counts bytes).
        let block = tail.take_last(50, PcmFormat::S16LE, 1000, 1000).unwrap();
        let rest = joined(back, Some((block, 10 * fb)));
        assert_eq!((rest.position_ms, rest.frames), (50, 190));
        assert!(tail.take_last(1, PcmFormat::S16LE, 1000, 1000).is_none());
        // The tail keeps only the newest music.
        for piece in 0..100 {
            tail.push(piece * 512, &stereo16(&[0; 512]), fb);
        }
        assert!(tail.frames() < TAIL_FRAMES + 512 && tail.frames() >= TAIL_FRAMES - 512);
    }
}
