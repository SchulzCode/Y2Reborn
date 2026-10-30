//! Writing decoded audio to the sink: the bounded sends and the tail window a crossfade reads from.
use super::*;

pub(crate) fn emit(tx: &SyncSender<PlaybackEvent>, e: Event) {
    let _ = tx.send(PlaybackEvent::Core(e));
}

pub(crate) fn emit_artwork(
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

pub(crate) fn send_before_deadline<T>(
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

pub(crate) fn send_stream_item(
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

pub(crate) const MAX_SINK_WRITE_BYTES: usize = 524_288;

pub(crate) fn send_stream(
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

pub(crate) struct TailWindow {
    pub(crate) data: Vec<u8>,
    pub(crate) position_ms: u64,
    pub(crate) rate: u32,
    pub(crate) format: PcmFormat,
}

impl TailWindow {
    pub(crate) fn new(rate: u32, format: PcmFormat) -> Self {
        Self {
            data: Vec::new(),
            position_ms: 0,
            rate,
            format,
        }
    }
    pub(crate) fn push(&mut self, block: Pcm, retain_frames: usize) -> Option<Pcm> {
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
    pub(crate) fn frames(&self) -> usize {
        self.data.len() / self.format.bytes_per_frame()
    }
    pub(crate) fn take_prefix(&mut self, frames: usize) -> Option<Pcm> {
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
    pub(crate) fn take_all(&mut self) -> Option<Pcm> {
        let frames = self.frames();
        self.take_prefix(frames)
    }
}

pub(crate) fn read_pending_or_decoder(
    decoder: &mut Decoder,
    pending: &mut VecDeque<(Pcm, usize)>,
) -> Result<Option<Pcm>, String> {
    if let Some(block) = take_pending_block(pending) {
        return Ok(Some(block));
    }
    decoder.read()
}

pub(crate) fn take_pending_block(pending: &mut VecDeque<(Pcm, usize)>) -> Option<Pcm> {
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

pub(crate) struct WindowReadFailure {
    pub(crate) partial: Option<Pcm>,
    pub(crate) error: String,
}

pub(crate) fn window_pcm(
    data: Vec<u8>,
    position_ms: u64,
    rate: u32,
    format: PcmFormat,
) -> Option<Pcm> {
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

pub(crate) fn read_window_with(
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

pub(crate) fn read_window(
    decoder: &mut Decoder,
    pending: &mut VecDeque<(Pcm, usize)>,
    wanted_frames: usize,
) -> Result<Option<Pcm>, WindowReadFailure> {
    read_window_with(pending, wanted_frames, || decoder.read())
}

pub(crate) fn recover_window_failure(
    tail: &mut TailWindow,
    failure: WindowReadFailure,
) -> Vec<Pcm> {
    let mut blocks = Vec::with_capacity(2);
    if let Some(suffix) = tail.take_all() {
        blocks.push(suffix);
    }
    if let Some(partial) = failure.partial {
        blocks.push(partial);
    }
    blocks
}

pub(crate) fn push_front_block(pending: &mut VecDeque<(Pcm, usize)>, block: Pcm) {
    if block.frames > 0 {
        pending.push_front((block, 0));
    }
}
