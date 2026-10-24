//! The search page (Beta 8.32): the field, then what was found under
//! headings — artists, albums, audiobooks, songs with Play All. The keys
//! (`keyboard`) sit over it while typing and fold away into it; the field
//! stays in place, and Select on it brings the keys back.
use crate::{
    canvas::{Canvas, Icon, Moving},
    catalog::SearchKind,
    components::{self, cover},
    keyboard,
    screens::{is_current, row_track},
    text::{self, Face},
    theme::{color, HEIGHT, WIDTH},
    Ui,
};
use reborn_core::{AppModel, Track};

/// Where the rows start, under the status bar.
const TOP: f32 = 38.;
const VIEW_H: f32 = HEIGHT - TOP;
const FIELD_ROW: f32 = 44.;
const HEADING_ROW: f32 = 28.;
pub(crate) const ROW: f32 = 53.;

pub(crate) fn row_height(kind: SearchKind) -> f32 {
    match kind {
        SearchKind::Field => FIELD_ROW,
        SearchKind::Heading => HEADING_ROW,
        SearchKind::PlayAll | SearchKind::Result => ROW,
    }
}

/// The first row shown so the focused one is wholly in view: from the
/// last first row, a result coming into view from above brings its heading,
/// and the first heading brings the field.
pub(crate) fn window(heights: &[f32], focus: usize, scroll: usize) -> usize {
    if heights.is_empty() {
        return 0;
    }
    let focus = focus.min(heights.len() - 1);
    let mut first = scroll.min(focus);
    if first == focus && first > 0 && heights[first - 1] == HEADING_ROW {
        first -= 1;
    }
    if first == 1 {
        first = 0;
    }
    while first < focus && heights[first..=focus].iter().sum::<f32>() > VIEW_H {
        first += 1;
    }
    first
}

pub fn page(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    let accent = ui.accent();
    components::status_bar(c, ui, m, "Search", true);
    let heights = ui.search_heights(m, tracks);
    let (summary, found) = ui.search_summary(m, tracks);
    let count = heights.len();
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    let first = window(&heights, focus, m.navigation.scroll);
    let mut tops = Vec::with_capacity(count + 1);
    let mut y = 0.;
    for h in &heights {
        tops.push(y);
        y += h;
    }
    tops.push(y);
    let offset = tops[first];
    // With UI motion a row more on each side slides in as the page scrolls.
    let from = if ui.ui_motion {
        first.saturating_sub(1)
    } else {
        first
    };
    let reach = if ui.ui_motion { VIEW_H + ROW } else { VIEW_H };
    let rows_start = c.draw.len();
    let mut index = from;
    while index < count && tops[index] - offset < reach {
        let Some(row) = ui.row_at(m, tracks, index) else {
            break;
        };
        let y = TOP + tops[index] - offset;
        let selected = interactive && index == focus;
        match row_kind(&row.key, row.enabled) {
            SearchKind::Field => {
                if selected {
                    components::focus_row(
                        c,
                        10.,
                        keyboard::FIELD_Y - TOP + y,
                        WIDTH - 24.,
                        keyboard::FIELD_H,
                    );
                }
                // The field as the keys left it; focused, its caret says
                // Select types again.
                keyboard::field(
                    c,
                    (
                        10.,
                        keyboard::FIELD_Y - TOP + y,
                        WIDTH - 24.,
                        keyboard::FIELD_H,
                    ),
                    &m.navigation.filter,
                    // Nothing found: the page says so below.
                    if m.navigation.filter.trim().is_empty() || found == 0 {
                        ""
                    } else {
                        &summary
                    },
                    "Artist, album or song",
                    selected.then_some(accent),
                );
            }
            SearchKind::Heading => {
                let mid = y + HEADING_ROW / 2. + 3.;
                components::section_label(c, 20., mid, &row.label);
                c.text_mid_right(WIDTH - 40., mid, &row.secondary, Face::R11, color::DIM);
            }
            SearchKind::PlayAll => {
                if selected {
                    components::focus_row(c, 10., y + 1., WIDTH - 24., ROW - 3.);
                }
                c.rrect(20., y + 7., 38., 38., 8., color::GLASS);
                c.icon(Icon::Play, 39., y + 26., 20., color::TEXT);
                lines(c, &row.label, &row.secondary, y, 0., selected, color::TEXT);
            }
            SearchKind::Result => {
                if selected {
                    components::focus_row(c, 10., y + 1., WIDTH - 24., ROW - 3.);
                }
                let art = row.art.and_then(|i| tracks.get(i).map(|t| (i, t)));
                cover(c, ui, art, 20., y + 7., 38.);
                if row.key.starts_with("artist:") {
                    components::round_cover(c, 20., y + 7., 38., selected);
                }
                let song = row_track(&row, tracks).map(|(_, t)| t);
                let playing = song.is_some_and(|t| is_current(m, t));
                let tail = song.map_or(0., |t| {
                    text::width(Face::R11, &components::time(t.duration_ms)) + 16.
                });
                lines(
                    c,
                    &row.label,
                    &row.secondary,
                    y,
                    tail,
                    selected,
                    if playing { accent } else { color::TEXT },
                );
                if let Some(t) = song {
                    c.text_mid_right(
                        WIDTH - 40.,
                        y + 26.,
                        &components::time(t.duration_ms),
                        Face::R11,
                        if selected { color::TEXT } else { color::DIM },
                    );
                }
            }
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
    if !m.navigation.filter.trim().is_empty() && found == 0 {
        components::empty_state(
            c,
            150.,
            components::Mark::Icon(Icon::Search),
            "No Matches",
            "Check the spelling, or type less of the name.",
        );
    }
    // The rail follows the rows' place on the page, not their count.
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

fn row_kind(key: &str, enabled: bool) -> SearchKind {
    match key {
        crate::catalog::SEARCH_FIELD => SearchKind::Field,
        crate::catalog::SEARCH_PLAY_ALL => SearchKind::PlayAll,
        _ if !enabled => SearchKind::Heading,
        _ => SearchKind::Result,
    }
}

/// A row's title and the line under it, leaving `tail` free at the right.
fn lines(
    c: &mut Canvas,
    label: &str,
    secondary: &str,
    y: f32,
    tail: f32,
    selected: bool,
    title: u32,
) {
    c.text_mid(
        70.,
        y + 18.,
        340. - tail,
        label,
        if selected { Face::S15 } else { Face::R15 },
        title,
    );
    c.text_mid(
        70.,
        y + 36.,
        340. - tail,
        secondary,
        Face::R11,
        color::MUTED,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_keeps_the_focus_in_view_with_its_heading_and_the_field() {
        // Field, Artists heading, 2 artists, Songs heading, Play All, 8 songs.
        let mut h = vec![FIELD_ROW, HEADING_ROW, ROW, ROW, HEADING_ROW, ROW];
        h.extend([ROW; 8]);
        // The first result: the field and its heading stay in view.
        assert_eq!(window(&h, 2, 0), 0);
        // Far down: as many rows as fit end at the focus.
        let first = window(&h, 13, 0);
        let shown: f32 = h[first..=13].iter().sum();
        assert!(shown <= VIEW_H && shown + h[first - 1] > VIEW_H, "{first}");
        // Back up to Play All: the Songs heading comes along.
        assert_eq!(window(&h, 5, 13), 4);
        // Up to the first artist: back to the top.
        assert_eq!(window(&h, 2, 4), 0);
        assert_eq!(window(&[], 3, 3), 0);
    }
}
