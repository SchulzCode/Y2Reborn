//! The decode thread ("playback"): opens and decodes each job, crossfades and gapless, and streams PCM to the sink thread.
use super::*;

/// What the thread owns and shares with `Playback`.
pub(super) struct DecodeWorker {
    pub(super) dl: Observer,
    pub(super) dr: Receiver<DecodeCommand>,
    pub(super) pt: SyncSender<Stream>,
    pub(super) det: SyncSender<PlaybackEvent>,
    pub(super) de: Arc<AtomicU64>,
    pub(super) db: Arc<AtomicU64>,
    pub(super) decode_tempo: Arc<AtomicU64>,
    pub(super) decode_upcoming: Arc<Mutex<Option<Upcoming>>>,
    pub(super) media_state: Arc<Mutex<serde_json::Value>>,
    pub(super) cache: Arc<PathBuf>,
}

impl DecodeWorker {
    pub(super) fn run(self) {
        let DecodeWorker {
            dl,
            dr,
            det,
            de,
            decode_tempo,
            ..
        } = &self;
        loop {
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
            decode_tempo.store((job.dsp.tempo() * 1000.).round() as u64, Ordering::Release);
            // A station may wait up to 45 s for data while it reconnects
            // (media.c RB_STREAM_STALL_MS); that is not a stuck worker.
            let heartbeat_s = if job.track.is_radio() { 60 } else { 20 };
            dl.heartbeat("playback", heartbeat_s);
            let operation = self.play(&job, heartbeat_s);
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
                        det,
                        Event::PlaybackError {
                            generation: job.generation,
                            message: e,
                        },
                    );
                }
            }
        }
    }

    /// Play one job: open its track (and the queue after it), then decode, crossfade or join
    /// gaplessly and stream to the sink thread until the job ends or is replaced.
    fn play(&self, job: &Job, heartbeat_s: u64) -> Result<(), String> {
        let DecodeWorker {
            dl,
            pt,
            de,
            db,
            decode_upcoming,
            media_state,
            cache,
            ..
        } = self;
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
            media_state,
            &tracks[0],
            &decoder,
            job,
            processing_format,
            tracks.len() > 1,
        );
        dl.add("audio_filter_rebuilds", 1.);
        if decoder.metadata.resampling {
            dl.add("audio_resamples", 1.);
        }
        dl.add(
            "ffmpeg_corrupt_packets",
            decoder.metadata.corrupt_packets as f64,
        );
        // A live station has no position to return to.
        if job.position > 0 && !tracks[0].is_radio() {
            decoder.seek(job.position)?;
        }
        let artwork = if tracks[0].is_radio() {
            None
        } else {
            publish_artwork(cache, &tracks[0], &mut decoder)
        };
        if let Some(bytes) = artwork {
            send_stream_item(
                pt,
                db,
                de,
                job.generation,
                Stream::Artwork {
                    generation: job.generation,
                    entry_id: job.entry_id,
                    track_id: tracks[0].id,
                    bytes,
                },
                0,
                dl,
            )?;
        }
        let overlap_frames =
            (u64::from(job.spec.rate) * u64::from(job.dsp.crossfade_ms) / 1000) as usize;
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
            if let Some(next) = take_upcoming(decode_upcoming, job.generation) {
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
            if let Some(block) = read_pending_or_decoder(&mut decoder, &mut decoder_pending)? {
                let packet_delta = block.packets.saturating_sub(packet_count);
                packet_count = packet_count.max(block.packets);
                dl.add("ffmpeg_packets_decoded", packet_delta as f64);
                dl.add("ffmpeg_frames_decoded", block.frames as f64);
                let block_frames = block.frames;
                if crossfade_enabled && overlap_frames > 0 {
                    if let Some(ready) = tail.push(block, overlap_frames) {
                        let ready_frames = ready.frames;
                        send_stream(
                            pt,
                            db,
                            de,
                            job.generation,
                            Stream::Pcm {
                                generation: job.generation,
                                block: ready,
                            },
                            ready_frames,
                            dl,
                        )?;
                    }
                } else {
                    send_stream(
                        pt,
                        db,
                        de,
                        job.generation,
                        Stream::Pcm {
                            generation: job.generation,
                            block,
                        },
                        block_frames,
                        dl,
                    )?;
                }
                continue;
            }
            if next_decoder.is_none() {
                if crossfade_enabled {
                    if let Some(ready) = tail.take_all() {
                        let ready_frames = ready.frames;
                        send_stream(
                            pt,
                            db,
                            de,
                            job.generation,
                            Stream::Pcm {
                                generation: job.generation,
                                block: ready,
                            },
                            ready_frames,
                            dl,
                        )?;
                    }
                }
                send_stream(
                    pt,
                    db,
                    de,
                    job.generation,
                    Stream::End {
                        generation: job.generation,
                    },
                    0,
                    dl,
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
                self.crossfade(
                    job,
                    &tracks[next_index],
                    &mut next,
                    &mut next_pending,
                    &mut tail,
                    overlap_frames,
                )?;
            }
            send_stream(
                pt,
                db,
                de,
                job.generation,
                Stream::Boundary {
                    generation: job.generation,
                    next_entry_id: Some(entry_ids[next_index]),
                    next_track_id: Some(tracks[next_index].id),
                    transition_ms: transition_started.elapsed().as_millis() as u64,
                },
                0,
                dl,
            )?;
            if let Some(bytes) = publish_artwork(cache, &tracks[next_index], &mut next) {
                send_stream_item(
                    pt,
                    db,
                    de,
                    job.generation,
                    Stream::Artwork {
                        generation: job.generation,
                        entry_id: entry_ids[next_index],
                        track_id: tracks[next_index].id,
                        bytes,
                    },
                    0,
                    dl,
                )?;
            }
            if let Some((candidate, following)) = open_next(&tracks, next_index + 1) {
                preopened_index = Some(candidate);
                next_decoder = Some(following);
            }
            update_media_state(
                media_state,
                &tracks[next_index],
                &next,
                job,
                processing_format,
                next_decoder.is_some(),
            );
            dl.add("audio_filter_rebuilds", 1.);
            if next.metadata.resampling {
                dl.add("audio_resamples", 1.);
            }
            dl.add(
                "ffmpeg_corrupt_packets",
                next.metadata.corrupt_packets as f64,
            );
            decoder = next;
            current_index = next_index;
            packet_count = 0;
            decoder_pending = std::mem::take(&mut next_pending);
            next_pending = VecDeque::new();
            tail = TailWindow::new(job.spec.rate, processing_format);
        }
    }

    /// Crossfade into the next track: mix the end of this one (`tail`) with the start of the next,
    /// read from `next` (what is left over stays in `next_pending`), and stream the mixed blocks.
    fn crossfade(
        &self,
        job: &Job,
        next_track: &Track,
        next: &mut BoundDecoder,
        next_pending: &mut VecDeque<(Pcm, usize)>,
        tail: &mut TailWindow,
        overlap_frames: usize,
    ) -> Result<(), String> {
        let DecodeWorker { dl, pt, de, db, .. } = self;
        match read_window(next, next_pending, overlap_frames) {
            Ok(Some(head)) => {
                let fade_frames = tail.frames().min(head.frames as usize);
                let frame_bytes = head.format.bytes_per_frame();
                if tail.frames() > fade_frames {
                    if let Some(prefix) = tail.take_prefix(tail.frames() - fade_frames) {
                        let prefix_frames = prefix.frames;
                        send_stream(
                            pt,
                            db,
                            de,
                            job.generation,
                            Stream::Pcm {
                                generation: job.generation,
                                block: prefix,
                            },
                            prefix_frames,
                            dl,
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
                                pt,
                                db,
                                de,
                                job.generation,
                                Stream::Pcm {
                                    generation: job.generation,
                                    block,
                                },
                                block_frames,
                                dl,
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
                                json!({"track_id":next_track.id}),
                            );
                            let suffix_frames = suffix.frames;
                            send_stream(
                                pt,
                                db,
                                de,
                                job.generation,
                                Stream::Pcm {
                                    generation: job.generation,
                                    block: suffix,
                                },
                                suffix_frames,
                                dl,
                            )?;
                            /* The next-track prefix was already removed from
                             * the decoder window. If mixing fails, put that
                             * exact prefix back on the stream after the old
                             * suffix so no audible samples disappear. */
                            if fade_frames > 0 {
                                let prefix = Pcm {
                                    data: head.data[..fade_frames * frame_bytes].to_vec(),
                                    position_ms: head.position_ms,
                                    packets: head.packets,
                                    frames: fade_frames as u64,
                                    rate: head.rate,
                                    format: head.format,
                                };
                                let prefix_frames = prefix.frames;
                                send_stream(
                                    pt,
                                    db,
                                    de,
                                    job.generation,
                                    Stream::Pcm {
                                        generation: job.generation,
                                        block: prefix,
                                    },
                                    prefix_frames,
                                    dl,
                                )?;
                            }
                        }
                    }
                }
                if head.frames as usize > fade_frames {
                    let offset = fade_frames * frame_bytes;
                    push_front_block(
                        next_pending,
                        Pcm {
                            data: head.data[offset..].to_vec(),
                            position_ms: head.position_ms
                                + fade_frames as u64 * 1000 / u64::from(head.rate),
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
                        pt,
                        db,
                        de,
                        job.generation,
                        Stream::Pcm {
                            generation: job.generation,
                            block: ready,
                        },
                        ready_frames,
                        dl,
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
                                        json!({"track_id":next_track.id,"recovery":"old_suffix_then_consumed_next_prefix"}),
                                    );
                for ready in recover_window_failure(tail, e) {
                    let ready_frames = ready.frames;
                    send_stream(
                        pt,
                        db,
                        de,
                        job.generation,
                        Stream::Pcm {
                            generation: job.generation,
                            block: ready,
                        },
                        ready_frames,
                        dl,
                    )?;
                }
            }
        }
        Ok(())
    }
}
