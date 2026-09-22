use reborn_audio::{AlsaSink, AudioSink, SinkSpec};
use reborn_core::{Event, PcmFormat, QueueEntryId, Track};
use reborn_media::{
    convert_pcm, crossfade_pcm, external_artwork, Cancel, Decoder, DspConfig, OutputSpec, Pcm,
};
use reborn_observability::{HealthState, Level, Observer};
use serde_json::json;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
pub enum PlaybackEvent {
    Core(Event),
    Artwork {
        generation: u64,
        track_id: i64,
        bytes: Vec<u8>,
    },
}
pub struct LoadRequest {
    pub track: Track,
    pub entry_id: QueueEntryId,
    pub queue: Vec<Track>,
    pub queue_entry_ids: Vec<QueueEntryId>,
    pub position: u64,
    pub spec: SinkSpec,
    pub generation: u64,
    pub dsp: DspConfig,
    pub gapless_enabled: bool,
    pub id: u64,
}
struct Job {
    track: Track,
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
enum DecodeCommand {
    Load(Box<Job>),
    Stop,
}
enum SinkCommand {
    Shutdown(SyncSender<()>),
    Open {
        generation: u64,
        spec: SinkSpec,
        id: u64,
    },
    Stop(Option<SyncSender<()>>),
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
    End {
        generation: u64,
    },
}
pub struct Playback {
    decode: SyncSender<DecodeCommand>,
    sink: SyncSender<SinkCommand>,
    pub events: Receiver<PlaybackEvent>,
    epoch: Arc<AtomicU64>,
    cancel: Option<Cancel>,
    state: Arc<Mutex<serde_json::Value>>,
    observer: Observer,
}
fn emit(tx: &SyncSender<PlaybackEvent>, e: Event) {
    let _ = tx.send(PlaybackEvent::Core(e));
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
    if let Some((block, offset)) = pending.pop_front() {
        let frame_bytes = block.format.bytes_per_frame();
        let start = offset.saturating_mul(frame_bytes);
        if start >= block.data.len() {
            return read_pending_or_decoder(decoder, pending);
        }
        let data = block.data[start..].to_vec();
        return Ok(Some(Pcm {
            position_ms: block.position_ms + offset as u64 * 1000 / u64::from(block.rate),
            packets: block.packets,
            frames: data.len() as u64 / frame_bytes as u64,
            rate: block.rate,
            format: block.format,
            data,
        }));
    }
    decoder.read()
}

fn read_window(
    decoder: &mut Decoder,
    pending: &mut VecDeque<(Pcm, usize)>,
    wanted_frames: usize,
) -> Result<Option<Pcm>, String> {
    let mut data = Vec::new();
    let mut position_ms = 0;
    let mut rate = 0;
    let mut format = PcmFormat::S32LE;
    let mut frames = 0usize;
    while frames < wanted_frames {
        let Some(block) = read_pending_or_decoder(decoder, pending)? else {
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
    Ok(Some(Pcm {
        data,
        position_ms,
        packets: 0,
        frames: frames as u64,
        rate,
        format,
    }))
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

fn publish_artwork(
    cache: &std::path::Path,
    track: &Track,
    decoder: &mut Decoder,
    generation: u64,
    events: &SyncSender<PlaybackEvent>,
) {
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
    let path = cache.join(format!("{}-{}-{}.rgba", track.id, track.size, track.mtime));
    let cached = std::fs::read(&path)
        .ok()
        .filter(|b| b.len() == 160 * 160 * 4);
    let bytes = cached
        .clone()
        .or_else(|| {
            decoder
                .metadata
                .artwork
                .then(|| decoder.artwork())
                .and_then(Result::ok)
        })
        .or_else(|| {
            let parent = track.path.parent()?;
            [
                "cover.jpg",
                "cover.jpeg",
                "cover.png",
                "cover.webp",
                "folder.jpg",
                "folder.jpeg",
                "folder.png",
                "folder.webp",
                "album.jpg",
                "album.jpeg",
                "album.png",
                "album.webp",
            ]
            .iter()
            .map(|name| parent.join(name))
            .find_map(|candidate| external_artwork(&candidate).ok())
        });
    if let Some(bytes) = bytes {
        if cached.is_none() {
            let _ = reborn_core::atomic_write(&path, &bytes);
        }
        let _ = events.try_send(PlaybackEvent::Artwork {
            generation,
            track_id: track.id,
            bytes,
        });
    }
}
type SinkFactory =
    Box<dyn Fn(&SinkSpec, Observer, u64) -> Result<Box<dyn AudioSink>, String> + Send>;
impl Playback {
    pub fn spawn(log: Observer, cache: PathBuf) -> Result<Self, String> {
        Self::with_sink(
            log,
            cache,
            Box::new(|spec, log, id| {
                AlsaSink::open_spec(spec, log, id).map(|s| Box::new(s) as Box<dyn AudioSink>)
            }),
        )
    }
    fn with_sink(log: Observer, cache: PathBuf, factory: SinkFactory) -> Result<Self, String> {
        let (dt, dr) = sync_channel::<DecodeCommand>(4);
        let (st, sr) = sync_channel::<SinkCommand>(8);
        let (pt, pr) = sync_channel::<Stream>(8);
        let (et, er) = sync_channel(64);
        let epoch = Arc::new(AtomicU64::new(0));
        let buffered = Arc::new(AtomicU64::new(0));
        let state = Arc::new(Mutex::new(json!({
            "state":"stopped",
            "canonical_sample_format":"fltp",
            "output_format":"unknown",
            "gapless":{"enabled":true,"preopened":false},
            "crossfade":{"enabled":false,"duration_ms":0}
        })));
        let de = epoch.clone();
        let se = epoch.clone();
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
                let operation: Result<(), String> = (|| {
                    let mut tracks = Vec::with_capacity(1 + job.queue.len());
                    tracks.push(job.track.clone());
                    tracks.extend(job.queue.clone());
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
                    let mut decoder = Decoder::open_with(
                        &tracks[0].path,
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
                    if job.position > 0 {
                        decoder.seek(job.position)?;
                    }
                    publish_artwork(&cache, &tracks[0], &mut decoder, job.generation, &det);
                    let overlap_frames = (u64::from(job.spec.rate)
                        * u64::from(job.dsp.crossfade_ms)
                        / 1000) as usize;
                    let mut tail = TailWindow::new(job.spec.rate, processing_format);
                    let mut decoder_pending = VecDeque::new();
                    let mut next_decoder = None;
                    let mut next_pending = VecDeque::new();
                    let mut packet_count = 0u64;
                    let open_next = |start: usize| -> Option<(usize, Decoder)> {
                        for (candidate, track) in tracks.iter().enumerate().skip(start) {
                            match Decoder::open_with(
                                &track.path,
                                output,
                                job.dsp.clone(),
                                job.cancel.clone(),
                            ) {
                                Ok(next) => {
                                    return Some((candidate, next));
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
                        if let Some((candidate, next)) = open_next(1) {
                            preopened_index = Some(candidate);
                            next_decoder = Some(next);
                        }
                    }
                    loop {
                        dl.heartbeat("playback", 20);
                        if de.load(Ordering::Acquire) != job.generation {
                            return Ok(());
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
                                        &e,
                                        Some(job.id),
                                        json!({"track_id":tracks[next_index].id,"recovery":"contiguous_boundary"}),
                                    );
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
                        publish_artwork(
                            &cache,
                            &tracks[next_index],
                            &mut next,
                            job.generation,
                            &det,
                        );
                        if let Some((candidate, following)) = open_next(next_index + 1) {
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
                let mut pending: Option<(Pcm, usize)> = None;
                let mut started = false;
                let mut eof = false;
                let mut drain_start = Instant::now();
                let mut last_progress = Instant::now();
                loop {
                    al.heartbeat("audio", 10);
                    while let Ok(cmd) = sr.try_recv() {
                        match cmd {
                            SinkCommand::Shutdown(reply) => {
                                drop(sink.take());
                                let _ = reply.try_send(());
                                return;
                            }
                            SinkCommand::Open {
                                generation: g,
                                spec,
                                id,
                            } => {
                                sink = None;
                                pending = None;
                                generation = g;
                                correlation = id;
                                rate = spec.rate;
                                sink_format = spec.format;
                                started = false;
                                eof = false;
                                match factory(&spec, al.clone(), id) {
                                    Ok(s) => {
                                        let params = s.parameters();
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
                                        if params.fallback {
                                            al.add("audio_sink_fallbacks", 1.);
                                        }
                                        sink = Some(s);
                                        al.health_set("audio", HealthState::Ok, true, "sink ready");
                                    }
                                    Err(e) => {
                                        al.health_set("audio", HealthState::Failed, true, &e);
                                        emit(
                                            &et,
                                            Event::PlaybackError {
                                                generation,
                                                message: e,
                                            },
                                        );
                                    }
                                }
                                last_progress = Instant::now();
                            }
                            SinkCommand::Stop(reply) => {
                                sink = None;
                                pending = None;
                                eof = false;
                                if let Some(reply) = reply {
                                    let _ = reply.try_send(());
                                }
                            }
                        }
                    }
                    if generation != se.load(Ordering::Acquire) {
                        sink = None;
                        pending = None;
                        eof = false;
                    }
                    if pending.is_none() {
                        match pr.recv_timeout(Duration::from_millis(10)) {
                            Ok(Stream::Pcm {
                                generation: g,
                                mut block,
                            }) => {
                                ab.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
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
                        let max_bytes = (MAX_SINK_WRITE_BYTES / frame_bytes) * frame_bytes;
                        let end = (*offset + max_bytes).min(block.data.len());
                        match s.write(&block.data[*offset..end]) {
                            Ok(n) => {
                                if n > 0 {
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
                                    let ms = block.position_ms
                                        + (*offset as u64 / frame_bytes).saturating_sub(s.delay())
                                            * 1000
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
                        sink = None;
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
            epoch,
            cancel: None,
            state,
            observer: state_observer,
        })
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
        })
    }
    pub fn load_with_gapless(&mut self, request: LoadRequest) -> Result<(), String> {
        let LoadRequest {
            track,
            entry_id,
            queue,
            queue_entry_ids,
            position,
            spec,
            generation,
            dsp,
            gapless_enabled,
            id,
        } = request;
        if !(8000..=384_000).contains(&spec.rate) {
            return Err("unsupported PCM rate".into());
        }
        self.stop(generation);
        let cancel = Cancel::new()?;
        self.cancel = Some(cancel.clone());
        self.sink
            .try_send(SinkCommand::Open {
                generation,
                spec: spec.clone(),
                id,
            })
            .map_err(|_| "audio command queue full")?;
        self.decode
            .try_send(DecodeCommand::Load(Box::new(Job {
                track,
                entry_id,
                queue,
                queue_entry_ids,
                spec,
                dsp,
                gapless_enabled,
                position,
                generation,
                id,
                cancel,
            })))
            .map_err(|_| "playback command queue full".into())
    }
    pub fn stop(&mut self, generation: u64) {
        self.epoch.store(generation, Ordering::Release);
        if let Some(c) = self.cancel.take() {
            c.cancel()
        }
        let _ = self.sink.try_send(SinkCommand::Stop(None));
        let _ = self.decode.try_send(DecodeCommand::Stop);
    }
    pub fn stop_and_wait(&mut self, generation: u64) -> Result<(), String> {
        self.epoch.store(generation, Ordering::Release);
        if let Some(c) = self.cancel.take() {
            c.cancel();
        }
        let (tx, rx) = sync_channel(1);
        self.sink
            .send(SinkCommand::Stop(Some(tx)))
            .map_err(|_| "audio worker unavailable")?;
        let _ = self.decode.try_send(DecodeCommand::Stop);
        rx.recv_timeout(Duration::from_secs(2))
            .map_err(|_| "audio sink release deadline exceeded".into())
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
                for frame in s.chunks_exact(8).take(n) {
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
    fn track() -> Track {
        Track {
            path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac"),
            ..Default::default()
        }
    }
    fn gapless_track(name: &str, id: i64) -> Track {
        Track {
            id,
            path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/fixtures")
                .join(name),
            ..Default::default()
        }
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
            transport_generation: 0,
            fallback: false,
            fallback_reason: String::new(),
        }
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
}
