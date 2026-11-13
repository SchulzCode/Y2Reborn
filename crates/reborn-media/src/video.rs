//! A film's pictures (`native/video.c`): a second, picture-only reader of a file whose sound the
//! audio decoder plays. Each picture comes out as a small RGBA image for the screen; subtitle text
//! comes with it. H.264 decodes on the Y2's hardware decoder when the stream allows it.
use crate::{
    loader::symbol,
    native::{error_text, handle},
};
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    path::Path,
    ptr::NonNull,
    sync::OnceLock,
};

#[repr(C)]
#[derive(Default)]
struct RawInfo {
    width: u32,
    height: u32,
    out_width: u32,
    out_height: u32,
    hardware: u32,
    fps_milli: u32,
    subtitles: u32,
    reserved: u32,
    duration_ms: u64,
    codec: [c_char; 32],
}

type Open = unsafe extern "C" fn(
    *const c_char,
    c_int,
    c_int,
    c_int,
    *mut *mut c_void,
    *mut RawInfo,
) -> c_int;
type Next = unsafe extern "C" fn(*mut c_void, *mut i64, *mut u32) -> c_int;
type Convert = unsafe extern "C" fn(*mut c_void, *mut u8) -> c_int;
type Seek = unsafe extern "C" fn(*mut c_void, i64) -> c_int;
type Hurry = unsafe extern "C" fn(*mut c_void, c_int);
type Subtitle = unsafe extern "C" fn(*mut c_void, i64, *mut c_char, c_int) -> c_int;
type Close = unsafe extern "C" fn(*mut c_void);
type Subtitles = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
type Thumbnail = unsafe extern "C" fn(*const c_char, *mut u8, c_int) -> c_int;

struct Api {
    open: Open,
    next: Next,
    convert: Convert,
    seek: Seek,
    hurry: Hurry,
    subtitle: Subtitle,
    subtitles: Subtitles,
    close: Close,
    thumbnail: Thumbnail,
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| {
        let handle = handle()?;
        // SAFETY: the membrane stays loaded for the life of the process, and each type is the
        // signature of its function in native/video.c.
        unsafe {
            Ok(Api {
                open: symbol(handle, b"rb_video_open\0")?,
                next: symbol(handle, b"rb_video_next\0")?,
                convert: symbol(handle, b"rb_video_convert\0")?,
                seek: symbol(handle, b"rb_video_seek\0")?,
                hurry: symbol(handle, b"rb_video_hurry\0")?,
                subtitle: symbol(handle, b"rb_video_subtitle\0")?,
                subtitles: symbol(handle, b"rb_video_subtitles\0")?,
                close: symbol(handle, b"rb_video_close\0")?,
                thumbnail: symbol(handle, b"rb_video_thumbnail\0")?,
            })
        }
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn c_path(path: &Path) -> Result<CString, String> {
    CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| "path holds a NUL".to_owned())
}

/// What a film is and how its pictures come out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoInfo {
    /// The picture as stored.
    pub width: u32,
    pub height: u32,
    /// The RGBA image `Video::convert` writes: the picture fitted to the screen, never enlarged.
    pub out_width: u32,
    pub out_height: u32,
    pub fps_milli: u32,
    pub duration_ms: u64,
    /// The file, or a file beside it, has subtitle text.
    pub subtitles: bool,
    pub codec: String,
}

/// A decoded picture, held by the reader until the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Picture {
    /// Its time from the start of the film; negative when the stream gives none.
    pub pts_ms: i64,
    /// The hardware decoder produced it.
    pub hardware: bool,
}

pub struct Video {
    raw: NonNull<c_void>,
    api: &'static Api,
    pub info: VideoInfo,
}

// One owner at a time; FFmpeg contexts may move between threads.
unsafe impl Send for Video {}

impl Video {
    /// Open `path` for its pictures, fitted into `max_width` x `max_height`. `hardware`: try the
    /// hardware decoder first (software takes over when it refuses the stream).
    pub fn open(
        path: &Path,
        max_width: u32,
        max_height: u32,
        hardware: bool,
    ) -> Result<Self, String> {
        let api = api()?;
        let name = c_path(path)?;
        let mut raw = std::ptr::null_mut();
        let mut info = RawInfo::default();
        // SAFETY: valid NUL-terminated path and out-pointers for the duration of the call.
        let r = unsafe {
            (api.open)(
                name.as_ptr(),
                max_width as c_int,
                max_height as c_int,
                c_int::from(hardware),
                &mut raw,
                &mut info,
            )
        };
        let raw = NonNull::new(raw)
            .filter(|_| r >= 0)
            .ok_or_else(|| error_text(r))?;
        // SAFETY: C wrote a NUL-terminated name into the fixed buffer.
        let codec = unsafe { CStr::from_ptr(info.codec.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            raw,
            api,
            info: VideoInfo {
                width: info.width,
                height: info.height,
                out_width: info.out_width,
                out_height: info.out_height,
                fps_milli: info.fps_milli,
                duration_ms: info.duration_ms,
                subtitles: info.subtitles != 0,
                codec,
            },
        })
    }

    /// Decode the next picture; `None` at the end of the film.
    pub fn next_picture(&mut self) -> Result<Option<Picture>, String> {
        let (mut pts_ms, mut hardware) = (0i64, 0u32);
        // SAFETY: the reader is live and exclusively borrowed; both out-pointers are valid.
        let r = unsafe { (self.api.next)(self.raw.as_ptr(), &mut pts_ms, &mut hardware) };
        match r {
            0 => Ok(None),
            r if r < 0 => Err(error_text(r)),
            _ => Ok(Some(Picture {
                pts_ms,
                hardware: hardware != 0,
            })),
        }
    }

    /// The picture `next` returned, as `out_width` x `out_height` RGBA.
    pub fn convert(&mut self, rgba: &mut [u8]) -> Result<(), String> {
        let need = self.info.out_width as usize * self.info.out_height as usize * 4;
        if rgba.len() != need {
            return Err(format!(
                "picture buffer is {} bytes, not {need}",
                rgba.len()
            ));
        }
        // SAFETY: the buffer holds exactly the image the reader writes.
        let r = unsafe { (self.api.convert)(self.raw.as_ptr(), rgba.as_mut_ptr()) };
        if r < 0 {
            return Err(error_text(r));
        }
        Ok(())
    }

    /// Continue from the key picture at or before `ms`.
    pub fn seek(&mut self, ms: u64) -> Result<(), String> {
        // SAFETY: the reader is live and exclusively borrowed.
        let r = unsafe { (self.api.seek)(self.raw.as_ptr(), ms as i64) };
        if r < 0 {
            return Err(error_text(r));
        }
        Ok(())
    }

    /// While catching up: skip pictures no other picture needs.
    pub fn hurry(&mut self, on: bool) {
        // SAFETY: the reader is live and exclusively borrowed.
        unsafe { (self.api.hurry)(self.raw.as_ptr(), c_int::from(on)) }
    }

    /// Read the subtitle file `path` (SubRip, ASS, WebVTT) for a film without a text track of its
    /// own; `true` when it gave lines.
    pub fn subtitles_from(&mut self, path: &Path) -> bool {
        let Ok(name) = c_path(path) else {
            return false;
        };
        // SAFETY: the reader is live and exclusively borrowed; the path is NUL-terminated.
        let lines = unsafe { (self.api.subtitles)(self.raw.as_ptr(), name.as_ptr()) };
        self.info.subtitles |= lines > 0;
        lines > 0
    }

    /// The subtitle text shown at `ms` (lines separated by '\n'), empty for none.
    pub fn subtitle(&mut self, ms: i64) -> String {
        let mut text = [0 as c_char; 512];
        // SAFETY: C writes at most the buffer's length including the NUL.
        unsafe {
            (self.api.subtitle)(
                self.raw.as_ptr(),
                ms,
                text.as_mut_ptr(),
                text.len() as c_int,
            );
            CStr::from_ptr(text.as_ptr()).to_string_lossy().into_owned()
        }
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        // SAFETY: the reader was opened by this API and is closed once.
        unsafe { (self.api.close)(self.raw.as_ptr()) }
    }
}

/// One picture of the film at `path` as `side` x `side` RGBA (centred on black), for lists.
pub fn video_thumbnail(path: &Path, side: u32) -> Result<Vec<u8>, String> {
    let api = api()?;
    let name = c_path(path)?;
    let mut rgba = vec![0u8; side as usize * side as usize * 4];
    // SAFETY: the buffer holds side * side RGBA pixels.
    let r = unsafe { (api.thumbnail)(name.as_ptr(), rgba.as_mut_ptr(), side as c_int) };
    if r < 0 {
        return Err(error_text(r));
    }
    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With a film at hand:
    /// `REBORN_FILM=/path/film.mp4 cargo test -p reborn-media -- --ignored film`.
    #[test]
    #[ignore]
    fn a_film_scans_as_a_film_and_gives_pictures_and_a_thumbnail() {
        let path = std::env::var("REBORN_FILM").expect("REBORN_FILM names a film");
        let path = Path::new(&path);
        let mut file = crate::MediaFile::open(path, crate::Cancel::new().unwrap()).unwrap();
        assert!(file.metadata.codec.contains('/'), "{}", file.metadata.codec);
        assert!(file.metadata.artwork);
        assert_eq!(file.artwork(64).unwrap().len(), 64 * 64 * 4);
        let mut video = Video::open(path, 480, 360, false).unwrap();
        let info = video.info.clone();
        assert!(
            info.out_width <= 480 && info.out_height <= 360 && info.out_width.is_multiple_of(2)
        );
        let mut rgba = vec![0; info.out_width as usize * info.out_height as usize * 4];
        let first = video.next_picture().unwrap().expect("a first picture");
        assert!(!first.hardware);
        video.convert(&mut rgba).unwrap();
        assert!(rgba.iter().any(|&b| b != 0));
        video.seek(info.duration_ms / 2).unwrap();
        let later = video
            .next_picture()
            .unwrap()
            .expect("a picture after the seek");
        assert!(later.pts_ms <= info.duration_ms as i64 / 2 + 50);
        eprintln!(
            "{info:?} codec={} first={first:?} later={later:?}",
            file.metadata.codec
        );
    }

    #[test]
    fn a_missing_film_is_an_error_not_a_crash() {
        assert!(Video::open(Path::new("/nonexistent/film.mp4"), 480, 360, false).is_err());
        assert!(video_thumbnail(Path::new("/nonexistent/film.mp4"), 64).is_err());
    }
}
