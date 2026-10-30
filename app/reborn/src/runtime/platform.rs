//! The platform tasks and the snapshot polling.
use crate::*;

impl Runtime {
    /// Keep a task the owner started for the worker's next free turn; a
    /// periodic observation is simply skipped.
    pub(crate) fn queue_platform(&mut self, task: reborn_core::PlatformTask) {
        if task == reborn_core::PlatformTask::Refresh {
            return;
        }
        if self.queued_platform != Some(task) {
            self.log.emit(
                Level::Info,
                "platform",
                "task_queued",
                "Platform task waits for the one still running",
                None,
                json!({"task":format!("{task:?}"),"running":format!("{:?}", self.model.platform.busy)}),
            );
        }
        self.queued_platform = Some(task);
    }

    /// Start a queued task once the worker is free, and never let the update
    /// screen own the display with no update task behind it.
    pub(crate) fn platform_tick(&mut self) {
        if self.model.platform.busy.is_none() {
            if let Some(task) = self.queued_platform.take() {
                if let Err(error) = self.effect(Effect::Platform(task)) {
                    if matches!(
                        task,
                        reborn_core::PlatformTask::UpdateStage
                            | reborn_core::PlatformTask::UpdateApply
                    ) {
                        self.ui.update_run = false;
                    }
                    self.ui.flash(error);
                }
            }
        }
        let waiting = update_screen_waits(
            self.ui.update_run,
            self.ui.update_restarting,
            self.model.platform.busy,
            self.queued_platform,
        );
        if !waiting {
            self.update_idle_since = None;
            return;
        }
        let since = *self.update_idle_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= Duration::from_secs(30) {
            self.update_idle_since = None;
            self.ui.update_run = false;
            self.log.emit(
                Level::Warn,
                "update",
                "screen_without_task",
                "Update screen showed no running update task for 30 s",
                None,
                json!({"busy":format!("{:?}", self.model.platform.busy)}),
            );
            self.ui
                .flash("The update didn't start. Nothing was changed. Please try again.");
            self.dirty.mark_render();
        }
    }

    pub(crate) fn poll_platform(&mut self) {
        let Some(worker) = &self.platform else {
            return;
        };
        let Ok(reply) = worker.replies.try_recv() else {
            return;
        };
        let mut refresh = false;
        // The one-step update installs right after its download (Beta 7.1).
        let mut install_next = false;
        let mut restart_after_setup = false;
        let p = &mut self.model.platform;
        match reply {
            client::Reply::Sleep(result) => {
                p.busy = None;
                match result {
                    Ok(state) => {
                        // A refusal leaves the screen as it is: the request
                        // turned it off already, and lit now means a press
                        // woke it (F48: it went dark again ≈1 s after a wake).
                        self.sleep_refused_for_charger = state.phase
                            == reborn_core::platform::SleepPhase::SleepRefused
                            && state.problem
                                == Some(reborn_core::platform::SleepProblem::ChargerActive);
                        if state.phase == reborn_core::platform::SleepPhase::SleepRefused {
                            self.log.emit(
                                Level::Info,
                                "power",
                                "sleep_refused",
                                "Platform refused sleep",
                                None,
                                json!({"problem":format!("{:?}", state.problem)}),
                            );
                        }
                        p.snapshot.sleep = state;
                        p.failure = None;
                    }
                    Err(error) => {
                        p.snapshot.sleep.phase = reborn_core::platform::SleepPhase::SleepRefused;
                        p.snapshot.sleep.problem = Some(reborn_core::platform::SleepProblem::Other);
                        p.failure = Some(error.clone());
                        self.ui.flash(error);
                    }
                }
            }
            client::Reply::Snapshot(snapshot) => {
                self.platform_observed = true;
                self.platform_refresh_pending = false;
                let health = p.snapshot.health;
                let health_section = p.snapshot.section("health").cloned();
                let firmware_before = p.snapshot.radio_firmware;
                let sd_before = p.snapshot.storage.sd;
                p.snapshot = *snapshot;
                // Y2Linux 8.27: a card with errors plays read-only; say so once
                // when it arrives (Settings > Storage offers the repair).
                if matches!(
                    p.snapshot.storage.sd,
                    reborn_core::platform::SdCard::ReadOnly(_)
                ) && !matches!(sd_before, reborn_core::platform::SdCard::ReadOnly(_))
                {
                    self.ui.flash(
                        "SD card has errors · Playing only. Repair it in Settings > Storage",
                    );
                }
                // The status call took ≈1.6 s: the live cable state is newer.
                let usb = client::usb_now();
                if usb != reborn_core::platform::UsbTransfer::Unknown {
                    p.snapshot.usb = usb;
                }
                p.snapshot.health = health;
                {
                    use reborn_core::platform::RadioFirmware::*;
                    if matches!(firmware_before, Missing | Installing | Failed)
                        && p.snapshot.radio_firmware == Ready
                    {
                        self.ui.flash("Wi-Fi and Bluetooth are ready");
                    }
                }
                {
                    // Y2Linux STORAGE-SETUP: said once when it ran at this start;
                    // a real old library is asked about once (Settings > Storage
                    // keeps the choice).
                    use reborn_core::platform::InternalSetup;
                    match p.snapshot.internal_setup {
                        InternalSetup::Done { kept } if !self.announced_setup => {
                            self.announced_setup = true;
                            self.ui.flash(match kept {
                                0 => "Internal Storage is ready".to_owned(),
                                1 => "Internal Storage is ready · Kept 1 file in Old Player Files"
                                    .to_owned(),
                                n => format!(
                                    "Internal Storage is ready · Kept {n} files in Old Player Files"
                                ),
                            });
                        }
                        InternalSetup::Waiting { .. }
                            if !self.model.settings.internal_setup_asked
                                && self.model.navigation.modal.is_none() =>
                        {
                            self.model.settings.internal_setup_asked = true;
                            self.model.navigation.modal = Some(reborn_core::Modal::Confirm(
                                reborn_core::ConfirmAction::Platform(
                                    reborn_core::PlatformTask::InternalStorageSetup,
                                ),
                            ));
                            self.model.navigation.modal_focus = 0;
                            self.dirty.mark_both();
                        }
                        _ => {}
                    }
                }
                p.snapshot.diagnostics.extend(health_section);
                if let reborn_core::platform::UpdatePhase::Available { version } =
                    &p.snapshot.update.phase
                {
                    if self.announced_update.as_ref() != Some(version) {
                        self.announced_update = Some(version.clone());
                        self.ui.flash("Software update available");
                        self.dirty.mark_render();
                    }
                }
                if p.busy == Some(reborn_core::PlatformTask::Refresh) {
                    p.busy = None;
                }
            }
            client::Reply::Reports(task, result) => {
                use reborn_core::PlatformTask::*;
                p.busy = None;
                p.report = None;
                match result {
                    Ok(state) => {
                        let enabled = state.enabled;
                        // The platform sends a new report at once when it can.
                        let sent = state.last.as_ref().is_some_and(|l| l.state == "sent");
                        p.snapshot.reports = Some(state);
                        p.failure = None;
                        self.ui.flash(match task {
                            ReportEnable(true) => "Error reports are on".to_owned(),
                            ReportEnable(false) => "Error reports are off".to_owned(),
                            ReportNewId => "New install ID".to_owned(),
                            ReportSend if sent => "Report sent".to_owned(),
                            ReportSend if enabled == Some(true) => {
                                "Report saved · waiting to send".to_owned()
                            }
                            ReportSave(to) => format!("Report saved to {}", to.label()),
                            _ => "Done".to_owned(),
                        });
                    }
                    Err(error) => {
                        self.ui.flash(error.clone());
                        p.failure = Some(error);
                    }
                }
            }
            client::Reply::Health(level, section) => {
                p.busy = None;
                p.snapshot.health = level;
                p.snapshot.diagnostics.retain(|s| s.id != "health");
                p.snapshot.diagnostics.push(section);
            }
            client::Reply::Operation(task, result) => {
                if matches!(
                    task,
                    reborn_core::PlatformTask::UpdateStage | reborn_core::PlatformTask::UpdateApply
                ) {
                    self.log.emit(
                        if result.is_ok() {
                            Level::Info
                        } else {
                            Level::Warn
                        },
                        "update",
                        "task_finished",
                        "Update task answered",
                        None,
                        json!({"task":format!("{task:?}"),"error":result.as_ref().err()}),
                    );
                }
                use reborn_core::PlatformTask::*;
                p.busy = None;
                refresh = true;
                match result {
                    Ok(result) => {
                        if task == UpdateStage {
                            p.download = None;
                        }
                        if !result.succeeded {
                            p.failure =
                                Some("The operation did not finish. See Latest Result.".into());
                            p.result = Some(result);
                            self.ui
                                .flash("The operation did not finish. See Latest Result.");
                            self.dirty.mark_render();
                            return;
                        }
                        p.failure = None;
                        p.result = Some(result);
                        match task {
                            UpdateStage => {
                                // Ready at once, wherever the owner is; the
                                // refresh that follows confirms the journal.
                                let u = &mut p.snapshot.update;
                                u.phase = reborn_core::platform::UpdatePhase::ReadyToInstall;
                                u.can_install = true;
                                u.can_cancel = true;
                                u.can_download = false;
                                if self.ui.update_run {
                                    install_next = true;
                                } else {
                                    self.ui.flash("Update downloaded. Ready to install.");
                                }
                            }
                            UpdateApply if self.ui.update_run => {
                                // The platform restarts into the installer.
                                self.ui.update_restarting = true;
                            }
                            UpdateCheck => {
                                // The check's own answer, at once: no wait for
                                // the status refresh behind it (Beta 7.1).
                                use reborn_core::platform::UpdatePhase;
                                let fact = |label: &str| {
                                    p.result.as_ref().and_then(|r| {
                                        r.facts
                                            .iter()
                                            .find(|f| f.label == label)
                                            .map(|f| f.value.clone())
                                    })
                                };
                                let (state, release) = (fact("State"), fact("Release"));
                                let u = &mut p.snapshot.update;
                                if state.as_deref() == Some("UpToDate") {
                                    u.phase = UpdatePhase::UpToDate;
                                    u.can_download = false;
                                } else if let Some(version) = release {
                                    u.phase = UpdatePhase::Available { version };
                                    u.can_download = true;
                                }
                            }
                            UpdateCancel => {}
                            Export => self.ui.flash("Player data exported"),
                            LdacQuality(_) | LdacAbr(_) | SbcQuality(_) | AacQuality(_)
                            | AacAbr(_) => {
                                self.ui.flash("Quality saved. Restart the player to apply.")
                            }
                            DiagnosticsExport => {
                                self.ui.flash("Diagnostic report saved. See Latest Result.");
                                self.ui.show_operation_result(&mut self.model);
                            }
                            InternalStorageSetup => {
                                // Library, USB and folders start again on the new
                                // Internal Storage.
                                self.ui.flash("Internal Storage is ready. Restarting…");
                                restart_after_setup = true;
                            }
                            SdRepair => {
                                let shortened = p
                                    .result
                                    .as_ref()
                                    .and_then(|r| {
                                        r.facts.iter().find(|f| f.label == "Files shortened")
                                    })
                                    .and_then(|f| f.value.parse::<u64>().ok())
                                    .unwrap_or(0);
                                self.ui.flash(match shortened {
                                    0 => "SD card repaired".to_owned(),
                                    1 => {
                                        "SD card repaired · 1 damaged file was shortened".to_owned()
                                    }
                                    n => format!(
                                        "SD card repaired · {n} damaged files were shortened"
                                    ),
                                });
                            }
                            SetClock(_) => {
                                // A time set by hand is never trusted: read
                                // the platform's verdict again at once.
                                self.clock_trusted = None;
                                self.ui.flash("Date and time set");
                            }
                            _ => self.ui.flash("Check finished. See Latest Result."),
                        }
                    }
                    Err(error) => {
                        // The update page presents its own journaled reason.
                        if matches!(task, UpdateStage | UpdateApply) {
                            self.ui.update_run = false;
                            self.ui.update_restarting = false;
                        }
                        if matches!(task, UpdateApply | UpdateRollback) {
                            self.ui.flash(install_refusal(&error));
                        } else if task == UpdateStage && error.contains("download_cancelled") {
                            p.download = None;
                            self.ui.flash("Update cancelled. Nothing was changed.");
                        } else if task == UpdateStage {
                            p.download = None;
                            self.ui
                                .flash("The download didn't finish. Nothing was changed.");
                        } else if task == UpdateCheck {
                            // The page keeps a release it already offered.
                            self.ui
                                .flash("Couldn't reach the update server. Try again in a moment.");
                        } else {
                            self.ui.flash(error.clone());
                        }
                        p.failure = Some(error);
                    }
                }
            }
            client::Reply::Failed(task, error) => {
                if task == reborn_core::PlatformTask::UpdateStage {
                    p.download = None;
                }
                if task == reborn_core::PlatformTask::Refresh {
                    self.platform_refresh_pending = false;
                    // A stale observation cannot keep a control enabled.
                    p.snapshot.observed = false;
                    p.snapshot.update = Default::default();
                } else {
                    self.ui.flash(error.clone());
                }
                if p.busy == Some(task) {
                    p.busy = None;
                }
                p.failure = Some(error);
            }
        }
        if restart_after_setup {
            if let Err(error) = self.effect(Effect::Reboot) {
                self.ui.flash(error);
            }
        }
        if install_next {
            if let Err(error) =
                self.effect(Effect::Platform(reborn_core::PlatformTask::UpdateApply))
            {
                self.ui.update_run = false;
                self.ui.flash(error);
            }
            self.dirty.mark_render();
            return;
        }
        // Operations change platform state; observe it again.
        if refresh {
            if let Some(worker) = &self.platform {
                if worker
                    .commands
                    .try_send(reborn_core::PlatformTask::Refresh)
                    .is_ok()
                {
                    self.platform_refresh_pending = true;
                }
            }
        }
        self.dirty.mark_render();
    }
}
