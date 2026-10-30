//! The sink thread ("audio"): opens, keeps and switches the sink and writes the PCM it is sent, with volume, balance, hold and the level meter.
use super::*;

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
    pub(super) audio_state: Arc<Mutex<serde_json::Value>>,
    pub(super) meter: crate::meter::Meter,
    pub(super) factory: SinkFactory,
}

impl SinkWorker {
    pub(super) fn run(self) {
        let SinkWorker {
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
            audio_state,
            mut meter,
            factory,
        } = self;
        {
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
                                                serde_json::to_value(&params).unwrap_or_default(),
                                            );
                                            object.insert("output_rate".into(), json!(params.rate));
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
                    if let Some(k) = keep_sink(sink.take(), open_spec.take(), &al, correlation) {
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
                            if g == generation && g == se.load(Ordering::Acquire) && sink.is_some()
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
                    if let Some(k) = keep_sink(sink.take(), open_spec.take(), &al, correlation) {
                        kept = Some(k);
                    }
                    eof = false;
                }
                let n = ab.load(Ordering::Relaxed)
                    + pending
                        .as_ref()
                        .map(|(b, o)| (b.data.len() - o) as u64 / b.format.bytes_per_frame() as u64)
                        .unwrap_or(0)
                    + sink.as_ref().map(|s| s.delay()).unwrap_or(0);
                al.gauge("audio_buffer_frames", n as f64);
                al.gauge("audio_buffer_ms", n as f64 * 1000. / f64::from(rate));
            }
        }
    }
}
