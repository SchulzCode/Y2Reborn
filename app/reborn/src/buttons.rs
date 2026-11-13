//! Customize > Buttons in the runtime: a newly saved layout that was not kept within its trial
//! goes back to the one before (`Ui::buttons_trial_expired`), also when its sheet was left with
//! Back or the new buttons could not reach Keep.
use crate::Runtime;
use std::time::Instant;

impl Runtime {
    pub(crate) fn buttons_trial_tick(&mut self) {
        if let Some(previous) = self
            .ui
            .buttons_trial_expired(&mut self.model, Instant::now())
        {
            self.model.settings.buttons = previous;
            self.checkpoint();
            self.dirty.mark_render();
        }
    }
}
