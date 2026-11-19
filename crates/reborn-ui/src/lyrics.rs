//! Native lyrics composition. File reads, parsing and wrapping finish before publication.
use crate::{
    canvas::Canvas,
    motion,
    text::{self, Face},
    theme::{color, HEIGHT, WIDTH},
    Ui,
};
use reborn_core::{
    lyrics::{Key, Lyrics},
    Action, AppModel, Effect, PlaybackState, Screen,
};
use std::{cell::Cell, sync::Arc};

const TOP: f32 = 54.;
const BOTTOM: f32 = 326.;
const PITCH: f32 = 30.;

#[derive(Debug)]
struct Row {
    text: String,
    entry: usize,
    y: f32,
}

/// Fixed-metric layout built once on the reader thread, never during drawing.
#[derive(Debug)]
pub struct Prepared {
    pub document: Lyrics,
    rows: Vec<Row>,
    centers: Vec<f32>,
    total: f32,
}
impl Prepared {
    pub fn new(document: Lyrics) -> Self {
        let (mut rows, mut centers, mut y) = (Vec::new(), Vec::new(), 0.);
        for (entry, line) in document.lines.iter().enumerate() {
            let start = y;
            let text = if line.text.is_empty() {
                "Instrumental"
            } else {
                &line.text
            };
            for text in text::wrap(Face::S25, text, 424., 4096) {
                rows.push(Row { text, entry, y });
                y += PITCH;
            }
            centers.push((start + y) / 2.);
            y += 14.;
        }
        Self {
            document,
            rows,
            centers,
            total: y,
        }
    }
    fn target(&self, active: Option<usize>) -> f32 {
        active
            .and_then(|i| self.centers.get(i))
            .copied()
            .map_or(0., |y| y - (BOTTOM - TOP) / 2.)
            .clamp(0., self.maximum())
    }
    fn maximum(&self) -> f32 {
        (self.total - (BOTTOM - TOP)).max(0.)
    }
}

#[derive(Clone, Debug, Default)]
pub enum State {
    Loading,
    #[default]
    Missing,
    Ready(Arc<Prepared>),
    Failed(String),
}
#[derive(Default)]
pub struct View {
    pub key: Option<Key>,
    pub state: State,
    manual: Cell<Option<f32>>,
    shown: Cell<f32>,
    generation: Cell<Option<u64>>,
    position: Cell<Option<u64>>,
    snap: Cell<bool>,
}
impl View {
    pub fn publish(&mut self, key: Option<Key>, state: State) {
        self.key = key;
        self.state = state;
        self.follow();
        self.generation.set(None);
        self.position.set(None);
    }
    fn follow(&self) {
        self.manual.set(None);
        self.snap.set(true);
    }
    fn prepared(&self, model: &AppModel) -> Option<&Prepared> {
        let track = model.current()?;
        if !reborn_core::lyrics::available(track, &model.sources)
            || !self.key.as_ref()?.matches(track, &model.sources)
        {
            return None;
        }
        match &self.state {
            State::Ready(prepared) => Some(prepared),
            _ => None,
        }
    }
    pub fn active(&self, model: &AppModel) -> Option<usize> {
        self.prepared(model)?.document.active(model.position_ms)
    }
    fn browse(&self, model: &AppModel, steps: i32) {
        if let Some(prepared) = self.prepared(model) {
            let offset = self.manual.get().unwrap_or(self.shown.get());
            self.manual.set(Some(
                (offset + steps as f32 * PITCH).clamp(0., prepared.maximum()),
            ));
        }
    }
}

impl Ui {
    /// The new menu entry uses ordinary context-menu selection and navigation.
    pub(crate) fn lyrics_menu(&mut self, model: &mut AppModel, key: &str) -> Option<Effect> {
        if key != "lyrics" || model.screen != Screen::NowPlaying {
            return None;
        }
        Self::go(model, Screen::Lyrics, "");
        self.lyrics.follow();
        Some(Effect::None)
    }
    /// Wheel browsing is local to Lyrics; physical playback actions retain their normal routing.
    pub(crate) fn lyrics_action(&mut self, model: &AppModel, action: Action) -> Option<Effect> {
        if model.screen != Screen::Lyrics
            || model.screen_off
            || model.navigation.modal.is_some()
            || self.keyboard.is_some()
            || self.alert.is_some()
            || self.update_run
            || model.platform.busy.is_some()
            || model.platform.shutting_down.is_some()
        {
            return None;
        }
        match action {
            Action::WheelClockwise(n) => self.lyrics.browse(model, i32::from(n.max(1))),
            Action::WheelCounterClockwise(n) => self.lyrics.browse(model, -i32::from(n.max(1))),
            Action::Select => self.lyrics.follow(),
            Action::SeekForward
            | Action::SeekBackward
            | Action::PreviousTrack
            | Action::NextTrack => {
                self.lyrics.follow();
                return None;
            }
            _ => return None,
        }
        Some(Effect::None)
    }
}

pub(crate) fn draw(c: &mut Canvas, ui: &Ui, model: &AppModel) {
    let view = &ui.lyrics;
    if let Some(prepared) = view.prepared(model) {
        draw_lines(c, ui, model, prepared);
    } else {
        let same = model.current().is_some_and(|t| {
            view.key
                .as_ref()
                .is_some_and(|key| key.matches(t, &model.sources))
        });
        let (title, body) = match &view.state {
            State::Loading if same => ("Loading lyrics…", ""),
            State::Failed(reason) if same => ("Unable to read lyrics", reason.as_str()),
            _ => (
                "No lyrics found",
                "Add an .lrc file beside your song to display synced lyrics.",
            ),
        };
        c.text_center(240., 164., title, Face::S25, color::TEXT);
        for (i, row) in text::wrap(Face::R15, body, 390., 4).iter().enumerate() {
            c.text_center(240., 199. + i as f32 * 21., row, Face::R15, color::MUTED);
        }
    }
    // The same opaque bands as native list windows clip text under the context and controls.
    c.rect(0., 0., WIDTH, TOP, color::BG);
    c.rect(0., BOTTOM, WIDTH, HEIGHT - BOTTOM, color::BG);
    c.text(20., 20., "LYRICS", Face::S10, color::ACCENT);
    let title = model
        .current()
        .map(crate::track_title)
        .unwrap_or("Now Playing");
    c.text_mid(20., 38., 405., title, Face::S13, color::TEXT);
    let status = match model.playback {
        PlaybackState::Playing => "",
        PlaybackState::Paused => "Paused",
        PlaybackState::Buffering => "Starting…",
        PlaybackState::Stopped => "Stopped",
        PlaybackState::Error => "Playback stopped",
    };
    c.text_right(460., 20., status, Face::R11, color::MUTED);
    let hint = if view.prepared(model).is_none() {
        "Back · Now Playing"
    } else if view.manual.get().is_some() {
        "Browsing · Centre to follow"
    } else {
        "Wheel · Browse lyrics     Centre · Follow"
    };
    c.text_center(240., 348., hint, Face::R11, color::MUTED);
}

fn draw_lines(c: &mut Canvas, ui: &Ui, model: &AppModel, prepared: &Prepared) {
    let view = &ui.lyrics;
    let active = prepared.document.active(model.position_ms);
    let changed = view.generation.replace(Some(model.generation)) != Some(model.generation);
    let jumped = view
        .position
        .replace(Some(model.position_ms))
        .is_some_and(|old| old.abs_diff(model.position_ms) > 1500);
    if changed || jumped {
        view.follow();
    }
    let target = view.manual.get().unwrap_or_else(|| prepared.target(active));
    if view.snap.replace(false) || model.playback != PlaybackState::Playing {
        ui.motion.reset("lyrics-scroll", target);
    }
    let offset = ui.ease("lyrics-scroll", None, target, motion::CALM, 0.3);
    view.shown.set(offset);
    let stamp = active.map(|i| prepared.document.lines[i].at_ms);
    let first = prepared.rows.partition_point(|row| row.y + PITCH <= offset);
    for row in &prepared.rows[first..] {
        let y = TOP + row.y - offset;
        if y >= BOTTOM {
            break;
        }
        let at = prepared.document.lines[row.entry].at_ms;
        let color = if stamp == Some(at) {
            color::ACCENT
        } else {
            color::MUTED
        };
        c.text(28., y + 23., &row.text, Face::S25, color);
        if stamp == Some(at) {
            c.rect(14., y + 4., 3., 23., color::ACCENT);
        }
    }
}

#[cfg(test)]
#[path = "lyrics_tests.rs"]
mod tests;
