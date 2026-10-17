//! Drawing primitives: flat rectangles, pixel-exact rounded corners, discs,
//! rings, glows, gradients, icons and text. Everything is a `Quad`; the native
//! renderer samples coverage masks, so one tint colour draws any shape.
use crate::{
    maskdata as mask,
    text::{self, Face, Source},
};
use reborn_graphics::{Quad, Tex};

/// The icon catalogue; every glyph the asset script draws, used or not.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Back,
    Chevron,
    Check,
    Close,
    Play,
    Pause,
    Prev,
    Next,
    PlayLarge,
    PauseLarge,
    PrevLarge,
    NextLarge,
    Shuffle,
    Repeat,
    RepeatOne,
    Gapless,
    Eq,
    Normalise,
    Bluetooth,
    Wifi,
    Lock,
    Bolt,
    Folder,
    Note,
    Speaker,
    Headphones,
    Sd,
    Usb,
    Warning,
    Info,
    Sun,
    Drive,
    Power,
    Download,
    Refresh,
    Search,
    Vinyl,
    Person,
    Crossfade,
    Shift,
    ShiftOn,
    Backspace,
    Eye,
    EyeOff,
    Star,
    StarOn,
    Globe,
    Trend,
    Broadcast,
    Heart,
    HeartOn,
    Plus,
    Palette,
}

impl Icon {
    fn src(self) -> [u16; 4] {
        match self {
            Self::Back => mask::BACK,
            Self::Chevron => mask::CHEVRON,
            Self::Check => mask::CHECK,
            Self::Close => mask::CLOSE,
            Self::Play => mask::PLAY,
            Self::Pause => mask::PAUSE,
            Self::Prev => mask::PREV,
            Self::Next => mask::NEXT,
            Self::PlayLarge => mask::PLAY_L,
            Self::PauseLarge => mask::PAUSE_L,
            Self::PrevLarge => mask::PREV_L,
            Self::NextLarge => mask::NEXT_L,
            Self::Shuffle => mask::SHUFFLE,
            Self::Repeat => mask::REPEAT,
            Self::RepeatOne => mask::REPEAT_ONE,
            Self::Gapless => mask::GAPLESS,
            Self::Eq => mask::EQ,
            Self::Normalise => mask::NORM,
            Self::Bluetooth => mask::BLUETOOTH,
            Self::Wifi => mask::WIFI,
            Self::Lock => mask::LOCK,
            Self::Bolt => mask::BOLT,
            Self::Folder => mask::FOLDER,
            Self::Note => mask::NOTE,
            Self::Speaker => mask::SPEAKER,
            Self::Headphones => mask::HEADPHONES,
            Self::Sd => mask::SD,
            Self::Usb => mask::USB,
            Self::Warning => mask::WARNING,
            Self::Info => mask::INFO,
            Self::Sun => mask::SUN,
            Self::Drive => mask::DRIVE,
            Self::Power => mask::POWER,
            Self::Download => mask::DOWNLOAD,
            Self::Refresh => mask::REFRESH,
            Self::Search => mask::SEARCH,
            Self::Vinyl => mask::VINYL,
            Self::Person => mask::PERSON,
            Self::Crossfade => mask::CROSSFADE,
            Self::Shift => mask::SHIFT,
            Self::ShiftOn => mask::SHIFT_ON,
            Self::Backspace => mask::BACKSPACE,
            Self::Eye => mask::EYE,
            Self::EyeOff => mask::EYE_OFF,
            Self::Star => mask::STAR,
            Self::StarOn => mask::STAR_ON,
            Self::Globe => mask::GLOBE,
            Self::Trend => mask::TREND,
            Self::Broadcast => mask::BROADCAST,
            Self::Heart => mask::HEART,
            Self::HeartOn => mask::HEART_ON,
            Self::Plus => mask::PLUS,
            Self::Palette => mask::PALETTE,
        }
    }
}

/// Quarter index within a corner table.
#[derive(Clone, Copy)]
enum Corner {
    TopLeft = 0,
    TopRight = 1,
    BottomLeft = 2,
    BottomRight = 3,
}

fn radius_slot(r: f32) -> usize {
    let r = r.round().max(1.);
    mask::CORNER_RADII
        .iter()
        .enumerate()
        .min_by(|a, b| {
            (f32::from(*a.1) - r)
                .abs()
                .total_cmp(&(f32::from(*b.1) - r).abs())
        })
        .map_or(0, |(i, _)| i)
}

pub struct Canvas {
    pub draw: Vec<Quad>,
    /// Where shared components drew things that move (Beta 7.2); one pass
    /// eases them once the frame is built (`motion::apply`).
    pub moving: Vec<Moving>,
}

/// A run of quads `[start, end)` that one kind of motion moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Moving {
    /// A focus highlight at `y` (it glides to the next row); `layer` 0 is
    /// the screen, 1 a sheet over it.
    Focus {
        start: usize,
        end: usize,
        y: f32,
        layer: u8,
    },
    /// The status bar, kept above rows that scroll under it.
    StatusBar { start: usize, end: usize },
    /// List rows in the window `top..bottom`, `row_h` apart, starting at row
    /// `first` (one row more is drawn on each side to scroll into view;
    /// whatever is wholly outside the window is hidden).
    List {
        start: usize,
        end: usize,
        top: f32,
        bottom: f32,
        row_h: f32,
        first: usize,
    },
    /// A sheet `height` tall rising from the bottom over its scrim.
    Sheet {
        scrim: usize,
        start: usize,
        end: usize,
        height: f32,
    },
    /// A dialog fading in over its scrim.
    Dialog {
        scrim: usize,
        start: usize,
        end: usize,
    },
    /// A message dropping in at the top.
    Toast { start: usize, end: usize },
    /// A switch: track quads `[start, knob)`, knob quads `[knob, end)`.
    Switch {
        start: usize,
        knob: usize,
        end: usize,
        on: bool,
        x: f32,
        y: f32,
    },
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

impl Canvas {
    pub fn new() -> Self {
        Self {
            draw: Vec::with_capacity(1200),
            moving: Vec::new(),
        }
    }
    pub fn from_quads(draw: Vec<Quad>) -> Self {
        Self {
            draw,
            moving: Vec::new(),
        }
    }
    /// Note quads that move (see `Moving`).
    pub fn mark_moving(&mut self, mark: Moving) {
        self.moving.push(mark);
    }
    pub fn finish(self) -> Vec<Quad> {
        self.draw
    }
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: u32) {
        if w > 0. && h > 0. && color & 0xFF != 0 {
            self.draw.push(Quad::rect(x, y, w, h, color));
        }
    }
    fn masked(&mut self, src: [u16; 4], x: f32, y: f32, w: f32, h: f32, color: u32) {
        // A sprite the atlas does not have (zero-sized) draws nothing.
        if w > 0. && h > 0. && src[2] > 0 && src[3] > 0 && color & 0xFF != 0 {
            self.draw
                .push(Quad::masked(Tex::Mask, src, x, y, w, h, color));
        }
    }
    /// Mark the most recent quad as the screen's focus target.
    pub fn mark_focus(&mut self, since: usize) {
        if let Some(q) = self.draw.get_mut(since) {
            q.focus_target = true;
        }
    }

    // ----------------------------------------------------------------- shapes

    /// Filled rounded rectangle built from non-overlapping pieces, so
    /// translucent colours blend once everywhere.
    pub fn rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: u32) {
        let (x, y, w, h) = (x.round(), y.round(), w.round(), h.round());
        let r = r.min(w / 2.).min(h / 2.).floor();
        if r < 2. {
            return self.rect(x, y, w, h, color);
        }
        let slot = radius_slot(r);
        let r = f32::from(mask::CORNER_RADII[slot]);
        let c = &mask::CORNERS[slot];
        self.rect(x + r, y, w - 2. * r, r, color);
        self.rect(x, y + r, w, h - 2. * r, color);
        self.rect(x + r, y + h - r, w - 2. * r, r, color);
        self.masked(c[Corner::TopLeft as usize], x, y, r, r, color);
        self.masked(c[Corner::TopRight as usize], x + w - r, y, r, r, color);
        self.masked(c[Corner::BottomLeft as usize], x, y + h - r, r, r, color);
        self.masked(
            c[Corner::BottomRight as usize],
            x + w - r,
            y + h - r,
            r,
            r,
            color,
        );
    }

    /// Rounded outline of `width` 1, 2 or 3 pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn rrect_outline(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        width: f32,
        color: u32,
    ) {
        let (x, y, w, h) = (x.round(), y.round(), w.round(), h.round());
        let t = width.round().clamp(1., 3.);
        let r = r.min(w / 2.).min(h / 2.).floor();
        if r < 3. {
            self.rect(x, y, w, t, color);
            self.rect(x, y + h - t, w, t, color);
            self.rect(x, y + t, t, h - 2. * t, color);
            self.rect(x + w - t, y + t, t, h - 2. * t, color);
            return;
        }
        let slot = radius_slot(r);
        let r = f32::from(mask::CORNER_RADII[slot]);
        let wi = (t as usize - 1).min(mask::CORNER_RING_WIDTHS.len() - 1);
        let c = &mask::CORNER_RINGS[slot][wi];
        self.rect(x + r, y, w - 2. * r, t, color);
        self.rect(x + r, y + h - t, w - 2. * r, t, color);
        self.rect(x, y + r, t, h - 2. * r, color);
        self.rect(x + w - t, y + r, t, h - 2. * r, color);
        self.masked(c[Corner::TopLeft as usize], x, y, r, r, color);
        self.masked(c[Corner::TopRight as usize], x + w - r, y, r, r, color);
        self.masked(c[Corner::BottomLeft as usize], x, y + h - r, r, r, color);
        self.masked(
            c[Corner::BottomRight as usize],
            x + w - r,
            y + h - r,
            r,
            r,
            color,
        );
    }

    /// Hide the corners of the square at (x, y, size) behind `bg`, turning a
    /// picture drawn there into a rounded one. `bg` must be what lies behind.
    pub fn round_off(&mut self, x: f32, y: f32, size: f32, r: f32, bg: u32) {
        let slot = radius_slot(r.min(size / 2.));
        let r = f32::from(mask::CORNER_RADII[slot]);
        let c = &mask::INVERSE_CORNERS[slot];
        self.masked(c[Corner::TopLeft as usize], x, y, r, r, bg);
        self.masked(c[Corner::TopRight as usize], x + size - r, y, r, r, bg);
        self.masked(c[Corner::BottomLeft as usize], x, y + size - r, r, r, bg);
        self.masked(
            c[Corner::BottomRight as usize],
            x + size - r,
            y + size - r,
            r,
            r,
            bg,
        );
    }

    /// A filled circle of radius `r` (exact corners up to r 32, scaled above).
    pub fn disc(&mut self, cx: f32, cy: f32, r: f32, color: u32) {
        if r <= 32. {
            self.rrect(cx - r, cy - r, 2. * r, 2. * r, r, color);
        } else {
            self.masked(mask::DISC, cx - r, cy - r, 2. * r, 2. * r, color);
        }
    }

    /// A ring (scaled sprite) of radius `r`; `thickness` picks the closest stroke.
    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, thickness: f32, color: u32) {
        let scale = 2. * r / 64.;
        let strokes = [
            (3., mask::RING_THIN),
            (6., mask::RING_MED),
            (10., mask::RING_THICK),
        ];
        let (_, src) = strokes
            .iter()
            .min_by(|a, b| {
                (a.0 * scale - thickness)
                    .abs()
                    .total_cmp(&(b.0 * scale - thickness).abs())
            })
            .copied()
            .unwrap_or(strokes[0]);
        self.masked(src, cx - r, cy - r, 2. * r, 2. * r, color);
    }

    /// A clockwise arc from 12 o'clock covering `turn` (0..=1), drawn from one
    /// of the pre-rendered frames; `r` is the outer radius.
    pub fn arc(&mut self, cx: f32, cy: f32, r: f32, turn: f32, color: u32) {
        let frame = (turn.clamp(0., 1.) * mask::ARC_FRAMES as f32).round() as usize;
        if frame == 0 {
            return;
        }
        let src = mask::ARCS[frame.min(mask::ARC_FRAMES) - 1];
        self.masked(src, cx - r, cy - r, 2. * r, 2. * r, color);
    }

    /// A spinner on a flat background: a bright segment that grows from 12
    /// o'clock, slides round the ring and shrinks away, then repeats.
    /// `track` is the opaque colour of the ring behind it; `phase` is 0..1.
    pub fn spinner(&mut self, cx: f32, cy: f32, r: f32, bright: u32, track: u32, phase: f32) {
        const LENGTH: f32 = 0.34;
        // Head and tail move in whole arc frames together, so the segment
        // keeps its length while it slides instead of flickering by a frame.
        let frames = mask::ARC_FRAMES as f32;
        let head = (phase.rem_euclid(1.) * (1. + LENGTH) * frames).round();
        let tail = head - (LENGTH * frames).round();
        self.arc(cx, cy, r, 1., track);
        self.arc(cx, cy, r, (head / frames).min(1.), bright);
        self.arc(cx, cy, r, (tail / frames).max(0.), track);
    }

    /// A soft radial glow; alpha falls from `color` at the centre to 0 at the edge.
    pub fn glow(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, color: u32) {
        self.masked(mask::GLOW, cx - rx, cy - ry, 2. * rx, 2. * ry, color);
    }

    pub fn fade_from_bottom(&mut self, x: f32, y: f32, w: f32, h: f32, color: u32) {
        self.masked(mask::RAMP_UP, x, y, w, h, color);
    }
    pub fn icon(&mut self, icon: Icon, cx: f32, cy: f32, size: f32, color: u32) {
        let src = icon.src();
        let size = size.round();
        self.masked(
            src,
            (cx - size / 2.).round(),
            (cy - size / 2.).round(),
            size,
            size,
            color,
        );
    }

    // ------------------------------------------------------------------- text

    /// Draw `text` with its baseline at `baseline`, starting at `x`. Returns
    /// the pen advance, so callers can place trailing content.
    pub fn text(&mut self, x: f32, baseline: f32, text: &str, face: Face, color: u32) -> f32 {
        self.text_tracked(x, baseline, text, face, color, 0.)
    }

    pub fn text_tracked(
        &mut self,
        x: f32,
        baseline: f32,
        text: &str,
        face: Face,
        color: u32,
        tracking: f32,
    ) -> f32 {
        if color & 0xFF == 0 {
            return text::width_tracked(face, text, tracking);
        }
        let set = face.set();
        let baseline = baseline.round();
        let mut pen = 0.;
        for ch in text.chars().filter(|c| !c.is_control()) {
            let Some(source) = text::lookup(face, ch) else {
                continue;
            };
            match source {
                Source::Exact(g) => {
                    let (w, h) = (f32::from(set.cell_w), f32::from(set.cell_h));
                    self.draw.push(Quad::masked(
                        Tex::Font(set.page),
                        [g.1, g.2, u16::from(set.cell_w), u16::from(set.cell_h)],
                        (x + pen).round() - f32::from(set.pad),
                        baseline - f32::from(set.ascent),
                        w,
                        h,
                        color,
                    ));
                    pen += f32::from(g.3) / 64. + tracking;
                }
                Source::Fallback(g) => {
                    let k = face.px() / text::FALLBACK_PX;
                    self.draw.push(Quad::masked(
                        Tex::Font(2),
                        [g.1, g.2, 32, 32],
                        (x + pen).round() - text::FALLBACK_PAD * k,
                        baseline - text::FALLBACK_ASCENT * k,
                        text::FALLBACK_CELL * k,
                        text::FALLBACK_CELL * k,
                        color,
                    ));
                    pen += f32::from(g.3) / 64. * k + tracking;
                }
            }
        }
        pen - tracking.min(pen)
    }

    pub fn text_right(&mut self, right: f32, baseline: f32, text: &str, face: Face, color: u32) {
        let w = text::width(face, text);
        self.text(right - w, baseline, text, face, color);
    }

    pub fn text_center(&mut self, cx: f32, baseline: f32, text: &str, face: Face, color: u32) {
        let w = text::width(face, text);
        self.text(cx - w / 2., baseline, text, face, color);
    }

    pub fn text_center_tracked(
        &mut self,
        cx: f32,
        baseline: f32,
        text: &str,
        face: Face,
        color: u32,
        tracking: f32,
    ) {
        let w = text::width_tracked(face, text, tracking);
        self.text_tracked(cx - w / 2., baseline, text, face, color, tracking);
    }

    /// Text vertically centred on `cy`, truncated to `max_width`.
    pub fn text_mid(
        &mut self,
        x: f32,
        cy: f32,
        max_width: f32,
        text: &str,
        face: Face,
        color: u32,
    ) {
        let shown = text::fit(face, text, max_width);
        self.text(x, cy + face.mid_offset(), &shown, face, color);
    }

    pub fn text_mid_right(&mut self, right: f32, cy: f32, text: &str, face: Face, color: u32) {
        self.text_right(right, cy + face.mid_offset(), text, face, color);
    }

    pub fn text_mid_center(&mut self, cx: f32, cy: f32, text: &str, face: Face, color: u32) {
        self.text_center(cx, cy + face.mid_offset(), text, face, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_rectangles_do_not_overlap_so_glass_blends_once() {
        let mut c = Canvas::new();
        c.rrect(10., 20., 100., 40., 12., 0xFFFFFF30);
        let pieces: Vec<_> = c.draw.iter().map(|q| (q.x, q.y, q.w, q.h)).collect();
        for (i, a) in pieces.iter().enumerate() {
            for b in &pieces[i + 1..] {
                let overlap_x = a.0 < b.0 + b.2 && b.0 < a.0 + a.2;
                let overlap_y = a.1 < b.1 + b.3 && b.1 < a.1 + a.3;
                assert!(!(overlap_x && overlap_y), "{a:?} overlaps {b:?}");
            }
        }
        let area: f32 = pieces.iter().map(|p| p.2 * p.3).sum();
        assert_eq!(area, 100. * 40.);
    }

    #[test]
    fn pills_and_discs_use_exact_corners() {
        let mut c = Canvas::new();
        c.rrect(0., 0., 60., 28., 14., 0xFFFFFFFF);
        assert!(c.draw.iter().all(|q| q.h <= 28. && q.w <= 60.));
        let before = c.draw.len();
        c.disc(100., 100., 20., 0xFFFFFFFF);
        assert!(c.draw.len() > before);
        assert!(c
            .draw
            .iter()
            .skip(before)
            .all(|q| q.x >= 80. && q.x + q.w <= 120.));
    }

    #[test]
    fn text_is_pixel_aligned_and_tracking_widens_it() {
        let mut c = Canvas::new();
        let plain = c.text(10.3, 40.6, "Songs", Face::R15, 0xFFFFFFFF);
        assert!(c
            .draw
            .iter()
            .all(|q| q.x.fract() == 0. && q.y.fract() == 0.));
        let tracked = c.text_tracked(10., 40., "SONGS", Face::S10, 0xFFFFFFFF, 1.5);
        assert!(plain > 0. && tracked > text::width(Face::S10, "SONGS"));
        let n = c.draw.len();
        c.text(0., 0., "x", Face::R15, 0xFFFFFF00);
        assert_eq!(c.draw.len(), n, "invisible text draws nothing");
    }
}
