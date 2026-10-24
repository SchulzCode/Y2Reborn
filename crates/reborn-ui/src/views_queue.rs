//! Up Next (Beta 8.33), drawn like the search page: rows under headings,
//! scrolled by their heights. See `up_next` for its parts.
use crate::{
    canvas::{Canvas, Moving},
    components::{self, cover},
    text::{self, Face},
    theme::{color, HEIGHT, WIDTH},
    up_next::Row,
    views_search::{window, HEADING_ROW, ROW, TOP, VIEW_H},
    Ui,
};
use reborn_core::{AppModel, Track};

pub fn page(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    let layout = ui.up_next(m, tracks);
    let count = layout.len();
    components::status_bar(c, ui, m, &ui.list_title(m), true);
    if count == 0 {
        return crate::views_library::empty(c, ui, m, interactive);
    }
    let accent = ui.accent();
    let heights = ui.search_heights(m, tracks);
    let focus = m.navigation.focus.min(count - 1);
    let first = window(&heights, focus, m.navigation.scroll);
    let mut tops = Vec::with_capacity(count + 1);
    let mut y = 0.;
    for h in &heights {
        tops.push(y);
        y += h;
    }
    tops.push(y);
    let offset = tops[first];
    let from = if ui.ui_motion {
        first.saturating_sub(1)
    } else {
        first
    };
    let reach = if ui.ui_motion { VIEW_H + ROW } else { VIEW_H };
    let moving = ui.moving();
    let rows_start = c.draw.len();
    let mut index = from;
    while index < count && tops[index] - offset < reach {
        let (Some(row), Some(item)) = (
            layout.row(index),
            ui.up_next_item(m, tracks, &layout, index),
        ) else {
            break;
        };
        let y = TOP + tops[index] - offset;
        let selected = interactive && index == focus;
        if row.heading() {
            let mid = y + HEADING_ROW / 2. + 3.;
            let label = text::fit(Face::S10, &item.label.to_uppercase(), 300.);
            components::section_label(c, 20., mid, &label);
            c.text_mid_right(WIDTH - 40., mid, &item.secondary, Face::R11, color::DIM);
            index += 1;
            continue;
        }
        let (song, tone) = match row {
            Row::History(lib) => (&tracks[lib], Tone::Played),
            Row::Now => (&m.queue[m.queue_position], Tone::Playing),
            Row::Queued(i) | Row::Source(i) => (&m.queue[i], Tone::Next),
            _ => unreachable!("headings are drawn above"),
        };
        let moved = moving.is_some_and(|id| item.key == format!("queue:{}", id.0));
        if selected || moved {
            components::focus_row(c, 10., y + 1., WIDTH - 24., ROW - 3.);
        }
        if moved {
            c.rrect_outline(10., y + 1., WIDTH - 24., ROW - 3., 12., 1.5, accent);
        }
        let art = item.art.and_then(|i| tracks.get(i).map(|t| (i, t)));
        cover(c, ui, art, 20., y + 7., 38.);
        let (title_ink, sub, sub_ink) = match tone {
            Tone::Playing => (accent, format!("Playing  ·  {}", item.secondary), accent),
            Tone::Played => (color::DIM, item.secondary.clone(), color::DIM),
            Tone::Next => (color::TEXT, item.secondary.clone(), color::MUTED),
        };
        let mark = ui.selection_mark(m, &item.key);
        let time = components::time(song.duration_ms);
        let tail = match mark {
            Some(_) => 34.,
            None => text::width(Face::R11, &time) + 16.,
        };
        c.text_mid(
            70.,
            y + 18.,
            340. - tail,
            &item.label,
            if selected { Face::S15 } else { Face::R15 },
            title_ink,
        );
        c.text_mid(70., y + 36., 340. - tail, &sub, Face::R11, sub_ink);
        match mark {
            Some(on) => components::select_mark(c, WIDTH - 50., y + 26., on, accent),
            None => c.text_mid_right(
                WIDTH - 40.,
                y + 26.,
                &time,
                Face::R11,
                if selected { color::TEXT } else { color::DIM },
            ),
        }
        index += 1;
    }
    c.mark_moving(Moving::Scroll {
        start: rows_start,
        end: c.draw.len(),
        top: TOP,
        bottom: HEIGHT,
        offset,
        row_h: ROW,
    });
    let total = tops[count];
    if total > VIEW_H {
        let thumb = (VIEW_H * VIEW_H / total).clamp(18., VIEW_H - 12.);
        let rail_top = TOP + 6.;
        let rail_h = VIEW_H - 12.;
        let at = rail_top + (rail_h - thumb) * (offset / (total - VIEW_H)).min(1.);
        c.rrect(WIDTH - 14., rail_top, 2., rail_h, 1., color::TRACK);
        c.rrect(WIDTH - 14., at.round(), 2., thumb.round(), 1., accent);
    }
}

/// How a row reads: played (quiet), playing (the accent), still to come.
enum Tone {
    Played,
    Playing,
    Next,
}
