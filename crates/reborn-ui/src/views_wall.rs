//! The Wall: every album cover edge to edge, six across with hairline gaps,
//! scrolling under the header after the focus (an eased value, as every
//! list scrolls since Beta 7.2). The focused cover lifts out
//! of the wall and its album is named in a caption at the bottom.
use crate::{
    canvas::Canvas,
    components::{self, cover_rounded, cover_shadow},
    screens::screen_title,
    text::{self, Face},
    theme::{alpha, color, layout, HEIGHT, WIDTH},
    Ui,
};
use reborn_core::{AppModel, Track};
use reborn_graphics::{Quad, Tex, Warp};

const COLUMNS: usize = 6;
const GAP: f32 = 2.;
/// Six covers and five gaps span 478 of the 480 pixels.
const CELL: f32 = 78.;
const PITCH: f32 = CELL + GAP;
/// The focused cover grows to about thumbnail size (96 px) as it lifts.
const LIFT: f32 = 1.24;

/// Scroll in pixels for a fractional focused `row`: that row about a third
/// of the way down the area under the header, never past either end.
fn scroll_for(row: f32, rows: usize) -> f32 {
    let area = HEIGHT - layout::LIST_TOP;
    let content = rows as f32 * PITCH - GAP;
    let max = (content - area).max(0.);
    (row * PITCH + CELL / 2. - area * 0.36).clamp(0., max)
}

pub fn albums(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    let top = layout::LIST_TOP;
    let area = HEIGHT - top;
    let count = ui.row_count(m, tracks);
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    let rows_total = count.div_ceil(COLUMNS);
    // The scroll eases toward the focused row's place (Beta 7.2's motion);
    // without UI motion it is there at once.
    let scroll = ui.ease(
        "wall:scroll",
        None,
        scroll_for((focus / COLUMNS) as f32, rows_total),
        crate::motion::CALM,
        0.5,
    );
    // A short library sits in the middle of the area under the header.
    let content = rows_total as f32 * PITCH - GAP;
    let y0 = top + ((area - content) / 2.).max(0.).floor() - scroll;
    let x0 = ((WIDTH - (COLUMNS as f32 * PITCH - GAP)) / 2.).floor();
    let place = |i: usize| {
        let (col, line) = ((i % COLUMNS) as f32, (i / COLUMNS) as f32);
        (x0 + col * PITCH, y0 + line * PITCH)
    };
    // Rows on screen, the one passing under the header included.
    let first_row = ((-y0) / PITCH).floor().max(0.) as usize;
    let last_row = (((HEIGHT - y0) / PITCH).ceil().max(0.) as usize).min(rows_total);
    let first = first_row * COLUMNS;
    let shown = (last_row.saturating_sub(first_row) * COLUMNS).min(count.saturating_sub(first));
    let rows = ui.visible_rows(m, tracks, first, shown);
    let art = |row: &crate::Item| row.art.and_then(|a| tracks.get(a).map(|t| (a, t)));
    for (n, row) in rows.iter().enumerate() {
        let i = first + n;
        if interactive && i == focus {
            continue;
        }
        let (x, y) = place(i);
        cover_rounded(c, ui, art(row), x, y, CELL, 4.);
        if interactive {
            // The rest of the wall steps back behind the focused cover.
            c.rect(x, y, CELL, CELL, 0x00000060);
        }
    }
    // The header reads over the covers passing under it.
    let bg = color::BG;
    c.rect(0., 0., WIDTH, top - 8., alpha(bg, 0xE0));
    c.draw.push(Quad {
        tex: Tex::Solid,
        warp: Some(Warp {
            corners: [
                [0., top - 8.],
                [WIDTH, top - 8.],
                [WIDTH, top + 8.],
                [0., top + 8.],
            ],
            colors: [alpha(bg, 0xE0), alpha(bg, 0xE0), bg & !0xff, bg & !0xff],
        }),
        ..Quad::rect(0., 0., 1., 1., bg)
    });
    components::status_bar(c, ui, m, &screen_title(m), true);
    // The focused cover last, lifted over its neighbours.
    if let Some(row) = rows.get(focus.wrapping_sub(first)).filter(|_| interactive) {
        let (x, y) = place(focus);
        let size = (CELL * LIFT).round();
        let x = (x + (CELL - size) / 2.).clamp(3., WIDTH - size - 3.);
        let y = (y + (CELL - size) / 2.).clamp(top + 3., HEIGHT - size - 3.);
        cover_shadow(c, x, y, size);
        let at = c.draw.len();
        cover_rounded(c, ui, art(row), x, y, size, 6.);
        c.rrect_outline(x - 1., y - 1., size + 2., size + 2., 7., 2., color::TEXT);
        c.mark_focus(at);
    }
    if content > area {
        let height = area - 16.;
        let thumb = (height * area / content).clamp(18., height);
        let y = top + 8. + (height - thumb) * scroll / (content - area);
        c.rrect(
            WIDTH - 6.,
            y.round(),
            3.,
            thumb.round(),
            1.,
            alpha(color::TEXT, 0x90),
        );
    }
    let Some(row) = ui.row_at(m, tracks, focus) else {
        return;
    };
    // The caption, at the bottom unless the focus is down there.
    let (_, fy) = place(focus);
    let y = if fy + CELL > HEIGHT - 60. {
        top + 8.
    } else {
        HEIGHT - 44.
    };
    caption(c, &row, focus, count, y);
}

/// The focused album named in a dark pill: title, then artist and place.
fn caption(c: &mut Canvas, row: &crate::Item, focus: usize, count: usize, y: f32) {
    let title = text::fit(Face::S15, &row.label, 250.);
    let detail = text::fit(
        Face::R13,
        &format!("{}  ·  {} of {}", row.secondary, focus + 1, count),
        420. - text::width(Face::S15, &title),
    );
    let w = (text::width(Face::S15, &title) + text::width(Face::R13, &detail) + 52.).min(464.);
    let x = (240. - w / 2.).round();
    c.rrect(x, y, w, 34., 17., color::SHEET);
    c.rrect_outline(x, y, w, 34., 17., 1., color::GLASS);
    let end = c.text(
        x + 20.,
        y + 17. + Face::S15.mid_offset(),
        &title,
        Face::S15,
        color::TEXT,
    );
    c.text(
        x + 32. + end,
        y + 17. + Face::R13.mid_offset(),
        &detail,
        Face::R13,
        color::MUTED,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wall_spans_the_screen_with_hairline_gaps() {
        let span = COLUMNS as f32 * PITCH - GAP;
        assert!(span <= WIDTH && WIDTH - span <= 4., "{span}");
        // About four rows fill the area under the header.
        let rows = (HEIGHT - layout::LIST_TOP) / PITCH;
        assert!((4. ..5.).contains(&rows), "{rows}");
    }

    #[test]
    fn the_wall_scrolls_to_keep_the_focused_row_in_view() {
        let top = layout::LIST_TOP;
        // The start does not scroll, nor does a library that fits.
        assert_eq!(scroll_for(0., 20), 0.);
        assert_eq!(scroll_for(1., 4), 0.);
        // Further down, the focused row rests a third of the way down.
        for row in [5., 8., 11.] {
            let y = top + row * PITCH - scroll_for(row, 20);
            assert!(y >= top && y + CELL <= HEIGHT, "{row}: {y}");
        }
        // The last row rests fully on screen, at the bottom.
        let y = top + 19. * PITCH - scroll_for(19., 20);
        assert!((y + CELL - HEIGHT).abs() < 1., "{y}");
    }
}
