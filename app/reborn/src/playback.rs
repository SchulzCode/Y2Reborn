use crate::volume::{VolumeRamp, MAX_BALANCE};
use reborn_audio::{AlsaSink, AudioSink, SinkSpec};
use reborn_core::{AudioOutput, Event, PcmFormat, QueueEntryId, Track};
use reborn_graphics::ART_SIZE;
use reborn_media::{
    convert_pcm, crossfade_pcm, external_artwork, folder_artwork_candidates, Cancel, Decoder,
    DspConfig, OutputSpec, Pcm,
};
use reborn_observability::{HealthState, Level, Observer};
use serde_json::json;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicI8, AtomicU64, AtomicU8, Ordering},
        mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
/// Fade-in of the first stream after the headphone level went up.
const LEVEL_RAISE_FADE_MS: u32 = 2000;

/// The gain a newly opened sink starts with, and the fade it logs: a fresh
/// fade-in when the headphone level (`raised`) or the DSP (`dsp_rise`) went
/// up, else the rest of a fade the previous sink did not finish, else the
/// volume at once.
fn opening_gain(
    volume: u8,
    rate: u32,
    raised: bool,
    dsp_rise: bool,
    carried: Option<(f64, u32)>,
) -> (VolumeRamp, Option<(&'static str, &'static str, u32)>) {
    if raised {
        let message = "headphone level went up: fading in";
        let ramp = VolumeRamp::fade_in(volume, rate, LEVEL_RAISE_FADE_MS);
        (
            ramp,
            Some(("level_raise_fade", message, LEVEL_RAISE_FADE_MS)),
        )
    } else if dsp_rise {
        let message = "sound settings play louder: fading in";
        let ramp = VolumeRamp::fade_in(volume, rate, LEVEL_RAISE_FADE_MS);
        (ramp, Some(("dsp_rise_fade", message, LEVEL_RAISE_FADE_MS)))
    } else if let Some((fraction, ms)) = carried {
        let message = "the previous stream's fade-in continues";
        let ramp = VolumeRamp::fade_in_from(volume, rate, ms, fraction);
        (ramp, Some(("fade_continued", message, ms)))
    } else {
        (VolumeRamp::new(volume, rate), None)
    }
}
/// What keeps a decoder's input valid: a pinned library file, or a radio
/// station's network transfer (stopped when the decoder is dropped).
enum Source {
    File(Box<reborn_platform::storage::PinnedTrack>),
    /// Held only so dropping the decoder also stops the transfer.
    Radio(#[allow(dead_code)] reborn_platform::radio::Stream),
    /// spotifyd's sound; dropping it detaches (spotifyd keeps running).
    Spotify(#[allow(dead_code)] reborn_platform::spotify::Audio),
}
struct BoundDecoder {
    decoder: Decoder,
    _source: Source,
}
impl BoundDecoder {
    fn open(
        track: &Track,
        music: &std::path::Path,
        spec: OutputSpec,
        dsp: DspConfig,
        cancel: Cancel,
    ) -> Result<Self, String> {
        if track.is_spotify() {
            let audio = reborn_platform::spotify::Audio::attach()
                .map_err(|e| format!("Spotify could not start: {e}"))?;
            let decoder = Decoder::open_stream(audio.reader(), spec, dsp, cancel)
                .map_err(|e| format!("Spotify could not be played: {e}"))?;
            return Ok(Self {
                decoder,
                _source: Source::Spotify(audio),
            });
        }
        if track.is_radio() {
            let url = track.path.to_string_lossy();
            let stream = reborn_platform::radio::Stream::start(&url)
                .map_err(|e| format!("radio station could not start: {e}"))?;
            let decoder = Decoder::open_stream(stream.reader(), spec, dsp, cancel).map_err(
                |e| match stream.state() {
                    reborn_platform::radio::StreamState::Failed(reason) => reason,
                    _ => format!("radio station could not be played: {e}"),
                },
            )?;
            return Ok(Self {
                decoder,
                _source: Source::Radio(stream),
            });
        }
        let source = reborn_platform::storage::pin_track(track, music)?;
        let decoder = Decoder::open_with(source.path(), spec, dsp, cancel)?;
        Ok(Self {
            decoder,
            _source: Source::File(Box::new(source)),
        })
    }
}
impl std::ops::Deref for BoundDecoder {
    type Target = Decoder;
    fn deref(&self) -> &Decoder {
        &self.decoder
    }
}
impl std::ops::DerefMut for BoundDecoder {
    fn deref_mut(&mut self) -> &mut Decoder {
        &mut self.decoder
    }
}
pub enum PlaybackEvent {
    Core(Event),
    Artwork {
        generation: u64,
        entry_id: QueueEntryId,
        track_id: i64,
        bytes: Vec<u8>,
    },
}
pub struct LoadRequest {
    pub track: Track,
    pub music_root: PathBuf,
    pub entry_id: QueueEntryId,
    pub queue: Vec<Track>,
    pub queue_entry_ids: Vec<QueueEntryId>,
    pub position: u64,
    pub spec: SinkSpec,
    pub generation: u64,
    pub dsp: DspConfig,
    pub gapless_enabled: bool,
    pub id: u64,
    /// Fade the new stream in: its DSP plays louder than the last one's.
    pub fade_in: bool,
}
struct Job {
    track: Track,
    music_root: PathBuf,
    entry_id: QueueEntryId,
    queue: Vec<Track>,
    queue_entry_ids: Vec<QueueEntryId>,
    spec: SinkSpec,
    dsp: DspConfig,
    gapless_enabled: bool,
    position: u64,
    generation: u64,
    id: u64,
    cancel: Cancel,
}
/// What follows the song playing, sent while it plays (a queue edit, the
/// next song reached): the decoder takes it at its next block instead of the
/// song starting over (Beta 8.33; every edit used to reload the song).
struct Upcoming {
    generation: u64,
    tracks: Vec<Track>,
    entry_ids: Vec<QueueEntryId>,
}
fn take_upcoming(slot: &Mutex<Option<Upcoming>>, generation: u64) -> Option<Upcoming> {
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|u| u.generation == generation) {
        slot.take()
    } else {
        None
    }
}
struct PendingStart {
    opened: Receiver<Result<(), String>>,
    job: Box<Job>,
    deadline: Instant,
}
enum DecodeCommand {
    Load(Box<Job>),
    Stop,
}
// Keep the sink specification inline in the bounded command channel; avoid an
// extra allocation in this already bounded, infrequent control path.
#[allow(clippy::large_enum_variant)]
enum SinkCommand {
    Shutdown(SyncSender<()>),
    Open {
        generation: u64,
        spec: SinkSpec,
        id: u64,
        fade_in: bool,
        reply: SyncSender<Result<(), String>>,
    },
    Stop(Option<SyncSender<()>>),
    /// Stop for the next stream: a wired sink is kept (the reply names it).
    Switch(SyncSender<Option<SinkSpec>>),
}
enum Stream {
    Pcm {
        generation: u64,
        block: Pcm,
    },
    Boundary {
        generation: u64,
        next_entry_id: Option<QueueEntryId>,
        next_track_id: Option<i64>,
        transition_ms: u64,
    },
    Artwork {
        generation: u64,
        entry_id: QueueEntryId,
        track_id: i64,
        bytes: Vec<u8>,
    },
    End {
        generation: u64,
    },
}
pub struct Playback {
    decode: SyncSender<DecodeCommand>,
    sink: SyncSender<SinkCommand>,
    pub events: Receiver<PlaybackEvent>,
    #[cfg(test)]
    event_sender: SyncSender<PlaybackEvent>,
    epoch: Arc<AtomicU64>,
    volume: Arc<AtomicU8>,
    /// Left/right balance in dB, -10..=10 (negative: the left is louder).
    balance: Arc<AtomicI8>,
    /// Paused in place: the audio thread stops writing and pauses the open
    /// device; the decoder waits on the full queue. Resume is instant.
    hold: Arc<AtomicBool>,
    upcoming: Arc<Mutex<Option<Upcoming>>>,
    cancel: Option<Cancel>,
    pending_start: Option<PendingStart>,
    sink_released: bool,
    state: Arc<Mutex<serde_json::Value>>,
    observer: Observer,
    /// The music's loudness as the output plays it (the wave, Beta 8.38).
    levels: crate::meter::Shared,
}
fn emit(tx: &SyncSender<PlaybackEvent>, e: Event) {
    let _ = tx.send(PlaybackEvent::Core(e));
}
fn emit_artwork(
    tx: &SyncSender<PlaybackEvent>,
    generation: u64,
    entry_id: QueueEntryId,
    track_id: i64,
    bytes: Vec<u8>,
) {
    let _ = tx.send(PlaybackEvent::Artwork {
        generation,
        entry_id,
        track_id,
        bytes,
    });
}
fn send_before_deadline<T>(
    sender: &SyncSender<T>,
    mut item: T,
    deadline: Instant,
    unavailable: &str,
    timed_out: &str,
) -> Result<(), String> {
    loop {
        if Instant::now() >= deadline {
            return Err(timed_out.into());
        }
        match sender.try_send(item) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Disconnected(_)) => return Err(unavailable.into()),
            Err(TrySendError::Full(returned)) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(timed_out.into());
                }
                item = returned;
                thread::sleep(remaining.min(Duration::from_millis(1)));
            }
        }
    }
}
fn send_stream_item(
    tx: &SyncSender<Stream>,
    buffered: &AtomicU64,
    epoch: &AtomicU64,
    generation: u64,
    mut item: Stream,
    frames: u64,
    log: &Observer,
) -> Result<(), String> {
    let mut stalled = false;
    let mut beat = Instant::now();
    loop {
        if epoch.load(Ordering::Acquire) != generation {
            return Ok(());
        }
        buffered.fetch_add(frames, Ordering::Relaxed);
        match tx.try_send(item) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(v)) => {
                buffered.fetch_sub(frames, Ordering::Relaxed);
                item = v;
                if !stalled {
                    log.add("decoder_stalls", 1.);
                    stalled = true;
                }
                // A paused player keeps the queue full for minutes: waiting
                // for room is the decoder working, not hanging.
                if beat.elapsed() >= Duration::from_secs(1) {
                    log.heartbeat("playback", 30);
                    beat = Instant::now();
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return Err("audio queue closed".into()),
        }
    }
}

const MAX_SINK_WRITE_BYTES: usize = 524_288;

/* Keep every queued PCM block within the direct ALSA writer's byte limit.
 * This matters for the configured 5/10/15 second crossfade windows, which
 * are intentionally produced by FFmpeg as one transition result. */
fn send_stream(
    tx: &SyncSender<Stream>,
    buffered: &AtomicU64,
    epoch: &AtomicU64,
    generation: u64,
    item: Stream,
    frames: u64,
    log: &Observer,
) -> Result<(), String> {
    let Stream::Pcm {
        generation: item_generation,
        block,
    } = item
    else {
        return send_stream_item(tx, buffered, epoch, generation, item, frames, log);
    };
    if item_generation != generation {
        return Ok(());
    }
    let frame_bytes = block.format.bytes_per_frame();
    let total_frames = usize::try_from(block.frames)
        .map_err(|_| "PCM frame count exceeds platform bounds".to_string())?;
    if frame_bytes == 0 || total_frames == 0 || block.data.len() != total_frames * frame_bytes {
        return Err("PCM block is not frame aligned".into());
    }
    let max_frames = MAX_SINK_WRITE_BYTES / frame_bytes;
    if max_frames == 0 {
        return Err("PCM format exceeds sink byte limit".into());
    }
    let mut offset = 0usize;
    while offset < total_frames {
        let count = (total_frames - offset).min(max_frames);
        let start = offset * frame_bytes;
        let end = (offset + count) * frame_bytes;
        let chunk = Pcm {
            data: block.data[start..end].to_vec(),
            position_ms: block.position_ms + offset as u64 * 1000 / u64::from(block.rate),
            packets: if offset == 0 { block.packets } else { 0 },
            frames: count as u64,
            rate: block.rate,
            format: block.format,
        };
        send_stream_item(
            tx,
            buffered,
            epoch,
            generation,
            Stream::Pcm {
                generation,
                block: chunk,
            },
            count as u64,
            log,
        )?;
        offset += count;
    }
    Ok(())
}

struct TailWindow {
    data: Vec<u8>,
    position_ms: u64,
    rate: u32,
    format: PcmFormat,
}
impl TailWindow {
    fn new(rate: u32, format: PcmFormat) -> Self {
        Self {
            data: Vec::new(),
            position_ms: 0,
            rate,
            format,
        }
    }
    fn push(&mut self, block: Pcm, retain_frames: usize) -> Option<Pcm> {
        if self.data.is_empty() {
            self.position_ms = block.position_ms;
        }
        self.data.extend_from_slice(&block.data);
        let frame_bytes = self.format.bytes_per_frame();
        let retain_bytes = retain_frames.saturating_mul(frame_bytes);
        let emit_bytes = self.data.len().saturating_sub(retain_bytes) / frame_bytes * frame_bytes;
        if emit_bytes == 0 {
            return None;
        }
        let emitted = self.data.drain(..emit_bytes).collect::<Vec<_>>();
        let frames = emitted.len() / frame_bytes;
        let position_ms = self.position_ms;
        self.position_ms = self
            .position_ms
            .saturating_add(frames as u64 * 1000 / u64::from(self.rate));
        Some(Pcm {
            data: emitted,
            position_ms,
            packets: block.packets,
            frames: frames as u64,
            rate: self.rate,
            format: self.format,
        })
    }
    fn frames(&self) -> usize {
        self.data.len() / self.format.bytes_per_frame()
    }
    fn take_prefix(&mut self, frames: usize) -> Option<Pcm> {
        let bytes = frames.saturating_mul(self.format.bytes_per_frame());
        if bytes == 0 || bytes > self.data.len() {
            return None;
        }
        let data = self.data.drain(..bytes).collect::<Vec<_>>();
        let position_ms = self.position_ms;
        self.position_ms = self
            .position_ms
            .saturating_add(frames as u64 * 1000 / u64::from(self.rate));
        Some(Pcm {
            data,
            position_ms,
            packets: 0,
            frames: frames as u64,
            rate: self.rate,
            format: self.format,
        })
    }
    fn take_all(&mut self) -> Option<Pcm> {
        let frames = self.frames();
        self.take_prefix(frames)
    }
}

fn read_pending_or_decoder(
    decoder: &mut Decoder,
    pending: &mut VecDeque<(Pcm, usize)>,
) -> Result<Option<Pcm>, String> {
    if let Some(block) = take_pending_block(pending) {
        return Ok(Some(block));
    }
    decoder.read()
}

fn take_pending_block(pending: &mut VecDeque<(Pcm, usize)>) -> Option<Pcm> {
    while let Some((block, offset)) = pending.pop_front() {
        let frame_bytes = block.format.bytes_per_frame();
        let start = offset.saturating_mul(frame_bytes);
        if start >= block.data.len() {
            continue;
        }
        let data = block.data[start..].to_vec();
        return Some(Pcm {
            position_ms: block.position_ms + offset as u64 * 1000 / u64::from(block.rate),
            packets: block.packets,
            frames: data.len() as u64 / frame_bytes as u64,
            rate: block.rate,
            format: block.format,
            data,
        });
    }
    None
}

struct WindowReadFailure {
    partial: Option<Pcm>,
    error: String,
}

fn window_pcm(data: Vec<u8>, position_ms: u64, rate: u32, format: PcmFormat) -> Option<Pcm> {
    let frames = data.len() / format.bytes_per_frame();
    (frames > 0).then_some(Pcm {
        data,
        position_ms,
        packets: 0,
        frames: frames as u64,
        rate,
        format,
    })
}

fn read_window_with(
    pending: &mut VecDeque<(Pcm, usize)>,
    wanted_frames: usize,
    mut read: impl FnMut() -> Result<Option<Pcm>, String>,
) -> Result<Option<Pcm>, WindowReadFailure> {
    let mut data = Vec::new();
    let mut position_ms = 0;
    let mut rate = 0;
    let mut format = PcmFormat::S32LE;
    let mut frames = 0usize;
    while frames < wanted_frames {
        let next = if let Some(block) = take_pending_block(pending) {
            Ok(Some(block))
        } else {
            read()
        };
        let next = match next {
            Ok(next) => next,
            Err(error) => {
                return Err(WindowReadFailure {
                    partial: window_pcm(data, position_ms, rate, format),
                    error,
                });
            }
        };
        let Some(block) = next else {
            break;
        };
        if frames == 0 {
            position_ms = block.position_ms;
            rate = block.rate;
            format = block.format;
        }
        let frame_bytes = block.format.bytes_per_frame();
        let need = wanted_frames - frames;
        let take = need.min(block.frames as usize);
        data.extend_from_slice(&block.data[..take * frame_bytes]);
        frames += take;
        if take < block.frames as usize {
            let remainder = Pcm {
                position_ms: block.position_ms + take as u64 * 1000 / u64::from(block.rate),
                packets: block.packets,
                frames: block.frames - take as u64,
                rate: block.rate,
                format: block.format,
                data: block.data[take * frame_bytes..].to_vec(),
            };
            pending.push_front((remainder, 0));
        }
    }
    if frames == 0 {
        return Ok(None);
    }
    Ok(window_pcm(data, position_ms, rate, format))
}

fn read_window(
    decoder: &mut Decoder,
    pending: &mut VecDeque<(Pcm, usize)>,
    wanted_frames: usize,
) -> Result<Option<Pcm>, WindowReadFailure> {
    read_window_with(pending, wanted_frames, || decoder.read())
}

fn recover_window_failure(tail: &mut TailWindow, failure: WindowReadFailure) -> Vec<Pcm> {
    let mut blocks = Vec::with_capacity(2);
    if let Some(suffix) = tail.take_all() {
        blocks.push(suffix);
    }
    if let Some(partial) = failure.partial {
        blocks.push(partial);
    }
    blocks
}

fn push_front_block(pending: &mut VecDeque<(Pcm, usize)>, block: Pcm) {
    if block.frames > 0 {
        pending.push_front((block, 0));
    }
}

fn update_media_state(
    state: &Arc<Mutex<serde_json::Value>>,
    track: &Track,
    decoder: &Decoder,
    job: &Job,
    processing_format: PcmFormat,
    preopened: bool,
) {
    if let Ok(mut value) = state.lock() {
        let alsa = value.get("alsa").cloned();
        *value = json!({
            "state":"buffering",
            "correlation_id":job.id,
            "generation":job.generation,
            "track_id":track.id,
            "filename":track.filename,
            "source":decoder.metadata.clone(),
            "decoder_format":decoder.metadata.decoder_sample_fmt.clone(),
            "internal_processing_format":"fltp",
            "filters":decoder.metadata.filters.clone(),
            "dsp":job.dsp.clone(),
            "output":{
                "rate":job.spec.rate,
                "format":job.spec.format,
                "processing_format":processing_format,
                "fallback":job.spec.fallback,
                "fallback_reason":job.spec.fallback_reason
            },
            "replay_gain":{
                "mode":decoder.metadata.replaygain_mode,
                "track_gain_db":decoder.metadata.track_gain_db,
                "album_gain_db":decoder.metadata.album_gain_db,
                "track_peak":decoder.metadata.track_peak,
                "album_peak":decoder.metadata.album_peak,
                "applied_gain_db":decoder.metadata.applied_gain_db,
                "headroom_db":decoder.metadata.headroom_db
            },
            "eq":{"active":decoder.metadata.eq_active},
            "resampling":{
                "active":decoder.metadata.resampling,
                "reason":decoder.metadata.resample_reason,
                "source_rate":decoder.metadata.sample_rate,
                "output_rate":job.spec.rate
            },
            "final_format_conversion":decoder.metadata.format_conversion,
            "gapless":{"enabled":job.gapless_enabled,"preopened":preopened},
            "crossfade":{"enabled":job.dsp.crossfade_ms>0,"duration_ms":job.dsp.crossfade_ms}
        });
        if let Some(alsa) = alsa {
            if let Some(object) = value.as_object_mut() {
                object.insert("alsa".into(), alsa);
            }
        }
    }
}

/// FFmpeg's AVERROR_EXIT ("Immediate exit requested"): the job's
/// cancellation interrupted the open.
fn open_cancelled(error: &str) -> bool {
    error.starts_with("FFmpeg -1414092869:")
}
/// A cached embedded cover: `ART_SIZE` RGBA. A cover cached at another size
/// (before Beta 8.18) is decoded again and replaced.
const ART_BYTES: usize = ART_SIZE * ART_SIZE * 4;
fn publish_artwork(
    cache: &std::path::Path,
    track: &Track,
    decoder: &mut BoundDecoder,
) -> Option<Vec<u8>> {
    let _ = std::fs::create_dir_all(cache);
    if let Ok(entries) = std::fs::read_dir(cache) {
        let mut files = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "rgba"))
            .collect::<Vec<_>>();
        files.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
        let remove = files.len().saturating_sub(63);
        for entry in files.into_iter().take(remove) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    // The folder's cover first, the embedded picture only when the folder has
    // none (same order as the thumbnail worker). Only embedded pictures are
    // cached here: a folder image can change without the track changing.
    let Source::File(pinned) = &decoder._source else {
        return None; // a station has no folder or embedded cover
    };
    if let Ok(folder) = pinned_folder_artwork(pinned) {
        return Some(folder);
    }
    let path = cache.join(format!("{}-{}-{}.rgba", track.id, track.size, track.mtime));
    let cached = decoder
        .metadata
        .artwork
        .then(|| std::fs::metadata(&path).ok())
        .flatten()
        .filter(|meta| meta.len() == ART_BYTES as u64)
        .and_then(|_| std::fs::read(&path).ok())
        .filter(|b| b.len() == ART_BYTES);
    let have_cached = cached.is_some();
    let embedded = cached.or_else(|| {
        decoder
            .metadata
            .artwork
            .then(|| decoder.artwork(ART_SIZE as u32))
            .and_then(Result::ok)
    });
    if let Some(bytes) = &embedded {
        if !have_cached {
            let _ = reborn_core::atomic_write(&path, bytes);
        }
    }
    embedded
}
fn pinned_folder_artwork(
    source: &reborn_platform::storage::PinnedTrack,
) -> Result<Vec<u8>, String> {
    for name in folder_artwork_candidates(&source.sidecar_directory())? {
        if let Ok(file) = source.open_sidecar(&name) {
            if let Ok(pixels) = external_artwork(file.path(), ART_SIZE as u32) {
                return Ok(pixels);
            }
        }
    }
    Err("no usable pinned folder artwork".into())
}
pub(crate) type SinkFactory =
    Box<dyn Fn(&SinkSpec, Observer, u64) -> Result<Box<dyn AudioSink>, String> + Send>;

/// How long a start waits for its sink. A wired open whose crystal misses its
/// start cycles the codec's rails in the kernel (≈1.9 s), and the sink opens a
/// second time: 2 s gave up during the second open even when it played (#52).
const OPEN_DEADLINE: Duration = Duration::from_secs(5);
/// What one slow open can take (the crystal case above): no retry starts so
/// late that it cannot finish inside `OPEN_DEADLINE`.
const SLOW_OPEN: Duration = Duration::from_secs(2);
/// A busy output is tried again for this long from the first open (F24: the
/// headphone open refused with ALSA -16 for 1–3 s after a wake).
const BUSY_OPEN_FOR: Duration = Duration::from_secs(2);
/// How long a wired sink stays open after its stream ended or was stopped
/// for the next one: the next track starts in it well within this.
const KEEP_SINK_FOR: Duration = Duration::from_secs(2);
/// The fade-in a kept sink starts the next stream with, so that the new
/// stream does not start with a click where the old one was cut.
const SWITCH_FADE_MS: u32 = 10;
/// A wired sink between two streams, its queued audio dropped. Reopening the
/// headphone output for every track power-cycles the codec's amplifier and
/// charge pump, on the rail of its crystal: a marginal crystal stopped after
/// enough of them (#52). The stock firmware keeps its output open too.
struct KeptSink {
    sink: Box<dyn AudioSink>,
    spec: SinkSpec,
    /// `reborn_audio::wired_epoch` when it opened.
    wired: u64,
    since: Instant,
}
/// Keep a wired sink for the next stream, its queued audio dropped at once
/// (nothing of the old track plays on). Any other sink, or one that cannot
/// drop its audio, closes.
fn keep_sink(
    sink: Option<Box<dyn AudioSink>>,
    opened: Option<(SinkSpec, u64)>,
    log: &Observer,
    id: u64,
) -> Option<KeptSink> {
    let (mut sink, (spec, wired)) = (sink?, opened?);
    if !matches!(spec.output, AudioOutput::Wired) {
        return None;
    }
    if let Err(e) = sink.discard() {
        log.emit(
            Level::Warn,
            "audio",
            "sink_keep_failed",
            &e,
            Some(id),
            json!({"recovery":"close"}),
        );
        return None;
    }
    Some(KeptSink {
        sink,
        spec,
        wired,
        since: Instant::now(),
    })
}
/// Why a kept sink cannot play the next stream; None: it can, being exactly
/// the sink that stream wants, with the wired level and DAC filter it opened
/// with (the amplifier takes a new choice only when it powers up).
fn sink_change(
    kept: &SinkSpec,
    kept_wired: u64,
    wanted: &SinkSpec,
    wired: u64,
) -> Option<&'static str> {
    if !matches!(kept.output, AudioOutput::Wired) || kept.output != wanted.output {
        Some("output")
    } else if kept.rate != wanted.rate {
        Some("rate")
    } else if kept.format != wanted.format {
        Some("format")
    } else if kept != wanted {
        Some("spec")
    } else if kept_wired != wired {
        Some("wired_settings")
    } else {
        None
    }
}
/// An output that is busy, not broken: ALSA -16 (EBUSY, also the activity
/// lease the sleep helper holds until the radios are back) or -11 (EAGAIN).
fn open_busy(error: &str) -> bool {
    error.starts_with("ALSA -16:") || error.starts_with("ALSA -11:")
}
/// The pause before busy retry `attempt` (from 1): 100 ms, growing to 250.
fn busy_open_delay(attempt: u32) -> Duration {
    Duration::from_millis((50 + 50 * u64::from(attempt)).min(250))
}
/// Open, and open again after a refusal. A codec whose crystal did not start
/// in time (#48) refuses one open; a second open a moment later plays. A busy
/// output is tried every 100–250 ms until `BUSY_OPEN_FOR` has passed. No retry
/// starts after `OPEN_DEADLINE` minus `SLOW_OPEN`. `refused` hears of each
/// refusal that is tried again, with the attempt number and the pause.
fn open_with_retries<T>(
    mut open: impl FnMut() -> Result<T, String>,
    mut refused: impl FnMut(&str, u32, Duration),
    mut pause: impl FnMut(Duration),
    elapsed: impl Fn() -> Duration,
) -> Result<T, String> {
    let mut attempt = 0u32;
    let mut other_retried = false;
    loop {
        let error = match open() {
            Ok(sink) => return Ok(sink),
            Err(error) => error,
        };
        let busy = open_busy(&error);
        let delay = if busy {
            busy_open_delay(attempt + 1)
        } else {
            Duration::from_millis(250)
        };
        let resume = elapsed() + delay;
        let again = resume + SLOW_OPEN <= OPEN_DEADLINE
            && if busy {
                resume <= BUSY_OPEN_FOR
            } else {
                !std::mem::replace(&mut other_retried, true)
            };
        if !again {
            return Err(error);
        }
        attempt += 1;
        refused(&error, attempt, delay);
        pause(delay);
    }
}
impl Playback {
    pub fn spawn(log: Observer, cache: PathBuf) -> Result<Self, String> {
        Self::with_sink(
            log,
            cache,
            Box::new(|spec, log, id| {
                let started = Instant::now();
                let sink = open_with_retries(
                    || AlsaSink::open_spec(spec, log.clone(), id),
                    |error, attempt, delay| {
                        let mut fields = json!({
                            "error":error,
                            "output":format!("{:?}", spec.output),
                            "attempt":attempt,
                            "retry_ms":delay.as_millis() as u64
                        });
                        if open_busy(error) {
                            fields["holders"] = reborn_platform::power::output_busy_holders();
                        }
                        log.emit(
                            Level::Warn,
                            "audio",
                            "sink_open_retry",
                            "audio output refused to open; trying again",
                            None,
                            fields,
                        );
                    },
                    thread::sleep,
                    || started.elapsed(),
                )?;
                Ok(Box::new(sink) as Box<dyn AudioSink>)
            }),
        )
    }
    pub(crate) fn with_sink(
        log: Observer,
        cache: PathBuf,
        factory: SinkFactory,
    ) -> Result<Self, String> {
        let (dt, dr) = sync_channel::<DecodeCommand>(4);
        let (st, sr) = sync_channel::<SinkCommand>(8);
        let (pt, pr) = sync_channel::<Stream>(8);
        let (et, er) = sync_channel(64);
        #[cfg(test)]
        let event_sender = et.clone();
        let epoch = Arc::new(AtomicU64::new(0));
        let buffered = Arc::new(AtomicU64::new(0));
        let volume = Arc::new(AtomicU8::new(100));
        let sink_volume = volume.clone();
        let balance = Arc::new(AtomicI8::new(0));
        let sink_balance = balance.clone();
        let levels = crate::meter::Shared::default();
        let mut meter = crate::meter::Meter::new(levels.clone());
        let hold = Arc::new(AtomicBool::new(false));
        let sink_hold = hold.clone();
        let upcoming = Arc::new(Mutex::new(None::<Upcoming>));
        let decode_upcoming = upcoming.clone();
        let state = Arc::new(Mutex::new(json!({
            "state":"stopped",
            "canonical_sample_format":"fltp",
            "output_format":"unknown",
            "gapless":{"enabled":true,"preopened":false},
            "crossfade":{"enabled":false,"duration_ms":0}
        })));
        let de = epoch.clone();
        let se = epoch.clone();
        // The playing job's speed × 1000 (an audiobook's tempo): output time
        // is source time divided by it, so positions scale back by it.
        let tempo = Arc::new(AtomicU64::new(1000));
        let decode_tempo = tempo.clone();
        let sink_tempo = tempo;
        let db = buffered.clone();
        let ab = buffered.clone();
        let dl = log.clone();
        let al = log;
        let det = et.clone();
        let cache = Arc::new(cache);
        let media_state = state.clone();
        let audio_state = state.clone();
        let state_observer = al.clone();
        thread::Builder::new()
            .name("playback".into())
            .spawn(move || loop {
                dl.heartbeat("playback", 20);
                let job = match dr.recv_timeout(Duration::from_millis(500)) {
                    Ok(DecodeCommand::Load(j)) => j,
                            Ok(DecodeCommand::Stop) => continue,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                };
                if de.load(Ordering::Acquire) != job.generation {
                    continue;
                }
                decode_tempo.store(
                    (job.dsp.tempo() * 1000.).round() as u64,
                    Ordering::Release,
                );
                // A station may wait up to 45 s for data while it reconnects
                // (media.c RB_STREAM_STALL_MS); that is not a stuck worker.
                let heartbeat_s = if job.track.is_live() { 60 } else { 20 };
                dl.heartbeat("playback", heartbeat_s);
                let operation: Result<(), String> = (|| {
                    let mut tracks = Vec::with_capacity(1 + job.queue.len());
                    tracks.push(job.track.clone());
                    tracks.extend(job.queue.clone());
                    // The song decoding now, as an index into `tracks`.
                    let mut current_index = 0usize;
                    let mut entry_ids = Vec::with_capacity(1 + job.queue_entry_ids.len());
                    entry_ids.push(job.entry_id);
                    entry_ids.extend(job.queue_entry_ids.iter().copied());
                    if entry_ids.len() != tracks.len() {
                        return Err("queue entry identity and track schedule diverged".into());
                    }
                    let crossfade_enabled = job.dsp.crossfade_ms > 0;
                    /* Crossfade uses S32 as its bounded transition working
                     * format. The sink worker performs the one final
                     * conversion to the negotiated sink format. */
                    let processing_format = if crossfade_enabled {
                        PcmFormat::S32LE
                    } else {
                        job.spec.format
                    };
                    let output = OutputSpec {
                        rate: job.spec.rate,
                        format: processing_format,
                    };
                    let mut decoder = BoundDecoder::open(
                        &tracks[0],
                        &job.music_root,
                        output,
                        job.dsp.clone(),
                        job.cancel.clone(),
                    )?;
                    update_media_state(
                        &media_state,
                        &tracks[0],
                        &decoder,
                        &job,
                        processing_format,
                        tracks.len() > 1,
                    );
                    dl.add("audio_filter_rebuilds", 1.);
                    if decoder.metadata.resampling {
                        dl.add("audio_resamples", 1.);
                    }
                    dl.add("ffmpeg_corrupt_packets", decoder.metadata.corrupt_packets as f64);
                    // A live station has no position to return to.
                    if job.position > 0 && !tracks[0].is_live() {
                        decoder.seek(job.position)?;
                    }
                    let artwork = if tracks[0].is_live() {
                        None
                    } else {
                        publish_artwork(&cache, &tracks[0], &mut decoder)
                    };
                    if let Some(bytes) = artwork {
                        send_stream_item(
                            &pt,
                            &db,
                            &de,
                            job.generation,
                            Stream::Artwork {
                                generation: job.generation,
                                entry_id: job.entry_id,
                                track_id: tracks[0].id,
                                bytes,
                            },
                            0,
                            &dl,
                        )?;
                    }
                    let overlap_frames = (u64::from(job.spec.rate)
                        * u64::from(job.dsp.crossfade_ms)
                        / 1000) as usize;
                    let mut tail = TailWindow::new(job.spec.rate, processing_format);
                    let mut decoder_pending = VecDeque::new();
                    let mut next_decoder = None;
                    let mut next_pending = VecDeque::new();
                    let mut packet_count = 0u64;
                    let open_next = |tracks: &[Track], start: usize| -> Option<(usize, BoundDecoder)> {
                        for (candidate, track) in tracks.iter().enumerate().skip(start) {
                            match BoundDecoder::open(
                                track,
                                &job.music_root,
                                output,
                                job.dsp.clone(),
                                job.cancel.clone(),
                            ) {
                                Ok(next) => {
                                    return Some((candidate, next));
                                }
                                // A stopped or replaced job makes every open
                                // exit at once: no file failed (F29, ten of
                                // these per quick Next).
                                Err(e) if open_cancelled(&e) || job.cancel.is_cancelled() => {
                                    return None;
                                }
                                Err(e) => dl.emit(
                                    Level::Warn,
                                    "playback",
                                    "next_track_open_failed",
                                    &e,
                                    Some(job.id),
                                    json!({"track_id":track.id,"recovery":"skip_to_next_track"}),
                                ),
                            }
                        }
                        None
                    };
                    let mut preopened_index = None;
                    if job.gapless_enabled || crossfade_enabled {
                        if let Some((candidate, next)) = open_next(&tracks, 1) {
                            preopened_index = Some(candidate);
                            next_decoder = Some(next);
                        }
                    }
                    loop {
                        dl.heartbeat("playback", heartbeat_s);
                        if de.load(Ordering::Acquire) != job.generation {
                            return Ok(());
                        }
                        if let Some(next) = take_upcoming(&decode_upcoming, job.generation) {
                            let kept = preopened_index.and_then(|p| entry_ids.get(p).copied());
                            tracks.truncate(current_index + 1);
                            entry_ids.truncate(current_index + 1);
                            tracks.extend(next.tracks);
                            entry_ids.extend(next.entry_ids);
                            let first = current_index + 1;
                            if kept.is_some() && entry_ids.get(first).copied() == kept {
                                preopened_index = Some(first);
                            } else {
                                // Another song comes next: open that one.
                                next_decoder = None;
                                next_pending.clear();
                                preopened_index = None;
                                if job.gapless_enabled || crossfade_enabled {
                                    if let Some((candidate, next)) = open_next(&tracks, first) {
                                        preopened_index = Some(candidate);
                                        next_decoder = Some(next);
                                    }
                                }
                            }
                        }
                        if let Some(block) =
                            read_pending_or_decoder(&mut decoder, &mut decoder_pending)?
                        {
                            let packet_delta = block.packets.saturating_sub(packet_count);
                            packet_count = packet_count.max(block.packets);
                            dl.add("ffmpeg_packets_decoded", packet_delta as f64);
                            dl.add("ffmpeg_frames_decoded", block.frames as f64);
                            let block_frames = block.frames;
                            if crossfade_enabled && overlap_frames > 0 {
                                if let Some(ready) = tail.push(block, overlap_frames) {
                                    let ready_frames = ready.frames;
                                    send_stream(
                                        &pt,
                                        &db,
                                        &de,
                                        job.generation,
                                        Stream::Pcm {
                                            generation: job.generation,
                                            block: ready,
                                        },
                                        ready_frames,
                                        &dl,
                                    )?;
                                }
                            } else {
                                send_stream(
                                    &pt,
                                    &db,
                                    &de,
                                    job.generation,
                                    Stream::Pcm {
                                        generation: job.generation,
                                        block,
                                    },
                                    block_frames,
                                    &dl,
                                )?;
                            }
                            continue;
                        }
                        if next_decoder.is_none() {
                            if crossfade_enabled {
                                if let Some(ready) = tail.take_all() {
                                    let ready_frames = ready.frames;
                                    send_stream(
                                        &pt,
                                        &db,
                                        &de,
                                        job.generation,
                                        Stream::Pcm {
                                            generation: job.generation,
                                            block: ready,
                                        },
                                        ready_frames,
                                        &dl,
                                    )?;
                                }
                            }
                            send_stream(
                                &pt,
                                &db,
                                &de,
                                job.generation,
                                Stream::End {
                                    generation: job.generation,
                                },
                                0,
                                &dl,
                            )?;
                            return Ok(());
                        }
                        let next_index = preopened_index
                            .take()
                            .ok_or_else(|| "preopened next decoder index disappeared".to_string())?;
                        let transition_started = Instant::now();
                        let mut next = next_decoder
                            .take()
                            .ok_or_else(|| "preopened next decoder disappeared".to_string())?;
                        if crossfade_enabled && overlap_frames > 0 {
                            match read_window(&mut next, &mut next_pending, overlap_frames) {
                                Ok(Some(head)) => {
                                    let fade_frames = tail.frames().min(head.frames as usize);
                                    let frame_bytes = head.format.bytes_per_frame();
                                    if tail.frames() > fade_frames {
                                        if let Some(prefix) =
                                            tail.take_prefix(tail.frames() - fade_frames)
                                        {
                                            let prefix_frames = prefix.frames;
                                            send_stream(
                                                &pt,
                                                &db,
                                                &de,
                                                job.generation,
                                                Stream::Pcm {
                                                    generation: job.generation,
                                                    block: prefix,
                                                },
                                                prefix_frames,
                                                &dl,
                                            )?;
                                        }
                                    }
                                    if let Some(suffix) = tail.take_all() {
                                        let head_bytes = &head.data[..fade_frames * frame_bytes];
                                        match crossfade_pcm(
                                            &suffix.data,
                                            head_bytes,
                                            fade_frames,
                                            suffix.rate,
                                            suffix.format,
                                        ) {
                                            Ok(data) => {
                                                let block = Pcm {
                                                    data,
                                                    position_ms: suffix.position_ms,
                                                    packets: 0,
                                                    frames: fade_frames as u64,
                                                    rate: suffix.rate,
                                                    format: suffix.format,
                                                };
                                                dl.add("audio_crossfades", 1.);
                                                let block_frames = block.frames;
                                                send_stream(
                                                    &pt,
                                                    &db,
                                                    &de,
                                                    job.generation,
                                                    Stream::Pcm {
                                                        generation: job.generation,
                                                        block,
                                                    },
                                                    block_frames,
                                                    &dl,
                                                )?;
                                            }
                                            Err(e) => {
                                                dl.add("audio_crossfade_errors", 1.);
                                                dl.emit(
                                                    Level::Warn,
                                                    "playback",
                                                    "crossfade_failed",
                                                    &e,
                                                    Some(job.id),
                                                    json!({"track_id":tracks[next_index].id}),
                                                );
                                                let suffix_frames = suffix.frames;
                                                send_stream(
                                                    &pt,
                                                    &db,
                                                    &de,
                                                    job.generation,
                                                    Stream::Pcm {
                                                        generation: job.generation,
                                                        block: suffix,
                                                    },
                                                    suffix_frames,
                                                    &dl,
                                                )?;
                                                /* The next-track prefix was already removed from
                                                 * the decoder window. If mixing fails, put that
                                                 * exact prefix back on the stream after the old
                                                 * suffix so no audible samples disappear. */
                                                if fade_frames > 0 {
                                                    let prefix = Pcm {
                                                        data: head.data[..fade_frames * frame_bytes]
                                                            .to_vec(),
                                                        position_ms: head.position_ms,
                                                        packets: head.packets,
                                                        frames: fade_frames as u64,
                                                        rate: head.rate,
                                                        format: head.format,
                                                    };
                                                    let prefix_frames = prefix.frames;
                                                    send_stream(
                                                        &pt,
                                                        &db,
                                                        &de,
                                                        job.generation,
                                                        Stream::Pcm {
                                                            generation: job.generation,
                                                            block: prefix,
                                                        },
                                                        prefix_frames,
                                                        &dl,
                                                    )?;
                                                }
                                            }
                                        }
                                    }
                                    if head.frames as usize > fade_frames {
                                        let offset = fade_frames * frame_bytes;
                                        push_front_block(
                                            &mut next_pending,
                                            Pcm {
                                                data: head.data[offset..].to_vec(),
                                                position_ms: head.position_ms
                                                    + fade_frames as u64 * 1000
                                                        / u64::from(head.rate),
                                                packets: head.packets,
                                                frames: head.frames - fade_frames as u64,
                                                rate: head.rate,
                                                format: head.format,
                                            },
                                        );
                                    }
                                }
                                Ok(None) => {
                                    if let Some(ready) = tail.take_all() {
                                        let ready_frames = ready.frames;
                                        send_stream(
                                            &pt,
                                            &db,
                                            &de,
                                            job.generation,
                                            Stream::Pcm {
                                                generation: job.generation,
                                                block: ready,
                                            },
                                            ready_frames,
                                            &dl,
                                        )?;
                                    }
                                }
                                Err(e) => {
                                    dl.add("audio_crossfade_errors", 1.);
                                    dl.emit(
                                        Level::Warn,
                                        "playback",
                                        "crossfade_input_failed",
                                        &e.error,
                                        Some(job.id),
                                        json!({"track_id":tracks[next_index].id,"recovery":"old_suffix_then_consumed_next_prefix"}),
                                    );
                                    for ready in recover_window_failure(&mut tail, e) {
                                        let ready_frames = ready.frames;
                                        send_stream(
                                            &pt,
                                            &db,
                                            &de,
                                            job.generation,
                                            Stream::Pcm {
                                                generation: job.generation,
                                                block: ready,
                                            },
                                            ready_frames,
                                            &dl,
                                        )?;
                                    }
                                }
                            }
                        }
                        send_stream(
                            &pt,
                            &db,
                            &de,
                            job.generation,
                            Stream::Boundary {
                                generation: job.generation,
                                next_entry_id: Some(entry_ids[next_index]),
                                next_track_id: Some(tracks[next_index].id),
                                transition_ms: transition_started.elapsed().as_millis() as u64,
                            },
                            0,
                            &dl,
                        )?;
                        if let Some(bytes) =
                            publish_artwork(&cache, &tracks[next_index], &mut next)
                        {
                            send_stream_item(
                                &pt,
                                &db,
                                &de,
                                job.generation,
                                Stream::Artwork {
                                    generation: job.generation,
                                    entry_id: entry_ids[next_index],
                                    track_id: tracks[next_index].id,
                                    bytes,
                                },
                                0,
                                &dl,
                            )?;
                        }
                        if let Some((candidate, following)) = open_next(&tracks, next_index + 1) {
                            preopened_index = Some(candidate);
                            next_decoder = Some(following);
                        }
                        update_media_state(
                            &media_state,
                            &tracks[next_index],
                            &next,
                            &job,
                            processing_format,
                            next_decoder.is_some(),
                        );
                        dl.add("audio_filter_rebuilds", 1.);
                        if next.metadata.resampling {
                            dl.add("audio_resamples", 1.);
                        }
                        dl.add("ffmpeg_corrupt_packets", next.metadata.corrupt_packets as f64);
                        decoder = next;
                        current_index = next_index;
                        packet_count = 0;
                        decoder_pending = std::mem::take(&mut next_pending);
                        next_pending = VecDeque::new();
                        tail = TailWindow::new(job.spec.rate, processing_format);
                    }
                })();
                if let Err(e) = operation {
                    if de.load(Ordering::Acquire) == job.generation {
                        dl.add("ffmpeg_decode_errors", 1.);
                        dl.add("playback_errors", 1.);
                        dl.emit(
                            Level::Error,
                            "playback",
                            "decode_failed",
                            &e,
                            Some(job.id),
                            json!({"track_id":job.track.id,"recovery":"pause","state":"decoding"}),
                        );
                        emit(
                            &det,
                            Event::PlaybackError {
                                generation: job.generation,
                                message: e,
                            },
                        );
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                let mut sink: Option<Box<dyn AudioSink>> = None;
                let mut generation = 0;
                let mut correlation = 0;
                let mut rate = 44100;
                let mut sink_format = PcmFormat::S32LE;
                let mut gain = VolumeRamp::new(100, rate);
                let mut pending: Option<(Pcm, usize)> = None;
                let mut started = false;
                let mut eof = false;
                let mut drain_start = Instant::now();
                let mut last_progress = Instant::now();
                let mut held = false;
                // The playing sink's spec and wired epoch, and a wired sink
                // kept open for the next stream.
                let mut open_spec: Option<(SinkSpec, u64)> = None;
                let mut kept: Option<KeptSink> = None;
                loop {
                    al.heartbeat("audio", 10);
                    // Without a PCM sink there is no audio deadline. Wait on the
                    // command channel so Open/Stop/Shutdown still wake immediately;
                    // the timeout only maintains the worker heartbeat (and
                    // closes a kept sink on time).
                    let idle_command = if sink.is_none() {
                        let wait = kept.as_ref().map_or(Duration::from_secs(1), |k| {
                            KEEP_SINK_FOR
                                .saturating_sub(k.since.elapsed())
                                .max(Duration::from_millis(10))
                        });
                        match sr.recv_timeout(wait) {
                            Ok(command) => Some(command),
                            Err(RecvTimeoutError::Timeout) => None,
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    } else {
                        None
                    };
                    for cmd in idle_command.into_iter().chain(sr.try_iter()) {
                        match cmd {
                            SinkCommand::Shutdown(reply) => {
                                drop(sink.take());
                                drop(kept.take());
                                let _ = reply.try_send(());
                                return;
                            }
                            SinkCommand::Open {
                                generation: g,
                                spec,
                                id,
                                fade_in,
                                reply,
                            } => {
                                if g != se.load(Ordering::Acquire) {
                                    let _ = reply.try_send(Err("stale sink generation".into()));
                                    continue;
                                }
                                if let Some(k) =
                                    keep_sink(sink.take(), open_spec.take(), &al, correlation)
                                {
                                    kept = Some(k);
                                }
                                pending = None;
                                held = false;
                                generation = g;
                                correlation = id;
                                // An unfinished fade carries over (Next, a seek or another
                                // rate during it must not jump to full volume).
                                let carried = gain.fade_progress(rate);
                                let volume = sink_volume.load(Ordering::Relaxed);
                                rate = spec.rate;
                                sink_format = spec.format;
                                gain = opening_gain(volume, rate, false, false, carried).0;
                                gain.mono(spec.speaker);
                                started = false;
                                eof = false;
                                let wired = reborn_audio::wired_epoch();
                                let reused = kept.take().and_then(|k| {
                                    let summary = |s: &SinkSpec| {
                                        json!({"output":s.output,"rate":s.rate,"format":s.format.as_str(),"device":s.device})
                                    };
                                    match sink_change(&k.spec, k.wired, &spec, wired) {
                                        None => {
                                            al.emit(
                                                Level::Info,
                                                "audio",
                                                "sink_reused",
                                                "the open output plays the next stream",
                                                Some(id),
                                                json!({"spec":summary(&spec),"kept_ms":k.since.elapsed().as_millis() as u64}),
                                            );
                                            Some(k.sink)
                                        }
                                        Some(reason) => {
                                            al.emit(
                                                Level::Info,
                                                "audio",
                                                "sink_reopened",
                                                "the next stream needs another output",
                                                Some(id),
                                                json!({"reason":reason,"from":summary(&k.spec),"to":summary(&spec)}),
                                            );
                                            None
                                        }
                                    }
                                });
                                let opened = match reused {
                                    Some(s) => Ok((s, true)),
                                    None => factory(&spec, al.clone(), id).map(|s| (s, false)),
                                };
                                match opened {
                                    Ok((s, reused)) => {
                                        let params = s.parameters();
                                        // A kept stream did not power the
                                        // amplifier up again: no level rise.
                                        let (ramp, fade) = opening_gain(
                                            volume,
                                            rate,
                                            params.level_raised && !reused,
                                            fade_in,
                                            carried,
                                        );
                                        gain = match fade {
                                            None if reused => {
                                                VolumeRamp::fade_in(volume, rate, SWITCH_FADE_MS)
                                            }
                                            _ => ramp,
                                        };
                                        // #28: the speaker plays one channel.
                                        gain.mono(spec.speaker);
                                        if let Some((event, message, ms)) = fade {
                                            al.emit(
                                                Level::Info,
                                                "audio",
                                                event,
                                                message,
                                                Some(id),
                                                json!({"hp_level":params.hp_level_live,"fade_ms":ms}),
                                            );
                                        }
                                        if let Ok(mut value) = audio_state.lock() {
                                            if let Some(object) = value.as_object_mut() {
                                                object.insert(
                                                    "alsa".into(),
                                                    serde_json::to_value(&params)
                                                        .unwrap_or_default(),
                                                );
                                                object.insert(
                                                    "output_rate".into(),
                                                    json!(params.rate),
                                                );
                                                object.insert(
                                                    "output_format".into(),
                                                    json!(params.format),
                                                );
                                            }
                                        }
                                        if params.fallback && !reused {
                                            al.add("audio_sink_fallbacks", 1.);
                                        }
                                        open_spec = Some((spec, wired));
                                        sink = Some(s);
                                        al.health_set("audio", HealthState::Ok, true, "sink ready");
                                        let _ = reply.try_send(Ok(()));
                                    }
                                    Err(e) => {
                                        al.health_set("audio", HealthState::Failed, true, &e);
                                        let _ = reply.try_send(Err(e));
                                    }
                                }
                                last_progress = Instant::now();
                            }
                            SinkCommand::Stop(reply) => {
                                sink = None;
                                kept = None;
                                pending = None;
                                eof = false;
                                held = false;
                                if let Some(reply) = reply {
                                    let _ = reply.try_send(());
                                }
                            }
                            SinkCommand::Switch(reply) => {
                                if let Some(k) =
                                    keep_sink(sink.take(), open_spec.take(), &al, correlation)
                                {
                                    kept = Some(k);
                                }
                                pending = None;
                                eof = false;
                                held = false;
                                let _ = reply.try_send(kept.as_ref().map(|k| k.spec.clone()));
                            }
                        }
                    }
                    // Stopped or replaced: a wired sink waits for the next
                    // stream (a Stop closes it, as does the time running out).
                    if generation != se.load(Ordering::Acquire) {
                        if let Some(k) = keep_sink(sink.take(), open_spec.take(), &al, correlation)
                        {
                            kept = Some(k);
                        }
                        pending = None;
                        eof = false;
                    }
                    if kept
                        .as_ref()
                        .is_some_and(|k| k.since.elapsed() >= KEEP_SINK_FOR)
                    {
                        kept = None;
                    }
                    // Paused in place: keep the device open and paused, write
                    // nothing, read nothing (the decoder waits on the full
                    // queue). The stall and drain clocks restart on resume.
                    let want_hold = sink_hold.load(Ordering::Acquire) && sink.is_some();
                    if want_hold != held {
                        if let Some(s) = &mut sink {
                            if let Err(e) = s.pause(want_hold) {
                                al.emit(
                                    Level::Warn,
                                    "audio",
                                    "pause_failed",
                                    &e,
                                    Some(correlation),
                                    json!({"pause":want_hold}),
                                );
                            }
                        }
                        held = want_hold;
                        last_progress = Instant::now();
                        drain_start = Instant::now();
                    }
                    if held {
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    if pending.is_none() {
                        match pr.recv_timeout(Duration::from_millis(10)) {
                            Ok(Stream::Pcm {
                                generation: g,
                                mut block,
                            }) => {
                                ab.try_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                                    Some(v.saturating_sub(block.frames))
                                })
                                .ok();
                                if g == generation
                                    && g == se.load(Ordering::Acquire)
                                    && sink.is_some()
                                {
                                    if block.format != sink_format {
                                        match convert_pcm(
                                            &block.data,
                                            block.rate,
                                            block.format,
                                            sink_format,
                                        ) {
                                            Ok(data) => {
                                                block.data = data;
                                                block.format = sink_format;
                                                block.frames = block.data.len() as u64
                                                    / sink_format.bytes_per_frame() as u64;
                                            }
                                            Err(e) => {
                                                al.add("playback_errors", 1.);
                                                emit(
                                                    &et,
                                                    Event::PlaybackError {
                                                        generation: g,
                                                        message: e,
                                                    },
                                                );
                                                continue;
                                            }
                                        }
                                    }
                                    pending = Some((block, 0));
                                }
                            }
                            Ok(Stream::Artwork {
                                generation: g,
                                entry_id,
                                track_id,
                                bytes,
                            }) if g == generation
                                && g == se.load(Ordering::Acquire)
                                && sink.is_some() =>
                            {
                                emit_artwork(&et, g, entry_id, track_id, bytes);
                            }
                            Ok(Stream::Boundary {
                                generation: g,
                                next_entry_id,
                                next_track_id,
                                transition_ms,
                            }) if g == generation => {
                                started = false;
                                al.add("gapless_transitions", 1.);
                                al.gauge("gapless_transition_ms", transition_ms as f64);
                                if let Ok(mut value) = audio_state.lock() {
                                    if let Some(object) = value.as_object_mut() {
                                        object.insert(
                                            "gapless_transition_ms".into(),
                                            json!(transition_ms),
                                        );
                                        object.insert("state".into(), json!("buffering"));
                                    }
                                }
                                emit(
                                    &et,
                                    Event::TrackBoundary {
                                        generation: g,
                                        next_entry_id,
                                        next_track_id,
                                        output_position_ms: 0,
                                    },
                                );
                            }
                            Ok(Stream::End { generation: g }) if g == generation => {
                                eof = true;
                                drain_start = Instant::now();
                            }
                            Err(RecvTimeoutError::Disconnected) => break,
                            _ => {}
                        }
                    }
                    if let (Some(s), Some((block, offset))) = (&mut sink, &mut pending) {
                        let frame_bytes = block.format.bytes_per_frame();
                        // Bound gain work and control latency even for a large
                        // decoded crossfade. Keep original PCM for partial writes.
                        let max_bytes = 512 * frame_bytes;
                        let end = (*offset + max_bytes).min(block.data.len());
                        gain.target(sink_volume.load(Ordering::Relaxed));
                        gain.balance(sink_balance.load(Ordering::Relaxed));
                        let scaled = gain.render(&block.data[*offset..end], block.format);
                        match s.write(&scaled) {
                            Ok(n) => {
                                gain.advance(n);
                                if n > 0 {
                                    let written = *offset + n * frame_bytes;
                                    meter.measure(
                                        &block.data[*offset..written.min(block.data.len())],
                                        block.format,
                                        block.rate,
                                        s.delay(),
                                    );
                                    *offset += n * block.format.bytes_per_frame();
                                    last_progress = Instant::now();
                                    if !started {
                                        started = true;
                                        if let Ok(mut value) = audio_state.lock() {
                                            if let Some(object) = value.as_object_mut() {
                                                object.insert("state".into(), json!("playing"));
                                            }
                                        }
                                        al.emit(
                                            Level::Info,
                                            "audio",
                                            "started",
                                            "PCM playback started",
                                            Some(correlation),
                                            json!({"generation":generation}),
                                        );
                                        al.add("playback_tracks_started", 1.);
                                        emit(&et, Event::TrackStarted { generation });
                                    }
                                    let frame_bytes = block.format.bytes_per_frame() as u64;
                                    // Played frames in source time (× speed).
                                    let ms = block.position_ms
                                        + (*offset as u64 / frame_bytes).saturating_sub(s.delay())
                                            * sink_tempo.load(Ordering::Acquire)
                                            / u64::from(block.rate);
                                    let _ = et.try_send(PlaybackEvent::Core(Event::Position {
                                        generation,
                                        ms,
                                    }));
                                }
                                if *offset >= block.data.len() {
                                    pending = None;
                                } else if last_progress.elapsed() > Duration::from_secs(3) {
                                    emit(
                                        &et,
                                        Event::PlaybackError {
                                            generation,
                                            message: "audio write made no progress for 3s".into(),
                                        },
                                    );
                                    sink = None;
                                    pending = None;
                                }
                            }
                            Err(e) => {
                                al.add("playback_errors", 1.);
                                al.health_set("audio", HealthState::Failed, true, &e);
                                emit(
                                    &et,
                                    Event::PlaybackError {
                                        generation,
                                        message: e,
                                    },
                                );
                                sink = None;
                                pending = None;
                            }
                        }
                    }
                    if eof
                        && pending.is_none()
                        && (sink.as_ref().is_none_or(|s| s.delay() == 0)
                            || drain_start.elapsed() > Duration::from_secs(3))
                    {
                        al.add("playback_tracks_completed", 1.);
                        al.emit(
                            Level::Info,
                            "audio",
                            "drained",
                            "PCM reached end of stream",
                            Some(correlation),
                            json!({"generation":generation}),
                        );
                        emit(&et, Event::TrackEnded { generation });
                        if let Ok(mut value) = audio_state.lock() {
                            if let Some(object) = value.as_object_mut() {
                                object.insert("state".into(), json!("stopped"));
                            }
                        }
                        // The next track (gapless off, Repeat One) starts in it.
                        if let Some(k) = keep_sink(sink.take(), open_spec.take(), &al, correlation)
                        {
                            kept = Some(k);
                        }
                        eof = false;
                    }
                    let n = ab.load(Ordering::Relaxed)
                        + pending
                            .as_ref()
                            .map(|(b, o)| {
                                (b.data.len() - o) as u64 / b.format.bytes_per_frame() as u64
                            })
                            .unwrap_or(0)
                        + sink.as_ref().map(|s| s.delay()).unwrap_or(0);
                    al.gauge("audio_buffer_frames", n as f64);
                    al.gauge("audio_buffer_ms", n as f64 * 1000. / f64::from(rate));
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            decode: dt,
            sink: st,
            events: er,
            #[cfg(test)]
            event_sender,
            epoch,
            volume,
            balance,
            hold,
            upcoming,
            cancel: None,
            pending_start: None,
            sink_released: true,
            state,
            observer: state_observer,
            levels,
        })
    }
    /// The music's loudness as heard now (none while nothing plays).
    pub fn heard(&self) -> Option<reborn_ui::Levels> {
        crate::meter::heard_now(&self.levels)
    }
    /// What follows the song playing changed (a queue edit, the next song
    /// reached): the decoder of `generation` takes it without a restart.
    pub fn set_upcoming(&self, generation: u64, tracks: Vec<Track>, entry_ids: Vec<QueueEntryId>) {
        *self.upcoming.lock().unwrap_or_else(|e| e.into_inner()) = Some(Upcoming {
            generation,
            tracks,
            entry_ids,
        });
    }
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn load(
        &mut self,
        track: Track,
        queue: Vec<Track>,
        position: u64,
        spec: SinkSpec,
        generation: u64,
        dsp: DspConfig,
        id: u64,
    ) -> Result<(), String> {
        let queue_entry_ids = (2..=queue.len() as u64 + 1).map(QueueEntryId).collect();
        self.load_with_gapless(LoadRequest {
            music_root: track
                .path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .into(),
            track,
            entry_id: QueueEntryId(1),
            queue,
            queue_entry_ids,
            position,
            spec,
            generation,
            dsp,
            gapless_enabled: true,
            id,
            fade_in: false,
        })
    }
    #[cfg(test)]
    pub fn load_with_gapless(&mut self, request: LoadRequest) -> Result<(), String> {
        self.stop_and_wait(request.generation)?;
        self.start_with_gapless(request)
    }
    #[cfg(test)]
    pub(crate) fn start_with_gapless(&mut self, request: LoadRequest) -> Result<(), String> {
        self.begin_start_with_gapless(request)?;
        loop {
            if let Some(result) = self.poll_start() {
                return result;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
    pub(crate) fn begin_start_with_gapless(&mut self, request: LoadRequest) -> Result<(), String> {
        if self.pending_start.is_some() {
            return Err("audio sink open is already pending".into());
        }
        let LoadRequest {
            track,
            music_root,
            entry_id,
            queue,
            queue_entry_ids,
            position,
            spec,
            generation,
            mut dsp,
            gapless_enabled,
            id,
            fade_in,
        } = request;
        if !(8000..=384_000).contains(&spec.rate) {
            return Err("unsupported PCM rate".into());
        }
        if !self.sink_released {
            return Err("audio sink release acknowledgement required before open".into());
        }
        let cancel = Cancel::new()?;
        self.hold.store(false, Ordering::Release);
        self.set_volume(dsp.volume);
        // Decode once at neutral user gain; ReplayGain/EQ/headroom/limiting
        // stay in FFmpeg. The live sink applies user attenuation after them.
        dsp.volume = 100;
        let (reply, opened) = sync_channel(1);
        let deadline = Instant::now() + OPEN_DEADLINE;
        let job = Box::new(Job {
            track,
            music_root,
            entry_id,
            queue,
            queue_entry_ids,
            spec: spec.clone(),
            dsp,
            gapless_enabled,
            position,
            generation,
            id,
            cancel: cancel.clone(),
        });
        match self.sink.try_send(SinkCommand::Open {
            generation,
            spec,
            id,
            fade_in,
            reply,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                cancel.cancel();
                return Err("audio worker busy".into());
            }
            Err(TrySendError::Disconnected(_)) => {
                cancel.cancel();
                return Err("audio worker unavailable".into());
            }
        }
        self.sink_released = false;
        self.cancel = Some(cancel);
        self.pending_start = Some(PendingStart {
            opened,
            job,
            deadline,
        });
        Ok(())
    }
    /// Pause in place (true) or continue (false) the playback that runs.
    pub fn hold(&self, on: bool) {
        self.hold.store(on, Ordering::Release);
    }
    /// A pause is held in place; any stop or new start ends it.
    pub fn is_held(&self) -> bool {
        self.hold.load(Ordering::Acquire)
    }
    /// Left/right balance in dB (negative: the left is louder), live like
    /// the volume.
    pub(crate) fn set_balance(&self, balance: i8) {
        self.balance
            .store(balance.clamp(-MAX_BALANCE, MAX_BALANCE), Ordering::Relaxed);
    }
    pub(crate) fn set_volume(&self, volume: u8) {
        self.volume.store(volume.min(100), Ordering::Relaxed);
        self.observer
            .gauge("audio_user_volume", f64::from(volume.min(100)));
    }
    pub(crate) fn has_pending_start(&self) -> bool {
        self.pending_start.is_some()
    }
    pub(crate) fn poll_start(&mut self) -> Option<Result<(), String>> {
        let PendingStart {
            opened,
            job,
            deadline,
        } = self.pending_start.take()?;
        let job = job;
        let opened_result = match opened.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) if Instant::now() < deadline => {
                self.pending_start = Some(PendingStart {
                    opened,
                    job,
                    deadline,
                });
                return None;
            }
            Err(TryRecvError::Empty) => Err("audio open deadline exceeded".into()),
            Err(TryRecvError::Disconnected) => Err("audio worker unavailable".into()),
        };
        if let Err(error) = opened_result {
            job.cancel.cancel();
            self.cancel.take();
            self.invalidate_start(job.generation);
            return Some(Err(error));
        }
        let cancel = job.cancel.clone();
        let generation = job.generation;
        match self.decode.try_send(DecodeCommand::Load(job)) {
            Ok(()) => {
                self.cancel = Some(cancel);
                Some(Ok(()))
            }
            Err(_) => {
                cancel.cancel();
                self.cancel.take();
                self.invalidate_start(generation);
                Some(Err("playback command queue full".into()))
            }
        }
    }
    fn invalidate_start(&mut self, generation: u64) {
        self.hold.store(false, Ordering::Release);
        let invalidated = generation.wrapping_add(1);
        self.epoch.store(invalidated, Ordering::Release);
        let _ = self.sink.try_send(SinkCommand::Stop(None));
        let _ = self.decode.try_send(DecodeCommand::Stop);
    }
    pub fn stop(&mut self, generation: u64) {
        self.hold.store(false, Ordering::Release);
        self.epoch.store(generation, Ordering::Release);
        self.pending_start.take();
        if let Some(c) = self.cancel.take() {
            c.cancel()
        }
        let _ = self.sink.try_send(SinkCommand::Stop(None));
        let _ = self.decode.try_send(DecodeCommand::Stop);
    }
    pub fn stop_and_wait(&mut self, generation: u64) -> Result<(), String> {
        self.stop_and_wait_with_timeout(generation, Duration::from_secs(2))
    }
    /// Stop what plays for the next stream (a track change, a seek): a wired
    /// sink stays open with its queued audio dropped, and its spec comes back
    /// for the next start to ask for again; anything else is released as by
    /// `stop_and_wait`. A stop, a released pause and sleep still close it.
    pub fn switch_and_wait(&mut self, generation: u64) -> Result<Option<SinkSpec>, String> {
        self.release_and_wait(generation, Duration::from_secs(2), SinkCommand::Switch)
    }
    fn stop_and_wait_with_timeout(
        &mut self,
        generation: u64,
        timeout: Duration,
    ) -> Result<(), String> {
        self.release_and_wait(generation, timeout, |reply| SinkCommand::Stop(Some(reply)))
    }
    fn release_and_wait<T>(
        &mut self,
        generation: u64,
        timeout: Duration,
        command: impl FnOnce(SyncSender<T>) -> SinkCommand,
    ) -> Result<T, String> {
        let deadline = Instant::now() + timeout;
        self.hold.store(false, Ordering::Release);
        self.epoch.store(generation, Ordering::Release);
        self.pending_start.take();
        if let Some(c) = self.cancel.take() {
            c.cancel();
        }
        let (tx, rx) = sync_channel(1);
        send_before_deadline(
            &self.sink,
            command(tx),
            deadline,
            "audio worker unavailable",
            "audio sink release deadline exceeded",
        )?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        let result = rx
            .recv_timeout(remaining)
            .map_err(|_| "audio sink release deadline exceeded".to_string());
        if result.is_ok() {
            self.sink_released = true;
        }
        let _ = self.decode.try_send(DecodeCommand::Stop);
        result
    }
    pub fn shutdown(&mut self, generation: u64) -> Result<(), String> {
        self.stop(generation);
        let (tx, rx) = sync_channel(1);
        self.sink
            .try_send(SinkCommand::Shutdown(tx))
            .map_err(|_| "audio worker unavailable")?;
        rx.recv_timeout(Duration::from_secs(2))
            .map_err(|_| "audio shutdown deadline exceeded".into())
    }
    pub fn audio_state(&self) -> serde_json::Value {
        let mut value = self
            .state
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        if let Some(object) = value.as_object_mut() {
            object.insert("metrics".into(), self.observer.metrics());
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reborn_audio::Parameters;
    use reborn_core::{AudioOutput, PcmFormat};

    #[test]
    fn an_opened_sink_fades_for_a_rise_and_otherwise_continues_an_unfinished_fade() {
        let event =
            |raised, dsp, carried| opening_gain(100, 1000, raised, dsp, carried).1.map(|e| e.0);
        assert_eq!(event(true, true, None), Some("level_raise_fade"));
        assert_eq!(event(false, true, Some((0.5, 10))), Some("dsp_rise_fade"));
        assert_eq!(
            event(false, false, Some((0.5, 1000))),
            Some("fade_continued")
        );
        assert_eq!(event(false, false, None), None);
        // A fresh fade starts silent; a continued one where the last one was.
        let (fresh, _) = opening_gain(100, 1000, true, false, Some((0.5, 1000)));
        assert_eq!(fresh.fade_progress(1000), Some((0.0, LEVEL_RAISE_FADE_MS)));
        let (continued, _) = opening_gain(100, 1000, false, false, Some((0.5, 1000)));
        assert_eq!(continued.fade_progress(1000), Some((0.5, 1000)));
        assert!(opening_gain(100, 1000, false, false, None)
            .0
            .fade_progress(1000)
            .is_none());
    }

    struct Sink {
        frames: Arc<AtomicU64>,
        fail: bool,
        rate: u32,
        samples: Option<Arc<Mutex<Vec<i32>>>>,
    }
    impl AudioSink for Sink {
        fn parameters(&self) -> Parameters {
            Parameters {
                rate: self.rate,
                period: 512,
                buffer: 4096,
                format: PcmFormat::S32LE,
                channels: 2,
                hardware_mixer_gain_db: None,
                device: "test".into(),
                fallback: false,
                fallback_reason: String::new(),
                hp_level_live: None,
                level_raised: false,
            }
        }
        fn write(&mut self, s: &[u8]) -> Result<usize, String> {
            if self.fail {
                return Err("injected sink disconnect".into());
            }
            thread::sleep(Duration::from_millis(2));
            let n = (s.len() / 8).min(128);
            if let Some(samples) = &self.samples {
                let mut samples = samples.lock().unwrap();
                for frame in s.as_chunks::<8>().0.iter().take(n) {
                    samples.push(i32::from_le_bytes(frame[..4].try_into().unwrap()));
                }
            }
            self.frames.fetch_add(n as u64, Ordering::Relaxed);
            Ok(n)
        }
        fn discard(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn delay(&self) -> u64 {
            0
        }
    }
    /// Runs `open_with_retries` on a virtual clock that only the pauses move
    /// (plus `open_ms` per open); returns the result, opens and pauses (ms).
    fn retried(
        mut outcomes: Vec<Result<(), &'static str>>,
        open_ms: u64,
    ) -> (Result<(), String>, usize, Vec<u64>) {
        use std::cell::Cell;
        let clock = Cell::new(Duration::ZERO);
        let opens = Cell::new(0);
        let pauses = std::cell::RefCell::new(Vec::new());
        outcomes.reverse();
        let result = open_with_retries(
            || {
                opens.set(opens.get() + 1);
                clock.set(clock.get() + Duration::from_millis(open_ms));
                outcomes.pop().unwrap_or(Ok(())).map_err(str::to_owned)
            },
            |_, attempt, _| assert_eq!(attempt as usize, opens.get()),
            |delay| {
                pauses.borrow_mut().push(delay.as_millis() as u64);
                clock.set(clock.get() + delay);
            },
            || clock.get(),
        );
        (result, opens.get(), pauses.into_inner())
    }
    const BUSY: &str = "ALSA -16: Device or resource busy";
    const AGAIN: &str = "ALSA -11: Resource temporarily unavailable";
    const CRYSTAL: &str = "ALSA -5: Input/output error";

    #[test]
    fn a_busy_output_is_tried_again_for_two_seconds_with_short_pauses() {
        // Busy for ≈1.5 s after a wake, then free: it plays.
        let (result, opens, pauses) = retried(vec![Err(BUSY); 7], 1);
        assert!(result.is_ok());
        assert_eq!(opens, 8);
        assert_eq!(pauses, [100, 150, 200, 250, 250, 250, 250]);
        let (result, _, _) = retried(vec![Err(AGAIN), Err(BUSY)], 1);
        assert!(result.is_ok());
        // Never free: the busy error after ≈2 s, well inside the open deadline.
        let (result, opens, pauses) = retried(vec![Err(BUSY); 100], 1);
        assert_eq!(result.unwrap_err(), BUSY);
        let waited = pauses.iter().sum::<u64>() + opens as u64;
        assert!(waited <= BUSY_OPEN_FOR.as_millis() as u64, "{waited}");
        assert!(waited >= 1800, "{waited}");
    }

    #[test]
    fn any_other_refusal_is_tried_once_more_and_never_past_the_deadline() {
        // The crystal case (#48, #52): ≈1.9 s per open, one retry plays.
        let (result, opens, pauses) = retried(vec![Err(CRYSTAL)], 1900);
        assert!(result.is_ok());
        assert_eq!((opens, pauses), (2, vec![250]));
        let (result, opens, _) = retried(vec![Err(CRYSTAL), Err(CRYSTAL)], 1);
        assert_eq!(result.unwrap_err(), CRYSTAL);
        assert_eq!(opens, 2);
        // A refusal too late for one more slow open inside the 5 s deadline.
        let (result, opens, _) = retried(vec![Err(CRYSTAL)], 3000);
        assert_eq!(result.unwrap_err(), CRYSTAL);
        assert_eq!(opens, 1);
        // Busy at first, then the crystal: still its one retry.
        let (result, opens, _) = retried(vec![Err(BUSY), Err(CRYSTAL)], 1);
        assert!(result.is_ok());
        assert_eq!(opens, 3);
        assert!(!open_busy(CRYSTAL));
        assert!(!open_busy(
            "Bluetooth sink spec has no observed transport epoch"
        ));
    }

    #[test]
    fn a_cancelled_job_opens_no_file_and_is_not_a_file_failure() {
        let cancel = Cancel::new().unwrap();
        cancel.cancel();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
        let output = OutputSpec {
            rate: 44_100,
            format: PcmFormat::S32LE,
        };
        let error = BoundDecoder::open(
            &gapless_track("gapless-a.flac", 1),
            &root,
            output,
            DspConfig::default(),
            cancel,
        )
        .err()
        .unwrap();
        assert!(open_cancelled(&error), "{error}");
        assert!(!open_cancelled(
            "FFmpeg -1094995529: Invalid data found when processing input"
        ));
    }
    fn track() -> Track {
        Track {
            source_id: "internal".into(),
            path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac"),
            ..Default::default()
        }
    }
    fn gapless_track(name: &str, id: i64) -> Track {
        Track {
            source_id: "internal".into(),
            id,
            path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/fixtures")
                .join(name),
            ..Default::default()
        }
    }
    #[test]
    fn pinned_file_probe_preserves_fixture_metadata_and_pcm_without_extension() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
        let spec = OutputSpec {
            rate: 44_100,
            format: PcmFormat::S32LE,
        };
        let mut tested = 0;
        for name in manifest["files"].as_object().unwrap().keys() {
            if name.starts_with("malformed")
                || name.starts_with("truncated")
                || matches!(
                    std::path::Path::new(name)
                        .extension()
                        .and_then(|s| s.to_str()),
                    Some("jpg" | "png" | "webp")
                )
            {
                continue;
            }
            let track = gapless_track(name, 1);
            let mut direct = Decoder::open_with(
                &track.path,
                spec,
                DspConfig::default(),
                Cancel::new().unwrap(),
            )
            .unwrap();
            let mut pinned = BoundDecoder::open(
                &track,
                &root,
                spec,
                DspConfig::default(),
                Cancel::new().unwrap(),
            )
            .unwrap();
            let semantic = |metadata: &reborn_media::Metadata| {
                let mut value = serde_json::to_value(metadata).unwrap();
                for key in ["open_us", "probe_us", "io_bytes", "info_runs"] {
                    value.as_object_mut().unwrap().remove(key);
                }
                value
            };
            assert_eq!(
                semantic(&direct.metadata),
                semantic(&pinned.metadata),
                "{name}"
            );
            assert!(direct.metadata.io_bytes > 0 && pinned.metadata.io_bytes > 0);
            let expected = direct.read().unwrap().unwrap();
            let actual = pinned.read().unwrap().unwrap();
            assert_eq!(actual.data, expected.data, "{name}");
            assert_eq!(actual.position_ms, expected.position_ms, "{name}");
            tested += 1;
        }
        // 36 music fixtures and the three audiobook specimens (book-*).
        assert_eq!(tested, 39);
        let pinned = reborn_platform::storage::pin_track(&track(), &root).unwrap();
        assert!(external_artwork(pinned.path(), ART_SIZE as u32).is_err());
        for name in [
            "artwork-external.jpg",
            "artwork-external.png",
            "artwork-external.webp",
        ] {
            let file = pinned.open_sidecar(name).unwrap();
            assert_eq!(
                external_artwork(file.path(), ART_SIZE as u32).unwrap(),
                external_artwork(&root.join(name), ART_SIZE as u32).unwrap(),
                "{name}"
            );
        }
    }
    #[test]
    fn pinned_sidecars_keep_case_fallback_and_parent_identity_without_following_links() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("reborn-pinned-art-{}", std::process::id()));
        let moved = root.with_extension("moved");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&moved);
        std::fs::create_dir(&root).unwrap();
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
        std::fs::copy(fixtures.join("tone.flac"), root.join("song.flac")).unwrap();
        std::fs::write(root.join("CoVeR.jpg"), b"corrupt cover").unwrap();
        std::fs::copy(
            fixtures.join("artwork-external.png"),
            root.join("FoLdEr.PNG"),
        )
        .unwrap();
        symlink(
            fixtures.join("artwork-external.jpg"),
            root.join("album.jpg"),
        )
        .unwrap();
        let track = Track {
            source_id: "internal".into(),
            path: root.join("song.flac"),
            ..Default::default()
        };
        let pinned = reborn_platform::storage::pin_track(&track, &root).unwrap();
        let expected = external_artwork(&root.join("FoLdEr.PNG"), ART_SIZE as u32).unwrap();
        assert_eq!(pinned_folder_artwork(&pinned).unwrap(), expected);
        assert!(pinned.open_sidecar("album.jpg").is_err());
        std::fs::rename(&root, &moved).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::copy(
            fixtures.join("artwork-external.jpg"),
            root.join("cover.jpg"),
        )
        .unwrap();
        assert_eq!(pinned_folder_artwork(&pinned).unwrap(), expected);
        std::fs::remove_file(moved.join("FoLdEr.PNG")).unwrap();
        assert!(pinned_folder_artwork(&pinned).is_err());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(moved).unwrap();
    }
    fn spec(output: AudioOutput, rate: u32) -> SinkSpec {
        SinkSpec {
            output,
            rate,
            format: PcmFormat::S32LE,
            physical_bits: 32,
            valid_bits: 32,
            channels: 2,
            layout: "stereo".into(),
            device: "test".into(),
            codec: None,
            transport_object: None,
            transport_device: None,
            transport: None,
            mode: None,
            transport_generation: 0,
            fallback: false,
            fallback_reason: String::new(),
            speaker: false,
        }
    }
    struct VolumeTestSink {
        captured: Arc<Mutex<Vec<i32>>>,
        allowed: Arc<AtomicU64>,
        frames: Arc<AtomicU64>,
    }
    impl AudioSink for VolumeTestSink {
        fn parameters(&self) -> Parameters {
            Sink {
                frames: self.frames.clone(),
                fail: false,
                rate: 44_100,
                samples: None,
            }
            .parameters()
        }
        fn write(&mut self, data: &[u8]) -> Result<usize, String> {
            let written = self.frames.load(Ordering::Acquire);
            let room = self.allowed.load(Ordering::Acquire).saturating_sub(written) as usize;
            let count = (data.len() / 8).min(73).min(room);
            if count == 0 {
                thread::sleep(Duration::from_millis(1));
            }
            let mut captured = self.captured.lock().unwrap();
            captured.extend(data.as_chunks::<8>().0.iter().take(count).map(|frame| {
                let left = i32::from_le_bytes(frame[..4].try_into().unwrap());
                let right = i32::from_le_bytes(frame[4..].try_into().unwrap());
                assert_eq!(left, right);
                left
            }));
            self.frames.fetch_add(count as u64, Ordering::Release);
            Ok(count)
        }
        fn discard(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn delay(&self) -> u64 {
            0
        }
    }
    #[test]
    fn live_volume_changes_queued_pcm_without_reopening_or_losing_frames() {
        let root = std::env::temp_dir().join(format!("reborn-live-volume-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("constant.wav");
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36u32 + 4096 * 8).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&44_100u32.to_le_bytes());
        wav.extend_from_slice(&(44_100u32 * 8).to_le_bytes());
        wav.extend_from_slice(&8u16.to_le_bytes());
        wav.extend_from_slice(&32u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(4096u32 * 8).to_le_bytes());
        wav.extend_from_slice(&(1i32 << 28).to_le_bytes().repeat(4096 * 2));
        std::fs::write(&source, wav).unwrap();
        let frames = Arc::new(AtomicU64::new(0));
        let allowed = Arc::new(AtomicU64::new(512));
        let samples = Arc::new(Mutex::new(Vec::new()));
        let opens = Arc::new(AtomicU64::new(0));
        let (f, a, c, o) = (
            frames.clone(),
            allowed.clone(),
            samples.clone(),
            opens.clone(),
        );
        let log = Observer::new(&root).unwrap();
        let mut playback = Playback::with_sink(
            log.clone(),
            root.clone(),
            Box::new(move |_, _, _| {
                o.fetch_add(1, Ordering::Relaxed);
                Ok(Box::new(VolumeTestSink {
                    captured: c.clone(),
                    allowed: a.clone(),
                    frames: f.clone(),
                }))
            }),
        )
        .unwrap();
        playback
            .load(
                Track {
                    source_id: "internal".into(),
                    path: source,
                    ..Default::default()
                },
                vec![],
                0,
                spec(AudioOutput::Bluetooth("test-peer".into()), 44_100),
                81,
                DspConfig::with_volume(0),
                81,
            )
            .unwrap();
        let mut started = 0;
        for (limit, volume) in [(512, 100), (1024, 0), (1536, 25), (2048, 100)] {
            let deadline = Instant::now() + Duration::from_secs(3);
            while frames.load(Ordering::Acquire) < limit {
                for event in playback.events.try_iter() {
                    match event {
                        PlaybackEvent::Core(Event::TrackStarted { generation: 81 }) => started += 1,
                        PlaybackEvent::Core(Event::PlaybackError { message, .. }) => {
                            panic!("{message}")
                        }
                        _ => {}
                    }
                }
                assert!(Instant::now() < deadline, "PCM progress stopped");
                thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(playback.epoch.load(Ordering::Acquire), 81);
            playback.set_volume(volume);
            allowed.store(
                if limit == 2048 { 4096 } else { limit + 512 },
                Ordering::Release,
            );
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match playback.events.recv_timeout(Duration::from_millis(50)) {
                Ok(PlaybackEvent::Core(Event::TrackStarted { generation: 81 })) => started += 1,
                Ok(PlaybackEvent::Core(Event::TrackEnded { generation: 81 })) => break,
                Ok(PlaybackEvent::Core(Event::PlaybackError { message, .. })) => {
                    panic!("{message}")
                }
                _ => {}
            }
            assert!(Instant::now() < deadline, "stream did not finish");
        }
        assert_eq!(opens.load(Ordering::Relaxed), 1);
        assert_eq!(started, 1);
        assert_eq!(frames.load(Ordering::Acquire), 4096);
        let samples = samples.lock().unwrap();
        assert!(samples[..512].iter().all(|s| *s == 0));
        // A volume update can race one already rendered 73-frame partial
        // write. After that write plus the five-millisecond ramp, the exact
        // target must hold; this bound is independent of host scheduling.
        let settled_after = 73 + 44_100 / 200;
        assert!(samples[512 + settled_after..1024]
            .iter()
            .all(|s| *s == 1 << 28));
        assert!(samples[1024 + settled_after..1536].iter().all(|s| *s == 0));
        let quarter = (f64::from(1 << 28) * reborn_core::volume_gain(25)).round() as i32;
        assert!(samples[1536 + settled_after..2048]
            .iter()
            .all(|s| *s == quarter));
        assert!(samples[2048 + settled_after..]
            .iter()
            .all(|s| *s == 1 << 28));
        drop(samples);
        playback.shutdown(82).unwrap();
    }

    #[test]
    fn boundary_event_precedes_its_queue_entry_artwork() {
        let (tx, rx) = sync_channel(4);
        let entry_id = QueueEntryId(22);
        emit(
            &tx,
            Event::TrackBoundary {
                generation: 7,
                next_entry_id: Some(entry_id),
                next_track_id: Some(4),
                output_position_ms: 0,
            },
        );
        emit_artwork(&tx, 7, entry_id, 4, vec![1, 2, 3]);

        assert!(matches!(
            rx.recv().unwrap(),
            PlaybackEvent::Core(Event::TrackBoundary {
                next_entry_id: Some(id),
                ..
            }) if id == entry_id
        ));
        assert!(matches!(
            rx.recv().unwrap(),
            PlaybackEvent::Artwork {
                entry_id: id,
                track_id: 4,
                bytes,
                ..
            } if id == entry_id && bytes == [1, 2, 3]
        ));
    }
    fn indexed_pcm(first: usize, frames: usize, rate: u32) -> Pcm {
        let mut data = Vec::with_capacity(frames * 8);
        for frame in first..first + frames {
            let sample = frame as i32;
            data.extend_from_slice(&sample.to_le_bytes());
            data.extend_from_slice(&(-sample).to_le_bytes());
        }
        Pcm {
            data,
            position_ms: first as u64 * 1000 / u64::from(rate),
            packets: 0,
            frames: frames as u64,
            rate,
            format: PcmFormat::S32LE,
        }
    }
    fn indexed_samples(pcm: &Pcm) -> Vec<i32> {
        pcm.data
            .as_chunks::<8>()
            .0
            .iter()
            .map(|frame| i32::from_le_bytes(frame[..4].try_into().unwrap()))
            .collect()
    }

    #[test]
    fn partial_crossfade_windows_preserve_consumed_input_at_all_durations() {
        let rate = 1000;
        for seconds in [5usize, 10, 15] {
            let wanted = seconds * rate as usize;
            let near_end_a = wanted / 2;
            let near_end_b = wanted / 3;
            let cases = [
                Vec::new(),
                vec![1],
                vec![3, 7, 13],
                vec![near_end_a, near_end_b, wanted - 1 - near_end_a - near_end_b],
            ];
            for block_sizes in cases {
                let consumed: usize = block_sizes.iter().sum();
                let mut next = 0;
                let mut blocks = VecDeque::new();
                for size in block_sizes {
                    blocks.push_back(indexed_pcm(next, size, rate));
                    next += size;
                }
                let mut pending = VecDeque::new();
                let result = read_window_with(&mut pending, wanted, || {
                    blocks
                        .pop_front()
                        .map(|block| Ok(Some(block)))
                        .unwrap_or_else(|| Err("injected decoder read failure".into()))
                });
                let failure = match result {
                    Err(failure) => failure,
                    Ok(_) => panic!("decoder failure after each injected prefix"),
                };

                assert_eq!(failure.error, "injected decoder read failure");
                match (consumed, failure.partial) {
                    (0, None) => {}
                    (count, Some(partial)) => {
                        assert_eq!(partial.frames, count as u64);
                        assert_eq!(
                            indexed_samples(&partial),
                            (0..count as i32).collect::<Vec<_>>()
                        );
                        assert_eq!(partial.position_ms, 0);
                    }
                    _ => panic!("partial consumed PCM was not returned"),
                }
            }
        }
    }

    #[test]
    fn failed_window_recovery_keeps_old_suffix_before_next_prefix() {
        let mut tail = TailWindow::new(1000, PcmFormat::S32LE);
        assert!(tail.push(indexed_pcm(1000, 4, 1000), 4).is_none());
        let failure = WindowReadFailure {
            partial: Some(indexed_pcm(0, 3, 1000)),
            error: "injected read failure".into(),
        };

        let recovered = recover_window_failure(&mut tail, failure);

        assert_eq!(recovered.len(), 2);
        assert_eq!(indexed_samples(&recovered[0]), vec![1000, 1001, 1002, 1003]);
        assert_eq!(indexed_samples(&recovered[1]), vec![0, 1, 2]);
    }

    #[test]
    fn partial_window_recovery_is_chunked_under_sink_write_limit() {
        let root =
            std::env::temp_dir().join(format!("reborn-window-chunks-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let (tx, rx) = sync_channel(4);
        let buffered = AtomicU64::new(0);
        let epoch = AtomicU64::new(9);
        let frames = MAX_SINK_WRITE_BYTES / 8 + 11;
        let input = indexed_pcm(0, frames, 1000);
        let input_samples = indexed_samples(&input);
        let recovered = recover_window_failure(
            &mut TailWindow::new(1000, PcmFormat::S32LE),
            WindowReadFailure {
                partial: Some(input),
                error: "injected read failure".into(),
            },
        );
        for block in recovered {
            send_stream(
                &tx,
                &buffered,
                &epoch,
                9,
                Stream::Pcm {
                    generation: 9,
                    block,
                },
                frames as u64,
                &log,
            )
            .unwrap();
        }
        let mut output = Vec::new();
        while let Ok(Stream::Pcm { block, .. }) = rx.try_recv() {
            assert!(block.data.len() <= MAX_SINK_WRITE_BYTES);
            output.extend(indexed_samples(&block));
        }
        assert_eq!(output, input_samples);
    }
    #[test]
    fn async_open_failure_is_reported_without_waiting_or_starting_decode() {
        let root = std::env::temp_dir().join(format!(
            "reborn-open-failure-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let log = Observer::new(&root).unwrap();
        let (entered_tx, entered_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let mut playback = Playback::with_sink(
            log,
            root,
            Box::new(move |_, _, _| {
                let _ = entered_tx.try_send(());
                let _ = release_rx.recv();
                Err("injected sink open failure".into())
            }),
        )
        .unwrap();
        playback.stop_and_wait(2).unwrap();
        playback
            .begin_start_with_gapless(LoadRequest {
                track: track(),
                music_root: track().path.parent().unwrap().into(),
                entry_id: QueueEntryId(1),
                queue: vec![],
                queue_entry_ids: vec![],
                position: 0,
                spec: spec(AudioOutput::Wired, 44_100),
                generation: 2,
                dsp: DspConfig::with_volume(35),
                gapless_enabled: true,
                id: 1,
                fade_in: false,
            })
            .unwrap();
        // The opener is still held behind `release_rx`; reaching this point
        // proves begin_start returned without waiting for the worker result.
        assert!(entered_rx.recv_timeout(Duration::from_millis(100)).is_ok());
        assert!(playback.has_pending_start());
        assert!(playback.poll_start().is_none());
        release_tx.try_send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let error = loop {
            if let Some(result) = playback.poll_start() {
                break result.unwrap_err();
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        };
        assert!(error.contains("injected sink open failure"));
        assert!(playback.events.try_recv().is_err());
        playback.shutdown(3).unwrap();
    }

    #[test]
    fn stop_send_deadline_covers_a_full_command_queue() {
        let root = std::env::temp_dir().join(format!(
            "reborn-stop-full-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let log = Observer::new(&root).unwrap();
        let (sink, sink_rx) = sync_channel(1);
        sink.try_send(SinkCommand::Stop(None)).unwrap();
        let (decode, _decode_rx) = sync_channel(1);
        let (event_sender, events) = sync_channel(64);
        let mut playback = Playback {
            decode,
            sink,
            events,
            #[cfg(test)]
            event_sender,
            epoch: Arc::new(AtomicU64::new(0)),
            volume: Arc::new(AtomicU8::new(100)),
            balance: Arc::new(AtomicI8::new(0)),
            hold: Arc::new(AtomicBool::new(false)),
            upcoming: Arc::new(Mutex::new(None)),
            cancel: None,
            pending_start: None,
            sink_released: false,
            state: Arc::new(Mutex::new(json!({}))),
            observer: log,
            levels: Default::default(),
        };
        let start = Instant::now();
        let error = playback
            .stop_and_wait_with_timeout(1, Duration::from_millis(40))
            .unwrap_err();

        assert!(error.contains("deadline"));
        assert!(start.elapsed() < Duration::from_millis(250));
        drop(sink_rx);
    }

    #[test]
    fn stop_with_unavailable_worker_fails_without_blocking() {
        let root = std::env::temp_dir().join(format!("reborn-stop-gone-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let (sink, sink_rx) = sync_channel(1);
        drop(sink_rx);
        let (decode, _decode_rx) = sync_channel(1);
        let (event_sender, events) = sync_channel(64);
        let mut playback = Playback {
            decode,
            sink,
            events,
            #[cfg(test)]
            event_sender,
            epoch: Arc::new(AtomicU64::new(0)),
            volume: Arc::new(AtomicU8::new(100)),
            balance: Arc::new(AtomicI8::new(0)),
            hold: Arc::new(AtomicBool::new(false)),
            upcoming: Arc::new(Mutex::new(None)),
            cancel: None,
            pending_start: None,
            sink_released: false,
            state: Arc::new(Mutex::new(json!({}))),
            observer: log,
            levels: Default::default(),
        };
        let start = Instant::now();

        assert_eq!(
            playback
                .stop_and_wait_with_timeout(1, Duration::from_secs(1))
                .unwrap_err(),
            "audio worker unavailable"
        );
        assert!(start.elapsed() < Duration::from_millis(250));
    }

    struct SignalingFailureSink(Arc<AtomicBool>);
    impl AudioSink for SignalingFailureSink {
        fn parameters(&self) -> Parameters {
            Parameters {
                rate: 44_100,
                period: 512,
                buffer: 4096,
                format: PcmFormat::S32LE,
                channels: 2,
                hardware_mixer_gain_db: None,
                device: "test".into(),
                fallback: false,
                fallback_reason: String::new(),
                hp_level_live: None,
                level_raised: false,
            }
        }
        fn write(&mut self, _pcm: &[u8]) -> Result<usize, String> {
            self.0.store(true, Ordering::Release);
            Err("injected sink write failure".into())
        }
        fn discard(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn delay(&self) -> u64 {
            0
        }
    }

    #[test]
    fn full_event_queue_cannot_extend_the_release_deadline() {
        let root = std::env::temp_dir().join(format!("reborn-events-full-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let attempted = Arc::new(AtomicBool::new(false));
        let sink_attempted = attempted.clone();
        let mut playback = Playback::with_sink(
            log,
            root,
            Box::new(move |_, _, _| Ok(Box::new(SignalingFailureSink(sink_attempted.clone())))),
        )
        .unwrap();
        for _ in 0..64 {
            playback
                .event_sender
                .try_send(PlaybackEvent::Core(Event::Position {
                    generation: 0,
                    ms: 0,
                }))
                .unwrap();
        }
        playback
            .load(
                track(),
                vec![],
                0,
                spec(AudioOutput::Wired, 44_100),
                1,
                DspConfig::with_volume(35),
                1,
            )
            .unwrap();
        let wait_until = Instant::now() + Duration::from_secs(3);
        while !attempted.load(Ordering::Acquire) {
            assert!(Instant::now() < wait_until, "sink write did not start");
            thread::sleep(Duration::from_millis(2));
        }

        let start = Instant::now();
        let error = playback
            .stop_and_wait_with_timeout(2, Duration::from_millis(40))
            .unwrap_err();
        assert!(error.contains("deadline"));
        assert!(start.elapsed() < Duration::from_millis(250));

        while playback.events.try_recv().is_ok() {}
        playback
            .stop_and_wait_with_timeout(3, Duration::from_secs(1))
            .unwrap();
        playback.shutdown(4).unwrap();
    }

    /// Pause holds the open pipeline: the device is paused, nothing is
    /// written, and resume continues the same stream without a reopen.
    #[test]
    fn a_held_pause_stops_writing_and_resumes_without_reopening() {
        struct HoldSink {
            frames: Arc<AtomicU64>,
            pauses: Arc<Mutex<Vec<bool>>>,
        }
        impl AudioSink for HoldSink {
            fn parameters(&self) -> Parameters {
                Parameters {
                    rate: 44100,
                    period: 512,
                    buffer: 4096,
                    format: PcmFormat::S32LE,
                    channels: 2,
                    hardware_mixer_gain_db: None,
                    device: "test".into(),
                    fallback: false,
                    fallback_reason: String::new(),
                    hp_level_live: None,
                    level_raised: false,
                }
            }
            fn write(&mut self, s: &[u8]) -> Result<usize, String> {
                // Real time: 128 frames take ~2.9 ms at 44.1 kHz.
                thread::sleep(Duration::from_millis(3));
                let n = (s.len() / 8).min(128);
                self.frames.fetch_add(n as u64, Ordering::Relaxed);
                Ok(n)
            }
            fn discard(&mut self) -> Result<(), String> {
                Ok(())
            }
            fn delay(&self) -> u64 {
                0
            }
            fn pause(&mut self, on: bool) -> Result<(), String> {
                self.pauses.lock().unwrap().push(on);
                Ok(())
            }
        }
        let frames = Arc::new(AtomicU64::new(0));
        let pauses = Arc::new(Mutex::new(vec![]));
        let opened = Arc::new(AtomicU64::new(0));
        let (f, ps, o) = (frames.clone(), pauses.clone(), opened.clone());
        let root = std::env::temp_dir().join(format!("reborn-hold-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let mut p = Playback::with_sink(
            log,
            root,
            Box::new(move |_, _, _| {
                o.fetch_add(1, Ordering::Relaxed);
                Ok(Box::new(HoldSink {
                    frames: f.clone(),
                    pauses: ps.clone(),
                }))
            }),
        )
        .unwrap();
        p.load(
            track(),
            vec![],
            0,
            spec(AudioOutput::Wired, 44100),
            1,
            DspConfig::with_volume(80),
            1,
        )
        .unwrap();
        let wait_for = |what: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !what() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            what()
        };
        assert!(
            wait_for(&|| frames.load(Ordering::Relaxed) > 2_000),
            "playback started"
        );
        p.hold(true);
        assert!(p.is_held());
        assert!(
            wait_for(&|| pauses.lock().unwrap().as_slice() == [true]),
            "device paused"
        );
        let at_pause = frames.load(Ordering::Relaxed);
        thread::sleep(Duration::from_millis(300));
        assert_eq!(
            frames.load(Ordering::Relaxed),
            at_pause,
            "nothing written while held"
        );
        p.hold(false);
        assert!(
            wait_for(&|| frames.load(Ordering::Relaxed) > at_pause + 2_000),
            "continues"
        );
        assert_eq!(pauses.lock().unwrap().as_slice(), [true, false]);
        assert_eq!(
            opened.load(Ordering::Relaxed),
            1,
            "the device was never reopened"
        );
        // Any stop ends the hold, so a later Play cannot resume a stopped pipeline.
        p.hold(true);
        p.stop(2);
        assert!(!p.is_held());
    }

    #[test]
    fn stop_invalidates_pending_pcm_and_output_switch_reopens() {
        let frames = Arc::new(AtomicU64::new(0));
        let count = frames.clone();
        let opened = Arc::new(AtomicU64::new(0));
        let calls = opened.clone();
        let root = std::env::temp_dir().join(format!("reborn-pipeline-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let mut p = Playback::with_sink(
            log,
            root,
            Box::new(move |requested, _, _| {
                let previous = calls.fetch_add(1, Ordering::Relaxed);
                let rate = requested.rate;
                assert_eq!(rate, if previous == 0 { 44100 } else { 48000 });
                Ok(Box::new(Sink {
                    frames: count.clone(),
                    fail: false,
                    rate,
                    samples: None,
                }))
            }),
        )
        .unwrap();
        p.load(
            track(),
            vec![],
            0,
            spec(AudioOutput::Wired, 44100),
            1,
            DspConfig::with_volume(35),
            1,
        )
        .unwrap();
        let second_open_without_release = p
            .start_with_gapless(LoadRequest {
                track: track(),
                music_root: track().path.parent().unwrap().into(),
                entry_id: QueueEntryId(2),
                queue: vec![],
                queue_entry_ids: vec![],
                position: 0,
                spec: spec(AudioOutput::Bluetooth("12:34:56:78:90:AB".into()), 48_000),
                generation: 2,
                dsp: DspConfig::with_volume(35),
                gapless_enabled: true,
                id: 2,
                fade_in: false,
            })
            .unwrap_err();
        assert!(second_open_without_release.contains("release acknowledgement"));
        assert_eq!(opened.load(Ordering::Relaxed), 1);
        thread::sleep(Duration::from_millis(60));
        assert!(frames.load(Ordering::Relaxed) > 0);
        p.stop(2);
        thread::sleep(Duration::from_millis(40));
        let before = frames.load(Ordering::Relaxed);
        thread::sleep(Duration::from_millis(40));
        assert_eq!(before, frames.load(Ordering::Relaxed));
        while p.events.try_recv().is_ok() {}
        p.load(
            track(),
            vec![],
            500,
            spec(AudioOutput::Bluetooth("12:34:56:78:90:AB".into()), 48000),
            3,
            DspConfig::with_volume(35),
            2,
        )
        .unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(PlaybackEvent::Core(Event::TrackStarted { generation })) =
                p.events.recv_timeout(Duration::from_millis(100))
            {
                assert_eq!(generation, 3);
                break;
            }
            assert!(Instant::now() < until)
        }
        assert_eq!(opened.load(Ordering::Relaxed), 2);
        p.stop(4);
    }
    #[test]
    fn a_kept_sink_plays_on_only_for_exactly_its_spec_and_wired_settings() {
        let wired = spec(AudioOutput::Wired, 44_100);
        assert_eq!(sink_change(&wired, 3, &wired.clone(), 3), None);
        assert_eq!(
            sink_change(&wired, 3, &spec(AudioOutput::Wired, 48_000), 3),
            Some("rate")
        );
        let mut s24 = wired.clone();
        s24.format = PcmFormat::S24LE;
        assert_eq!(sink_change(&wired, 3, &s24, 3), Some("format"));
        let bluetooth = spec(AudioOutput::Bluetooth("12:34:56:78:90:AB".into()), 44_100);
        assert_eq!(sink_change(&wired, 3, &bluetooth, 3), Some("output"));
        // A Bluetooth sink is never kept, even for the same transport.
        assert_eq!(
            sink_change(&bluetooth, 3, &bluetooth.clone(), 3),
            Some("output")
        );
        let mut other_device = wired.clone();
        other_device.device = "other".into();
        assert_eq!(sink_change(&wired, 3, &other_device, 3), Some("spec"));
        // A new headphone level or DAC filter needs the amplifier powered up.
        assert_eq!(
            sink_change(&wired, 3, &wired.clone(), 4),
            Some("wired_settings")
        );
    }
    /// A track change keeps the wired sink open when the next track wants
    /// the same one (#52): its queued audio is dropped and nothing reopens.
    /// Another rate or output opens a new sink after the kept one closed; a
    /// kept sink no stream asks for closes on its own.
    #[test]
    fn consecutive_tracks_with_one_wired_spec_open_the_sink_once() {
        struct KeptTestSink {
            open: Arc<AtomicU64>,
            discards: Arc<AtomicU64>,
            rate: u32,
        }
        impl AudioSink for KeptTestSink {
            fn parameters(&self) -> Parameters {
                Parameters {
                    rate: self.rate,
                    period: 512,
                    buffer: 4096,
                    format: PcmFormat::S32LE,
                    channels: 2,
                    device: "test".into(),
                    ..Default::default()
                }
            }
            fn write(&mut self, s: &[u8]) -> Result<usize, String> {
                thread::sleep(Duration::from_millis(2));
                Ok((s.len() / 8).min(128))
            }
            fn discard(&mut self) -> Result<(), String> {
                self.discards.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            fn delay(&self) -> u64 {
                0
            }
        }
        impl Drop for KeptTestSink {
            fn drop(&mut self) {
                self.open.fetch_sub(1, Ordering::AcqRel);
            }
        }
        let opens = Arc::new(AtomicU64::new(0));
        let open = Arc::new(AtomicU64::new(0));
        let peak = Arc::new(AtomicU64::new(0));
        let discards = Arc::new(AtomicU64::new(0));
        let (factory_opens, factory_open, factory_peak, factory_discards) =
            (opens.clone(), open.clone(), peak.clone(), discards.clone());
        let root = std::env::temp_dir().join(format!("reborn-kept-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let mut p = Playback::with_sink(
            log,
            root,
            Box::new(move |requested, _, _| {
                factory_opens.fetch_add(1, Ordering::Relaxed);
                let now = factory_open.fetch_add(1, Ordering::AcqRel) + 1;
                factory_peak.fetch_max(now, Ordering::AcqRel);
                Ok(Box::new(KeptTestSink {
                    open: factory_open.clone(),
                    discards: factory_discards.clone(),
                    rate: requested.rate,
                }))
            }),
        )
        .unwrap();
        let start = |p: &mut Playback, generation: u64, spec: SinkSpec| {
            let kept = p.switch_and_wait(generation).unwrap();
            p.start_with_gapless(LoadRequest {
                track: track(),
                music_root: track().path.parent().unwrap().into(),
                entry_id: QueueEntryId(generation),
                queue: vec![],
                queue_entry_ids: vec![],
                position: 0,
                spec,
                generation,
                dsp: DspConfig::with_volume(35),
                gapless_enabled: true,
                id: generation,
                fade_in: false,
            })
            .unwrap();
            let until = Instant::now() + Duration::from_secs(2);
            loop {
                if let Ok(PlaybackEvent::Core(Event::TrackStarted { generation: g })) =
                    p.events.recv_timeout(Duration::from_millis(100))
                {
                    if g == generation {
                        break;
                    }
                }
                assert!(Instant::now() < until, "track {generation} never started");
            }
            kept
        };
        let wired = spec(AudioOutput::Wired, 44_100);
        assert_eq!(start(&mut p, 1, wired.clone()), None);
        assert_eq!(opens.load(Ordering::Relaxed), 1);
        // Next: the same sink, its old audio dropped.
        assert_eq!(start(&mut p, 2, wired.clone()), Some(wired.clone()));
        assert_eq!(opens.load(Ordering::Relaxed), 1);
        assert!(discards.load(Ordering::Relaxed) >= 1);
        assert_eq!(open.load(Ordering::Acquire), 1);
        // Another rate: the kept sink closes before the new one opens.
        assert_eq!(
            start(&mut p, 3, spec(AudioOutput::Wired, 48_000)),
            Some(wired)
        );
        assert_eq!(opens.load(Ordering::Relaxed), 2);
        assert_eq!(peak.load(Ordering::Acquire), 1);
        // Kept for a stream that never comes: it closes on its own.
        assert!(p.switch_and_wait(4).unwrap().is_some());
        assert_eq!(open.load(Ordering::Acquire), 1);
        let until = Instant::now() + KEEP_SINK_FOR + Duration::from_secs(1);
        while open.load(Ordering::Acquire) != 0 {
            assert!(Instant::now() < until, "the kept sink stayed open");
            thread::sleep(Duration::from_millis(20));
        }
        // Another output opens its own sink, which is not kept.
        let bluetooth = spec(AudioOutput::Bluetooth("12:34:56:78:90:AB".into()), 48_000);
        assert_eq!(start(&mut p, 5, bluetooth), None);
        assert_eq!(opens.load(Ordering::Relaxed), 3);
        assert_eq!(p.switch_and_wait(6).unwrap(), None);
        assert_eq!(open.load(Ordering::Acquire), 0);
        // A stop closes a kept wired sink at once.
        assert_eq!(start(&mut p, 7, spec(AudioOutput::Wired, 48_000)), None);
        p.stop_and_wait(8).unwrap();
        assert_eq!(open.load(Ordering::Acquire), 0);
        assert_eq!(opens.load(Ordering::Relaxed), 4);
        p.shutdown(9).unwrap();
    }
    #[test]
    fn decoder_and_sink_agree_on_both_baseline_rates() {
        for rate in [44100, 48000] {
            let frames = Arc::new(AtomicU64::new(0));
            let count = frames.clone();
            let root =
                std::env::temp_dir().join(format!("reborn-rate-{}-{rate}", std::process::id()));
            let log = Observer::new(&root).unwrap();
            let mut p = Playback::with_sink(
                log,
                root,
                Box::new(move |requested, _, _| {
                    assert_eq!(requested.rate, rate);
                    Ok(Box::new(Sink {
                        frames: count.clone(),
                        fail: false,
                        rate,
                        samples: None,
                    }))
                }),
            )
            .unwrap();
            p.load(
                track(),
                vec![],
                0,
                spec(AudioOutput::Wired, rate),
                1,
                DspConfig::with_volume(35),
                1,
            )
            .unwrap();
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(PlaybackEvent::Core(event)) =
                    p.events.recv_timeout(Duration::from_millis(100))
                {
                    match event {
                        Event::TrackEnded { generation: 1 } => break,
                        Event::PlaybackError { message, .. } => panic!("{message}"),
                        _ => {}
                    }
                }
                assert!(Instant::now() < until, "PCM drain deadline");
            }
            assert_eq!(frames.load(Ordering::Relaxed), u64::from(rate));
            p.shutdown(2).unwrap();
        }
    }
    #[test]
    fn sink_failure_reaches_authoritative_model() {
        let root = std::env::temp_dir().join(format!("reborn-sink-fault-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let mut p = Playback::with_sink(
            log,
            root,
            Box::new(|requested, _, _| {
                Ok(Box::new(Sink {
                    frames: Arc::new(AtomicU64::new(0)),
                    fail: true,
                    rate: requested.rate,
                    samples: None,
                }))
            }),
        )
        .unwrap();
        p.load(
            track(),
            vec![],
            0,
            spec(AudioOutput::Wired, 44100),
            9,
            DspConfig::with_volume(35),
            1,
        )
        .unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(PlaybackEvent::Core(Event::PlaybackError {
                generation,
                message,
            })) = p.events.recv_timeout(Duration::from_millis(100))
            {
                assert_eq!(generation, 9);
                assert!(message.contains("disconnect"));
                break;
            }
            assert!(Instant::now() < until)
        }
        p.stop(10);
    }
    #[test]
    fn gapless_queue_is_contiguous_when_crossfade_is_off() {
        let frames = Arc::new(AtomicU64::new(0));
        let count = frames.clone();
        let samples = Arc::new(Mutex::new(Vec::new()));
        let captured = samples.clone();
        let root = std::env::temp_dir().join(format!("reborn-gapless-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let mut p = Playback::with_sink(
            log,
            root,
            Box::new(move |requested, _, _| {
                Ok(Box::new(Sink {
                    frames: count.clone(),
                    fail: false,
                    rate: requested.rate,
                    samples: Some(captured.clone()),
                }))
            }),
        )
        .unwrap();
        p.load(
            gapless_track("gapless-a.flac", 1),
            vec![gapless_track("gapless-b.flac", 2)],
            0,
            spec(AudioOutput::Wired, 44100),
            21,
            DspConfig::with_volume(100),
            21,
        )
        .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let mut boundary = false;
        loop {
            if let Ok(PlaybackEvent::Core(event)) =
                p.events.recv_timeout(Duration::from_millis(100))
            {
                match event {
                    Event::TrackBoundary { generation: 21, .. } => boundary = true,
                    Event::TrackEnded { generation: 21 } => break,
                    Event::PlaybackError { message, .. } => panic!("{message}"),
                    _ => {}
                }
            }
            assert!(Instant::now() < until, "gapless queue drain deadline");
        }
        assert!(boundary);
        assert_eq!(frames.load(Ordering::Relaxed), 8192);
        let samples = samples.lock().unwrap();
        assert_eq!(samples.len(), 8192);
        let previous_step = samples[4095] - samples[4094];
        let boundary_step = samples[4096] - samples[4095];
        assert!(previous_step > 0);
        assert!((boundary_step - previous_step).abs() <= 2);
        p.shutdown(22).unwrap();
    }
    /// A queue edit while a song plays (Beta 8.33): the song after it
    /// changes without the song starting over.
    #[test]
    fn a_new_upcoming_list_changes_the_next_song_without_a_restart() {
        let frames = Arc::new(AtomicU64::new(0));
        let count = frames.clone();
        let root = std::env::temp_dir().join(format!("reborn-upcoming-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let mut p = Playback::with_sink(
            log,
            root,
            Box::new(move |requested, _, _| {
                Ok(Box::new(Sink {
                    frames: count.clone(),
                    fail: false,
                    rate: requested.rate,
                    samples: None,
                }))
            }),
        )
        .unwrap();
        // Waiting for job 31 before it starts: taken at its first block.
        p.set_upcoming(
            31,
            vec![gapless_track("gapless-a.flac", 3)],
            vec![QueueEntryId(7)],
        );
        p.load(
            gapless_track("gapless-a.flac", 1),
            vec![gapless_track("gapless-b.flac", 2)],
            0,
            spec(AudioOutput::Wired, 44100),
            31,
            DspConfig::with_volume(100),
            31,
        )
        .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let mut boundaries = vec![];
        loop {
            if let Ok(PlaybackEvent::Core(event)) =
                p.events.recv_timeout(Duration::from_millis(100))
            {
                match event {
                    Event::TrackBoundary {
                        generation: 31,
                        next_entry_id,
                        next_track_id,
                        ..
                    } => boundaries.push((next_entry_id, next_track_id)),
                    Event::TrackEnded { generation: 31 } => break,
                    Event::PlaybackError { message, .. } => panic!("{message}"),
                    _ => {}
                }
            }
            assert!(Instant::now() < until, "upcoming drain deadline");
        }
        assert_eq!(
            boundaries,
            [(Some(QueueEntryId(7)), Some(3))],
            "the new next song, not the one the job started with"
        );
        assert_eq!(
            frames.load(Ordering::Relaxed),
            8192,
            "both songs whole, once"
        );
        // Another job's list is never taken.
        p.set_upcoming(99, vec![], vec![]);
        p.shutdown(32).unwrap();
    }
    #[test]
    fn failed_next_track_is_skipped_to_the_following_decodable_track() {
        let frames = Arc::new(AtomicU64::new(0));
        let count = frames.clone();
        let root = std::env::temp_dir().join(format!("reborn-skip-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let mut p = Playback::with_sink(
            log,
            root,
            Box::new(move |requested, _, _| {
                Ok(Box::new(Sink {
                    frames: count.clone(),
                    fail: false,
                    rate: requested.rate,
                    samples: None,
                }))
            }),
        )
        .unwrap();
        let mut missing = gapless_track("missing.flac", 2);
        missing.path = PathBuf::from("/missing/reborn-next-track.flac");
        p.load(
            gapless_track("gapless-a.flac", 1),
            vec![missing, gapless_track("gapless-b.flac", 3)],
            0,
            spec(AudioOutput::Wired, 44100),
            41,
            DspConfig::with_volume(100),
            41,
        )
        .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let mut skipped_to = None;
        loop {
            if let Ok(PlaybackEvent::Core(event)) =
                p.events.recv_timeout(Duration::from_millis(100))
            {
                match event {
                    Event::TrackBoundary {
                        generation: 41,
                        next_track_id,
                        ..
                    } => skipped_to = next_track_id,
                    Event::TrackEnded { generation: 41 } => break,
                    Event::PlaybackError { message, .. } => panic!("{message}"),
                    _ => {}
                }
            }
            assert!(Instant::now() < until, "skip queue drain deadline");
        }
        assert_eq!(skipped_to, Some(3));
        assert_eq!(frames.load(Ordering::Relaxed), 8192);
        p.shutdown(42).unwrap();
    }
    #[test]
    fn crossfade_is_separate_from_gapless_and_reduces_the_overlap_window() {
        let frames = Arc::new(AtomicU64::new(0));
        let count = frames.clone();
        let root = std::env::temp_dir().join(format!("reborn-crossfade-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let mut p = Playback::with_sink(
            log,
            root,
            Box::new(move |requested, _, _| {
                Ok(Box::new(Sink {
                    frames: count.clone(),
                    fail: false,
                    rate: requested.rate,
                    samples: None,
                }))
            }),
        )
        .unwrap();
        p.load(
            gapless_track("gapless-a.flac", 1),
            vec![gapless_track("gapless-b.flac", 2)],
            0,
            spec(AudioOutput::Wired, 44100),
            31,
            DspConfig {
                volume: 100,
                crossfade_ms: 50,
                ..Default::default()
            },
            31,
        )
        .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let mut boundary = false;
        loop {
            if let Ok(PlaybackEvent::Core(event)) =
                p.events.recv_timeout(Duration::from_millis(100))
            {
                match event {
                    Event::TrackBoundary { generation: 31, .. } => boundary = true,
                    Event::TrackEnded { generation: 31 } => break,
                    Event::PlaybackError { message, .. } => panic!("{message}"),
                    _ => {}
                }
            }
            assert!(Instant::now() < until, "crossfade queue drain deadline");
        }
        assert!(boundary);
        assert_eq!(frames.load(Ordering::Relaxed), 5987);
        p.shutdown(32).unwrap();
    }

    /// Live end-to-end check (needs the internet): a real station through
    /// the fetcher and the stream decoder, as playback opens it.
    #[test]
    #[ignore]
    fn live_radio_station_decodes() {
        let directory = reborn_platform::radio::Directory::discover();
        let popular = directory.popular().expect("directory reachable");
        for codec in ["MP3", "AAC"] {
            let station = popular
                .iter()
                .find(|s| s.codec == codec)
                .expect("station")
                .clone();
            let track = reborn_core::Track::radio(&station);
            let spec = OutputSpec {
                rate: 48_000,
                format: PcmFormat::S16LE,
            };
            let mut decoder = BoundDecoder::open(
                &track,
                std::path::Path::new("/"),
                spec,
                DspConfig::with_volume(100),
                Cancel::new().unwrap(),
            )
            .unwrap();
            let (mut frames, mut energy, mut peak) = (0u64, 0f64, 0i32);
            while frames < 48_000 * 3 {
                let pcm = decoder.read().unwrap().expect("live stream does not end");
                frames += pcm.frames;
                for sample in pcm.data.as_chunks::<2>().0 {
                    let v = i16::from_le_bytes(*sample) as i32;
                    energy += (v * v) as f64;
                    peak = peak.max(v.abs());
                }
            }
            let rms = (energy / (frames * 2) as f64).sqrt();
            eprintln!(
                "{codec}: {} decoded 3 s, rms {rms:.0}, peak {peak}",
                station.name
            );
            assert!(rms > 100.0, "decoded audio is silent");
        }
    }
}
