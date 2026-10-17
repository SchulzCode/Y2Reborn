//! Cover Flow: the album covers stand in a row, the focused one faces the
//! listener and the others turn toward it. The wheel glides the row; Select
//! flies the centre cover into the album page.
use crate::{
    canvas::Canvas,
    components::{self, art_key},
    flow::{self, TransitionKind},
    screens::screen_title,
    text::{self, Face},
    theme::{alpha, color, mix, HEIGHT, WIDTH},
    views_library::letter_bubble,
    Ui,
};
use reborn_core::{AppModel, Track};
use reborn_graphics::{Quad, Tex, Warp};

/// Covers drawn on each side of the centre.
const SIDE: usize = 5;
/// The album page's cover box, where the centre cover flies to.
pub const PAGE_COVER: (f32, f32, f32) = (24., 44., 104.);

fn gray(level: f32, a: u8) -> u32 {
    let v = (level.clamp(0., 1.) * 255.).round() as u32;
    v << 24 | v << 16 | v << 8 | u32::from(a)
}

fn box_corners(x: f32, y: f32, s: f32) -> [[f32; 2]; 4] {
    [[x, y], [x + s, y], [x + s, y + s], [x, y + s]]
}

fn lerp_corners(a: [[f32; 2]; 4], b: [[f32; 2]; 4], t: f32) -> [[f32; 2]; 4] {
    let mut out = a;
    for i in 0..4 {
        for k in 0..2 {
            out[i][k] = a[i][k] + (b[i][k] - a[i][k]) * t;
        }
    }
    out
}

fn warp(tex: Tex, corners: [[f32; 2]; 4], colors: [u32; 4]) -> Quad {
    Quad {
        tex,
        warp: Some(Warp { corners, colors }),
        ..Quad::rect(0., 0., 1., 1., colors[0])
    }
}

pub fn albums(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    components::status_bar(c, ui, m, &screen_title(m), true);
    let count = ui.row_count(m, tracks);
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    let pos = ui.flow_position(m, focus);
    // Back from the album page: the cover lands and the row fades in.
    let landing = ui
        .transition_progress()
        .filter(|(kind, _)| *kind == TransitionKind::Close)
        .map(|(_, t)| t);
    let first = (pos.floor() as usize).saturating_sub(SIDE);
    let last = ((pos.ceil() as usize) + SIDE).min(count.saturating_sub(1));
    let rows = ui.visible_rows(m, tracks, first, last + 1 - first);
    let mut order: Vec<usize> = (0..rows.len()).collect();
    let offset = |i: usize| (first + i) as f32 - pos;
    // Far covers first, the centre last.
    order.sort_by(|a, b| offset(*b).abs().total_cmp(&offset(*a).abs()));
    let mut focus_quad = None;
    for i in order {
        let row = &rows[i];
        let index = first + i;
        let d = offset(i);
        let mut corners = flow::cover_corners(d);
        let mut fade = 1.;
        if let Some(t) = landing {
            if index == focus {
                let (x, y, s) = PAGE_COVER;
                corners = lerp_corners(box_corners(x, y, s), corners, t);
            } else {
                fade = t;
            }
        }
        let level = 1. - 0.16 * d.abs().min(3.);
        let a = (255. * fade).round() as u8;
        let art = row.art.and_then(|a| tracks.get(a).map(|t| (a, t)));
        let key = art.map(|(_, t)| art_key(t));
        let tex = match (art, key) {
            (Some(_), Some(k)) if index == focus && ui.collection_art.get() == Some(k) => {
                Some(Tex::CollectionArt)
            }
            (Some(_), Some(k)) if ui.thumb_resident(k) => Some(Tex::Thumb(k)),
            (Some((at, _)), Some(k)) => {
                ui.want_thumb(k, at);
                None
            }
            _ => None,
        };
        let (tex, tint) = match tex {
            Some(tex) => (tex, gray(level, a)),
            // Loading or no cover: the calm placeholder tint, dimmed the same way.
            None => {
                let base = components::placeholder_color(key.unwrap_or(index as u64 * 977));
                (
                    Tex::Solid,
                    mix(base & !0xff | u32::from(a), color::BG, 1. - level),
                )
            }
        };
        // Reflection first: the cover's own bottom edge sits on it.
        let r = flow::reflection_corners(corners);
        let top = alpha(tint, (0x50 as f32 * fade) as u8);
        c.draw
            .push(warp(tex, r, [tint & !0xff, tint & !0xff, top, top]));
        let at = c.draw.len();
        c.draw.push(warp(tex, corners, [tint; 4]));
        if index == focus {
            focus_quad = Some(at);
        }
    }
    // The reflections melt into the background above the titles.
    let floor = flow::CENTRE_Y + flow::SIZE / 2.;
    let clear = color::BG & !0xff;
    c.draw.push(warp(
        Tex::Solid,
        [
            [0., floor + 4.],
            [WIDTH, floor + 4.],
            [WIDTH, floor + 52.],
            [0., floor + 52.],
        ],
        [clear, clear, color::BG, color::BG],
    ));
    c.rect(0., floor + 52., WIDTH, HEIGHT - floor - 52., color::BG);
    if interactive {
        if let Some(at) = focus_quad {
            c.mark_focus(at);
        }
    }
    if let Some(row) = ui.row_at(m, tracks, focus) {
        c.text_center(
            240.,
            266. + Face::S20.mid_offset(),
            &text::fit(Face::S20, &row.label, 440.),
            Face::S20,
            color::TEXT,
        );
        c.text_center(
            240.,
            290. + Face::R13.mid_offset(),
            &text::fit(Face::R13, &row.secondary, 440.),
            Face::R13,
            color::MUTED,
        );
        if row.key != "all_songs" {
            c.text_center(
                240.,
                312. + Face::R11.mid_offset(),
                &ui.album_detail(m, tracks, &row.key),
                Face::R11,
                color::DIM,
            );
        }
        c.text_center(
            240.,
            338. + Face::R11.mid_offset(),
            &format!("{} of {}", focus + 1, count),
            Face::R11,
            color::DIM,
        );
    }
    // A fast spin names the letter it is passing, over the centre cover.
    letter_bubble(c, ui, m, tracks, focus, flow::CENTRE_Y);
    // Once the row rests, ask for the centre cover at full resolution.
    if let Some((at, t)) = ui
        .row_at(m, tracks, focus)
        .and_then(|r| r.art)
        .and_then(|a| tracks.get(a).map(|t| (a, t)))
    {
        ui.request_cover(art_key(t), at);
    }
}
