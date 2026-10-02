//! Native 480×360 composition. One status bar, one title line, one content
//! area and one footer strip; every interactive screen shows one focus.
use crate::{
    components::{self, progress, time, Canvas},
    diagnostics,
    pages::{self, Page},
    theme::{color, type_scale},
    Item, Ui,
};
use reborn_core::{AppModel, PlaybackState, RepeatMode, Screen, Track};
use reborn_graphics::Quad;

const CONTENT_TOP: f32 = 76.;
const ROW_PITCH: f32 = 48.;
const FOOTER_TOP: f32 = 324.;

/// Rows visible in one window for a screen's list area.
pub fn visible_rows_for(screen: Screen) -> usize {
    if matches!(screen, Screen::Album | Screen::Artist) {
        3
    } else {
        5
    }
}

/// The wordmark with its accent rule, shown while an update is installed.
fn update_mark(c: &mut Canvas) {
    c.centered(
        240.,
        148.,
        "Reborn",
        crate::boot::MARK_SCALE,
        color::TEXT_PRIMARY,
    );
    c.rect(220., 198., 40., 2., color::ACCENT_GOLD);
}

pub fn draw(ui: &Ui, m: &AppModel, tracks: &[Track], has_art: bool) -> Vec<Quad> {
    let mut c = Canvas::new();
    c.rect(0., 0., 480., 360., color::BG);
    if matches!(
        m.platform.busy,
        Some(reborn_core::PlatformTask::UpdateApply | reborn_core::PlatformTask::UpdateRollback)
    ) {
        update_mark(&mut c);
        c.centered(
            240.,
            236.,
            "Preparing to install…",
            type_scale::BODY,
            color::TEXT_SECONDARY,
        );
        c.centered(
            240.,
            262.,
            "Keep the player charging",
            type_scale::SECONDARY,
            color::TEXT_MUTED,
        );
        return c.finish();
    }
    if m.screen_off {
        return c.finish();
    }
    components::status_bar(&mut c, ui, m);
    let interactive = m.navigation.modal.is_none() && ui.pairing.is_none() && !ui.text_entry;
    match m.screen {
        Screen::NowPlaying if m.current().is_some() => {
            now_playing(&mut c, ui, m, has_art, interactive)
        }
        Screen::ValueDetail => value_detail(&mut c, m, interactive),
        _ => list(&mut c, ui, m, tracks, has_art, interactive),
    }
    if let Some(pair) = &ui.pairing {
        components::dialog(
            &mut c,
            "Pair with this device?",
            pair,
            &[Item::new("Pair", "yes"), Item::new("Cancel", "no")],
            ui.pairing_focus,
        );
    } else if ui.text_entry {
        entry(&mut c, ui);
    } else if m.navigation.modal.is_some() {
        let (title, body) = ui.modal_copy(m);
        let rows = ui.modal_rows_public(m, tracks);
        components::dialog(
            &mut c,
            &title,
            &body,
            &rows,
            m.navigation.modal_focus.min(rows.len().saturating_sub(1)),
        );
    }
    if !ui.notice.is_empty()
        && ui
            .notice_until
            .is_some_and(|t| t > std::time::Instant::now())
    {
        components::toast(&mut c, &ui.notice);
    }
    c.finish()
}

fn screen_title(m: &AppModel) -> String {
    match m.screen {
        Screen::Home => "Reborn".into(),
        Screen::Music => "Music".into(),
        Screen::Albums => "Albums".into(),
        Screen::Artists => "Artists".into(),
        Screen::Tracks => "Songs".into(),
        Screen::Folders => {
            if m.navigation.filter.is_empty() {
                "Folders".into()
            } else {
                std::path::Path::new(m.navigation.filter.trim_start_matches("folder:"))
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Folders".into())
            }
        }
        Screen::Album => "Album".into(),
        Screen::Artist => "Artist".into(),
        Screen::LibraryIndex => "Jump to Letter".into(),
        Screen::NowPlaying => "Now Playing".into(),
        Screen::Queue => "Queue".into(),
        Screen::TrackInfo => "Song Info".into(),
        Screen::Settings => "Settings".into(),
        Screen::Wifi => "Wi-Fi".into(),
        Screen::Bluetooth => "Bluetooth".into(),
        Screen::PcTransfer => "PC Transfer".into(),
        Screen::SettingsAudio => "Audio".into(),
        Screen::Equalizer => "Equalizer".into(),
        Screen::SettingsPlayback => "Playback".into(),
        Screen::SettingsLibrary => "Library".into(),
        Screen::SettingsDisplay => "Display".into(),
        Screen::SettingsSystem => "System".into(),
        Screen::Battery => "Battery".into(),
        Screen::Sleep => "Sleep".into(),
        Screen::Storage => "Storage".into(),
        Screen::Update => "Software Update".into(),
        Screen::About => "About".into(),
        Screen::Maintenance => "Reset & Maintenance".into(),
        Screen::Diagnostics => "Diagnostics".into(),
        Screen::DiagnosticSection => diagnostics::title(&m.navigation.filter).into(),
        Screen::ValueDetail | Screen::TextEntry | Screen::Pairing => "Reborn".into(),
    }
}

/// Long lists show their position; short menus do not.
fn shows_counter(screen: Screen) -> bool {
    matches!(
        screen,
        Screen::Albums
            | Screen::Artists
            | Screen::Tracks
            | Screen::Folders
            | Screen::Album
            | Screen::Artist
            | Screen::Queue
            | Screen::DiagnosticSection
    )
}

fn catalog_like(screen: Screen) -> bool {
    matches!(
        screen,
        Screen::Albums
            | Screen::Artists
            | Screen::Tracks
            | Screen::Folders
            | Screen::Album
            | Screen::Artist
            | Screen::Queue
            | Screen::LibraryIndex
            | Screen::Diagnostics
            | Screen::DiagnosticSection
    )
}

fn list(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], has_art: bool, interactive: bool) {
    let page = if catalog_like(m.screen) {
        Page::default()
    } else {
        pages::page(ui, m, tracks)
    };
    let count = ui.row_count(m, tracks);
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    let detail = if count > 0 && shows_counter(m.screen) {
        format!("{} / {}", focus + 1, count)
    } else {
        String::new()
    };
    components::title(c, &screen_title(m), &detail);
    let mut top = CONTENT_TOP;
    let mut visible = visible_rows_for(m.screen);
    if matches!(m.screen, Screen::Album | Screen::Artist) && count > 0 {
        collection_header(c, ui, m, tracks, has_art, count);
        top = 170.;
    }
    if count == 0 && catalog_like(m.screen) {
        let (title, body, action) = empty_catalog(m);
        hero(c, title, body, 112.);
        if let Some(action) = action {
            components::row(c, &Item::new(action, "empty"), 232., interactive, "", false);
        }
    } else {
        if let Some(h) = &page.hero {
            let y = if page.rows.is_empty() && page.facts.is_empty() {
                128.
            } else {
                96.
            };
            top = hero(c, &h.title, &h.body, y) + 14.;
        }
        if !page.facts.is_empty() {
            top = facts(c, &page.facts, top + 4.) + 10.;
        }
        if page.hero.is_some() || !page.facts.is_empty() {
            visible = (((FOOTER_TOP - top) / ROW_PITCH).floor() as usize).max(1);
        }
        let scroll = m
            .navigation
            .scroll
            .min(focus)
            .max(focus.saturating_add(1).saturating_sub(visible));
        let rows = ui.visible_rows(m, tracks, scroll, visible);
        for (i, row) in rows.iter().enumerate() {
            let absolute = scroll + i;
            let t = row
                .key
                .strip_prefix("track:")
                .and_then(|s| s.parse::<usize>().ok())
                .and_then(|i| tracks.get(i));
            let queue_t = if m.screen == Screen::Queue {
                m.queue.get(absolute)
            } else {
                None
            };
            let tail = t
                .or(queue_t)
                .map(|t| time(t.duration_ms))
                .unwrap_or_default();
            let playing = if m.screen == Screen::Queue {
                absolute == m.queue_position
            } else {
                t.zip(m.current())
                    .is_some_and(|(a, b)| a.id == b.id && a.source_id == b.source_id)
            };
            components::row(
                c,
                row,
                top + i as f32 * ROW_PITCH,
                interactive && absolute == focus,
                &tail,
                playing,
            );
        }
        if count > visible {
            components::scroll_indicator(
                c,
                top,
                visible as f32 * ROW_PITCH,
                scroll,
                visible,
                count,
            );
        }
    }
    match &page.status {
        Some(status) => components::status_strip(c, status),
        None => components::footer(c, m, has_art),
    }
}

#[cfg(test)]
pub(crate) fn is_empty_catalog(ui: &Ui, m: &AppModel) -> bool {
    catalog_like(m.screen) && ui.row_count(m, &m.library.tracks) == 0
}

/// Heading and explanation of an empty list, for product-surface tests.
#[cfg(test)]
pub(crate) fn empty_catalog_copy(m: &AppModel) -> &'static str {
    let (title, body, _) = empty_catalog(m);
    Box::leak(format!("{title} {body}").into_boxed_str())
}

fn empty_catalog(m: &AppModel) -> (&'static str, &'static str, Option<&'static str>) {
    if m.screen == Screen::Queue {
        return (
            "Queue Is Empty",
            "Choose an album, artist or song from Music.",
            Some("Open Music"),
        );
    }
    if m.screen == Screen::Diagnostics || m.screen == Screen::DiagnosticSection {
        return ("Nothing Reported", "", None);
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
            "Library Scan Didn't Finish",
            "Your existing music was kept. Scan again to finish.",
            Some("Scan for Music"),
        );
    }
    (
        "No Music Yet",
        "Copy music to the player with a computer, then scan for music.",
        Some("Scan for Music"),
    )
}

/// Draw a hero message; returns the y just below it.
fn hero(c: &mut Canvas, title: &str, body: &str, y: f32) -> f32 {
    let lines = wrap_lines(title, 432., 20., 2);
    let mut cursor = y;
    for line in &lines {
        c.text_box(
            24.,
            cursor,
            432.,
            line,
            type_scale::SCREEN_TITLE,
            color::TEXT_PRIMARY,
        );
        cursor += 28.;
    }
    if !body.is_empty() {
        cursor += 6.;
        for line in wrap_lines(body, 432., 14., 3) {
            c.text_box(
                24.,
                cursor,
                432.,
                &line,
                type_scale::BODY,
                color::TEXT_SECONDARY,
            );
            cursor += 21.;
        }
    }
    cursor
}

/// Two-column label/value facts; returns the y just below them.
fn facts(c: &mut Canvas, facts: &[(String, String)], top: f32) -> f32 {
    let mut y = top;
    for (label, value) in facts {
        c.text_box(
            28.,
            y + 7.,
            150.,
            label,
            type_scale::BODY,
            color::TEXT_SECONDARY,
        );
        let value = components::fit_pixels(value, 270., 14.);
        let width = components::text_width(&value, 14.);
        c.text(
            452. - width,
            y + 7.,
            &value,
            type_scale::BODY,
            color::TEXT_PRIMARY,
        );
        y += 34.;
        if y > FOOTER_TOP - 34. {
            break;
        }
    }
    y
}

fn collection_header(
    c: &mut Canvas,
    ui: &Ui,
    m: &AppModel,
    tracks: &[Track],
    has_art: bool,
    count: usize,
) {
    let t = ui.collection_track(m, tracks);
    let art_matches = t.zip(m.current()).is_some_and(|(t, current)| {
        t.album == current.album && t.album_artist == current.album_artist
    });
    let collection_art = t.is_some_and(|t| {
        ui.collection_art
            .as_ref()
            .is_some_and(|(source, id)| source == &t.source_id && *id == t.id)
    });
    c.artwork(16., 77., 82., collection_art || (has_art && art_matches));
    if collection_art {
        if let Some(quad) = c.draw.last_mut() {
            quad.collection_artwork = true;
        }
    }
    let Some(t) = t else {
        return;
    };
    let songs = count.saturating_sub(if m.screen == Screen::Artist { 2 } else { 1 });
    let (name, sub) = if m.screen == Screen::Artist {
        (
            crate::display_or_unknown(&t.artist).to_owned(),
            format!("{songs} songs"),
        )
    } else {
        (
            crate::display_or_unknown(&t.album).to_owned(),
            crate::display_or_unknown(if t.album_artist.is_empty() {
                &t.artist
            } else {
                &t.album_artist
            })
            .to_owned(),
        )
    };
    let lines = wrap_lines(&name, 350., 18., 2);
    let mut y = 84.;
    for line in &lines {
        c.text_box(
            114.,
            y,
            350.,
            line,
            type_scale::SECTION,
            color::TEXT_PRIMARY,
        );
        y += 25.;
    }
    c.text_box(
        114.,
        y + 4.,
        350.,
        &sub,
        type_scale::BODY,
        color::TEXT_SECONDARY,
    );
}

fn now_playing(c: &mut Canvas, ui: &Ui, m: &AppModel, has_art: bool, interactive: bool) {
    let Some(t) = m.current() else {
        return;
    };
    c.artwork(16., 46., 176., has_art);
    let title_lines = wrap_lines(crate::track_title(t), 260., 22., 3);
    let mut y = 50.;
    for line in &title_lines {
        c.text_box(
            206.,
            y,
            260.,
            line,
            type_scale::HERO - 0.5,
            color::TEXT_PRIMARY,
        );
        y += 29.;
    }
    y += 6.;
    c.text_box(
        206.,
        y,
        260.,
        crate::display_or_unknown(&t.artist),
        type_scale::SECTION,
        color::TEXT_PRIMARY,
    );
    c.text_box(
        206.,
        y + 26.,
        260.,
        crate::display_or_unknown(&t.album),
        type_scale::BODY,
        color::TEXT_SECONDARY,
    );
    let source_offline =
        !m.sources.is_empty() && !m.sources.iter().any(|s| s.id == t.source_id && s.online);
    let (icon, state) = if source_offline {
        ("pause", "SD card removed")
    } else {
        match m.playback {
            PlaybackState::Playing => ("play", "Playing"),
            PlaybackState::Paused => ("pause", "Paused"),
            PlaybackState::Buffering => ("play", "Starting…"),
            PlaybackState::Error => ("pause", "No audio output"),
            PlaybackState::Stopped => ("pause", "Stopped"),
        }
    };
    c.icon(icon, 206., 200., 16., color::TEXT_SECONDARY);
    c.text(228., 201., state, type_scale::BODY, color::TEXT_SECONDARY);
    if source_offline || m.playback == PlaybackState::Error {
        c.text(
            206.,
            222.,
            if source_offline {
                "Reinsert the card to keep listening"
            } else {
                "Check headphones, then press Play"
            },
            type_scale::SECONDARY,
            color::TEXT_MUTED,
        );
    }
    c.progress(16., 244., 448., progress(m.position_ms, t.duration_ms));
    c.text(
        16.,
        258.,
        &time(m.position_ms),
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
    let remaining = format!("-{}", time(t.duration_ms.saturating_sub(m.position_ms)));
    let width = components::text_width(&remaining, 12.);
    c.text(
        464. - width,
        258.,
        &remaining,
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
    // The wheel adjusts volume here; the volume bar is the wheel's target.
    c.icon("volume", 16., 289., 18., color::TEXT_SECONDARY);
    c.rect(44., 297., 372., 3., color::TRACK);
    let i = c.draw.len();
    c.rect(
        44.,
        297.,
        372. * f32::from(m.settings.volume.min(100)) / 100.,
        3.,
        color::ACCENT_GOLD,
    );
    if interactive {
        if let Some(q) = c.draw.get_mut(i) {
            q.focus_target = true;
        }
    }
    let volume = m.settings.volume.to_string();
    c.text(
        464. - components::text_width(&volume, 14.),
        290.,
        &volume,
        type_scale::BODY,
        color::TEXT_PRIMARY,
    );
    c.rect(0., FOOTER_TOP, 480., 36., color::BG_RAISED);
    c.icon(
        if m.output == reborn_core::AudioOutput::Wired {
            "headphones"
        } else {
            "bluetooth"
        },
        16.,
        333.,
        18.,
        color::TEXT_SECONDARY,
    );
    c.text_box(
        42.,
        335.,
        250.,
        &pages::output_name(ui, m),
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
    c.icon(
        "shuffle",
        410.,
        333.,
        18.,
        if m.settings.shuffle {
            color::ACCENT_GOLD
        } else {
            color::TEXT_MUTED
        },
    );
    c.icon(
        "repeat",
        440.,
        333.,
        18.,
        if m.settings.repeat == RepeatMode::Off {
            color::TEXT_MUTED
        } else {
            color::ACCENT_GOLD
        },
    );
    if m.settings.repeat == RepeatMode::Track {
        c.text(456., 344., "1", type_scale::SECONDARY, color::ACCENT_GOLD);
    }
}

/// Split `text` into at most `lines` lines that fit `width` at `size` px.
pub(crate) fn wrap_lines(text: &str, width: f32, size: f32, lines: usize) -> Vec<String> {
    let mut out = vec![];
    let mut remaining = text.trim();
    while !remaining.is_empty() && out.len() < lines {
        if out.len() == lines - 1 || components::text_width(remaining, size) <= width {
            out.push(components::fit_pixels(remaining, width, size));
            break;
        }
        let mut split = remaining.len();
        let mut last_space = 0;
        let mut used = 0.;
        for (i, ch) in remaining.char_indices() {
            used += components::text_width(&ch.to_string(), size);
            if ch == ' ' {
                last_space = i;
            }
            if used > width {
                split = if last_space > 0 { last_space } else { i };
                break;
            }
        }
        if split == 0 {
            break;
        }
        out.push(remaining[..split].trim_end().to_owned());
        remaining = remaining[split..].trim_start();
    }
    out
}

fn value_detail(c: &mut Canvas, m: &AppModel, interactive: bool) {
    let (title, value) = m
        .navigation
        .filter
        .split_once('\u{1f}')
        .unwrap_or(("Detail", &m.navigation.filter));
    components::title(c, title, "");
    let mut y = 88.;
    for line in wrap_lines(value, 432., 15., 8) {
        c.text_box(24., y, 432., &line, type_scale::ROW, color::TEXT_PRIMARY);
        y += 22.;
    }
    components::row(c, &Item::new("Back", "back"), 270., interactive, "", false);
    components::footer(c, m, false);
}

fn entry(c: &mut Canvas, ui: &Ui) {
    c.rect(0., 28., 480., 332., color::BG);
    components::title(c, "Wi-Fi Password", "");
    c.text_box(
        16.,
        80.,
        448.,
        &ui.ssid,
        type_scale::BODY,
        color::TEXT_SECONDARY,
    );
    let dots = "•".repeat(ui.password.len().min(32));
    c.text_box(
        16.,
        110.,
        448.,
        if dots.is_empty() {
            "No characters yet"
        } else {
            &dots
        },
        type_scale::BODY,
        if dots.is_empty() {
            color::TEXT_MUTED
        } else {
            color::TEXT_PRIMARY
        },
    );
    let focus = ui.letter;
    let label = if focus < crate::LETTERS.len() {
        if crate::LETTERS[focus] == b' ' {
            "Space".into()
        } else {
            (crate::LETTERS[focus] as char).to_string()
        }
    } else {
        ["Delete", "Connect", "Cancel"][focus - crate::LETTERS.len()].into()
    };
    c.focus_panel(150., 154., 180., 70., true);
    c.centered(
        240.,
        177.,
        &label,
        type_scale::SCREEN_TITLE,
        color::TEXT_PRIMARY,
    );
    c.text(
        16.,
        262.,
        "Turn to choose · Select adds · Hold Select connects",
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
    c.rect(0., FOOTER_TOP, 480., 36., color::BG_RAISED);
    c.text(
        16.,
        336.,
        "Back cancels",
        type_scale::SECONDARY,
        color::TEXT_MUTED,
    );
}
