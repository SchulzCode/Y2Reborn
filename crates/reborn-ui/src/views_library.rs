//! Home and the music library: lists with covers, the album grid, album and
//! artist pages, folders and the queue.
use crate::{
    canvas::{Canvas, Icon, Moving},
    components::{self, cover, cover_rounded, cover_shadow},
    screens::{is_current, row_track, screen_title, visible_rows_for},
    text::{self, Face},
    theme::{alpha, color, WIDTH},
    Item, Ui,
};
use reborn_core::{AppModel, MediaSource, Screen, Track};
use reborn_graphics::{Quad, Tex};

fn art_quad(c: &mut Canvas, x: f32, y: f32, size: f32, has_art: bool) {
    if has_art {
        c.draw.push(Quad {
            tex: Tex::Art,
            ..Quad::rect(x, y, size, size, color::TEXT)
        });
    } else {
        components::placeholder(c, x, y, size, size * 0.085, 0);
    }
}

// ----------------------------------------------------------------------- home

pub fn home(
    c: &mut Canvas,
    ui: &Ui,
    m: &AppModel,
    tracks: &[Track],
    has_art: bool,
    interactive: bool,
) {
    let accent = ui.accent();
    // The cover card is the one place outside Now Playing that wears the
    // cover's colour.
    let cover = ui.cover_accent();
    let current = m.current();
    if let Some(t) = current {
        c.glow(360., 172., 150., 160., alpha(cover, 0x46));
        art_quad(c, 266., 58., 180., has_art);
        // As wide as the line below: a long title ran into the menu.
        c.text_center(
            356.,
            270. + Face::S15.mid_offset(),
            &text::fit(Face::S15, crate::track_title(t), 200.),
            Face::S15,
            color::TEXT,
        );
        let sub = format!(
            "{}  ·  {} / {}",
            crate::display_or_unknown(&t.artist),
            components::time(m.position_ms),
            components::time(t.duration_ms)
        );
        let shown = text::fit(Face::R11, &sub, 200.);
        c.text_center(
            356.,
            290. + Face::R11.mid_offset(),
            &shown,
            Face::R11,
            color::MUTED,
        );
        components::slider(
            c,
            272.,
            313.,
            168.,
            components::progress(m.position_ms, t.duration_ms),
            cover,
            false,
        );
    } else {
        recently_added(c, ui, tracks);
    }
    components::home_bar(c, ui, m);
    let rows = ui.rows(m, tracks);
    let focus = m.navigation.focus.min(rows.len().saturating_sub(1));
    // Six rows by default, up to eleven with every Music list pinned; all end
    // above the screen's bottom edge.
    let step = (296. / rows.len().max(1) as f32).min(44.).floor();
    let mut y = 50.;
    for (i, row) in rows.iter().enumerate() {
        let selected = i == focus;
        let cy = y + if selected { step / 2. + 3. } else { step / 2. };
        if selected {
            let at = c.draw.len();
            c.rrect(26., y + 8., 4., 34., 2., accent);
            if interactive {
                c.mark_focus(at);
            }
            // The marker glides to the chosen place (Beta 7.2).
            let end = c.draw.len();
            c.mark_moving(Moving::Focus {
                start: at,
                end,
                y: y + 8.,
                layer: 0,
            });
            c.text(
                42.,
                cy + Face::S28.mid_offset(),
                &row.label,
                Face::S28,
                color::TEXT,
            );
        } else {
            c.text(
                42.,
                cy + Face::L23.mid_offset(),
                &row.label,
                Face::L23,
                color::DIM,
            );
        }
        y += if selected { step + 6. } else { step };
    }
}

// ---------------------------------------------------------------------- music

const MUSIC_H: f32 = 56.;

/// Music: the library's lists as menu entries, each with what it holds.
pub fn music(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    components::status_bar(c, ui, m, "Music", true);
    let rows = ui.rows(m, tracks);
    let focus = m.navigation.focus.min(rows.len().saturating_sub(1));
    for (i, row) in rows.iter().enumerate() {
        let (tint, icon) = match row.key.as_str() {
            "artists" => (0x6EA0FFFF, Icon::Person),
            "albums" => (0xFF7A59FF, Icon::Vinyl),
            "tracks" => (0x38C9B4FF, Icon::Headphones),
            "genres" => (0xA07BFFFF, Icon::Note),
            _ => (0x5CCB7AFF, Icon::Folder),
        };
        let y = 42. + i as f32 * MUSIC_H;
        components::menu_entry(c, row, y, MUSIC_H, tint, icon, interactive && i == focus);
    }
}

fn recently_added(c: &mut Canvas, ui: &Ui, tracks: &[Track]) {
    let mut seen = std::collections::HashSet::new();
    let recent: Vec<(usize, &Track)> = tracks
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, t)| t.is_music() && t.artwork && seen.insert(components::art_key(t)))
        .take(4)
        .collect();
    if recent.is_empty() {
        return;
    }
    components::section_label(c, 272., 72., "Recently added");
    for (slot, item) in recent.iter().enumerate() {
        let x = 272. + (slot % 2) as f32 * 92.;
        let y = 94. + (slot / 2) as f32 * 92.;
        cover_rounded(c, ui, Some(*item), x, y, 84., 10.);
    }
}

// --------------------------------------------------------------------- browse

pub fn browse(
    c: &mut Canvas,
    ui: &Ui,
    m: &AppModel,
    tracks: &[Track],
    has_art: bool,
    interactive: bool,
) {
    let count = ui.row_count(m, tracks);
    if count == 0 {
        components::status_bar(c, ui, m, &screen_title(m), true);
        return empty(c, ui, m, interactive);
    }
    match m.screen {
        Screen::Albums if m.settings.album_view == reborn_core::AlbumView::CoverFlow => {
            crate::views_coverflow::albums(c, ui, m, tracks, interactive)
        }
        Screen::Albums => albums_grid(c, ui, m, tracks, interactive),
        Screen::Album => album_page(c, ui, m, tracks, has_art, interactive),
        Screen::Artist => artist_page(c, ui, m, tracks, interactive),
        Screen::Folders if m.navigation.filter.is_empty() => {
            folder_roots(c, ui, m, tracks, interactive)
        }
        Screen::Folders => folder_list(c, ui, m, tracks, interactive),
        Screen::Queue => song_list(c, ui, m, tracks, interactive),
        _ => song_list(c, ui, m, tracks, interactive),
    }
}

/// Songs, artists and queue entries: a cover, two lines and a duration.
fn song_list(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    let accent = ui.accent();
    let count = ui.row_count(m, tracks);
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    let visible = visible_rows_for(m.screen);
    let scroll = components::list_window(m, focus, visible);
    components::status_bar(c, ui, m, &screen_title(m), true);
    let artists = m.screen == Screen::Artists;
    let queue = m.screen == Screen::Queue;
    // With UI motion one row more on each side slides in as the list scrolls.
    let (from, shown) = components::scroll_rows(ui, scroll, visible, count);
    let rows_start = c.draw.len();
    for (i, row) in ui.visible_rows(m, tracks, from, shown).iter().enumerate() {
        let index = from + i;
        let y = 38. + (index as f32 - scroll as f32) * 53.;
        let selected = interactive && index == focus;
        let queue_track = if queue { m.queue.get(index) } else { None };
        let song = row_track(row, tracks).map(|(_, t)| t).or(queue_track);
        let playing = if queue {
            index == m.queue_position
        } else {
            song.is_some_and(|t| is_current(m, t))
        };
        if selected {
            components::focus_row(c, 10., y + 1., WIDTH - 24., 50.);
        }
        let art_track = row.art.and_then(|i| tracks.get(i).map(|t| (i, t)));
        let queue_art = queue_track.and_then(|q| {
            tracks
                .iter()
                .position(|t| t.id == q.id && t.source_id == q.source_id)
                .map(|i| (i, &tracks[i]))
        });
        if artists {
            // Artists are round.
            cover(c, ui, art_track, 20., y + 7., 38.);
            components::round_cover(c, 20., y + 7., 38., selected);
        } else {
            cover(c, ui, art_track.or(queue_art), 20., y + 7., 38.);
        }
        let title_face = if selected { Face::S15 } else { Face::R15 };
        let title_color = if playing { accent } else { color::TEXT };
        let tail_w = song.map_or(0., |t| {
            text::width(Face::R11, &components::time(t.duration_ms)) + 16.
        });
        c.text_mid(
            70.,
            y + 18.,
            340. - tail_w,
            &row.label,
            title_face,
            title_color,
        );
        let sub = if playing && queue {
            format!("Playing  ·  {}", row.secondary)
        } else {
            row.secondary.clone()
        };
        let sub_color = if playing { accent } else { color::MUTED };
        c.text_mid(70., y + 36., 340. - tail_w, &sub, Face::R11, sub_color);
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
    c.mark_moving(Moving::List {
        start: rows_start,
        end: c.draw.len(),
        top: 38.,
        bottom: 38. + visible as f32 * 53.,
        row_h: 53.,
        first: scroll,
    });
    if count > visible {
        components::scroll_rail(c, 44., 286., scroll, count, visible, accent);
    }
    if m.screen == Screen::Tracks || m.screen == Screen::Artists {
        letter_bubble(c, ui, m, tracks, focus);
    }
}

/// The large letter shown while a long list is scrolling fast.
fn letter_bubble(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], focus: usize) {
    if ui
        .fast_scroll_until
        .is_none_or(|t| t <= std::time::Instant::now())
    {
        return;
    }
    let Some(row) = ui.row_at(m, tracks, focus) else {
        return;
    };
    let letter = crate::initial(&row.label, m.screen).to_string();
    let accent = ui.accent();
    c.rrect(206., 150., 68., 64., 18., 0x0E1014F5);
    c.rrect_outline(206., 150., 68., 64., 18., 1., alpha(accent, 170));
    c.text_center(
        240.,
        182. + Face::S36.mid_offset(),
        &letter,
        Face::S36,
        accent,
    );
}

// ---------------------------------------------------------------- album grid

fn albums_grid(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    components::status_bar(c, ui, m, &screen_title(m), true);
    let count = ui.row_count(m, tracks);
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    let focus_row = focus / 4;
    let first_row = (m.navigation.scroll / 4)
        .min(focus_row)
        .max((focus_row + 1).saturating_sub(2));
    let (size, gap) = (96., 14.);
    let x0 = ((WIDTH - (4. * size + 3. * gap)) / 2.).floor();
    let rows = ui.visible_rows(m, tracks, first_row * 4, 8);
    for (i, row) in rows.iter().enumerate() {
        let index = first_row * 4 + i;
        let (col, line) = ((i % 4) as f32, (i / 4) as f32);
        let x = x0 + col * (size + gap);
        let y = 46. + line * (size + gap + 8.);
        let art = row.art.and_then(|a| tracks.get(a).map(|t| (a, t)));
        if interactive && index == focus {
            cover_shadow(c, x - 7., y - 7., size + 14.);
            let at = c.draw.len();
            cover_rounded(c, ui, art, x - 7., y - 7., size + 14., 10.);
            c.rrect_outline(x - 8., y - 8., size + 16., size + 16., 11., 2., color::TEXT);
            c.mark_focus(at);
        } else {
            cover_rounded(c, ui, art, x, y, size, 8.);
            c.rrect(x, y, size, size, 8., 0x00000046);
        }
    }
    if let Some(row) = ui.row_at(m, tracks, focus) {
        c.text_center(
            240.,
            292. + Face::S20.mid_offset(),
            &text::fit(Face::S20, &row.label, 420.),
            Face::S20,
            color::TEXT,
        );
        c.text_center(
            240.,
            316. + Face::R13.mid_offset(),
            &text::fit(Face::R13, &row.secondary, 420.),
            Face::R13,
            color::MUTED,
        );
        let detail = ui.album_detail(m, tracks, &row.key);
        c.text_center(
            240.,
            338. + Face::R11.mid_offset(),
            &detail,
            Face::R11,
            color::DIM,
        );
    }
}

/// "9 songs · 38 min" for the album with filter `key`; an album shorter than
/// a minute shows its length as "0:42" rather than "0 min".
pub(crate) fn album_detail(m: &AppModel, tracks: &[Track], key: &str) -> String {
    let _ = m;
    let (songs, ms) = tracks
        .iter()
        .filter(|t| crate::track_matches(t, key))
        .fold((0, 0u64), |(n, ms), t| (n + 1, ms + t.duration_ms));
    let length = if ms < 60_000 {
        components::time(ms)
    } else {
        format!("{} min", ms / 60_000)
    };
    match songs {
        1 => format!("1 song · {length}"),
        n => format!("{n} songs · {length}"),
    }
}

// ------------------------------------------------------------ album / artist

pub(crate) fn pill_row(c: &mut Canvas, x: f32, y: f32, labels: &[(&str, bool)], interactive: bool) {
    let mut x = x;
    for (label, focused) in labels {
        let w = text::width(Face::S13, label) + 44.;
        components::button(c, x, y, w, 34., label, Face::S13, interactive && *focused);
        x += w + 12.;
    }
}

fn album_page(
    c: &mut Canvas,
    ui: &Ui,
    m: &AppModel,
    tracks: &[Track],
    has_art: bool,
    interactive: bool,
) {
    let accent = ui.accent();
    components::status_bar(c, ui, m, "Album", true);
    let count = ui.row_count(m, tracks);
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    let scroll = components::list_window(m, focus, visible_rows_for(Screen::Album));
    let rows_all = ui.visible_rows(m, tracks, scroll, 6);
    // Opening from Cover Flow: the centre cover flies into the header box.
    let opening = ui
        .transition_progress()
        .filter(|(kind, _)| *kind == crate::flow::TransitionKind::Open)
        .map(|(_, t)| t);
    let cover_at = c.draw.len();
    let mut rest_at = cover_at;
    if scroll == 0 {
        let head = ui.collection_track(m, tracks);
        let key = head.map(|t| {
            (
                tracks.iter().position(|x| std::ptr::eq(x, t)).unwrap_or(0),
                t,
            )
        });
        let matches_current = head
            .zip(m.current())
            .is_some_and(|(t, cur)| t.album == cur.album && t.album_artist == cur.album_artist);
        if has_art && matches_current {
            art_quad(c, 24., 44., 104., true);
        } else {
            cover_rounded(c, ui, key, 24., 44., 104., 9.);
        }
        rest_at = c.draw.len();
        if let Some(t) = head {
            c.text_mid(
                144.,
                72.,
                316.,
                &t.album_or_unknown(),
                Face::S25,
                color::TEXT,
            );
            let artist = if t.album_artist.is_empty() {
                &t.artist
            } else {
                &t.album_artist
            };
            c.text_mid(
                144.,
                98.,
                310.,
                crate::display_or_unknown(artist),
                Face::R15,
                color::TEXT,
            );
            if let Some(Item { secondary, .. }) = rows_all.first() {
                c.text_mid(144., 119., 310., secondary, Face::R11, color::MUTED);
            }
        }
        pill_row(
            c,
            144.,
            134.,
            &[("Play", focus == 0), ("Shuffle", focus == 1)],
            interactive,
        );
    }
    let (top, first, shown) = if scroll == 0 {
        (178., 2usize, 3usize)
    } else {
        (38., scroll, 6usize)
    };
    for (n, row) in ui.visible_rows(m, tracks, first, shown).iter().enumerate() {
        let index = first + n;
        let y = top + n as f32 * 53.;
        let song = row_track(row, tracks).map(|(_, t)| t);
        let selected = interactive && index == focus;
        if selected {
            components::focus_row(c, 10., y + 1., WIDTH - 20., 50.);
        }
        let number = song.map_or(index.saturating_sub(1), |t| {
            if t.track > 0 {
                t.track as usize
            } else {
                index - 1
            }
        });
        c.text_mid_center(
            34.,
            y + 26.,
            &number.to_string(),
            Face::R11,
            if selected { color::TEXT } else { color::DIM },
        );
        let playing = song.is_some_and(|t| is_current(m, t));
        let face = if selected { Face::S15 } else { Face::R15 };
        let tint = if playing { accent } else { color::TEXT };
        let tail = song.map_or(0., |t| {
            text::width(Face::R11, &components::time(t.duration_ms)) + 20.
        });
        c.text_mid(58., y + 26., 360. - tail, &row.label, face, tint);
        if let Some(t) = song {
            c.text_mid_right(
                WIDTH - 28.,
                y + 26.,
                &components::time(t.duration_ms),
                Face::R11,
                if selected { color::TEXT } else { color::DIM },
            );
        }
    }
    if count > 6 {
        components::scroll_rail(c, 178., 160., scroll, count, 6, accent);
    }
    if let Some(t) = opening {
        fly_in(c, cover_at, rest_at, t);
    }
}

/// The album page's first frames after Cover Flow: every quad of the header
/// cover is carried from the centre of Cover Flow into its box, and the rest
/// of the page rises and fades in.
fn fly_in(c: &mut Canvas, cover_at: usize, rest_at: usize, t: f32) {
    let (bx, by, bs) = crate::views_coverflow::PAGE_COVER;
    let from = crate::flow::cover_corners(0.);
    let at = |u: f32, v: f32| -> [f32; 2] {
        // The point (u, v) of the box, moved along the flight.
        let top = [
            from[0][0] + (from[1][0] - from[0][0]) * u,
            from[0][1] + (from[1][1] - from[0][1]) * u,
        ];
        let bottom = [
            from[3][0] + (from[2][0] - from[3][0]) * u,
            from[3][1] + (from[2][1] - from[3][1]) * u,
        ];
        let start = [
            top[0] + (bottom[0] - top[0]) * v,
            top[1] + (bottom[1] - top[1]) * v,
        ];
        let end = [bx + u * bs, by + v * bs];
        [
            start[0] + (end[0] - start[0]) * t,
            start[1] + (end[1] - start[1]) * t,
        ]
    };
    for q in &mut c.draw[cover_at..rest_at] {
        let (u0, v0) = ((q.x - bx) / bs, (q.y - by) / bs);
        let (u1, v1) = ((q.x + q.w - bx) / bs, (q.y + q.h - by) / bs);
        q.warp = Some(reborn_graphics::Warp {
            corners: [at(u0, v0), at(u1, v0), at(u1, v1), at(u0, v1)],
            colors: [q.color; 4],
        });
    }
    let rise = (1. - t) * 48.;
    for q in &mut c.draw[rest_at..] {
        q.y += rise;
        let a = ((q.color & 0xff) as f32 * t).round() as u32;
        q.color = q.color & !0xff | a;
    }
}

fn artist_page(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    components::status_bar(c, ui, m, "Artist", true);
    let count = ui.row_count(m, tracks);
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    let head = tracks
        .iter()
        .enumerate()
        .find(|(_, t)| crate::track_matches(t, &m.navigation.filter));
    let key = head;
    let head = head.map(|(_, t)| t);
    cover(c, ui, key, 24., 52., 64.);
    c.round_off(24., 52., 64., 32., color::BG);
    let all = ui.rows(m, tracks);
    let play = all.first();
    if let Some(t) = head {
        c.text_mid(
            108.,
            70.,
            350.,
            crate::display_or_unknown(&t.artist),
            Face::S25,
            color::TEXT,
        );
        let albums = all.len().saturating_sub(1);
        let sub = format!(
            "{} · {}",
            components::count(albums, "album", "albums"),
            play.map(|p| p.secondary.split(" · ").next().unwrap_or("").to_owned())
                .unwrap_or_default()
        );
        c.text_mid(108., 96., 350., &sub, Face::R13, color::MUTED);
    }
    pill_row(c, 108., 112., &[("Play All", focus == 0)], interactive);
    components::section_label(c, 28., 172., "Albums");
    // Three covers at a time, the focused one lifted.
    let albums = all.len().saturating_sub(1);
    if albums == 0 {
        return;
    }
    let album_focus = focus.saturating_sub(1);
    let first = album_focus.saturating_sub(2);
    for slot in 0..3usize.min(albums - first) {
        let index = first + slot;
        let Some(row) = all.get(1 + index) else {
            continue;
        };
        let selected = interactive && focus >= 1 && index == album_focus;
        let x = 24. + slot as f32 * 150.;
        let art = row.art.and_then(|a| tracks.get(a).map(|t| (a, t)));
        if selected {
            let at = c.draw.len();
            cover_shadow(c, x - 4., 184., 128.);
            cover_rounded(c, ui, art, x - 4., 184., 128., 11.);
            c.rrect_outline(x - 5., 183., 130., 130., 12., 2., color::TEXT);
            c.mark_focus(at);
        } else {
            cover_rounded(c, ui, art, x, 190., 116., 10.);
            c.rrect(x, 190., 116., 116., 10., 0x00000046);
        }
        c.text_center(
            x + 60.,
            328. + Face::S13.mid_offset(),
            &text::fit(Face::S13, &row.label, 130.),
            Face::S13,
            color::TEXT,
        );
        c.text_center(
            x + 60.,
            346. + Face::R11.mid_offset(),
            &text::fit(Face::R11, &row.secondary, 130.),
            Face::R11,
            color::DIM,
        );
    }
}

// ------------------------------------------------------------------- folders

fn folder_roots(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    components::status_bar(c, ui, m, "Folders", true);
    let focus = m.navigation.focus;
    for (i, row) in ui.visible_rows(m, tracks, 0, 4).iter().enumerate() {
        let y = 44. + i as f32 * 72.;
        let source = m
            .sources
            .iter()
            .find(|s| row.key.ends_with(&*s.root.to_string_lossy()));
        let internal = source.is_none_or(|s| matches!(s.kind, MediaSource::Internal));
        if interactive && i == focus {
            components::focus_row(c, 10., y, WIDTH - 20., 64.);
        } else {
            components::card(c, 10., y, WIDTH - 20., 64.);
        }
        let tint = if internal { 0x6EA0FFFF } else { 0x3CC878FF };
        c.icon(
            if internal { Icon::Folder } else { Icon::Sd },
            38.,
            y + 24.,
            26.,
            tint,
        );
        c.text_mid(66., y + 24., 300., &row.label, Face::S17, color::TEXT);
        // What a listener finds there (UI v4); free space is on Storage.
        let songs = source.map_or(0, |s| {
            tracks
                .iter()
                .filter(|t| t.online && t.source_id == s.id)
                .count()
        });
        let detail = if songs > 0 {
            components::count(songs, "song", "songs")
        } else {
            String::new()
        };
        c.text_mid(66., y + 44., 300., &detail, Face::R11, color::MUTED);
        components::chevron(c, WIDTH - 24., y + 32.);
    }
}

fn folder_list(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    let accent = ui.accent();
    components::status_bar(c, ui, m, &screen_title(m), true);
    let count = ui.row_count(m, tracks);
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    let visible = visible_rows_for(Screen::Folders);
    let scroll = components::list_window(m, focus, visible);
    let (from, shown) = components::scroll_rows(ui, scroll, visible, count);
    let rows_start = c.draw.len();
    for (i, row) in ui.visible_rows(m, tracks, from, shown).iter().enumerate() {
        let index = from + i;
        let y = 40. + (index as f32 - scroll as f32) * 54.;
        let song = row_track(row, tracks).map(|(_, t)| t);
        if interactive && index == focus {
            components::focus_row(c, 10., y, WIDTH - 24., 50.);
        }
        match song {
            Some(t) => {
                c.icon(Icon::Note, 36., y + 25., 22., color::MUTED);
                let playing = is_current(m, t);
                c.text_mid(
                    64.,
                    y + 25.,
                    310.,
                    &row.label,
                    Face::R15,
                    if playing { accent } else { color::TEXT },
                );
                c.text_mid_right(
                    WIDTH - 36.,
                    y + 25.,
                    &components::time(t.duration_ms),
                    Face::R11,
                    color::DIM,
                );
            }
            None => {
                c.icon(Icon::Folder, 36., y + 25., 24., 0x6EA0FFFF);
                c.text_mid(64., y + 25., 310., &row.label, Face::R15, color::TEXT);
                let songs = songs_under(tracks, &row.key);
                if songs > 0 {
                    let label = components::count(songs, "song", "songs");
                    c.text_mid_right(WIDTH - 36., y + 25., &label, Face::R11, color::DIM);
                }
            }
        }
    }
    c.mark_moving(Moving::List {
        start: rows_start,
        end: c.draw.len(),
        top: 40.,
        bottom: 40. + visible as f32 * 54.,
        row_h: 54.,
        first: scroll,
    });
    if count > visible {
        components::scroll_rail(c, 44., 250., scroll, count, visible, accent);
    }
}

fn songs_under(tracks: &[Track], key: &str) -> usize {
    let Some(path) = key.strip_prefix("folder:") else {
        return 0;
    };
    tracks
        .iter()
        .filter(|t| t.is_music() && t.path.parent().is_some_and(|p| p.starts_with(path)))
        .count()
}

// --------------------------------------------------------------- empty states

fn empty(c: &mut Canvas, ui: &Ui, m: &AppModel, interactive: bool) {
    let (title, body, action) = empty_copy(m);
    let mark = if m.library.scanning {
        components::Mark::Busy(ui.spin_phase())
    } else {
        components::Mark::Empty
    };
    let y = components::empty_state(c, 128., mark, title, body);
    if let Some(label) = action {
        components::empty_button(c, (y + 16.).max(292.), label, interactive);
    }
}

pub(crate) fn empty_copy(m: &AppModel) -> (&'static str, &'static str, Option<&'static str>) {
    if m.screen == Screen::Queue {
        return (
            "Nothing Up Next",
            "Choose an album, artist or song to start listening.",
            Some("Browse Music"),
        );
    }
    if m.library.scanning {
        return (
            "Finding Your Music…",
            "Your library appears here as music is found.",
            None,
        );
    }
    if !m.sources.iter().any(|s| s.online) {
        return (
            "Can't Read Your Music",
            "Restart the player. If an SD card was removed, reinsert it.",
            None,
        );
    }
    if m.library.error.is_some() {
        return (
            "Scan Didn't Finish",
            "Your existing music was kept. Scan again to finish.",
            Some("Scan for Music"),
        );
    }
    (
        "No Music Yet",
        "Connect Y2 to a computer and copy songs to Internal Storage or the SD card.",
        Some("Scan for Music"),
    )
}

/// Heading and explanation of an empty list, for product-surface tests.
#[cfg(test)]
pub(crate) fn empty_catalog_copy(m: &AppModel) -> String {
    let (title, body, _) = empty_copy(m);
    format!("{title} {body}")
}

trait AlbumName {
    fn album_or_unknown(&self) -> String;
}
impl AlbumName for Track {
    fn album_or_unknown(&self) -> String {
        crate::display_or_unknown(&self.album).to_owned()
    }
}
