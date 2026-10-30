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
mod media_state;
mod sink;
/// Fade-in of the first stream after the headphone level went up.
mod stream;
use media_state::*;
use sink::*;
use stream::*;
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

/* Keep every queued PCM block within the direct ALSA writer's byte limit.
 * This matters for the configured 5/10/15 second crossfade windows, which
 * are intentionally produced by FFmpeg as one transition result. */

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
                let heartbeat_s = if job.track.is_radio() { 60 } else { 20 };
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
                    if job.position > 0 && !tracks[0].is_radio() {
                        decoder.seek(job.position)?;
                    }
                    let artwork = if tracks[0].is_radio() {
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
mod tests;
