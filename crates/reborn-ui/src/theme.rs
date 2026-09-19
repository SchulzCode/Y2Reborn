//! The single source of truth for the Reborn visual system.
//!
//! The values mirror `Reborn_Y2_UI_Implementation_Pack/tokens/reborn_tokens.json`.
//! Screen code consumes these named tokens instead of inventing colors,
//! spacing, radii, or type sizes locally.

pub const WIDTH: f32 = 480.0;
pub const HEIGHT: f32 = 360.0;

pub mod color {
    pub const BG: u32 = 0x090B0DFF;
    pub const BG_RAISED: u32 = 0x101317FF;
    pub const SURFACE: u32 = 0x15191EFF;
    pub const SURFACE_HOVER: u32 = 0x1B2026FF;
    pub const SURFACE_BORDER: u32 = 0x2A3037FF;
    pub const TEXT_PRIMARY: u32 = 0xF2F1EDFF;
    pub const TEXT_SECONDARY: u32 = 0xAAAEB5FF;
    pub const TEXT_MUTED: u32 = 0x747A83FF;
    pub const ACCENT_GOLD: u32 = 0xE6B965FF;
    pub const ACCENT_GOLD_BRIGHT: u32 = 0xFFD17BFF;
    pub const ACCENT_GOLD_DIM: u32 = 0x7D6337FF;
    pub const FOCUS_GLOW: u32 = 0xF5C973FF;
    pub const FOCUS_FILL: u32 = 0x211B12FF;
    pub const DANGER: u32 = 0xD46B65FF;
    pub const SUCCESS: u32 = 0x74B68BFF;
    pub const TRACK: u32 = 0x444A51FF;
    pub const BLACK: u32 = 0x000000FF;
    pub const SCRIM: u32 = 0x090B0DCC;
    pub const SCRIM_STRONG: u32 = 0x090B0DEE;
    pub const BOOT_SCRIM: u32 = 0x090B0DD8;
    pub const BOOT_SCRIM_STRONG: u32 = 0x090B0DEA;
}

pub mod space {
    pub const BASE: f32 = 4.0;
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
    pub const SCREEN_MARGIN: f32 = 16.0;
    pub const ROW_GAP: f32 = 8.0;
}

pub mod radius {
    pub const SMALL: f32 = 6.0;
    pub const MEDIUM: f32 = 10.0;
    pub const LARGE: f32 = 14.0;
}

pub mod stroke {
    pub const HAIRLINE: f32 = 1.0;
    pub const FOCUS: f32 = 2.0;
}

pub mod type_scale {
    // The renderer's 16 px glyph cells are displayed at these scales.
    pub const HERO: f32 = 3.0;
    pub const SCREEN_TITLE: f32 = 2.25;
    pub const SECTION: f32 = 2.0;
    pub const ROW: f32 = 1.75;
    pub const BODY: f32 = 1.5;
    pub const SECONDARY: f32 = 1.25;
    pub const MICRO: f32 = 1.0;
    pub const ICON_LABEL: f32 = 1.15;
}

pub mod layout {
    pub const STATUS_H: f32 = 30.0;
    pub const CONTENT_TOP: f32 = 36.0;
    pub const FOOTER_TOP: f32 = 318.0;
    pub const BOTTOM_H: f32 = 42.0;
    pub const MIN_ROW_H: f32 = 44.0;
    pub const ROW_H: f32 = 44.0;
    pub const ART_NOW: f32 = 168.0;
    pub const ART_LIST: f32 = 42.0;
    pub const LIST_VISIBLE: usize = 5;
    pub const SIDEBAR_W: f32 = 132.0;
    pub const CONTENT_X: f32 = 148.0;
}

pub const UI_FONT: &str = "DejaVu Sans";
pub const DISPLAY_FONT: &str = "DejaVu Serif";
pub const ICON_COLUMNS: usize = 6;
pub const ICON_ROWS: usize = 5;
