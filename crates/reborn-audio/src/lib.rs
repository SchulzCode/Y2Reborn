mod native;
pub use native::{initialize_logging, wired_device, AlsaSink};
use reborn_core::{AudioOutput, PcmFormat};
use serde::Serialize;
#[derive(Debug, Clone, Serialize, Default)]
pub struct Parameters {
    pub rate: u32,
    pub period: u32,
    pub buffer: u32,
    pub format: PcmFormat,
    pub channels: u32,
    pub hardware_mixer_gain_db: Option<f32>,
    pub device: String,
    pub fallback: bool,
    pub fallback_reason: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct SinkSpec {
    pub output: AudioOutput,
    pub rate: u32,
    pub format: PcmFormat,
    pub physical_bits: u8,
    pub valid_bits: u8,
    pub channels: u8,
    pub layout: String,
    pub device: String,
    pub codec: Option<String>,
    pub transport_generation: u64,
    pub fallback: bool,
    pub fallback_reason: String,
}
pub trait AudioSink {
    fn parameters(&self) -> Parameters;
    fn write(&mut self, pcm: &[u8]) -> Result<usize, String>;
    fn discard(&mut self) -> Result<(), String>;
    fn delay(&self) -> u64;
}
pub fn valid_address(s: &str) -> bool {
    s.len() == 17
        && s.split(':').count() == 6
        && s.split(':')
            .all(|p| p.len() == 2 && p.bytes().all(|c| c.is_ascii_hexdigit()))
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
    fn missing_sink_is_error() {
        let p = std::env::temp_dir().join("reborn-alsa-test");
        let o = reborn_observability::Observer::new(&p).unwrap();
        assert!(AlsaSink::open_named(
            "reborn-nonexistent-test-device",
            48000,
            PcmFormat::S32LE,
            false,
            None,
            o,
            1,
        )
        .is_err());
    }
}
