use super::{valid_address, AudioSink, Parameters, SinkSpec};
use reborn_core::{AudioOutput, BluetoothPcm, PcmFormat};
use reborn_observability::{Level, Observer};
use serde_json::json;
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
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
    hp_level_live: c_int,
    level_raised: c_int,
}
// Pinned with RbParams in native/audio.c (a _Static_assert there).
const _: () = assert!(
    std::mem::size_of::<RawParams>() == 420
        && std::mem::offset_of!(RawParams, device) == 28
        && std::mem::offset_of!(RawParams, hp_level_live) == 412
        && std::mem::offset_of!(RawParams, level_raised) == 416
);
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
    fn rb_wired_configure(level: c_int, slow: c_int, phase: c_int, nos: c_int) -> c_int;
    fn rb_wired_masters(low: c_int, medium: c_int, high: c_int) -> c_int;
    #[cfg(test)]
    fn rb_wired_note_live(live: c_int) -> c_int;
    fn rb_sink_close(p: *mut c_void);
    fn rb_sink_write(p: *mut c_void, b: *const u8, frames: c_int) -> c_int;
    fn rb_sink_wait(p: *mut c_void, ms: c_int) -> c_int;
    fn rb_sink_recover(p: *mut c_void, e: c_int) -> c_int;
    fn rb_sink_drop(p: *mut c_void) -> c_int;
    fn rb_sink_pause(p: *mut c_void, on: c_int, device_pause: c_int) -> c_int;
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
        hp_level_live: u8::try_from(raw.hp_level_live).ok(),
        level_raised: raw.level_raised != 0,
    })
}
/// The last wired level and filter chosen (packed), and how often the choice
/// changed: a wired stream opened before a change plays the old ones, since
/// the amplifier takes them only when it powers up.
static WIRED_CHOICE: AtomicU32 = AtomicU32::new(u32::MAX);
static WIRED_EPOCH: AtomicU64 = AtomicU64::new(0);
/// Changes with every new wired level or DAC filter choice: an open wired
/// sink serves a later stream only while this stays the same.
pub fn wired_epoch() -> u64 {
    WIRED_EPOCH.load(Ordering::Acquire)
}
/// Choose the wired headphone level and DAC filter (Y2Linux kernel controls).
/// Remembered for every wired stream; applied now where the amplifier is
/// down. Ok(false): the kernel has no level control (older Y2Linux).
pub fn wired_configure(
    level: reborn_core::HpLevel,
    filter: reborn_core::DacFilter,
) -> Result<bool, String> {
    let (slow, phase, nos) = filter.switches();
    let choice =
        level.index() | u32::from(slow) << 8 | u32::from(phase) << 9 | u32::from(nos) << 10;
    if WIRED_CHOICE.swap(choice, Ordering::AcqRel) != choice {
        WIRED_EPOCH.fetch_add(1, Ordering::AcqRel);
    }
    let master = |level: reborn_core::HpLevel| (level.master_db() * 100.0).round() as c_int;
    // SAFETY: plain integers, checked by the C side.
    let r = unsafe {
        rb_wired_masters(
            master(reborn_core::HpLevel::Low),
            master(reborn_core::HpLevel::Medium),
            master(reborn_core::HpLevel::High),
        )
    };
    if r < 0 {
        return Err(error(r));
    }
    // SAFETY: plain integers; the C side opens and closes its own handle.
    let r = unsafe {
        rb_wired_configure(
            level.index() as c_int,
            c_int::from(slow),
            c_int::from(phase),
            c_int::from(nos),
        )
    };
    if r < 0 {
        Err(error(r))
    } else {
        Ok(r > 0)
    }
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
    if !device.starts_with("bluealsa:") {
        return Ok(None);
    }
    // Y2 coordination with codec switching is platform policy.
    reborn_platform::bluetooth::playback_pcm_lease()
}
struct WritePacing {
    partial: bool,
    waiting: bool,
}
impl WritePacing {
    fn write(
        &mut self,
        requested: usize,
        mut wait: impl FnMut() -> c_int,
        mut write: impl FnMut() -> c_int,
    ) -> Result<usize, c_int> {
        let waited = self.waiting;
        if waited {
            let ready = wait();
            if ready == 0 {
                return Ok(0);
            }
            if ready < 0 {
                return Err(ready);
            }
            self.waiting = false;
        }
        let n = write();
        if n >= 0 {
            self.waiting = self.partial && (n as usize) < requested;
            return Ok(n as usize);
        }
        if n == -11 {
            // A ready notification can race the write. Keep one bounded wait
            // per call so the audio loop polls controls at least every 20 ms.
            if waited {
                self.waiting = true;
                return Ok(0);
            }
            let ready = wait();
            return if ready < 0 { Err(ready) } else { Ok(0) };
        }
        Err(n)
    }
}
pub struct AlsaSink {
    _pcm_lease: Option<std::fs::File>,
    raw: NonNull<c_void>,
    params: Parameters,
    log: Observer,
    correlation: u64,
    pacing: WritePacing,
}
impl AlsaSink {
    /// The wired sink, always at `WIRED_RATE` (the decoder converts).
    pub fn plan(
        output: &AudioOutput,
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
            crate::WIRED_RATE,
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
    // This boundary keeps the explicit ALSA probe/transport parameters together.
    #[allow(clippy::too_many_arguments)]
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
            speaker: false,
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
                "enabled_format",
                "ALSA sink supports the enabled software profile",
                Some(id),
                json!({"device":device,"format":actual.as_str(),"rate":raw.rate}),
            );
        }
        Ok((spec, params))
    }
    pub fn open(output: &AudioOutput, log: Observer, id: u64) -> Result<Self, String> {
        let (spec, _) = Self::plan(output, log.clone(), id)?;
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
        // 0 Bluetooth, 1 the headphone jack, 2 the internal speaker (#28).
        let route = if wired && planned.is_some_and(|spec| spec.speaker) {
            2
        } else {
            i32::from(wired)
        };
        // SAFETY: synchronous C constructor receives live input and output storage.
        let r = unsafe {
            rb_sink_open(
                name.as_ptr(),
                rate,
                native_format(pcm_format),
                route,
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
            pacing: WritePacing {
                partial: wired,
                waiting: false,
            },
        })
    }
    fn write_error(&mut self, n: c_int) -> Result<usize, String> {
        // A signal consumes no PCM. The audio loop can poll controls again.
        if n == -4 {
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
                self.pacing.waiting = false;
                self.log.add("audio_recoveries", 1.);
                return Ok(0);
            }
            return Err(error(r));
        }
        Err(error(n))
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
        // A short wired write fills available space. Repeatedly rendering
        // 512 frames for the few frames consumed during gain work can spin
        // without EAGAIN. Return accepted frames now; wait on the next call,
        // after the audio loop polls controls. Bluetooth keeps its old pacing.
        let requested = pcm.len() / frame_bytes;
        let raw = self.raw.as_ptr();
        match self.pacing.write(
            requested,
            // SAFETY: bounded wait on the handle uniquely owned by this sink.
            || unsafe { rb_sink_wait(raw, 20) },
            // SAFETY: live bytes and handle; requested count uses their frame stride.
            || unsafe { rb_sink_write(raw, pcm.as_ptr(), requested as c_int) },
        ) {
            Ok(n) => Ok(n),
            Err(n) => self.write_error(n),
        }
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
            self.pacing.waiting = false;
            Ok(())
        }
    }
    fn pause(&mut self, on: bool) -> Result<(), String> {
        // BlueALSA's own pause left AirPods silent after resume (Beta 6
        // device log): a Bluetooth stream drains and is prepared again.
        let device_pause = !self.params.device.starts_with("bluealsa");
        // SAFETY: caller uniquely owns the PCM context.
        let r = unsafe {
            rb_sink_pause(
                self.raw.as_ptr(),
                c_int::from(on),
                c_int::from(device_pause),
            )
        };
        self.log.emit(
            Level::Info,
            "alsa",
            if on { "paused" } else { "resumed" },
            if on {
                "PCM paused in place"
            } else {
                "PCM resumed"
            },
            Some(self.correlation),
            json!({"native_code":r,"device_pause":r == 1}),
        );
        self.pacing.waiting = false;
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
mod wired_level_tests {
    use super::*;
    use reborn_core::{DacFilter, HpLevel};

    /// The only test that touches the process-wide wired state. There is no
    /// Y2Audio card on the host, so configuring reports an error but still
    /// remembers the choice, as on the player before the card appears.
    #[test]
    fn a_louder_stream_or_a_chosen_rise_fades_in_but_the_startup_level_does_not() {
        let fades = |live| unsafe { rb_wired_note_live(live) } != 0;
        // Startup: the saved level (High) is the baseline, not a rise.
        let _ = wired_configure(HpLevel::High, DacFilter::Fast);
        // The same choice again keeps open wired sinks usable; another one
        // does not (the amplifier takes it at its next power-up).
        let epoch = wired_epoch();
        let _ = wired_configure(HpLevel::High, DacFilter::Fast);
        assert_eq!(wired_epoch(), epoch);
        let _ = wired_configure(HpLevel::High, DacFilter::Slow);
        assert_eq!(wired_epoch(), epoch + 1);
        let _ = wired_configure(HpLevel::High, DacFilter::Fast);
        assert_eq!(wired_epoch(), epoch + 2);
        assert!(!fades(2), "first stream after boot at the saved level");
        assert!(!fades(2));
        let _ = wired_configure(HpLevel::Low, DacFilter::Fast);
        assert!(!fades(0), "lower is never faded");
        // Chosen higher: the next stream fades, even when the comparison with
        // the previous stream alone could not tell (none since boot).
        let _ = wired_configure(HpLevel::Medium, DacFilter::Slow);
        assert!(fades(1));
        assert!(!fades(1), "only once");
        // A stream louder than the last one fades without a choice too.
        assert!(fades(2));
        // A rise chosen and then taken back before any stream: no fade at Low.
        let _ = wired_configure(HpLevel::High, DacFilter::Fast);
        let _ = wired_configure(HpLevel::Low, DacFilter::Fast);
        assert!(!fades(0));
        assert!(wired_configure(HpLevel::Low, DacFilter::Nos).is_err());
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn partial_write_pacing_bounds_waits_and_never_consumes_unwritten_frames() {
        use std::{cell::RefCell, collections::VecDeque};
        let waits = RefCell::new(VecDeque::from([0, -4, 1, 1, 0, -32, -19]));
        let writes = RefCell::new(VecDeque::from([73, -11, 512, 9]));
        let wait = || {
            waits
                .borrow_mut()
                .pop_front()
                .expect("unexpected extra wait")
        };
        let write = || {
            writes
                .borrow_mut()
                .pop_front()
                .expect("unexpected extra write")
        };
        let mut pacing = WritePacing {
            partial: true,
            waiting: false,
        };
        assert_eq!(pacing.write(512, wait, write), Ok(73));
        assert!(pacing.waiting);
        assert_eq!(pacing.write(512, wait, write), Ok(0)); // timeout: no write
        assert_eq!(pacing.write(512, wait, write), Err(-4)); // interrupted wait
        assert!(pacing.waiting);
        assert_eq!(pacing.write(512, wait, write), Ok(0)); // ready -> EAGAIN: one wait
        assert!(pacing.waiting);
        assert_eq!(pacing.write(512, wait, write), Ok(512));
        assert!(!pacing.waiting);
        assert_eq!(pacing.write(512, wait, write), Ok(9));
        assert_eq!(pacing.write(512, wait, write), Ok(0)); // timeout: still no write
        assert_eq!(pacing.write(512, wait, write), Err(-32));
        assert_eq!(pacing.write(512, wait, write), Err(-19));
        assert!(writes.borrow().is_empty());
        assert!(waits.borrow().is_empty());

        let mut bluetooth = WritePacing {
            partial: false,
            waiting: false,
        };
        assert_eq!(
            bluetooth.write(512, || panic!("BT short write must not wait"), || 73),
            Ok(73)
        );
        assert!(!bluetooth.waiting);
        assert_eq!(bluetooth.write(512, || 1, || -11), Ok(0));
    }

    #[test]
    fn readiness_and_recovery_preserve_pcm_frame_accounting() {
        let directory = std::env::temp_dir().join(format!("reborn-ready-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let log = Observer::new(&directory).unwrap();
        for format in [PcmFormat::S16LE, PcmFormat::S24LE, PcmFormat::S32LE] {
            let mut sink =
                AlsaSink::open_named("null", 48_000, format, false, None, log.clone(), 1).unwrap();
            let pcm = vec![0; 73 * format.bytes_per_frame()];
            sink.pacing.waiting = true;
            assert_eq!(sink.write(&pcm).unwrap(), 73);
            assert!(!sink.pacing.waiting);
            assert_eq!(sink.write_error(-4).unwrap(), 0);
            assert_eq!(sink.write_error(-32).unwrap(), 0);
            assert_eq!(sink.write(&pcm).unwrap(), 73);
            assert!(sink.write_error(-19).is_err());
            sink.pacing.waiting = true;
            sink.discard().unwrap();
            assert!(!sink.pacing.waiting);
            assert_eq!(sink.write(&pcm).unwrap(), 73);
        }
        drop(log);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn opened_pcm_preserves_the_planned_device_contract() {
        let directory =
            std::env::temp_dir().join(format!("reborn-open-contract-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let log = Observer::new(&directory).unwrap();
        for rate in [44_100, 48_000] {
            // ALSA's null PCM exercises the real C probe/open boundary without
            // requiring a sound card, a Bluetooth peer, or the wired mixer.
            let (spec, _) = AlsaSink::plan_named(
                "null",
                AudioOutput::Wired,
                rate,
                false,
                PcmFormat::S16LE,
                true,
                None,
                log.clone(),
                1,
            )
            .unwrap();
            let sink = AlsaSink::open_named(
                "null",
                rate,
                spec.format,
                false,
                Some(&spec),
                log.clone(),
                1,
            )
            .unwrap();
            let opened = sink.parameters();
            assert_eq!(opened.device, "null");
            assert_eq!(opened.rate, rate);
            assert_eq!(opened.format, spec.format);
            assert_eq!(opened.channels, 2);
            drop(sink);

            let mut wrong_device = spec;
            wrong_device.device = "a-different-pcm".into();
            assert!(AlsaSink::open_named(
                "null",
                rate,
                wrong_device.format,
                false,
                Some(&wrong_device),
                log.clone(),
                1,
            )
            .is_err());
        }
        drop(log);
        std::fs::remove_dir_all(directory).unwrap();
    }

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
    fn s32_alsa_contract_and_observed_bluetooth_plan_preserve_the_container() {
        assert_eq!(format_contract(2).unwrap(), PcmFormat::S32LE);
        let directory = std::env::temp_dir().join(format!("reborn-s32-{}", std::process::id()));
        let log = Observer::new(&directory).unwrap();
        let (spec, params) = AlsaSink::plan_named(
            "null",
            AudioOutput::Bluetooth("01:02:03:04:05:06".into()),
            96_000,
            false,
            PcmFormat::S32LE,
            true,
            None,
            log.clone(),
            1,
        )
        .unwrap();
        assert_eq!(spec.format, PcmFormat::S32LE);
        assert_eq!(
            (spec.valid_bits, spec.physical_bits, spec.rate),
            (32, 32, 96_000)
        );
        assert!(!params.fallback);
        let mut sink = AlsaSink::open_named(
            "null",
            96_000,
            PcmFormat::S32LE,
            false,
            Some(&spec),
            log.clone(),
            1,
        )
        .unwrap();
        // Low bits are deliberately nonzero; this exercises the same ALSA
        // frame contract used by real sinks, without claiming DAC capture.
        let frames: Vec<u8> = [256_i32, -256, 512, -512]
            .into_iter()
            .flat_map(i32::to_le_bytes)
            .collect();
        assert_eq!(sink.write(&frames).unwrap(), 2);
        drop(sink);
        drop(log);
        std::fs::remove_dir_all(directory).unwrap();
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
