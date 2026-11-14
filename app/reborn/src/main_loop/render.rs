//! Presenting the existing UI frame and completing the boot handoff.
use super::*;

impl MainLoop {
    /// Drawing and presenting a frame.
    pub(super) fn render(&mut self, rt: &mut Runtime) {
        if rt.dirty.render
            && !rt.model.screen_off
            && self.render_time.elapsed() >= frame_gap(rt.ui.gliding())
        {
            self.render_time = Instant::now();
            let frame_started = Instant::now();
            let tracks = std::mem::take(&mut rt.model.library.tracks);
            rt.ui.normalize(&mut rt.model, &tracks);
            rt.model.library.tracks = tracks;
            if rt.model.screen == Screen::DiagnosticSection && rt.model.navigation.filter == "audio"
            {
                rt.model.platform.audio_facts = audio_facts(&rt.playback.audio_state(), &rt.model);
            }
            if let Some(g) = &rt.graphics {
                rt.ui.set_resident_thumbs(g.resident_thumbs());
            }
            // The playing wave moves with what is heard (Beta 8.38).
            let heard = (rt.model.playback == PlaybackState::Playing)
                .then(|| rt.playback.heard())
                .flatten();
            rt.ui.set_heard(heard);
            let mut draw = rt.ui.draw(&rt.model, &rt.model.library.tracks, rt.art);
            let layout_ms = frame_started.elapsed().as_secs_f64() * 1000.;
            self.log.gauge("ui_layout_ms", layout_ms);
            if !self.first_frame_presented {
                // The splash's bar completes while this first frame renders.
                reborn_platform::contract::boot_milestone("ready");
            }
            let fading = rt.boot_fade > 0;
            if fading {
                // The first frame is the complete boot screen over the real UI
                // (identical to the splash's last frame); it dissolves at once.
                // A step is only consumed once its frame was presented.
                draw = reborn_ui::boot_transition(
                    draw,
                    f32::from(rt.boot_fade) / f32::from(reborn_ui::BOOT_FADE_FRAMES),
                );
            }
            if rt.graphics.is_none() && fading {
                rt.boot_fade -= 1;
            }
            let mut presentation_failed = false;
            if let Some(g) = &mut rt.graphics {
                if let Err(e) = g.render(&draw) {
                    if !rt.retry_after_wake(&e) {
                        presentation_failed = true;
                        rt.fail("graphics", e);
                        rt.graphics = None;
                        // One bounded recreation attempt. Further attempts wait for an explicit wake.
                        if rt.graphics_recovery.failed() {
                            if let Err(error) = rt.recreate_graphics() {
                                rt.fail("graphics", error);
                            }
                        }
                        let _ =
                            self.log
                                .diagnostic(&rt.root.join("diagnostics"), rt.snapshot(), true);
                    }
                } else {
                    rt.graphics_recovery.presented();
                    if self.performance.enabled() {
                        self.performance.record(
                            "frame",
                            json!({
                                "screen": rt.model.screen, "focus": rt.model.navigation.focus,
                                "quads": draw.len(), "layout_ms": layout_ms,
                                "frame_ms": frame_started.elapsed().as_secs_f64() * 1000.,
                                "render": g.timings
                            }),
                        );
                    }
                    self.captured_frame();
                    if fading {
                        rt.boot_fade -= 1;
                    }
                    if rt.boot_fade == 0 && !self.first_interactive_presented {
                        self.first_interactive_presented = true;
                        self.log.emit(
                            Level::Info,
                            "startup",
                            "interactive",
                            "Boot cover fully revealed; input enabled",
                            None,
                            json!({"elapsed_ms":self.process_started.elapsed().as_millis()}),
                        );
                    }
                    if !self.first_frame_presented {
                        self.first_frame_presented = true;
                        if !rt.headless {
                            // The UI is up with the screen on: the DAC stays
                            // powered until the screen sleeps.
                            if !rt.model.screen_off {
                                hold_dac(&self.log, true);
                            }
                            if let Err(error) = reborn_platform::contract::application_ready() {
                                self.log.emit(
                                    Level::Warn,
                                    "platform",
                                    "readiness",
                                    &error,
                                    None,
                                    json!({}),
                                );
                            }
                        }
                        self.log.emit(
                            Level::Info,
                            "startup",
                            "ready",
                            "First Reborn frame presented",
                            None,
                            json!({"display_handoff":"explicit KMS presentation",
                                "elapsed_ms":self.process_started.elapsed().as_millis()}),
                        );
                        // Early splash evidence lives on the /run mount carried out
                        // of initramfs; retain this bounded record alongside app logs.
                        if let Some(bytes) = reborn_platform::contract::splash_evidence() {
                            let _ = reborn_core::atomic_write(
                                &rt.root.join("logs/splash-boot.jsonl"),
                                &bytes,
                            );
                        }
                    }
                }
            }
            if rt.boot_fade == 0 && !presentation_failed {
                rt.dirty.rendered();
            }
        }
    }
}
