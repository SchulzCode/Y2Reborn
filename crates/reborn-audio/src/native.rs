use super::{valid_address, AudioSink, Parameters, SinkSpec};
use reborn_core::{AudioOutput, BluetoothPcm, PcmFormat};
use reborn_observability::{Level, Observer};
use serde_json::json;
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    ptr::NonNull,
};

#[repr(C)]
struct RawParams {
    rate: u32,
    period: u32,
    buffer: u32,
    format: u32,
    channels: u32,
    fallback: c_int,
    hardware_mixer_gain_cdb: c_int,
    device: [c_char; 128],
    fallback_reason: [c_char; 256],
}
unsafe extern "C" {
    fn rb_alsa_logging(callback: extern "C" fn(*const c_char));
    fn rb_alsa_error(e: c_int) -> *const c_char;
    fn rb_alsa_format_info(format: u32, valid_bits: *mut c_int, physical_bits: *mut c_int)
        -> c_int;
    fn rb_wired_device(p: *mut c_char, n: c_int) -> c_int;
    fn rb_alsa_plan(
        name: *const c_char,
        rate: u32,
        preferred_format: u32,
        wired: c_int,
        strict_format: c_int,
        params: *mut RawParams,
    ) -> c_int;
    fn rb_sink_open(
        name: *const c_char,
        rate: u32,
        format: u32,
        wired: c_int,
        out: *mut *mut c_void,
        params: *mut RawParams,
    ) -> c_int;
    fn rb_sink_close(p: *mut c_void);
    fn rb_sink_write(p: *mut c_void, b: *const u8, frames: c_int) -> c_int;
    fn rb_sink_wait(p: *mut c_void, ms: c_int) -> c_int;
    fn rb_sink_recover(p: *mut c_void, e: c_int) -> c_int;
    fn rb_sink_drop(p: *mut c_void) -> c_int;
    fn rb_sink_delay(p: *mut c_void) -> c_int;
}
fn error(e: c_int) -> String {
    // SAFETY: snd_strerror provides a static terminated string for all ALSA errors.
    format!(
        "ALSA {e}: {}",
        unsafe { CStr::from_ptr(rb_alsa_error(e)) }.to_string_lossy()
    )
}
fn text(b: &[c_char]) -> String {
    b.iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect::<Vec<_>>()
        .into_iter()
        .map(|b| b as char)
        .collect()
}
fn native_format(format: PcmFormat) -> u32 {
    match format {
        PcmFormat::S16LE => 1,
        PcmFormat::S24LE => 3,
        PcmFormat::S32LE => 2,
    }
}
fn format(value: u32) -> Result<PcmFormat, String> {
    match value {
        1 => Ok(PcmFormat::S16LE),
        2 => Ok(PcmFormat::S32LE),
        3 => Ok(PcmFormat::S24LE),
        _ => Err(format!("unsupported ALSA format code {value}")),
    }
}
fn format_contract(value: u32) -> Result<PcmFormat, String> {
    let format = format(value)?;
    let mut valid_bits = 0;
    let mut physical_bits = 0;
    // SAFETY: ALSA writes two integers through live pointers.
    let result = unsafe { rb_alsa_format_info(value, &mut valid_bits, &mut physical_bits) };
    if result < 0 {
        return Err(error(result));
    }
    if valid_bits != i32::from(format.valid_bits())
        || physical_bits != i32::from(format.physical_bits())
    {
        return Err(format!(
            "ALSA format {} reports {valid_bits} valid bits in {physical_bits} physical bits",
            format.as_str()
        ));
    }
    Ok(format)
}
fn parameters(raw: &RawParams, planned: Option<&SinkSpec>) -> Result<Parameters, String> {
    Ok(Parameters {
        rate: raw.rate,
        period: raw.period,
        buffer: raw.buffer,
        format: format_contract(raw.format)?,
        channels: raw.channels,
        hardware_mixer_gain_db: (raw.hardware_mixer_gain_cdb != i32::MIN)
            .then_some(raw.hardware_mixer_gain_cdb as f32 / 100.0),
        device: text(&raw.device),
        fallback: planned.is_some_and(|s| s.fallback) || raw.fallback != 0,
        fallback_reason: planned
            .map(|s| s.fallback_reason.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| text(&raw.fallback_reason)),
    })
}
pub fn wired_device() -> Result<String, String> {
    let mut b = [0; 128];
    // SAFETY: C receives writable storage with matching capacity.
    let r = unsafe { rb_wired_device(b.as_mut_ptr(), b.len() as c_int) };
    if r < 0 {
        Err(error(r))
    } else {
        // SAFETY: successful C call uses snprintf and NUL terminates.
        Ok(unsafe { CStr::from_ptr(b.as_ptr()) }
            .to_string_lossy()
            .into_owned())
    }
}
fn output_name(output: &AudioOutput) -> Result<(String, bool), String> {
    match output {
        AudioOutput::Wired => Ok((wired_device()?, true)),
        AudioOutput::Bluetooth(address) => {
            if !valid_address(address) {
                return Err("invalid Bluetooth address".into());
            }
            Ok((format!("bluealsa:DEV={address},PROFILE=a2dp"), false))
        }
    }
}
fn bluetooth_pcm_lease(device: &str) -> Result<Option<std::fs::File>, String> {
    if !device.starts_with("bluealsa:")
        || !std::path::Path::new("/etc/y2linux/platform-contract").exists()
    {
        return Ok(None);
    }
    pcm_lease_at(std::path::Path::new("/run/y2"))
}
fn pcm_lease_at(directory: &std::path::Path) -> Result<Option<std::fs::File>, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("bt-pcm.lock"))
        .map_err(|e| e.to_string())?;
    lock.try_lock_shared()
        .map_err(|_| "Bluetooth codec selection in progress; retry playback")?;
    if directory.join("bt-codec-uncertain.json").exists() {
        return Err("Bluetooth codec outcome unknown; restart BlueALSA before playback".into());
    }
    Ok(Some(lock))
}
pub struct AlsaSink {
    _pcm_lease: Option<std::fs::File>,
    raw: NonNull<c_void>,
    params: Parameters,
    log: Observer,
    correlation: u64,
}
impl AlsaSink {
    pub fn plan(
        output: &AudioOutput,
        rate: u32,
        log: Observer,
        id: u64,
    ) -> Result<(SinkSpec, Parameters), String> {
        let (device, wired) = output_name(output)?;
        if !wired {
            return Err("Bluetooth sink requires an observed BlueALSA PCM object".into());
        }
        Self::plan_named(
            &device,
            output.clone(),
            rate,
            wired,
            PcmFormat::S32LE,
            false,
            None,
            log,
            id,
        )
    }
    pub fn plan_bluetooth(
        output: &AudioOutput,
        pcm: &BluetoothPcm,
        log: Observer,
        id: u64,
    ) -> Result<(SinkSpec, Parameters), String> {
        let (device, wired) = output_name(output)?;
        if wired {
            return Err("Bluetooth PCM observation cannot plan a wired sink".into());
        }
        if pcm.channels != Some(2) {
            return Err("Bluetooth PCM must negotiate stereo for the audio sink".into());
        }
        if pcm.transport_generation == 0
            || pcm.object.is_empty()
            || pcm.device.is_empty()
            || pcm.transport.is_empty()
            || pcm.mode.is_empty()
        {
            return Err("Bluetooth PCM has no complete observed transport identity".into());
        }
        let rate = pcm.negotiated_rate()?;
        let preferred = pcm.negotiated_format()?;
        Self::plan_named(
            &device,
            output.clone(),
            rate,
            wired,
            preferred,
            true,
            Some(pcm),
            log,
            id,
        )
    }
    fn plan_named(
        device: &str,
        output: AudioOutput,
        rate: u32,
        wired: bool,
        preferred: PcmFormat,
        strict_format: bool,
        observed: Option<&BluetoothPcm>,
        log: Observer,
        id: u64,
    ) -> Result<(SinkSpec, Parameters), String> {
        let _pcm_lease = bluetooth_pcm_lease(device)?;
        let name = CString::new(device).map_err(|_| "NUL in PCM device")?;
        let mut raw: RawParams = unsafe { std::mem::zeroed() };
        // SAFETY: synchronous probe receives a live device name and output storage.
        let r = unsafe {
            rb_alsa_plan(
                name.as_ptr(),
                rate,
                native_format(preferred),
                i32::from(wired),
                i32::from(strict_format),
                &mut raw,
            )
        };
        if r < 0 {
            log.emit(
                Level::Error,
                "alsa",
                "plan_failed",
                &error(r),
                Some(id),
                json!({"device":device,"requested_rate":rate,"preferred_format":preferred.as_str()}),
            );
            return Err(error(r));
        }
        let actual = format_contract(raw.format)?;
        if raw.rate != rate {
            return Err(format!(
                "ALSA sink rate {} does not match requested rate {rate}",
                raw.rate
            ));
        }
        if raw.channels != 2 {
            return Err(format!(
                "ALSA sink opened {} channels; stereo output is required",
                raw.channels
            ));
        }
        if strict_format && actual != preferred {
            return Err(format!(
                "ALSA sink changed observed Bluetooth PCM format from {} to {}",
                preferred.as_str(),
                actual.as_str()
            ));
        }
        let spec = SinkSpec {
            output,
            rate: raw.rate,
            format: actual,
            physical_bits: actual.physical_bits(),
            valid_bits: actual.valid_bits(),
            channels: raw.channels as u8,
            layout: "stereo".into(),
            device: device.into(),
            codec: observed.and_then(|pcm| pcm.codec.clone()),
            transport_object: observed.map(|pcm| pcm.object.clone()),
            transport_device: observed.map(|pcm| pcm.device.clone()),
            transport: observed.map(|pcm| pcm.transport.clone()),
            mode: observed.map(|pcm| pcm.mode.clone()),
            transport_generation: observed.map_or(0, |pcm| pcm.transport_generation),
            fallback: raw.fallback != 0,
            fallback_reason: text(&raw.fallback_reason),
        };
        let params = parameters(&raw, Some(&spec))?;
        if let Some(observed) = observed {
            spec.validate_bluetooth_observation(observed)?;
        }
        if spec.fallback {
            log.emit(
                Level::Warn,
                "alsa",
                "explicit_format_fallback",
                &spec.fallback_reason,
                Some(id),
                json!({"device":device,"requested_format":preferred.as_str(),"selected_format":actual.as_str(),"rate":raw.rate}),
            );
        } else {
            log.emit(
                Level::Info,
                "alsa",
                "qualified_format",
                "ALSA sink capability and qualification profile agree",
                Some(id),
                json!({"device":device,"format":actual.as_str(),"rate":raw.rate}),
            );
        }
        Ok((spec, params))
    }
    pub fn open(output: &AudioOutput, rate: u32, log: Observer, id: u64) -> Result<Self, String> {
        let (spec, _) = Self::plan(output, rate, log.clone(), id)?;
        Self::open_spec(&spec, log, id)
    }
    pub fn open_spec(spec: &SinkSpec, log: Observer, id: u64) -> Result<Self, String> {
        if matches!(spec.output, AudioOutput::Bluetooth(_))
            && (spec.transport_generation == 0
                || spec.transport_object.as_deref().is_none_or(str::is_empty)
                || spec.transport_device.as_deref().is_none_or(str::is_empty)
                || spec.transport.as_deref().is_none_or(str::is_empty)
                || spec.mode.as_deref().is_none_or(str::is_empty))
        {
            return Err("Bluetooth sink spec has no observed transport epoch".into());
        }
        if spec.channels != 2
            || spec.physical_bits != spec.format.physical_bits()
            || spec.valid_bits != spec.format.valid_bits()
        {
            return Err("planned ALSA sink parameters are internally inconsistent".into());
        }
        let (device, wired) = if spec.device.is_empty() {
            output_name(&spec.output)?
        } else {
            (
                spec.device.clone(),
                matches!(spec.output, AudioOutput::Wired),
            )
        };
        Self::open_named(&device, spec.rate, spec.format, wired, Some(spec), log, id)
    }
    pub fn open_named(
        device: &str,
        rate: u32,
        pcm_format: PcmFormat,
        wired: bool,
        planned: Option<&SinkSpec>,
        log: Observer,
        id: u64,
    ) -> Result<Self, String> {
        let pcm_lease = bluetooth_pcm_lease(device)?;
        let name = CString::new(device).map_err(|_| "NUL in PCM device")?;
        let mut raw = std::ptr::null_mut();
        let mut p: RawParams = unsafe { std::mem::zeroed() };
        // SAFETY: synchronous C constructor receives live input and output storage.
        let r = unsafe {
            rb_sink_open(
                name.as_ptr(),
                rate,
                native_format(pcm_format),
                i32::from(wired),
                &mut raw,
                &mut p,
            )
        };
        if r < 0 {
            log.emit(
                Level::Error,
                "alsa",
                "open_failed",
                &error(r),
                Some(id),
                json!({"native_code":r,"device":device,"format":pcm_format.as_str(),"rate":rate}),
            );
            return Err(error(r));
        }
        let params = match parameters(&p, planned) {
            Ok(params) => params,
            Err(error) => {
                // The native constructor owns a live handle even if its returned
                // parameter record fails our format contract.
                if !raw.is_null() {
                    unsafe { rb_sink_close(raw) };
                }
                return Err(error);
            }
        };
        if let Some(spec) = planned {
            if params.rate != spec.rate
                || params.format != spec.format
                || params.channels != u32::from(spec.channels)
                || params.device != spec.device
            {
                if !raw.is_null() {
                    unsafe { rb_sink_close(raw) };
                }
                return Err("opened ALSA sink does not match the planned sink contract".into());
            }
        }
        log.emit(
            Level::Info,
            "alsa",
            "prepared",
            "PCM open and parameters negotiated",
            Some(id),
            json!({"device":device,"params":params,"format":params.format.as_str(),"channels":params.channels}),
        );
        Ok(Self {
            _pcm_lease: pcm_lease,
            raw: NonNull::new(raw).ok_or_else(|| "null ALSA handle".to_string())?,
            params,
            log,
            correlation: id,
        })
    }
}
impl AudioSink for AlsaSink {
    fn parameters(&self) -> Parameters {
        self.params.clone()
    }
    fn write(&mut self, pcm: &[u8]) -> Result<usize, String> {
        let frame_bytes = self.params.format.bytes_per_frame();
        if pcm.is_empty() || !pcm.len().is_multiple_of(frame_bytes) || pcm.len() > 524_288 {
            return Err("PCM block bounds or format mismatch".into());
        }
        // SAFETY: unique initialized PCM handle; bytes remain live during the write.
        let n = unsafe {
            rb_sink_write(
                self.raw.as_ptr(),
                pcm.as_ptr(),
                (pcm.len() / frame_bytes) as c_int,
            )
        };
        if n >= 0 {
            return Ok(n as usize);
        }
        if n == -11 {
            // SAFETY: wait is bounded and the handle remains owned by this sink.
            unsafe { rb_sink_wait(self.raw.as_ptr(), 20) };
            return Ok(0);
        }
        if n == -32 || n == -86 {
            self.log.add("audio_xruns", 1.);
            self.log.emit(
                Level::Warn,
                "alsa",
                "xrun",
                &error(n),
                Some(self.correlation),
                json!({"native_code":n,"recovery_attempted":true}),
            );
            // SAFETY: recovery mutates only this uniquely borrowed PCM context.
            let r = unsafe { rb_sink_recover(self.raw.as_ptr(), n) };
            if r >= 0 {
                self.log.add("audio_recoveries", 1.);
                return Ok(0);
            }
            return Err(error(r));
        }
        Err(error(n))
    }
    fn discard(&mut self) -> Result<(), String> {
        // SAFETY: caller uniquely owns the PCM context.
        let r = unsafe { rb_sink_drop(self.raw.as_ptr()) };
        self.log.emit(
            Level::Info,
            "alsa",
            "discard",
            "PCM dropped and prepared",
            Some(self.correlation),
            json!({"native_code":r}),
        );
        if r < 0 {
            Err(error(r))
        } else {
            Ok(())
        }
    }
    fn delay(&self) -> u64 {
        // SAFETY: same-thread ALSA query.
        unsafe { rb_sink_delay(self.raw.as_ptr()) }.max(0) as u64
    }
}
impl Drop for AlsaSink {
    fn drop(&mut self) {
        // SAFETY: unique owner closes exactly once.
        unsafe { rb_sink_close(self.raw.as_ptr()) };
        self.log.emit(
            Level::Info,
            "alsa",
            "closed",
            "PCM closed",
            Some(self.correlation),
            json!({}),
        );
    }
}

static LOGGER: std::sync::OnceLock<Observer> = std::sync::OnceLock::new();
extern "C" fn alsa_log(message: *const c_char) {
    if message.is_null() {
        return;
    }
    // SAFETY: C supplies a terminated local buffer valid until callback return.
    let text = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    if let Some(log) = LOGGER.get() {
        log.emit(Level::Warn, "alsa", "library_log", &text, None, json!({}));
    }
}
pub fn initialize_logging(log: Observer) {
    if LOGGER.set(log).is_ok() {
        // SAFETY: callback has process lifetime and is registered once.
        unsafe { rb_alsa_logging(alsa_log) }
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn s24_code_maps_to_alsa_24_valid_bits_in_a_32_bit_container() {
        let mut valid_bits = 0;
        let mut physical_bits = 0;
        let result = unsafe {
            rb_alsa_format_info(
                native_format(PcmFormat::S24LE),
                &mut valid_bits,
                &mut physical_bits,
            )
        };

        assert_eq!(result, 0);
        assert_eq!(valid_bits, 24);
        assert_eq!(physical_bits, 32);
        assert_eq!(PcmFormat::S24LE.bytes_per_frame(), 8);
        assert_eq!(format_contract(3).unwrap(), PcmFormat::S24LE);
    }

    #[test]
    fn unknown_native_format_is_rejected() {
        assert!(format(4).is_err());
        assert!(format_contract(4).is_err());
        let mut valid_bits = 0;
        let mut physical_bits = 0;
        assert!(unsafe { rb_alsa_format_info(4, &mut valid_bits, &mut physical_bits) } < 0);
    }
}

#[cfg(test)]
mod platform_pcm_lease_tests {
    #[test]
    fn open_pcm_excludes_codec_change_and_unknown_outcome_blocks_reopen() {
        let dir = std::env::temp_dir().join(format!("reborn-pcm-lease-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let opened = super::pcm_lease_at(&dir).unwrap();
        let exclusive = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join("bt-pcm.lock"))
            .unwrap();
        assert!(exclusive.try_lock().is_err());
        drop(opened);
        exclusive.try_lock().unwrap();
        assert!(super::pcm_lease_at(&dir).is_err());
        std::fs::write(dir.join("bt-codec-uncertain.json"), b"{}").unwrap();
        drop(exclusive);
        assert!(super::pcm_lease_at(&dir).is_err());
        std::fs::remove_file(dir.join("bt-codec-uncertain.json")).unwrap();
        assert!(super::pcm_lease_at(&dir).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
