//! Reborn Light (Y2Linux #112): Reborn's own look in light colours. Every screen is drawn as in
//! the dark look and each finished frame is recoloured once, so the light look has the same
//! layout, motion and screens with no second set of drawing code. Covers, thumbnails, films and
//! skin pictures keep their colours; flat colours, type and icons are mapped:
//!
//! - the named tokens of `theme::color` have hand-picked light counterparts;
//! - translucent white (glass, rules, tracks) becomes translucent black;
//! - other near-neutral or dark colours (greys mixed from the tokens, placeholder tiles) have
//!   their lightness turned around, keeping their hue;
//! - the remaining bright colours (a cover's accent, a status colour) are darkened until they
//!   read on the light background (4.5:1);
//! - translucent black (shadows, halos, scrims) is softer: 40 % of its alpha.
use crate::theme::color;
use reborn_graphics::{Quad, Tex};

/// The light look's background and accent.
pub const BG: u32 = 0xF3F4F6FF;
pub const ACCENT: u32 = 0x2563D9FF;

/// The dark look's tokens and their light counterparts (RGB; alpha is kept).
const TOKENS: [(u32, u32); 11] = [
    (color::BG, BG),
    (color::TEXT, 0x15171CFF),
    (color::MUTED, 0x50555FFF),
    (color::DIM, 0x646976FF),
    (color::ACCENT, ACCENT),
    (color::DANGER, 0xC8321EFF),
    (color::SUCCESS, 0x1C8048FF),
    (color::WARNING, 0x9A6400FF),
    (color::SHEET, 0xFFFFFFFF),
    (color::RING_TRACK, 0xD5D8DEFF),
    (color::ON_ACCENT, 0xFFFFFFFF),
];

/// Recolour a finished frame for the light look.
pub fn apply(frame: &mut [Quad]) {
    for quad in frame {
        if !matches!(
            quad.tex,
            Tex::Solid | Tex::Font(_) | Tex::Mask | Tex::Glyphs
        ) {
            continue;
        }
        quad.color = light(quad.color);
        if let Some(warp) = quad.warp.as_mut() {
            warp.colors = warp.colors.map(light);
        }
    }
}

/// One colour of the dark look in the light look.
pub fn light(rgba: u32) -> u32 {
    let a = rgba & 0xFF;
    let rgb = rgba | 0xFF;
    if let Some((from, to)) = TOKENS.iter().find(|(from, _)| *from | 0xFF == rgb) {
        // A see-through accent fill under white type: twice as solid on the light background.
        let a = if *from == color::ACCENT {
            (a * 2).min(0xFF)
        } else {
            a
        };
        return *to & !0xFF | a;
    }
    match rgb >> 8 {
        // Translucent white: the same step, in black, a little lighter (black weighs more on white).
        0xFFFFFF if a < 0xFF => return (a * 7 / 10).max(1),
        // Shadows, halos and scrims: softer, as on any light screen.
        0x000000 => return (a * 2 / 5).max(u32::from(a > 0)),
        _ => {}
    }
    let (h, s, l) = hsl(rgb);
    let (s, l) = if s < 0.25 || l < 0.35 {
        (s, 1. - l)
    } else {
        (s, readable(h, s, l))
    };
    from_hsl(h, s, l) | a
}

/// The highest lightness at or below `l` with 4.5:1 on the light background.
fn readable(h: f32, s: f32, mut l: f32) -> f32 {
    let bg = luminance(BG);
    while l > 0.05 {
        let lum = luminance(from_hsl(h, s, l) | 0xFF);
        if (bg + 0.05) / (lum + 0.05) >= 4.5 {
            break;
        }
        l -= 0.02;
    }
    l
}

fn luminance(rgba: u32) -> f32 {
    let lin = |shift: u32| {
        let v = ((rgba >> shift) & 0xFF) as f32 / 255.;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(24) + 0.7152 * lin(16) + 0.0722 * lin(8)
}

fn hsl(rgba: u32) -> (f32, f32, f32) {
    let [r, g, b] = [24, 16, 8].map(|s| ((rgba >> s) & 0xFF) as f32 / 255.);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.;
    let d = max - min;
    if d == 0. {
        return (0., 0., l);
    }
    let s = d / (1. - (2. * l - 1.).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.)
    } else if max == g {
        (b - r) / d + 2.
    } else {
        (r - g) / d + 4.
    };
    (h * 60., s, l)
}

/// An opaque-free RGB (alpha 0) from hue, saturation and lightness.
fn from_hsl(h: f32, s: f32, l: f32) -> u32 {
    let c = (1. - (2. * l - 1.).abs()) * s;
    let x = c * (1. - ((h / 60.).rem_euclid(2.) - 1.).abs());
    let m = l - c / 2.;
    let (r, g, b) = match (h / 60.) as u32 {
        0 => (c, x, 0.),
        1 => (x, c, 0.),
        2 => (0., c, x),
        3 => (0., x, c),
        4 => (x, 0., c),
        _ => (c, 0., x),
    };
    let byte = |v: f32| (((v + m) * 255.).round().clamp(0., 255.)) as u32;
    byte(r) << 24 | byte(g) << 16 | byte(b) << 8
}

#[cfg(test)]
#[path = "light_tests.rs"]
mod tests;
