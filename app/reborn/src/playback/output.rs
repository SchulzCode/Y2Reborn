//! The sink thread ("audio"): opens, keeps and switches the sink and writes the PCM it is sent, with volume, balance, hold and the level meter.
use super::click::{Click, CLICK_KEEP, LINGER};
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
    /// A wheel click was asked for (#67), and the wired sink it may open (#77).
    pub(super) sink_click: Arc<AtomicBool>,
    pub(super) sink_click_spec: Arc<Mutex<Option<SinkSpec>>>,
    pub(super) audio_state: Arc<Mutex<serde_json::Value>>,
    pub(super) meter: crate::meter::Meter,
    pub(super) factory: SinkFactory,
}

impl SinkWorker {
    pub(super) fn run(mut self) {
        let realtime = reborn_platform::audio_thread_priority();
        self.al.emit(
            Level::Info,
            "audio",
            "thread_priority",
            "the audio thread asked for real-time scheduling",
            None,
            json!({"realtime":realtime}),
        );
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
            stop_seen: None,
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
        self.bridge(st);
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
            match self.sr.recv_timeout(st.idle_wait()) {
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
                    st.forget_clicks();
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
                        Some(s) => reuse_kept(s, &spec),
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
                    st.forget_clicks();
                    if let Some(reply) = reply {
                        let _ = reply.try_send(());
                    }
                }
                SinkCommand::Click => {}
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
                    st.forget_clicks();
                    let _ = reply.try_send(st.kept.as_ref().map(|k| k.spec.clone()));
                }
            }
        }
        ControlFlow::Continue(())
    }

    /// Stopped or replaced: the sink waits for the next stream; a kept sink closes when its time is up.
    fn stopped(&mut self, st: &mut Playing) {
        // Stopped or replaced: the sink waits for the next stream
        // (a Stop closes it, as does the time running out).
        let current = self.se.load(Ordering::Acquire);
        if st.generation != current {
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
            // Once per stop: the generation stays behind until the next song opens, and a click
            // forgotten on every pass got one queue of silence and no more, so the next click
            // only revived the run-dry stream and was lost: every other row clicked (Y2Linux #127).
            if st.stop_seen != Some(current) {
                st.forget_clicks();
                st.stop_seen = Some(current);
            }
        }
        if st.kept.as_ref().is_some_and(|k| Instant::now() >= k.until) {
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
            // A Bluetooth sink is only left alone: its own pause drained the
            // stream (BlueALSA 5.0 stops it after ~100 ms), a gap in the
            // packets that AirPods Pro never recovered from; the hold before a
            // track change (Next) now keeps them flowing, and a real Bluetooth
            // pause closes the sink (F43).
            let device_pause = !st
                .open_spec
                .as_ref()
                .is_some_and(|(spec, _)| keeps_queued_audio(spec));
            if let Some(s) = st.sink.as_mut().filter(|_| device_pause) {
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
            // Held before a track change: a Bluetooth stream stays fed (no device pause there).
            self.bridge(st);
            self.click_while_held(st);
            let pace = if st.clicking.is_some() { 10 } else { 20 };
            thread::sleep(Duration::from_millis(pace));
            return ControlFlow::Break(Pass::Again);
        }
        if st.sink.is_none() {
            self.click_while_idle(st);
        } else {
            // The wheel clicks only while no music plays (#77).
            self.sink_click.store(false, Ordering::Relaxed);
        }
        ControlFlow::Continue(())
    }

    /// The next block of PCM, or the end of the stream.
    fn receive(&mut self, st: &mut Playing) -> ControlFlow<Pass> {
        if st.pending.is_none() {
            // No music can come without a sink: clicks or a kept Bluetooth sink's silence are
            // not kept waiting on it (they underran at a ~20 ms refill, reports 2026-11-06).
            let wait = if st.sink.is_none() && st.idle_wait() <= Duration::from_millis(10) {
                Duration::ZERO
            } else {
                Duration::from_millis(10)
            };
            match self.pr.recv_timeout(wait) {
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
        if let (Some(s), Some((block, offset))) = (&mut st.sink, &mut st.pending) {
            let frame_bytes = block.format.bytes_per_frame();
            // Bound gain work and control latency even for a large
            // decoded crossfade. Keep original PCM for partial writes.
            let max_bytes = 512 * frame_bytes;
            let end = (*offset + max_bytes).min(block.data.len());
            st.gain.target(self.sink_volume.load(Ordering::Relaxed));
            st.gain.balance(self.sink_balance.load(Ordering::Relaxed));
            let scaled = st.gain.render(&block.data[*offset..end], block.format);
            match s.write(&scaled) {
                Ok(n) => {
                    st.gain.advance(n);
                    if n > 0 {
                        let written = *offset + n * frame_bytes;
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
                        let ms = heard_ms(
                            block.position_ms,
                            *offset as u64 / frame_bytes,
                            s.delay(),
                            self.sink_tempo.load(Ordering::Acquire),
                            block.rate,
                        );
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
                        st.forget_clicks();
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
                    st.forget_clicks();
                }
            }
        }
    }

    /// A Bluetooth stream never runs dry while it is open: an underrun stops it inside BlueALSA
    /// (xrun recovery is Drop and Start), a gap after which AirPods stay silent until a new
    /// stream starts (Y2Linux #96, Y2Reborn #99, F69). Whenever no music is written (a song's
    /// start after a skip or seek, a decoder catching up, a pause, the end of a song, the sink
    /// kept between two streams) silence keeps it fed, as a phone's mixer does. The stream only
    /// stops when its sink closes: a Stop, the kept sink's time running out, the pause grace.
    fn bridge(&mut self, st: &mut Playing) {
        let written = match (st.sink.as_mut(), st.kept.as_mut()) {
            (Some(s), _) => {
                let bluetooth =
                    (st.open_spec.as_ref()).is_some_and(|(spec, _)| keeps_queued_audio(spec));
                // While held, nothing of the pending music is written either.
                if !bluetooth || (st.pending.is_some() && !st.held) {
                    return;
                }
                top_up_silence(s.as_mut(), st.sink_format, st.rate)
            }
            (None, Some(k)) if keeps_queued_audio(&k.spec) => {
                top_up_silence(k.sink.as_mut(), k.spec.format, k.spec.rate)
            }
            _ => return,
        };
        if written > 0 {
            self.al.add("bluetooth_bridge_frames", written as f64);
        }
    }

    /// Whether a wheel click is due: asked for, and the open sink `wired` (#67).
    fn take_click(&self, wired: bool) -> bool {
        self.sink_click.swap(false, Ordering::AcqRel) && wired
    }

    /// Paused in place: a wheel click plays over silence, and `LINGER` after the last click the
    /// stream stops again. The wired codec has no pause of its own: a pause plays out the queued
    /// music and stops the stream (#77: writing to it failed "File descriptor in bad state"), so
    /// the first click prepares it again, once that music has played. Later clicks go into the
    /// next silence written; the short queue keeps them prompt (taking the queued silence back
    /// left the stream empty and underrunning: crackle).
    fn click_while_held(&mut self, st: &mut Playing) {
        let was_stopped = st.clicking.is_none();
        let wired = st.wired();
        let Some(s) = &mut st.sink else { return };
        if self.take_click(wired) {
            if was_stopped && s.delay() > 0 {
                // The paused music is still playing out: click once it has.
                self.sink_click.store(true, Ordering::Relaxed);
                return;
            }
            st.click.start(st.rate);
            st.clicking = Some(Instant::now());
        }
        let Some(since) = st.clicking else { return };
        let gain = self.click_gain();
        let result = if since.elapsed() < LINGER {
            let resumed = if was_stopped { s.pause(false) } else { Ok(()) };
            resumed
                .and_then(|()| click_over_silence(s.as_mut(), &mut st.click, st.sink_format, gain))
        } else {
            st.clicking = None;
            s.pause(true)
        };
        self.click_result(st, result);
    }

    /// No stream: a wheel click plays in the wired sink kept after a song, or opens one for the
    /// output asked for (headphones or speaker) and keeps it `CLICK_KEEP` after the last click, so
    /// the next clicks (and a song started meanwhile) find it open. `LINGER` after the last click
    /// its queued silence is dropped, as a kept sink's is.
    fn click_while_idle(&mut self, st: &mut Playing) {
        let wanted = self
            .sink_click_spec
            .lock()
            .ok()
            .and_then(|mut spec| spec.take());
        if self.sink_click.swap(false, Ordering::AcqRel) {
            if let Some(spec) = wanted {
                self.keep_for_clicks(st, spec);
            }
            // A click plays only in a wired sink, never in the headphones'.
            let Some(k) = st
                .kept
                .as_mut()
                .filter(|k| k.spec.output == AudioOutput::Wired)
            else {
                return;
            };
            let now = Instant::now();
            k.until = k.until.max(now + CLICK_KEEP);
            st.click.start(k.spec.rate);
            st.clicking = Some(now);
        }
        let Some(since) = st.clicking else { return };
        let gain = self.click_gain();
        let Some(k) = &mut st.kept else {
            st.clicking = None;
            return;
        };
        let result = if since.elapsed() < LINGER {
            click_over_silence(k.sink.as_mut(), &mut st.click, k.spec.format, gain)
        } else {
            st.clicking = None;
            k.sink.discard()
        };
        self.click_result(st, result);
    }

    /// The kept sink when it is the one `spec` asks for, else one opened for it.
    fn keep_for_clicks(&mut self, st: &mut Playing, spec: SinkSpec) {
        let wired = reborn_audio::wired_epoch();
        if (st.kept.as_ref()).is_some_and(|k| sink_change(&k.spec, k.wired, &spec, wired).is_none())
        {
            return;
        }
        drop(st.kept.take());
        st.clicking = None;
        match (self.factory)(&spec, self.al.clone(), st.correlation) {
            Ok(sink) => {
                let since = Instant::now();
                st.kept = Some(KeptSink {
                    sink,
                    spec,
                    wired,
                    since,
                    until: since + CLICK_KEEP,
                });
            }
            Err(e) => self.click_result(st, Err(e)),
        }
    }

    /// A click's volume: the live one (the sink's ramp is not kept up while no music plays).
    fn click_gain(&self) -> f64 {
        reborn_core::volume_gain(self.sink_volume.load(Ordering::Relaxed))
    }

    fn click_result(&self, st: &mut Playing, result: Result<(), String>) {
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

    /// The stream's end: drain the sink, then report the end of the track. A Bluetooth sink is
    /// not drained: its last music plays on behind silence (`bridge`) while the next track starts
    /// in it; draining it empty stopped the stream between two songs (Y2Reborn #99).
    fn drain(&mut self, st: &mut Playing) {
        let bluetooth = (st.open_spec.as_ref()).is_some_and(|(spec, _)| keeps_queued_audio(spec));
        if st.eof
            && st.pending.is_none()
            && (bluetooth
                || st.sink.as_ref().is_none_or(|s| s.delay() == 0)
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

/// How much a Bluetooth sink keeps queued while no music arrives (`bridge`): 100 ms of its
/// ~180 ms ring, refilled every pass of the loop (~20 ms).
const BRIDGE_MS: u64 = 100;

/// Silence written until `sink` holds `BRIDGE_MS` of audio in its ring; the frames written.
pub(super) fn top_up_silence(sink: &mut dyn AudioSink, format: PcmFormat, rate: u32) -> u64 {
    const CHUNK: usize = 256;
    let low = u64::from(rate) * BRIDGE_MS / 1000;
    let silence = vec![0; CHUNK * format.bytes_per_frame()];
    let mut written = 0;
    // Bounded: a sink that misreports its ring never keeps the thread here.
    for _ in 0..=low as usize / CHUNK {
        if !sink.queued().is_some_and(|queued| queued < low) {
            break;
        }
        match sink.write(&silence) {
            Ok(n) if n > 0 => written += n as u64,
            _ => break,
        }
    }
    written
}

/// Silence with the click in it, enough that a paused stream that clicked keeps playing: about
/// 46 ms queued (a click waits at most that long; 35 ms underran on a busy pass), from 256-frame
/// pieces.
/// Where the listener is: the block's start, plus its frames written, less the output's delay, in
/// source time (× speed, per mille). The delay reaches back before the block when it is longer than
/// what of the block went out: on Bluetooth (BlueALSA's buffer, encoder and the headphones' own
/// delay, several hundred ms) the position stopped at the block's start and ran ahead of the sound,
/// and a film's picture with it (Y2Reborn #133).
pub(super) fn heard_ms(block_ms: u64, written: u64, delay: u64, tempo: u64, rate: u32) -> u64 {
    let rate = u64::from(rate.max(1));
    (block_ms + written * tempo / rate).saturating_sub(delay * tempo / rate)
}

fn click_over_silence(
    sink: &mut dyn AudioSink,
    click: &mut Click,
    format: PcmFormat,
    gain: f64,
) -> Result<(), String> {
    const CHUNK: usize = 256;
    const QUEUED: u64 = 2048;
    let mut pcm = vec![0; CHUNK * format.bytes_per_frame()];
    for _ in 0..QUEUED as usize / CHUNK {
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
    /// Paused in place, the stream plays for clicks: since the last one.
    clicking: Option<Instant>,
    kept: Option<KeptSink>,
    /// The stop (sink generation) whose clicks were forgotten.
    stop_seen: Option<u64>,
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
    /// How long the thread without a sink may wait for a command: until a kept sink closes (at
    /// most a second, for the heartbeat), 10 ms while clicks play in it or while a kept Bluetooth
    /// sink needs its silence (`bridge`).
    fn idle_wait(&self) -> Duration {
        let wait = self.kept.as_ref().map_or(Duration::from_secs(1), |k| {
            k.until
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(10))
        });
        let feeding = self
            .kept
            .as_ref()
            .is_some_and(|k| keeps_queued_audio(&k.spec));
        if self.clicking.is_some() || feeding {
            wait.min(Duration::from_millis(10))
        } else {
            wait
        }
    }
    fn wired(&self) -> bool {
        (self.open_spec.as_ref()).is_some_and(|(spec, _)| matches!(spec.output, AudioOutput::Wired))
    }
    /// The stream ended or changed: no click sounds.
    fn forget_clicks(&mut self) {
        self.click.stop();
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
