//! The platform tasks the UI asks for.
use crate::*;

impl Runtime {
    /// `Effect::Platform`.
    pub(crate) fn on_platform(&mut self, task: reborn_core::PlatformTask) -> Result<(), String> {
        if task == reborn_core::PlatformTask::SleepRequest
            && !matches!(
                self.model.playback,
                PlaybackState::Playing | PlaybackState::Buffering
            )
        {
            // F48: Power turns the screen off at once, as on stock. It
            // stayed lit ≈1.7 s until the platform prepared, people
            // pressed again, and that press cancelled the sleep.
            self.effect_inner(Effect::ScreenSleep)?;
            self.sleep_requested_at = Some(Instant::now());
        }
        if self.model.platform.busy.is_some() {
            self.queue_platform(task);
            return Ok(());
        }
        if task == reborn_core::PlatformTask::InternalStorageSetup
            && matches!(
                self.model.platform.snapshot.usb,
                reborn_core::platform::UsbTransfer::Ready
                    | reborn_core::platform::UsbTransfer::Copying
                    | reborn_core::platform::UsbTransfer::Starting
            )
        {
            return Err("Unplug from the computer first".into());
        }
        if task == reborn_core::PlatformTask::SdRepair {
            // The card is unmounted for the repair: nothing may hold a
            // song or its folder open on it.
            let on_card = self.model.current().is_some_and(|t| {
                self.model.sources.iter().any(|s| {
                    s.id == t.source_id && matches!(s.kind, reborn_core::MediaSource::SdCard(_))
                })
            });
            if on_card {
                if self.model.playback == PlaybackState::Playing {
                    self.pause();
                }
                self.release_hold_with(true);
                self.playback.stop(self.model.generation);
            }
        }
        if task == reborn_core::PlatformTask::SleepRequest {
            self.sleep_attempt = Some(Instant::now());
            use reborn_core::platform::{SleepPhase, SleepProblem, SleepState};
            if matches!(
                self.model.playback,
                PlaybackState::Playing | PlaybackState::Buffering
            ) {
                self.model.platform.snapshot.sleep = SleepState {
                    phase: SleepPhase::SleepRefused,
                    problem: Some(SleepProblem::PlaybackActive),
                    ..Default::default()
                };
                return Ok(());
            }
            // A pause held in place keeps the sink, and its activity
            // lease, open: give it back first so the platform's lock
            // check passes (Play then restarts at the same place).
            self.release_hold_with(true);
            // So does a sink kept after a song or for wheel clicks (#77).
            if let Err(error) = self.playback.stop_and_wait(self.model.generation) {
                self.log.emit(
                    Level::Warn,
                    "playback",
                    "sleep_sink_release_slow",
                    "Kept sink did not close in time",
                    None,
                    json!({"error":error}),
                );
            }
            self.checkpoint();
        }
        let worker = self.platform.as_ref().ok_or("System service is starting")?;
        if worker.commands.try_send(task).is_err() {
            // An observation runs and another waits: take the next turn.
            if task == reborn_core::PlatformTask::Refresh {
                return Err("busy: another operation is still running".into());
            }
            self.queue_platform(task);
            return Ok(());
        }
        if matches!(
            task,
            reborn_core::PlatformTask::UpdateStage | reborn_core::PlatformTask::UpdateApply
        ) {
            self.log.emit(
                Level::Info,
                "update",
                "task_started",
                "Update task sent to the platform",
                None,
                json!({"task":format!("{task:?}")}),
            );
        }
        self.model.platform.busy = Some(task);
        if task == reborn_core::PlatformTask::SleepRequest {
            self.model.platform.snapshot.sleep.phase =
                reborn_core::platform::SleepPhase::SleepRequested;
        }
        if task != reborn_core::PlatformTask::Refresh {
            self.model.platform.failure = None;
            self.model.platform.result = None;
        }
        self.dirty.mark_render();
        Ok(())
    }
}
