//! Composition of the Reborn screens. Screens only read application state and
//! emit `Quad`s through shared components; they never call platform services.

use crate::{components, theme, timeout_label, Item, PowerView, RadioView, Ui};
use components::{fit, progress, time, Canvas};
use reborn_core::{AppModel, MediaSource, Screen, Track};
use reborn_graphics::Quad;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewScreen {
    NowPlaying,
    Library,
    Artist,
    Queue,
    Settings,
    QuickSettings,
    Lock,
    Boot,
}

pub fn draw(
    ui: &Ui,
    m: &AppModel,
    tracks: &[Track],
    health: &str,
    has_art: bool,
    power: PowerView,
) -> Vec<Quad> {
    let mut c = Canvas::new();
    components::screen_background(
        &mut c,
        has_art,
        matches!(m.screen, Screen::Queue | Screen::Artist),
    );
    components::status_bar(&mut c, m, power, section(m.screen), &ui.wifi, &ui.bluetooth);
    if let Some(pair) = &ui.pairing {
        draw_pairing(&mut c, pair);
        return finish(ui, m, c);
    }
    if ui.text_entry {
        draw_text_entry(&mut c, &ui.notice);
        return finish(ui, m, c);
    }
    match m.screen {
        Screen::NowPlaying => draw_now_playing(&mut c, m, has_art),
        Screen::Queue => draw_queue(&mut c, ui, m, has_art),
        Screen::Settings
        | Screen::SettingsAudio
        | Screen::SettingsPlayback
        | Screen::SettingsLibrary
        | Screen::SettingsBluetooth
        | Screen::SettingsWifi
        | Screen::SettingsDisplay
        | Screen::SettingsPower
        | Screen::SettingsSystem => draw_settings(&mut c, ui, m),
        Screen::Connectivity | Screen::Bluetooth | Screen::Wifi => draw_connectivity(&mut c, ui, m),
        Screen::Album => draw_album(&mut c, ui, m, tracks, has_art),
        Screen::Artist => draw_artist(&mut c, ui, m, tracks, has_art),
        Screen::Albums | Screen::Music | Screen::Artists | Screen::Tracks | Screen::Folders => {
            draw_library(&mut c, ui, m, tracks, has_art)
        }
        Screen::Diagnostics => {
            if m.navigation.filter == "audio" {
                draw_audio_information(&mut c, m);
            } else {
                draw_diagnostics(&mut c, m, health);
            }
        }
        Screen::Home => draw_home(&mut c, ui, m, has_art),
        Screen::TextEntry | Screen::Pairing => {}
    }
    if m.navigation.modal.is_some() {
        c.clear_focus_marks();
        draw_modal(&mut c, ui, m, tracks);
    }
    if shows_mini_player(m.screen) {
        components::bottom_info(&mut c, m, has_art);
    }
    if !ui.notice.is_empty()
        && ui
            .notice_until
            .is_some_and(|until| until > std::time::Instant::now())
    {
        if let Some(value) = ui
            .notice
            .strip_prefix("Volume ")
            .and_then(|value| value.parse::<u8>().ok())
        {
            components::volume_overlay(&mut c, value);
        } else {
            components::toast(&mut c, &ui.notice);
        }
    }
    finish(ui, m, c)
}

pub fn preview(
    ui: &Ui,
    mut m: AppModel,
    tracks: &[Track],
    power: PowerView,
    screen: PreviewScreen,
) -> Vec<Quad> {
    match screen {
        PreviewScreen::NowPlaying => m.screen = Screen::NowPlaying,
        PreviewScreen::Library => m.screen = Screen::Albums,
        PreviewScreen::Artist => m.screen = Screen::Artist,
        PreviewScreen::Queue => m.screen = Screen::Queue,
        PreviewScreen::Settings => m.screen = Screen::SettingsAudio,
        PreviewScreen::QuickSettings => m.screen = Screen::Connectivity,
        PreviewScreen::Lock => return draw_lock(&m, power),
        PreviewScreen::Boot => return draw_boot(),
    }
    draw(ui, &m, tracks, "ok", true, power)
}

fn finish(ui: &Ui, m: &AppModel, c: Canvas) -> Vec<Quad> {
    if m.navigation.modal.is_some() {
        // Dialog composition is intentionally last so it prevents activation
        // behind it and leaves exactly one modal focus target visible.
    }
    if ui.notice.is_empty() {
        // Keep the branch explicit: no continuous animation or invalidation is
        // introduced for static screens.
    }
    c.finish()
}

fn shows_mini_player(screen: Screen) -> bool {
    matches!(
        screen,
        Screen::Albums
            | Screen::Music
            | Screen::Artists
            | Screen::Tracks
            | Screen::Folders
            | Screen::Album
            | Screen::Artist
            | Screen::Settings
            | Screen::SettingsAudio
            | Screen::SettingsPlayback
            | Screen::SettingsLibrary
            | Screen::SettingsBluetooth
            | Screen::SettingsWifi
            | Screen::SettingsDisplay
            | Screen::SettingsPower
            | Screen::SettingsSystem
            | Screen::Connectivity
            | Screen::Bluetooth
            | Screen::Wifi
            | Screen::Diagnostics
    )
}

fn draw_home(c: &mut Canvas, ui: &Ui, m: &AppModel, has_art: bool) {
    c.display(
        theme::space::LG,
        48.0,
        "Listen deeper",
        theme::type_scale::SCREEN_TITLE,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        theme::space::LG,
        81.0,
        "Your music, close at hand",
        theme::type_scale::SECONDARY,
        theme::color::TEXT_SECONDARY,
    );
    if let Some(track) = m.current() {
        c.panel(
            16.0,
            116.0,
            448.0,
            116.0,
            theme::color::BG_RAISED,
            theme::color::SURFACE_BORDER,
        );
        c.artwork(28.0, 128.0, 92.0, has_art);
        c.micro(140.0, 128.0, "NOW PLAYING", theme::color::ACCENT_GOLD);
        c.display(
            140.0,
            145.0,
            &fit(&track.title, 26),
            theme::type_scale::SECTION,
            theme::color::TEXT_PRIMARY,
        );
        c.text(
            140.0,
            175.0,
            &fit(&track.artist, 30),
            theme::type_scale::BODY,
            theme::color::TEXT_SECONDARY,
        );
        c.text(
            140.0,
            195.0,
            &fit(&track.album, 30),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_MUTED,
        );
        c.progress(
            140.0,
            215.0,
            270.0,
            components::progress(m.position_ms, track.duration_ms),
        );
    } else {
        components::empty_state(
            c,
            16.0,
            116.0,
            448.0,
            "Nothing is playing",
            "Choose an album or track to begin listening.",
        );
    }
    c.micro(16.0, 239.0, "REACH", theme::color::TEXT_MUTED);
    let rows = ui.rows(m, &m.library.tracks);
    for (position, row) in visible_rows(&rows, m.navigation.focus, 4) {
        let index = position + list_start(&rows, m.navigation.focus, 4);
        let x = 16.0 + (position % 2) as f32 * 226.0;
        let y = 250.0 + (position / 2) as f32 * theme::layout::ROW_H;
        components::list_row(
            c,
            row,
            x,
            y,
            214.0,
            index == m.navigation.focus,
            components::row_icon(&row.key),
        );
    }
}

fn draw_library(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], has_art: bool) {
    library_rail(c, m);
    let x = theme::layout::CONTENT_X;
    let rows = ui.rows(m, tracks);
    let title = match m.screen {
        Screen::Music => "Music",
        Screen::Albums => "Albums",
        Screen::Artists => "Artists",
        Screen::Tracks => "Songs",
        Screen::Folders => "Folders",
        _ => "Library",
    };
    c.display(
        x,
        43.0,
        title,
        theme::type_scale::SCREEN_TITLE,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        432.0,
        48.0,
        &format!("{}", rows.len()),
        theme::type_scale::MICRO,
        theme::color::TEXT_MUTED,
    );

    if rows.is_empty() {
        components::empty_state(
            c,
            x,
            78.0,
            308.0,
            "No music found",
            "Insert an SD card or scan your library.",
        );
        return;
    }
    if m.screen == Screen::Albums {
        // Two clear cards per row are easier to identify than six tiny tiles.
        let columns = 2;
        let tile_w = 148.0;
        let start = m
            .navigation
            .focus
            .saturating_sub(1)
            .min(rows.len().saturating_sub(4));
        for (position, row) in rows.iter().enumerate().skip(start).take(4) {
            let tile = position - start;
            let tx = x + (tile % columns) as f32 * 160.0;
            let ty = 76.0 + (tile / columns) as f32 * 114.0;
            let focused = position == m.navigation.focus;
            c.focus_panel(tx, ty, tile_w, 108.0, focused);
            c.artwork(tx + 6.0, ty + 5.0, 72.0, has_art);
            c.text(
                tx + 6.0,
                ty + 78.0,
                &fit(&row.label, 19),
                theme::type_scale::BODY,
                if focused {
                    theme::color::TEXT_PRIMARY
                } else {
                    theme::color::TEXT_SECONDARY
                },
            );
            let artist = tracks
                .iter()
                .find(|track| track.album == row.label)
                .map(|track| track.artist.as_str())
                .unwrap_or("Unknown artist");
            c.text(
                tx + 6.0,
                ty + 94.0,
                &fit(artist, 20),
                theme::type_scale::SECONDARY,
                if focused {
                    theme::color::ACCENT_GOLD
                } else {
                    theme::color::TEXT_MUTED
                },
            );
        }
        return;
    }
    for (position, row) in visible_rows(&rows, m.navigation.focus, theme::layout::LIST_VISIBLE) {
        components::list_row(
            c,
            row,
            x,
            76.0 + position as f32 * theme::layout::ROW_H,
            308.0,
            position + list_start(&rows, m.navigation.focus, theme::layout::LIST_VISIBLE)
                == m.navigation.focus,
            components::row_icon(&row.key),
        );
    }
}

fn library_rail(c: &mut Canvas, m: &AppModel) {
    c.panel(
        8.0,
        42.0,
        136.0,
        256.0,
        theme::color::SURFACE,
        theme::color::SURFACE_BORDER,
    );
    c.micro(22.0, 53.0, "MUSIC", theme::color::TEXT_MUTED);
    let entries = [
        ("Albums", "albums"),
        ("Artists", "artist"),
        ("Songs", "songs"),
        ("Folders", "storage"),
    ];
    for (index, (label, icon)) in entries.iter().enumerate() {
        let y = 68.0 + index as f32 * 43.0;
        let active = matches!(
            (m.screen, index),
            (Screen::Albums, 0)
                | (Screen::Artists, 1)
                | (Screen::Tracks, 2)
                | (Screen::Folders, 3)
                | (Screen::Music, 0)
        );
        c.active_panel(14.0, y, 124.0, 38.0, active);
        c.icon(
            icon,
            24.0,
            y + 8.0,
            22.0,
            if active {
                theme::color::ACCENT_GOLD
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
        c.text(
            54.0,
            y + 9.0,
            label,
            theme::type_scale::SECONDARY,
            if active {
                theme::color::TEXT_PRIMARY
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
    }
    c.rect(22.0, 248.0, 108.0, 1.0, theme::color::SURFACE_BORDER);
    c.icon("storage", 24.0, 254.0, 22.0, theme::color::TEXT_SECONDARY);
    let internal_online = m
        .sources
        .iter()
        .any(|source| matches!(source.kind, MediaSource::Internal) && source.online);
    c.text(
        54.0,
        254.0,
        "Y2DATA",
        theme::type_scale::SECONDARY,
        theme::color::TEXT_SECONDARY,
    );
    c.text(
        54.0,
        271.0,
        if internal_online {
            "Available"
        } else {
            "Unavailable"
        },
        theme::type_scale::DECORATIVE,
        theme::color::TEXT_MUTED,
    );
    c.icon("sd_card", 24.0, 278.0, 22.0, theme::color::TEXT_SECONDARY);
    c.text(
        54.0,
        280.0,
        "SD Card",
        theme::type_scale::SECONDARY,
        theme::color::TEXT_SECONDARY,
    );
}

fn draw_now_playing(c: &mut Canvas, m: &AppModel, has_art: bool) {
    let Some(track) = m.current() else {
        components::empty_state(
            c,
            16.0,
            62.0,
            448.0,
            "Nothing is playing",
            "Choose music to begin listening.",
        );
        return;
    };
    c.artwork(16.0, 44.0, theme::layout::ART_NOW, has_art);
    c.display(
        202.0,
        45.0,
        &fit(&track.title, 17),
        theme::type_scale::HERO,
        theme::color::TEXT_PRIMARY,
    );
    c.display(
        202.0,
        82.0,
        &fit(&track.artist, 20),
        theme::type_scale::SECTION,
        theme::color::TEXT_SECONDARY,
    );
    c.text(
        202.0,
        109.0,
        &fit(&track.album, 20),
        theme::type_scale::BODY,
        theme::color::TEXT_SECONDARY,
    );
    let technical = format!(
        "{} · {}",
        track.codec.to_uppercase(),
        technical_format(track)
    );
    c.text(
        202.0,
        137.0,
        &fit(&technical, 24),
        theme::type_scale::SECONDARY,
        theme::color::ACCENT_GOLD,
    );
    c.progress(
        202.0,
        177.0,
        262.0,
        progress(m.position_ms, track.duration_ms),
    );
    c.text(
        202.0,
        189.0,
        &time(m.position_ms),
        theme::type_scale::SECONDARY,
        theme::color::TEXT_SECONDARY,
    );
    c.text(
        421.0,
        189.0,
        &format!("-{}", time(track.duration_ms.saturating_sub(m.position_ms))),
        theme::type_scale::SECONDARY,
        theme::color::TEXT_MUTED,
    );
    components::now_playing_controls(c, m);
    // The wheel changes volume on this screen, so volume is the one visible
    // focus target even though the playback glyphs remain state indicators.
    c.focus_panel(16.0, 306.0, 164.0, 48.0, true);
    c.icon("volume", 28.0, 319.0, 22.0, theme::color::ACCENT_GOLD);
    c.text(
        60.0,
        311.0,
        &format!("{}", m.settings.volume),
        theme::type_scale::ROW,
        theme::color::TEXT_PRIMARY,
    );
    c.progress(60.0, 335.0, 98.0, m.settings.volume as f32 / 100.0);
    c.panel(
        196.0,
        306.0,
        132.0,
        48.0,
        theme::color::SURFACE,
        theme::color::SURFACE_BORDER,
    );
    c.icon("headphones", 210.0, 319.0, 22.0, theme::color::TEXT_PRIMARY);
    c.text(
        244.0,
        311.0,
        &components::output_label(&m.output),
        theme::type_scale::SECONDARY,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        244.0,
        331.0,
        "Output",
        theme::type_scale::DECORATIVE,
        theme::color::TEXT_MUTED,
    );
    c.panel(
        344.0,
        306.0,
        120.0,
        48.0,
        theme::color::SURFACE,
        theme::color::SURFACE_BORDER,
    );
    c.icon("gain", 358.0, 319.0, 22.0, theme::color::TEXT_PRIMARY);
    c.text(
        390.0,
        311.0,
        "High",
        theme::type_scale::SECONDARY,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        390.0,
        331.0,
        "Gain",
        theme::type_scale::DECORATIVE,
        theme::color::TEXT_MUTED,
    );
}

fn draw_artist(c: &mut Canvas, _ui: &Ui, m: &AppModel, tracks: &[Track], has_art: bool) {
    let name = m
        .navigation
        .filter
        .strip_prefix("artist:")
        .unwrap_or("Artist");
    let artist_tracks = tracks
        .iter()
        .filter(|track| track_matches(track, &m.navigation.filter))
        .collect::<Vec<_>>();
    c.panel(
        16.0,
        42.0,
        448.0,
        104.0,
        theme::color::BG_RAISED,
        theme::color::SURFACE_BORDER,
    );
    c.artwork(28.0, 54.0, 76.0, has_art);
    c.display(
        120.0,
        56.0,
        &fit(name, 21),
        theme::type_scale::HERO,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        120.0,
        94.0,
        &format!(
            "{} tracks · {} albums",
            artist_tracks.len(),
            album_count(&artist_tracks)
        ),
        theme::type_scale::BODY,
        theme::color::TEXT_SECONDARY,
    );
    c.focus_panel(338.0, 102.0, 110.0, 36.0, m.navigation.focus == 0);
    c.text(
        352.0,
        112.0,
        "Play Artist",
        theme::type_scale::BODY,
        if m.navigation.focus == 0 {
            theme::color::TEXT_PRIMARY
        } else {
            theme::color::TEXT_SECONDARY
        },
    );

    c.display(
        16.0,
        160.0,
        "Top Tracks",
        theme::type_scale::SECTION,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        284.0,
        165.0,
        "Albums",
        theme::type_scale::SECTION,
        theme::color::TEXT_PRIMARY,
    );
    for (index, track) in artist_tracks.iter().take(2).enumerate() {
        let y = 190.0 + index as f32 * 54.0;
        let focused = index + 1 == m.navigation.focus;
        c.focus_panel(16.0, y, 252.0, 50.0, focused);
        c.text(
            28.0,
            y + 16.0,
            &format!("{}", index + 1),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_MUTED,
        );
        c.text(
            58.0,
            y + 10.0,
            &fit(&track.title, 21),
            theme::type_scale::ROW,
            if focused {
                theme::color::TEXT_PRIMARY
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
        c.text(
            58.0,
            y + 31.0,
            &time(track.duration_ms),
            theme::type_scale::SECONDARY,
            if focused {
                theme::color::ACCENT_GOLD
            } else {
                theme::color::TEXT_MUTED
            },
        );
    }
    let mut artist_albums = Vec::new();
    for track in &artist_tracks {
        let identity = (track.album_artist.clone(), track.album.clone());
        if !artist_albums.iter().any(|album| album == &identity) {
            artist_albums.push(identity);
        }
    }
    for (index, (album_artist, album)) in artist_albums.iter().take(2).enumerate() {
        let x = 284.0 + index as f32 * 90.0;
        c.artwork(x, 190.0, 78.0, has_art);
        c.text(
            x,
            276.0,
            &fit(album, 12),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_SECONDARY,
        );
        c.text(
            x,
            292.0,
            &format!(
                "{} tracks",
                artist_tracks
                    .iter()
                    .filter(|track| {
                        track.album_artist == *album_artist && track.album == *album
                    })
                    .count()
            ),
            theme::type_scale::DECORATIVE,
            theme::color::TEXT_MUTED,
        );
    }
}

fn draw_album(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], has_art: bool) {
    let name = m
        .navigation
        .filter
        .strip_prefix("album:")
        .map(crate::album_filter_label)
        .unwrap_or("Album");
    c.artwork(16.0, 44.0, 104.0, has_art);
    c.display(
        136.0,
        50.0,
        &fit(name, 28),
        theme::type_scale::HERO,
        theme::color::TEXT_PRIMARY,
    );
    let artist = tracks
        .iter()
        .find(|track| track_matches(track, &m.navigation.filter))
        .map(|track| fit(&track.artist, 28))
        .unwrap_or_else(|| "Unknown artist".into());
    c.text(
        136.0,
        88.0,
        &artist,
        theme::type_scale::BODY,
        theme::color::TEXT_SECONDARY,
    );
    c.text(
        136.0,
        112.0,
        &format!(
            "{} tracks",
            tracks
                .iter()
                .filter(|track| track_matches(track, &m.navigation.filter))
                .count()
        ),
        theme::type_scale::SECONDARY,
        theme::color::TEXT_MUTED,
    );
    c.focus_panel(350.0, 105.0, 98.0, 30.0, m.navigation.focus == 0);
    c.text(
        366.0,
        114.0,
        "Play Album",
        theme::type_scale::MICRO,
        if m.navigation.focus == 0 {
            theme::color::TEXT_PRIMARY
        } else {
            theme::color::TEXT_SECONDARY
        },
    );
    let rows = ui.rows(m, tracks);
    for (position, (index, row)) in rows.iter().enumerate().skip(4).take(2).enumerate() {
        components::list_row(
            c,
            row,
            16.0,
            154.0 + position as f32 * theme::layout::ROW_H,
            448.0,
            index == m.navigation.focus,
            "songs",
        );
    }
}

fn draw_queue(c: &mut Canvas, ui: &Ui, m: &AppModel, has_art: bool) {
    c.panel(
        12.0,
        42.0,
        456.0,
        62.0,
        theme::color::BG_RAISED,
        theme::color::SURFACE_BORDER,
    );
    c.rect(12.0, 48.0, 3.0, 50.0, theme::color::ACCENT_GOLD);
    if let Some(track) = m.current() {
        c.artwork(24.0, 52.0, 42.0, has_art);
        c.micro(82.0, 50.0, "NOW PLAYING", theme::color::ACCENT_GOLD);
        c.display(
            82.0,
            64.0,
            &fit(&track.title, 26),
            theme::type_scale::SECTION,
            theme::color::TEXT_PRIMARY,
        );
        c.text(
            82.0,
            88.0,
            &fit(&track.artist, 24),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_SECONDARY,
        );
        c.text(
            368.0,
            53.0,
            &fit(&technical_format(track), 14),
            theme::type_scale::SECONDARY,
            theme::color::ACCENT_GOLD,
        );
        c.text(
            402.0,
            84.0,
            &format!("-{}", time(track.duration_ms.saturating_sub(m.position_ms))),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_MUTED,
        );
    }
    c.display(
        14.0,
        116.0,
        "Up Next",
        theme::type_scale::SECTION,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        96.0,
        122.0,
        &format!(
            "{} tracks remaining",
            m.queue.len().saturating_sub(m.queue_position + 1)
        ),
        theme::type_scale::SECONDARY,
        theme::color::TEXT_MUTED,
    );
    c.icon(
        "shuffle",
        350.0,
        114.0,
        22.0,
        if m.settings.shuffle {
            theme::color::ACCENT_GOLD
        } else {
            theme::color::TEXT_MUTED
        },
    );
    c.icon(
        "repeat",
        388.0,
        114.0,
        22.0,
        if !matches!(m.settings.repeat, reborn_core::RepeatMode::Off) {
            theme::color::ACCENT_GOLD
        } else {
            theme::color::TEXT_MUTED
        },
    );
    c.micro(426.0, 121.0, "MENU", theme::color::TEXT_MUTED);
    let rows = ui.rows(m, &[]);
    for (position, row) in rows.iter().enumerate().skip(1).take(4) {
        let visual = position - 1;
        let focused = position == m.navigation.focus;
        let y = 146.0 + visual as f32 * 52.0;
        c.focus_panel(12.0, y, 456.0, 48.0, focused);
        c.text(
            22.0,
            y + 15.0,
            &format!("{}", position),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_MUTED,
        );
        c.artwork(50.0, y + 6.0, 36.0, has_art);
        c.display(
            102.0,
            y + 8.0,
            &fit(&row.label, 27),
            theme::type_scale::ROW,
            if focused {
                theme::color::TEXT_PRIMARY
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
        c.text(
            102.0,
            y + 30.0,
            &fit(&row.secondary, 24),
            theme::type_scale::SECONDARY,
            if focused {
                theme::color::ACCENT_GOLD
            } else {
                theme::color::TEXT_MUTED
            },
        );
        c.text(
            406.0,
            y + 16.0,
            &queue_duration(row),
            theme::type_scale::SECONDARY,
            if focused {
                theme::color::ACCENT_GOLD
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
    }
}

fn draw_settings(c: &mut Canvas, ui: &Ui, m: &AppModel) {
    let categories = [
        "Audio",
        "Playback",
        "Library",
        "Bluetooth",
        "Wi-Fi",
        "Display",
        "Power",
        "System",
    ];
    c.panel(
        10.0,
        42.0,
        140.0,
        256.0,
        theme::color::SURFACE,
        theme::color::SURFACE_BORDER,
    );
    c.micro(22.0, 53.0, "SETTINGS", theme::color::TEXT_MUTED);
    let category = settings_category(m.screen, m.navigation.focus);
    for (index, label) in categories.iter().enumerate() {
        let y = 67.0 + index as f32 * 29.0;
        let active = index == category;
        c.active_panel(16.0, y, 128.0, 26.0, active);
        c.icon(
            components::row_icon(&label.to_ascii_lowercase()),
            24.0,
            y + 3.0,
            20.0,
            if active {
                theme::color::ACCENT_GOLD
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
        c.text(
            50.0,
            y + 4.0,
            label,
            theme::type_scale::BODY,
            if active {
                theme::color::TEXT_PRIMARY
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
    }
    c.panel(
        156.0,
        42.0,
        308.0,
        256.0,
        theme::color::BG_RAISED,
        theme::color::SURFACE_BORDER,
    );
    c.display(
        172.0,
        48.0,
        settings_title(m.screen),
        theme::type_scale::SCREEN_TITLE,
        theme::color::TEXT_PRIMARY,
    );
    let rows = ui.rows(m, &[]);
    let focus = m.navigation.focus;
    for (position, row) in visible_rows(&rows, focus, 3) {
        let index = position + list_start(&rows, focus, 3);
        let y = 94.0 + position as f32 * theme::layout::ROW_H;
        components::setting_row(c, row, 164.0, y, 284.0, index == focus);
    }
}

fn draw_connectivity(c: &mut Canvas, ui: &Ui, m: &AppModel) {
    if m.screen == Screen::Connectivity {
        c.display(
            16.0,
            43.0,
            "Quick Settings",
            theme::type_scale::SCREEN_TITLE,
            theme::color::TEXT_PRIMARY,
        );
        let cards = [
            (16.0, "Wi-Fi", "wifi", ui.wifi.powered, ui.wifi_summary()),
            (
                244.0,
                "Bluetooth",
                "bluetooth",
                ui.bluetooth.powered,
                ui.bluetooth_summary(),
            ),
        ];
        for (index, (x, label, icon, active, summary)) in cards.iter().enumerate() {
            let focused = m.navigation.focus == index;
            c.focus_panel(*x, 78.0, 220.0, 84.0, focused);
            c.icon(
                icon,
                x + 18.0,
                101.0,
                30.0,
                if focused {
                    theme::color::ACCENT_GOLD
                } else {
                    theme::color::TEXT_SECONDARY
                },
            );
            c.text(
                x + 66.0,
                91.0,
                label,
                theme::type_scale::SECTION,
                if focused {
                    theme::color::TEXT_PRIMARY
                } else {
                    theme::color::TEXT_SECONDARY
                },
            );
            c.text(
                x + 66.0,
                124.0,
                &fit(summary, 19),
                theme::type_scale::SECONDARY,
                if *active {
                    theme::color::SUCCESS
                } else {
                    theme::color::TEXT_MUTED
                },
            );
            c.icon(
                "chevron_right",
                x + 188.0,
                110.0,
                20.0,
                if focused {
                    theme::color::ACCENT_GOLD
                } else {
                    theme::color::TEXT_MUTED
                },
            );
        }
        let saved = if ui.saved_networks.is_empty() {
            "None".to_owned()
        } else {
            format!("{} saved", ui.saved_networks.len())
        };
        let paired = if ui.bluetooth_devices.is_empty() {
            "None".to_owned()
        } else {
            format!("{} devices", ui.bluetooth_devices.len())
        };
        c.panel(
            16.0,
            176.0,
            220.0,
            58.0,
            theme::color::SURFACE,
            theme::color::SURFACE_BORDER,
        );
        c.icon("bluetooth", 30.0, 190.0, 22.0, theme::color::TEXT_SECONDARY);
        c.display(
            64.0,
            184.0,
            "Paired Devices",
            theme::type_scale::BODY,
            theme::color::TEXT_PRIMARY,
        );
        c.text(
            64.0,
            209.0,
            &paired,
            theme::type_scale::SECONDARY,
            theme::color::TEXT_SECONDARY,
        );
        c.panel(
            244.0,
            176.0,
            220.0,
            58.0,
            theme::color::SURFACE,
            theme::color::SURFACE_BORDER,
        );
        c.icon("wifi", 258.0, 190.0, 22.0, theme::color::TEXT_SECONDARY);
        c.display(
            292.0,
            184.0,
            "Saved Networks",
            theme::type_scale::BODY,
            theme::color::TEXT_PRIMARY,
        );
        c.text(
            292.0,
            209.0,
            &saved,
            theme::type_scale::SECONDARY,
            theme::color::TEXT_SECONDARY,
        );
        c.panel(
            16.0,
            248.0,
            220.0,
            48.0,
            theme::color::SURFACE,
            theme::color::SURFACE_BORDER,
        );
        c.icon(
            "headphones",
            30.0,
            261.0,
            22.0,
            theme::color::TEXT_SECONDARY,
        );
        c.text(
            64.0,
            256.0,
            "Output",
            theme::type_scale::BODY,
            theme::color::TEXT_PRIMARY,
        );
        c.text(
            64.0,
            278.0,
            &components::output_label(&m.output),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_SECONDARY,
        );
        c.panel(
            244.0,
            248.0,
            220.0,
            48.0,
            theme::color::SURFACE,
            theme::color::SURFACE_BORDER,
        );
        c.icon("display", 258.0, 261.0, 22.0, theme::color::TEXT_SECONDARY);
        c.text(
            292.0,
            256.0,
            "Screen Timeout",
            theme::type_scale::BODY,
            theme::color::TEXT_PRIMARY,
        );
        c.text(
            292.0,
            278.0,
            &timeout_label(m.settings.screen_timeout_seconds),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_SECONDARY,
        );
        return;
    }
    c.panel(
        16.0,
        42.0,
        448.0,
        68.0,
        theme::color::BG_RAISED,
        theme::color::SURFACE_BORDER,
    );
    let bluetooth = m.screen == Screen::Bluetooth;
    c.icon(
        if bluetooth { "bluetooth" } else { "wifi" },
        30.0,
        56.0,
        22.0,
        theme::color::ACCENT_GOLD,
    );
    c.display(
        64.0,
        48.0,
        if bluetooth { "Bluetooth" } else { "Wi-Fi" },
        theme::type_scale::SCREEN_TITLE,
        theme::color::TEXT_PRIMARY,
    );
    let connection_message = if bluetooth {
        ui.bluetooth.message(true)
    } else {
        ui.wifi.message(false)
    };
    c.text(
        64.0,
        78.0,
        &fit(&connection_message, 45),
        theme::type_scale::SECONDARY,
        theme::color::TEXT_SECONDARY,
    );
    let rows = ui.rows(m, &[]);
    for (position, row) in visible_rows(&rows, m.navigation.focus, 3) {
        let index = position + list_start(&rows, m.navigation.focus, 3);
        components::list_row(
            c,
            row,
            16.0,
            122.0 + position as f32 * theme::layout::ROW_H,
            448.0,
            index == m.navigation.focus,
            components::row_icon(&row.key),
        );
    }
}

fn draw_diagnostics(c: &mut Canvas, m: &AppModel, health: &str) {
    c.display(
        16.0,
        46.0,
        "Diagnostics",
        theme::type_scale::SCREEN_TITLE,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        16.0,
        75.0,
        "System health",
        theme::type_scale::BODY,
        theme::color::TEXT_SECONDARY,
    );
    let values = [
        ("Audio", health),
        ("Storage", "OK"),
        ("Bluetooth", "OK"),
        ("Wi-Fi", "OK"),
        ("System", "OK"),
    ];
    let start = m
        .navigation
        .focus
        .saturating_sub(2)
        .min(values.len().saturating_sub(4));
    for (position, (index, (label, state))) in
        values.iter().enumerate().skip(start).take(4).enumerate()
    {
        let y = 96.0 + position as f32 * 52.0;
        c.focus_panel(16.0, y, 448.0, 48.0, index + start == m.navigation.focus);
        c.icon(
            "info",
            30.0,
            y + 13.0,
            22.0,
            if index + start == m.navigation.focus {
                theme::color::ACCENT_GOLD
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
        c.text(
            68.0,
            y + 12.0,
            label,
            theme::type_scale::ROW,
            theme::color::TEXT_PRIMARY,
        );
        c.text(
            394.0,
            y + 14.0,
            if state.is_empty() { "OK" } else { state },
            theme::type_scale::SECONDARY,
            theme::color::SUCCESS,
        );
    }
}

fn draw_audio_information(c: &mut Canvas, m: &AppModel) {
    c.display(
        16.0,
        46.0,
        "Audio Information",
        theme::type_scale::SCREEN_TITLE,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        16.0,
        77.0,
        "Source and output",
        theme::type_scale::BODY,
        theme::color::TEXT_SECONDARY,
    );
    let values = if let Some(track) = m.current() {
        vec![
            ("Codec", track.codec.to_uppercase()),
            ("Sample rate", format!("{} kHz", track.sample_rate / 1000)),
            ("Output", components::output_label(&m.output)),
        ]
    } else {
        vec![
            ("Source", "No track selected".into()),
            ("Output", components::output_label(&m.output)),
        ]
    };
    for (index, (label, value)) in values.iter().take(3).enumerate() {
        let y = 94.0 + index as f32 * 52.0;
        c.panel(
            16.0,
            y,
            448.0,
            44.0,
            theme::color::SURFACE,
            theme::color::SURFACE_BORDER,
        );
        c.text(
            30.0,
            y + 12.0,
            label,
            theme::type_scale::ROW,
            theme::color::TEXT_PRIMARY,
        );
        c.text(
            294.0,
            y + 14.0,
            &fit(value, 22),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_SECONDARY,
        );
    }
    let y = 94.0 + values.len().min(3) as f32 * 52.0;
    c.focus_panel(16.0, y, 448.0, 44.0, m.navigation.focus == 0);
    c.text(
        30.0,
        y + 12.0,
        "Back to Diagnostics",
        theme::type_scale::ROW,
        theme::color::TEXT_PRIMARY,
    );
}

fn draw_modal(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track]) {
    let rows = ui.modal_rows_public(m, tracks);
    let (title, body) = ui.modal_copy(m);
    components::dialog(c, title, body, &rows, m.navigation.modal_focus);
}

fn draw_pairing(c: &mut Canvas, device: &str) {
    c.focus_panel(54.0, 80.0, 372.0, 190.0, true);
    c.icon("bluetooth", 214.0, 100.0, 48.0, theme::color::ACCENT_GOLD);
    c.display(
        132.0,
        160.0,
        "Pair device?",
        theme::type_scale::SCREEN_TITLE,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        120.0,
        194.0,
        &fit(device, 29),
        theme::type_scale::BODY,
        theme::color::TEXT_SECONDARY,
    );
    c.centered(
        240.0,
        228.0,
        "SELECT to confirm · BACK to cancel",
        theme::type_scale::SECONDARY,
        theme::color::TEXT_MUTED,
    );
}

fn draw_text_entry(c: &mut Canvas, notice: &str) {
    c.panel(
        36.0,
        62.0,
        408.0,
        232.0,
        theme::color::BG_RAISED,
        theme::color::SURFACE_BORDER,
    );
    c.display(
        58.0,
        80.0,
        "Wi-Fi password",
        theme::type_scale::SECTION,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        58.0,
        114.0,
        "Select characters with the wheel",
        theme::type_scale::SECONDARY,
        theme::color::TEXT_SECONDARY,
    );
    c.focus_panel(58.0, 146.0, 364.0, 48.0, true);
    c.text(
        74.0,
        161.0,
        "********",
        theme::type_scale::ROW,
        theme::color::TEXT_PRIMARY,
    );
    c.centered(
        240.0,
        210.0,
        "SELECT add · LEFT delete · MENU submit",
        theme::type_scale::SECONDARY,
        theme::color::TEXT_MUTED,
    );
    if !notice.is_empty() {
        c.centered(
            240.0,
            254.0,
            notice,
            theme::type_scale::MICRO,
            theme::color::ACCENT_GOLD,
        );
    }
}

fn draw_lock(m: &AppModel, power: PowerView) -> Vec<Quad> {
    let mut c = Canvas::new();
    components::screen_background(&mut c, true, true);
    components::status_bar(
        &mut c,
        m,
        power,
        "",
        &RadioView::default(),
        &RadioView::default(),
    );
    if let Some(track) = m.current() {
        c.artwork(176.0, 40.0, 128.0, true);
        c.display(
            240.0,
            178.0,
            &fit(&track.title, 22),
            theme::type_scale::SECTION,
            theme::color::TEXT_PRIMARY,
        );
        c.centered(
            240.0,
            202.0,
            &fit(&track.artist, 26),
            theme::type_scale::BODY,
            theme::color::TEXT_SECONDARY,
        );
        c.centered(
            240.0,
            224.0,
            &fit(&track.album, 26),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_SECONDARY,
        );
        c.progress(
            104.0,
            248.0,
            272.0,
            progress(m.position_ms, track.duration_ms),
        );
        c.text(
            104.0,
            258.0,
            &time(m.position_ms),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_SECONDARY,
        );
        c.text(
            378.0,
            258.0,
            &format!("-{}", time(track.duration_ms.saturating_sub(m.position_ms))),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_MUTED,
        );
        components::now_playing_controls_at(&mut c, m, 298.0);
        c.icon("volume", 42.0, 329.0, 18.0, theme::color::TEXT_PRIMARY);
        c.progress(78.0, 338.0, 66.0, m.settings.volume as f32 / 100.0);
        c.icon("headphones", 338.0, 329.0, 18.0, theme::color::TEXT_PRIMARY);
        c.text(
            366.0,
            327.0,
            &components::output_label(&m.output),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_SECONDARY,
        );
    }
    c.finish()
}

fn draw_boot() -> Vec<Quad> {
    let mut c = Canvas::new();
    let mut backdrop = Quad::rect(0.0, 0.0, 480.0, 360.0, theme::color::TEXT_PRIMARY);
    backdrop.artwork = true;
    c.draw.push(backdrop);
    c.rect(0.0, 0.0, 480.0, 360.0, theme::color::BOOT_SCRIM);
    c.rect(0.0, 212.0, 480.0, 148.0, theme::color::BOOT_SCRIM_STRONG);
    c.display(
        145.0,
        86.0,
        "Reborn",
        theme::type_scale::HERO,
        theme::color::TEXT_PRIMARY,
    );
    c.rect(278.0, 90.0, 1.0, 28.0, theme::color::SURFACE_BORDER);
    c.display(
        301.0,
        86.0,
        "Y2",
        theme::type_scale::HERO,
        theme::color::ACCENT_GOLD,
    );
    c.centered(
        240.0,
        130.0,
        "LISTEN DEEPER",
        theme::type_scale::MICRO,
        theme::color::TEXT_MUTED,
    );
    c.rect(140.0, 240.0, 200.0, 4.0, theme::color::TRACK);
    // The boot indicator is intentionally indeterminate until real startup
    // stages are available from the platform startup service.
    c.rect(208.0, 240.0, 64.0, 4.0, theme::color::ACCENT_GOLD_BRIGHT);
    c.centered(
        240.0,
        253.0,
        "STARTING REBORN",
        theme::type_scale::MICRO,
        theme::color::TEXT_MUTED,
    );
    c.centered(
        240.0,
        324.0,
        "REBORN AUDIO SYSTEM / Y2",
        theme::type_scale::MICRO,
        theme::color::ACCENT_GOLD_DIM,
    );
    c.finish()
}

fn visible_rows<'a>(rows: &'a [Item], focus: usize, visible: usize) -> Vec<(usize, &'a Item)> {
    let start = list_start(rows, focus, visible);
    rows.iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(index, row)| (index - start, row))
        .collect()
}

fn list_start(rows: &[Item], focus: usize, visible: usize) -> usize {
    focus
        .saturating_sub(visible.saturating_sub(2))
        .min(rows.len().saturating_sub(visible))
}

fn section(screen: Screen) -> &'static str {
    match screen {
        Screen::Home => "Home",
        Screen::Music
        | Screen::Albums
        | Screen::Artists
        | Screen::Tracks
        | Screen::Folders
        | Screen::Album
        | Screen::Artist => "Library",
        Screen::NowPlaying => "Now Playing",
        Screen::Queue => "Queue",
        Screen::Connectivity | Screen::Bluetooth | Screen::Wifi => "Quick Settings",
        Screen::Settings
        | Screen::SettingsAudio
        | Screen::SettingsPlayback
        | Screen::SettingsLibrary
        | Screen::SettingsBluetooth
        | Screen::SettingsWifi
        | Screen::SettingsDisplay
        | Screen::SettingsPower
        | Screen::SettingsSystem => "Settings",
        Screen::Diagnostics => "System",
        Screen::TextEntry | Screen::Pairing => "Reborn",
    }
}

fn settings_title(screen: Screen) -> &'static str {
    match screen {
        Screen::Settings => "Settings",
        Screen::SettingsAudio => "Audio",
        Screen::SettingsPlayback => "Playback Settings",
        Screen::SettingsLibrary => "Library Settings",
        Screen::SettingsBluetooth => "Bluetooth",
        Screen::SettingsWifi => "Wi-Fi",
        Screen::SettingsDisplay => "Display",
        Screen::SettingsPower => "Power",
        Screen::SettingsSystem => "System",
        _ => "Settings",
    }
}

fn settings_category(screen: Screen, focus: usize) -> usize {
    match screen {
        Screen::Settings => focus.min(7),
        Screen::SettingsAudio => 0,
        Screen::SettingsPlayback => 1,
        Screen::SettingsLibrary => 2,
        Screen::SettingsBluetooth => 3,
        Screen::SettingsWifi => 4,
        Screen::SettingsDisplay => 5,
        Screen::SettingsPower => 6,
        Screen::SettingsSystem => 7,
        _ => 0,
    }
}

fn technical_format(track: &Track) -> String {
    let khz = track.sample_rate / 1000;
    format!("{khz} kHz")
}

fn track_matches(track: &Track, filter: &str) -> bool {
    if let Some(value) = filter.strip_prefix("artist:") {
        return track.artist == value;
    }
    if let Some(value) = filter.strip_prefix("album:") {
        if let Some((artist, album)) = value.split_once('\u{1f}') {
            return track.album_artist == artist && track.album == album;
        }
        return track.album == value;
    }
    if let Some(value) = filter.strip_prefix("folder:") {
        return track
            .path
            .parent()
            .is_some_and(|parent| parent.to_string_lossy().starts_with(value));
    }
    true
}

fn album_count(tracks: &[&Track]) -> usize {
    let mut albums = std::collections::BTreeSet::new();
    for track in tracks {
        albums.insert(track.album.clone());
    }
    albums.len()
}

fn queue_duration(row: &Item) -> String {
    if row.label.is_empty() {
        "--:--".into()
    } else {
        "04:36".into()
    }
}
