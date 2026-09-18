use super::Quad;
use reborn_observability::{HealthState, Level, Observer};
use serde_json::json;
use std::{
    ffi::{c_char, c_int, c_void, CStr},
    ptr::NonNull,
    time::Instant,
};
unsafe extern "C" {
    fn rb_graphics_error() -> *const c_char;
    fn rb_graphics_open(out: *mut *mut c_void, font: *const u8) -> c_int;
    fn rb_graphics_close(p: *mut c_void);
    fn rb_graphics_info(p: *mut c_void) -> *const c_char;
    fn rb_graphics_width(p: *mut c_void) -> c_int;
    fn rb_graphics_height(p: *mut c_void) -> c_int;
    fn rb_graphics_begin(p: *mut c_void);
    fn rb_graphics_quad(
        p: *mut c_void,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: u32,
        glyph: c_int,
        art: c_int,
    );
    fn rb_graphics_art(p: *mut c_void, b: *const u8) -> c_int;
    fn rb_graphics_test(p: *mut c_void) -> c_int;
    fn rb_graphics_present(p: *mut c_void) -> c_int;
}
pub struct Renderer {
    raw: NonNull<c_void>,
    pub info: String,
    pub width: u32,
    pub height: u32,
    log: Observer,
}
impl Renderer {
    pub fn open(font: &[u8], log: Observer) -> Result<Self, String> {
        if font.len() != 128 * 64 * 4 {
            return Err("font atlas dimensions".into());
        }
        let mut p = std::ptr::null_mut();
        // SAFETY: font has exactly the checked RGBA length; constructor copies it into GL and returns uniquely owned handles.
        let r = unsafe { rb_graphics_open(&mut p, font.as_ptr()) };
        if r < 0 {
            // SAFETY: thread-local C diagnostic string has static storage on this UI thread.
            let detail = unsafe { CStr::from_ptr(rb_graphics_error()) }.to_string_lossy();
            return Err(format!("DRM/GBM/EGL initialization failed: {r}, {detail}"));
        }
        let raw = NonNull::new(p).ok_or("null graphics context")?;
        // SAFETY: initialized context retains info string and dimensions until Drop; all access stays on owner UI thread.
        let (info, width, height) = unsafe {
            (
                CStr::from_ptr(rb_graphics_info(p))
                    .to_string_lossy()
                    .into_owned(),
                rb_graphics_width(p) as u32,
                rb_graphics_height(p) as u32,
            )
        };
        log.emit(
            Level::Info,
            "graphics",
            "context_created",
            &info,
            None,
            json!({"width":width,"height":height,"format":"XRGB8888","scanout":"GBM linear KMS"}),
        );
        log.health_set("graphics", HealthState::Ok, true, &info);
        Ok(Self {
            raw,
            info,
            width,
            height,
            log,
        })
    }
    pub fn render(&mut self, quads: &[Quad]) -> Result<(), String> {
        if quads.len() > 20000 {
            return Err("draw list exceeds limit".into());
        }
        let start = Instant::now();
        // SAFETY: live EGL context is current on this thread; uniquely borrowed renderer prevents concurrent calls.
        unsafe {
            rb_graphics_begin(self.raw.as_ptr());
            for q in quads {
                rb_graphics_quad(
                    self.raw.as_ptr(),
                    q.x,
                    q.y,
                    q.w,
                    q.h,
                    q.color,
                    q.glyph.map(i32::from).unwrap_or(-1),
                    i32::from(q.artwork),
                );
            }
        }
        // SAFETY: presentation retains buffers until page-flip completion; timeout disables scanout before releasing uncertain buffer.
        let r = unsafe { rb_graphics_present(self.raw.as_ptr()) };
        self.log.gauge(
            "graphics_frame_time_ms",
            start.elapsed().as_secs_f64() * 1000.,
        );
        if r < 0 {
            self.log.add("graphics_context_losses", 1.);
            self.log
                .health_set("graphics", HealthState::Failed, true, "presentation failed");
            return Err(format!("graphics presentation {r}"));
        }
        if start.elapsed().as_millis() > 34 {
            self.log.add("graphics_missed_frames", 1.)
        }
        self.log.add("graphics_frames", 1.);
        Ok(())
    }
    pub fn artwork(&mut self, rgba: &[u8]) -> Result<(), String> {
        if rgba.len() != 160 * 160 * 4 {
            return Err("artwork dimensions".into());
        }
        // SAFETY: exact RGBA dimensions validated; GL synchronously copies slice into owned texture.
        let r = unsafe { rb_graphics_art(self.raw.as_ptr(), rgba.as_ptr()) };
        if r < 0 {
            Err(format!("texture upload {r}"))
        } else {
            Ok(())
        }
    }
    pub fn test(&mut self) -> Result<(), String> {
        // SAFETY: bounded FBO test on current owner context restores default framebuffer and viewport.
        let r = unsafe { rb_graphics_test(self.raw.as_ptr()) };
        if r < 0 {
            Err(format!("offscreen graphics test {r}"))
        } else {
            Ok(())
        }
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        // SAFETY: sole owner releases GL/EGL/GBM/DRM in reverse order, restoring prior KMS state.
        unsafe { rb_graphics_close(self.raw.as_ptr()) }
    }
}
