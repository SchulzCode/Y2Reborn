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
}
#[repr(C)]
#[derive(Clone, Copy)]
struct RawEqBand {
    frequency_hz: c_double,
    gain_db: c_double,
    q: c_double,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct RawDspConfig {
    replay_gain_mode: c_int,
    volume: u32,
    eq_count: u32,
    eq: [RawEqBand; 8],
    crossfade_ms: u32,
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
    fn rb_media_components(out: *mut c_char, size: c_int) -> c_int;
    fn rb_media_error(code: c_int, out: *mut c_char, size: c_int);
    fn rb_media_open(
        path: *const c_char,
        rate: c_int,
        output_format: c_int,
        dsp: *const RawDspConfig,
        cancel: *mut c_void,
        out: *mut *mut c_void,
        meta: *mut RawMetadata,
    ) -> c_int;
    fn rb_media_close(p: *mut c_void);
    fn rb_media_read(
        p: *mut c_void,
        out: *mut u8,
        capacity: c_int,
        packets: *mut u64,
        frames: *mut u64,
        position: *mut i64,
    ) -> c_int;
    fn rb_media_seek(p: *mut c_void, ms: i64) -> c_int;
    fn rb_media_art(p: *mut c_void, out: *mut u8, side: c_int) -> c_int;
    fn rb_media_art_file(path: *const c_char, out: *mut u8, side: c_int) -> c_int;
    fn rb_media_convert(
        input: *const u8,
        frames: c_int,
        rate: c_int,
        input_format: c_int,
        output_format: c_int,
        output: *mut u8,
        capacity_frames: c_int,
    ) -> c_int;
    fn rb_media_crossfade(
        a: *const u8,
        b: *const u8,
        frames: c_int,
        rate: c_int,
        format: c_int,
        output: *mut u8,
        capacity_frames: c_int,
        written_frames: *mut c_int,
    ) -> c_int;
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
        // SAFETY: callbacks have static lifetime and are installed once.
        unsafe { rb_media_logging(log_callback, log_enabled) }
    }
}
pub fn version() -> String {
    // SAFETY: FFmpeg returns an immutable process-lifetime string.
    unsafe { CStr::from_ptr(rb_media_version()) }
        .to_string_lossy()
        .into_owned()
}
pub fn runtime_components() -> Result<Value, String> {
    let mut bytes = vec![0 as c_char; 512 * 1024];
    // SAFETY: the C function writes at most the supplied capacity and NUL terminates JSON.
    let n = unsafe { rb_media_components(bytes.as_mut_ptr(), bytes.len() as c_int) };
    if n < 0 {
        return Err(error(n));
    }
    let json = unsafe { CStr::from_ptr(bytes.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    serde_json::from_str(&json).map_err(|e| format!("FFmpeg component manifest: {e}"))
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
    let frames = input.len() / input_frame_bytes;
    let mut output = vec![0u8; frames * output_format.bytes_per_frame()];
    let frames_i32 = i32::try_from(frames).map_err(|_| "PCM conversion is too large")?;
    let r = unsafe {
        rb_media_convert(
            input.as_ptr(),
            frames_i32,
            rate as c_int,
            match input_format {
                PcmFormat::S16LE => 1,
                PcmFormat::S32LE => 2,
            },
            match output_format {
                PcmFormat::S16LE => 1,
                PcmFormat::S32LE => 2,
            },
            output.as_mut_ptr(),
            frames_i32,
        )
    };
    if r < 0 {
        return Err(error(r));
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
    let frames_i32 = i32::try_from(frames).map_err(|_| "crossfade window is too large")?;
    let mut output = vec![0u8; frames * format.bytes_per_frame()];
    let mut written = 0;
    let r = unsafe {
        rb_media_crossfade(
            a.as_ptr(),
            b.as_ptr(),
            frames_i32,
            rate as c_int,
            match format {
                PcmFormat::S16LE => 1,
                PcmFormat::S32LE => 2,
            },
            output.as_mut_ptr(),
            frames_i32,
            &mut written,
        )
    };
    if r < 0 {
        return Err(error(r));
    }
    output.truncate(written.max(0) as usize * format.bytes_per_frame());
    Ok(output)
}
fn error(code: c_int) -> String {
    let mut b = [0 as c_char; 256];
    // SAFETY: C receives a writable fixed buffer and promises NUL termination.
    unsafe {
        rb_media_error(code, b.as_mut_ptr(), b.len() as c_int);
        format!(
            "FFmpeg {code}: {}",
            CStr::from_ptr(b.as_ptr()).to_string_lossy()
        )
    }
}

struct CancelInner(NonNull<c_void>);
// SAFETY: the allocation contains only C11 atomics and is reference counted.
unsafe impl Send for CancelInner {}
// SAFETY: cancellation access is atomic on the C side.
unsafe impl Sync for CancelInner {}
impl Drop for CancelInner {
    fn drop(&mut self) {
        // SAFETY: this is the final Arc owner.
        unsafe { rb_cancel_free(self.0.as_ptr()) }
    }
}
#[derive(Clone)]
pub struct Cancel(Arc<CancelInner>);
impl Cancel {
    pub fn new() -> Result<Self, String> {
        // SAFETY: constructor returns a new allocation or null.
        let p = unsafe { rb_cancel_new() };
        NonNull::new(p)
            .map(|p| Self(Arc::new(CancelInner(p))))
            .ok_or_else(|| "cancellation allocation failed".into())
    }
    pub fn cancel(&self) {
        // SAFETY: Arc keeps the atomic allocation alive for this call.
        unsafe { rb_cancel_set(self.0 .0.as_ptr()) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DspConfig {
    pub volume: u8,
    pub replay_gain: ReplayGainMode,
    pub eq_enabled: bool,
    pub eq_bands: Vec<EqBand>,
    pub crossfade_ms: u32,
}
impl DspConfig {
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
        }; 8];
        let bands = if self.eq_enabled {
            self.eq_bands.iter().take(8).collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let eq_count = bands.len() as u32;
        for (dst, src) in eq.iter_mut().zip(bands) {
            *dst = RawEqBand {
                frequency_hz: src.frequency_hz as c_double,
                gain_db: src.gain_db as c_double,
                q: src.q as c_double,
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
}

pub struct Decoder {
    raw: NonNull<c_void>,
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
        if !(8000..=384_000).contains(&spec.rate) {
            return Err("unsupported sink rate".into());
        }
        if !path.is_file() {
            return Err("media must be an existing regular file".into());
        }
        use std::os::unix::ffi::OsStrExt;
        let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| "NUL in path")?;
        let raw_dsp = dsp.raw();
        let mut ptr = std::ptr::null_mut();
        // SAFETY: output storage and path live through this synchronous call; C retains only the cancellation pointer.
        let mut meta: RawMetadata = unsafe { std::mem::zeroed() };
        let r = unsafe {
            rb_media_open(
                name.as_ptr(),
                spec.rate as c_int,
                match spec.format {
                    PcmFormat::S16LE => 1,
                    PcmFormat::S32LE => 2,
                },
                &raw_dsp,
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
        let raw = NonNull::new(ptr).ok_or_else(|| "missing decoder".to_string())?;
        Ok(Self {
            raw,
            _cancel: cancel,
            spec,
            metadata: Metadata {
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
                output_format: if meta.output_format == 1 {
                    PcmFormat::S16LE
                } else {
                    PcmFormat::S32LE
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
            },
        })
    }
    pub fn read(&mut self) -> Result<Option<Pcm>, String> {
        let mut data = vec![0u8; 4096 * self.spec.format.bytes_per_frame()];
        let (mut packets, mut frames, mut pos) = (0, 0, 0);
        // SAFETY: unique decoder borrow and a buffer sized for 4096 stereo frames.
        let n = unsafe {
            rb_media_read(
                self.raw.as_ptr(),
                data.as_mut_ptr(),
                4096,
                &mut packets,
                &mut frames,
                &mut pos,
            )
        };
        if n < 0 {
            return Err(error(n));
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
        let r = unsafe { rb_media_seek(self.raw.as_ptr(), ms as i64) };
        if r < 0 {
            Err(error(r))
        } else {
            Ok(())
        }
    }
    pub fn artwork(&mut self) -> Result<Vec<u8>, String> {
        let mut b = vec![0; 160 * 160 * 4];
        // SAFETY: C receives exactly side*side*4 writable bytes.
        let r = unsafe { rb_media_art(self.raw.as_ptr(), b.as_mut_ptr(), 160) };
        if r < 0 {
            Err(error(r))
        } else {
            Ok(b)
        }
    }
}
pub fn external_artwork(path: &Path) -> Result<Vec<u8>, String> {
    use std::os::unix::ffi::OsStrExt;
    let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| "NUL in path")?;
    let mut b = vec![0; 160 * 160 * 4];
    // SAFETY: C receives a valid local path and exactly side*side*4 writable bytes.
    let r = unsafe { rb_media_art_file(name.as_ptr(), b.as_mut_ptr(), 160) };
    if r < 0 {
        Err(error(r))
    } else {
        Ok(b)
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: unique context is closed exactly once.
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
                }],
                crossfade_ms: 0,
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
            for sample in block.data.chunks_exact(4) {
                peak =
                    peak.max(i32::from_le_bytes(sample.try_into().unwrap()).unsigned_abs() as i64);
            }
        }
        assert!(peak <= (i32::MAX as f64 * 0.981) as i64);
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
    fn runtime_manifest_is_json() {
        let components = runtime_components().unwrap();
        assert!(components["version"].as_str().unwrap().contains("9.0.1"));
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
            let pixels = d.artwork().unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(pixels.len(), 160 * 160 * 4);
            assert!(pixels.chunks_exact(4).any(|p| p[3] != 0), "{name}");
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
            let pixels = external_artwork(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(pixels.len(), 160 * 160 * 4);
            assert!(pixels.chunks_exact(4).any(|p| p[3] != 0), "{name}");
        }
    }
}
