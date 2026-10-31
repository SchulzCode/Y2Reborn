//! The sink thread ("audio"): opens, keeps and switches the sink and writes the PCM it is sent, with volume, balance, hold and the level meter.
use super::click::{joined, Click, Tail, LINGER};
use super::*;
use std::ops::ControlFlow;

/// What the thread owns and shares with `Playback`.
pub(super) struct SinkWorker {
    pub(super) al: Observer,
    pub(super) sr: Receiver<SinkCommand>,
    pub(super) pr: Receiver<Stream>,
    pub(super) et: SyncSender<PlaybackEvent>,
    pub(super) se: Arc<AtomicU64>,
    pub(super) ab: Arc<AtomicU64>,
    pub(super) sink_tempo: Arc<AtomicU64>,
    pub(super) sink_volume: Arc<AtomicU8>,
    pub(super) sink_balance: Arc<AtomicI8>,
    pub(super) sink_hold: Arc<AtomicBool>,
    /// A wheel click was asked for (#67).
    pub(super) sink_click: Arc<AtomicBool>,
    pub(super) audio_state: Arc<Mutex<serde_json::Value>>,
    pub(super) meter: crate::meter::Meter,
    pub(super) factory: SinkFactory,
}

impl SinkWorker {
    pub(super) fn run(mut self) {
        let sink: Option<Box<dyn AudioSink>> = None;
        let generation = 0;
        let correlation = 0;
        let rate = 44100;
        let sink_format = PcmFormat::S32LE;
        let gain = VolumeRamp::new(100, rate);
        let pending: Option<(Pcm, usize)> = None;
        let started = false;
        let eof = false;
        let drain_start = Instant::now();
        let last_progress = Instant::now();
        let held = false;
        // The playing sink's spec and wired epoch, and a wired sink
        // kept open for the next stream.
        let open_spec: Option<(SinkSpec, u64)> = None;
        let kept: Option<KeptSink> = None;
        let mut st = Playing {
            click: Click::default(),
            tail: Tail::default(),
            silence_queued: false,
            clicking: None,
            sink,
            generation,
            correlation,
            rate,
            sink_format,
            gain,
            pending,
            started,
            eof,
            drain_start,
            last_progress,
            held,
            open_spec,
            kept,
        };
        while let ControlFlow::Continue(()) | ControlFlow::Break(Pass::Again) = self.pass(&mut st) {
        }
    }

    /// One pass of the sink loop; `Break(Exit)` ends the thread, `Break(Again)` the pass.
    fn pass(&mut self, st: &mut Playing) -> ControlFlow<Pass> {
        self.wait_and_commands(st)?;
        self.stopped(st);
        self.hold(st)?;
        self.receive(st)?;
        self.write(st);
        self.drain(st);
        self.gauges(st);
        ControlFlow::Continue(())
    }

    /// The worker heartbeat, then the commands: open, stop, hold changes and shutdown (waiting on them while no sink is open).
    fn wait_and_commands(&mut self, st: &mut Playing) -> ControlFlow<Pass> {
        self.al.heartbeat("audio", 10);
        // Without a PCM sink there is no audio deadline. Wait on the
        // command channel so Open/Stop/Shutdown still wake immediately;
        // the timeout only maintains the worker heartbeat (and
        // closes a kept sink on time).
        let idle_command = if st.sink.is_none() {
            let wait = st.kept.as_ref().map_or(Duration::from_secs(1), |k| {
                KEEP_SINK_FOR
                    .saturating_sub(k.since.elapsed())
                    .max(Duration::from_millis(10))
            });
            match self.sr.recv_timeout(wait) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return ControlFlow::Break(Pass::Exit),
            }
        } else {
            None
        };
        for cmd in idle_command.into_iter().chain(self.sr.try_iter()) {
            match cmd {
                SinkCommand::Shutdown(reply) => {
                    drop(st.sink.take());
                    drop(st.kept.take());
                    let _ = reply.try_send(());
                    return ControlFlow::Break(Pass::Exit);
                }
                SinkCommand::Open {
                    generation: g,
                    spec,
                    id,
                    fade_in,
                    reply,
                } => {
                    if g != self.se.load(Ordering::Acquire) {
                        let _ = reply.try_send(Err("stale sink generation".into()));
                        continue;
                    }
                    if let Some(k) = keep_sink(
                        st.sink.take(),
                        st.open_spec.take(),
                        &self.al,
                        st.correlation,
                    ) {
                        st.kept = Some(k);
                    }
                    st.pending = None;
                    st.held = false;
                    st.forget_written();
                    self.sink_click.store(false, Ordering::Relaxed);
                    st.generation = g;
                    st.correlation = id;
                    // An unfinished fade carries over (Next, a seek or another
                    // rate during it must not jump to full volume).
                    let carried = st.gain.fade_progress(st.rate);
                    let volume = self.sink_volume.load(Ordering::Relaxed);
                    st.rate = spec.rate;
                    st.sink_format = spec.format;
                    st.gain = opening_gain(volume, st.rate, false, false, carried).0;
                    st.gain.mono(spec.speaker);
                    st.started = false;
                    st.eof = false;
                    let wired = reborn_audio::wired_epoch();
                    let reused = st.kept.take().and_then(|k| {
                                let summary = |s: &SinkSpec| {
                                    json!({"output":s.output,"rate":s.rate,"format":s.format.as_str(),"device":s.device})
                                };
                                match sink_change(&k.spec, k.wired, &spec, wired) {
                                    None => {
                                        self.al.emit(
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
                                        self.al.emit(
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
                        None => (self.factory)(&spec, self.al.clone(), id).map(|s| (s, false)),
                    };
                    match opened {
                        Ok((s, reused)) => {
                            let params = s.parameters();
                            // A kept stream did not power the
                            // amplifier up again: no level rise.
                            let (ramp, fade) = opening_gain(
                                volume,
                                st.rate,
                                params.level_raised && !reused,
                                fade_in,
                                carried,
                            );
                            st.gain = match fade {
                                None if reused => {
                                    VolumeRamp::fade_in(volume, st.rate, SWITCH_FADE_MS)
                                }
                                _ => ramp,
                            };
                            // #28: the speaker plays one channel.
                            st.gain.mono(spec.speaker);
                            if let Some((event, message, ms)) = fade {
                                self.al.emit(
                                    Level::Info,
                                    "audio",
                                    event,
                                    message,
                                    Some(id),
                                    json!({"hp_level":params.hp_level_live,"fade_ms":ms}),
                                );
                            }
                            if let Ok(mut value) = self.audio_state.lock() {
                                if let Some(object) = value.as_object_mut() {
                                    object.insert(
                                        "alsa".into(),
                                        serde_json::to_value(&params).unwrap_or_default(),
                                    );
                                    object.insert("output_rate".into(), json!(params.rate));
                                    object.insert("output_format".into(), json!(params.format));
                                }
                            }
                            if params.fallback && !reused {
                                self.al.add("audio_sink_fallbacks", 1.);
                            }
                            st.open_spec = Some((spec, wired));
                            st.sink = Some(s);
                            self.al
                                .health_set("audio", HealthState::Ok, true, "sink ready");
                            let _ = reply.try_send(Ok(()));
                        }
                        Err(e) => {
                            self.al.health_set("audio", HealthState::Failed, true, &e);
                            let _ = reply.try_send(Err(e));
                        }
                    }
                    st.last_progress = Instant::now();
                }
                SinkCommand::Stop(reply) => {
                    st.sink = None;
                    st.kept = None;
                    st.pending = None;
                    st.eof = false;
                    st.held = false;
                    st.forget_written();
                    if let Some(reply) = reply {
                        let _ = reply.try_send(());
                    }
                }
                SinkCommand::Switch(reply) => {
                    if let Some(k) = keep_sink(
                        st.sink.take(),
                        st.open_spec.take(),
                        &self.al,
                        st.correlation,
                    ) {
                        st.kept = Some(k);
                    }
                    st.pending = None;
                    st.eof = false;
                    st.held = false;
                    st.forget_written();
                    let _ = reply.try_send(st.kept.as_ref().map(|k| k.spec.clone()));
                }
            }
        }
        ControlFlow::Continue(())
    }

    /// Stopped or replaced: a wired sink waits for the next stream; a kept sink closes when its time is up.
    fn stopped(&mut self, st: &mut Playing) {
        // Stopped or replaced: a wired sink waits for the next
        // stream (a Stop closes it, as does the time running out).
        if st.generation != self.se.load(Ordering::Acquire) {
            if let Some(k) = keep_sink(
                st.sink.take(),
                st.open_spec.take(),
                &self.al,
                st.correlation,
            ) {
                st.kept = Some(k);
            }
            st.pending = None;
            st.eof = false;
            st.forget_written();
        }
        if st
            .kept
            .as_ref()
            .is_some_and(|k| k.since.elapsed() >= KEEP_SINK_FOR)
        {
            st.kept = None;
        }
    }

    /// Paused in place: the device stays open and paused and nothing is written.
    fn hold(&mut self, st: &mut Playing) -> ControlFlow<Pass> {
        // Paused in place: keep the device open and paused, write
        // nothing, read nothing (the decoder waits on the full
        // queue). The stall and drain clocks restart on resume.
        let want_hold = self.sink_hold.load(Ordering::Acquire) && st.sink.is_some();
        if want_hold != st.held {
            if let Some(s) = &mut st.sink {
                // Clicks left silence queued: the music goes on without it.
                if !want_hold && std::mem::take(&mut st.silence_queued) {
                    let _ = s.rewind(u64::MAX);
                }
                st.clicking = None;
                if let Err(e) = s.pause(want_hold) {
                    self.al.emit(
                        Level::Warn,
                        "audio",
                        "pause_failed",
                        &e,
                        Some(st.correlation),
                        json!({"pause":want_hold}),
                    );
                }
            }
            st.held = want_hold;
            st.last_progress = Instant::now();
            st.drain_start = Instant::now();
        }
        if st.held {
            self.click_while_held(st);
            let pace = if st.clicking.is_some() { 10 } else { 20 };
            thread::sleep(Duration::from_millis(pace));
            return ControlFlow::Break(Pass::Again);
        }
        ControlFlow::Continue(())
    }

    /// The next block of PCM, or the end of the stream.
    fn receive(&mut self, st: &mut Playing) -> ControlFlow<Pass> {
        if st.pending.is_none() {
            match self.pr.recv_timeout(Duration::from_millis(10)) {
                Ok(Stream::Pcm {
                    generation: g,
                    mut block,
                }) => {
                    self.ab
                        .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                            Some(v.saturating_sub(block.frames))
                        })
                        .ok();
                    if g == st.generation
                        && g == self.se.load(Ordering::Acquire)
                        && st.sink.is_some()
                    {
                        if block.format != st.sink_format {
                            match convert_pcm(&block.data, block.rate, block.format, st.sink_format)
                            {
                                Ok(data) => {
                                    block.data = data;
                                    block.format = st.sink_format;
                                    block.frames = block.data.len() as u64
                                        / st.sink_format.bytes_per_frame() as u64;
                                }
                                Err(e) => {
                                    self.al.add("playback_errors", 1.);
                                    emit(
                                        &self.et,
                                        Event::PlaybackError {
                                            generation: g,
                                            message: e,
                                        },
                                    );
                                    return ControlFlow::Break(Pass::Again);
                                }
                            }
                        }
                        st.pending = Some((block, 0));
                    }
                }
                Ok(Stream::Artwork {
                    generation: g,
                    entry_id,
                    track_id,
                    bytes,
                }) if g == st.generation
                    && g == self.se.load(Ordering::Acquire)
                    && st.sink.is_some() =>
                {
                    emit_artwork(&self.et, g, entry_id, track_id, bytes);
                }
                Ok(Stream::Boundary {
                    generation: g,
                    next_entry_id,
                    next_track_id,
                    transition_ms,
                }) if g == st.generation => {
                    st.started = false;
                    self.al.add("gapless_transitions", 1.);
                    self.al.gauge("gapless_transition_ms", transition_ms as f64);
                    if let Ok(mut value) = self.audio_state.lock() {
                        if let Some(object) = value.as_object_mut() {
                            object.insert("gapless_transition_ms".into(), json!(transition_ms));
                            object.insert("state".into(), json!("buffering"));
                        }
                    }
                    emit(
                        &self.et,
                        Event::TrackBoundary {
                            generation: g,
                            next_entry_id,
                            next_track_id,
                            output_position_ms: 0,
                        },
                    );
                }
                Ok(Stream::End { generation: g }) if g == st.generation => {
                    st.eof = true;
                    st.drain_start = Instant::now();
                }
                Err(RecvTimeoutError::Disconnected) => return ControlFlow::Break(Pass::Exit),
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }

    /// Write what is pending to the sink, with volume, balance and the level meter.
    fn write(&mut self, st: &mut Playing) {
        self.click_in_music(st);
        let wired = st.wired();
        if let (Some(s), Some((block, offset))) = (&mut st.sink, &mut st.pending) {
            let frame_bytes = block.format.bytes_per_frame();
            // Bound gain work and control latency even for a large
            // decoded crossfade. Keep original PCM for partial writes.
            let max_bytes = 512 * frame_bytes;
            let end = (*offset + max_bytes).min(block.data.len());
            st.gain.target(self.sink_volume.load(Ordering::Relaxed));
            st.gain.balance(self.sink_balance.load(Ordering::Relaxed));
            let mut scaled = st.gain.render(&block.data[*offset..end], block.format);
            st.click.mix(&mut scaled, block.format, st.gain.level());
            match s.write(&scaled) {
                Ok(n) => {
                    st.gain.advance(n);
                    st.click.advance(n);
                    if n > 0 {
                        let written = *offset + n * frame_bytes;
                        if wired {
                            let ms = block.position_ms
                                + (*offset / frame_bytes) as u64
                                    * self.sink_tempo.load(Ordering::Acquire)
                                    / u64::from(block.rate);
                            st.tail.push(ms, &block.data[*offset..written], frame_bytes);
                        }
                        self.meter.measure(
                            &block.data[*offset..written.min(block.data.len())],
                            block.format,
                            block.rate,
                            s.delay(),
                        );
                        *offset += n * block.format.bytes_per_frame();
                        st.last_progress = Instant::now();
                        if !st.started {
                            st.started = true;
                            if let Ok(mut value) = self.audio_state.lock() {
                                if let Some(object) = value.as_object_mut() {
                                    object.insert("state".into(), json!("playing"));
                                }
                            }
                            self.al.emit(
                                Level::Info,
                                "audio",
                                "started",
                                "PCM playback started",
                                Some(st.correlation),
                                json!({"generation":st.generation}),
                            );
                            self.al.add("playback_tracks_started", 1.);
                            emit(
                                &self.et,
                                Event::TrackStarted {
                                    generation: st.generation,
                                },
                            );
                        }
                        let frame_bytes = block.format.bytes_per_frame() as u64;
                        // Played frames in source time (× speed).
                        let ms = block.position_ms
                            + (*offset as u64 / frame_bytes).saturating_sub(s.delay())
                                * self.sink_tempo.load(Ordering::Acquire)
                                / u64::from(block.rate);
                        let _ = self.et.try_send(PlaybackEvent::Core(Event::Position {
                            generation: st.generation,
                            ms,
                        }));
                    }
                    if *offset >= block.data.len() {
                        st.pending = None;
                    } else if st.last_progress.elapsed() > Duration::from_secs(3) {
                        emit(
                            &self.et,
                            Event::PlaybackError {
                                generation: st.generation,
                                message: "audio write made no progress for 3s".into(),
                            },
                        );
                        st.sink = None;
                        st.pending = None;
                        st.forget_written();
                    }
                }
                Err(e) => {
                    self.al.add("playback_errors", 1.);
                    self.al.health_set("audio", HealthState::Failed, true, &e);
                    emit(
                        &self.et,
                        Event::PlaybackError {
                            generation: st.generation,
                            message: e,
                        },
                    );
                    st.sink = None;
                    st.pending = None;
                    st.forget_written();
                }
            }
        }
    }

    /// Whether a wheel click is due: asked for, and a wired sink open to take it (#67).
    fn take_click(&self, st: &Playing) -> bool {
        self.sink_click.swap(false, Ordering::AcqRel) && st.sink.is_some() && st.wired()
    }

    /// A wheel click while music is written: it goes in front of the music not yet played.
    fn click_in_music(&mut self, st: &mut Playing) {
        if self.take_click(st) {
            self.take_back(st);
            st.click.start(st.rate);
        }
    }

    /// Paused in place: a wheel click plays over silence, the music queued before it waits for
    /// the resume; `LINGER` after the last click the stream pauses again.
    fn click_while_held(&mut self, st: &mut Playing) {
        let was_paused = st.clicking.is_none();
        if self.take_click(st) {
            self.take_back(st);
            st.silence_queued = true;
            st.click.start(st.rate);
            st.clicking = Some(Instant::now());
        }
        let Some(since) = st.clicking else { return };
        let Some(s) = &mut st.sink else { return };
        let result = if since.elapsed() < LINGER {
            click_over_silence(s.as_mut(), &mut st.click, st.sink_format, st.gain.level())
                .and_then(|()| if was_paused { s.pause(false) } else { Ok(()) })
        } else {
            st.clicking = None;
            s.pause(true)
        };
        if let Err(e) = result {
            st.clicking = None;
            self.al.emit(
                Level::Warn,
                "audio",
                "click_failed",
                &e,
                Some(st.correlation),
                json!({}),
            );
        }
    }

    /// Before a click, what the sink queued and has not played comes back: music to be written
    /// again after the click's start, a paused stream's silence to be dropped.
    fn take_back(&self, st: &mut Playing) {
        let Some(s) = &mut st.sink else { return };
        if st.silence_queued {
            let _ = s.rewind(u64::MAX);
            return;
        }
        let back = s.rewind(st.tail.frames() as u64).unwrap_or(0) as usize;
        let tempo = self.sink_tempo.load(Ordering::Acquire);
        if let Some(music) = st.tail.take_last(back, st.sink_format, st.rate, tempo) {
            st.pending = Some((joined(music, st.pending.take()), 0));
        }
    }

    /// The stream's end: drain the sink, then report the end of the track.
    fn drain(&mut self, st: &mut Playing) {
        if st.eof
            && st.pending.is_none()
            && (st.sink.as_ref().is_none_or(|s| s.delay() == 0)
                || st.drain_start.elapsed() > Duration::from_secs(3))
        {
            self.al.add("playback_tracks_completed", 1.);
            self.al.emit(
                Level::Info,
                "audio",
                "drained",
                "PCM reached end of stream",
                Some(st.correlation),
                json!({"generation":st.generation}),
            );
            emit(
                &self.et,
                Event::TrackEnded {
                    generation: st.generation,
                },
            );
            if let Ok(mut value) = self.audio_state.lock() {
                if let Some(object) = value.as_object_mut() {
                    object.insert("state".into(), json!("stopped"));
                }
            }
            // The next track (gapless off, Repeat One) starts in it.
            if let Some(k) = keep_sink(
                st.sink.take(),
                st.open_spec.take(),
                &self.al,
                st.correlation,
            ) {
                st.kept = Some(k);
            }
            st.eof = false;
        }
    }

    /// The buffer gauges.
    fn gauges(&mut self, st: &mut Playing) {
        let n = self.ab.load(Ordering::Relaxed)
            + st.pending
                .as_ref()
                .map(|(b, o)| (b.data.len() - o) as u64 / b.format.bytes_per_frame() as u64)
                .unwrap_or(0)
            + st.sink.as_ref().map(|s| s.delay()).unwrap_or(0);
        self.al.gauge("audio_buffer_frames", n as f64);
        self.al
            .gauge("audio_buffer_ms", n as f64 * 1000. / f64::from(st.rate));
    }
}

/// Silence with the click in it, enough that a paused stream that clicked keeps playing.
fn click_over_silence(
    sink: &mut dyn AudioSink,
    click: &mut Click,
    format: PcmFormat,
    gain: f64,
) -> Result<(), String> {
    const CHUNK: usize = 512;
    const QUEUED: u64 = 2048;
    let mut pcm = vec![0; CHUNK * format.bytes_per_frame()];
    for _ in 0..4 {
        if sink.delay() >= QUEUED {
            break;
        }
        pcm.fill(0);
        click.mix(&mut pcm, format, gain);
        let n = sink.write(&pcm)?;
        click.advance(n);
        if n < CHUNK {
            break;
        }
    }
    Ok(())
}

/// The playing stream as the sink thread keeps it from one pass to the next. Declared in reverse: the
/// fields drop as the thread's locals did, the kept sink before the playing one.
pub(super) struct Playing {
    /// The wheel click sounding (#67).
    click: Click,
    /// The music last written to a wired sink, for a click to put back behind it.
    tail: Tail,
    /// Paused in place, the sink holds silence a click left, not music.
    silence_queued: bool,
    /// Paused in place, the stream plays for clicks: since the last one.
    clicking: Option<Instant>,
    kept: Option<KeptSink>,
    open_spec: Option<(SinkSpec, u64)>,
    held: bool,
    last_progress: Instant,
    drain_start: Instant,
    eof: bool,
    started: bool,
    pending: Option<(Pcm, usize)>,
    gain: VolumeRamp,
    sink_format: PcmFormat,
    rate: u32,
    correlation: u64,
    generation: u64,
    sink: Option<Box<dyn AudioSink>>,
}

impl Playing {
    fn wired(&self) -> bool {
        (self.open_spec.as_ref()).is_some_and(|(spec, _)| matches!(spec.output, AudioOutput::Wired))
    }
    /// The stream ended or changed: nothing written can come back, no click sounds.
    fn forget_written(&mut self) {
        self.tail.clear();
        self.click.stop();
        self.silence_queued = false;
        self.clicking = None;
    }
}

/// How a pass of the sink loop ends early.
pub(super) enum Pass {
    /// Start the next pass at once (the rest of this one is skipped).
    Again,
    /// The thread ends: shut down, or its channels are gone.
    Exit,
}
