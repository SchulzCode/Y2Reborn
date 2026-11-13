mod glyphs;
mod native;
pub use glyphs::{cjk_font, glyph_page, GlyphPage, CJK_FONT_ENV, CJK_FONT_PATH, GLYPH_PAGE};
pub use native::{Atlases, Renderer};
use serde::{Deserialize, Serialize};

/// Which texture a quad samples. Glyph pages, the shape/icon mask page and the
/// thumbnail atlas are addressed with `Quad::src`, in texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Tex {
    /// A flat colour.
    #[default]
    Solid,
    /// Glyph page 0 Regular, 1 SemiBold or 2 the fallback page; coverage masks
    /// tinted by `color`.
    Font(u8),
    /// Shapes, gradients and icons; coverage masks tinted by `color`.
    Mask,
    /// The current track's artwork (`ART_SIZE` square).
    Art,
    /// The artwork of the album, artist or song being browsed (`ART_SIZE`).
    CollectionArt,
    /// A library thumbnail (`THUMB_SIZE`) identified by an opaque key; the renderer
    /// maps it to an atlas cell and skips it until its pixels are resident.
    Thumb(u64),
    /// The active skin's RGBA atlas (images and white glyph coverage),
    /// addressed with `Quad::src` in texels.
    Skin,
    /// The runtime glyph page (`glyph_page`): characters rasterised from the
    /// CJK font; coverage masks tinted by `color`, `Quad::src` in texels.
    Glyphs,
    /// The playing film's current picture (`Renderer::video`), whole; not
    /// drawn until a picture is uploaded.
    Video,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: u32,
    #[serde(default)]
    pub tex: Tex,
    /// Source rectangle `[x, y, w, h]` in texels (glyph, mask textures only).
    #[serde(default)]
    pub src: [u16; 4],
    /// Presentation metadata used by deterministic UI validation. Native
    /// rendering intentionally ignores this field.
    pub focus_target: bool,
    /// Draw into an arbitrary quadrilateral instead of the `x/y/w/h` box,
    /// with perspective-correct texturing and per-corner tints (Cover Flow).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warp: Option<Warp>,
}
/// Corners in screen pixels, clockwise from top-left: TL, TR, BR, BL. The
/// texture's top-left maps to TL. `colors` tint each corner (RGBA), which
/// fades a reflection without a separate gradient texture.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Warp {
    pub corners: [[f32; 2]; 4],
    pub colors: [u32; 4],
}
impl Quad {
    pub fn rect(x: f32, y: f32, w: f32, h: f32, color: u32) -> Self {
        Self {
            x,
            y,
            w,
            h,
            color,
            tex: Tex::Solid,
            src: [0; 4],
            focus_target: false,
            warp: None,
        }
    }
    /// A tinted region of a coverage-mask texture.
    pub fn masked(tex: Tex, src: [u16; 4], x: f32, y: f32, w: f32, h: f32, color: u32) -> Self {
        Self {
            tex,
            src,
            ..Self::rect(x, y, w, h, color)
        }
    }
}

/// Texture geometry shared with `native/graphics.c`.
pub const FONT_PAGE_WIDTH: usize = 1024;
/// Edge of the current track's and the browsed collection's artwork: the
/// largest cover on screen (Home's card) shows it texel for texel.
pub const ART_SIZE: usize = 180;
/// Edge of a library thumbnail: the largest list cover (128 px) is never enlarged.
pub const THUMB_SIZE: usize = 128;
pub const THUMB_CELL_W: usize = level_cell(THUMB_SIZE).0;
pub const THUMB_CELL_H: usize = level_cell(THUMB_SIZE).1;
pub const THUMB_ATLAS_W: usize = 2048;
pub const THUMB_ATLAS_H: usize = 1024;
pub const THUMB_COLUMNS: usize = THUMB_ATLAS_W / THUMB_CELL_W;
pub const THUMB_ROWS: usize = THUMB_ATLAS_H / THUMB_CELL_H;
pub const THUMB_SLOTS: usize = THUMB_COLUMNS * THUMB_ROWS;
/// The largest skin atlas edge, in texels.
pub const SKIN_ATLAS: usize = 1024;

/// A picture is uploaded with its half- and quarter-size copies, so a quad
/// never samples it at less than half its size: GL's bilinear filter alone
/// would skip texels there (grainy small covers). Each level has a replicated
/// one-texel gutter; the full size sits left, the smaller two stacked right:
///
/// ```text
/// +--------------+-------+
/// |              |  n/2  |
/// |      n       +----+--+
/// |              |n/4 |
/// +--------------+----+
/// ```
pub const fn level_cell(n: usize) -> (usize, usize) {
    (n + 2 + n / 2 + 2, n + 2)
}
/// Where level `i` (0 full, 1 half, 2 quarter) of an `n` picture sits in its
/// cell: `[x, y, edge]` of the image inside its gutter.
const fn level_at(n: usize, i: usize) -> [usize; 3] {
    match i {
        0 => [1, 1, n],
        1 => [n + 3, 1, n / 2],
        _ => [n + 3, n / 2 + 3, n / 4],
    }
}
/// The source rectangle, inside its cell, for a picture of edge `n` drawn
/// `edge` pixels large: the smallest level at least that large, so sampling
/// shrinks it by less than half, or the full size when it is drawn larger.
pub fn level_source(n: usize, edge: f32) -> [u16; 4] {
    let i = (0..3)
        .rev()
        .find(|&i| level_at(n, i)[2] as f32 >= edge)
        .unwrap_or(0);
    let [x, y, e] = level_at(n, i).map(|v| v as u16);
    [x, y, e, e]
}
/// Lay out an `n` x `n` RGBA picture and its box-filtered half and quarter
/// copies in one `level_cell(n)` RGBA block, gutters replicated.
pub fn level_block(rgba: &[u8], n: usize) -> Option<Vec<u8>> {
    if !n.is_multiple_of(4) || rgba.len() != n * n * 4 {
        return None;
    }
    let (w, h) = level_cell(n);
    let mut block = vec![0u8; w * h * 4];
    let mut level = rgba.to_vec();
    for i in 0..3 {
        let [x0, y0, e] = level_at(n, i);
        if i > 0 {
            let src = level;
            let half = e * 2;
            level = vec![0u8; e * e * 4];
            for y in 0..e {
                for x in 0..e {
                    for c in 0..4 {
                        let at = |dx: usize, dy: usize| {
                            u32::from(src[((2 * y + dy) * half + 2 * x + dx) * 4 + c])
                        };
                        let sum = at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1);
                        level[(y * e + x) * 4 + c] = ((sum + 2) / 4) as u8;
                    }
                }
            }
        }
        for y in 0..e + 2 {
            for x in 0..e + 2 {
                let sx = x.saturating_sub(1).min(e - 1);
                let sy = y.saturating_sub(1).min(e - 1);
                let from = (sy * e + sx) * 4;
                let to = ((y0 - 1 + y) * w + x0 - 1 + x) * 4;
                block[to..to + 4].copy_from_slice(&level[from..from + 4]);
            }
        }
    }
    Some(block)
}
/// How large a quad draws its texture: its longest edge on screen.
pub fn drawn_edge(q: &Quad) -> f32 {
    match &q.warp {
        Some(warp) => {
            let c = warp.corners;
            (0..4)
                .map(|i| {
                    let (a, b) = (c[i], c[(i + 1) % 4]);
                    (a[0] - b[0]).hypot(a[1] - b[1])
                })
                .fold(0., f32::max)
        }
        None => q.w.max(q.h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_halve_with_replicated_gutters() {
        let n = 8;
        let rgba: Vec<u8> = (0..n * n)
            .flat_map(|i| [(i % n * 30) as u8, (i / n * 30) as u8, 7, 255])
            .collect();
        let block = level_block(&rgba, n).unwrap();
        let (w, h) = level_cell(n);
        assert_eq!((w, h), (16, 10));
        let px = |x: usize, y: usize| &block[(y * w + x) * 4..(y * w + x) * 4 + 4];
        assert_eq!(px(1, 1), &rgba[0..4]);
        assert_eq!(px(0, 0), &rgba[0..4], "corner gutter replicates");
        // Half level: each texel the mean of a 2x2 block.
        let [x, y, e, _] = level_source(n, 4.).map(usize::from);
        assert_eq!((x, y, e), (11, 1, 4));
        assert_eq!(px(x, y), &[15, 15, 7, 255]);
        assert_eq!(px(x + 3, y + 3), &[195, 195, 7, 255]);
        // Quarter level below it.
        assert_eq!(level_source(n, 2.), [11, 7, 2, 2]);
        assert_eq!(px(11, 7), &[45, 45, 7, 255]);
        assert!(level_block(&rgba[4..], n).is_none());
    }

    #[test]
    fn the_level_is_the_smallest_at_least_as_large_as_the_quad() {
        let n = THUMB_SIZE;
        assert_eq!(level_source(n, 180.)[2], 128, "drawn larger: the full size");
        assert_eq!(level_source(n, 84.)[2], 128);
        assert_eq!(level_source(n, 64.)[2], 64);
        assert_eq!(level_source(n, 38.)[2], 64, "list rows sample 64 at 0.59");
        assert_eq!(level_source(n, 19.)[2], 32);
        assert_eq!(level_source(ART_SIZE, 176.)[2], 180);
        assert_eq!(level_source(ART_SIZE, 104.)[2], 180);
        assert_eq!(level_source(ART_SIZE, 60.)[2], 90);
        // The atlas holds at least as many thumbnails as the 96x96 one did.
        const { assert!(THUMB_SLOTS >= 50) };
    }

    #[test]
    fn a_warped_quad_is_as_large_as_its_longest_side() {
        let q = Quad {
            warp: Some(Warp {
                corners: [[0., 0.], [30., 10.], [30., 110.], [0., 120.]],
                colors: [0; 4],
            }),
            ..Quad::rect(0., 0., 30., 120., 0)
        };
        assert_eq!(drawn_edge(&q), 120.);
        assert_eq!(drawn_edge(&Quad::rect(0., 0., 40., 38., 0)), 40.);
    }
}
