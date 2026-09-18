mod native;
pub use native::{initialize_logging, wired_device, AlsaSink};
use serde::Serialize;
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct Parameters {
    pub rate: u32,
    pub period: u32,
    pub buffer: u32,
}
#[derive(Clone)]
pub struct SinkSpec {
    pub output: reborn_core::AudioOutput,
    pub rate: u32,
}
pub trait AudioSink {
    fn parameters(&self) -> Parameters;
    fn write(&mut self, samples: &[i16]) -> Result<usize, String>;
    fn discard(&mut self) -> Result<(), String>;
    fn delay(&self) -> u64;
}
pub fn valid_address(s: &str) -> bool {
    s.len() == 17
        && s.split(':').count() == 6
        && s.split(':')
            .all(|p| p.len() == 2 && p.bytes().all(|c| c.is_ascii_hexdigit()))
}
pub fn gain(samples: &mut [i16], volume: u8) {
    let gain = (volume.min(100) as f32 / 100.).powi(2);
    for s in samples {
        *s = (*s as f32 * gain).round() as i16;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn addresses_cannot_inject_pcm_options() {
        assert!(valid_address("12:34:56:78:90:AB"));
        assert!(!valid_address("12:34:56:78:90:AB,PROFILE=hfp"));
    }
    #[test]
    fn gain_is_bounded() {
        let mut v = [i16::MAX, i16::MIN];
        gain(&mut v, 255);
        assert_eq!(v, [i16::MAX, i16::MIN]);
        gain(&mut v, 0);
        assert_eq!(v, [0, 0]);
    }
    #[test]
    fn missing_sink_is_error() {
        let p = std::env::temp_dir().join("reborn-alsa-test");
        let o = reborn_observability::Observer::new(&p).unwrap();
        assert!(
            AlsaSink::open_named("reborn-nonexistent-test-device", 48000, false, o, 1).is_err()
        );
    }
}
