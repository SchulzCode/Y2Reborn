use reborn_audio::{gain, AlsaSink, AudioSink};
use reborn_core::{AudioOutput, Event, Track};
use reborn_media::{Cancel, Decoder, Pcm};
use reborn_observability::{HealthState, Level, Observer};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
pub enum PlaybackEvent {
    Core(Event),
    Artwork { generation: u64, bytes: Vec<u8> },
}
struct Job {
    track: Track,
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
        output: AudioOutput,
        id: u64,
        volume: u8,
    },
    Stop,
    Volume(u8),
}
enum Stream {
    Pcm { generation: u64, block: Pcm },
    End { generation: u64 },
}
pub struct Playback {
    decode: SyncSender<DecodeCommand>,
    sink: SyncSender<SinkCommand>,
    pub events: Receiver<PlaybackEvent>,
    epoch: Arc<AtomicU64>,
    cancel: Option<Cancel>,
}
fn emit(tx: &SyncSender<PlaybackEvent>, e: Event) {
    let _ = tx.send(PlaybackEvent::Core(e));
}
type SinkFactory =
    Box<dyn Fn(&AudioOutput, u32, Observer, u64) -> Result<Box<dyn AudioSink>, String> + Send>;
impl Playback {
    pub fn spawn(log: Observer, cache: PathBuf) -> Result<Self, String> {
        Self::with_sink(
            log,
            cache,
            Box::new(|output, rate, log, id| {
                AlsaSink::open(output, rate, log, id).map(|s| Box::new(s) as Box<dyn AudioSink>)
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
        let de = epoch.clone();
        let se = epoch.clone();
        let db = buffered.clone();
        let ab = buffered.clone();
        let dl = log.clone();
        let al = log;
        let det = et.clone();
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
                    dl.emit(
                        Level::Info,
                        "playback",
                        "open_file",
                        "Opening local track",
                        Some(job.id),
                        json!({"track_id":job.track.id,"path":job.track.path}),
                    );
                    let mut d = Decoder::open(&job.track.path, 48000, job.cancel.clone())?;
                    if job.position > 0 {
                        d.seek(job.position)?;
                    }
                    if d.metadata.artwork {
                        // Keep the lazy artwork cache below 64 fixed-size RGBA files (6.25 MiB).
                        if let Ok(entries) = std::fs::read_dir(&cache) {
                            let mut files = entries
                                .filter_map(Result::ok)
                                .filter(|e| e.path().extension().is_some_and(|x| x == "rgba"))
                                .collect::<Vec<_>>();
                            files.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
                            let remove = files.len().saturating_sub(63);
                            for e in files.into_iter().take(remove) {
                                let _ = std::fs::remove_file(e.path());
                            }
                        }
                        let key = format!(
                            "{}-{}-{}.rgba",
                            job.track.id, job.track.size, job.track.mtime
                        );
                        let path = cache.join(key);
                        let bytes = std::fs::read(&path)
                            .ok()
                            .filter(|b| b.len() == 160 * 160 * 4)
                            .or_else(|| d.artwork().ok());
                        if let Some(bytes) = bytes {
                            let _ = reborn_core::atomic_write(&path, &bytes);
                            let _ = det.try_send(PlaybackEvent::Artwork {
                                generation: job.generation,
                                bytes,
                            });
                        }
                    }
                    let mut packets = 0;
                    let mut frames = 0;
                    loop {
                        dl.heartbeat("playback", 20);
                        if de.load(Ordering::Acquire) != job.generation {
                            return Ok(());
                        }
                        let next = d.read()?;
                        let mut item = if let Some(block) = next {
                            dl.add(
                                "ffmpeg_packets_decoded",
                                block.packets.saturating_sub(packets) as f64,
                            );
                            dl.add(
                                "ffmpeg_frames_decoded",
                                block.frames.saturating_sub(frames) as f64,
                            );
                            packets = block.packets;
                            frames = block.frames;
                            Stream::Pcm {
                                generation: job.generation,
                                block,
                            }
                        } else {
                            Stream::End {
                                generation: job.generation,
                            }
                        };
                        let end = matches!(item, Stream::End { .. });
                        let frames = match &item {
                            Stream::Pcm { block, .. } => block.samples.len() / 2,
                            _ => 0,
                        };
                        let mut stall = false;
                        loop {
                            if de.load(Ordering::Acquire) != job.generation {
                                return Ok(());
                            }
                            db.fetch_add(frames as u64, Ordering::Relaxed);
                            match pt.try_send(item) {
                                Ok(()) => break,
                                Err(TrySendError::Full(v)) => {
                                    db.fetch_sub(frames as u64, Ordering::Relaxed);
                                    item = v;
                                    if !stall {
                                        dl.add("decoder_stalls", 1.);
                                        stall = true
                                    }
                                    dl.heartbeat("playback", 20);
                                    thread::sleep(Duration::from_millis(5));
                                }
                                Err(_) => return Err("audio queue closed".into()),
                            }
                        }
                        if end {
                            return Ok(());
                        }
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
                let mut volume = 35;
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
                                output,
                                id,
                                volume: v,
                            } => {
                                sink = None;
                                pending = None;
                                generation = g;
                                correlation = id;
                                volume = v;
                                started = false;
                                eof = false;
                                match factory(&output, 48000, al.clone(), id) {
                                    Ok(s) => {
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
                            SinkCommand::Stop => {
                                sink = None;
                                pending = None;
                                eof = false;
                            }
                            SinkCommand::Volume(v) => volume = v,
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
                                    Some(v.saturating_sub(block.samples.len() as u64 / 2))
                                })
                                .ok();
                                if g == generation
                                    && g == se.load(Ordering::Acquire)
                                    && sink.is_some()
                                {
                                    gain(&mut block.samples, volume);
                                    pending = Some((block, 0));
                                }
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
                        match s.write(&block.samples[*offset..]) {
                            Ok(n) => {
                                if n > 0 {
                                    *offset += n * 2;
                                    last_progress = Instant::now();
                                    if !started {
                                        started = true;
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
                                    let ms = block.position_ms
                                        + (*offset as u64 / 2).saturating_sub(s.delay()) * 1000
                                            / 48000;
                                    let _ = et.try_send(PlaybackEvent::Core(Event::Position {
                                        generation,
                                        ms,
                                    }));
                                }
                                if *offset >= block.samples.len() {
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
                        sink = None;
                        eof = false;
                    }
                    let n = ab.load(Ordering::Relaxed)
                        + pending
                            .as_ref()
                            .map(|(b, o)| (b.samples.len() - o) as u64 / 2)
                            .unwrap_or(0)
                        + sink.as_ref().map(|s| s.delay()).unwrap_or(0);
                    al.gauge("audio_buffer_frames", n as f64);
                    al.gauge("audio_buffer_ms", n as f64 / 48.);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            decode: dt,
            sink: st,
            events: er,
            epoch,
            cancel: None,
        })
    }
    pub fn load(
        &mut self,
        track: Track,
        position: u64,
        output: AudioOutput,
        generation: u64,
        volume: u8,
        id: u64,
    ) -> Result<(), String> {
        self.stop(generation);
        let cancel = Cancel::new()?;
        self.cancel = Some(cancel.clone());
        self.sink
            .try_send(SinkCommand::Open {
                generation,
                output,
                id,
                volume,
            })
            .map_err(|_| "audio command queue full")?;
        self.decode
            .try_send(DecodeCommand::Load(Box::new(Job {
                track,
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
        let _ = self.sink.try_send(SinkCommand::Stop);
        let _ = self.decode.try_send(DecodeCommand::Stop);
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
    pub fn volume(&self, v: u8) {
        let _ = self.sink.try_send(SinkCommand::Volume(v));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reborn_audio::Parameters;
    struct Sink {
        frames: Arc<AtomicU64>,
        fail: bool,
    }
    impl AudioSink for Sink {
        fn parameters(&self) -> Parameters {
            Parameters {
                rate: 48000,
                period: 512,
                buffer: 4096,
            }
        }
        fn write(&mut self, s: &[i16]) -> Result<usize, String> {
            if self.fail {
                return Err("injected sink disconnect".into());
            }
            thread::sleep(Duration::from_millis(2));
            let n = (s.len() / 2).min(128);
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
            Box::new(move |_, _, _, _| {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(Box::new(Sink {
                    frames: count.clone(),
                    fail: false,
                }))
            }),
        )
        .unwrap();
        p.load(track(), 0, AudioOutput::Wired, 1, 35, 1).unwrap();
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
            500,
            AudioOutput::Bluetooth("12:34:56:78:90:AB".into()),
            3,
            35,
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
    fn sink_failure_reaches_authoritative_model() {
        let root = std::env::temp_dir().join(format!("reborn-sink-fault-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let mut p = Playback::with_sink(
            log,
            root,
            Box::new(|_, _, _, _| {
                Ok(Box::new(Sink {
                    frames: Arc::new(AtomicU64::new(0)),
                    fail: true,
                }))
            }),
        )
        .unwrap();
        p.load(track(), 0, AudioOutput::Wired, 9, 35, 1).unwrap();
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
}
