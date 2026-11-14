//! Current-track lyrics: bounded asynchronous loading, revision checks and audio-position updates.
use crate::*;
use reborn_core::lyrics::Key;
use reborn_ui::lyrics::State;
mod worker;
use worker::Worker;

#[derive(Default)]
pub(crate) struct Controller {
    key: Option<Key>,
    worker: Option<Worker>,
    pending: bool,
    active: Option<usize>,
}
impl Controller {
    pub(crate) fn quiesce(&mut self) -> bool {
        self.key = None;
        self.pending = false;
        self.active = None;
        self.worker.as_mut().is_none_or(Worker::quiesce)
    }
    fn tick(&mut self, model: &AppModel, view: &mut reborn_ui::lyrics::View) -> bool {
        if model.screen != Screen::Lyrics || model.screen_off {
            if self.key.is_some() {
                self.quiesce();
                view.publish(None, State::Missing);
            }
            return false;
        }
        let Some(track) = model
            .current()
            .filter(|t| reborn_core::lyrics::available(t, &model.sources))
        else {
            let changed = view.key.is_some();
            self.quiesce();
            view.publish(None, State::Missing);
            return changed;
        };
        let mut changed = false;
        if !self
            .key
            .as_ref()
            .is_some_and(|key| key.matches(track, &model.sources))
        {
            if let Some(worker) = &mut self.worker {
                worker.stop();
            }
            self.key = Some(Key::new(track, &model.sources));
            self.pending = false;
            self.active = None;
            view.publish(self.key.clone(), State::Loading);
            changed = true;
        }
        if !self.pending && matches!(view.state, State::Loading) {
            if self.worker.is_none() {
                match Worker::spawn() {
                    Ok(worker) => self.worker = Some(worker),
                    Err(error) => {
                        view.publish(self.key.clone(), State::Failed(error.to_string()));
                        return true;
                    }
                }
            }
            if let (Some(worker), Some(key)) = (&mut self.worker, &self.key) {
                self.pending = worker.request(
                    key.clone(),
                    track.clone(),
                    model.settings.music_directory.clone(),
                );
            }
        }
        if let Some((key, state)) = self.worker.as_mut().and_then(Worker::reply) {
            if self.key.as_ref() == Some(&key) && key.matches(track, &model.sources) {
                view.publish(Some(key), state);
                self.pending = false;
                changed = true;
            }
        }
        let active = view.active(model);
        changed |= active != self.active;
        self.active = active;
        changed
    }
}
impl Runtime {
    pub(crate) fn lyrics_tick(&mut self) {
        if self.lyrics.tick(&self.model, &mut self.ui.lyrics) {
            self.dirty.mark_render();
        }
    }
}

#[cfg(test)]
mod tests;
