//! Playlists as their own section: every playlist with a mosaic of its first
//! albums, Favorites with its heart, and one playlist's page (mosaic, Play and
//! Shuffle, its songs; songs the player does not have are shown dimmed; a song
//! being moved is lifted).
use crate::{
    canvas::{Canvas, Icon, Moving},
    components::{self, cover},
    playlists_ui::{is_favorites, summary},
    screens::screen_title,
    text::{self, Face},
    theme::{color, WIDTH},
    views_library::pill_row,
    Ui,
};
use reborn_core::{AppModel, Screen, Track};
use std::path::Path;

/// Playlists' own colour: a warm pink, as Radio has its red.
const TINT: u32 = 0xFF6FA5FF;
const FAVORITE: u32 = 0xFF5A7AFF;

pub fn screen(
    c: &mut Canvas,
    ui: &Ui,
    m: &AppModel,
    tracks: &[Track],
    has_art: bool,
    interactive: bool,
) {
    let _ = has_art;
    match m.screen {
        Screen::Playlists => home(c, ui, m, tracks, interactive),
        _ => page(c, ui, m, tracks, interactive),
    }
}

/// A playlist's picture: one cover, or four in a 2×2 grid.
fn mosaic(c: &mut Canvas, ui: &Ui, tracks: &[Track], picked: &[usize], x: f32, y: f32, size: f32) {
    match picked.len() {
        0 => {
            components::icon_tile(
                c,
                x,
                y,
                size,
                (size * 0.11).max(4.),
                TINT,
                Icon::Note,
                size * 0.45,
            );
        }
        1..=3 => cover(c, ui, Some((picked[0], &tracks[picked[0]])), x, y, size),
        _ => {
            let half = (size / 2.).floor();
            for (n, &t) in picked.iter().enumerate() {
                let (col, line) = ((n % 2) as f32, (n / 2) as f32);
                cover(
                    c,
                    ui,
                    Some((t, &tracks[t])),
                    x + col * half,
                    y + line * half,
                    half,
                );
            }
        }
    }
}

fn storage_of(m: &AppModel, path: &Path) -> &'static str {
    if path.starts_with(&m.settings.music_directory) {
        "Internal"
    } else {
        "SD Card"
    }
}

const ROW_H: f32 = 53.;
const ROWS: usize = 6;

fn home(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    let accent = ui.accent();
    components::status_bar(c, ui, m, "Playlists", true);
    let count = ui.row_count(m, tracks);
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    let first = components::list_window(m, focus, ROWS);
    let (from, shown) = components::scroll_rows(ui, first, ROWS, count);
    let rows_start = c.draw.len();
    for (n, row) in ui.visible_rows(m, tracks, from, shown).iter().enumerate() {
        let i = from + n;
        let y = 38. + (i as f32 - first as f32) * ROW_H;
        let selected = interactive && i == focus;
        if selected {
            components::focus_row(c, 10., y + 1., WIDTH - 24., ROW_H - 3.);
        }
        let (tx, ty, tile) = (20., y + 7., 38.);
        let playlist = row
            .key
            .strip_prefix("playlist:")
            .and_then(|i| i.parse::<usize>().ok())
            .and_then(|i| ui.playlists.lists.get(i));
        match playlist {
            None => {
                components::icon_tile(c, tx, ty, tile, 9., TINT, Icon::Plus, 20.);
            }
            Some(p) if is_favorites(&p.path) => {
                components::icon_tile(c, tx, ty, tile, 9., FAVORITE, Icon::HeartOn, 20.);
            }
            Some(p) => {
                let picked = ui.facts(tracks, p).mosaic.clone();
                mosaic(c, ui, tracks, &picked, tx, ty, tile);
            }
        }
        let face = if selected { Face::S15 } else { Face::R15 };
        c.text_mid(72., y + 18., 300., &row.label, face, color::TEXT);
        let mut detail = row.secondary.clone();
        if let Some(p) = playlist {
            if storage_of(m, &p.path) == "SD Card" {
                detail.push_str(" · SD Card");
            }
            if p.read_only {
                detail.push_str(" · View only");
            }
        }
        c.text_mid(72., y + 36., 330., &detail, Face::R11, color::MUTED);
        c.icon(Icon::Chevron, WIDTH - 36., y + ROW_H / 2., 16., color::DIM);
    }
    c.mark_moving(Moving::List {
        start: rows_start,
        end: c.draw.len(),
        top: 38.,
        bottom: 38. + ROWS as f32 * ROW_H,
        row_h: ROW_H,
        first,
    });
    if count > ROWS {
        components::scroll_rail(c, 44., 286., first, count, ROWS, accent);
    }
    let hint = if ui.playlists.loading && ui.playlists.lists.is_empty() {
        Some("Looking for playlists…")
    } else if ui.playlists.lists.is_empty() {
        Some("Copy .m3u or .m3u8 playlists to the player, or hold Select on any song.")
    } else {
        None
    };
    if let Some(hint) = hint {
        for (n, line) in text::wrap(Face::R13, hint, 400., 2).iter().enumerate() {
            c.text_center(
                240.,
                140. + n as f32 * 20. + Face::R13.mid_offset(),
                line,
                Face::R13,
                color::MUTED,
            );
        }
    }
}

fn page(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    let accent = ui.accent();
    components::status_bar(c, ui, m, "Playlist", true);
    let Some(p) = ui.playlists.get(Path::new(&m.navigation.filter)) else {
        c.text_center(
            240.,
            170. + Face::R15.mid_offset(),
            "This playlist is gone. It may have been deleted on a computer.",
            Face::R13,
            color::MUTED,
        );
        return;
    };
    let count = ui.row_count(m, tracks);
    let lead = ui.with_playlist_rows(m, tracks, |rows| {
        rows.iter()
            .take_while(|r| !r.key.starts_with("pl_entry:"))
            .count()
    });
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    let moving = ui.playlists.moving.is_some();
    // The header (picture, name, Play and Shuffle) is the list's first row
    // and scrolls away with it, as on an album page (8.37).
    let first = components::header_page_first(focus, lead, count, m.navigation.scroll);
    let offset = components::header_page_offset(first);
    let page_at = c.draw.len();
    if components::header_page_shows_header(ui, first) {
        let favorite = is_favorites(&p.path);
        let size = 104.;
        if favorite {
            components::icon_tile(c, 24., 44., size, 11., FAVORITE, Icon::HeartOn, 52.);
        } else {
            let picked = ui.facts(tracks, p).mosaic.clone();
            mosaic(c, ui, tracks, &picked, 24., 44., size);
        }
        c.text_mid(144., 70., 316., &screen_title(m), Face::S25, color::TEXT);
        let facts = ui.facts(tracks, p);
        let ms = facts.ms;
        c.text_mid(
            144.,
            96.,
            310.,
            &summary(p.entries.len(), ms),
            Face::R15,
            color::TEXT,
        );
        let missing = facts.missing;
        let mut where_ = format!(
            "{} · {}",
            storage_of(m, &p.path),
            if favorite { "Favorites" } else { "Playlist" }
        );
        if missing > 0 {
            where_ = format!("{where_} · {missing} not on the player");
        }
        if p.read_only {
            where_.push_str(" · View only");
        }
        c.text_mid(144., 117., 310., &where_, Face::R11, color::MUTED);
        if lead == 2 {
            pill_row(
                c,
                144.,
                134.,
                &[("Play", focus == 0), ("Shuffle", focus == 1)],
                interactive && !moving,
            );
        }
        if p.entries.is_empty() {
            for (n, line) in text::wrap(
                Face::R13,
                "Empty. Hold Select on any song, album or Now Playing and choose Add to Playlist.",
                420.,
                2,
            )
            .iter()
            .enumerate()
            {
                c.text_center(
                    240.,
                    214. + n as f32 * 20. + Face::R13.mid_offset(),
                    line,
                    Face::R13,
                    color::MUTED,
                );
            }
        }
    }
    let range = components::header_page_rows(ui, first, lead, count);
    for (n, row) in ui
        .visible_rows(m, tracks, range.start, range.len())
        .iter()
        .enumerate()
    {
        let i = range.start + n;
        let y = components::header_page_y(i, lead, 0.);
        let selected = interactive && i == focus;
        let lifted = moving
            && row
                .key
                .strip_prefix("pl_entry:")
                .and_then(|i| i.parse::<usize>().ok())
                == ui.playlists.moving.map(|(from, _)| from);
        let song = row.art.and_then(|i| tracks.get(i).map(|t| (i, t)));
        let available = song.is_some_and(|(_, t)| t.online);
        if lifted {
            // Being moved is a state: the focus row, ringed in the accent.
            components::focus_row(c, 8., y - 1., WIDTH - 16., ROW_H + 1.);
            c.rrect_outline(8., y - 1., WIDTH - 16., ROW_H + 1., 12., 2., accent);
        } else if selected {
            components::focus_row(c, 10., y + 1., WIDTH - 24., ROW_H - 3.);
        }
        match song {
            Some(s) => cover(c, ui, Some(s), 20., y + 7., 38.),
            None => {
                c.rrect(20., y + 7., 38., 38., 6., color::GLASS_FAINT);
                c.icon(Icon::Note, 39., y + 26., 18., color::DIM);
            }
        }
        let playing = song.is_some_and(|(_, t)| crate::screens::is_current(m, t));
        let face = if selected || lifted {
            Face::S15
        } else {
            Face::R15
        };
        let tint = if !available {
            color::DIM
        } else if playing {
            accent
        } else {
            color::TEXT
        };
        let tail = song.map_or(0., |(_, t)| {
            text::width(Face::R11, &components::time(t.duration_ms)) + 16.
        });
        c.text_mid(72., y + 18., 340. - tail, &row.label, face, tint);
        c.text_mid(
            72.,
            y + 36.,
            340. - tail,
            &row.secondary,
            Face::R11,
            if lifted { accent } else { color::MUTED },
        );
        if let Some((_, t)) = song {
            c.text_mid_right(
                WIDTH - 40.,
                y + 26.,
                &components::time(t.duration_ms),
                Face::R11,
                color::DIM,
            );
        }
    }
    components::shift_from(c, page_at, -offset);
    c.mark_moving(Moving::Scroll {
        start: page_at,
        end: c.draw.len(),
        top: crate::views_search::TOP,
        bottom: crate::theme::HEIGHT,
        offset,
        row_h: components::PAGE_ROW,
    });
    let total = components::PAGE_HEADER + (count - lead) as f32 * components::PAGE_ROW;
    components::page_rail(c, offset, total, accent);
}
