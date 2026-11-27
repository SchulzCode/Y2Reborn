mod native;
mod wired_rates;
pub use native::{initialize_logging, wired_configure, wired_device, wired_epoch, AlsaSink};
use reborn_core::{AudioOutput, BluetoothPcm, PcmFormat};
use serde::Serialize;
pub use wired_rates::{wired_rate, WIRED_RATE, WIRED_RATE_48K};
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
    /// Wired: the headphone level the stream got (0 Low, 1 Medium, 2 High).
    pub hp_level_live: Option<u8>,
    /// Wired: louder than the previous wired stream (playback fades in).
    pub level_raised: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
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
    pub transport_object: Option<String>,
    pub transport_device: Option<String>,
    pub transport: Option<String>,
    pub mode: Option<String>,
    pub transport_generation: u64,
    pub fallback: bool,
    pub fallback_reason: String,
    /// Wired: the internal speaker instead of the headphone jack (#28). The
    /// kernel routes the DAC to the amplifier while its Speaker pin is on.
    pub speaker: bool,
}
impl SinkSpec {
    pub fn validate_bluetooth_observation(&self, pcm: &BluetoothPcm) -> Result<(), String> {
        let AudioOutput::Bluetooth(address) = &self.output else {
            return Err("Bluetooth transport observation cannot validate a wired sink".into());
        };
        let expected_device = format!("dev_{}", address.replace(':', "_"));
        let pcm_matches_peer = pcm
            .device
            .rsplit('/')
            .next()
            .is_some_and(|device| device.eq_ignore_ascii_case(&expected_device));
        if self.transport_generation == 0
            || self.transport_generation != pcm.transport_generation
            || self.transport_object.as_deref() != Some(pcm.object.as_str())
            || self.transport_device.as_deref() != Some(pcm.device.as_str())
            || self.transport.as_deref() != Some(pcm.transport.as_str())
            || self.mode.as_deref() != Some(pcm.mode.as_str())
            || self.codec != pcm.codec
            || self.rate != pcm.negotiated_rate()?
            || self.format != pcm.negotiated_format()?
            || self.channels != pcm.channels.unwrap_or_default()
            || !pcm_matches_peer
            || !pcm.is_a2dp_playback_for(&pcm.device)
        {
            return Err(
                "Bluetooth sink plan no longer matches its observed transport epoch".into(),
            );
        }
        Ok(())
    }
    /// Whether a source at `source_rate` plays in this sink at the rate a fresh plan would give
    /// it: always on Bluetooth (the headphones set the rate), on the wired output when its rate
    /// is the source family's (`wired_rate`). A kept sink and a gapless join need this.
    pub fn fits_source(&self, source_rate: u32) -> bool {
        !matches!(self.output, AudioOutput::Wired) || self.rate == wired_rate(source_rate)
    }
}

#[cfg(test)]
mod bluetooth_contract_tests {
    use super::SinkSpec;
    use reborn_core::{AudioOutput, BluetoothPcm, PcmFormat};

    fn observed() -> BluetoothPcm {
        BluetoothPcm {
            object: "/org/bluealsa/hci0/dev_01_02_03_04_05_06/a2dp".into(),
            device: "/org/bluez/hci0/dev_01_02_03_04_05_06".into(),
            transport: "A2DP-source".into(),
            mode: "sink".into(),
            codec: Some("SBC".into()),
            format: Some(0x8210),
            rate: Some(48_000),
            channels: Some(2),
            transport_generation: 41,
            ..Default::default()
        }
    }

    fn planned(pcm: &BluetoothPcm) -> SinkSpec {
        SinkSpec {
            output: AudioOutput::Bluetooth("01:02:03:04:05:06".into()),
            rate: 48_000,
            format: PcmFormat::S16LE,
            physical_bits: 16,
            valid_bits: 16,
            channels: 2,
            layout: "stereo".into(),
            device: "bluealsa:DEV=01:02:03:04:05:06,PROFILE=a2dp".into(),
            codec: pcm.codec.clone(),
            transport_object: Some(pcm.object.clone()),
            transport_device: Some(pcm.device.clone()),
            transport: Some(pcm.transport.clone()),
            mode: Some(pcm.mode.clone()),
            transport_generation: pcm.transport_generation,
            fallback: false,
            fallback_reason: String::new(),
            speaker: false,
        }
    }

    #[test]
    fn bluetooth_sink_spec_is_bound_to_the_observed_transport_epoch_and_contract() {
        let pcm = observed();
        let spec = planned(&pcm);
        assert!(spec.validate_bluetooth_observation(&pcm).is_ok());

        let mut stale = pcm.clone();
        stale.transport_generation += 1;
        assert!(spec.validate_bluetooth_observation(&stale).is_err());
        let mut wrong_peer = spec.clone();
        wrong_peer.output = AudioOutput::Bluetooth("01:02:03:04:05:07".into());
        assert!(wrong_peer.validate_bluetooth_observation(&pcm).is_err());

        let changes: [fn(&mut BluetoothPcm); 8] = [
            |pcm: &mut BluetoothPcm| pcm.object.push_str("_new"),
            |pcm: &mut BluetoothPcm| pcm.device.push_str("_new"),
            |pcm: &mut BluetoothPcm| pcm.transport.push_str("_new"),
            |pcm: &mut BluetoothPcm| pcm.mode.push_str("_new"),
            |pcm: &mut BluetoothPcm| pcm.codec = Some("Other".into()),
            |pcm: &mut BluetoothPcm| pcm.format = Some(0x8420),
            |pcm: &mut BluetoothPcm| pcm.rate = Some(44_100),
            |pcm: &mut BluetoothPcm| pcm.channels = Some(1),
        ];
        for change in changes {
            let mut changed = pcm.clone();
            change(&mut changed);
            assert!(spec.validate_bluetooth_observation(&changed).is_err());
        }
    }
}
pub trait AudioSink {
    fn parameters(&self) -> Parameters;
    fn write(&mut self, pcm: &[u8]) -> Result<usize, String>;
    fn discard(&mut self) -> Result<(), String>;
    fn delay(&self) -> u64;
    /// Frames waiting in the sink's own ring, without what lies beyond it (a Bluetooth codec's
    /// delay is in `delay`); None where the sink cannot tell.
    fn queued(&self) -> Option<u64> {
        None
    }
    /// Pause in place and resume; the stream stays open. A sink without a
    /// pause of its own simply stops being written to.
    fn pause(&mut self, on: bool) -> Result<(), String> {
        let _ = on;
        Ok(())
    }
    /// Set the output's own volume per channel, in centi-dB below full scale of the
    /// stream (the DAC's Master). `Ok(true)`: the output now applies it, so the PCM
    /// written needs no gain of the user's volume. `Ok(false)` (the default): this
    /// output has no volume of its own and the caller keeps applying gain itself.
    fn set_hardware_gain(&mut self, left_cdb: i32, right_cdb: i32) -> Result<bool, String> {
        let _ = (left_cdb, right_cdb);
        Ok(false)
    }
    /// Whether this output applies the user's volume itself (see `set_hardware_gain`), so the
    /// PCM written to it is not scaled by it first.
    fn applies_volume(&self) -> bool {
        false
    }
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
