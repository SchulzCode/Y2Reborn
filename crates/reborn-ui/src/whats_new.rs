//! Settings → System → Software Update → What's New (Y2Linux #128): this version's release notes,
//! read with the wheel. The runtime fetches them (`Effect::FetchWhatsNew`) and hands them in as
//! lines (`reborn_core::whats_new`); wrapping happens once, when they arrive.
use crate::{
    canvas::Canvas,
    motion,
    text::{self, Face},
    theme::{color, HEIGHT, WIDTH},
    Ui,
};
use reborn_core::{whats_new::Line, Action, AppModel, Effect, Screen};
use std::cell::Cell;

const TOP: f32 = 54.;
const BOTTOM: f32 = 326.;
/// One wheel detent moves this far.
const STEP: f32 = 44.;

/// What the page knows about the notes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum State {
    #[default]
    NotAsked,
    Loading,
    Ready(Vec<Line>),
    Failed(String),
}

/// A laid-out row: its text, face, colour, left edge and top within the notes.
#[derive(Debug)]
struct Row {
    text: String,
    face: Face,
    color: u32,
    x: f32,
    y: f32,
    bullet: bool,
}

#[derive(Debug, Default)]
pub struct View {
    state: State,
    rows: Vec<Row>,
    total: f32,
    offset: Cell<f32>,
}

impl View {
    pub fn state(&self) -> &State {
        &self.state
    }
    /// The notes, or why there are none; laid out once here, not while drawing.
    pub fn set(&mut self, state: State) {
        self.rows.clear();
        self.total = 0.;
        self.offset.set(0.);
        if let State::Ready(lines) = &state {
            let mut y = 0.;
            for line in lines {
                let (face, color, x, gap, bullet, text) = match line {
                    Line::Heading(t) => (Face::S15, color::ACCENT, 28., 12., false, t),
                    Line::Bullet(t) => (Face::R15, color::TEXT, 44., 6., true, t),
                    Line::Text(t) => (Face::S15, color::TEXT, 28., 8., false, t),
                };
                y += gap;
                let parts = text::wrap(face, text, WIDTH - x - 28., 64);
                for (i, text) in parts.into_iter().enumerate() {
                    let bullet = bullet && i == 0;
                    self.rows.push(Row {
                        text,
                        face,
                        color,
                        x,
                        y,
                        bullet,
                    });
                    y += 22.;
                }
            }
            self.total = y + 8.;
        }
        self.state = state;
    }
    fn scroll(&self, detents: i32) {
        let room = (self.total - (BOTTOM - TOP)).max(0.);
        let next = (self.offset.get() + detents as f32 * STEP).clamp(0., room);
        self.offset.set(next);
    }
}

impl Ui {
    /// Open the page; the notes are fetched once (a failed fetch is tried again on the next visit).
    pub fn open_whats_new(&mut self, model: &mut AppModel) -> Effect {
        Self::go(model, Screen::WhatsNew, "");
        match self.whats_new.state {
            State::NotAsked | State::Failed(_) => {
                self.whats_new.set(State::Loading);
                Effect::FetchWhatsNew
            }
            _ => Effect::None,
        }
    }

    /// The wheel reads the notes; Back leaves as everywhere.
    pub(crate) fn whats_new_action(&mut self, model: &AppModel, action: Action) -> Option<Effect> {
        if model.screen != Screen::WhatsNew || model.navigation.modal.is_some() {
            return None;
        }
        match action {
            Action::WheelClockwise(n) => self.whats_new.scroll(i32::from(n.max(1))),
            Action::WheelCounterClockwise(n) => self.whats_new.scroll(-i32::from(n.max(1))),
            _ => return None,
        }
        Some(Effect::None)
    }
}

pub(crate) fn draw(c: &mut Canvas, ui: &Ui) {
    let view = &ui.whats_new;
    let empty = |c: &mut Canvas, title: &str, body: &str| {
        c.text_center(240., 164., title, Face::S25, color::TEXT);
        for (i, row) in text::wrap(Face::R15, body, 390., 4).iter().enumerate() {
            c.text_center(240., 199. + i as f32 * 21., row, Face::R15, color::MUTED);
        }
    };
    match view.state() {
        State::NotAsked | State::Loading => {
            empty(c, "Loading…", "The notes come from the internet.")
        }
        State::Failed(reason) => empty(c, "No Notes Yet", reason),
        State::Ready(_) if view.rows.is_empty() => {
            empty(c, "No Notes", "This version has no notes for the player.")
        }
        State::Ready(_) => {
            let offset = ui.ease(
                "whats-new-scroll",
                None,
                view.offset.get(),
                motion::CALM,
                0.3,
            );
            for row in &view.rows {
                let y = TOP + row.y - offset;
                if y + 22. < TOP || y >= BOTTOM {
                    continue;
                }
                if row.bullet {
                    c.text(30., y + 16., "•", row.face, color::MUTED);
                }
                c.text(row.x, y + 16., &row.text, row.face, row.color);
            }
        }
    }
    // Bands as in every list window: text scrolls under the title and the hint.
    c.rect(0., 0., WIDTH, TOP, color::BG);
    c.rect(0., BOTTOM, WIDTH, HEIGHT - BOTTOM, color::BG);
    c.text(20., 20., "WHAT'S NEW", Face::S10, color::ACCENT);
    let version = reborn_core::whats_new::release_title(reborn_core::BUILD_LABEL)
        .unwrap_or_else(|| "This version".into());
    c.text_mid(20., 38., 440., &version, Face::S13, color::TEXT);
    let hint = if matches!(view.state(), State::Ready(_)) {
        "Wheel · Read     Back · Software Update"
    } else {
        "Back · Software Update"
    };
    c.text_center(240., 348., hint, Face::R11, color::MUTED);
}

#[cfg(test)]
#[path = "whats_new_tests.rs"]
mod tests;
