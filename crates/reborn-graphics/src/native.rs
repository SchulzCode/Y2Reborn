use super::{
    drawn_edge, glyph_page, level_block, level_cell, level_source, Quad, Tex, ART_SIZE,
    FONT_PAGE_WIDTH, GLYPH_PAGE, SKIN_ATLAS, THUMB_CELL_H, THUMB_CELL_W, THUMB_COLUMNS, THUMB_SIZE,
    THUMB_SLOTS,
};
use reborn_observability::{HealthState, Level, Observer};
use serde_json::json;
use std::{
    collections::HashMap,
    ffi::{c_char, c_int, c_void, CStr},
    ptr::NonNull,
    time::Instant,
};
unsafe extern "C" {
    fn rb_graphics_thread_cpu_seconds() -> f64;
    fn rb_graphics_draw_calls(p: *mut c_void) -> u32;
    fn rb_graphics_submit(p: *mut c_void);
    fn rb_graphics_error() -> *const c_char;
    fn rb_graphics_open(
        out: *mut *mut c_void,
        font_pages: *const *const u8,
        font_heights: *const c_int,
        masks: *const u8,
        mask_w: c_int,
        mask_h: c_int,
    ) -> c_int;
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
        tex: c_int,
        sx: c_int,
        sy: c_int,
        sw: c_int,
        sh: c_int,
    );
    fn rb_graphics_quad_warp(
        p: *mut c_void,
        corners: *const f32,
        colors: *const u32,
        tex: c_int,
        sx: c_int,
        sy: c_int,
        sw: c_int,
        sh: c_int,
    );
    fn rb_graphics_thumb(p: *mut c_void, cx: c_int, cy: c_int, rgba: *const u8) -> c_int;
    fn rb_graphics_art(
        p: *mut c_void,
        b: *const u8,
        w: c_int,
        h: c_int,
        collection: c_int,
    ) -> c_int;
    fn rb_graphics_skin(p: *mut c_void, b: *const u8, w: c_int, h: c_int) -> c_int;
    fn rb_graphics_video(p: *mut c_void, b: *const u8, w: c_int, h: c_int) -> c_int;
    fn rb_graphics_glyphs(p: *mut c_void, y: c_int, rows: c_int, a8: *const u8) -> c_int;
    fn rb_graphics_test(p: *mut c_void) -> c_int;
    fn rb_graphics_readback(p: *mut c_void, pixels: *mut u8) -> c_int;
    fn rb_graphics_present(p: *mut c_void) -> c_int;
    fn rb_graphics_sleep(p: *mut c_void, off: c_int) -> c_int;
}
/// Coverage-mask atlases, one byte per texel: three 1024-wide glyph pages
/// (Regular, SemiBold, fallback) and the shape/icon page.
pub struct Atlases<'a> {
    pub font_pages: [&'a [u8]; 3],
    pub masks: &'a [u8],
    pub mask_size: (usize, usize),
}
pub struct Renderer {
    raw: NonNull<c_void>,
    pub info: String,
    pub width: u32,
    pub height: u32,
    log: Observer,
    presented: bool,
    pub timings: FrameTimings,
    thumbs: HashMap<u64, ThumbSlot>,
    frame: u64,
    missing: Vec<u64>,
    mask_size: (usize, usize),
    skin_size: (usize, usize),
    page_heights: [usize; 3],
    /// Whether this context's glyph-cache texture holds the whole shared
    /// page; a new context starts empty and takes one full upload.
    glyphs_resident: bool,
    /// Size of the film picture in its texture; (0, 0): none.
    video_size: (u16, u16),
    /// The last glyph upload failed (logged once per failure streak).
    glyphs_failed: bool,
}
#[derive(Clone, Copy)]
struct ThumbSlot {
    cell: usize,
    used: u64,
}
#[derive(Default, serde::Serialize)]
pub struct FrameTimings {
    pub submit_ms: f64,
    pub present_ms: f64,
    pub cpu_ms: Option<f64>,
    pub draw_calls: u32,
}
fn thread_cpu_seconds() -> Option<f64> {
    // SAFETY: no arguments or retained pointers; C reads this thread's clock.
    let value = unsafe { rb_graphics_thread_cpu_seconds() };
    (value >= 0.).then_some(value)
}
impl Renderer {
    pub fn sleep(&mut self, off: bool) -> Result<(), String> {
        // SAFETY: the UI thread owns DRM master and retained scanout buffers;
        // the C call synchronously disables/restores the CRTC before returning.
        let ret = unsafe { rb_graphics_sleep(self.raw.as_ptr(), i32::from(off)) };
        if ret < 0 {
            return Err(format!("graphics screen sleep {ret}"));
        }
        self.log
            .gauge("graphics_scanout_active", if off { 0.0 } else { 1.0 });
        Ok(())
    }
    pub fn open(atlases: &Atlases, log: Observer) -> Result<Self, String> {
        let mut heights = [0 as c_int; 3];
        for (height, page) in heights.iter_mut().zip(atlases.font_pages) {
            if page.is_empty() || page.len() % FONT_PAGE_WIDTH != 0 {
                return Err("font page dimensions".into());
            }
            *height = (page.len() / FONT_PAGE_WIDTH) as c_int;
        }
        let (mask_w, mask_h) = atlases.mask_size;
        if mask_w == 0 || mask_h == 0 || atlases.masks.len() != mask_w * mask_h {
            return Err("mask atlas dimensions".into());
        }
        let pages = atlases.font_pages.map(<[u8]>::as_ptr);
        let mut p = std::ptr::null_mut();
        // SAFETY: every page has the checked one-byte-per-texel length; the
        // constructor copies them into GL and returns uniquely owned handles.
        let r = unsafe {
            rb_graphics_open(
                &mut p,
                pages.as_ptr(),
                heights.as_ptr(),
                atlases.masks.as_ptr(),
                mask_w as c_int,
                mask_h as c_int,
            )
        };
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
        log.health_set(
            "graphics",
            HealthState::Degraded,
            true,
            "context created; waiting for frame completion",
        );
        Ok(Self {
            raw,
            info,
            width,
            height,
            log,
            presented: false,
            timings: FrameTimings::default(),
            thumbs: HashMap::new(),
            frame: 0,
            missing: Vec::new(),
            mask_size: atlases.mask_size,
            skin_size: (1, 1),
            page_heights: heights.map(|h| h as usize),
            glyphs_resident: false,
            video_size: (0, 0),
            glyphs_failed: false,
        })
    }
    pub fn render(&mut self, quads: &[Quad]) -> Result<(), String> {
        self.render_inner(quads, None)
    }
    /// Qualification-only readback before swapping; normal frames never read
    /// pixels back or synchronize the GPU for screenshots.
    pub fn capture(&mut self, quads: &[Quad]) -> Result<Vec<u8>, String> {
        let mut pixels = vec![0; self.width as usize * self.height as usize * 4];
        self.render_inner(quads, Some(&mut pixels))?;
        Ok(pixels)
    }
    fn render_inner(&mut self, quads: &[Quad], pixels: Option<&mut [u8]>) -> Result<(), String> {
        if quads.iter().any(|q| !self.valid_source(q)) {
            return Err("texture source outside validated bounds".into());
        }
        if quads.len() > 20000 {
            return Err("draw list exceeds limit".into());
        }
        self.frame += 1;
        self.missing.clear();
        let start = Instant::now();
        let cpu_start = thread_cpu_seconds();
        self.sync_glyphs();
        // SAFETY: live EGL context is current on this thread; uniquely borrowed renderer prevents concurrent calls.
        unsafe {
            rb_graphics_begin(self.raw.as_ptr());
            for q in quads {
                let (tex, src) = match q.tex {
                    Tex::Solid => (0, [0; 4]),
                    Tex::Font(page) => (1 + c_int::from(page), q.src),
                    Tex::Mask => (4, q.src),
                    Tex::Art => (5, level_source(ART_SIZE, drawn_edge(q))),
                    Tex::CollectionArt => (6, level_source(ART_SIZE, drawn_edge(q))),
                    Tex::Skin => (8, q.src),
                    // Not drawn while the page failed to upload.
                    Tex::Glyphs if !self.glyphs_resident => continue,
                    Tex::Glyphs => (9, q.src),
                    Tex::Video if self.video_size.0 == 0 => continue,
                    Tex::Video => (10, [0, 0, self.video_size.0, self.video_size.1]),
                    Tex::Thumb(key) => match self.thumbs.get_mut(&key) {
                        Some(slot) => {
                            slot.used = self.frame;
                            let [x, y, w, h] = level_source(THUMB_SIZE, drawn_edge(q));
                            let cx = (slot.cell % THUMB_COLUMNS * THUMB_CELL_W) as u16;
                            let cy = (slot.cell / THUMB_COLUMNS * THUMB_CELL_H) as u16;
                            (7, [cx + x, cy + y, w, h])
                        }
                        None => {
                            if !self.missing.contains(&key) && self.missing.len() < 64 {
                                self.missing.push(key);
                            }
                            continue;
                        }
                    },
                };
                if let Some(warp) = &q.warp {
                    let corners = warp.corners.as_flattened();
                    rb_graphics_quad_warp(
                        self.raw.as_ptr(),
                        corners.as_ptr(),
                        warp.colors.as_ptr(),
                        tex,
                        c_int::from(src[0]),
                        c_int::from(src[1]),
                        c_int::from(src[2]),
                        c_int::from(src[3]),
                    );
                    continue;
                }
                rb_graphics_quad(
                    self.raw.as_ptr(),
                    q.x,
                    q.y,
                    q.w,
                    q.h,
                    q.color,
                    tex,
                    c_int::from(src[0]),
                    c_int::from(src[1]),
                    c_int::from(src[2]),
                    c_int::from(src[3]),
                );
            }
            rb_graphics_submit(self.raw.as_ptr());
        }
        let submitted = Instant::now();
        if let Some(pixels) = pixels {
            // SAFETY: buffer allocated above to the actual context dimensions;
            // qualification readback completes before swapping the framebuffer.
            if unsafe { rb_graphics_readback(self.raw.as_ptr(), pixels.as_mut_ptr()) } < 0 {
                return Err("graphics readback failed".into());
            }
        }
        // SAFETY: presentation retains buffers until page-flip completion; timeout disables scanout before releasing uncertain buffer.
        let r = unsafe { rb_graphics_present(self.raw.as_ptr()) };
        self.timings = FrameTimings {
            submit_ms: submitted.duration_since(start).as_secs_f64() * 1000.,
            present_ms: submitted.elapsed().as_secs_f64() * 1000.,
            cpu_ms: cpu_start
                .zip(thread_cpu_seconds())
                .map(|(a, b)| (b - a) * 1000.),
            // SAFETY: live renderer owned by this thread; reads its frame counter.
            draw_calls: unsafe { rb_graphics_draw_calls(self.raw.as_ptr()) },
        };
        self.log.gauge("graphics_submit_ms", self.timings.submit_ms);
        self.log
            .gauge("graphics_present_ms", self.timings.present_ms);
        if let Some(ms) = self.timings.cpu_ms {
            self.log.gauge("graphics_cpu_ms", ms);
        }
        self.log
            .gauge("graphics_draw_calls", f64::from(self.timings.draw_calls));
        self.log.gauge(
            "graphics_frame_time_ms",
            start.elapsed().as_secs_f64() * 1000.,
        );
        if r < 0 {
            self.presented = false;
            self.log.add("graphics_context_losses", 1.);
            self.log
                .health_set("graphics", HealthState::Failed, true, "presentation failed");
            return Err(format!("graphics presentation {r}"));
        }
        if start.elapsed().as_millis() > 34 {
            self.log.add("graphics_missed_frames", 1.)
        }
        self.log.add("graphics_frames", 1.);
        if !self.presented {
            self.log
                .health_set("graphics", HealthState::Ok, true, &self.info);
            self.presented = true;
        }
        Ok(())
    }
    fn valid_source(&self, q: &Quad) -> bool {
        let [x, y, w, h] = q.src.map(usize::from);
        let inside = |width: usize, height: usize| x + w <= width && y + h <= height;
        match q.tex {
            Tex::Font(page) => {
                usize::from(page) < 3
                    && inside(FONT_PAGE_WIDTH, self.page_heights[usize::from(page)])
            }
            Tex::Mask => inside(self.mask_size.0, self.mask_size.1),
            Tex::Skin => inside(self.skin_size.0, self.skin_size.1),
            Tex::Glyphs => inside(GLYPH_PAGE, GLYPH_PAGE),
            _ => true,
        }
    }
    /// Upload the glyph-page rows the UI wrote since the last frame (all of
    /// it once per context). Before the first runtime glyph nothing exists.
    /// A failed upload hides runtime glyphs and is retried in full next frame.
    fn sync_glyphs(&mut self) {
        let mut page = glyph_page();
        if !page.allocated() {
            return;
        }
        let dirty = page.take_dirty();
        let (start, end) = if self.glyphs_resident {
            match dirty {
                Some(rows) => rows,
                None => return,
            }
        } else {
            (0, GLYPH_PAGE)
        };
        let rows = &page.pixels()[start * GLYPH_PAGE..end * GLYPH_PAGE];
        // SAFETY: `rows` holds exactly (end - start) full 1024-texel rows of
        // the page, inside its bounds; GL copies them before returning.
        let r = unsafe {
            rb_graphics_glyphs(
                self.raw.as_ptr(),
                start as c_int,
                (end - start) as c_int,
                rows.as_ptr(),
            )
        };
        let resident = r >= 0;
        if !resident && !self.glyphs_failed {
            self.log.emit(
                Level::Warn,
                "graphics",
                "glyph_upload_failed",
                "runtime glyph page upload failed; retrying",
                None,
                json!({"result": r, "rows": [start, end]}),
            );
        }
        self.glyphs_resident = resident;
        self.glyphs_failed = !resident;
    }
    /// Library thumbnails the last frame wanted but that are not resident,
    /// in draw order. The caller loads them and calls `upload_thumb`.
    pub fn missing_thumbs(&self) -> &[u64] {
        &self.missing
    }
    /// Keys of every thumbnail the atlas currently holds.
    pub fn resident_thumbs(&self) -> impl Iterator<Item = u64> + '_ {
        self.thumbs.keys().copied()
    }
    /// Forget every thumbnail (the cells are reused): they are asked for again, shaped for a
    /// new look (Y2Reborn #100).
    pub fn forget_thumbs(&mut self) {
        self.thumbs.clear();
    }
    pub fn thumb_resident(&self, key: u64) -> bool {
        self.thumbs.contains_key(&key)
    }
    /// Store a `THUMB_SIZE` RGBA thumbnail, evicting the least recently drawn cell
    /// when the atlas is full. Cells drawn in the current frame are kept.
    pub fn upload_thumb(&mut self, key: u64, rgba: &[u8]) -> Result<(), String> {
        if rgba.len() != THUMB_SIZE * THUMB_SIZE * 4 {
            return Err("thumbnail dimensions".into());
        }
        let cell = match self.thumbs.get(&key) {
            Some(slot) => slot.cell,
            None if self.thumbs.len() < THUMB_SLOTS => (0..THUMB_SLOTS)
                .find(|cell| self.thumbs.values().all(|slot| slot.cell != *cell))
                .ok_or("thumbnail atlas full")?,
            None => {
                let (victim, slot) = self
                    .thumbs
                    .iter()
                    .min_by_key(|(_, slot)| slot.used)
                    .map(|(key, slot)| (*key, *slot))
                    .ok_or("thumbnail atlas empty")?;
                if slot.used == self.frame {
                    return Err("thumbnail atlas full of visible cells".into());
                }
                self.thumbs.remove(&victim);
                slot.cell
            }
        };
        let block = level_block(rgba, THUMB_SIZE).ok_or("thumbnail dimensions")?;
        // SAFETY: the block is exactly one THUMB_CELL_W x THUMB_CELL_H RGBA
        // cell and the cell index is inside the atlas; GL copies it before returning.
        let r = unsafe {
            rb_graphics_thumb(
                self.raw.as_ptr(),
                (cell % THUMB_COLUMNS) as c_int,
                (cell / THUMB_COLUMNS) as c_int,
                block.as_ptr(),
            )
        };
        if r < 0 {
            return Err(format!("thumbnail upload {r}"));
        }
        self.thumbs.insert(
            key,
            ThumbSlot {
                cell,
                used: self.frame,
            },
        );
        self.log.add("thumbnail_uploads", 1.);
        Ok(())
    }
    /// Show `rgba` (`width` x `height`, at most 512 each way) as the film's picture.
    pub fn video(&mut self, rgba: &[u8], width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
            return Err("film picture dimensions".into());
        }
        // SAFETY: the buffer holds exactly width * height RGBA texels; GL copies them at once.
        let r = unsafe {
            rb_graphics_video(
                self.raw.as_ptr(),
                rgba.as_ptr(),
                width as c_int,
                height as c_int,
            )
        };
        if r != 0 {
            self.video_size = (0, 0);
            return Err(format!("film picture upload failed ({r})"));
        }
        self.video_size = (width as u16, height as u16);
        Ok(())
    }
    /// The film ended: its picture is no longer drawn and its texture shrinks to one texel.
    pub fn forget_video(&mut self) {
        if self.video_size.0 != 0 {
            // SAFETY: one RGBA texel; GL copies it at once.
            unsafe { rb_graphics_video(self.raw.as_ptr(), [0u8, 0, 0, 255].as_ptr(), 1, 1) };
            self.video_size = (0, 0);
        }
    }
    pub fn artwork(&mut self, rgba: &[u8]) -> Result<(), String> {
        self.upload_art(rgba, false)
    }
    pub fn collection_artwork(&mut self, rgba: &[u8]) -> Result<(), String> {
        self.upload_art(rgba, true)
    }
    fn upload_art(&mut self, rgba: &[u8], collection: bool) -> Result<(), String> {
        let block = level_block(rgba, ART_SIZE).ok_or("artwork dimensions")?;
        let (w, h) = level_cell(ART_SIZE);
        let started = Instant::now();
        // SAFETY: the block holds exactly w*h RGBA texels; GL synchronously copies it into the owned texture.
        let r = unsafe {
            rb_graphics_art(
                self.raw.as_ptr(),
                block.as_ptr(),
                w as c_int,
                h as c_int,
                i32::from(collection),
            )
        };
        self.log
            .gauge("artwork_upload_ms", started.elapsed().as_secs_f64() * 1000.);
        self.log.add("artwork_uploads", 1.);
        if r < 0 {
            Err(format!("texture upload {r}"))
        } else {
            Ok(())
        }
    }
    /// Replace the skin atlas with `width` x `height` RGBA texels (at most
    /// `SKIN_ATLAS` square); `None` releases it. Skin quads address it in texels.
    pub fn skin_atlas(&mut self, atlas: Option<(&[u8], usize, usize)>) -> Result<(), String> {
        const WHITE: [u8; 4] = [255; 4];
        let (rgba, width, height) = atlas.unwrap_or((&WHITE, 1, 1));
        if width == 0 || height == 0 || width > SKIN_ATLAS || height > SKIN_ATLAS {
            return Err("skin atlas dimensions".into());
        }
        if rgba.len() != width * height * 4 {
            return Err("skin atlas size".into());
        }
        let started = Instant::now();
        // SAFETY: the slice holds exactly width*height RGBA texels; GL copies it synchronously.
        let r = unsafe {
            rb_graphics_skin(
                self.raw.as_ptr(),
                rgba.as_ptr(),
                width as c_int,
                height as c_int,
            )
        };
        self.log
            .gauge("skin_upload_ms", started.elapsed().as_secs_f64() * 1000.);
        if r < 0 {
            self.skin_size = (1, 1);
            return Err(format!("skin upload {r}"));
        }
        self.skin_size = (width, height);
        Ok(())
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
