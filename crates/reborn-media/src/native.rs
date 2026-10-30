//! FFmpeg 9 media boundary.
//!
//! The C side is intentionally small and opaque. It exposes one canonical
//! stereo PCM stream after an FFmpeg FLTP filter graph and one final
//! libswresample conversion. Rust owns cancellation, worker scheduling and
//! observability; it does not decode or apply audio gain itself.
use reborn_core::{EqBand, PcmFormat, ReplayGainMode};
use reborn_observability::{Level, Observer};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    ffi::{c_char, c_double, c_int, c_void, CStr, CString},
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
};

#[repr(C)]
struct RawMetadata {
    title: [c_char; 512],
    artist: [c_char; 512],
    album: [c_char; 512],
    album_artist: [c_char; 512],
    codec: [c_char; 64],
    source_sample_fmt: [c_char; 32],
    decoder_sample_fmt: [c_char; 32],
    internal_sample_fmt: [c_char; 32],
    final_sample_fmt: [c_char; 32],
    filters: [c_char; 1024],
    resample_reason: [c_char; 256],
    duration_ms: u64,
    bitrate: u64,
    skip_start: u64,
    skip_end: u64,
    corrupt_packets: u64,
    rate: u32,
    channels: u32,
    source_bits: u32,
    output_rate: u32,
    output_channels: u32,
    track: u32,
    disc: u32,
    artwork: u32,
    output_format: u32,
    resampling: u32,
    format_conversion: u32,
    replaygain_mode: u32,
    eq_active: u32,
    track_gain_db: c_double,
    album_gain_db: c_double,
    track_peak: c_double,
    album_peak: c_double,
    applied_gain_db: c_double,
    headroom_db: c_double,
    date: [c_char; 128],
    genre: [c_char; 512],
    open_us: u64,
    probe_us: u64,
    io_bytes: u64,
    info_runs: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct RawEqBand {
    frequency_hz: c_double,
    gain_db: c_double,
    q: c_double,
    /// 0 peaking, 1 low shelf, 2 high shelf.
    kind: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct RawDspConfig {
    replay_gain_mode: c_int,
    volume: u32,
    eq_count: u32,
    eq: [RawEqBand; reborn_core::eq::MAX_BANDS],
    crossfade_ms: u32,
    preamp_db: c_double,
    speed: c_double,
}
// Pinned with RbDspConfig in native/media.c (a _Static_assert there).
const _: () = assert!(
    std::mem::size_of::<RawDspConfig>() == 360
        && std::mem::offset_of!(RawDspConfig, eq) == 16
        && std::mem::size_of::<RawEqBand>() == 32
        && std::mem::offset_of!(RawEqBand, kind) == 24
        && std::mem::offset_of!(RawDspConfig, crossfade_ms) == 336
        && std::mem::offset_of!(RawDspConfig, preamp_db) == 344
        && std::mem::offset_of!(RawDspConfig, speed) == 352
);

type CancelNew = unsafe extern "C" fn() -> *mut c_void;
type CancelFree = unsafe extern "C" fn(*mut c_void);
type CancelSet = unsafe extern "C" fn(*mut c_void);
type MediaLogging =
    unsafe extern "C" fn(extern "C" fn(c_int, *const c_char), extern "C" fn(c_int) -> c_int);
type MediaComponents = unsafe extern "C" fn(*mut c_char, c_int) -> c_int;
type MediaError = unsafe extern "C" fn(c_int, *mut c_char, c_int);
type MediaOpen = unsafe extern "C" fn(
    *const c_char,
    c_int,
    c_int,
    *const RawDspConfig,
    *mut c_void,
    *mut *mut c_void,
    *mut RawMetadata,
) -> c_int;
type MediaInspect =
    unsafe extern "C" fn(*const c_char, *mut c_void, *mut *mut c_void, *mut RawMetadata) -> c_int;
type MediaLimits = unsafe extern "C" fn();
type MediaClose = unsafe extern "C" fn(*mut c_void);
type MediaRead =
    unsafe extern "C" fn(*mut c_void, *mut u8, c_int, *mut u64, *mut u64, *mut i64) -> c_int;
type MediaSeek = unsafe extern "C" fn(*mut c_void, i64) -> c_int;
type MediaArtwork = unsafe extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int;
type MediaChapters = unsafe extern "C" fn(*mut c_void, *mut RawChapter, c_int) -> c_int;
type MediaArtworkFile = unsafe extern "C" fn(*const c_char, *mut u8, c_int) -> c_int;
type MediaImageFile =
    unsafe extern "C" fn(*const c_char, *mut u8, c_int, c_int, *mut c_int, *mut c_int) -> c_int;
type MediaConvert =
    unsafe extern "C" fn(*const u8, c_int, c_int, c_int, c_int, *mut u8, c_int) -> c_int;
type MediaCrossfade = unsafe extern "C" fn(
    *const u8,
    *const u8,
    c_int,
    c_int,
    c_int,
    *mut u8,
    c_int,
    *mut c_int,
) -> c_int;

struct MediaApi {
    // Kept open for the whole process. Decoder contexts can outlive the call
    // which first loaded this membrane, so unloading is deliberately absent.
    _handle: *mut c_void,
    cancel_new: CancelNew,
    cancel_free: CancelFree,
    cancel_set: CancelSet,
    logging: MediaLogging,
    components: MediaComponents,
    error: MediaError,
    open: MediaOpen,
    inspect: MediaInspect,
    limits: MediaLimits,
    close: MediaClose,
    read: MediaRead,
    seek: MediaSeek,
    artwork: MediaArtwork,
    chapters: MediaChapters,
    artwork_file: MediaArtworkFile,
    logo_file: MediaArtworkFile,
    image_file: MediaImageFile,
    convert: MediaConvert,
    crossfade: MediaCrossfade,
}

// The loaded library is process lifetime state. FFmpeg contexts remain
// separately owned by their calling worker and retain their existing
// cancellation/thread-safety contract.
unsafe impl Send for MediaApi {}
unsafe impl Sync for MediaApi {}

impl MediaApi {
    unsafe fn symbol<T: Copy>(handle: *mut c_void, name: &'static [u8]) -> Result<T, String> {
        let ptr = libc::dlsym(handle, name.as_ptr().cast());
        if ptr.is_null() {
            return Err(format!(
                "FFmpeg media membrane symbol {} is missing",
                String::from_utf8_lossy(&name[..name.len().saturating_sub(1)])
            ));
        }
        // POSIX specifies dlsym results as usable function addresses. A
        // transmute_copy is required because Rust keeps data and function
        // pointers as distinct types even on this target.
        Ok(std::mem::transmute_copy(&ptr))
    }

    fn load() -> Result<Self, String> {
        let mut candidates = Vec::<PathBuf>::new();
        if let Ok(path) = std::env::var("REBORN_MEDIA_LIB") {
            candidates.push(path.into());
        }
        if let Some(path) = option_env!("REBORN_MEDIA_BUILD_LIB") {
            candidates.push(path.into());
        }
        candidates.extend([
            PathBuf::from("/usr/lib/reborn/libreborn_media.so"),
            PathBuf::from("/usr/lib/libreborn_media.so"),
        ]);
        let mut last_error = String::from("no FFmpeg media membrane path succeeded");
        for path in candidates {
            let Ok(name) = CString::new(path.to_string_lossy().as_bytes()) else {
                continue;
            };
            // SAFETY: path is NUL-free and the handle remains open for the
            // lifetime of the returned API.
            let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                let detail = unsafe { libc::dlerror() };
                if !detail.is_null() {
                    last_error = unsafe { CStr::from_ptr(detail) }
                        .to_string_lossy()
                        .into_owned();
                }
                continue;
            }
            let loaded = unsafe {
                Ok(Self {
                    _handle: handle,
                    cancel_new: Self::symbol(handle, b"rb_cancel_new\0")?,
                    cancel_free: Self::symbol(handle, b"rb_cancel_free\0")?,
                    cancel_set: Self::symbol(handle, b"rb_cancel_set\0")?,
                    logging: Self::symbol(handle, b"rb_media_logging\0")?,
                    components: Self::symbol(handle, b"rb_media_components\0")?,
                    error: Self::symbol(handle, b"rb_media_error\0")?,
                    open: Self::symbol(handle, b"rb_media_open\0")?,
                    inspect: Self::symbol(handle, b"rb_media_inspect\0")?,
                    limits: Self::symbol(handle, b"rb_media_limits\0")?,
                    close: Self::symbol(handle, b"rb_media_close\0")?,
                    read: Self::symbol(handle, b"rb_media_read\0")?,
                    seek: Self::symbol(handle, b"rb_media_seek\0")?,
                    artwork: Self::symbol(handle, b"rb_media_art\0")?,
                    chapters: Self::symbol(handle, b"rb_media_chapters\0")?,
                    artwork_file: Self::symbol(handle, b"rb_media_art_file\0")?,
                    logo_file: Self::symbol(handle, b"rb_media_logo_file\0")?,
                    image_file: Self::symbol(handle, b"rb_media_image_file\0")?,
                    convert: Self::symbol(handle, b"rb_media_convert\0")?,
                    crossfade: Self::symbol(handle, b"rb_media_crossfade\0")?,
                })
            };
            match loaded {
                Ok(api) => return Ok(api),
                Err(error) => {
                    last_error = error;
                    // The process never unloads a successful membrane. A
                    // partially resolved handle is safe to release here.
                    unsafe { libc::dlclose(handle) };
                }
            }
        }
        Err(last_error)
    }
}

static API: OnceLock<Result<MediaApi, String>> = OnceLock::new();

fn api() -> Result<&'static MediaApi, String> {
    match API.get_or_init(|| {
        let api = MediaApi::load()?;
        // SAFETY: OnceLock serializes this process-wide allocation limit before worker access.
        unsafe { (api.limits)() };
        if LOG.get().is_some() {
            // SAFETY: the callbacks are static and the loaded membrane stays
            // resident for the lifetime of the process.
            unsafe { (api.logging)(log_callback, log_enabled) };
        }
        Ok(api)
    }) {
        Ok(api) => Ok(api),
        Err(error) => Err(error.clone()),
    }
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
        // SAFETY: FFmpeg bridge supplies a terminated buffer for this call.
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
        if let Some(Ok(api)) = API.get() {
            // SAFETY: callbacks have static lifetime and the loaded membrane
            // remains resident for the process lifetime.
            unsafe { (api.logging)(log_callback, log_enabled) }
        }
    }
}
pub const FFMPEG_VERSION: &str = include_str!("../../../FFMPEG_VERSION");
pub fn version() -> String {
    // This constant is the pinned production FFmpeg ABI. The actual loaded
    // library version is exposed by runtime_components(), which is used by
    // status/diagnostics after the lazy membrane has been opened.
    FFMPEG_VERSION.into()
}
fn components(api: &MediaApi) -> Result<Value, String> {
    let mut bytes = vec![0 as c_char; 512 * 1024];
    // SAFETY: the C function writes at most the supplied capacity and NUL terminates JSON.
    let n = unsafe { (api.components)(bytes.as_mut_ptr(), bytes.len() as c_int) };
    if n < 0 {
        return Err(error(api, n));
    }
    let json = unsafe { CStr::from_ptr(bytes.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    serde_json::from_str(&json).map_err(|e| format!("FFmpeg component manifest: {e}"))
}
pub fn runtime_components() -> Result<Value, String> {
    components(api()?)
}
pub fn runtime_components_if_loaded() -> Option<Value> {
    API.get()
        .and_then(|result| result.as_ref().ok())
        .and_then(|api| components(api).ok())
}
pub fn convert_pcm(
    input: &[u8],
    rate: u32,
    input_format: PcmFormat,
    output_format: PcmFormat,
) -> Result<Vec<u8>, String> {
    let input_frame_bytes = input_format.bytes_per_frame();
    if input.is_empty() || !input.len().is_multiple_of(input_frame_bytes) {
        return Err("PCM conversion input is not frame aligned".into());
    }
    let api = api()?;
    let frames = input.len() / input_frame_bytes;
    let mut output = vec![0u8; frames * output_format.bytes_per_frame()];
    let frames_i32 = i32::try_from(frames).map_err(|_| "PCM conversion is too large")?;
    let r = unsafe {
        (api.convert)(
            input.as_ptr(),
            frames_i32,
            rate as c_int,
            match input_format {
                PcmFormat::S16LE => 1,
                PcmFormat::S24LE => 3,
                PcmFormat::S32LE => 2,
            },
            match output_format {
                PcmFormat::S16LE => 1,
                PcmFormat::S24LE => 3,
                PcmFormat::S32LE => 2,
            },
            output.as_mut_ptr(),
            frames_i32,
        )
    };
    if r < 0 {
        return Err(error(api, r));
    }
    output.truncate(r as usize * output_format.bytes_per_frame());
    Ok(output)
}
pub fn crossfade_pcm(
    a: &[u8],
    b: &[u8],
    frames: usize,
    rate: u32,
    format: PcmFormat,
) -> Result<Vec<u8>, String> {
    if frames == 0
        || a.len() != frames * format.bytes_per_frame()
        || b.len() != frames * format.bytes_per_frame()
    {
        return Err("crossfade input is not frame aligned".into());
    }
    let api = api()?;
    let frames_i32 = i32::try_from(frames).map_err(|_| "crossfade window is too large")?;
    let mut output = vec![0u8; frames * format.bytes_per_frame()];
    let mut written = 0;
    let r = unsafe {
        (api.crossfade)(
            a.as_ptr(),
            b.as_ptr(),
            frames_i32,
            rate as c_int,
            match format {
                PcmFormat::S16LE => 1,
                PcmFormat::S24LE => 3,
                PcmFormat::S32LE => 2,
            },
            output.as_mut_ptr(),
            frames_i32,
            &mut written,
        )
    };
    if r < 0 {
        return Err(error(api, r));
    }
    output.truncate(written.max(0) as usize * format.bytes_per_frame());
    Ok(output)
}
fn error(api: &MediaApi, code: c_int) -> String {
    let mut b = [0 as c_char; 256];
    // SAFETY: C receives a writable fixed buffer and promises NUL termination.
    unsafe {
        (api.error)(code, b.as_mut_ptr(), b.len() as c_int);
        format!(
            "FFmpeg {code}: {}",
            CStr::from_ptr(b.as_ptr()).to_string_lossy()
        )
    }
}

struct CancelInner {
    raw: NonNull<c_void>,
    api: &'static MediaApi,
    cancelled: AtomicBool,
}
// SAFETY: the allocation contains only C11 atomics and is reference counted.
unsafe impl Send for CancelInner {}
// SAFETY: cancellation access is atomic on the C side.
unsafe impl Sync for CancelInner {}
impl Drop for CancelInner {
    fn drop(&mut self) {
        // SAFETY: this is the final Arc owner.
        unsafe { (self.api.cancel_free)(self.raw.as_ptr()) }
    }
}
#[derive(Clone)]
pub struct Cancel(Arc<CancelInner>);
impl Cancel {
    pub fn new() -> Result<Self, String> {
        let api = api()?;
        // SAFETY: constructor returns a new allocation or null.
        let p = unsafe { (api.cancel_new)() };
        NonNull::new(p)
            .map(|raw| {
                Self(Arc::new(CancelInner {
                    raw,
                    api,
                    cancelled: AtomicBool::new(false),
                }))
            })
            .ok_or_else(|| "cancellation allocation failed".into())
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        // SAFETY: Arc keeps the atomic allocation alive for this call.
        unsafe { (self.0.api.cancel_set)(self.0.raw.as_ptr()) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DspConfig {
    pub volume: u8,
    pub replay_gain: ReplayGainMode,
    pub eq_enabled: bool,
    pub eq_bands: Vec<EqBand>,
    pub crossfade_ms: u32,
    /// Gain before the equalizer (dB, ≤ 0): a headphone profile's preamp.
    #[serde(default)]
    pub preamp_db: f32,
    /// Playback speed of an audiobook, 0.5..2.0; 0 (unset) and 1 are normal.
    #[serde(default)]
    pub speed: f32,
}
impl DspConfig {
    /// The speed the filter applies: 1.0 unless a valid one is set.
    pub fn tempo(&self) -> f64 {
        if (0.5..=2.0).contains(&self.speed) {
            f64::from(self.speed)
        } else {
            1.0
        }
    }
    pub fn with_volume(volume: u8) -> Self {
        Self {
            volume,
            ..Default::default()
        }
    }
    fn raw(&self) -> RawDspConfig {
        let mut eq = [RawEqBand {
            frequency_hz: 0.0,
            gain_db: 0.0,
            q: 1.0,
            kind: 0,
        }; reborn_core::eq::MAX_BANDS];
        let bands = if self.eq_enabled {
            self.eq_bands
                .iter()
                .take(reborn_core::eq::MAX_BANDS)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let eq_count = bands.len() as u32;
        for (dst, src) in eq.iter_mut().zip(bands) {
            *dst = RawEqBand {
                frequency_hz: src.frequency_hz as c_double,
                gain_db: src.gain_db as c_double,
                q: src.q as c_double,
                kind: match src.kind {
                    reborn_core::EqKind::Peak => 0,
                    reborn_core::EqKind::LowShelf => 1,
                    reborn_core::EqKind::HighShelf => 2,
                },
            };
        }
        RawDspConfig {
            replay_gain_mode: match self.replay_gain {
                ReplayGainMode::Off => 0,
                ReplayGainMode::Track => 1,
                ReplayGainMode::Album => 2,
            },
            volume: self.volume.min(100) as u32,
            eq_count,
            eq,
            crossfade_ms: self.crossfade_ms.min(30_000),
            preamp_db: if self.eq_enabled {
                c_double::from(self.preamp_db.clamp(-60.0, 0.0))
            } else {
                0.0
            },
            speed: self.tempo(),
        }
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct OutputSpec {
    pub rate: u32,
    pub format: PcmFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Metadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub codec: String,
    pub source_sample_fmt: String,
    pub decoder_sample_fmt: String,
    pub internal_sample_fmt: String,
    pub final_sample_fmt: String,
    pub filters: String,
    pub resample_reason: String,
    pub duration_ms: u64,
    pub bitrate: u64,
    pub sample_rate: u32,
    pub channels: u32,
    pub source_bits: u32,
    pub output_rate: u32,
    pub output_channels: u32,
    pub track: u32,
    pub disc: u32,
    pub artwork: bool,
    pub output_format: PcmFormat,
    pub resampling: bool,
    pub format_conversion: bool,
    pub eq_active: bool,
    pub replaygain_mode: ReplayGainMode,
    pub track_gain_db: f64,
    pub album_gain_db: f64,
    pub track_peak: f64,
    pub album_peak: f64,
    pub applied_gain_db: f64,
    pub headroom_db: f64,
    pub skip_start: u64,
    pub skip_end: u64,
    pub corrupt_packets: u64,
    pub date: String,
    pub genre: String,
    /// Native open/analysis times and AVIO bytes, for product-path benchmarks.
    pub open_us: u64,
    pub probe_us: u64,
    pub io_bytes: u64,
    pub info_runs: u32,
}

impl Metadata {
    fn from_raw(meta: &RawMetadata) -> Self {
        Metadata {
            title: text(&meta.title),
            artist: text(&meta.artist),
            album: text(&meta.album),
            album_artist: text(&meta.album_artist),
            codec: text(&meta.codec),
            source_sample_fmt: text(&meta.source_sample_fmt),
            decoder_sample_fmt: text(&meta.decoder_sample_fmt),
            internal_sample_fmt: text(&meta.internal_sample_fmt),
            final_sample_fmt: text(&meta.final_sample_fmt),
            filters: text(&meta.filters),
            resample_reason: text(&meta.resample_reason),
            duration_ms: meta.duration_ms,
            bitrate: meta.bitrate,
            sample_rate: meta.rate,
            channels: meta.channels,
            source_bits: meta.source_bits,
            output_rate: meta.output_rate,
            output_channels: meta.output_channels,
            track: meta.track,
            disc: meta.disc,
            artwork: meta.artwork != 0,
            output_format: match meta.output_format {
                1 => PcmFormat::S16LE,
                3 => PcmFormat::S24LE,
                _ => PcmFormat::S32LE,
            },
            resampling: meta.resampling != 0,
            format_conversion: meta.format_conversion != 0,
            eq_active: meta.eq_active != 0,
            replaygain_mode: match meta.replaygain_mode {
                1 => ReplayGainMode::Track,
                2 => ReplayGainMode::Album,
                _ => ReplayGainMode::Off,
            },
            track_gain_db: meta.track_gain_db,
            album_gain_db: meta.album_gain_db,
            track_peak: meta.track_peak,
            album_peak: meta.album_peak,
            applied_gain_db: meta.applied_gain_db,
            headroom_db: meta.headroom_db,
            skip_start: meta.skip_start,
            skip_end: meta.skip_end,
            corrupt_packets: meta.corrupt_packets,
            date: text(&meta.date),
            genre: text(&meta.genre),
            open_us: meta.open_us,
            probe_us: meta.probe_us,
            io_bytes: meta.io_bytes,
            info_runs: meta.info_runs,
        }
    }
}

/// Demux-only local metadata/artwork context, closed exactly once on every path.
/// Retains encoded pictures only for its lifetime; library rows contain no blobs.
/// A NUL-terminated C text field as UTF-8 (invalid bytes replaced).
fn text(b: &[c_char]) -> String {
    let length = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    // SAFETY: c_char and u8 are one-byte types with identical alignment;
    // the initialized ABI buffer stays borrowed for this conversion.
    let bytes = unsafe { std::slice::from_raw_parts(b.as_ptr().cast::<u8>(), length) };
    String::from_utf8_lossy(bytes).into_owned()
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawChapter {
    start_ms: i64,
    end_ms: i64,
    title: [c_char; 256],
}
const _: () = assert!(std::mem::size_of::<RawChapter>() == 272);

/// A chapter inside a file (an audiobook's M4B or MP3), as FFmpeg reads it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Chapter {
    pub start_ms: u64,
    pub end_ms: u64,
    pub title: String,
}
/// More chapters than any real book has; the rest are left out.
pub const MAX_CHAPTERS: usize = 2048;

pub struct MediaFile {
    raw: NonNull<c_void>,
    api: &'static MediaApi,
    _cancel: Cancel,
    pub metadata: Metadata,
}
impl MediaFile {
    pub fn open(path: &Path, cancel: Cancel) -> Result<Self, String> {
        use std::os::unix::ffi::OsStrExt;
        let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| "NUL in path")?;
        let api = cancel.0.api;
        let mut ptr = std::ptr::null_mut();
        // SAFETY: zeroed repr(C) POD output; all pointers live through this synchronous call.
        let mut meta: RawMetadata = unsafe { std::mem::zeroed() };
        let r = unsafe { (api.inspect)(name.as_ptr(), cancel.0.raw.as_ptr(), &mut ptr, &mut meta) };
        if r < 0 {
            return Err(error(api, r));
        }
        Ok(Self {
            raw: NonNull::new(ptr).ok_or("missing media context")?,
            api,
            _cancel: cancel,
            metadata: Metadata::from_raw(&meta),
        })
    }
    /// The file's chapters, in order (empty when it has none).
    pub fn chapters(&mut self) -> Result<Vec<Chapter>, String> {
        let empty = RawChapter {
            start_ms: 0,
            end_ms: 0,
            title: [0; 256],
        };
        let mut raw = vec![empty; MAX_CHAPTERS];
        // SAFETY: uniquely borrowed context; C writes at most MAX_CHAPTERS records.
        let n = unsafe {
            (self.api.chapters)(self.raw.as_ptr(), raw.as_mut_ptr(), MAX_CHAPTERS as c_int)
        };
        if n < 0 {
            return Err(error(self.api, n));
        }
        Ok(raw[..n as usize]
            .iter()
            .map(|c| Chapter {
                start_ms: c.start_ms as u64,
                end_ms: c.end_ms as u64,
                title: text(&c.title),
            })
            .collect())
    }
    /// The embedded cover, centre-cropped to a `side` x `side` RGBA square.
    pub fn artwork(&mut self, side: u32) -> Result<Vec<u8>, String> {
        let side = art_side(side)?;
        let mut pixels = vec![0; side * side * 4];
        // SAFETY: uniquely borrowed context, side*side*4 writable buffer.
        let r =
            unsafe { (self.api.artwork)(self.raw.as_ptr(), pixels.as_mut_ptr(), side as c_int) };
        if r < 0 {
            Err(error(self.api, r))
        } else {
            Ok(pixels)
        }
    }
}
impl Drop for MediaFile {
    fn drop(&mut self) {
        // SAFETY: this owner closes the native context exactly once.
        unsafe { (self.api.close)(self.raw.as_ptr()) }
    }
}

pub struct Decoder {
    raw: NonNull<c_void>,
    api: &'static MediaApi,
    _cancel: Cancel,
    pub metadata: Metadata,
    spec: OutputSpec,
}
pub struct Pcm {
    pub data: Vec<u8>,
    pub position_ms: u64,
    pub packets: u64,
    pub frames: u64,
    pub rate: u32,
    pub format: PcmFormat,
}
impl Decoder {
    pub fn open(path: &Path, rate: u32, cancel: Cancel) -> Result<Self, String> {
        Self::open_with(
            path,
            OutputSpec {
                rate,
                format: PcmFormat::S32LE,
            },
            DspConfig::with_volume(100),
            cancel,
        )
    }
    pub fn open_with(
        path: &Path,
        spec: OutputSpec,
        dsp: DspConfig,
        cancel: Cancel,
    ) -> Result<Self, String> {
        if !path.is_file() {
            return Err("media must be an existing regular file".into());
        }
        use std::os::unix::ffi::OsStrExt;
        let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| "NUL in path")?;
        Self::open_named(name, spec, dsp, cancel)
    }
    /// Internet radio: decode plain audio bytes (MP3, AAC/ADTS, Ogg, FLAC)
    /// from the read end of a pipe the network fetcher writes. The decoder
    /// keeps its own duplicate of the descriptor; FFmpeg itself never opens
    /// a network connection. Not seekable; ends when the writer closes.
    pub fn open_stream(
        pipe: std::os::fd::BorrowedFd<'_>,
        spec: OutputSpec,
        dsp: DspConfig,
        cancel: Cancel,
    ) -> Result<Self, String> {
        use std::os::fd::AsRawFd;
        let name = CString::new(format!("rbpipe:{}", pipe.as_raw_fd())).map_err(|_| "NUL")?;
        Self::open_named(name, spec, dsp, cancel)
    }
    fn open_named(
        name: CString,
        spec: OutputSpec,
        dsp: DspConfig,
        cancel: Cancel,
    ) -> Result<Self, String> {
        if !(8000..=384_000).contains(&spec.rate) {
            return Err("unsupported sink rate".into());
        }
        let api = cancel.0.api;
        let raw_dsp = dsp.raw();
        let mut ptr = std::ptr::null_mut();
        // SAFETY: output storage and path live through this synchronous call; C retains only the cancellation pointer.
        let mut meta: RawMetadata = unsafe { std::mem::zeroed() };
        let r = unsafe {
            (api.open)(
                name.as_ptr(),
                spec.rate as c_int,
                match spec.format {
                    PcmFormat::S16LE => 1,
                    PcmFormat::S24LE => 3,
                    PcmFormat::S32LE => 2,
                },
                &raw_dsp,
                cancel.0.raw.as_ptr(),
                &mut ptr,
                &mut meta,
            )
        };
        if r < 0 {
            return Err(error(api, r));
        }
        let raw = NonNull::new(ptr).ok_or_else(|| "missing decoder".to_string())?;
        Ok(Self {
            raw,
            api,
            _cancel: cancel,
            spec,
            metadata: Metadata::from_raw(&meta),
        })
    }
    pub fn read(&mut self) -> Result<Option<Pcm>, String> {
        let mut data = vec![0u8; 4096 * self.spec.format.bytes_per_frame()];
        let (mut packets, mut frames, mut pos) = (0, 0, 0);
        // SAFETY: unique decoder borrow and a buffer sized for 4096 stereo frames.
        let n = unsafe {
            (self.api.read)(
                self.raw.as_ptr(),
                data.as_mut_ptr(),
                4096,
                &mut packets,
                &mut frames,
                &mut pos,
            )
        };
        if n < 0 {
            return Err(error(self.api, n));
        }
        if n == 0 {
            return Ok(None);
        }
        data.truncate(n as usize * self.spec.format.bytes_per_frame());
        Ok(Some(Pcm {
            data,
            position_ms: pos.max(0) as u64,
            packets,
            frames,
            rate: self.spec.rate,
            format: self.spec.format,
        }))
    }
    pub fn seek(&mut self, ms: u64) -> Result<(), String> {
        if ms > i64::MAX as u64 / 1000 {
            return Err("seek overflow".into());
        }
        // SAFETY: uniquely borrowed initialized context and range-checked timestamp.
        let r = unsafe { (self.api.seek)(self.raw.as_ptr(), ms as i64) };
        if r < 0 {
            Err(error(self.api, r))
        } else {
            Ok(())
        }
    }
    /// The embedded cover, centre-cropped to a `side` x `side` RGBA square.
    pub fn artwork(&mut self, side: u32) -> Result<Vec<u8>, String> {
        let side = art_side(side)?;
        let mut b = vec![0; side * side * 4];
        // SAFETY: C receives exactly side*side*4 writable bytes.
        let r = unsafe { (self.api.artwork)(self.raw.as_ptr(), b.as_mut_ptr(), side as c_int) };
        if r < 0 {
            Err(error(self.api, r))
        } else {
            Ok(b)
        }
    }
}
/// Covers are decoded at most this large (the native decoder's bound).
const ART_SIDE_MAX: u32 = 256;
fn art_side(side: u32) -> Result<usize, String> {
    if side == 0 || side > ART_SIDE_MAX {
        return Err("artwork size".into());
    }
    Ok(side as usize)
}
/// A cover image file, centre-cropped to a `side` x `side` RGBA square.
pub fn external_artwork(path: &Path, side: u32) -> Result<Vec<u8>, String> {
    use std::os::unix::ffi::OsStrExt;
    let side = art_side(side)?;
    let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| "NUL in path")?;
    let api = api()?;
    let mut b = vec![0; side * side * 4];
    // SAFETY: C receives a valid local path and exactly side*side*4 writable bytes.
    let r = unsafe { (api.artwork_file)(name.as_ptr(), b.as_mut_ptr(), side as c_int) };
    if r < 0 {
        Err(error(api, r))
    } else {
        Ok(b)
    }
}
/// A station logo (JPEG, PNG or WebP) whole and undistorted, centred in a
/// `side` x `side` straight-alpha RGBA square with transparent margins.
pub fn logo_image(path: &Path, side: u32) -> Result<Vec<u8>, String> {
    use std::os::unix::ffi::OsStrExt;
    if side == 0 || side > 256 {
        return Err("logo size".into());
    }
    let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| "NUL in path")?;
    let api = api()?;
    let mut b = vec![0; side as usize * side as usize * 4];
    // SAFETY: C receives a valid local path and exactly side*side*4 writable bytes.
    let r = unsafe { (api.logo_file)(name.as_ptr(), b.as_mut_ptr(), side as c_int) };
    if r < 0 {
        Err(error(api, r))
    } else {
        Ok(b)
    }
}
/// A skin picture (JPEG, PNG or WebP) at its own size as straight-alpha RGBA:
/// `(width, height, pixels)`. Pictures larger than `max_w` x `max_h` are refused.
pub fn decode_image(path: &Path, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
    use std::os::unix::ffi::OsStrExt;
    if max_w == 0 || max_h == 0 || max_w > 2048 || max_h > 2048 {
        return Err("image bounds".into());
    }
    let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| "NUL in path")?;
    let api = api()?;
    let mut b = vec![0; max_w as usize * max_h as usize * 4];
    let (mut w, mut h) = (0, 0);
    // SAFETY: C receives a valid local path, max_w*max_h*4 writable bytes and two out-ints.
    let r = unsafe {
        (api.image_file)(
            name.as_ptr(),
            b.as_mut_ptr(),
            max_w as c_int,
            max_h as c_int,
            &mut w,
            &mut h,
        )
    };
    if r < 0 {
        return Err(error(api, r));
    }
    let (w, h) = (w as u32, h as u32);
    if w == 0 || h == 0 || w > max_w || h > max_h {
        return Err("image dimensions".into());
    }
    b.truncate(w as usize * h as usize * 4);
    Ok((w, h, b))
}
/// Deterministic sidecar preference, shared by playback and collection artwork.
/// Only candidate paths are retained; failed images fall through to the next cover.
/// Names in priority order (any case, .jpg/.jpeg/.png/.webp): cover, folder,
/// front, album, albumart, artwork; then Windows Media Player's
/// AlbumArt_{GUID}_Large / AlbumArtSmall; finally, the only image in the folder
/// (e.g. "<album>.jpg"). macOS "._*" companions never count.
pub fn folder_artwork_candidates(parent: &Path) -> Result<Vec<String>, String> {
    const STEMS: [&str; 6] = ["cover", "folder", "front", "album", "albumart", "artwork"];
    const EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];
    // Rank: named stems (stem-major, extension-minor), WMP large, WMP small.
    let rank = |name: &str| -> Option<usize> {
        let lower = name.to_ascii_lowercase();
        let (stem, extension) = lower.rsplit_once('.')?;
        let ext = EXTENSIONS.iter().position(|e| *e == extension)?;
        if let Some(index) = STEMS.iter().position(|s| *s == stem) {
            return Some(index * EXTENSIONS.len() + ext);
        }
        let base = STEMS.len() * EXTENSIONS.len();
        if stem.starts_with("albumart_{") && stem.ends_with("}_large") {
            return Some(base + ext);
        }
        (stem == "albumartsmall" || (stem.starts_with("albumart_{") && stem.ends_with("}_small")))
            .then_some(base + EXTENSIONS.len() + ext)
    };
    let mut named: Vec<(usize, PathBuf)> = Vec::new();
    let mut images: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(parent).map_err(|e| e.to_string())? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("._") || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        if let Some(index) = rank(&name) {
            named.push((index, entry.path()));
        } else if name
            .rsplit_once('.')
            .is_some_and(|(_, e)| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        {
            images.push(entry.path());
        }
    }
    named.sort();
    let mut out: Vec<String> = named
        .into_iter()
        .filter_map(|(_, p)| p.file_name()?.to_str().map(str::to_owned))
        .collect();
    if out.is_empty() && images.len() == 1 {
        out.extend(
            images[0]
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned),
        );
    }
    Ok(out)
}
pub fn folder_artwork(media: &Path, side: u32) -> Result<Vec<u8>, String> {
    let parent = media.parent().ok_or("media has no parent")?;
    for name in folder_artwork_candidates(parent)? {
        if let Ok(pixels) = external_artwork(&parent.join(name), side) {
            return Ok(pixels);
        }
    }
    Err("no usable folder artwork".into())
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: unique context is closed exactly once.
        unsafe { (self.api.close)(self.raw.as_ptr()) }
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
    /// Feed a fixture through a pipe in small, slow chunks, as a radio
    /// stream arrives, and decode it with the stream decoder.
    fn decode_streamed(name: &str, chunk: usize, pause_ms: u64) -> (u64, Result<(), String>) {
        decode_streamed_bytes(std::fs::read(fixture(name)).unwrap(), chunk, pause_ms)
    }
    fn decode_streamed_bytes(
        bytes: Vec<u8>,
        chunk: usize,
        pause_ms: u64,
    ) -> (u64, Result<(), String>) {
        use std::io::Write;
        use std::os::fd::{AsFd, FromRawFd, OwnedFd};
        let mut fds = [0; 2];
        // SAFETY: pipe2 fills two descriptors that we take ownership of.
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let writer = std::thread::spawn(move || {
            let mut file = std::fs::File::from(write);
            for part in bytes.chunks(chunk) {
                if file.write_all(part).is_err() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(pause_ms));
            }
        });
        let spec = OutputSpec {
            rate: 48_000,
            format: PcmFormat::S16LE,
        };
        let mut frames = 0;
        let result = Decoder::open_stream(
            read.as_fd(),
            spec,
            DspConfig::with_volume(100),
            Cancel::new().unwrap(),
        )
        .and_then(|mut d| {
            drop(read);
            while let Some(pcm) = d.read()? {
                frames += pcm.frames;
            }
            Ok(())
        });
        writer.join().unwrap();
        (frames, result)
    }
    #[test]
    fn radio_streams_decode_from_a_pipe_without_seeking() {
        for name in ["tone.mp3", "aac.aac", "tone.ogg"] {
            let (frames, result) = decode_streamed(name, 4096, 1);
            assert!(result.is_ok(), "{name}: {result:?}");
            assert!(frames > 1000, "{name}: {frames} frames");
        }
    }
    /// Spotify's sound: 16-bit stereo PCM behind a WAV header whose sizes
    /// are unknown (reborn-platform's `spotify::wav_header`), as a pipe.
    #[test]
    fn spotify_pcm_decodes_from_a_streamed_wav_pipe() {
        let mut bytes = b"RIFF\xff\xff\xff\xffWAVEfmt ".to_vec();
        for field in [16u32, 1 | 2 << 16, 44_100, 44_100 * 4, 4 | 16 << 16] {
            bytes.extend_from_slice(&field.to_le_bytes());
        }
        bytes.extend_from_slice(b"data\xff\xff\xff\xff");
        for i in 0..44_100u32 {
            let s = ((i as f32 * 0.0627).sin() * 8000.) as i16;
            bytes.extend_from_slice(&s.to_le_bytes());
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let (frames, result) = decode_streamed_bytes(bytes, 16384, 1);
        assert!(result.is_ok(), "{result:?}");
        // One second at 44.1 kHz, resampled to the 48 kHz sink.
        assert!((47_000..=49_000).contains(&frames), "{frames} frames");
    }
    #[test]
    fn stream_decoder_refuses_regular_files_and_odd_names() {
        let file = std::fs::File::open(fixture("tone.mp3")).unwrap();
        use std::os::fd::AsFd;
        let spec = OutputSpec {
            rate: 48_000,
            format: PcmFormat::S16LE,
        };
        assert!(Decoder::open_stream(
            file.as_fd(),
            spec,
            DspConfig::with_volume(100),
            Cancel::new().unwrap()
        )
        .is_err());
        // A library path can never be mistaken for a pipe.
        assert!(Decoder::open_with(
            Path::new("rbpipe:5"),
            spec,
            DspConfig::with_volume(100),
            Cancel::new().unwrap()
        )
        .is_err());
    }
    #[test]
    fn skin_pictures_decode_at_their_own_size_with_alpha_and_never_scale() {
        let skin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skins/classic/images");
        let (w, h, rgba) = decode_image(&skin.join("battery.png"), 480, 360).unwrap();
        assert_eq!((w, h, rgba.len()), (28, 14, 28 * 14 * 4));
        // Outline opaque, inside transparent: straight alpha survives.
        assert_eq!(rgba[3], 255);
        assert_eq!(rgba[(7 * 28 + 10) * 4 + 3], 0);
        let (_, _, strip) = decode_image(&skin.join("highlight.png"), 480, 360).unwrap();
        assert_ne!(
            strip[..4],
            strip[strip.len() - 4..],
            "a gradient, not one colour"
        );
        // Too large for the bound: refused, not scaled down.
        assert!(decode_image(&skin.join("battery.png"), 20, 360).is_err());
        assert!(decode_image(&fixture("missing.png"), 480, 360).is_err());
    }
    #[test]
    fn packed24_decode_float_dsp_and_sink_packing_keep_bits_below_s16() {
        let path = std::env::temp_dir().join(format!("reborn-lowbits-{}.wav", std::process::id()));
        let pattern = [
            0_i32, 1, -1, 127, -127, 255, -255, 256, -256, 257, -257, 1023, -1023, 32767, -32768,
            0, 0x234500, 0x234501, 0x23457f, 0x234580, 0x2345ff, -0x234600, -0x2345ff, -0x234581,
            -0x234580, -0x234501,
        ];
        let values: Vec<i32> = (0..1024)
            .flat_map(|frame| {
                [
                    pattern[frame % pattern.len()],
                    pattern[(frame + 5) % pattern.len()],
                ]
            })
            .collect();
        for rate in [44_100_u32, 48_000, 88_200, 96_000] {
            let length = (values.len() * 3) as u32;
            let mut wav = Vec::new();
            wav.extend_from_slice(b"RIFF");
            wav.extend_from_slice(&(length + 36).to_le_bytes());
            wav.extend_from_slice(b"WAVEfmt ");
            wav.extend_from_slice(&16_u32.to_le_bytes());
            wav.extend_from_slice(&1_u16.to_le_bytes());
            wav.extend_from_slice(&2_u16.to_le_bytes());
            wav.extend_from_slice(&rate.to_le_bytes());
            wav.extend_from_slice(&(rate * 6).to_le_bytes());
            wav.extend_from_slice(&6_u16.to_le_bytes());
            wav.extend_from_slice(&24_u16.to_le_bytes());
            wav.extend_from_slice(b"data");
            wav.extend_from_slice(&length.to_le_bytes());
            for value in &values {
                wav.extend_from_slice(&value.to_le_bytes()[..3]);
            }
            std::fs::write(&path, wav).unwrap();
            let mut sources = vec![path.clone()];
            if rate == 44_100 || rate == 48_000 {
                sources.push(fixture(&format!("precision-24-{rate}.flac")));
            }
            for source in sources {
                for format in [PcmFormat::S24LE, PcmFormat::S32LE] {
                    let mut decoder = Decoder::open_with(
                        &source,
                        OutputSpec { rate, format },
                        DspConfig::with_volume(100),
                        Cancel::new().unwrap(),
                    )
                    .unwrap();
                    assert_eq!(decoder.metadata.source_bits, 24);
                    assert_eq!(decoder.metadata.decoder_sample_fmt, "s32");
                    let mut output = Vec::new();
                    while let Some(block) = decoder.read().unwrap() {
                        output.extend_from_slice(&block.data);
                    }
                    assert_eq!(output.len(), values.len() * 4);
                    for (sample, expected) in output.as_chunks::<4>().0.iter().zip(&values) {
                        let word = i32::from_le_bytes(*sample);
                        let signed24 = if format == PcmFormat::S32LE {
                            word >> 8
                        } else {
                            (word << 8) >> 8
                        };
                        assert_eq!(signed24, *expected, "{rate} {format:?}");
                    }
                }
            }
        }
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn s24_quantization_rounds_both_signs_ties_even_and_saturates() {
        let cases = [
            (0, 0),
            (127, 0),
            (128, 0),
            (129, 1),
            (384, 2),
            (640, 2),
            (-127, 0),
            (-128, 0),
            (-129, -1),
            (-384, -2),
            (-640, -2),
            (i32::MAX, 8_388_607),
            (i32::MIN, -8_388_608),
        ];
        let input: Vec<u8> = cases
            .iter()
            .flat_map(|(word, _)| [*word, *word].into_iter().flat_map(i32::to_le_bytes))
            .collect();
        let output = convert_pcm(&input, 48_000, PcmFormat::S32LE, PcmFormat::S24LE).unwrap();
        for (frame, (_, expected)) in output.as_chunks::<8>().0.iter().zip(cases) {
            for sample in frame.as_chunks::<4>().0 {
                assert_eq!(sample[3], 0);
                let signed = (i32::from_le_bytes(*sample) << 8) >> 8;
                assert_eq!(signed, expected);
            }
        }
    }

    #[test]
    fn crossfade_final_s16_quantization_rounds_both_signs_and_saturates() {
        let cases = [
            (0, 0),
            (32767, 0),
            (32768, 0),
            (32769, 1),
            (98304, 2),
            (163840, 2),
            (-32767, 0),
            (-32768, 0),
            (-32769, -1),
            (-98304, -2),
            (-163840, -2),
            (i32::MAX, i16::MAX),
            (i32::MIN, i16::MIN),
        ];
        let input: Vec<u8> = cases
            .iter()
            .flat_map(|(word, _)| [*word, *word].into_iter().flat_map(i32::to_le_bytes))
            .collect();
        let output = convert_pcm(&input, 44_100, PcmFormat::S32LE, PcmFormat::S16LE).unwrap();
        for (frame, (_, expected)) in output.as_chunks::<4>().0.iter().zip(cases) {
            for sample in frame.as_chunks::<2>().0 {
                assert_eq!(i16::from_le_bytes(*sample), expected);
            }
        }
    }

    #[test]
    fn float_dsp_s32_container_does_not_claim_32_bit_preservation() {
        let path =
            std::env::temp_dir().join(format!("reborn-lowbits32-{}.wav", std::process::id()));
        // Each pair differs only below float32's significand at this amplitude.
        let pattern = [0x2345_0001_i32, 0x2345_0002, -0x2345_0001, -0x2345_0002];
        let values: Vec<i32> = (0..1024)
            .flat_map(|frame| [pattern[frame % 4], pattern[(frame + 2) % 4]])
            .collect();
        for rate in [44_100_u32, 48_000] {
            let length = (values.len() * 4) as u32;
            let mut wav = Vec::new();
            wav.extend_from_slice(b"RIFF");
            wav.extend_from_slice(&(length + 36).to_le_bytes());
            wav.extend_from_slice(b"WAVEfmt ");
            wav.extend_from_slice(&16_u32.to_le_bytes());
            wav.extend_from_slice(&1_u16.to_le_bytes());
            wav.extend_from_slice(&2_u16.to_le_bytes());
            wav.extend_from_slice(&rate.to_le_bytes());
            wav.extend_from_slice(&(rate * 8).to_le_bytes());
            wav.extend_from_slice(&8_u16.to_le_bytes());
            wav.extend_from_slice(&32_u16.to_le_bytes());
            wav.extend_from_slice(b"data");
            wav.extend_from_slice(&length.to_le_bytes());
            for value in &values {
                wav.extend_from_slice(&value.to_le_bytes());
            }
            std::fs::write(&path, wav).unwrap();
            let mut decoder = Decoder::open_with(
                &path,
                OutputSpec {
                    rate,
                    format: PcmFormat::S32LE,
                },
                DspConfig::with_volume(100),
                Cancel::new().unwrap(),
            )
            .unwrap();
            assert_eq!(decoder.metadata.source_bits, 32);
            assert_eq!(decoder.metadata.decoder_sample_fmt, "s32");
            assert_eq!(decoder.metadata.internal_sample_fmt, "fltp");
            let mut output = Vec::new();
            while let Some(block) = decoder.read().unwrap() {
                output.extend_from_slice(&block.data);
            }
            let words: Vec<i32> = output
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| i32::from_le_bytes(*p))
                .collect();
            assert_eq!(words.len(), values.len());
            assert_ne!(words, values);
            for pair in words.as_chunks::<4>().0 {
                assert_eq!(pair[0], pair[2]);
                assert_eq!(pair[1], pair[3]);
            }
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn fixture_formats_are_float_processed_and_s32_output() {
        for n in [
            "tone.wav",
            "tone.flac",
            "tone.mp3",
            "tone.m4a",
            "tone.ogg",
            "tone.opus",
        ] {
            let mut d = Decoder::open(&fixture(n), 48_000, Cancel::new().unwrap()).unwrap();
            assert_eq!(d.metadata.internal_sample_fmt, "fltp");
            assert_eq!(d.metadata.output_format, PcmFormat::S32LE);
            let mut frames = 0;
            while let Some(p) = d.read().unwrap() {
                frames += p.frames as usize;
                assert_eq!(p.data.len(), p.frames as usize * 8);
            }
            assert!((45_000..55_000).contains(&frames), "{n}: {frames}");
        }
    }
    #[test]
    fn production_source_matrix_decodes_without_a_second_audio_stack() {
        for name in [
            "flac-16-44100.flac",
            "flac-16-48000.flac",
            "flac-24-44100.flac",
            "flac-24-48000.flac",
            "flac-24-88200.flac",
            "flac-24-96000.flac",
            "wav-pcm16.wav",
            "wav-pcm24.wav",
            "wav-pcm32.wav",
            "wav-float32.wav",
            "mp3.mp3",
            "aac.aac",
            "m4a-aac.m4a",
            "m4a-alac.m4a",
            "vorbis.ogg",
            "opus.opus",
            "aiff-pcm24.aiff",
            "ape-silence.ape",
            "wavpack-silence.wv",
        ] {
            let mut decoder = Decoder::open(&fixture(name), 48_000, Cancel::new().unwrap())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(decoder.metadata.internal_sample_fmt, "fltp", "{name}");
            assert_eq!(decoder.metadata.channels, 2, "{name}");
            assert!(decoder.metadata.format_conversion, "{name}");
            if name.contains("24-") || name == "wav-pcm24.wav" || name == "aiff-pcm24.aiff" {
                assert_eq!(decoder.metadata.source_bits, 24, "{name}");
            }
            if name == "flac-16-48000.flac" {
                assert!(!decoder.metadata.resampling, "{name}");
            }
            let mut frames = 0;
            while let Some(block) = decoder.read().unwrap_or_else(|e| panic!("{name}: {e}")) {
                frames += block.frames;
            }
            assert!(frames > 0, "{name} produced no PCM");
        }
    }
    #[test]
    fn tags_and_no_metadata_are_distinguishable() {
        let tagged = Decoder::open_with(
            &fixture("flac-16-44100.flac"),
            OutputSpec {
                rate: 44_100,
                format: PcmFormat::S16LE,
            },
            DspConfig::with_volume(100),
            Cancel::new().unwrap(),
        )
        .unwrap();
        assert_eq!(tagged.metadata.track_gain_db, -6.0);
        assert_eq!(tagged.metadata.album_gain_db, -8.0);
        assert_eq!(tagged.metadata.track_peak, 0.5);
        assert_eq!(tagged.metadata.album_peak, 0.6);
        assert!(!tagged.metadata.resampling);
        let plain = Decoder::open_with(
            &fixture("source-no-metadata.flac"),
            OutputSpec {
                rate: 44_100,
                format: PcmFormat::S16LE,
            },
            DspConfig::with_volume(100),
            Cancel::new().unwrap(),
        )
        .unwrap();
        assert_eq!(plain.metadata.title, "");
        assert_eq!(plain.metadata.track_gain_db, 0.0);
    }
    #[test]
    fn replaygain_volume_eq_and_headroom_share_the_ffmpeg_graph() {
        let mut decoder = Decoder::open_with(
            &fixture("flac-16-44100.flac"),
            OutputSpec {
                rate: 44_100,
                format: PcmFormat::S32LE,
            },
            DspConfig {
                volume: 80,
                replay_gain: ReplayGainMode::Track,
                eq_enabled: true,
                eq_bands: vec![EqBand {
                    frequency_hz: 1000.0,
                    gain_db: 3.0,
                    q: 1.0,
                    kind: reborn_core::EqKind::Peak,
                }],
                crossfade_ms: 0,
                preamp_db: 0.0,
                speed: 1.0,
            },
            Cancel::new().unwrap(),
        )
        .unwrap();
        assert!(decoder.metadata.filters.contains("volume"));
        assert!(decoder.metadata.filters.contains("EQ=configured"));
        assert!(decoder.metadata.filters.contains("alimiter"));
        assert!(decoder.metadata.eq_active);
        assert_eq!(decoder.metadata.replaygain_mode, ReplayGainMode::Track);
        assert!(decoder.metadata.applied_gain_db < 0.0);
        let mut peak = 0i64;
        while let Some(block) = decoder.read().unwrap() {
            for sample in block.data.as_chunks::<4>().0 {
                peak = peak.max(i32::from_le_bytes(*sample).unsigned_abs() as i64);
            }
        }
        assert!(peak <= (i32::MAX as f64 * 0.981) as i64);
    }
    #[test]
    fn limiter_only_when_the_graph_can_raise_a_peak() {
        // Neutral playback (the player decodes at neutral user gain) and plain
        // attenuation cannot raise a peak: no limiter stage.
        let neutral = |volume| {
            Decoder::open_with(
                &fixture("flac-16-44100.flac"),
                OutputSpec {
                    rate: 44_100,
                    format: PcmFormat::S16LE,
                },
                DspConfig::with_volume(volume),
                Cancel::new().unwrap(),
            )
            .unwrap()
        };
        for volume in [100, 80] {
            let decoder = neutral(volume);
            assert!(
                decoder.metadata.filters.contains("no-limiter"),
                "{}",
                decoder.metadata.filters
            );
            assert!(!decoder.metadata.filters.contains("alimiter"));
        }
        // An active EQ band keeps the limiter (see the shared-graph test).
        let eq = Decoder::open_with(
            &fixture("flac-16-44100.flac"),
            OutputSpec {
                rate: 44_100,
                format: PcmFormat::S16LE,
            },
            DspConfig {
                volume: 100,
                eq_enabled: true,
                eq_bands: vec![EqBand {
                    frequency_hz: 100.0,
                    gain_db: -2.0,
                    q: 1.0,
                    kind: reborn_core::EqKind::Peak,
                }],
                ..Default::default()
            },
            Cancel::new().unwrap(),
        )
        .unwrap();
        assert!(eq.metadata.filters.contains("alimiter"));
    }
    /// Decode the fixture with one filter and the given preamp; return the
    /// largest sample and the reported gain stage.
    fn shaped(
        kind: reborn_core::EqKind,
        frequency_hz: f32,
        gain_db: f32,
        preamp_db: f32,
    ) -> (i64, f64, String) {
        let mut decoder = Decoder::open_with(
            &fixture("flac-16-44100.flac"),
            OutputSpec {
                rate: 44_100,
                format: PcmFormat::S32LE,
            },
            DspConfig {
                volume: 100,
                eq_enabled: true,
                eq_bands: vec![EqBand {
                    frequency_hz,
                    gain_db,
                    q: 0.7,
                    kind,
                }],
                preamp_db,
                ..Default::default()
            },
            Cancel::new().unwrap(),
        )
        .unwrap();
        let mut peak = 0i64;
        while let Some(block) = decoder.read().unwrap() {
            for sample in block.data.as_chunks::<4>().0 {
                peak = peak.max(i32::from_le_bytes(*sample).unsigned_abs() as i64);
            }
        }
        (
            peak,
            decoder.metadata.applied_gain_db,
            decoder.metadata.filters.clone(),
        )
    }
    /// A shipped 10-filter profile with positive ReplayGain on a near
    /// full-scale 24/96 file: the preamp, the filters and +6 dB without a peak
    /// tag would pass full scale; the limiter must hold every output format
    /// below it.
    #[test]
    fn a_full_profile_with_replaygain_stays_below_full_scale_at_24_and_16_bits() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/eq/Sennheiser HD 650 ParametricEQ.txt");
        let profile =
            reborn_core::eq::parse("HD 650", &std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(profile.bands.len(), 10);
        for (format, full) in [(PcmFormat::S24LE, 8_388_607i64), (PcmFormat::S16LE, 32_767)] {
            let mut decoder = Decoder::open_with(
                &fixture("loud-24-96000-rg.flac"),
                OutputSpec {
                    rate: 48_000,
                    format,
                },
                DspConfig {
                    volume: 100,
                    replay_gain: ReplayGainMode::Track,
                    eq_enabled: true,
                    eq_bands: profile.bands.clone(),
                    crossfade_ms: 0,
                    preamp_db: profile.preamp_db,
                    speed: 1.0,
                },
                Cancel::new().unwrap(),
            )
            .unwrap();
            let filters = &decoder.metadata.filters;
            assert!(filters.contains("ReplayGain=track") && filters.contains("preamp=-6.4dB"));
            assert!(filters.contains("alimiter"), "{filters}");
            assert!((decoder.metadata.applied_gain_db - (6.0 - 6.4)).abs() < 1e-6);
            let mut peak = 0i64;
            while let Some(block) = decoder.read().unwrap() {
                if format == PcmFormat::S24LE {
                    for sample in block.data.as_chunks::<4>().0 {
                        let value = (i32::from_le_bytes(*sample) << 8) >> 8;
                        peak = peak.max(i64::from(value).abs());
                    }
                } else {
                    for sample in block.data.as_chunks::<2>().0 {
                        peak = peak.max(i64::from(i16::from_le_bytes(*sample)).abs());
                    }
                }
            }
            // Loud (the boosts work) but under the 0.98 limit, never clipped.
            assert!(peak > full * 9 / 10, "{format:?}: {peak}");
            assert!(peak <= full * 981 / 1000, "{format:?}: {peak}");
        }
    }
    #[test]
    fn profile_shelves_and_preamp_reach_the_ffmpeg_graph() {
        use reborn_core::EqKind::{HighShelf, LowShelf, Peak};
        // Each shape is a different FFmpeg filter: a -20 dB shelf below or
        // above 1 kHz removes different parts of the same music.
        let (low, _, _) = shaped(LowShelf, 1000., -20., 0.);
        let (high, _, _) = shaped(HighShelf, 1000., -20., 0.);
        let (peak, _, _) = shaped(Peak, 1000., -20., 0.);
        assert!(
            low != high && high != peak && low != peak,
            "{low} {high} {peak}"
        );
        // The preamp is part of the gain stage before the filters.
        let (quiet, applied, filters) = shaped(LowShelf, 100., 6., -6.4);
        assert!((applied + 6.4).abs() < 1e-6, "{applied}");
        assert!(filters.contains("preamp=-6.4dB"), "{filters}");
        let (loud, _, _) = shaped(LowShelf, 100., 6., 0.);
        assert!(quiet < loud, "{quiet} {loud}");
        // A preamp without an active band does nothing.
        let (_, applied, _) = shaped(Peak, 1000., 0., -6.4);
        assert_eq!(applied, 0.0);
    }
    #[test]
    fn folder_artwork_names_priority_and_single_image_fallback() {
        let root = std::env::temp_dir().join(format!("reborn-folder-art-{}", std::process::id()));
        let names = |files: &[&str]| {
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir(&root).unwrap();
            for f in files {
                std::fs::write(root.join(f), b"x").unwrap();
            }
            folder_artwork_candidates(&root).unwrap()
        };
        assert_eq!(
            names(&[
                "Album.JPG",
                "front.png",
                "Folder.webp",
                "cover.jpeg",
                "cover.jpg",
                "notes.txt"
            ]),
            [
                "cover.jpg",
                "cover.jpeg",
                "Folder.webp",
                "front.png",
                "Album.JPG"
            ]
        );
        assert_eq!(
            names(&["artwork.png", "AlbumArt.jpg"]),
            ["AlbumArt.jpg", "artwork.png"]
        );
        assert_eq!(
            names(&["AlbumArtSmall.jpg", "AlbumArt_{1A2B}_Large.jpg"]),
            ["AlbumArt_{1A2B}_Large.jpg", "AlbumArtSmall.jpg"]
        );
        // The only image in the folder is used; several unnamed ones are not guessed.
        assert_eq!(names(&["Afterglow.jpg", "01.flac"]), ["Afterglow.jpg"]);
        assert!(names(&["scan1.jpg", "scan2.jpg"]).is_empty());
        // macOS companions never count, neither as named nor as the only image.
        assert!(names(&["._cover.jpg"]).is_empty());
        assert_eq!(names(&["._cover.jpg", "front.jpg"]), ["front.jpg"]);
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn final_integer_conversion_stays_inside_the_ffmpeg_boundary() {
        let mut input = Vec::new();
        for value in [i32::MAX, i32::MIN, 0, 1] {
            input.extend_from_slice(&value.to_le_bytes());
            input.extend_from_slice(&value.to_le_bytes());
        }
        let output = convert_pcm(&input, 44_100, PcmFormat::S32LE, PcmFormat::S16LE).unwrap();
        assert_eq!(output.len(), 4 * PcmFormat::S16LE.bytes_per_frame());
        assert_eq!(
            i16::from_le_bytes(output[0..2].try_into().unwrap()),
            i16::MAX
        );
        assert_eq!(
            i16::from_le_bytes(output[2..4].try_into().unwrap()),
            i16::MAX
        );
        assert_eq!(i16::from_le_bytes(output[8..10].try_into().unwrap()), 0);
    }
    #[test]
    fn s32_to_s24_writes_signed_low_24_bits_in_four_byte_words() {
        let values = [
            0,
            0,
            0,
            0, // +0 and -0 both have the all-zero representation.
            256,
            -256,
            0x4000_0000,
            -0x4000_0000,
            i32::MAX,
            i32::MIN,
        ];
        let input = values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();

        let output = convert_pcm(&input, 48_000, PcmFormat::S32LE, PcmFormat::S24LE).unwrap();

        let expected: [[u8; 4]; 10] = [
            [0x00, 0x00, 0x00, 0x00],
            [0x00, 0x00, 0x00, 0x00],
            [0x00, 0x00, 0x00, 0x00],
            [0x00, 0x00, 0x00, 0x00],
            [0x01, 0x00, 0x00, 0x00],
            [0xff, 0xff, 0xff, 0x00],
            [0x00, 0x00, 0x40, 0x00],
            [0x00, 0x00, 0xc0, 0x00],
            [0xff, 0xff, 0x7f, 0x00],
            [0x00, 0x00, 0x80, 0x00],
        ];
        let expected = expected.into_iter().flatten().collect::<Vec<_>>();
        assert_eq!(output.len(), 5 * PcmFormat::S24LE.bytes_per_frame());
        assert_eq!(output, expected);
    }
    #[test]
    fn s24_input_is_sign_extended_to_q31_for_internal_conversion() {
        let values = [
            1u32,
            0x00ff_ffff,
            0x0040_0000,
            0x00c0_0000,
            0x007f_ffff,
            0x0080_0000,
        ];
        let mut input = Vec::new();
        for value in values {
            input.extend_from_slice(&[
                value as u8,
                (value >> 8) as u8,
                (value >> 16) as u8,
                0xa5, // Padding is ignored on input.
            ]);
        }

        let output = convert_pcm(&input, 48_000, PcmFormat::S24LE, PcmFormat::S32LE).unwrap();
        let samples = output
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| i32::from_le_bytes(*word))
            .collect::<Vec<_>>();

        assert_eq!(
            samples,
            [
                256,
                -256,
                0x4000_0000,
                -0x4000_0000,
                2_147_483_392,
                i32::MIN
            ]
        );
        assert_eq!(output.len(), 3 * PcmFormat::S32LE.bytes_per_frame());
    }
    #[test]
    fn s24_crossfade_unpacks_and_repacks_24_valid_bits() {
        fn input(frames: usize, left: u32, right: u32) -> Vec<u8> {
            let mut bytes = Vec::with_capacity(frames * PcmFormat::S24LE.bytes_per_frame());
            for _ in 0..frames {
                for value in [left, right] {
                    bytes.extend_from_slice(&[
                        value as u8,
                        (value >> 8) as u8,
                        (value >> 16) as u8,
                        0,
                    ]);
                }
            }
            bytes
        }

        let frames = 16;
        let first = input(frames, 0x0040_0000, 0x00c0_0000);
        let next = input(frames, 0x0020_0000, 0x00e0_0000);
        let output = crossfade_pcm(&first, &next, frames, 48_000, PcmFormat::S24LE).unwrap();

        assert_eq!(output.len(), frames * PcmFormat::S24LE.bytes_per_frame());
        assert!(output
            .as_chunks::<4>()
            .0
            .iter()
            .any(|sample| sample[..3] != [0, 0, 0]));
        assert!(output
            .as_chunks::<4>()
            .0
            .iter()
            .all(|sample| sample[3] == 0));
    }
    #[test]
    fn decoded_fltp_to_s24_rounds_q31_and_keeps_exact_frame_count() {
        fn decode(format: PcmFormat) -> Vec<u8> {
            let mut decoder = Decoder::open_with(
                &fixture("tone.flac"),
                OutputSpec {
                    rate: 44_100,
                    format,
                },
                DspConfig::with_volume(100),
                Cancel::new().unwrap(),
            )
            .unwrap();
            let mut pcm = Vec::new();
            while let Some(block) = decoder.read().unwrap() {
                pcm.extend_from_slice(&block.data);
            }
            if format == PcmFormat::S24LE {
                assert_eq!(decoder.metadata.final_sample_fmt, "s24le-in-32");
            }
            pcm
        }

        let s32 = decode(PcmFormat::S32LE);
        let s24 = decode(PcmFormat::S24LE);
        assert_eq!(s32.len() % PcmFormat::S32LE.bytes_per_frame(), 0);
        assert_eq!(s24.len() % PcmFormat::S24LE.bytes_per_frame(), 0);
        assert_eq!(s24.len(), s32.len());
        assert!(s32
            .as_chunks::<4>()
            .0
            .iter()
            .any(|sample| *sample != [0, 0, 0, 0]));

        for (source, destination) in s32.as_chunks::<4>().0.iter().zip(s24.as_chunks::<4>().0) {
            let q31 = i32::from_le_bytes(*source);
            let value = (f64::from(q31) / 256.0)
                .round_ties_even()
                .clamp(-8_388_608.0, 8_388_607.0) as i32;
            assert_eq!(
                *destination,
                [value as u8, (value >> 8) as u8, (value >> 16) as u8, 0]
            );
        }
    }
    #[test]
    fn unsupported_native_sample_format_fails_closed() {
        let api = api().unwrap();
        let input = [0u8; 8];
        let mut output = [0u8; 8];
        let result =
            unsafe { (api.convert)(input.as_ptr(), 1, 48_000, 4, 3, output.as_mut_ptr(), 1) };
        assert!(result < 0);
    }
    #[test]
    fn malformed_is_error() {
        assert!(Decoder::open(&fixture("malformed.mp3"), 44_100, Cancel::new().unwrap()).is_err())
    }
    #[test]
    fn cancellation_and_seek() {
        let c = Cancel::new().unwrap();
        let mut d = Decoder::open(&fixture("tone.flac"), 44_100, c.clone()).unwrap();
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
    #[test]
    fn seek_discards_coarse_demuxer_prefix_before_resampling() {
        let mut decoder =
            Decoder::open(&fixture("tone.flac"), 44_100, Cancel::new().unwrap()).unwrap();
        decoder.seek(500).unwrap();
        let mut frames = 0u64;
        while let Some(block) = decoder.read().unwrap() {
            frames += block.frames;
        }
        assert!(
            (22_049..=22_051).contains(&frames),
            "decoded {frames} frames"
        );
    }
    #[test]
    fn runtime_manifest_is_json() {
        let components = runtime_components().unwrap();
        assert_eq!(version(), FFMPEG_VERSION);
        assert!(!components["version"].as_str().unwrap().is_empty());
        assert!(components["filters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "volume"));
    }
    #[test]
    fn crossfade_is_processed_by_ffmpeg_filter_graph() {
        let frames = 1024;
        let mut a = vec![0u8; frames * PcmFormat::S32LE.bytes_per_frame()];
        let mut b = vec![0u8; frames * PcmFormat::S32LE.bytes_per_frame()];
        for i in 0..frames {
            let av = if i < frames / 2 { i as i32 * 1024 } else { 0 };
            let bv = if i < frames / 2 {
                0
            } else {
                (i as i32 - 512) * 1024
            };
            for channel in 0..2 {
                let a_at = (i * 2 + channel) * 4;
                let b_at = (i * 2 + channel) * 4;
                a[a_at..a_at + 4].copy_from_slice(&av.to_le_bytes());
                b[b_at..b_at + 4].copy_from_slice(&bv.to_le_bytes());
            }
        }
        let out = crossfade_pcm(&a, &b, frames, 48_000, PcmFormat::S32LE).unwrap();
        assert_eq!(out.len(), a.len());
        assert!(out.windows(4).any(|w| w != [0, 0, 0, 0]));
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
            "flac-24-96000.flac",
            "wav-pcm24.wav",
            "ape-silence.ape",
            "wavpack-silence.wv",
        ] {
            let data = std::fs::read(root.join(name)).unwrap();
            for length in [0, 8, 64, data.len() / 2] {
                let path = temp.join(name);
                std::fs::write(&path, &data[..length]).unwrap();
                if let Ok(mut d) = Decoder::open(&path, 48_000, Cancel::new().unwrap()) {
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
        assert!(Decoder::open(&path, 48_000, c).is_err());
    }
    #[test]
    fn nonfile_and_rate_rejected() {
        assert!(Decoder::open(Path::new("/dev/zero"), 48_000, Cancel::new().unwrap()).is_err());
        assert!(Decoder::open(Path::new("/missing"), 96_000, Cancel::new().unwrap()).is_err());
    }
}

#[cfg(test)]
mod artwork_tests {
    use super::*;
    #[test]
    fn embedded_art_is_decoded_to_fixed_rgba() {
        for name in ["artwork-png.flac", "artwork-jpeg.flac", "artwork-webp.flac"] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/fixtures")
                .join(name);
            let mut d = Decoder::open(&path, 48_000, Cancel::new().unwrap()).unwrap();
            assert!(d.metadata.artwork, "{name}");
            let pixels = d.artwork(160).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(pixels.len(), 160 * 160 * 4);
            assert!(
                pixels.as_chunks::<4>().0.iter().any(|p| p[3] != 0),
                "{name}"
            );
            assert!(d.read().unwrap().is_some());
        }
    }
    #[test]
    fn external_art_is_decoded_by_the_same_image_membrane() {
        for name in [
            "artwork-external.jpg",
            "artwork-external.png",
            "artwork-external.webp",
        ] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/fixtures")
                .join(name);
            let pixels = external_artwork(&path, 160).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(pixels.len(), 160 * 160 * 4);
            assert!(
                pixels.as_chunks::<4>().0.iter().any(|p| p[3] != 0),
                "{name}"
            );
        }
    }
    #[test]
    fn external_art_header_fallback_rejects_short_and_unknown_files() {
        let root =
            std::env::temp_dir().join(format!("reborn-image-headers-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        for (i, bytes) in [
            &b""[..],
            &b"unknown image"[..],
            &b"\xff\xd8"[..],
            &b"\xff\xd8\xff"[..],
            &b"\x89PNG\r\n\x1a"[..],
            &b"\x89PNG\r\n\x1a\n"[..],
            &b"RIFF\0\0\0\0WEB"[..],
            &b"RIFF\0\0\0\0WEBP"[..],
        ]
        .into_iter()
        .enumerate()
        {
            let path = root.join(format!("{i}"));
            std::fs::write(&path, bytes).unwrap();
            assert!(external_artwork(&path, 160).is_err(), "case {i}");
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        let fifo = root.join("fifo");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let socket = root.join("socket");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert!(external_artwork(&fifo, 160).is_err());
        assert!(external_artwork(&socket, 160).is_err());
        assert!(external_artwork(&root, 160).is_err());
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    fn path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/fixtures")
            .join(name)
    }
    fn inspect(name: &str) -> Metadata {
        MediaFile::open(&path(name), Cancel::new().unwrap())
            .unwrap()
            .metadata
            .clone()
    }
    #[test]
    fn tags_and_source_precision_are_consistent_across_demux_and_playback() {
        for name in [
            "metadata/tags.flac",
            "metadata/id3.mp3",
            "metadata/tags.ape",
        ] {
            let m = inspect(name);
            assert_eq!(m.title, "日本語 – café", "{name}");
            assert_eq!(m.artist, "Björk", "{name}");
            assert_eq!(m.album_artist, "Various", "{name}");
            assert_eq!((m.track, m.disc), (3, 1), "{name}");
            assert!(m.date.starts_with("2024"), "{name}");
            assert_eq!(m.genre, "Ambient", "{name}");
            assert_eq!(m.track_gain_db, -4.25, "{name}");
            let d = Decoder::open(&path(name), 48000, Cancel::new().unwrap()).unwrap();
            assert_eq!(m.title, d.metadata.title);
            assert_eq!(m.source_bits, d.metadata.source_bits);
        }
    }
    #[test]
    fn wav_aiff_and_existing_mp4_vorbis_tags_use_the_same_mapping() {
        for name in ["metadata/tags.wav", "metadata/tags.aiff"] {
            let m = inspect(name);
            assert_eq!(m.title, "日本語 – café", "{name}");
            assert_eq!(m.artist, "Björk", "{name}");
            assert_eq!(m.source_bits, 24);
        }
        let m = inspect("metadata/tags.wav");
        assert_eq!(m.track, 3);
        assert_eq!(m.date, "2024-02-29");
        assert_eq!(m.genre, "Ambient");
        for name in ["m4a-aac.m4a", "m4a-alac.m4a", "vorbis.ogg", "opus.opus"] {
            let m = inspect(name);
            assert_eq!(m.title, "Reborn 440 Hz", "{name}");
            assert_eq!(m.artist, "Y2 fixture", "{name}");
            assert_eq!(m.album_artist, "Y2 fixture", "{name}");
            assert_eq!((m.track, m.disc), (1, 1), "{name}");
        }
    }
    #[test]
    fn container_headers_preserve_properties_without_audio_or_picture_decode() {
        for (name, bits) in [
            ("flac-24-96000.flac", 24),
            ("wav-pcm24.wav", 24),
            ("wav-float32.wav", 32),
            ("aiff-pcm24.aiff", 24),
            ("m4a-alac.m4a", 24),
            ("m4a-aac.m4a", 0),
            ("vorbis.ogg", 0),
            ("opus.opus", 0),
            ("ape-silence.ape", 16),
            ("wavpack-silence.wv", 16),
            ("artwork-webp.flac", 16),
        ] {
            let m = inspect(name);
            assert_eq!(m.info_runs, 0, "{name}");
            assert_eq!(m.source_bits, bits, "{name}");
            assert!(m.decoder_sample_fmt.is_empty());
            assert!(m.filters.is_empty());
            let d = Decoder::open(&path(name), 48000, Cancel::new().unwrap()).unwrap();
            assert_eq!(m.sample_rate, d.metadata.sample_rate, "{name}");
            assert_eq!(m.channels, d.metadata.channels, "{name}");
            assert_eq!(m.duration_ms, d.metadata.duration_ms, "{name}");
        }
        for name in ["mp3.mp3", "aac.aac"] {
            let m = inspect(name);
            assert_eq!(m.info_runs, 1);
            assert_eq!(m.source_bits, 0);
            assert_eq!(m.source_sample_fmt, "unspecified");
        }
    }
    #[test]
    fn bounded_strings_ordinals_and_replaygain_reject_malformed_values() {
        let m = inspect("metadata/long.flac");
        assert_eq!(m.title.len(), 510);
        assert!(!m.title.contains('\u{fffd}'));
        assert_eq!((m.track, m.disc), (0, 0));
        assert_eq!(
            (m.track_gain_db, m.album_gain_db, m.track_peak),
            (0., 0., 0.)
        );
        let m = inspect("metadata/duplicate.flac");
        // FFmpeg normalizes repeated Vorbis comments to one semicolon-delimited value.
        assert_eq!(m.title, "first;second");
        assert_eq!(m.artist, "one;two");
        assert_eq!((m.track, m.disc), (0, 0));
        let m = inspect("metadata/invalid-utf8.flac");
        assert_eq!(m.track, 3);
        assert!(m.title.contains('\u{fffd}'));
        let m = inspect("source-no-metadata.flac");
        assert!(m.title.is_empty() && m.artist.is_empty());
    }
    #[test]
    fn corrupt_and_oversized_art_do_not_require_image_decode_during_ingest() {
        for name in [
            "metadata/corrupt-art.flac",
            "metadata/huge-art-dimensions.flac",
        ] {
            let mut m = MediaFile::open(&path(name), Cancel::new().unwrap()).unwrap();
            assert_eq!(m.metadata.info_runs, 0);
            assert_eq!(m.metadata.title, "日本語 – café");
            assert!(m.artwork(160).is_err());
        }
        // Invalid declared packet sizes are FFmpeg-owned: clean error or usable audio.
        if let Ok(mut m) = MediaFile::open(
            &path("metadata/huge-art-metadata.flac"),
            Cancel::new().unwrap(),
        ) {
            assert!(m.artwork(160).is_err());
        }
        assert!(external_artwork(&path("metadata/huge.png"), 160).is_err());
        for name in [
            "metadata/art-jpg.flac",
            "metadata/art-png.flac",
            "metadata/art-webp.flac",
        ] {
            let mut m = MediaFile::open(&path(name), Cancel::new().unwrap()).unwrap();
            assert!(m.metadata.artwork);
            assert_eq!(m.artwork(160).unwrap().len(), 160 * 160 * 4);
        }
    }
    #[test]
    fn sidecar_case_and_corrupt_cover_fallback_are_consistent() {
        let root = std::env::temp_dir().join(format!("reborn-sidecar-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Cover.JPG"), b"corrupt").unwrap();
        std::fs::copy(path("artwork-external.webp"), root.join("Album.WEBP")).unwrap();
        assert_eq!(
            folder_artwork(&root.join("track.flac"), 160).unwrap().len(),
            160 * 160 * 4
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn lossy_gapless_skip_side_data_preserves_the_expected_sample_count() {
        for (name, expected) in [
            ("mp3.mp3", 11025usize),
            ("opus.opus", 12000),
            ("m4a-aac.m4a", 11025),
        ] {
            let mut decoder = Decoder::open(
                &path(name),
                if name == "opus.opus" { 48000 } else { 44100 },
                Cancel::new().unwrap(),
            )
            .unwrap();
            let mut frames = 0;
            while let Some(block) = decoder.read().unwrap() {
                frames += block.data.len() / block.format.bytes_per_frame();
            }
            assert_eq!(frames, expected, "{name}");
            assert_eq!(decoder.metadata.duration_ms, 250, "{name}");
        }
    }
    /// An uncompressed RGB PNG (stored deflate blocks), `f` colouring each pixel.
    fn png(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        fn crc(bytes: &[u8]) -> u32 {
            let mut c = !0u32;
            for &b in bytes {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 == 1 {
                        0xEDB8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        }
        let mut raw = vec![];
        for y in 0..h {
            raw.push(0);
            (0..w).for_each(|x| raw.extend(f(x, y)));
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &v in &raw {
            a = (a + u32::from(v)) % 65521;
            b = (b + a) % 65521;
        }
        let mut z = vec![0x78, 0x01];
        let chunks: Vec<&[u8]> = raw.chunks(65535).collect();
        for (i, part) in chunks.iter().enumerate() {
            let len = part.len() as u16;
            z.push(u8::from(i + 1 == chunks.len()));
            z.extend(len.to_le_bytes());
            z.extend((!len).to_le_bytes());
            z.extend(*part);
        }
        z.extend((b << 16 | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = w.to_be_bytes().to_vec();
        ihdr.extend(h.to_be_bytes());
        ihdr.extend([8, 2, 0, 0, 0]);
        for (kind, data) in [(&b"IHDR"[..], ihdr), (b"IDAT", z), (b"IEND", vec![])] {
            out.extend((data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend(&data);
            out.extend(&body);
            out.extend(crc(&body).to_be_bytes());
        }
        out
    }
    #[test]
    fn a_wide_or_tall_cover_is_cropped_to_its_centre_not_stretched() {
        let dir = std::env::temp_dir().join(format!("reborn-art-crop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        // Thirds: red, green, blue across a wide picture, down a tall one.
        let third = |i: u32| [[220, 0, 0], [0, 200, 0], [0, 0, 220]][(i / 40) as usize];
        std::fs::write(dir.join("wide.png"), png(120, 40, |x, _| third(x))).unwrap();
        std::fs::write(dir.join("tall.png"), png(40, 120, |_, y| third(y))).unwrap();
        for name in ["wide.png", "tall.png"] {
            let pixels = external_artwork(&dir.join(name), 64).unwrap();
            assert_eq!(pixels.len(), 64 * 64 * 4);
            for p in pixels.as_chunks::<4>().0 {
                assert!(
                    p[1] > 190 && p[0] < 12 && p[2] < 12,
                    "{name}: {p:?} outside the centre"
                );
            }
        }
        // The same picture at two sizes: the size is the caller's.
        assert_eq!(
            external_artwork(&dir.join("wide.png"), 180).unwrap().len(),
            180 * 180 * 4
        );
        assert!(external_artwork(&dir.join("wide.png"), 0).is_err());
        assert!(external_artwork(&dir.join("wide.png"), 257).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn a_book_speed_plays_faster_without_moving_positions_off_the_source() {
        let run = |speed: f32| {
            let mut decoder = Decoder::open_with(
                &path("book-chapters.m4b"),
                OutputSpec {
                    rate: 44_100,
                    format: PcmFormat::S32LE,
                },
                DspConfig {
                    volume: 100,
                    speed,
                    ..Default::default()
                },
                Cancel::new().unwrap(),
            )
            .unwrap();
            let filters = decoder.metadata.filters.clone();
            let (mut frames, mut last) = (0u64, 0u64);
            while let Some(block) = decoder.read().unwrap() {
                frames += block.frames;
                last = block.position_ms;
            }
            (frames, last, filters)
        };
        let (normal, end, filters) = run(1.0);
        assert!(filters.ends_with("acrossfade=0ms"), "{filters}");
        let (fast, fast_end, filters) = run(2.0);
        assert!(filters.ends_with(",atempo=2.00"), "{filters}");
        let ratio = fast as f64 / normal as f64;
        assert!(
            (0.45..0.55).contains(&ratio),
            "2x plays half as long: {ratio}"
        );
        // Positions are the source's: the last block is near the end at any speed.
        assert!(end > 5_000 && fast_end > 5_000, "{end} {fast_end}");
        // Out of range is normal speed.
        assert_eq!(
            DspConfig {
                speed: 7.0,
                ..Default::default()
            }
            .tempo(),
            1.0
        );
        assert_eq!(DspConfig::default().tempo(), 1.0);
    }
    #[test]
    fn audiobook_chapters_are_read_from_m4b_and_mp3() {
        for name in [
            "book-chapters.m4b",
            "book-chapters.mp3",
            "book-cover-track.m4b",
        ] {
            let mut file = MediaFile::open(&path(name), Cancel::new().unwrap())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let chapters = file.chapters().unwrap();
            let summary: Vec<_> = chapters
                .iter()
                .map(|c| (c.start_ms, c.end_ms, c.title.as_str()))
                .collect();
            assert_eq!(
                summary,
                [
                    (0, 2000, "Opening"),
                    (2000, 4000, "Chapter 1: The Start"),
                    (4000, 6000, "Kapitel 2: Über Umwege"),
                ],
                "{name}"
            );
        }
        // Music has no chapters.
        let mut song = MediaFile::open(&path("m4a-aac.m4a"), Cancel::new().unwrap()).unwrap();
        assert!(song.chapters().unwrap().is_empty());
    }
    #[test]
    fn an_m4b_with_its_cover_as_a_video_track_plays_without_the_video() {
        let mut decoder = Decoder::open(
            &path("book-cover-track.m4b"),
            44_100,
            Cancel::new().unwrap(),
        )
        .unwrap();
        decoder.seek(4500).unwrap();
        let block = decoder.read().unwrap().expect("audio after the seek");
        assert!(
            (4400..4600).contains(&block.position_ms),
            "{}",
            block.position_ms
        );
        let mut frames = block.frames;
        while let Some(block) = decoder.read().unwrap() {
            frames += block.frames;
        }
        assert!(frames > 0);
    }
    #[test]
    fn large_jpeg_is_downscaled_and_small_jpeg_remains_usable() {
        for name in ["metadata/large.jpg", "artwork-external.jpg"] {
            let pixels = external_artwork(&path(name), 160).unwrap();
            assert_eq!(pixels.len(), 160 * 160 * 4);
            assert!(pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
            assert!(pixels.iter().any(|&v| v > 0));
        }
    }
    #[test]
    fn deterministic_truncation_and_header_mutations_are_isolated() {
        let file =
            std::env::temp_dir().join(format!("reborn-mutations-{}.media", std::process::id()));
        for name in [
            "tone.flac",
            "mp3.mp3",
            "m4a-aac.m4a",
            "vorbis.ogg",
            "opus.opus",
            "wav-pcm24.wav",
            "aiff-pcm24.aiff",
            "ape-silence.ape",
            "wavpack-silence.wv",
        ] {
            let bytes = std::fs::read(path(name)).unwrap();
            for cut in [0, 1, 8, 42, bytes.len() / 2, bytes.len() - 1] {
                std::fs::write(&file, &bytes[..cut.min(bytes.len())]).unwrap();
                if let Ok(mut media) = MediaFile::open(&file, Cancel::new().unwrap()) {
                    assert!((8000..=384000).contains(&media.metadata.sample_rate));
                    if media.metadata.artwork {
                        let _ = media.artwork(160);
                    }
                }
            }
            for offset in [0, 4, 8, 16, 32] {
                let mut mutation = bytes.clone();
                mutation[offset] ^= 0xff;
                std::fs::write(&file, mutation).unwrap();
                let _ = MediaFile::open(&file, Cancel::new().unwrap());
            }
        }
        std::fs::remove_file(file).unwrap();
    }
    #[test]
    fn aggregate_metadata_io_has_a_budget_even_for_many_individually_valid_blocks() {
        use std::io::Write;
        let file =
            std::env::temp_dir().join(format!("reborn-tag-budget-{}.flac", std::process::id()));
        let base = std::fs::read(path("flac-24-96000.flac")).unwrap();
        let mut out = std::fs::File::create(&file).unwrap();
        out.write_all(b"fLaC\0\0\0\x22").unwrap();
        out.write_all(&base[8..42]).unwrap();
        let filler = vec![b'x'; 64 * 1024];
        for i in 0..3 {
            let value_length = 11 * 1024 * 1024u32;
            let size = value_length + 12;
            out.write_all(&[
                if i == 2 { 132 } else { 4 },
                (size >> 16) as u8,
                (size >> 8) as u8,
                size as u8,
            ])
            .unwrap();
            out.write_all(&0u32.to_le_bytes()).unwrap(); // vendor length
            out.write_all(&1u32.to_le_bytes()).unwrap(); // comment count
            out.write_all(&value_length.to_le_bytes()).unwrap();
            let key = format!("TAG{i}=");
            out.write_all(key.as_bytes()).unwrap();
            let mut remaining = value_length as usize - key.len();
            while remaining > 0 {
                let n = remaining.min(filler.len());
                out.write_all(&filler[..n]).unwrap();
                remaining -= n;
            }
        }
        drop(out);
        assert!(MediaFile::open(&file, Cancel::new().unwrap()).is_err());
        assert!(Decoder::open(&file, 48000, Cancel::new().unwrap()).is_err());
        std::fs::remove_file(file).unwrap();
    }
    #[test]
    fn malformed_files_and_cancelled_inspection_fail_cleanly() {
        for name in ["metadata/empty.flac", "malformed.mp3"] {
            assert!(MediaFile::open(&path(name), Cancel::new().unwrap()).is_err());
        }
        let cancel = Cancel::new().unwrap();
        cancel.cancel();
        assert!(MediaFile::open(&path("tone.flac"), cancel).is_err());
    }
}
