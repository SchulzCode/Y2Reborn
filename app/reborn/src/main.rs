#![forbid(unsafe_code)]
mod diagnostics;
mod playback;
use reborn_control::{Command, PlaybackAction, Response};
use reborn_core::{
    AppModel, AudioOutput, Effect, Event, PlaybackState, QueueEntryId, RepeatMode, Screen, Source,
    Track,
};
use reborn_graphics::Renderer;
use reborn_library::{Database, Filter, Scanner};
use reborn_observability::{HealthState, Level, Observer};
use reborn_platform::{bluetooth, input, power, storage, wifi};
use reborn_ui::{Item, PowerView, Ui};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        mpsc::{Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
struct Runtime {
    model: AppModel,
    ui: Ui,
    playback: playback::Playback,
    log: Observer,
    db: Database,
    scanner: Scanner,
    wifi: Option<wifi::Wifi>,
    bluetooth: Option<bluetooth::Bluetooth>,
    wifi_state: wifi::Status,
    bt_state: bluetooth::Status,
    graphics: Option<Renderer>,
    power: Value,
    art: bool,
    dirty: DirtyState,
    root: PathBuf,
    headless: bool,
    last_activity: Instant,
    query: Option<Receiver<Result<Vec<Track>, String>>>,
    action_depth: usize,
    action_before: Option<AppModel>,
    action_dirty_before: Option<DirtyState>,
    pending_reconfiguration: Option<PendingReconfiguration>,
    reconfiguration_attempted: bool,
    sink_release_started: bool,
    transition_generation: Option<u64>,
    active_transport_generation: Option<u64>,
}

struct PendingReconfiguration {
    previous: AppModel,
    dirty_before: DirtyState,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransportEpochState {
    Current,
    Changed,
    Unavailable,
}

fn transport_epoch_state(
    output: &AudioOutput,
    playback: PlaybackState,
    active_generation: Option<u64>,
    status: &bluetooth::Status,
) -> TransportEpochState {
    let AudioOutput::Bluetooth(address) = output else {
        return TransportEpochState::Current;
    };
    if !matches!(playback, PlaybackState::Playing | PlaybackState::Buffering) {
        return TransportEpochState::Current;
    }
    let Some(active_generation) = active_generation else {
        return TransportEpochState::Current;
    };
    match status.playback_pcm(address) {
        Ok(pcm) if pcm.transport_generation == active_generation => TransportEpochState::Current,
        Ok(pcm) if pcm.transport_generation != 0 => TransportEpochState::Changed,
        _ => TransportEpochState::Unavailable,
    }
}

#[derive(Default, Clone, Copy)]
struct DirtyState {
    render: bool,
    persistence: bool,
}

impl DirtyState {
    fn mark_render(&mut self) {
        self.render = true;
    }

    fn mark_persistence(&mut self) {
        self.persistence = true;
    }

    fn mark_both(&mut self) {
        self.mark_render();
        self.mark_persistence();
    }

    fn rendered(&mut self) {
        self.render = false;
    }

    fn checkpoint_succeeded(&mut self) {
        self.persistence = false;
    }

    fn checkpoint_failed(&mut self) {
        self.persistence = true;
    }

    fn checkpoint_due(&self) -> bool {
        self.persistence
    }
}

fn run_sink_reconfiguration<P>(
    playback: &mut playback::Playback,
    generation: u64,
    plan: impl FnOnce() -> Result<P, String>,
    start: impl FnOnce(&mut playback::Playback, P) -> Result<(), String>,
) -> Result<(), String> {
    playback.stop_and_wait(generation)?;
    let plan = plan()?;
    start(playback, plan)
}

fn failed_reconfiguration_model(
    mut previous: AppModel,
    error: &str,
    sink_release_started: bool,
    failed_generation: Option<u64>,
) -> AppModel {
    if !sink_release_started {
        return previous;
    }
    let was_active = matches!(
        previous.playback,
        PlaybackState::Playing | PlaybackState::Buffering
    );
    let had_current = previous.current().is_some();
    previous.playback = if was_active && had_current {
        PlaybackState::Error
    } else if !had_current {
        PlaybackState::Stopped
    } else {
        previous.playback
    };
    previous.last_error = Some(error.to_owned());
    previous.generation = failed_generation
        .map(|generation| generation.wrapping_add(1))
        .unwrap_or_else(|| previous.generation.wrapping_add(1));
    previous
}

fn artwork_matches_current(
    model: &AppModel,
    generation: u64,
    entry_id: QueueEntryId,
    track_id: i64,
) -> bool {
    generation == model.generation
        && model.current_entry_id() == Some(entry_id)
        && model.current().is_some_and(|track| track.id == track_id)
}

impl Runtime {
    fn with_model_action(
        &mut self,
        action: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<(), String> {
        let outermost = self.action_depth == 0;
        if outermost {
            if self.pending_reconfiguration.is_some() {
                self.ui.notice = "Audio output is still starting".into();
                self.dirty.mark_render();
                return Err("audio reconfiguration is still pending".into());
            }
            self.action_before = Some(self.model.clone());
            self.action_dirty_before = Some(self.dirty);
            self.reconfiguration_attempted = false;
            self.sink_release_started = false;
            self.transition_generation = None;
        }
        self.action_depth += 1;
        let result = action(self);
        self.action_depth -= 1;
        if outermost {
            let before = self.action_before.take().expect("outer action snapshot");
            let dirty_before = self
                .action_dirty_before
                .take()
                .expect("outer action dirty snapshot");
            let attempted = self.reconfiguration_attempted;
            let release_started = self.sink_release_started;
            let generation = self.transition_generation;
            let start_pending = self.playback.has_pending_start();
            self.reconfiguration_attempted = false;
            self.sink_release_started = false;
            self.transition_generation = None;
            if result.is_ok() && attempted && start_pending {
                self.pending_reconfiguration = Some(PendingReconfiguration {
                    previous: before,
                    dirty_before,
                    generation: generation.unwrap_or(self.model.generation),
                });
            } else if let Err(error) = &result {
                if attempted {
                    self.restore_failed_reconfiguration(
                        before,
                        dirty_before,
                        error,
                        release_started,
                        generation,
                    );
                }
            }
        }
        result
    }

    fn restore_failed_reconfiguration(
        &mut self,
        previous: AppModel,
        dirty_before: DirtyState,
        error: &str,
        sink_release_started: bool,
        transition_generation: Option<u64>,
    ) {
        if !sink_release_started {
            self.model = previous;
            self.dirty = dirty_before;
            return;
        }
        self.model = failed_reconfiguration_model(previous, error, true, transition_generation);
        self.playback.stop(self.model.generation);
        self.active_transport_generation = None;
        self.art = false;
        self.dirty.mark_both();
        self.checkpoint();
    }
    fn poll_pending_reconfiguration(&mut self) {
        if self.pending_reconfiguration.is_none() {
            return;
        }
        let epoch_state = transport_epoch_state(
            &self.model.output,
            self.model.playback,
            self.active_transport_generation,
            &self.bt_state,
        );
        if epoch_state != TransportEpochState::Current {
            let pending = self.pending_reconfiguration.take().unwrap();
            let error = match epoch_state {
                TransportEpochState::Changed => "Bluetooth transport changed during sink open",
                TransportEpochState::Unavailable => {
                    "Bluetooth transport disappeared during sink open"
                }
                TransportEpochState::Current => unreachable!(),
            };
            self.restore_failed_reconfiguration(
                pending.previous,
                pending.dirty_before,
                error,
                true,
                Some(pending.generation),
            );
            self.ui.notice = error.into();
            self.dirty.mark_render();
            return;
        }
        let Some(result) = self.playback.poll_start() else {
            return;
        };
        let Some(pending) = self.pending_reconfiguration.take() else {
            if let Err(error) = result {
                self.fail("playback", error);
            }
            return;
        };
        match result {
            Ok(()) => {
                if self.dirty.checkpoint_due() {
                    self.checkpoint();
                }
            }
            Err(error) => {
                self.restore_failed_reconfiguration(
                    pending.previous,
                    pending.dirty_before,
                    &error,
                    true,
                    Some(pending.generation),
                );
                self.ui.notice = format!("Playback could not start: {error}");
                self.dirty.mark_render();
            }
        }
    }
    fn abandon_pending_reconfiguration(&mut self) {
        if let Some(pending) = self.pending_reconfiguration.take() {
            self.model = pending.previous;
            self.dirty = pending.dirty_before;
        }
    }

    fn play_index(&mut self, index: usize, force_shuffle: bool) -> Result<(), String> {
        self.play_members(
            index,
            (0..self.model.library.tracks.len()).collect(),
            force_shuffle,
        )
    }

    fn play_members(
        &mut self,
        index: usize,
        members: Vec<usize>,
        force_shuffle: bool,
    ) -> Result<(), String> {
        let selected = self
            .model
            .library
            .tracks
            .get(index)
            .cloned()
            .ok_or("invalid selection")?;
        if !selected.online {
            return Err("media source offline".into());
        }
        let mut queue = members
            .into_iter()
            .filter_map(|member| self.model.library.tracks.get(member).cloned())
            .filter(|track| track.online)
            .collect::<Vec<_>>();
        let selected_position = queue
            .iter()
            .position(|track| track.id == selected.id && track.path == selected.path)
            .ok_or("selected track is not in the playback collection")?;
        let use_shuffle = force_shuffle || self.model.settings.shuffle;
        if use_shuffle {
            let selected = queue.remove(selected_position);
            let mut seed = selected.id.unsigned_abs().wrapping_add(index as u64 + 1);
            for i in (1..queue.len()).rev() {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let j = (seed as usize % (i + 1)).min(i);
                queue.swap(i, j);
            }
            queue.insert(0, selected);
            self.model.replace_queue(queue, 0)?;
        } else {
            let selected_index = selected_position;
            self.model.replace_queue(queue, selected_index)?;
        }
        self.load()
    }

    fn fail(&mut self, sub: &str, error: String) {
        let friendly = friendly_error(&error);
        match self.model.screen {
            Screen::Wifi | Screen::SettingsWifi if sub == "ui" => {
                self.ui.wifi.failed(friendly.clone())
            }
            Screen::Bluetooth | Screen::SettingsBluetooth if sub == "ui" => {
                self.ui.bluetooth.failed(friendly.clone())
            }
            _ => self.ui.notice = friendly,
        }
        self.log.emit(
            Level::Error,
            sub,
            "operation_failed",
            &error,
            None,
            json!({"state":self.model.playback,"recovery_attempted":false}),
        );
        self.dirty.mark_render();
    }
    fn checkpoint(&mut self) {
        self.dirty.mark_persistence();
        if self.playback.has_pending_start() || self.pending_reconfiguration.is_some() {
            return;
        }
        if let Err(e) = self.model.checkpoint(&self.root.join("state/session.json")) {
            self.dirty.checkpoint_failed();
            self.log.emit(
                Level::Warn,
                "core",
                "checkpoint_failed",
                &e.to_string(),
                None,
                json!({}),
            );
        } else {
            self.dirty.checkpoint_succeeded();
        }
    }
    fn load(&mut self) -> Result<(), String> {
        self.reconfiguration_attempted = true;
        let track = self.model.current().cloned().ok_or("queue is empty")?;
        if !track.path.is_file() {
            return Err("track source unavailable".into());
        }
        let output = self.model.output.clone();
        let bluetooth_pcm = match &output {
            AudioOutput::Bluetooth(address) => Some(self.bt_state.playback_pcm(address)?),
            AudioOutput::Wired => None,
        };
        let requested_rate = match bluetooth_pcm.as_ref() {
            Some(pcm) => pcm.negotiated_rate()?,
            None if track.sample_rate >= 8_000 => track.sample_rate,
            None => self.output_rate()?,
        };
        let generation = self.model.generation.wrapping_add(1);
        self.transition_generation = Some(generation);
        let entry_id = self
            .model
            .current_entry_id()
            .unwrap_or(QueueEntryId(self.model.queue_position as u64 + 1));
        let (queue, queue_entry_ids) = if self.model.settings.repeat == RepeatMode::Track {
            (Vec::new(), Vec::new())
        } else {
            (
                self.model
                    .queue
                    .iter()
                    .skip(self.model.queue_position + 1)
                    .cloned()
                    .collect(),
                self.model
                    .queue_entry_ids
                    .iter()
                    .skip(self.model.queue_position + 1)
                    .copied()
                    .collect(),
            )
        };
        let dsp = reborn_media::DspConfig {
            volume: self.model.settings.volume,
            replay_gain: self.model.settings.replay_gain,
            eq_enabled: self.model.settings.eq_enabled,
            eq_bands: self.model.settings.eq_bands.clone(),
            crossfade_ms: self.model.settings.crossfade_ms,
        };
        let request_parts = (
            track,
            entry_id,
            queue,
            queue_entry_ids,
            self.model.position_ms,
            generation,
            dsp,
            self.model.settings.gapless_enabled,
            self.log.correlation(),
        );
        let log = self.log.clone();
        let correlation = self.log.correlation();
        let plan_output = output.clone();
        self.sink_release_started = true;
        let playback = &mut self.playback;
        run_sink_reconfiguration(
            playback,
            generation,
            || {
                let plan = |rate| match bluetooth_pcm.as_ref() {
                    Some(pcm) => reborn_audio::AlsaSink::plan_bluetooth(
                        &plan_output,
                        pcm,
                        log.clone(),
                        correlation,
                    ),
                    None => {
                        reborn_audio::AlsaSink::plan(&plan_output, rate, log.clone(), correlation)
                    }
                };
                match plan(requested_rate) {
                    Ok((spec, _planned)) => Ok(spec),
                    Err(error)
                        if matches!(&plan_output, AudioOutput::Wired)
                            && requested_rate != 44_100 =>
                    {
                        log.emit(
                            Level::Warn,
                            "audio",
                            "qualified_rate_fallback",
                            "Source rate is not in the current wired qualification profile",
                            Some(correlation),
                            json!({"source_rate":requested_rate,"selected_rate":44100,"reason":error}),
                        );
                        plan(44_100).map(|(spec, _planned)| spec)
                    }
                    Err(error) => Err(error),
                }
            },
            move |playback, spec| {
                let (
                    track,
                    entry_id,
                    queue,
                    queue_entry_ids,
                    position,
                    generation,
                    dsp,
                    gapless_enabled,
                    id,
                ) = request_parts;
                playback.begin_start_with_gapless(playback::LoadRequest {
                    track,
                    entry_id,
                    queue,
                    queue_entry_ids,
                    position,
                    spec,
                    generation,
                    dsp,
                    gapless_enabled,
                    id,
                })
            },
        )?;
        self.model.generation = generation;
        self.model.playback = PlaybackState::Buffering;
        self.active_transport_generation =
            bluetooth_pcm.as_ref().map(|pcm| pcm.transport_generation);
        self.art = false;
        self.dirty.mark_both();
        Ok(())
    }
    fn output_rate(&self) -> Result<u32, String> {
        match &self.model.output {
            AudioOutput::Wired => Ok(44100),
            AudioOutput::Bluetooth(address) => self.bt_state.playback_rate(address),
        }
    }
    fn pause(&mut self) {
        self.abandon_pending_reconfiguration();
        self.log.emit(
            Level::Info,
            "playback",
            "paused",
            "Playback paused and sink released",
            None,
            json!({"position_ms":self.model.position_ms}),
        );
        self.model.invalidate();
        self.playback.stop(self.model.generation);
        self.active_transport_generation = None;
        self.model.playback = PlaybackState::Paused;
        self.dirty.mark_both();
        self.checkpoint();
    }
    fn fail_active_transport(&mut self, error: &str) {
        self.abandon_pending_reconfiguration();
        self.model.invalidate();
        self.playback.stop(self.model.generation);
        self.active_transport_generation = None;
        self.model.playback = PlaybackState::Error;
        self.model.last_error = Some(error.to_owned());
        self.art = false;
        self.dirty.mark_both();
        self.log.emit(
            Level::Error,
            "bluetooth",
            "transport_invalidated",
            error,
            None,
            json!({"recovery":"stopped_without_sink"}),
        );
        self.checkpoint();
    }
    fn toggle(&mut self) -> Result<(), String> {
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.pause();
            Ok(())
        } else {
            self.load()
        }
    }

    fn finish_track(&mut self) -> Result<(), String> {
        self.with_model_action(Self::finish_track_inner)
    }
    fn finish_track_inner(&mut self) -> Result<(), String> {
        match self.model.settings.repeat {
            reborn_core::RepeatMode::Track => {
                self.model.position_ms = 0;
                self.load()
            }
            reborn_core::RepeatMode::All => {
                if self.model.queue.is_empty() {
                    return Ok(());
                }
                if self.model.queue_position + 1 < self.model.queue.len() {
                    self.model.step(1);
                } else {
                    self.model.queue_position = 0;
                    self.model.position_ms = 0;
                    self.model.invalidate();
                }
                self.load()
            }
            reborn_core::RepeatMode::Off => {
                if self.model.queue_position + 1 < self.model.queue.len() {
                    self.model.step(1);
                    self.load()
                } else {
                    Ok(())
                }
            }
        }
    }
    fn switch(&mut self, out: AudioOutput) -> Result<(), String> {
        if let AudioOutput::Bluetooth(address) = &out {
            self.bt_state.playback_rate(address)?;
        }
        let active = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        if !active {
            self.active_transport_generation = None;
        }
        self.model.output = out;
        self.dirty.mark_both();
        if active {
            self.load()?
        }
        self.log.emit(
            Level::Info,
            "audio",
            "output_switch",
            "Output preference changed",
            Some(self.log.correlation()),
            json!({"output":self.model.output}),
        );
        self.checkpoint();
        self.dirty.mark_render();
        Ok(())
    }
    fn effect(&mut self, e: Effect) -> Result<(), String> {
        self.with_model_action(|runtime| runtime.effect_inner(e))
    }
    fn effect_inner(&mut self, e: Effect) -> Result<(), String> {
        self.dirty.mark_render();
        match e {
            Effect::None => {}
            Effect::Play(index) => self.play_index(index, false)?,
            Effect::PlayShuffled(index) => self.play_index(index, true)?,
            Effect::PlayCollection {
                selected,
                members,
                shuffle,
            } => self.play_members(selected, members, shuffle)?,
            Effect::PlayQueue(index) => {
                if index >= self.model.queue.len() {
                    return Err("invalid queue entry".into());
                }
                self.model.queue_position = index;
                self.model.position_ms = 0;
                self.load()?;
            }
            Effect::PlayNext(index) => {
                let track = self
                    .model
                    .library
                    .tracks
                    .get(index)
                    .cloned()
                    .ok_or("invalid selection")?;
                if !track.online {
                    return Err("media source offline".into());
                }
                if self.model.queue.len() >= reborn_core::MAX_QUEUE {
                    return Err("queue is full".into());
                }
                let was_empty = self.model.queue.is_empty();
                let insert_at = if was_empty {
                    0
                } else {
                    (self.model.queue_position + 1).min(self.model.queue.len())
                };
                self.model.insert_queue_entry(insert_at, track)?;
                if was_empty {
                    self.model.queue_position = 0;
                    self.load()?;
                } else if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::PlayNextCollection(members) => {
                let tracks = members
                    .into_iter()
                    .filter_map(|index| self.model.library.tracks.get(index).cloned())
                    .filter(|track| track.online)
                    .collect::<Vec<_>>();
                if tracks.is_empty() {
                    return Err("collection has no online tracks".into());
                }
                let was_empty = self.model.queue.is_empty();
                if self.model.queue.len().saturating_add(tracks.len()) > reborn_core::MAX_QUEUE {
                    return Err("queue is full".into());
                }
                let mut insert_at = if was_empty {
                    0
                } else {
                    (self.model.queue_position + 1).min(self.model.queue.len())
                };
                for track in tracks {
                    self.model.insert_queue_entry(insert_at, track)?;
                    insert_at += 1;
                }
                self.model.queue_position = if was_empty {
                    0
                } else {
                    self.model.queue_position
                };
                if was_empty
                    || matches!(
                        self.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    )
                {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::AddToQueue(index) => {
                let track = self
                    .model
                    .library
                    .tracks
                    .get(index)
                    .cloned()
                    .ok_or("invalid selection")?;
                if self.model.queue.len() >= reborn_core::MAX_QUEUE {
                    return Err("queue is full".into());
                }
                let was_empty = self.model.queue.is_empty();
                let insert_at = self.model.queue.len();
                self.model.insert_queue_entry(insert_at, track)?;
                if was_empty {
                    self.model.queue_position = 0;
                } else if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::AddToQueueCollection(members) => {
                let tracks = members
                    .into_iter()
                    .filter_map(|index| self.model.library.tracks.get(index).cloned())
                    .filter(|track| track.online)
                    .collect::<Vec<_>>();
                if tracks.is_empty() {
                    return Err("collection has no online tracks".into());
                }
                if self.model.queue.len().saturating_add(tracks.len()) > reborn_core::MAX_QUEUE {
                    return Err("queue is full".into());
                }
                let was_empty = self.model.queue.is_empty();
                let mut insert_at = self.model.queue.len();
                for track in tracks {
                    self.model.insert_queue_entry(insert_at, track)?;
                    insert_at += 1;
                }
                if was_empty {
                    self.model.queue_position = 0;
                } else if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::TogglePlayback => self.toggle()?,
            Effect::NextTrack | Effect::PreviousTrack => {
                let delta = if matches!(e, Effect::NextTrack) {
                    1
                } else {
                    -1
                };
                if self.model.step(delta) {
                    self.load()?
                } else {
                    self.model.invalidate();
                    self.playback.stop(self.model.generation);
                }
            }
            Effect::Seek(delta) => {
                self.model.position_ms = self.model.position_ms.saturating_add_signed(delta).min(
                    self.model
                        .current()
                        .map(|t| t.duration_ms.saturating_sub(1))
                        .unwrap_or(0),
                );
                if self.model.playback == PlaybackState::Playing {
                    self.load()?
                }
            }
            Effect::AdjustVolume(delta) => {
                self.model.settings.volume =
                    (self.model.settings.volume as i16 + delta as i16).clamp(0, 100) as u8;
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.ui
                    .flash(format!("Volume {}", self.model.settings.volume));
                self.checkpoint();
            }
            Effect::ScanLibrary => {
                self.model.apply(Event::LibraryScanStarted);
                if let Err(error) = self.scanner.scan(self.model.sources.clone()) {
                    self.model.apply(Event::LibraryScanFailed(error.clone()));
                    return Err(error);
                }
                self.ui.notice = "Scanning music".into();
            }
            Effect::WifiPower => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi service unavailable")?
                .commands
                .try_send(wifi::Command::Power(!self.wifi_state.enabled))
                .map_err(|_| "Wi-Fi busy")?,
            Effect::WifiScan => {
                if self.ui.wifi.scan.active() {
                    return Ok(());
                }
                self.wifi
                    .as_ref()
                    .ok_or("Wi-Fi unavailable")?
                    .commands
                    .try_send(wifi::Command::Scan)
                    .map_err(|_| "Wi-Fi busy")?;
                self.ui.wifi.requested();
                self.wifi_state.scan = reborn_core::RadioScan::Starting;
            }
            Effect::WifiConnect { ssid, password } => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Connect { ssid, password })
                .map_err(|_| "Wi-Fi busy")?,
            Effect::WifiSaved(id) => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Saved(id))
                .map_err(|_| "Wi-Fi busy")?,
            Effect::WifiForget(id) => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Forget(id))
                .map_err(|_| "Wi-Fi busy")?,
            Effect::BluetoothPower => self
                .bluetooth
                .as_ref()
                .ok_or("Bluetooth unavailable")?
                .commands
                .try_send(bluetooth::Command::Power(!self.bt_state.powered))
                .map_err(|_| "Bluetooth busy")?,
            Effect::BluetoothScan => {
                if self.ui.bluetooth.scan.active() {
                    return Ok(());
                }
                self.bluetooth
                    .as_ref()
                    .ok_or("Bluetooth unavailable")?
                    .commands
                    .try_send(bluetooth::Command::Scan(true))
                    .map_err(|_| "Bluetooth busy")?;
                self.ui.bluetooth.requested();
                self.bt_state.scan = reborn_core::RadioScan::Starting;
            }
            Effect::BluetoothDevice { path, operation } => {
                if operation == "output" {
                    let address = self
                        .bt_state
                        .devices
                        .iter()
                        .find(|d| d.path == path)
                        .ok_or("device not found")?
                        .address
                        .clone();
                    self.switch(AudioOutput::Bluetooth(address))?;
                } else {
                    let cmd = match operation.as_str() {
                        "pair" => bluetooth::Command::Pair(path),
                        "connect" => bluetooth::Command::Connect(path),
                        "disconnect" => bluetooth::Command::Disconnect(path),
                        "forget" => bluetooth::Command::Forget(path),
                        _ => return Err("unsupported Bluetooth action".into()),
                    };
                    self.bluetooth
                        .as_ref()
                        .ok_or("Bluetooth unavailable")?
                        .commands
                        .try_send(cmd)
                        .map_err(|_| "Bluetooth busy")?;
                }
            }
            Effect::Output(o) => self.switch(o)?,
            Effect::ConfirmPairing(ok) => self
                .bluetooth
                .as_ref()
                .ok_or("Bluetooth unavailable")?
                .commands
                .try_send(bluetooth::Command::Confirm(ok))
                .map_err(|_| "Bluetooth busy")?,
            Effect::ScreenSleep => {
                if self.model.screen_off {
                    return Ok(());
                }
                if !self.headless {
                    power::blank(true)?;
                }
                self.model.apply(Event::ScreenSleep);
                self.log.emit(
                    Level::Info,
                    "power",
                    "screen_blank",
                    "Display put to sleep",
                    None,
                    json!({"playback":self.model.playback}),
                );
            }
            Effect::ScreenWake => {
                if !self.model.screen_off {
                    return Ok(());
                }
                if !self.headless {
                    power::blank(false)?;
                }
                self.model.apply(Event::ScreenWake);
                self.last_activity = Instant::now();
                self.log.emit(
                    Level::Info,
                    "power",
                    "screen_wake",
                    "Display woke",
                    None,
                    json!({"playback":self.model.playback}),
                );
            }
            Effect::SetReplayGain(mode) => {
                self.model.settings.replay_gain = mode;
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::ToggleEq => {
                self.model.settings.eq_enabled = !self.model.settings.eq_enabled;
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::SetCrossfade(milliseconds) => {
                self.model.settings.crossfade_ms = milliseconds.min(30_000);
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::SetGapless(enabled) => {
                self.model.settings.gapless_enabled = enabled;
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::SetShuffle(enabled) => {
                self.model.settings.shuffle = enabled;
                self.checkpoint();
            }
            Effect::SetRepeat(mode) => {
                self.model.settings.repeat = mode;
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.checkpoint();
            }
            Effect::SetScreenTimeout(seconds) => {
                self.model.settings.screen_timeout_seconds = seconds;
                self.checkpoint();
            }
            Effect::QueueRemove(index) => {
                if index == self.model.queue_position {
                    self.ui.notice = "The current track stays in the queue".into();
                } else if index < self.model.queue.len()
                    && self.model.remove_queue_entry(index).is_some()
                {
                    let active = matches!(
                        self.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    );
                    if active {
                        self.load()?;
                    }
                    self.checkpoint();
                }
            }
            Effect::QueueMove { index, delta } => {
                let Some(next) = index.checked_add_signed(delta as isize) else {
                    return Ok(());
                };
                if index > self.model.queue_position
                    && next > self.model.queue_position
                    && next < self.model.queue.len()
                {
                    self.model.move_queue_entry(index, next);
                    if matches!(
                        self.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    ) {
                        self.load()?;
                    }
                    self.checkpoint();
                }
            }
            Effect::ClearQueue => {
                if self.model.clear_future_queue() {
                    if matches!(
                        self.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    ) {
                        self.load()?;
                    }
                    self.checkpoint();
                }
            }
            Effect::RebuildLibrary => {
                self.model.apply(Event::LibraryScanStarted);
                if let Err(error) = self.scanner.scan(self.model.sources.clone()) {
                    self.model.apply(Event::LibraryScanFailed(error.clone()));
                    return Err(error);
                }
                self.ui.notice = "Rebuilding library".into();
            }
            Effect::PowerOff => {
                power::request_shutdown(false)?;
            }
            Effect::Reboot => {
                power::request_shutdown(true)?;
            }
            Effect::Checkpoint => self.checkpoint(),
        };
        Ok(())
    }
    fn status(&self) -> Value {
        json!({"version":reborn_core::VERSION,"build_id":option_env!("REBORN_BUILD_ID").unwrap_or("development"),"session":self.log.session(),"uptime_seconds":self.log.uptime(),"current_screen":self.model.screen,"screen_off":self.model.screen_off,"playback":{"state":self.model.playback,"track_id":self.model.current().map(|t|t.id),"position_ms":self.model.position_ms,"duration_ms":self.model.current().map(|t|t.duration_ms),"queue_length":self.model.queue.len(),"queue_position":self.model.queue_position,"generation":self.model.generation},"output":self.model.output,"audio":self.playback.audio_state(),"library":{"tracks_loaded":self.model.library.tracks.len(),"schema":reborn_library::SCHEMA_VERSION,"scanning":self.model.library.scanning,"last_scan":self.model.library.last_scan,"error":self.model.library.error},"wifi":self.wifi_state,"bluetooth":self.bt_state,"storage":self.model.sources,"power":self.power,"graphics":{"available":self.graphics.is_some(),"renderer":self.graphics.as_ref().map(|g|&g.info),"headless":self.headless},"decoder":{"ffmpeg":reborn_media::version(),"runtime":reborn_media::runtime_components_if_loaded()},"buffers":{"frames":self.log.metrics()["audio_buffer_frames"],"milliseconds":self.log.metrics()["audio_buffer_ms"]}})
    }
    fn snapshot(&self) -> Value {
        json!({"status":self.status(),"health":self.log.health(),"metrics":self.log.metrics(),"recent_errors":self.log.events(20,None,Some(Level::Warn),None),"resource_usage":fs::read_to_string("/proc/self/status").unwrap_or_default(),"kernel_events":kernel_events()})
    }
    fn play_control(&mut self, a: PlaybackAction) -> Result<(), String> {
        self.with_model_action(|runtime| runtime.play_control_inner(a))
    }
    fn play_control_inner(&mut self, a: PlaybackAction) -> Result<(), String> {
        self.dirty.mark_render();
        match a {
            PlaybackAction::Play(id) => {
                let index = self
                    .model
                    .library
                    .tracks
                    .iter()
                    .position(|t| t.id == id)
                    .ok_or("track id not in library")?;
                self.effect(Effect::Play(index))
            }
            PlaybackAction::Pause => {
                self.pause();
                Ok(())
            }
            PlaybackAction::Resume => self.load(),
            PlaybackAction::Stop => {
                self.model.invalidate();
                self.playback.stop(self.model.generation);
                self.model.playback = PlaybackState::Stopped;
                self.model.position_ms = 0;
                self.dirty.mark_both();
                self.checkpoint();
                Ok(())
            }
            PlaybackAction::Next => self.effect(Effect::NextTrack),
            PlaybackAction::Previous => self.effect(Effect::PreviousTrack),
            PlaybackAction::Seek(ms) => {
                self.model.position_ms = ms.min(
                    self.model
                        .current()
                        .map(|t| t.duration_ms.saturating_sub(1))
                        .unwrap_or(0),
                );
                self.dirty.mark_both();
                if self.model.playback == PlaybackState::Playing {
                    self.load()
                } else {
                    Ok(())
                }
            }
            PlaybackAction::Wired => self.switch(AudioOutput::Wired),
            PlaybackAction::Bluetooth(s) => self.switch(AudioOutput::Bluetooth(s)),
            PlaybackAction::Volume(v) => {
                self.model.settings.volume = v.min(100);
                self.dirty.mark_both();
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                Ok(())
            }
        }
    }
}
fn kernel_events() -> Vec<String> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let Ok(mut f) = fs::OpenOptions::new()
        .read(true)
        .custom_flags(0o4000)
        .open("/dev/kmsg")
    else {
        return vec![];
    };
    let mut events = std::collections::VecDeque::new();
    for _ in 0..2048 {
        let mut buf = [0; 4096];
        let Ok(n) = f.read(&mut buf) else { break };
        let line = String::from_utf8_lossy(&buf[..n]);
        let l = line.to_lowercase();
        if [
            "lima", "drm", "gpu", "alsa", "snd_", "mmc", "ext4", "suspend",
        ]
        .iter()
        .any(|s| l.contains(s))
            && ["error", "fault", "hang", "timeout", "corrupt"]
                .iter()
                .any(|s| l.contains(s))
            && !l.contains("calibration")
        {
            events.push_back(line.chars().take(512).collect());
            if events.len() > 32 {
                events.pop_front();
            }
        }
    }
    events.into()
}

fn friendly_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("unavailable")
        || lower.contains("not ready")
        || lower.contains("not inserted")
    {
        return "That feature is not available right now.".into();
    }
    if lower.contains("busy") || lower.contains("timeout") {
        return "That took too long. Please try again.".into();
    }
    if lower.contains("password") || lower.contains("ssid") {
        return "The network details could not be accepted.".into();
    }
    "Could not complete that action. Please try again.".into()
}
fn secure_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = fs::symlink_metadata(path) {
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err(format!(
                "persistent path is not a real directory: {}",
                path.display()
            ));
        }
    }
    fs::create_dir_all(path).map_err(|e| e.to_string())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}
fn option(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
fn startup_phase(log: &Observer, started: Instant, phase: &str) {
    log.emit(
        Level::Info,
        "startup",
        "phase",
        phase,
        None,
        json!({"phase":phase,"elapsed_ms":started.elapsed().as_millis()}),
    );
}
fn run() -> Result<(), String> {
    let process_started = Instant::now();
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|s| s == "--version") {
        let _ = writeln!(std::io::stdout(), "{}", reborn_core::VERSION);
        return Ok(());
    }
    if args.iter().any(|s| s == "--help") {
        let _ = writeln!(
            std::io::stdout(),
            "reborn [--headless --data-dir DIR --socket PATH --music-dir DIR --fixtures DIR]"
        );
        return Ok(());
    }
    let headless = args.iter().any(|s| s == "--headless");
    if !headless && !storage::data_ready() {
        return Err("Y2DATA is not ready; refusing mutable data on Y2ROOT".into());
    }
    let root = PathBuf::from(option(&args, "--data-dir").unwrap_or("/data/reborn".into()));
    if !headless && root != Path::new("/data/reborn") {
        return Err("production data directory must be /data/reborn".into());
    }
    for p in [
        root.clone(),
        root.join("state"),
        root.join("cache"),
        root.join("logs"),
        root.join("diagnostics"),
    ] {
        secure_dir(&p)?
    }
    let log = Observer::new(&root.join("logs")).map_err(|e| e.to_string())?;
    log.install_panic_hook(reborn_core::VERSION);
    reborn_platform::install_signals();
    reborn_media::initialize_logging(log.clone());
    reborn_audio::initialize_logging(log.clone());
    log.emit(Level::Info,"startup","starting","Reborn Baseline 01",None,json!({"version":reborn_core::VERSION,"ffmpeg":reborn_media::version(),"headless":headless,"process_setup_ms":process_started.elapsed().as_millis()}));
    let session_path = root.join("state/session.json");
    let mut model = match AppModel::restore(&session_path) {
        Ok(model) => model,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AppModel::default(),
        Err(error) => {
            log.emit(
                Level::Warn,
                "core",
                "restore_failed",
                "Saved session was rejected; starting with a clean session",
                None,
                json!({"path":session_path,"error":error.to_string()}),
            );
            AppModel::default()
        }
    };
    startup_phase(&log, process_started, "model_restored");
    // Open the display as soon as the process is alive. The first bounded
    // frame below hands KMS to Reborn while storage and library workers start.
    let mut graphics = if headless {
        log.health_set(
            "graphics",
            HealthState::Unavailable,
            true,
            "explicit headless mode",
        );
        None
    } else {
        match Renderer::open(
            &reborn_ui::font_atlas(),
            &reborn_ui::display_font_atlas(),
            &reborn_ui::icons_atlas(),
            log.clone(),
        ) {
            Ok(g) => Some(g),
            Err(e) => {
                log.health_set("graphics", HealthState::Failed, true, &e);
                log.emit(
                    Level::Error,
                    "graphics",
                    "startup_failed",
                    &e,
                    None,
                    json!({}),
                );
                None
            }
        }
    };
    startup_phase(&log, process_started, "graphics_ready");
    let ui = Ui::default();
    let mut first_frame_presented = false;
    if let Some(g) = &mut graphics {
        // Hand scanout to Reborn as soon as the renderer exists. The model is
        // already restored, so this bounded initial frame gives the user the
        // real UI while storage, library and radio workers start below it.
        let draw = ui.draw(&model, &[], "starting", false, power_view(&power::status()));
        if g.render(&draw).is_ok() {
            first_frame_presented = true;
            log.emit(
                Level::Info,
                "startup",
                "ready",
                "First Reborn frame presented",
                None,
                json!({"display_handoff":"explicit KMS presentation","initial":true}),
            );
        }
    }
    if let Some(m) = option(&args, "--music-dir") {
        model.settings.music_directory = m.into();
    }
    if !headless && !model.settings.music_directory.starts_with("/data/") {
        return Err("internal music must live under Y2DATA".into());
    }
    fs::create_dir_all(&model.settings.music_directory).map_err(|e| e.to_string())?;
    if !headless && storage::sd_present() {
        let _ = storage::mount_sd();
    }
    model.sources = if headless {
        vec![Source {
            id: "internal".into(),
            kind: reborn_core::MediaSource::Internal,
            root: model.settings.music_directory.clone(),
            online: true,
            mount: "host-test".into(),
            mount_id: None,
        }]
    } else {
        storage::sources(&model.settings.music_directory)
    };
    startup_phase(&log, process_started, "storage_ready");
    let db = Database::spawn(root.join("library.db"), log.clone())?;
    db.sources(model.sources.clone())?;
    let scanner = Scanner::spawn(db.clone(), log.clone())?;
    scanner.scan(model.sources.clone())?;
    startup_phase(&log, process_started, "library_workers_ready");
    let inputs = if headless {
        None
    } else {
        Some(input::InputManager::open())
    };
    log.health_set(
        "input",
        if inputs.as_ref().is_some_and(|i| i.count() > 0) {
            HealthState::Ok
        } else {
            HealthState::Unavailable
        },
        true,
        "evdev mapping",
    );
    log.health_set("storage", HealthState::Ok, true, "Y2DATA ready");
    log.health_set(
        "audio",
        if reborn_audio::wired_device().is_ok() {
            HealthState::Ok
        } else {
            HealthState::Unavailable
        },
        true,
        "ALSA card discovery",
    );
    startup_phase(&log, process_started, "core_services_ready");
    let playback = playback::Playback::spawn(log.clone(), root.join("cache"))?;
    let wifi = if headless {
        None
    } else {
        Some(wifi::Wifi::spawn(log.clone()).map_err(|e| e.to_string())?)
    };
    let bluetooth = if headless {
        None
    } else {
        Some(bluetooth::Bluetooth::spawn(log.clone()).map_err(|e| e.to_string())?)
    };
    startup_phase(&log, process_started, "radio_workers_ready");
    let socket =
        PathBuf::from(option(&args, "--socket").unwrap_or("/run/reborn/control.sock".into()));
    let server = reborn_control::Server::spawn(&socket, log.clone()).map_err(|e| e.to_string())?;
    let diagnostics = diagnostics::Diagnostics::spawn(
        log.clone(),
        db.clone(),
        PathBuf::from(option(&args, "--fixtures").unwrap_or("/usr/share/reborn/fixtures".into())),
        root.join("diagnostics"),
    )
    .map_err(|e| e.to_string())?;
    let mut rt = Runtime {
        model,
        ui,
        playback,
        log: log.clone(),
        db,
        scanner,
        wifi,
        bluetooth,
        wifi_state: Default::default(),
        bt_state: Default::default(),
        graphics,
        power: power::status(),
        art: false,
        dirty: DirtyState {
            render: true,
            persistence: false,
        },
        root: root.clone(),
        headless,
        last_activity: Instant::now(),
        query: None,
        action_depth: 0,
        action_before: None,
        action_dirty_before: None,
        pending_reconfiguration: None,
        reconfiguration_attempted: false,
        sink_release_started: false,
        transition_generation: None,
        active_transport_generation: None,
    };
    startup_phase(&log, process_started, "runtime_ready");
    let last_snapshot = Arc::new(Mutex::new(json!({"starting":true})));
    let watcher = last_snapshot.clone();
    let wl = log.clone();
    let diagnostic_root = root.join("diagnostics");
    thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || {
            let mut last_failed = false;
            loop {
                thread::sleep(Duration::from_secs(1));
                let health = wl.health();
                let failed = health["overall"] == "failed";
                if failed && !last_failed {
                    wl.emit(
                        Level::Error,
                        "diagnostics",
                        "worker_or_subsystem_failed",
                        "Health transitioned to failed",
                        None,
                        json!({"health":health}),
                    );
                    if let Ok(s) = watcher.try_lock() {
                        let _ = wl.diagnostic(&diagnostic_root, s.clone(), true);
                    }
                }
                last_failed = failed;
                if reborn_platform::stop_requested() {
                    break;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let mut inputs = inputs;
    let (mut periodic, mut checkpoint, mut render_time) = (
        Instant::now(),
        Instant::now(),
        Instant::now() - Duration::from_secs(1),
    );
    let mut monitor: Option<(Instant, SyncSender<Response>, u64, Vec<Value>)> = None;
    let mut action_router = input::ActionRouter::default();
    let mut last_sd = storage::sd_present();
    let mut xrun_bundle = 0.;
    let mut shutdown_intent = None;
    let mut power_poll = Instant::now() - Duration::from_secs(1);
    while !reborn_platform::stop_requested() {
        if !headless && power_poll.elapsed() >= Duration::from_millis(250) {
            power_poll = Instant::now();
            shutdown_intent = power::shutdown_intent();
            if shutdown_intent.is_some() {
                break;
            }
        }
        log.heartbeat("ui", 10);
        rt.ui.expire_notice();
        rt.poll_pending_reconfiguration();
        if let Some(input) = &mut inputs {
            for event in input.poll() {
                if let Some((_, _, _, events)) = &mut monitor {
                    if events.len() < 512 {
                        events.push(
                            json!({"device":event.device,"normalized":event.input,"mono_ms":log.uptime()*1000}),
                        );
                    }
                }
                log.emit(
                    Level::Debug,
                    "input",
                    "normalized_event",
                    "Normalized physical input",
                    None,
                    json!({"device":event.device,"input":event.input}),
                );
                let actions = action_router.route(&event, !rt.model.screen_off);
                for action in actions {
                    log.emit(
                        Level::Debug,
                        "input",
                        "action",
                        "Semantic application action",
                        None,
                        json!({"action":action}),
                    );
                    if !rt.model.screen_off || matches!(action, reborn_core::Action::ScreenWake) {
                        rt.last_activity = Instant::now();
                    }
                    let effect = rt.ui.model_action(&mut rt.model, action);
                    if let Err(e) = rt.effect(effect) {
                        rt.fail("ui", e)
                    }
                }
            }
        }
        if monitor.as_ref().is_some_and(|m| Instant::now() > m.0) {
            let (_, reply, id, events) = monitor.take().unwrap();
            let _ = reply.try_send(Response::ok(
                id,
                json!({"events":events,"count":events.len()}),
            ));
        }
        while let Ok(e) = rt.playback.events.try_recv() {
            match e {
                playback::PlaybackEvent::Artwork {
                    generation,
                    entry_id,
                    track_id,
                    bytes,
                } => {
                    if artwork_matches_current(&rt.model, generation, entry_id, track_id) {
                        if let Some(g) = &mut rt.graphics {
                            if g.artwork(&bytes).is_ok() {
                                rt.art = true;
                                rt.dirty.mark_render();
                            }
                        }
                    }
                }
                playback::PlaybackEvent::Core(event) => {
                    if let Event::Position { generation, ms } = event {
                        if generation == rt.model.generation && ms != rt.model.position_ms {
                            rt.dirty.mark_persistence();
                            if ms / 1000 != rt.model.position_ms / 1000 {
                                rt.dirty.mark_render();
                            }
                        }
                        rt.model.apply(Event::Position { generation, ms });
                        continue;
                    }
                    let persists = matches!(
                        &event,
                        Event::TrackBoundary { .. }
                            | Event::PlaybackError { .. }
                            | Event::SourceChanged(_)
                            | Event::BluetoothDisconnected(_)
                    );
                    let error = matches!(&event,Event::PlaybackError{generation,..}if *generation==rt.model.generation);
                    let ended = matches!(
                        &event,
                        Event::TrackEnded { generation } if *generation == rt.model.generation
                    );
                    if matches!(
                        &event,
                        Event::TrackBoundary { generation, .. }
                            if *generation == rt.model.generation
                    ) {
                        rt.art = false;
                    }
                    rt.model.apply(event);
                    rt.dirty.mark_render();
                    if persists {
                        rt.dirty.mark_persistence();
                    }
                    if error {
                        rt.model.invalidate();
                        rt.playback.stop(rt.model.generation);
                        let _ = log.diagnostic(&root.join("diagnostics"), rt.snapshot(), true);
                    }
                    if ended {
                        if let Err(e) = rt.finish_track() {
                            rt.fail("playback", e);
                        }
                    }
                }
            }
        }
        if let Ok(result) = rt.scanner.results.try_recv() {
            match result {
                Ok(stats) if stats.complete => {
                    rt.model
                        .apply(Event::LibraryScanFinished(reborn_core::ScanSummary {
                            discovered: stats.discovered,
                            reused: stats.reused,
                            elapsed_ms: stats.elapsed_ms,
                        }));
                    rt.query = rt
                        .db
                        .list(Filter {
                            limit: 20000,
                            ..Default::default()
                        })
                        .ok();
                    rt.ui.notice =
                        format!("Scan: {} tracks, {} reused", stats.discovered, stats.reused);
                }
                Ok(stats) => {
                    let error = format!(
                        "scan incomplete; retained existing entries ({} traversal failures)",
                        stats.failures
                    );
                    rt.model.apply(Event::LibraryScanFailed(error.clone()));
                    log.health_set("scanner", HealthState::Degraded, true, &error);
                    rt.fail("scanner", error);
                }
                Err(e) => {
                    rt.model.apply(Event::LibraryScanFailed(e.clone()));
                    log.health_set("scanner", HealthState::Failed, true, &e);
                    rt.fail("scanner", e);
                }
            }
            rt.dirty.mark_render();
        }
        if let Some(rx) = &rt.query {
            if let Ok(value) = rx.try_recv() {
                match value {
                    Ok(t) => rt.model.library.tracks = t,
                    Err(e) => rt.fail("database", e),
                }
                rt.query = None;
                rt.dirty.mark_render();
            }
        }
        if let Some(service) = &rt.wifi {
            while let Ok(s) = service.events.try_recv() {
                rt.ui.wifi = reborn_ui::RadioView {
                    available: s.available,
                    powered: s.enabled,
                    scan: s.scan.clone(),
                    error: s.error.clone(),
                    connection: if s.state == "COMPLETED" {
                        format!("Connected: {}", s.ssid)
                    } else if s.state == "STARTING" {
                        "Starting Wi-Fi...".into()
                    } else {
                        String::new()
                    },
                    count: s.networks.len(),
                };
                rt.ui.networks = s
                    .networks
                    .iter()
                    .map(|n| {
                        Item::new(
                            format!(
                                "{} {} dBm {}",
                                n.ssid,
                                n.signal,
                                if n.security.contains("WPA") {
                                    "secure"
                                } else {
                                    "open"
                                }
                            ),
                            n.ssid.clone(),
                        )
                    })
                    .collect();
                rt.ui.saved_networks = s
                    .saved
                    .iter()
                    .map(|n| {
                        Item::new(
                            format!("Saved: {}", n.ssid),
                            format!("saved:{}", n.saved_id.unwrap_or(0)),
                        )
                        .with_secondary("Left: forget")
                    })
                    .collect();
                rt.wifi_state = s;
                rt.dirty.render |= rt.model.screen == Screen::Wifi;
            }
        }
        let mut bt_lost = false;
        if let Some(service) = &rt.bluetooth {
            while let Ok(s) = service.events.try_recv() {
                if let AudioOutput::Bluetooth(address) = &rt.model.output {
                    bt_lost = rt
                        .bt_state
                        .devices
                        .iter()
                        .any(|d| &d.address == address && d.connected)
                        && !s
                            .devices
                            .iter()
                            .any(|d| &d.address == address && d.connected);
                }
                let connected = s.devices.iter().find(|device| device.connected);
                let connection = if let Some(device) = connected {
                    let connection = s
                        .pcms
                        .iter()
                        .find(|pcm| pcm.is_a2dp_playback_for(&device.path))
                        .and_then(|pcm| pcm.codec.as_deref())
                        .filter(|codec| !codec.is_empty())
                        .map(|codec| format!("Connected: {} · {}", device.name, codec))
                        .unwrap_or_else(|| format!("Connected: {}", device.name));
                    connection
                } else if s.discovering {
                    "Bluetooth discovery active".into()
                } else {
                    String::new()
                };
                rt.ui.bluetooth = reborn_ui::RadioView {
                    available: s.available,
                    powered: s.powered,
                    scan: s.scan.clone(),
                    error: s.error.clone(),
                    connection,
                    count: s.devices.len(),
                };
                rt.ui.bluetooth_devices = s
                    .devices
                    .iter()
                    .map(|d| {
                        Item::new(
                            format!(
                                "{} {}",
                                d.name,
                                if d.connected {
                                    "connected"
                                } else if d.paired {
                                    "paired"
                                } else {
                                    "available"
                                }
                            ),
                            d.path.clone(),
                        )
                        .with_secondary(if d.audio {
                            "Audio device"
                        } else {
                            "Device"
                        })
                    })
                    .collect();
                rt.ui.pairing = s.pending.as_ref().map(|p| p.display.clone());
                rt.dirty.render |= rt.model.screen == Screen::Bluetooth || rt.ui.pairing.is_some();
                rt.bt_state = s;
            }
        }
        if bt_lost {
            rt.abandon_pending_reconfiguration();
            if let AudioOutput::Bluetooth(address) = rt.model.output.clone() {
                rt.model.apply(Event::BluetoothDisconnected(address));
            }
            rt.pause();
            rt.ui.notice = "Bluetooth disconnected; playback paused".into();
            log.emit(
                Level::Warn,
                "bluetooth",
                "transport_lost",
                "Selected audio peer disconnected",
                None,
                json!({"recovery":"paused"}),
            );
        }
        match transport_epoch_state(
            &rt.model.output,
            rt.model.playback,
            rt.active_transport_generation,
            &rt.bt_state,
        ) {
            TransportEpochState::Current => {}
            TransportEpochState::Changed => {
                if rt.pending_reconfiguration.is_some() {
                    rt.fail_active_transport(
                        "Bluetooth transport changed while a new sink was opening",
                    );
                    rt.ui.notice = "Bluetooth audio transport changed; playback stopped".into();
                    rt.dirty.mark_render();
                } else if let Err(error) = rt.with_model_action(|runtime| runtime.load()) {
                    rt.ui.notice = format!("Bluetooth audio transport changed: {error}");
                    rt.dirty.mark_render();
                }
            }
            TransportEpochState::Unavailable => {
                rt.fail_active_transport(
                    "Bluetooth playback PCM disappeared or lost its negotiated identity",
                );
                rt.ui.notice = "Bluetooth audio transport changed; playback stopped".into();
                rt.dirty.mark_render();
            }
        }
        while let Ok(env) = server.requests.try_recv() {
            let req = env.request;
            let id = req.id;
            let reply = env.reply;
            let result: Result<Value, String> = match req.command.clone() {
                Command::Status => Ok(rt.status()),
                Command::Audio => Ok(
                    json!({"audio":rt.playback.audio_state(),"ffmpeg":reborn_media::runtime_components_if_loaded()}),
                ),
                Command::Health => Ok(log.health()),
                Command::Metrics => Ok(log.metrics()),
                Command::Snapshot => Ok(rt.snapshot()),
                Command::Events { last } => Ok(json!(log.events(last, None, None, None))),
                Command::Logs {
                    last,
                    subsystem,
                    level,
                    since_ms,
                } => Ok(json!(log.events(
                    last,
                    subsystem.as_deref(),
                    level,
                    since_ms
                ))),
                Command::LogLevel { subsystem, level } => log.level(subsystem.as_deref(), level),
                Command::Tests => Ok(
                    json!({"tests":reborn_control::TESTS.iter().map(|(n,a)|json!({"name":n,"audible":a,"bounded":true})).collect::<Vec<_>>()}),
                ),
                Command::Scan => rt
                    .effect(Effect::ScanLibrary)
                    .map(|_| json!({"accepted":true})),
                Command::Radio { radio, action } => {
                    use reborn_control::{Radio, RadioAction};
                    // Scan uses exactly the button/UI path, not an independent
                    // diagnostic implementation which could hide UI-worker bugs.
                    let r = match (radio, action) {
                        (Radio::Wifi, RadioAction::Scan) => rt.effect(Effect::WifiScan),
                        (Radio::Bluetooth, RadioAction::Scan) => rt.effect(Effect::BluetoothScan),
                        (Radio::Wifi, RadioAction::On | RadioAction::Off) => rt
                            .wifi
                            .as_ref()
                            .ok_or("Wi-Fi service unavailable".to_string())
                            .and_then(|w| {
                                w.commands
                                    .try_send(wifi::Command::Power(matches!(
                                        action,
                                        RadioAction::On
                                    )))
                                    .map_err(|_| "Wi-Fi busy".into())
                            }),
                        (Radio::Bluetooth, RadioAction::On | RadioAction::Off) => rt
                            .bluetooth
                            .as_ref()
                            .ok_or("Bluetooth service unavailable".to_string())
                            .and_then(|w| {
                                w.commands
                                    .try_send(bluetooth::Command::Power(matches!(
                                        action,
                                        RadioAction::On
                                    )))
                                    .map_err(|_| "Bluetooth busy".into())
                            }),
                    };
                    r.map(|_| json!({"accepted":true,"radio":radio,"action":action}))
                }
                Command::Playback { action } => rt.play_control(action).map(|_| rt.status()),
                Command::InputMonitor { seconds } => {
                    if monitor.is_some() {
                        Err("input monitor busy".into())
                    } else {
                        monitor = Some((
                            Instant::now() + Duration::from_secs(seconds),
                            reply,
                            id,
                            vec![],
                        ));
                        continue;
                    }
                }
                Command::Diagnose | Command::Test { .. } => {
                    let graphics = if matches!(
                        req.command,
                        Command::Test {
                            name: reborn_control::Test::Baseline | reborn_control::Test::Graphics,
                            ..
                        }
                    ) {
                        match &mut rt.graphics {
                            Some(g) => match g.test() {
                                Ok(()) => {
                                    json!({"passed":true,"renderer":g.info,"offscreen_readback":true})
                                }
                                Err(e) => json!({"passed":false,"error":e}),
                            },
                            None => json!({"passed":false,"error":"graphics unavailable"}),
                        }
                    } else {
                        json!({})
                    };
                    let job = diagnostics::Job {
                        request: req,
                        reply: reply.clone(),
                        snapshot: rt.snapshot(),
                        sources: rt.model.sources.clone(),
                        graphics,
                        output: rt.model.output.clone(),
                        output_rate: rt.output_rate(),
                        audio_busy: matches!(
                            rt.model.playback,
                            PlaybackState::Playing | PlaybackState::Buffering
                        ),
                    };
                    if diagnostics.sender.try_send(job).is_err() {
                        Err("diagnostic worker busy".into())
                    } else {
                        continue;
                    }
                }
            };
            let response = match result {
                Ok(v) => Response::ok(id, v),
                Err(e) => Response::error(id, e),
            };
            let _ = reply.try_send(response);
        }
        if periodic.elapsed() > Duration::from_secs(2) {
            periodic = Instant::now();
            let next_power = power::status();
            if next_power["supplies"] != rt.power["supplies"] {
                log.emit(
                    Level::Debug,
                    "power",
                    "supply_changed",
                    "Power supply state changed",
                    None,
                    next_power.clone(),
                );
            }
            rt.power = next_power;
            if !headless {
                let sd = storage::sd_present();
                if sd && !last_sd {
                    let _ = storage::mount_sd();
                }
                if !sd && last_sd {
                    rt.pause();
                    let _ = storage::unmount_sd();
                }
                last_sd = sd;
                let sources = storage::sources(&rt.model.settings.music_directory);
                if sources != rt.model.sources {
                    rt.abandon_pending_reconfiguration();
                    let old = rt.model.sources.clone();
                    for s in &sources {
                        if !old.iter().any(|v| v.id == s.id) {
                            log.add("sd_insertions", 1.);
                            log.emit(
                                Level::Info,
                                "storage",
                                "source_inserted",
                                "Media source mounted",
                                None,
                                json!({"source":s}),
                            );
                        }
                    }
                    for s in &old {
                        if !sources.iter().any(|v| v.id == s.id) {
                            log.add("sd_removals", 1.);
                            log.emit(
                                Level::Info,
                                "storage",
                                "source_removed",
                                "Media source offline",
                                None,
                                json!({"source_id":s.id}),
                            );
                        }
                    }
                    let g = rt.model.generation;
                    rt.model.apply(Event::SourceChanged(sources.clone()));
                    if g != rt.model.generation {
                        rt.playback.stop(rt.model.generation)
                    }
                    let _ = rt.db.sources(sources);
                    rt.model.apply(Event::LibraryScanStarted);
                    if let Err(error) = rt.scanner.scan(rt.model.sources.clone()) {
                        rt.model.apply(Event::LibraryScanFailed(error.clone()));
                        rt.fail("scanner", error);
                    }
                    rt.dirty.mark_render();
                    rt.dirty.mark_persistence();
                }
            }
            if !headless
                && !rt.model.screen_off
                && rt.model.settings.screen_timeout_seconds != 0
                && rt.last_activity.elapsed().as_secs()
                    >= rt.model.settings.screen_timeout_seconds as u64
            {
                let _ = rt.effect(Effect::ScreenSleep);
            }
            if let Ok(mut s) = last_snapshot.lock() {
                *s = rt.snapshot();
                log.context(rt.status());
            }
            let xruns = log.metrics()["audio_xruns"].as_f64().unwrap_or(0.);
            if xruns - xrun_bundle >= 3. {
                xrun_bundle = xruns;
                let _ = log.diagnostic(&root.join("diagnostics"), rt.snapshot(), true);
            }
        }
        if checkpoint.elapsed() > Duration::from_secs(15) && rt.dirty.checkpoint_due() {
            rt.checkpoint();
            checkpoint = Instant::now();
        } else if checkpoint.elapsed() > Duration::from_secs(15) {
            checkpoint = Instant::now();
        }
        if rt.dirty.render
            && !rt.model.screen_off
            && render_time.elapsed() > Duration::from_millis(34)
        {
            let draw = rt.ui.draw(
                &rt.model,
                &rt.model.library.tracks,
                log.health()["overall"].as_str().unwrap_or("unknown"),
                rt.art,
                power_view(&rt.power),
            );
            if let Some(g) = &mut rt.graphics {
                if let Err(e) = g.render(&draw) {
                    rt.fail("graphics", e);
                    rt.graphics = None;
                    // One bounded recreation attempt. Further attempts wait for an explicit wake.
                    if let Ok(g) = Renderer::open(
                        &reborn_ui::font_atlas(),
                        &reborn_ui::display_font_atlas(),
                        &reborn_ui::icons_atlas(),
                        log.clone(),
                    ) {
                        rt.graphics = Some(g);
                    }
                    let _ = log.diagnostic(&root.join("diagnostics"), rt.snapshot(), true);
                } else if !first_frame_presented {
                    first_frame_presented = true;
                    log.emit(
                        Level::Info,
                        "startup",
                        "ready",
                        "First Reborn frame presented",
                        None,
                        json!({"display_handoff":"explicit KMS presentation"}),
                    );
                    // Early splash evidence lives on the /run mount carried out
                    // of initramfs; retain this bounded record alongside app logs.
                    if let Ok(bytes) = fs::read("/run/reborn-splash/events.jsonl") {
                        if bytes.len() <= 8192 {
                            let _ = reborn_core::atomic_write(
                                &root.join("logs/splash-boot.jsonl"),
                                &bytes,
                            );
                        }
                    }
                }
            }
            rt.dirty.rendered();
            render_time = Instant::now();
        }
        thread::sleep(Duration::from_millis(15));
    }
    rt.model.invalidate();
    let mut shutdown_ready = true;
    if let Err(e) = rt.playback.shutdown(rt.model.generation) {
        shutdown_ready = false;
        log.emit(
            Level::Error,
            "audio",
            "shutdown_failed",
            &e,
            None,
            json!({}),
        );
    }
    rt.model.playback = PlaybackState::Paused;
    rt.pending_reconfiguration = None;
    rt.checkpoint();
    shutdown_ready &= !rt.dirty.persistence;
    rt.scanner.stop();
    if let Err(error) = rt.db.shutdown(Duration::from_secs(2)) {
        shutdown_ready = false;
        log.emit(
            Level::Error,
            "database",
            "shutdown_failed",
            &error,
            None,
            json!({}),
        );
    }
    if let Some(id) = shutdown_intent {
        if let Err(error) = power::acknowledge_shutdown(&id, shutdown_ready) {
            log.emit(Level::Warn, "power", "ack_failed", &error, None, json!({}));
        }
    }
    if let Some(w) = rt.wifi {
        let _ = w.commands.try_send(wifi::Command::Stop);
    }
    if let Some(b) = rt.bluetooth {
        let _ = b.commands.try_send(bluetooth::Command::Stop);
    }
    let _ = fs::remove_file(socket);
    log.emit(
        Level::Info,
        "core",
        "shutdown",
        "Reborn stopped",
        None,
        json!({}),
    );
    Ok(())
}

fn power_view(status: &Value) -> PowerView {
    let battery = status["supplies"].as_array().and_then(|supplies| {
        supplies
            .iter()
            .find(|supply| supply["type"].as_str() == Some("Battery"))
            .or_else(|| {
                supplies
                    .iter()
                    .find(|supply| supply["name"].as_str() == Some("BAT0"))
            })
    });
    let battery_percent = battery
        .and_then(|supply| supply["capacity"].as_str())
        .and_then(|value| value.trim().parse::<u8>().ok())
        .map(|value| value.min(100));
    let charging = battery
        .and_then(|supply| supply["status"].as_str())
        .is_some_and(|status| matches!(status, "Charging" | "Full"));
    PowerView {
        battery_percent,
        charging,
    }
}

fn main() {
    if let Err(error) = run() {
        let record = json!({"level":"ERROR","subsystem":"startup","event":"fatal","message":error});
        let _ = serde_json::to_writer(std::io::stderr(), &record);
        let _ = std::io::stderr().write_all(b"\n");
        std::process::exit(1)
    }
}

#[cfg(test)]
mod artwork_presentation_tests {
    use super::artwork_matches_current;
    use reborn_core::{AppModel, Event, QueueEntryId, Track};

    #[test]
    fn duplicate_track_occurrences_accept_art_only_after_their_boundary() {
        let track = Track {
            id: 4,
            ..Default::default()
        };
        let mut model = AppModel::default();
        model.generation = 7;
        model.queue = vec![track.clone(), track];
        model.queue_entry_ids = vec![QueueEntryId(21), QueueEntryId(22)];

        assert!(artwork_matches_current(&model, 7, QueueEntryId(21), 4));
        assert!(!artwork_matches_current(&model, 7, QueueEntryId(22), 4));

        model.apply(Event::TrackBoundary {
            generation: 7,
            next_entry_id: Some(QueueEntryId(22)),
            next_track_id: Some(4),
            output_position_ms: 0,
        });

        assert_eq!(model.current_entry_id(), Some(QueueEntryId(22)));
        assert!(artwork_matches_current(&model, 7, QueueEntryId(22), 4));
        assert!(!artwork_matches_current(&model, 6, QueueEntryId(22), 4));
    }
}

#[cfg(test)]
mod bluetooth_epoch_tests {
    use super::{transport_epoch_state, TransportEpochState};
    use crate::bluetooth::{self, Device};
    use reborn_core::{AudioOutput, BluetoothPcm, PlaybackState};

    fn status(generation: u64) -> bluetooth::Status {
        let mut status = bluetooth::Status::default();
        status.bluealsa = true;
        status.devices = vec![Device {
            path: "/org/bluez/hci0/dev_01_02_03_04_05_06".into(),
            address: "01:02:03:04:05:06".into(),
            connected: true,
            audio: true,
            ..Default::default()
        }];
        status.pcms = vec![BluetoothPcm {
            object: "/org/bluealsa/hci0/dev_01_02_03_04_05_06/a2dp".into(),
            device: status.devices[0].path.clone(),
            transport: "A2DP-source".into(),
            mode: "sink".into(),
            codec: Some("SBC".into()),
            format: Some(0x8210),
            rate: Some(44_100),
            channels: Some(2),
            transport_generation: generation,
            ..Default::default()
        }];
        status
    }

    #[test]
    fn active_sink_is_reopened_or_stopped_when_transport_epoch_is_stale() {
        let output = AudioOutput::Bluetooth("01:02:03:04:05:06".into());
        let current = status(10);
        assert_eq!(
            transport_epoch_state(&output, PlaybackState::Playing, Some(10), &current),
            TransportEpochState::Current
        );
        assert_eq!(
            transport_epoch_state(&output, PlaybackState::Playing, Some(10), &status(11)),
            TransportEpochState::Changed
        );

        let mut disappeared = status(10);
        disappeared.pcms.clear();
        assert_eq!(
            transport_epoch_state(&output, PlaybackState::Playing, Some(10), &disappeared),
            TransportEpochState::Unavailable
        );
        assert_eq!(
            transport_epoch_state(&output, PlaybackState::Paused, Some(10), &status(11)),
            TransportEpochState::Current
        );
    }
}

#[cfg(test)]
mod runtime_dirty_tests {
    use super::DirtyState;

    #[test]
    fn stopped_setting_save_keeps_its_redraw_request() {
        let mut dirty = DirtyState::default();
        dirty.mark_both(); // SetScreenTimeout while playback is stopped.

        dirty.checkpoint_succeeded();

        assert!(!dirty.checkpoint_due());
        assert!(dirty.render);
    }

    #[test]
    fn rendering_before_checkpoint_keeps_persistence_pending() {
        let mut dirty = DirtyState::default();
        dirty.mark_both();

        dirty.rendered();

        assert!(dirty.checkpoint_due());
        dirty.checkpoint_succeeded();
        assert!(!dirty.checkpoint_due());
    }

    #[test]
    fn checkpoint_before_render_keeps_redraw_pending() {
        let mut dirty = DirtyState::default();
        dirty.mark_both();

        dirty.checkpoint_succeeded();

        assert!(dirty.render);
        dirty.rendered();
        assert!(!dirty.render);
    }

    #[test]
    fn bluetooth_loss_notice_can_be_saved_and_still_rendered() {
        let mut dirty = DirtyState::default();
        dirty.mark_both(); // Lost transport changes output/playback and sets a notice.

        dirty.checkpoint_succeeded();

        assert!(!dirty.checkpoint_due());
        assert!(dirty.render);
    }

    #[test]
    fn rendered_position_change_remains_due_for_bounded_checkpoint() {
        let mut dirty = DirtyState::default();
        dirty.mark_persistence(); // Position events are saved on the 15-second cadence.
        dirty.mark_render();
        dirty.rendered();

        assert!(dirty.checkpoint_due());
        dirty.checkpoint_succeeded();
        assert!(!dirty.checkpoint_due());
    }

    #[test]
    fn unchanged_state_does_not_schedule_a_save_loop() {
        let mut dirty = DirtyState::default();
        assert!(!dirty.checkpoint_due());

        dirty.mark_render();
        dirty.rendered();

        assert!(!dirty.checkpoint_due());
    }

    #[test]
    fn failed_checkpoint_retains_persistence_intent() {
        let mut dirty = DirtyState::default();
        dirty.mark_persistence();

        dirty.checkpoint_failed();

        assert!(dirty.checkpoint_due());
    }
}

#[cfg(test)]
mod runtime_reconfiguration_tests {
    use super::{failed_reconfiguration_model, run_sink_reconfiguration};
    use crate::playback::Playback;
    use reborn_audio::{AudioSink, Parameters, SinkSpec};
    use reborn_core::{
        AppModel, AudioOutput, EqBand, PcmFormat, PlaybackState, QueueEntryId, ReplayGainMode,
        Track,
    };
    use reborn_media::DspConfig;
    use reborn_observability::Observer;
    use std::{
        path::PathBuf,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    struct CountingSink {
        active: Arc<AtomicUsize>,
    }
    impl AudioSink for CountingSink {
        fn parameters(&self) -> Parameters {
            Parameters {
                rate: 44_100,
                period: 512,
                buffer: 4096,
                format: PcmFormat::S32LE,
                channels: 2,
                hardware_mixer_gain_db: None,
                device: "transaction-test".into(),
                fallback: false,
                fallback_reason: String::new(),
            }
        }
        fn write(&mut self, pcm: &[u8]) -> Result<usize, String> {
            Ok(pcm.len() / 8)
        }
        fn discard(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn delay(&self) -> u64 {
            0
        }
    }
    impl Drop for CountingSink {
        fn drop(&mut self) {
            self.active.fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn active_model() -> AppModel {
        let first = Track {
            id: 1,
            path: PathBuf::from("/music/first.flac"),
            ..Default::default()
        };
        let second = Track {
            id: 2,
            path: PathBuf::from("/music/second.flac"),
            ..Default::default()
        };
        let mut model = AppModel::default();
        model.playback = PlaybackState::Playing;
        model.queue = vec![first, second];
        model.queue_entry_ids = vec![QueueEntryId(11), QueueEntryId(12)];
        model.queue_position = 0;
        model.position_ms = 12_345;
        model.generation = 7;
        model.output = AudioOutput::Wired;
        model.settings.volume = 42;
        model.settings.replay_gain = ReplayGainMode::Off;
        model.settings.eq_enabled = false;
        model.settings.eq_bands = vec![EqBand::default()];
        model
    }

    fn test_spec() -> SinkSpec {
        SinkSpec {
            output: AudioOutput::Wired,
            rate: 44_100,
            format: PcmFormat::S32LE,
            physical_bits: 32,
            valid_bits: 32,
            channels: 2,
            layout: "stereo".into(),
            device: "transaction-test".into(),
            codec: None,
            transport_object: None,
            transport_device: None,
            transport: None,
            mode: None,
            transport_generation: 0,
            fallback: false,
            fallback_reason: String::new(),
        }
    }

    #[test]
    fn plan_failure_after_release_never_starts_or_keeps_the_old_sink() {
        let root = std::env::temp_dir().join(format!("reborn-plan-failure-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let open_count = Arc::new(AtomicUsize::new(0));
        let factory_count = open_count.clone();
        let peak = Arc::new(AtomicUsize::new(0));
        let factory_peak = peak.clone();
        let mut playback = Playback::with_sink(
            log,
            root,
            Box::new(move |_, _, _| {
                let active = factory_count.fetch_add(1, Ordering::AcqRel) + 1;
                factory_peak.fetch_max(active, Ordering::AcqRel);
                Ok(Box::new(CountingSink {
                    active: factory_count.clone(),
                }))
            }),
        )
        .unwrap();
        playback
            .load(
                Track::default(),
                vec![],
                0,
                test_spec(),
                1,
                DspConfig::with_volume(42),
                1,
            )
            .unwrap();
        assert_eq!(open_count.load(Ordering::Acquire), 1);
        let mut started = false;
        let error = run_sink_reconfiguration(
            &mut playback,
            2,
            || Err::<SinkSpec, _>("injected sink plan failure".into()),
            |_, _| {
                started = true;
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.contains("injected sink plan failure"));
        assert!(!started);
        assert_eq!(open_count.load(Ordering::Acquire), 0);
        playback.shutdown(3).unwrap();
    }

    #[test]
    fn new_sink_opens_only_after_old_sink_release_acknowledges() {
        let root = std::env::temp_dir().join(format!("reborn-plan-start-{}", std::process::id()));
        let log = Observer::new(&root).unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let factory_active = active.clone();
        let peak = Arc::new(AtomicUsize::new(0));
        let factory_peak = peak.clone();
        let mut playback = Playback::with_sink(
            log,
            root,
            Box::new(move |_, _, _| {
                let count = factory_active.fetch_add(1, Ordering::AcqRel) + 1;
                factory_peak.fetch_max(count, Ordering::AcqRel);
                Ok(Box::new(CountingSink {
                    active: factory_active.clone(),
                }))
            }),
        )
        .unwrap();
        playback
            .load(
                Track::default(),
                vec![],
                0,
                test_spec(),
                1,
                DspConfig::with_volume(42),
                1,
            )
            .unwrap();
        assert_eq!(active.load(Ordering::Acquire), 1);
        let track = Track {
            id: 3,
            path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures/tone.flac"),
            ..Default::default()
        };

        run_sink_reconfiguration(
            &mut playback,
            2,
            || Ok(test_spec()),
            move |playback, spec| {
                playback.start_with_gapless(crate::playback::LoadRequest {
                    track,
                    entry_id: QueueEntryId(3),
                    queue: vec![],
                    queue_entry_ids: vec![],
                    position: 0,
                    spec,
                    generation: 2,
                    dsp: DspConfig::with_volume(42),
                    gapless_enabled: true,
                    id: 2,
                })
            },
        )
        .unwrap();

        assert_eq!(active.load(Ordering::Acquire), 1);
        assert_eq!(peak.load(Ordering::Acquire), 1);
        playback.shutdown(3).unwrap();
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn output_seek_volume_replaygain_and_eq_failures_restore_prior_model() {
        let previous = active_model();
        let transition_generation = 10;
        let mut intended = previous.clone();
        intended.output = AudioOutput::Bluetooth("12:34:56:78:90:AB".into());
        intended.queue_position = 1;
        intended.position_ms = 54_321;
        intended.settings.volume = 80;
        intended.settings.replay_gain = ReplayGainMode::Track;
        intended.settings.eq_enabled = true;
        assert_ne!(intended.output, previous.output);
        assert_ne!(intended.settings.volume, previous.settings.volume);

        let restored = failed_reconfiguration_model(
            previous.clone(),
            "injected start failure",
            true,
            Some(transition_generation),
        );
        assert_eq!(restored.output, previous.output);
        assert_eq!(restored.settings.volume, previous.settings.volume);
        assert_eq!(restored.settings.replay_gain, previous.settings.replay_gain);
        assert_eq!(restored.settings.eq_enabled, previous.settings.eq_enabled);
        assert_eq!(restored.queue_position, previous.queue_position);
        assert_eq!(restored.position_ms, previous.position_ms);
        assert_eq!(restored.queue[0].id, previous.queue[0].id);
        assert_eq!(restored.playback, PlaybackState::Error);
        assert_eq!(
            restored.last_error.as_deref(),
            Some("injected start failure")
        );
        assert_eq!(restored.generation, transition_generation + 1);
    }

    #[test]
    fn pre_release_failure_preserves_the_still_running_model() {
        let previous = active_model();
        let restored = failed_reconfiguration_model(
            previous.clone(),
            "transport observation unavailable",
            false,
            None,
        );

        assert_eq!(restored.playback, PlaybackState::Playing);
        assert_eq!(restored.output, previous.output);
        assert_eq!(restored.settings.volume, previous.settings.volume);
        assert_eq!(restored.generation, previous.generation);
        assert_eq!(restored.last_error, previous.last_error);
    }
}
