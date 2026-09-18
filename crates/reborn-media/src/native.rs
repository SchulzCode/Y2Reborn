//! Reviewed ownership boundary: opaque C contexts are unique and thread confined;
//! cancellation is a separate C11 atomic allocation shared by Arc.
use reborn_observability::{Level, Observer};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    path::Path,
    ptr::NonNull,
    sync::{Arc, OnceLock},
};
#[repr(C)]
struct RawMetadata {
    title: [c_char; 512],
    artist: [c_char; 512],
    album: [c_char; 512],
    album_artist: [c_char; 512],
    codec: [c_char; 64],
    duration_ms: u64,
    bitrate: u64,
    rate: u32,
    channels: u32,
    track: u32,
    disc: u32,
    artwork: u32,
}
unsafe extern "C" {
    fn rb_cancel_new() -> *mut c_void;
    fn rb_cancel_free(p: *mut c_void);
    fn rb_cancel_set(p: *mut c_void);
    fn rb_media_logging(
        f: extern "C" fn(c_int, *const c_char),
        enabled: extern "C" fn(c_int) -> c_int,
    );
    fn rb_media_version() -> *const c_char;
    fn rb_media_error(code: c_int, out: *mut c_char, size: c_int);
    fn rb_media_open(
        path: *const c_char,
        rate: c_int,
        cancel: *mut c_void,
        out: *mut *mut c_void,
        meta: *mut RawMetadata,
    ) -> c_int;
    fn rb_media_close(p: *mut c_void);
    fn rb_media_read(
        p: *mut c_void,
        out: *mut i16,
        capacity: c_int,
        packets: *mut u64,
        frames: *mut u64,
        position: *mut i64,
    ) -> c_int;
    fn rb_media_seek(p: *mut c_void, ms: i64) -> c_int;
    fn rb_media_art(p: *mut c_void, out: *mut u8, side: c_int) -> c_int;
}
static LOG: OnceLock<Observer> = OnceLock::new();
fn native_level(level: c_int) -> Level {
    if level <= 16 {
        Level::Error
    } else if level <= 24 {
        Level::Warn
    } else if level <= 32 {
        Level::Info
    } else if level <= 48 {
        Level::Debug
    } else {
        Level::Trace
    }
}
extern "C" fn log_enabled(level: c_int) -> c_int {
    i32::from(
        LOG.get()
            .is_some_and(|log| log.enabled("ffmpeg", native_level(level))),
    )
}
extern "C" fn log_callback(level: c_int, message: *const c_char) {
    if message.is_null() {
        return;
    }
    if let Some(log) = LOG.get() {
        // SAFETY: FFmpeg bridge supplies a NUL-terminated stack buffer valid for this call.
        let text = unsafe { CStr::from_ptr(message) }.to_string_lossy();
        log.emit(
            native_level(level),
            "ffmpeg",
            "library_log",
            &text,
            None,
            json!({"native_level":level}),
        );
    }
}
pub fn initialize_logging(log: Observer) {
    if LOG.set(log).is_ok() {
        // SAFETY: callback has static lifetime; initialized exactly once before decoder workers start.
        unsafe { rb_media_logging(log_callback, log_enabled) }
    }
}
pub fn version() -> String {
    // SAFETY: FFmpeg returns an immutable process-lifetime version string.
    unsafe { CStr::from_ptr(rb_media_version()) }
        .to_string_lossy()
        .into_owned()
}
fn error(code: c_int) -> String {
    let mut b = [0 as c_char; 256]; // SAFETY: output buffer has exactly the supplied capacity, C guarantees NUL termination.
    unsafe {
        rb_media_error(code, b.as_mut_ptr(), b.len() as c_int);
        format!(
            "FFmpeg {code}: {}",
            CStr::from_ptr(b.as_ptr()).to_string_lossy()
        )
    }
}
struct CancelInner(NonNull<c_void>);
// SAFETY: C allocation contains only C11 atomics; shared operations are atomic and Drop occurs after all Arc owners leave.
unsafe impl Send for CancelInner {}
// SAFETY: cancellation setters and interrupt reader access only C11 atomics.
unsafe impl Sync for CancelInner {}
impl Drop for CancelInner {
    fn drop(&mut self) {
        // SAFETY: this is the final Arc owner; decoders retain an Arc until closed.
        unsafe { rb_cancel_free(self.0.as_ptr()) }
    }
}
#[derive(Clone)]
pub struct Cancel(Arc<CancelInner>);
impl Cancel {
    pub fn new() -> Result<Self, String> {
        // SAFETY: constructor returns a new exclusively owned C allocation or null.
        let p = unsafe { rb_cancel_new() };
        NonNull::new(p)
            .map(|p| Self(Arc::new(CancelInner(p))))
            .ok_or("cancellation allocation failed".into())
    }
    pub fn cancel(&self) {
        // SAFETY: Arc retains this C11 atomic allocation for the call.
        unsafe { rb_cancel_set(self.0 .0.as_ptr()) }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Metadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub codec: String,
    pub duration_ms: u64,
    pub bitrate: u64,
    pub sample_rate: u32,
    pub channels: u32,
    pub track: u32,
    pub disc: u32,
    pub artwork: bool,
}
pub struct Decoder {
    raw: NonNull<c_void>,
    _cancel: Cancel,
    pub metadata: Metadata,
    rate: u32,
}
pub struct Pcm {
    pub samples: Vec<i16>,
    pub position_ms: u64,
    pub packets: u64,
    pub frames: u64,
    pub rate: u32,
}
impl Decoder {
    pub fn open(path: &Path, rate: u32, cancel: Cancel) -> Result<Self, String> {
        if ![44100, 48000].contains(&rate) {
            return Err("unsupported sink rate".into());
        }
        if !path.is_file() {
            return Err("media must be an existing regular file".into());
        }
        use std::os::unix::ffi::OsStrExt;
        let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| "NUL in path")?;
        let mut ptr = std::ptr::null_mut();
        // SAFETY: RawMetadata consists entirely of integer/byte fields, zero is a valid initial value.
        let mut meta: RawMetadata = unsafe { std::mem::zeroed() };
        // SAFETY: name and output storage live through this synchronous call; retained cancellation is owned by Decoder.
        let r = unsafe {
            rb_media_open(
                name.as_ptr(),
                rate as c_int,
                cancel.0 .0.as_ptr(),
                &mut ptr,
                &mut meta,
            )
        };
        if r < 0 {
            return Err(error(r));
        }
        fn text(b: &[c_char]) -> String {
            let v = b
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c as u8)
                .collect::<Vec<_>>();
            String::from_utf8_lossy(&v).into_owned()
        }
        Ok(Self {
            raw: NonNull::new(ptr).ok_or("missing decoder")?,
            _cancel: cancel,
            rate,
            metadata: Metadata {
                title: text(&meta.title),
                artist: text(&meta.artist),
                album: text(&meta.album),
                album_artist: text(&meta.album_artist),
                codec: text(&meta.codec),
                duration_ms: meta.duration_ms,
                bitrate: meta.bitrate,
                sample_rate: meta.rate,
                channels: meta.channels,
                track: meta.track,
                disc: meta.disc,
                artwork: meta.artwork != 0,
            },
        })
    }
    pub fn read(&mut self) -> Result<Option<Pcm>, String> {
        let mut v = vec![0i16; 4096];
        let (mut p, mut f, mut pos) = (0, 0, 0);
        // SAFETY: unique decoder borrow; buffer is sized for 2048 stereo frames, all outputs are valid local storage.
        let n = unsafe {
            rb_media_read(
                self.raw.as_ptr(),
                v.as_mut_ptr(),
                2048,
                &mut p,
                &mut f,
                &mut pos,
            )
        };
        if n < 0 {
            return Err(error(n));
        }
        if n == 0 {
            return Ok(None);
        }
        v.truncate(n as usize * 2);
        Ok(Some(Pcm {
            samples: v,
            position_ms: pos.max(0) as u64,
            packets: p,
            frames: f,
            rate: self.rate,
        }))
    }
    pub fn seek(&mut self, ms: u64) -> Result<(), String> {
        if ms > i64::MAX as u64 / 1000 {
            return Err("seek overflow".into());
        }
        // SAFETY: uniquely borrowed initialized context and range-checked timestamp.
        let r = unsafe { rb_media_seek(self.raw.as_ptr(), ms as i64) };
        if r < 0 {
            Err(error(r))
        } else {
            Ok(())
        }
    }
    pub fn artwork(&mut self) -> Result<Vec<u8>, String> {
        let mut b = vec![0; 160 * 160 * 4];
        // SAFETY: C is given exactly side*side*4 writable bytes and a unique live decoder context.
        let r = unsafe { rb_media_art(self.raw.as_ptr(), b.as_mut_ptr(), 160) };
        if r < 0 {
            Err(error(r))
        } else {
            Ok(b)
        }
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: unique context is closed once, before retained cancellation Arc is released.
        unsafe { rb_media_close(self.raw.as_ptr()) }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(n: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/fixtures")
            .join(n)
    }
    #[test]
    fn fixture_formats() {
        for n in [
            "tone.wav",
            "tone.flac",
            "tone.mp3",
            "tone.m4a",
            "tone.ogg",
            "tone.opus",
        ] {
            let mut d = Decoder::open(&fixture(n), 48000, Cancel::new().unwrap()).unwrap();
            let mut frames = 0;
            while let Some(p) = d.read().unwrap() {
                frames += p.samples.len() / 2;
            }
            assert!((45000..55000).contains(&frames), "{n}: {frames}");
        }
    }
    #[test]
    fn malformed_is_error() {
        assert!(Decoder::open(&fixture("malformed.mp3"), 44100, Cancel::new().unwrap()).is_err())
    }
    #[test]
    fn cancellation_and_seek() {
        let c = Cancel::new().unwrap();
        let mut d = Decoder::open(&fixture("tone.flac"), 44100, c.clone()).unwrap();
        d.seek(500).unwrap();
        assert!(d.read().unwrap().unwrap().position_ms >= 499);
        c.cancel();
        for _ in 0..100 {
            if d.read().is_err() {
                return;
            }
        }
        panic!("cancellation not observed")
    }
}

#[cfg(test)]
mod corruption_tests {
    use super::*;
    #[test]
    fn truncated_containers_do_not_crash() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
        let temp = std::env::temp_dir().join(format!("reborn-truncation-{}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        for name in [
            "tone.wav",
            "tone.flac",
            "tone.mp3",
            "tone.m4a",
            "tone.ogg",
            "tone.opus",
        ] {
            let data = std::fs::read(root.join(name)).unwrap();
            for length in [0, 8, 64, data.len() / 2] {
                let path = temp.join(name);
                std::fs::write(&path, &data[..length]).unwrap();
                if let Ok(mut d) = Decoder::open(&path, 48000, Cancel::new().unwrap()) {
                    for _ in 0..128 {
                        match d.read() {
                            Ok(Some(_)) => {}
                            _ => break,
                        }
                    }
                }
            }
        }
        std::fs::remove_dir_all(temp).unwrap();
    }
    #[test]
    fn cancel_before_open_is_an_error() {
        let c = Cancel::new().unwrap();
        c.cancel();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac");
        assert!(Decoder::open(&path, 48000, c).is_err());
    }
    #[test]
    fn nonfile_and_rate_rejected() {
        assert!(Decoder::open(Path::new("/dev/zero"), 48000, Cancel::new().unwrap()).is_err());
        assert!(Decoder::open(Path::new("/missing"), 96000, Cancel::new().unwrap()).is_err());
    }
}
