//! Settings → System → Software Update → What's New (Y2Linux #128): this version's release notes,
//! fetched once over Wi-Fi and kept in `state/whats-new.json`, and opened by themselves once after
//! an update when the listener turned that on.
use crate::*;
use reborn_core::whats_new::{lines, release_title, Line};
use reborn_ui::whats_new::State;

const CACHE: &str = "state/whats-new.json";
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(serde::Serialize, serde::Deserialize)]
struct Cached {
    version: String,
    lines: Vec<Line>,
}

/// The notes kept for this version, if any.
fn cached(root: &std::path::Path) -> Option<Vec<Line>> {
    let text = fs::read(root.join(CACHE)).ok()?;
    let kept: Cached = serde_json::from_slice(&text).ok()?;
    (kept.version == VERSION).then_some(kept.lines)
}

impl Runtime {
    /// Show this version's notes: kept ones at once, else fetched in the background.
    pub(crate) fn fetch_whats_new(&mut self) {
        if let Some(kept) = cached(&self.root) {
            self.ui.whats_new.set(State::Ready(kept));
            return;
        }
        if self.whats_new_reply.is_some() {
            return;
        }
        let Some(title) = release_title(reborn_core::BUILD_LABEL) else {
            self.ui
                .whats_new
                .set(State::Failed("This build has no release notes.".into()));
            return;
        };
        let (send, receive) = std::sync::mpsc::channel();
        self.whats_new_reply = Some(receive);
        let spawned = std::thread::Builder::new()
            .name("whats-new".into())
            .spawn(move || {
                let _ = send.send(reborn_platform::whats_new::fetch(&title).map(|b| lines(&b)));
            });
        if spawned.is_err() {
            self.whats_new_reply = None;
        }
    }

    /// Software Update → Show What's New After Updates. Turned on, it starts with the next update,
    /// not with the version already running.
    pub(crate) fn set_whats_new_after_update(&mut self, on: bool) {
        self.model.settings.whats_new_after_update = on;
        if on {
            self.model.settings.whats_new_seen = VERSION.into();
        }
        self.checkpoint();
    }

    /// Each pass: the fetch's answer, and after an update (when asked for) the notes once.
    pub(crate) fn whats_new_tick(&mut self) {
        let settings = &self.model.settings;
        let due = settings.whats_new_after_update && settings.whats_new_seen != VERSION;
        let online = matches!(self.ui.wifi.status, reborn_ui::WifiStatus::Connected(_));
        if due && !self.whats_new_asked && (online || cached(&self.root).is_some()) {
            self.whats_new_asked = true;
            self.fetch_whats_new();
        }
        if let Some(answer) = self
            .whats_new_reply
            .as_ref()
            .and_then(|r| r.try_recv().ok())
        {
            self.whats_new_reply = None;
            match answer {
                Ok(lines) => {
                    let kept = Cached {
                        version: VERSION.into(),
                        lines: lines.clone(),
                    };
                    if let Ok(bytes) = serde_json::to_vec(&kept) {
                        let _ = reborn_core::atomic_write(&self.root.join(CACHE), &bytes);
                    }
                    self.ui.whats_new.set(State::Ready(lines));
                }
                Err(reason) => self.ui.whats_new.set(State::Failed(reason)),
            }
            self.dirty.mark_render();
        }
        if due && matches!(self.ui.whats_new.state(), State::Ready(_)) {
            self.show_whats_new_once();
        }
    }

    /// Open the notes where nothing is interrupted (Home, Now Playing), else say where they are.
    fn show_whats_new_once(&mut self) {
        let m = &self.model;
        let quiet = matches!(
            m.screen,
            reborn_core::Screen::Home | reborn_core::Screen::NowPlaying
        ) && m.navigation.modal.is_none()
            && !m.screen_off;
        if quiet {
            self.ui.open_whats_new(&mut self.model);
        } else {
            self.ui
                .flash("What's New is in Settings › System › Software Update");
        }
        self.model.settings.whats_new_seen = VERSION.into();
        self.checkpoint();
        self.dirty.mark_render();
    }
}
