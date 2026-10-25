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
pub(crate) const TOP: f32 = 38.;
pub(crate) const VIEW_H: f32 = HEIGHT - TOP;
const FIELD_ROW: f32 = 44.;
/// "See All 24 Songs": a text row under a section's first results.
const SEE_ALL_ROW: f32 = 44.;
pub(crate) const HEADING_ROW: f32 = 28.;
pub(crate) const ROW: f32 = 53.;

pub(crate) fn row_height(kind: SearchKind) -> f32 {
    match kind {
        SearchKind::Field => FIELD_ROW,
        SearchKind::Heading => HEADING_ROW,
        SearchKind::SeeAll => SEE_ALL_ROW,
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

/// The search page's first result in view: the field (when `fixed`, row 0)
/// stays and the results under it scroll as every list (8.37: the field
/// scrolled away); a result coming into view from above brings its heading.
pub(crate) fn search_first(heights: &[f32], focus: usize, scroll: usize, fixed: bool) -> usize {
    let skip = usize::from(fixed && !heights.is_empty());
    if heights.len() <= skip {
        return skip;
    }
    let view = VIEW_H - heights[..skip].iter().sum::<f32>();
    let last = heights.len() - 1;
    // The field focused: the results stay where they are.
    let focus = if focus < skip {
        scroll.clamp(skip, last)
    } else {
        focus.min(last)
    };
    let mut first = scroll.clamp(skip, focus);
    if first == focus && first > skip && heights[first - 1] == HEADING_ROW {
        first -= 1;
    }
    while first < focus && heights[first..=focus].iter().sum::<f32>() > view {
        first += 1;
    }
    first
}

/// The search's field: as the keys left it; focused, its caret says Select
/// types again.
fn field_row(
    c: &mut Canvas,
    m: &AppModel,
    y: f32,
    selected: bool,
    summary: &str,
    found: usize,
    accent: u32,
) {
    let query = crate::catalog::search_parts(&m.navigation.filter).0;
    if selected {
        components::focus_row(
            c,
            10.,
            keyboard::FIELD_Y - TOP + y,
            WIDTH - 24.,
            keyboard::FIELD_H,
        );
    }
    keyboard::field(
        c,
        (
            10.,
            keyboard::FIELD_Y - TOP + y,
            WIDTH - 24.,
            keyboard::FIELD_H,
        ),
        query,
        // Nothing found: the page says so below.
        if query.trim().is_empty() || found == 0 {
            ""
        } else {
            summary
        },
        "Artist, album or song",
        selected.then_some(accent),
    );
}

pub fn page(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    let accent = ui.accent();
    // "Search", or the section shown alone (See All).
    let title = if ui.selecting(m).is_some() {
        ui.list_title(m)
    } else {
        crate::screens::screen_title(m)
    };
    components::status_bar(c, ui, m, &title, true);
    let heights = ui.search_heights(m, tracks);
    let (summary, found) = ui.search_summary(m, tracks);
    let count = heights.len();
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    // The field stays at the top; the results scroll under it (8.37).
    let fixed = ui
        .row_at(m, tracks, 0)
        .is_some_and(|r| r.key == crate::catalog::SEARCH_FIELD);
    let skip = usize::from(fixed);
    let first = search_first(&heights, focus, m.navigation.scroll, fixed);
    let mut tops = Vec::with_capacity(count + 1);
    let mut y = 0.;
    for h in &heights {
        tops.push(y);
        y += h;
    }
    tops.push(y);
    let list_top = TOP + tops[skip.min(count)];
    let offset = tops[first.min(count)];
    // With UI motion a row more on each side slides in as the page scrolls.
    let from = if ui.ui_motion {
        first.saturating_sub(1).max(skip)
    } else {
        first
    };
    let reach = HEIGHT - list_top + if ui.ui_motion { ROW } else { 0. };
    let rows_start = c.draw.len();
    let mut index = from;
    while index < count && tops[index] - offset < reach {
        let Some(row) = ui.row_at(m, tracks, index) else {
            break;
        };
        let y = list_top + tops[index] - offset;
        let selected = interactive && index == focus;
        match row_kind(&row.key, row.enabled) {
            // Drawn over the results below.
            SearchKind::Field => {}
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
            SearchKind::SeeAll => {
                if selected {
                    components::focus_row(c, 10., y + 1., WIDTH - 24., SEE_ALL_ROW - 3.);
                }
                let mid = y + (SEE_ALL_ROW - 3.) / 2. + 1.;
                c.text_mid(
                    20.,
                    mid,
                    360.,
                    &row.label,
                    if selected { Face::S15 } else { Face::R15 },
                    accent,
                );
                c.icon(Icon::Chevron, WIDTH - 44., mid, 16., color::DIM);
            }
            SearchKind::Result => {
                if selected {
                    components::focus_row(c, 10., y + 1., WIDTH - 24., ROW - 3.);
                }
                let art = row.art.and_then(|i| tracks.get(i).map(|t| (i, t)));
                // Playlists as Playlists shows them: Favorites a heart.
                let favorites = row
                    .key
                    .strip_prefix("playlist:")
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| ui.playlists.lists.get(i))
                    .is_some_and(|p| crate::playlists_ui::is_favorites(&p.path));
                if favorites {
                    components::icon_tile(c, 20., y + 7., 38., 9., 0xFF5A7AFF, Icon::HeartOn, 20.);
                } else {
                    cover(c, ui, art, 20., y + 7., 38.);
                }
                if row.key.starts_with("artist:") {
                    components::round_cover(c, 20., y + 7., 38., selected);
                }
                let song = row_track(&row, tracks).map(|(_, t)| t);
                let playing = song.is_some_and(|t| is_current(m, t));
                let mark = ui.selection_mark(m, &row.key);
                let tail = if mark.is_some() {
                    34.
                } else {
                    song.map_or(0., |t| {
                        text::width(Face::R11, &components::time(t.duration_ms)) + 16.
                    })
                };
                lines(
                    c,
                    &row.label,
                    &row.secondary,
                    y,
                    tail,
                    selected,
                    if playing { accent } else { color::TEXT },
                );
                if let Some(on) = mark {
                    components::select_mark(c, WIDTH - 50., y + 26., on, accent);
                } else if let Some(t) = song {
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
        top: list_top,
        bottom: HEIGHT,
        offset,
        row_h: ROW,
    });
    if fixed {
        // A result gliding up passes under the field.
        c.rect(0., TOP, WIDTH, list_top - TOP, color::BG);
        field_row(
            c,
            m,
            TOP,
            interactive && focus == 0,
            &summary,
            found,
            accent,
        );
    }
    if !crate::catalog::search_parts(&m.navigation.filter)
        .0
        .trim()
        .is_empty()
        && found == 0
    {
        components::empty_state(
            c,
            150.,
            components::Mark::Icon(Icon::Search),
            "No Matches",
            "Check the spelling, or type less of the name.",
        );
    }
    // The rail follows the results' place on the page, not their count.
    let field = tops[skip.min(count)];
    components::page_rail(c, list_top, offset - field, tops[count] - field, accent);
}

fn row_kind(key: &str, enabled: bool) -> SearchKind {
    match key {
        crate::catalog::SEARCH_FIELD => SearchKind::Field,
        crate::catalog::SEARCH_PLAY_ALL => SearchKind::PlayAll,
        k if k.starts_with(crate::catalog::SEARCH_SEE_ALL) => SearchKind::SeeAll,
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
