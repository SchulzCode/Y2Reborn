use super::{valid_address, AudioSink, Parameters};
use reborn_core::AudioOutput;
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
}
unsafe extern "C" {
    fn rb_alsa_logging(callback: extern "C" fn(*const c_char));
    fn rb_alsa_error(e: c_int) -> *const c_char;
    fn rb_wired_device(p: *mut c_char, n: c_int) -> c_int;
    fn rb_sink_open(
        name: *const c_char,
        rate: u32,
        wired: c_int,
        out: *mut *mut c_void,
        params: *mut RawParams,
    ) -> c_int;
    fn rb_sink_close(p: *mut c_void);
    fn rb_sink_write(p: *mut c_void, b: *const i16, frames: c_int) -> c_int;
    fn rb_sink_wait(p: *mut c_void, ms: c_int) -> c_int;
    fn rb_sink_recover(p: *mut c_void, e: c_int) -> c_int;
    fn rb_sink_drop(p: *mut c_void) -> c_int;
    fn rb_sink_delay(p: *mut c_void) -> c_int;
}
fn error(e: c_int) -> String {
    // SAFETY: snd_strerror provides a static NUL-terminated string for any error code.
    format!(
        "ALSA {e}: {}",
        unsafe { CStr::from_ptr(rb_alsa_error(e)) }.to_string_lossy()
    )
}
pub fn wired_device() -> Result<String, String> {
    let mut b = [0; 128]; // SAFETY: C receives writable storage with matching capacity.
    let r = unsafe { rb_wired_device(b.as_mut_ptr(), b.len() as c_int) };
    if r < 0 {
        Err(error(r))
    } else {
        // SAFETY: successful rb_wired_device uses snprintf which NUL terminates.
        Ok(unsafe { CStr::from_ptr(b.as_ptr()) }
            .to_string_lossy()
            .into_owned())
    }
}
pub struct AlsaSink {
    raw: NonNull<c_void>,
    params: Parameters,
    log: Observer,
    correlation: u64,
}
impl AlsaSink {
    pub fn open(output: &AudioOutput, rate: u32, log: Observer, id: u64) -> Result<Self, String> {
        let (wired, name) = match output {
            AudioOutput::Wired => (true, wired_device()?),
            AudioOutput::Bluetooth(address) => {
                if !valid_address(address) {
                    return Err("invalid Bluetooth address".into());
                }
                (false, format!("bluealsa:DEV={address},PROFILE=a2dp"))
            }
        };
        Self::open_named(&name, rate, wired, log, id)
    }
    pub fn open_named(
        name: &str,
        rate: u32,
        wired: bool,
        log: Observer,
        id: u64,
    ) -> Result<Self, String> {
        let name = CString::new(name).map_err(|_| "NUL in PCM device")?;
        let mut raw = std::ptr::null_mut();
        let mut p = RawParams {
            rate: 0,
            period: 0,
            buffer: 0,
        };
        // SAFETY: synchronous C constructor receives live CString and output storage; successful handle has one Rust owner.
        let r = unsafe {
            rb_sink_open(
                name.as_ptr(),
                rate,
                if wired { 1 } else { 0 },
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
                json!({"native_code":r,"device":name.to_string_lossy(),"recovery_attempted":false}),
            );
            return Err(error(r));
        }
        let params = Parameters {
            rate: p.rate,
            period: p.period,
            buffer: p.buffer,
        };
        log.emit(
            Level::Info,
            "alsa",
            "prepared",
            "PCM open and parameters negotiated",
            Some(id),
            json!({"device":name.to_string_lossy(),"params":params,"format":"S16_LE","channels":2}),
        );
        Ok(Self {
            raw: NonNull::new(raw).ok_or("null ALSA handle")?,
            params,
            log,
            correlation: id,
        })
    }
}
impl AudioSink for AlsaSink {
    fn parameters(&self) -> Parameters {
        self.params
    }
    fn write(&mut self, samples: &[i16]) -> Result<usize, String> {
        if !samples.len().is_multiple_of(2) || samples.len() > 131072 {
            return Err("PCM block bounds".into());
        }
        // SAFETY: unique initialized PCM handle; slice has stereo frames and stays alive during the nonblocking write.
        let n = unsafe {
            rb_sink_write(
                self.raw.as_ptr(),
                samples.as_ptr(),
                (samples.len() / 2) as c_int,
            )
        };
        if n >= 0 {
            return Ok(n as usize);
        }
        if n == -11 {
            // SAFETY: wait is bounded to 20ms and handle remains owned.
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
            // SAFETY: recovery mutates only the uniquely borrowed PCM context.
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
        // SAFETY: caller uniquely owns the PCM context and intentionally drops pending audio.
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
        // SAFETY: same-thread ALSA query; no other code accesses this context.
        unsafe { rb_sink_delay(self.raw.as_ptr()) }.max(0) as u64
    }
}
impl Drop for AlsaSink {
    fn drop(&mut self) {
        // SAFETY: unique owner closes exactly once and restores its saved mixer controls.
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
    // SAFETY: C supplies a terminated local buffer, valid until callback returns.
    let text = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    if let Some(log) = LOGGER.get() {
        log.emit(Level::Warn, "alsa", "library_log", &text, None, json!({}));
    }
}
pub fn initialize_logging(log: Observer) {
    if LOGGER.set(log).is_ok() {
        // SAFETY: callback has process lifetime and is registered before audio workers start.
        unsafe { rb_alsa_logging(alsa_log) }
    }
}
