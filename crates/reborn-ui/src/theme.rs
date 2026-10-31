//! The single source of truth for the Reborn visual system (UI v4).
//!
//! A near-black canvas, white type in three tiers, one fixed accent for every
//! control and state (`ACCENT`; the playing cover's colour only washes Now
//! Playing and the Home cover card), and translucent white "glass" for raised
//! areas and the focused row.
//! Screen code consumes these named tokens; nothing invents colours, sizes or
//! spacing locally. Type uses the exact-size Open Sans sets in `text::Face`.

pub const WIDTH: f32 = 480.0;
pub const HEIGHT: f32 = 360.0;

pub mod color {
    pub const BG: u32 = 0x06070AFF;
    pub const BLACK: u32 = 0x000000FF;
    pub const TEXT: u32 = 0xF6F6F8FF;
    /// Secondary text; at least 4.5:1 on `BG`. A step brighter in 9.2.3: at low screen brightness the
    /// grey values read too dark (Y2Reborn #59; was #A0A4AE).
    pub const MUTED: u32 = 0xB4B8C2FF;
    /// Quiet labels and hints; at least 4.5:1 on `BG` (was #848894).
    pub const DIM: u32 = 0x989CA8FF;
    pub const INK: u32 = 0x06070AFF;
    /// The accent of controls and state; also Now Playing's colour before a
    /// cover is known, or when a cover is too dull.
    pub const ACCENT: u32 = 0x6EA0FFFF;
    pub const DANGER: u32 = 0xFF7864FF;
    pub const SUCCESS: u32 = 0x3CC878FF;
    pub const WARNING: u32 = 0xFFBE3CFF;
    /// Translucent white steps for raised areas, rules and tracks.
    pub const GLASS_FAINT: u32 = 0xFFFFFF10;
    pub const GLASS: u32 = 0xFFFFFF24;
    pub const GLASS_STRONG: u32 = 0xFFFFFF3A;
    pub const RULE: u32 = 0xFFFFFF12;
    pub const TRACK: u32 = 0xFFFFFF32;
    pub const SCRIM: u32 = 0x000000A0;
    pub const SCRIM_STRONG: u32 = 0x000000E1;
    pub const SHEET: u32 = 0x0E1014FA;
    /// The opaque colour of a spinner's ring over `BG`.
    pub const RING_TRACK: u32 = 0x1E2023FF;
}

/// `color` with its alpha replaced.
pub const fn alpha(color: u32, a: u8) -> u32 {
    (color & 0xFFFF_FF00) | a as u32
}

/// `color` with its alpha scaled by `factor` (0..=1).
pub fn fade(color: u32, factor: f32) -> u32 {
    let a = ((color & 0xFF) as f32 * factor.clamp(0., 1.)).round() as u32;
    (color & 0xFFFF_FF00) | a
}

/// Linear blend of two colours' RGB, alpha taken from `a`.
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0., 1.);
    let channel = |shift: u32| {
        let x = ((a >> shift) & 0xFF) as f32;
        let y = ((b >> shift) & 0xFF) as f32;
        ((x + (y - x) * t).round() as u32) << shift
    };
    channel(24) | channel(16) | channel(8) | (a & 0xFF)
}

/// Linear blend of two colours, alpha included (a switch's track).
pub fn mix_rgba(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0., 1.);
    let channel = |shift: u32| {
        let x = ((a >> shift) & 0xFF) as f32;
        let y = ((b >> shift) & 0xFF) as f32;
        ((x + (y - x) * t).round() as u32) << shift
    };
    channel(24) | channel(16) | channel(8) | channel(0)
}

pub mod layout {
    pub const SIDE: f32 = 16.0;
    /// Vertical room taken by the status bar on list screens.
    pub const LIST_TOP: f32 = 38.0;
    pub const ROW_H: f32 = 53.0;
}
