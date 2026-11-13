//! Videos: the list of the library's films, and the picture on Now Playing while one plays (the
//! film's sound is a song to the player; its pictures arrive from the app's video reader as a
//! texture, with the subtitle line of the moment).
use crate::{
    canvas::Canvas,
    components,
    text::{self, Face},
    theme::{color, HEIGHT, WIDTH},
    Item, Ui,
};
use reborn_core::{AppModel, PlaybackState, Track};
use reborn_graphics::{Quad, Tex};

/// The picture the app last uploaded for the playing film, and its subtitle text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoPicture {
    /// Size of the uploaded picture in pixels; 0: none yet.
    pub width: u32,
    pub height: u32,
    /// The subtitle shown now (lines separated by '\n'), empty for none.
    pub subtitle: String,
}

/// The library's films by title: the Videos list's order, and the order Next and Previous
/// follow while a film plays (Y2Reborn #128).
pub(crate) fn films(tracks: &[Track]) -> Vec<usize> {
    let mut films: Vec<(usize, &Track)> = tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.online && t.is_video())
        .collect();
    films.sort_by_cached_key(|(_, t)| crate::track_title(t).to_lowercase());
    films.into_iter().map(|(i, _)| i).collect()
}

/// Every film of the library by title, each with its length; Select plays it.
pub(crate) fn rows(tracks: &[Track]) -> Vec<Item> {
    films(tracks)
        .into_iter()
        .map(|i| {
            let t = &tracks[i];
            let mut item = Item::new(crate::track_title(t), format!("track:{i}"))
                .with_secondary(crate::views_books::length(t.duration_ms));
            item.art = Some(i);
            item
        })
        .collect()
}

/// Where a `width` x `height` picture stands on the screen: as large as fits, centred.
pub fn frame(width: u32, height: u32) -> (f32, f32, f32, f32) {
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    let scale = (WIDTH / w).min(HEIGHT / h);
    let (w, h) = ((w * scale).round(), (h * scale).round());
    (
        ((WIDTH - w) / 2.).round(),
        ((HEIGHT - h) / 2.).round(),
        w,
        h,
    )
}

fn clock(ms: u64) -> String {
    let s = ms / 1000;
    match s / 3600 {
        0 => format!("{}:{:02}", s / 60, s % 60),
        h => format!("{h}:{:02}:{:02}", s / 60 % 60, s % 60),
    }
}

/// Now Playing for a film: the picture on black, subtitles over it, and while paused or just
/// after a button the title and the time line.
pub fn now_playing(c: &mut Canvas, ui: &Ui, m: &AppModel, t: &Track) {
    c.rect(0., 0., WIDTH, HEIGHT, 0x000000FF);
    let picture = &ui.video;
    if picture.width > 0 {
        let (x, y, w, h) = frame(picture.width, picture.height);
        c.draw.push(Quad {
            tex: Tex::Video,
            ..Quad::rect(x, y, w, h, color::TEXT)
        });
    } else {
        c.text_mid_center(
            WIDTH / 2.,
            HEIGHT / 2.,
            "Starting…",
            Face::S13,
            color::MUTED,
        );
    }
    let playing = matches!(
        m.playback,
        PlaybackState::Playing | PlaybackState::Buffering
    );
    let controls = !playing || ui.video_controls_shown();
    if !picture.subtitle.is_empty() {
        // Above the time line while it shows, at the bottom edge otherwise.
        let lines: Vec<&str> = picture.subtitle.lines().take(3).collect();
        let bottom = if controls { HEIGHT - 76. } else { HEIGHT - 18. };
        let mut baseline = bottom - (lines.len().saturating_sub(1)) as f32 * 20.;
        for line in lines {
            let line = text::fit(Face::S15, line, WIDTH - 24.);
            // A dark edge keeps white text readable on a bright picture.
            for (dx, dy) in [(-1., 0.), (1., 0.), (0., -1.), (0., 1.)] {
                c.text_center(WIDTH / 2. + dx, baseline + dy, &line, Face::S15, 0x000000E0);
            }
            c.text_center(WIDTH / 2., baseline, &line, Face::S15, color::TEXT);
            baseline += 20.;
        }
    }
    if !controls {
        return;
    }
    c.fade_from_bottom(0., HEIGHT - 110., WIDTH, 110., 0x000000D8);
    components::status_bar(c, ui, m, "", true);
    c.text(
        16.,
        HEIGHT - 44.,
        &text::fit(Face::S15, crate::track_title(t), WIDTH - 32.),
        Face::S15,
        color::TEXT,
    );
    let duration = t.duration_ms.max(1);
    let done = (m.position_ms.min(duration) as f32 / duration as f32).clamp(0., 1.);
    let (bar_x, bar_w, bar_y) = (16., WIDTH - 32., HEIGHT - 30.);
    c.rrect(bar_x, bar_y, bar_w, 4., 2., 0xFFFFFF40);
    c.rrect(bar_x, bar_y, (bar_w * done).max(4.), 4., 2., ui.accent());
    let state = if playing { "" } else { "Paused · " };
    c.text(
        16.,
        HEIGHT - 10.,
        &format!("{state}{}", clock(m.position_ms)),
        Face::R11,
        color::MUTED,
    );
    c.text_right(
        WIDTH - 16.,
        HEIGHT - 10.,
        &clock(t.duration_ms),
        Face::R11,
        color::MUTED,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_fills_the_screen_in_its_own_shape() {
        assert_eq!(frame(480, 270), (0., 45., 480., 270.));
        assert_eq!(frame(240, 180), (0., 0., 480., 360.));
        assert_eq!(frame(202, 360), (139., 0., 202., 360.));
        assert_eq!(clock(59_000), "0:59");
        assert_eq!(clock(3_725_000), "1:02:05");
    }
}
