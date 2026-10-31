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
mod click;
mod decode;
mod media_state;
mod output;
mod sink;
/// Fade-in of the first stream after the headphone level went up.
mod stream;
use decode::DecodeWorker;
use media_state::*;
use output::SinkWorker;
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
    /// A wheel click is waiting (it wakes a thread with no sink).
    Click,
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
    /// A wheel click for the sink thread (#67), and the wired sink it may open (#77).
    click: Arc<AtomicBool>,
    click_spec: Arc<Mutex<Option<SinkSpec>>>,
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
        let meter = crate::meter::Meter::new(levels.clone());
        let hold = Arc::new(AtomicBool::new(false));
        let sink_hold = hold.clone();
        let click = Arc::new(AtomicBool::new(false));
        let sink_click = click.clone();
        let click_spec = Arc::new(Mutex::new(None));
        let sink_click_spec = click_spec.clone();
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
        let decode_worker = DecodeWorker {
            dl,
            dr,
            pt,
            det,
            de,
            db,
            decode_tempo,
            decode_upcoming,
            media_state,
            cache,
        };
        thread::Builder::new()
            .name("playback".into())
            .spawn(move || decode_worker.run())
            .map_err(|e| e.to_string())?;
        let output_worker = SinkWorker {
            al,
            sr,
            pr,
            et,
            se,
            ab,
            sink_tempo,
            sink_volume,
            sink_balance,
            sink_hold,
            sink_click,
            sink_click_spec,
            audio_state,
            meter,
            factory,
        };
        thread::Builder::new()
            .name("audio".into())
            .spawn(move || output_worker.run())
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
            click,
            click_spec,
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
    /// One wheel click (#67, #77): in the paused stream, or with `spec` in a wired sink the sink
    /// thread keeps or opens for it. The first click of a run wakes the idle thread.
    pub(crate) fn click(&self, spec: Option<SinkSpec>) {
        if let Ok(mut wanted) = self.click_spec.lock() {
            *wanted = spec;
        }
        if !self.click.swap(true, Ordering::AcqRel) {
            let _ = self.sink.try_send(SinkCommand::Click);
        }
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
