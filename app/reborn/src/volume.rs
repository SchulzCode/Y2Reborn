//! User gain at the live sink boundary, after the decode/EQ/limiter graph.
//! Keeping queued PCM independent of user volume avoids reopening the decoder
//! or ALSA and lets volume changes affect already buffered/crossfaded samples.
use reborn_core::PcmFormat;

/// The largest balance in dB either way (Settings > Sound > Balance).
pub(crate) const MAX_BALANCE: i8 = reborn_core::MAX_BALANCE;

pub(crate) struct VolumeRamp {
    gain: f64,
    target: f64,
    step: f64,
    remaining: usize,
    ramp_frames: usize,
    /// A fade-in is running (it carries over a reopened sink).
    fading: bool,
    /// Per-channel factor of a stereo stream: the balance lowers the side
    /// away from it, never raises the other (no clipping).
    channels: [f64; 2],
    /// Both channels carry (L+R)/2: the internal speaker plays one channel
    /// (the DAC's speaker route takes the left slot only, #28).
    mono: bool,
}
impl VolumeRamp {
    pub(crate) fn new(volume: u8, rate: u32) -> Self {
        let gain = Self::gain(volume);
        Self {
            gain,
            target: gain,
            step: 0.0,
            remaining: 0,
            ramp_frames: (rate as usize / 200).max(1), // five milliseconds
            fading: false,
            channels: [1.0; 2],
            mono: false,
        }
    }
    /// Start from silence and reach `volume` over `ms`: the first stream after
    /// the headphone level went up (hearing safety; a later volume change
    /// takes over with the usual short ramp).
    pub(crate) fn fade_in(volume: u8, rate: u32, ms: u32) -> Self {
        Self::fade_in_from(volume, rate, ms, 0.0)
    }
    /// A fade-in that already reached `fraction` of its volume: the one a
    /// reopened sink (Next, a seek, another rate) continues instead of
    /// starting at full volume.
    pub(crate) fn fade_in_from(volume: u8, rate: u32, ms: u32, fraction: f64) -> Self {
        let mut ramp = Self::new(volume, rate);
        ramp.gain = ramp.target * fraction.clamp(0.0, 1.0);
        ramp.remaining = ((u64::from(rate) * u64::from(ms) / 1000) as usize).max(1);
        ramp.step = (ramp.target - ramp.gain) / ramp.remaining as f64;
        ramp.fading = true;
        ramp
    }
    /// How far an unfinished fade-in got: the fraction of its volume reached
    /// and the milliseconds left at `rate`. None when no fade-in runs.
    pub(crate) fn fade_progress(&self, rate: u32) -> Option<(f64, u32)> {
        if !self.fading || self.remaining == 0 || rate == 0 {
            return None;
        }
        let fraction = if self.target > 0.0 {
            self.gain / self.target
        } else {
            0.0
        };
        let ms = (self.remaining as u64 * 1000 / u64::from(rate)).max(1);
        Some((fraction, u32::try_from(ms).unwrap_or(u32::MAX)))
    }
    fn gain(volume: u8) -> f64 {
        // Even dB steps (Beta 8.35; the squared curve before).
        reborn_core::volume_gain(volume)
    }
    pub(crate) fn target(&mut self, volume: u8) {
        let target = Self::gain(volume);
        if target != self.target {
            self.target = target;
            if self.fading && self.remaining > 0 && target > self.gain {
                // Louder during a fade-in: keep fading, towards the new volume.
                self.step = (target - self.gain) / self.remaining as f64;
                return;
            }
            self.fading = false;
            self.remaining = self.ramp_frames;
            self.step = (target - self.gain) / self.remaining as f64;
        }
    }
    /// The balance in dB: negative lowers the right, positive the left.
    pub(crate) fn balance(&mut self, db: i8) {
        let db = db.clamp(-MAX_BALANCE, MAX_BALANCE);
        let lower = 10f64.powf(-f64::from(db.unsigned_abs()) / 20.0);
        self.channels = match db.signum() {
            -1 => [1.0, lower],
            1 => [lower, 1.0],
            _ => [1.0; 2],
        };
    }
    /// Mix both channels into one for the internal speaker; the balance
    /// does not apply to it.
    pub(crate) fn mono(&mut self, on: bool) {
        self.mono = on;
    }
    /// Render speculatively; the sink may accept only part (or none) of it.
    /// Call advance only for frames actually accepted by the sink.
    pub(crate) fn render(&self, data: &[u8], format: PcmFormat) -> Vec<u8> {
        // Frames are always stereo (PcmFormat::bytes_per_frame).
        let balanced = self.mono || self.channels == [1.0; 2];
        if self.remaining == 0 && self.gain == 1.0 && balanced && !self.mono {
            return data.to_vec();
        }
        let width = format.bytes_per_sample();
        let mut result = Vec::with_capacity(data.len());
        for (index, frame) in data.chunks_exact(format.bytes_per_frame()).enumerate() {
            let frame_gain = self.at(index + 1);
            let mut values = [
                decode(&frame[..width], format),
                decode(&frame[width..], format),
            ];
            if self.mono {
                values = [(values[0] + values[1]) / 2.0; 2];
            }
            for (channel, value) in values.into_iter().enumerate() {
                let gain = if balanced {
                    frame_gain
                } else {
                    frame_gain * self.channels[channel]
                };
                encode(value * gain, format, &mut result);
            }
        }
        result
    }
    fn at(&self, frames: usize) -> f64 {
        if frames >= self.remaining {
            self.target
        } else {
            self.gain + self.step * frames as f64
        }
    }
    /// The gain the volume asks for (a wheel click follows it).
    pub(crate) fn level(&self) -> f64 {
        self.target
    }
    pub(crate) fn advance(&mut self, frames: usize) {
        self.gain = self.at(frames);
        self.remaining = self.remaining.saturating_sub(frames);
        self.fading &= self.remaining > 0;
    }
}

/// One little-endian sample of `format` as a number (S24 in 32 bits, sign
/// extended from its low three bytes).
pub(crate) fn decode(sample: &[u8], format: PcmFormat) -> f64 {
    match format {
        PcmFormat::S16LE => f64::from(i16::from_le_bytes(sample.try_into().unwrap())),
        PcmFormat::S24LE => f64::from((i32::from_le_bytes(sample.try_into().unwrap()) << 8) >> 8),
        PcmFormat::S32LE => f64::from(i32::from_le_bytes(sample.try_into().unwrap())),
    }
}
/// Round, clamp to the format's range and append.
pub(crate) fn encode(value: f64, format: PcmFormat, out: &mut Vec<u8>) {
    match format {
        PcmFormat::S16LE => {
            let value = value
                .round()
                .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
            out.extend_from_slice(&value.to_le_bytes());
        }
        PcmFormat::S24LE | PcmFormat::S32LE => {
            let (min, max) = if format == PcmFormat::S24LE {
                (-8_388_608, 8_388_607)
            } else {
                (i32::MIN, i32::MAX)
            };
            let value = value.round().clamp(f64::from(min), f64::from(max)) as i32;
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fade_in_starts_silent_rises_steadily_and_yields_to_a_volume_change() {
        let mut ramp = VolumeRamp::fade_in(100, 1000, 2000); // 2000 frames
        let first = values(&ramp.render(&stereo16(10_000, 4), PcmFormat::S16LE));
        assert_eq!(first, [5, 5, 10, 10, 15, 15, 20, 20]);
        ramp.advance(1000);
        assert_eq!(
            values(&ramp.render(&stereo16(10_000, 1), PcmFormat::S16LE)),
            [5005, 5005]
        );
        ramp.advance(1000);
        assert_eq!(
            values(&ramp.render(&stereo16(10_000, 1), PcmFormat::S16LE)),
            [10_000, 10_000]
        );
        // Turning the volume down mid-fade uses the short ramp from where it is.
        let mut ramp = VolumeRamp::fade_in(100, 48_000, 2000);
        ramp.advance(48_000);
        ramp.target(80); // -12 dB
        ramp.advance(240);
        assert_eq!(
            values(&ramp.render(&stereo16(10_000, 1), PcmFormat::S16LE)),
            [2512, 2512]
        );
    }
    #[test]
    fn an_unfinished_fade_continues_in_a_reopened_sink_and_through_volume_up() {
        // A quarter into the fade, the sink reopens at another rate.
        let mut ramp = VolumeRamp::fade_in(100, 1000, 2000);
        ramp.advance(500);
        let (fraction, ms) = ramp.fade_progress(1000).unwrap();
        assert!(
            (fraction - 0.25).abs() < 1e-9 && ms == 1500,
            "{fraction} {ms}"
        );
        let mut ramp = VolumeRamp::fade_in_from(100, 2000, ms, fraction);
        assert_eq!(
            values(&ramp.render(&stereo16(10_000, 1), PcmFormat::S16LE)),
            [2502, 2502]
        );
        ramp.advance(1500);
        let (fraction, _) = ramp.fade_progress(2000).unwrap();
        assert!((fraction - 0.625).abs() < 1e-9, "{fraction}");
        // Volume up keeps the fade's pace (no 5-ms jump to full volume)...
        let mut up = VolumeRamp::fade_in(50, 1000, 2000);
        up.advance(1000);
        up.target(100);
        up.advance(5);
        let gain = values(&up.render(&stereo16(10_000, 1), PcmFormat::S16LE))[0];
        assert!(gain < 2000, "{gain}");
        up.advance(995);
        assert_eq!(
            values(&up.render(&stereo16(10_000, 1), PcmFormat::S16LE)),
            [10_000, 10_000]
        );
        assert!(up.fade_progress(1000).is_none(), "the fade is over");
        // ...volume down takes effect at once and ends the fade.
        let mut down = VolumeRamp::fade_in(100, 1000, 2000);
        down.advance(1000);
        down.target(10);
        assert!(down.fade_progress(1000).is_none());
        assert!(VolumeRamp::new(100, 1000).fade_progress(1000).is_none());
    }
    fn stereo16(sample: i16, frames: usize) -> Vec<u8> {
        sample.to_le_bytes().repeat(frames * 2)
    }
    fn values(data: &[u8]) -> Vec<i16> {
        data.as_chunks::<2>()
            .0
            .iter()
            .map(|s| i16::from_le_bytes(*s))
            .collect()
    }
    #[test]
    fn volume_steps_are_even_in_db_and_reach_silence() {
        assert_eq!(VolumeRamp::gain(100), 1.0);
        assert_eq!(VolumeRamp::gain(0), 0.0);
        for v in 10..100u8 {
            let step = 20.0 * (VolumeRamp::gain(v + 1) / VolumeRamp::gain(v)).log10();
            assert!((step - 0.6).abs() < 1e-9, "{v}: {step}");
        }
        for v in 1..10u8 {
            let step = 20.0 * (VolumeRamp::gain(v + 1) / VolumeRamp::gain(v)).log10();
            assert!((step - 3.0).abs() < 1e-9, "{v}: {step}");
        }
        assert!((20.0 * VolumeRamp::gain(1).log10() + 81.0).abs() < 1e-9);
    }
    #[test]
    fn mono_carries_both_channels_on_each_and_ignores_the_balance() {
        let mut ramp = VolumeRamp::new(100, 1000);
        ramp.mono(true);
        ramp.balance(6);
        let frame = [10_000i16, -2_000]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>();
        assert_eq!(
            values(&ramp.render(&frame, PcmFormat::S16LE)),
            [4_000, 4_000]
        );
        // Full-scale opposite channels cancel, equal ones stay (no clipping).
        let loud = [i16::MAX, i16::MAX]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>();
        assert_eq!(
            values(&ramp.render(&loud, PcmFormat::S16LE)),
            [i16::MAX, i16::MAX]
        );
        ramp.mono(false);
        assert_eq!(
            values(&ramp.render(&frame, PcmFormat::S16LE)),
            [5012, -2_000]
        );
    }
    #[test]
    fn balance_lowers_only_the_other_side() {
        let mut ramp = VolumeRamp::new(100, 1000);
        ramp.balance(-1); // left louder: the right is 1 dB lower
        assert_eq!(
            values(&ramp.render(&stereo16(10_000, 1), PcmFormat::S16LE)),
            [10_000, 8913]
        );
        ramp.balance(6);
        assert_eq!(
            values(&ramp.render(&stereo16(10_000, 1), PcmFormat::S16LE)),
            [5012, 10_000]
        );
        // Center is a pass-through again.
        ramp.balance(0);
        let data = stereo16(1234, 2);
        assert_eq!(ramp.render(&data, PcmFormat::S16LE), data);
    }
    #[test]
    fn live_change_ramps_both_channels_and_reaches_exact_mute() {
        let mut ramp = VolumeRamp::new(100, 48_000);
        ramp.target(0);
        let data = values(&ramp.render(&stereo16(24_000, 480), PcmFormat::S16LE));
        for (i, pair) in data.as_chunks::<2>().0.iter().enumerate() {
            assert_eq!(pair[0], pair[1]);
            assert_eq!(pair[0], (24_000 - 100 * (i as i32 + 1)).max(0) as i16);
        }
        ramp.advance(480);
        assert!(ramp
            .render(&stereo16(-24_000, 10), PcmFormat::S16LE)
            .iter()
            .all(|v| *v == 0));
        ramp.target(80); // -12 dB
        ramp.advance(240);
        assert_eq!(
            values(&ramp.render(&stereo16(-24_000, 1), PcmFormat::S16LE)),
            [-6029, -6029]
        );
    }
    #[test]
    fn zero_and_partial_writes_do_not_consume_or_repeat_the_ramp() {
        let mut ramp = VolumeRamp::new(100, 48_000);
        ramp.target(0);
        let input = stereo16(24_000, 480);
        let all = ramp.render(&input, PcmFormat::S16LE);
        ramp.advance(0);
        assert_eq!(ramp.render(&input, PcmFormat::S16LE), all);
        ramp.advance(73);
        assert_eq!(
            ramp.render(&input[73 * 4..], PcmFormat::S16LE),
            all[73 * 4..]
        );
        // A new target starts from the last accepted gain, not the old target.
        ramp.target(100);
        let resumed = values(&ramp.render(&input, PcmFormat::S16LE));
        assert!((16_700..17_000).contains(&resumed[0]));
        assert_eq!(resumed[478], 24_000);
    }
    #[test]
    fn unity_preserves_low_bits_and_gain_preserves_signed_sample_packing() {
        for format in [PcmFormat::S24LE, PcmFormat::S32LE] {
            let input = [0x00012345i32, -0x12345]
                .into_iter()
                .flat_map(i32::to_le_bytes)
                .collect::<Vec<_>>();
            assert_eq!(VolumeRamp::new(100, 44_100).render(&input, format), input);
            let scaled = VolumeRamp::new(80, 44_100).render(&input, format);
            let samples = scaled
                .as_chunks::<4>()
                .0
                .iter()
                .map(|s| i32::from_le_bytes(*s))
                .collect::<Vec<_>>();
            assert_eq!(samples, [18_730, -18_730]);
        }
        // S24 padding may be zero for a negative value; sign comes from bit23.
        let input = [0x00800000i32, 0x007fffff]
            .into_iter()
            .flat_map(i32::to_le_bytes)
            .collect::<Vec<_>>();
        let scaled = VolumeRamp::new(80, 44_100).render(&input, PcmFormat::S24LE);
        assert_eq!(
            i32::from_le_bytes(scaled[..4].try_into().unwrap()),
            -2_107_123
        );
        assert_eq!(
            i32::from_le_bytes(scaled[4..].try_into().unwrap()),
            2_107_123
        );
    }
}
