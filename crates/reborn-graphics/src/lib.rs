mod native;
pub use native::Renderer;
#[derive(Clone, Debug)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: u32,
    pub glyph: Option<u8>,
    pub artwork: bool,
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
            artwork: false,
        }
    }
}
