//! UI motion (Beta 7.2): values that ease toward their targets in time, as
//! Cover Flow's glide does. Every motion can be retargeted at any moment, so
//! input is never delayed; it looks the same at any frame rate; it settles
//! within a fraction of a second, after which nothing is redrawn. Off in
//! previews and tests (`Ui::ui_motion`), where every value is its target.
use crate::canvas::{Canvas, Moving};
use reborn_core::AppModel;
use reborn_graphics::Quad;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    time::Instant,
};

/// Rates of the exponential approach: ≈95 % of the way in 3/rate seconds.
pub const QUICK: f32 = 24.; // ≈125 ms: focus, switches
pub const CALM: f32 = 16.; // ≈190 ms: scrolling, sheets, fades

struct Spring {
    value: f32,
    target: f32,
    last: Instant,
    frame: u64,
}

#[derive(Default)]
pub struct Motion {
    springs: RefCell<HashMap<String, Spring>>,
    frame: Cell<u64>,
    /// The last frame that drew a value still on its way.
    unsettled: Cell<u64>,
    /// The last frame that drew continuous motion (the playing wave).
    live: Cell<u64>,
}

impl Motion {
    /// A new frame: values a frame no longer draws are forgotten soon after.
    pub fn begin_frame(&self) {
        let frame = self.frame.get() + 1;
        self.frame.set(frame);
        if frame.is_multiple_of(64) {
            self.springs
                .borrow_mut()
                .retain(|_, s| frame.saturating_sub(s.frame) < 64);
        }
    }

    /// `key`'s value this frame, on its way to `target`. A key seen for the
    /// first time starts at `start` (an entrance) or, without one, at its
    /// target (nothing to glide from).
    pub fn ease(
        &self,
        key: &str,
        start: Option<f32>,
        target: f32,
        rate: f32,
        settle: f32,
        now: Instant,
    ) -> f32 {
        let frame = self.frame.get();
        let mut springs = self.springs.borrow_mut();
        let spring = springs.entry(key.to_owned()).or_insert(Spring {
            value: start.unwrap_or(target),
            target,
            last: now,
            frame,
        });
        // A key not drawn for a while starts over instead of gliding from
        // where it was left (a screen opened again later).
        if frame.saturating_sub(spring.frame) > 2 {
            spring.value = start.unwrap_or(target);
            spring.last = now;
        }
        let dt = now
            .saturating_duration_since(spring.last)
            .as_secs_f32()
            .min(0.1);
        spring.last = now;
        spring.frame = frame;
        spring.target = target;
        spring.value += (target - spring.value) * (1. - (-rate * dt).exp());
        if (target - spring.value).abs() <= settle {
            spring.value = target;
        } else {
            self.unsettled.set(frame);
        }
        spring.value
    }

    /// Put `key` at `value` at once (a jump too long to glide across).
    pub fn reset(&self, key: &str, value: f32) {
        if let Some(s) = self.springs.borrow_mut().get_mut(key) {
            s.value = value;
        }
    }

    /// This frame draws motion that continues by itself (the playing wave).
    pub fn mark_live(&self) {
        self.live.set(self.frame.get());
    }

    /// Something drawn in the latest frame is still moving: keep the
    /// panel's full frame rate for the next one.
    pub fn moving(&self) -> bool {
        self.settling() || {
            let frame = self.frame.get();
            frame > 0 && self.live.get() == frame
        }
    }

    /// A value is still on its way (motion a button started), as opposed to
    /// motion that goes on by itself (the playing wave).
    pub fn settling(&self) -> bool {
        let frame = self.frame.get();
        frame > 0 && self.unsettled.get() == frame
    }
}

/// Ease the frame's marked motion (Reborn's own look; a skin draws its own).
pub(crate) fn apply(ui: &crate::Ui, m: &AppModel, c: &mut Canvas) {
    if !ui.ui_motion || c.moving.is_empty() {
        c.moving.clear();
        return;
    }
    let now = Instant::now();
    let screen = format!("{:?}:{}", m.screen, m.navigation.filter);
    let marks = std::mem::take(&mut c.moving);
    let ease = |key: &str, start: Option<f32>, target: f32, rate: f32, settle: f32| {
        ui.motion.ease(key, start, target, rate, settle, now)
    };
    // A focus highlight moves on its own; a list's scroll leaves it be.
    let focus: Vec<(usize, usize)> = marks
        .iter()
        .filter_map(|k| match *k {
            Moving::Focus { start, end, .. } => Some((start, end)),
            _ => None,
        })
        .collect();
    let in_focus = |i: usize| focus.iter().any(|&(s, e)| i >= s && i < e);
    let mut band: Option<(f32, usize)> = None;
    for mark in &marks {
        match *mark {
            Moving::List {
                start,
                end,
                top,
                bottom,
                row_h,
                ..
            }
            | Moving::Scroll {
                start,
                end,
                top,
                bottom,
                row_h,
                ..
            } => {
                let key = format!("list:{screen}");
                let target = match *mark {
                    Moving::List { first, .. } => first as f32 * row_h,
                    Moving::Scroll { offset, .. } => offset,
                    _ => unreachable!(),
                };
                let mut v = ease(&key, None, target, CALM, 0.3);
                // A long jump (fast wheel) glides only its last row.
                if (target - v).abs() > 2. * row_h {
                    v = target - (target - v).signum() * row_h;
                    ui.motion.reset(&key, v);
                }
                let dy = target - v;
                for i in (start..end).filter(|&i| !in_focus(i)) {
                    let q = &mut c.draw[i];
                    shift_y(q, dy);
                    // The extra rows show only while they slide in.
                    if q.y >= bottom || q.y + q.h <= top {
                        q.color &= 0xFFFF_FF00;
                    }
                }
                if dy != 0. {
                    band = Some((top, end));
                }
            }
            Moving::Focus {
                start,
                end,
                y,
                layer,
            } => {
                let key = format!("focus:{layer}:{screen}");
                let mut v = ease(&key, None, y, QUICK, 0.3);
                if (y - v).abs() > 160. {
                    ui.motion.reset(&key, y);
                    v = y;
                }
                for q in &mut c.draw[start..end] {
                    shift_y(q, v - y);
                }
            }
            Moving::Sheet {
                scrim,
                start,
                end,
                height,
            } => {
                let from = height + 16.;
                let dy = ease(&format!("sheet:{screen}"), Some(from), 0., CALM, 0.5);
                fade_all(&mut c.draw[scrim..start], 1. - dy / from);
                for q in &mut c.draw[start..end] {
                    shift_y(q, dy);
                }
            }
            Moving::Dialog { scrim, start, end } => {
                let a = ease("dialog", Some(0.), 1., CALM * 1.3, 0.01);
                fade_all(&mut c.draw[scrim..end], a);
                for q in &mut c.draw[start..end] {
                    shift_y(q, (1. - a) * 14.);
                }
            }
            Moving::Toast { start, end } => {
                let a = ease("toast", Some(0.), 1., CALM, 0.01);
                fade_all(&mut c.draw[start..end], a);
                for q in &mut c.draw[start..end] {
                    shift_y(q, -(1. - a) * 24.);
                }
            }
            Moving::Switch {
                start,
                knob,
                end,
                on,
                x,
                y,
            } => {
                let goal = if on { 1. } else { 0. };
                let t = ease(&format!("switch:{screen}:{x}:{y}"), None, goal, QUICK, 0.01);
                let track = crate::theme::mix_rgba(
                    crate::theme::color::GLASS_STRONG,
                    crate::theme::color::ACCENT,
                    t,
                );
                for q in &mut c.draw[start..knob] {
                    q.color = track;
                }
                for q in &mut c.draw[knob..end] {
                    q.x += (t - goal) * 18.;
                }
            }
            Moving::StatusBar { .. } | Moving::Corners { .. } => {}
        }
    }
    // Round covers take the colour under their corners where the focus
    // highlight is now, not where it is headed.
    let pill = marks
        .iter()
        .filter_map(|k| match *k {
            Moving::Focus {
                start,
                end,
                layer: 0,
                ..
            } => c.draw[start..end]
                .iter()
                .map(|q| (q.y, q.y + q.h))
                .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1))),
            _ => None,
        })
        .next();
    for mark in &marks {
        if let Moving::Corners {
            start,
            end,
            on,
            off,
        } = *mark
        {
            for q in &mut c.draw[start..end] {
                let mid = q.y + q.h / 2.;
                let covered = pill.is_some_and(|(top, bottom)| mid > top && mid < bottom);
                q.color = if covered { on } else { off };
            }
        }
    }
    // Rows that scrolled under the status bar stay under it: a band of the
    // background and the bar again, right above the list.
    if let Some((top, end)) = band {
        if let Some(Moving::StatusBar { start: s, end: e }) = marks
            .iter()
            .find(|k| matches!(k, Moving::StatusBar { .. }))
            .copied()
        {
            if e <= end {
                let bar: Vec<Quad> = c.draw.drain(s..e).collect();
                let at = end - (e - s);
                let mut cover = vec![Quad::rect(
                    0.,
                    0.,
                    crate::theme::WIDTH,
                    top,
                    crate::theme::color::BG,
                )];
                cover.extend(bar);
                c.draw.splice(at..at, cover);
            }
        }
    }
}

fn shift_y(q: &mut Quad, dy: f32) {
    q.y += dy;
    if let Some(w) = q.warp.as_mut() {
        for corner in &mut w.corners {
            corner[1] += dy;
        }
    }
}

/// Move every quad of a frame sideways (the screen slide), warped ones too.
pub fn shift(quads: &mut [Quad], dx: f32) {
    for q in quads {
        q.x += dx;
        if let Some(w) = q.warp.as_mut() {
            for corner in &mut w.corners {
                corner[0] += dx;
            }
        }
    }
}

/// Scale every quad's opacity (a fade), warped corners included.
pub fn fade_all(quads: &mut [Quad], factor: f32) {
    let f = |c: u32| crate::theme::fade(c, factor);
    for q in quads {
        q.color = f(q.color);
        if let Some(w) = q.warp.as_mut() {
            for c in &mut w.colors {
                *c = f(*c);
            }
        }
    }
}

/// A phase that turns `hz` times a second, for motion that goes on by
/// itself (the wave). It wraps after five turns: the wave's terms run at
/// 1, 1.6 and 0.6 times the phase, and all meet again there, so the wrap is
/// seamless while the value stays small enough for f32.
pub fn phase(hz: f32) -> f32 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let turns = START.get_or_init(Instant::now).elapsed().as_secs_f64() * f64::from(hz);
    (turns % 5.) as f32 * std::f32::consts::TAU
}

/// Ease-out cubic for timed motion (the screen slide).
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    1. - (1. - t).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn values_start_at_their_target_glide_when_retargeted_and_settle() {
        let m = Motion::default();
        let t0 = Instant::now();
        m.begin_frame();
        assert_eq!(m.ease("y", None, 100., QUICK, 0.3, t0), 100.);
        assert!(!m.moving());
        m.begin_frame();
        let mid = m.ease("y", None, 150., QUICK, 0.3, t0 + Duration::from_millis(16));
        assert!(mid > 100. && mid < 150., "{mid}");
        assert!(m.moving());
        // Retargeted mid-way: it turns around from where it is.
        m.begin_frame();
        let back = m.ease("y", None, 100., QUICK, 0.3, t0 + Duration::from_millis(32));
        assert!(back < mid);
        for i in 3..40 {
            m.begin_frame();
            m.ease(
                "y",
                None,
                100.,
                QUICK,
                0.3,
                t0 + Duration::from_millis(16 * i),
            );
        }
        assert!(!m.moving(), "settled after ≈0.6 s");
    }

    #[test]
    fn entrances_start_at_their_start_and_forgotten_keys_start_over() {
        let m = Motion::default();
        let t0 = Instant::now();
        m.begin_frame();
        assert_eq!(m.ease("sheet", Some(200.), 0., CALM, 0.3, t0), 200.);
        for _ in 0..5 {
            m.begin_frame();
        }
        assert_eq!(
            m.ease(
                "sheet",
                Some(200.),
                0.,
                CALM,
                0.3,
                t0 + Duration::from_secs(1)
            ),
            200.
        );
    }
}
