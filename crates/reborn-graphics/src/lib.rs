mod native;
pub use native::Renderer;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: u32,
    pub glyph: Option<u8>,
    pub icon: Option<u8>,
    pub display_font: bool,
    pub artwork: bool,
    /// Presentation metadata used by deterministic UI validation. Native
    /// rendering intentionally ignores this field.
    pub focus_target: bool,
}
impl Quad {
    pub fn rect(x: f32, y: f32, w: f32, h: f32, color: u32) -> Self {
        Self {
            x,
            y,
            w,
            h,
            color,
            glyph: None,
            icon: None,
            display_font: false,
            artwork: false,
            focus_target: false,
        }
    }
}
