//! Cover Flow motion: a continuous scroll position that eases toward the
//! focused album, and the cover's flight into (and back out of) the album page.
//! Time-based, so the glide looks the same at any frame rate; frozen in
//! previews and tests (`Ui::motion_fixed`).
use reborn_core::Screen;
use std::time::Instant;

/// Exponential approach rate: ≈95 % of the way in 200 ms.
const RATE: f32 = 15.;
/// Close enough to snap and stop redrawing (well under a pixel).
const SETTLED: f32 = 0.01;
/// The cover's flight between Cover Flow and the album page.
pub const TRANSITION_MS: f32 = 300.;

#[derive(Default)]
pub struct Flow {
    /// Fractional index of the album at the centre.
    pub pos: f32,
    target: usize,
    last: Option<Instant>,
    /// The album list this position belongs to (screen + filter).
    stamp: Option<(Screen, String)>,
    /// When the centre came to rest (for loading the sharp cover once).
    settled_at: Option<Instant>,
}

impl Flow {
    /// Advance toward `target` for this frame and return the position.
    pub fn advance(
        &mut self,
        stamp: (Screen, String),
        target: usize,
        fixed: bool,
        now: Instant,
    ) -> f32 {
        if self.stamp.as_ref() != Some(&stamp) {
            // A new list starts at its focus, without gliding across it.
            self.stamp = Some(stamp);
            self.pos = target as f32;
            self.last = Some(now);
            self.target = target;
            self.settled_at = Some(now);
            return self.pos;
        }
        self.target = target;
        let goal = target as f32;
        if fixed {
            self.pos = goal;
            self.last = Some(now);
            return self.pos;
        }
        let dt = self
            .last
            .map_or(0., |last| now.saturating_duration_since(last).as_secs_f32())
            .min(0.1);
        self.last = Some(now);
        let before = self.pos;
        self.pos += (goal - self.pos) * (1. - (-RATE * dt).exp());
        if (goal - self.pos).abs() < SETTLED {
            self.pos = goal;
        }
        if self.pos != before || self.settled_at.is_none() {
            self.settled_at = (self.pos == goal).then_some(now);
        }
        self.pos
    }
    /// Still gliding toward the focus.
    pub fn moving(&self) -> bool {
        self.stamp.is_some() && self.pos != self.target as f32
    }
    /// The centre has rested for at least `ms`.
    pub fn rested(&self, ms: u128, now: Instant) -> bool {
        self.settled_at
            .is_some_and(|at| now.saturating_duration_since(at).as_millis() >= ms)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransitionKind {
    /// Cover Flow → album page.
    Open,
    /// Album page → Cover Flow.
    Close,
}

#[derive(Clone, Copy, Debug)]
pub struct Transition {
    pub kind: TransitionKind,
    pub start: Instant,
}

impl Transition {
    /// 0..1 progress with ease-out-cubic; `None` once finished.
    pub fn progress(&self, now: Instant) -> Option<f32> {
        let t = now.saturating_duration_since(self.start).as_secs_f32() * 1000. / TRANSITION_MS;
        (t < 1.).then(|| 1. - (1. - t.max(0.)).powi(3))
    }
}

/// One cover's geometry in Cover Flow at signed offset `d` from the centre:
/// a square of side `size` rotated about its vertical axis and pushed back,
/// projected with focal length `F`. Corners TL, TR, BR, BL in screen pixels.
pub const F: f32 = 300.;
pub const CENTRE_X: f32 = 240.;
pub const CENTRE_Y: f32 = 132.;
pub const SIZE: f32 = 150.;
/// Distance from the centre to the first side cover, then between side covers.
const FIRST: f32 = 118.;
const STEP: f32 = 44.;
const MAX_ANGLE: f32 = 1.2; // radians, ≈69°
const DEPTH: f32 = 110.;

/// Where the centre of the cover `d` places from the middle stands, in screen pixels.
pub fn centre_x(d: f32) -> f32 {
    let x = if d.abs() <= 1. {
        d * FIRST
    } else {
        d.signum() * (FIRST + (d.abs() - 1.) * STEP)
    };
    CENTRE_X + x
}

pub fn cover_corners(d: f32) -> [[f32; 2]; 4] {
    let near = d.abs().min(1.);
    let x = centre_x(d) - CENTRE_X;
    // Left covers face right: their outer (left) edge comes toward the viewer.
    let angle = -d.clamp(-1., 1.) * MAX_ANGLE;
    let (sin, cos) = angle.sin_cos();
    let z_off = DEPTH * near;
    let half = SIZE / 2.;
    let project = |lx: f32, ly: f32| {
        let px = lx * cos;
        let pz = lx * sin + z_off;
        let k = F / (F + pz);
        [CENTRE_X + x + px * k, CENTRE_Y + ly * k]
    };
    [
        project(-half, -half),
        project(half, -half),
        project(half, half),
        project(-half, half),
    ]
}

/// The mirror image below a cover: texture top maps to the reflection's
/// bottom, so the cover's bottom edge touches its own reflection.
pub fn reflection_corners(cover: [[f32; 2]; 4]) -> [[f32; 2]; 4] {
    let [tl, tr, br, bl] = cover;
    let mirror = |top: [f32; 2], bottom: [f32; 2]| [bottom[0], 2. * bottom[1] - top[1]];
    [mirror(tl, bl), mirror(tr, br), br, bl]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn position_glides_to_focus_and_settles() {
        let mut f = Flow::default();
        let t0 = Instant::now();
        let stamp = (Screen::Albums, String::new());
        assert_eq!(f.advance(stamp.clone(), 0, false, t0), 0.);
        // Five albums ahead: smooth, monotonic, done within half a second.
        let mut last = 0.;
        for frame in 1..=30 {
            let p = f.advance(
                stamp.clone(),
                5,
                false,
                t0 + Duration::from_millis(frame * 16),
            );
            assert!(p >= last && p <= 5.);
            last = p;
        }
        assert_eq!(last, 5.);
        assert!(!f.moving());
        // A new list jumps straight to its focus.
        assert_eq!(
            f.advance((Screen::Albums, "genre:Jazz".into()), 3, false, t0),
            3.
        );
        // Frozen motion (previews/tests) is always at the focus.
        assert_eq!(
            f.advance((Screen::Albums, "genre:Jazz".into()), 7, true, t0),
            7.
        );
    }

    #[test]
    fn covers_are_symmetric_and_side_covers_recede() {
        let centre = cover_corners(0.);
        assert!((centre[1][0] - centre[0][0] - SIZE).abs() < 0.01);
        let left = cover_corners(-1.);
        let right = cover_corners(1.);
        for i in 0..4 {
            // Mirror images around the centre line.
            let j = [1, 0, 3, 2][i];
            assert!((left[i][0] + right[j][0] - 2. * CENTRE_X).abs() < 0.01);
            assert!((left[i][1] - right[j][1]).abs() < 0.01);
        }
        // A left cover's outer edge is taller than its inner edge.
        assert!(left[3][1] - left[0][1] > left[2][1] - left[1][1]);
        // Side covers sit inside the screen out to four places.
        for d in -4..=4 {
            for p in cover_corners(d as f32) {
                assert!(p[0] > -60. && p[0] < 540.);
            }
        }
        let r = reflection_corners(centre);
        assert_eq!(r[3], centre[3]);
        assert!((r[0][1] - (2. * centre[3][1] - centre[0][1])).abs() < 0.01);
    }

    #[test]
    fn transition_eases_out_and_ends() {
        let t0 = Instant::now();
        let t = Transition {
            kind: TransitionKind::Open,
            start: t0,
        };
        assert_eq!(t.progress(t0), Some(0.));
        let mid = t.progress(t0 + Duration::from_millis(150)).unwrap();
        assert!(mid > 0.5 && mid < 1.);
        assert!(t.progress(t0 + Duration::from_millis(301)).is_none());
    }
}
