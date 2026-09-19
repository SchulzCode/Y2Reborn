//! Composition of the Reborn screens. Screens only read application state and
//! emit `Quad`s through shared components; they never call platform services.

use crate::{components, theme, Item, PowerView, RadioView, Ui};
use components::{fit, progress, time, Canvas};
use reborn_core::{AppModel, Screen, Track};
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
        Screen::Diagnostics => draw_diagnostics(&mut c, m, health),
        Screen::Home => draw_home(&mut c, ui, m, has_art),
        Screen::TextEntry | Screen::Pairing => {}
    }
    if m.navigation.modal.is_some() {
        draw_modal(&mut c, ui, m);
    }
    if !matches!(
        m.screen,
        Screen::NowPlaying | Screen::Queue | Screen::Home | Screen::Connectivity
    ) {
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
    c.micro(16.0, 252.0, "REACH", theme::color::TEXT_MUTED);
    let rows = ui.rows(m, &m.library.tracks);
    for (index, row) in rows.iter().take(5).enumerate() {
        let x = 16.0 + (index % 2) as f32 * 226.0;
        let y = 266.0 + (index / 2) as f32 * 42.0;
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
        45.0,
        title,
        theme::type_scale::SCREEN_TITLE,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        404.0,
        51.0,
        &format!("{}", rows.len()),
        theme::type_scale::MICRO,
        theme::color::TEXT_MUTED,
    );

    if rows.is_empty() {
        components::empty_state(
            c,
            x,
            82.0,
            316.0,
            "No music found",
            "Insert an SD card or scan your library.",
        );
        return;
    }
    if m.screen == Screen::Albums {
        let columns = 3;
        let tile_w = 100.0;
        let start = m
            .navigation
            .focus
            .saturating_sub(2)
            .min(rows.len().saturating_sub(6));
        for (position, row) in rows.iter().enumerate().skip(start).take(6) {
            let tile = position - start;
            let tx = x + (tile % columns) as f32 * 108.0;
            let ty = 76.0 + (tile / columns) as f32 * 112.0;
            let focused = position == m.navigation.focus;
            c.focus_panel(tx, ty, tile_w, 104.0, focused);
            c.artwork(tx + 6.0, ty + 6.0, 88.0, has_art);
            c.text(
                tx + 6.0,
                ty + 96.0,
                &fit(&row.label, 14),
                theme::type_scale::MICRO,
                if focused {
                    theme::color::TEXT_PRIMARY
                } else {
                    theme::color::TEXT_SECONDARY
                },
            );
        }
        return;
    }
    for (position, row) in visible_rows(&rows, m.navigation.focus, 5) {
        components::list_row(
            c,
            row,
            x,
            76.0 + position as f32 * 46.0,
            316.0,
            position + list_start(&rows, m.navigation.focus, 5) == m.navigation.focus,
            components::row_icon(&row.key),
        );
    }
}

fn library_rail(c: &mut Canvas, m: &AppModel) {
    c.panel(
        8.0,
        42.0,
        104.0,
        252.0,
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
        let y = 72.0 + index as f32 * 42.0;
        let active = matches!(
            (m.screen, index),
            (Screen::Albums, 0)
                | (Screen::Artists, 1)
                | (Screen::Tracks, 2)
                | (Screen::Folders, 3)
                | (Screen::Music, 0)
        );
        c.panel(
            14.0,
            y - 4.0,
            92.0,
            34.0,
            theme::color::SURFACE,
            if active {
                theme::color::ACCENT_GOLD_DIM
            } else {
                theme::color::SURFACE_BORDER
            },
        );
        c.icon(
            icon,
            24.0,
            y + 4.0,
            18.0,
            if active {
                theme::color::ACCENT_GOLD
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
        c.text(
            50.0,
            y + 5.0,
            label,
            theme::type_scale::SECONDARY,
            if active {
                theme::color::TEXT_PRIMARY
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
    }
    c.rect(22.0, 238.0, 76.0, 1.0, theme::color::SURFACE_BORDER);
    c.icon("storage", 24.0, 250.0, 18.0, theme::color::TEXT_SECONDARY);
    c.text(
        50.0,
        250.0,
        "Internal",
        theme::type_scale::MICRO,
        theme::color::TEXT_SECONDARY,
    );
    c.text(
        50.0,
        265.0,
        "128 GB",
        theme::type_scale::MICRO,
        theme::color::TEXT_MUTED,
    );
    c.icon("sd_card", 24.0, 276.0, 18.0, theme::color::TEXT_SECONDARY);
    c.text(
        50.0,
        276.0,
        "SD Card",
        theme::type_scale::MICRO,
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
        47.0,
        &fit(&track.title, 29),
        theme::type_scale::HERO,
        theme::color::TEXT_PRIMARY,
    );
    c.display(
        202.0,
        82.0,
        &fit(&track.artist, 30),
        theme::type_scale::SECTION,
        theme::color::TEXT_SECONDARY,
    );
    c.text(
        202.0,
        108.0,
        &fit(&track.album, 30),
        theme::type_scale::BODY,
        theme::color::TEXT_MUTED,
    );
    c.badge(
        202.0,
        130.0,
        46.0,
        &fit(&track.codec.to_uppercase(), 7),
        theme::color::ACCENT_GOLD,
    );
    c.badge(
        254.0,
        130.0,
        82.0,
        &technical_format(track),
        theme::color::TEXT_SECONDARY,
    );
    c.badge(344.0, 130.0, 70.0, "Hi-Res", theme::color::TEXT_MUTED);
    c.progress(
        202.0,
        178.0,
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
    c.rect(16.0, 299.0, 448.0, 1.0, theme::color::SURFACE_BORDER);
    c.icon("volume", 21.0, 309.0, 18.0, theme::color::TEXT_PRIMARY);
    c.text(
        46.0,
        312.0,
        &format!("{}", m.settings.volume),
        theme::type_scale::MICRO,
        theme::color::TEXT_SECONDARY,
    );
    c.progress(64.0, 317.0, 90.0, m.settings.volume as f32 / 100.0);
    c.icon("headphones", 194.0, 308.0, 18.0, theme::color::TEXT_PRIMARY);
    c.text(
        221.0,
        309.0,
        &components::output_label(&m.output),
        theme::type_scale::MICRO,
        theme::color::TEXT_SECONDARY,
    );
    c.text(
        221.0,
        326.0,
        "4.4 mm",
        theme::type_scale::MICRO,
        theme::color::TEXT_MUTED,
    );
    c.icon("gain", 330.0, 309.0, 18.0, theme::color::TEXT_PRIMARY);
    c.text(
        357.0,
        309.0,
        "High Gain",
        theme::type_scale::MICRO,
        theme::color::TEXT_SECONDARY,
    );
    c.text(
        357.0,
        326.0,
        "Class AB",
        theme::type_scale::MICRO,
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
        108.0,
        theme::color::BG_RAISED,
        theme::color::SURFACE_BORDER,
    );
    c.artwork(28.0, 54.0, 84.0, has_art);
    c.display(
        132.0,
        55.0,
        &fit(name, 24),
        theme::type_scale::HERO,
        theme::color::TEXT_PRIMARY,
    );
    c.micro(
        132.0,
        91.0,
        "ALTERNATIVE · POST-ROCK",
        theme::color::TEXT_MUTED,
    );
    c.text(
        132.0,
        109.0,
        &format!(
            "{} tracks · {} albums",
            artist_tracks.len(),
            album_count(&artist_tracks)
        ),
        theme::type_scale::SECONDARY,
        theme::color::TEXT_SECONDARY,
    );
    c.focus_panel(352.0, 111.0, 96.0, 28.0, m.navigation.focus == 0);
    c.text(
        366.0,
        119.0,
        "Play Artist",
        theme::type_scale::MICRO,
        if m.navigation.focus == 0 {
            theme::color::TEXT_PRIMARY
        } else {
            theme::color::TEXT_SECONDARY
        },
    );

    c.display(
        16.0,
        166.0,
        "Top Tracks",
        theme::type_scale::SECTION,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        202.0,
        171.0,
        "See All",
        theme::type_scale::MICRO,
        theme::color::TEXT_MUTED,
    );
    c.icon(
        "chevron_right",
        238.0,
        169.0,
        12.0,
        theme::color::TEXT_MUTED,
    );
    for (index, track) in artist_tracks.iter().take(4).enumerate() {
        let y = 190.0 + index as f32 * 31.0;
        let focused = index + 1 == m.navigation.focus;
        c.focus_panel(16.0, y, 218.0, 27.0, focused);
        c.text(
            27.0,
            y + 7.0,
            &format!("{}", index + 1),
            theme::type_scale::MICRO,
            theme::color::TEXT_MUTED,
        );
        c.text(
            48.0,
            y + 7.0,
            &fit(&track.title, 19),
            theme::type_scale::MICRO,
            if focused {
                theme::color::TEXT_PRIMARY
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
        c.text(
            187.0,
            y + 7.0,
            &time(track.duration_ms),
            theme::type_scale::MICRO,
            if focused {
                theme::color::ACCENT_GOLD
            } else {
                theme::color::TEXT_MUTED
            },
        );
    }
    c.display(
        252.0,
        166.0,
        "Albums",
        theme::type_scale::SECTION,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        426.0,
        171.0,
        "See All",
        theme::type_scale::MICRO,
        theme::color::TEXT_MUTED,
    );
    for index in 0..3 {
        let x = 252.0 + index as f32 * 70.0;
        c.artwork(x, 188.0, 60.0, has_art);
        c.text(
            x,
            253.0,
            &fit(&format!("Album {}", index + 1), 10),
            theme::type_scale::MICRO,
            theme::color::TEXT_SECONDARY,
        );
        c.text(
            x,
            268.0,
            "2021",
            theme::type_scale::MICRO,
            theme::color::TEXT_MUTED,
        );
    }
}

fn draw_album(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], has_art: bool) {
    let name = m
        .navigation
        .filter
        .strip_prefix("album:")
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
    for (index, row) in rows.iter().enumerate().skip(4).take(5) {
        let position = index - 4;
        components::list_row(
            c,
            row,
            16.0,
            154.0 + position as f32 * 44.0,
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
    if let Some(track) = m.current() {
        c.artwork(22.0, 52.0, 42.0, has_art);
        c.micro(78.0, 51.0, "NOW PLAYING", theme::color::ACCENT_GOLD);
        c.display(
            78.0,
            66.0,
            &fit(&track.title, 26),
            theme::type_scale::SECTION,
            theme::color::TEXT_PRIMARY,
        );
        c.text(
            78.0,
            88.0,
            &fit(&track.artist, 24),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_SECONDARY,
        );
        c.text(
            399.0,
            54.0,
            &fit(&technical_format(track), 11),
            theme::type_scale::MICRO,
            theme::color::ACCENT_GOLD,
        );
        c.text(
            426.0,
            86.0,
            &format!("-{}", time(track.duration_ms.saturating_sub(m.position_ms))),
            theme::type_scale::MICRO,
            theme::color::TEXT_MUTED,
        );
    }
    c.display(
        14.0,
        123.0,
        "Up Next",
        theme::type_scale::SECTION,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        91.0,
        128.0,
        &format!(
            "{} tracks remaining",
            m.queue.len().saturating_sub(m.queue_position + 1)
        ),
        theme::type_scale::MICRO,
        theme::color::TEXT_MUTED,
    );
    c.icon("shuffle", 322.0, 119.0, 23.0, theme::color::ACCENT_GOLD);
    c.icon("repeat", 374.0, 119.0, 23.0, theme::color::TEXT_SECONDARY);
    c.icon("menu", 425.0, 119.0, 23.0, theme::color::TEXT_SECONDARY);
    let rows = ui.rows(m, &[]);
    for (position, row) in rows.iter().enumerate().skip(1).take(4) {
        let visual = position - 1;
        let focused = position == m.navigation.focus;
        let y = 154.0 + visual as f32 * 38.0;
        c.focus_panel(12.0, y, 456.0, 34.0, focused);
        c.text(
            20.0,
            y + 9.0,
            &format!("{}", position),
            theme::type_scale::MICRO,
            theme::color::TEXT_MUTED,
        );
        c.artwork(44.0, y + 3.0, 28.0, has_art);
        c.display(
            84.0,
            y + 5.0,
            &fit(&row.label, 27),
            theme::type_scale::SECONDARY,
            if focused {
                theme::color::TEXT_PRIMARY
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
        c.text(
            84.0,
            y + 22.0,
            &fit(&row.secondary, 24),
            theme::type_scale::MICRO,
            if focused {
                theme::color::ACCENT_GOLD
            } else {
                theme::color::TEXT_MUTED
            },
        );
        c.text(
            398.0,
            y + 9.0,
            &queue_duration(row),
            theme::type_scale::MICRO,
            if focused {
                theme::color::ACCENT_GOLD
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
        c.icon("menu", 445.0, y + 9.0, 14.0, theme::color::TEXT_MUTED);
    }
    c.rect(0.0, 318.0, 480.0, 42.0, theme::color::BG_RAISED);
    c.icon("menu", 16.0, 333.0, 14.0, theme::color::TEXT_MUTED);
    c.text(
        38.0,
        334.0,
        "Press MENU for options",
        theme::type_scale::MICRO,
        theme::color::TEXT_MUTED,
    );
    c.centered(
        240.0,
        348.0,
        "LISTEN DEEPER / REBORN",
        theme::type_scale::MICRO,
        theme::color::TEXT_MUTED,
    );
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
        142.0,
        252.0,
        theme::color::SURFACE,
        theme::color::SURFACE_BORDER,
    );
    c.micro(22.0, 53.0, "SETTINGS", theme::color::TEXT_MUTED);
    let category = settings_category(m.screen, m.navigation.focus);
    for (index, label) in categories.iter().enumerate() {
        let y = 70.0 + index as f32 * 27.0;
        let active = index == category;
        c.panel(
            16.0,
            y,
            130.0,
            24.0,
            theme::color::SURFACE,
            if active {
                theme::color::ACCENT_GOLD_DIM
            } else {
                theme::color::SURFACE_BORDER
            },
        );
        c.icon(
            components::row_icon(&label.to_ascii_lowercase()),
            24.0,
            y + 4.0,
            15.0,
            if active {
                theme::color::ACCENT_GOLD
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
        c.text(
            47.0,
            y + 4.0,
            label,
            theme::type_scale::MICRO,
            if active {
                theme::color::TEXT_PRIMARY
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
    }
    c.panel(
        160.0,
        42.0,
        308.0,
        252.0,
        theme::color::BG_RAISED,
        theme::color::SURFACE_BORDER,
    );
    c.display(
        176.0,
        52.0,
        settings_title(m.screen),
        theme::type_scale::SCREEN_TITLE,
        theme::color::TEXT_PRIMARY,
    );
    c.text(
        176.0,
        80.0,
        settings_description(m.screen),
        theme::type_scale::MICRO,
        theme::color::TEXT_SECONDARY,
    );
    let rows = ui.rows(m, &[]);
    let focus = m.navigation.focus;
    for (index, row) in rows.iter().enumerate().take(5) {
        let y = 96.0 + index as f32 * 39.0;
        components::setting_row(c, row, 168.0, y, 292.0, index == focus);
    }
}

fn draw_connectivity(c: &mut Canvas, ui: &Ui, m: &AppModel) {
    if m.screen == Screen::Connectivity {
        c.display(
            16.0,
            45.0,
            "Quick Settings",
            theme::type_scale::SCREEN_TITLE,
            theme::color::TEXT_PRIMARY,
        );
        c.text(
            16.0,
            76.0,
            "Connectivity and device shortcuts",
            theme::type_scale::SECONDARY,
            theme::color::TEXT_SECONDARY,
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
            c.focus_panel(*x, 98.0, 220.0, 78.0, focused);
            c.icon(
                icon,
                x + 16.0,
                119.0,
                28.0,
                if focused {
                    theme::color::ACCENT_GOLD
                } else {
                    theme::color::TEXT_SECONDARY
                },
            );
            c.text(
                x + 58.0,
                113.0,
                label,
                theme::type_scale::SECTION,
                if focused {
                    theme::color::TEXT_PRIMARY
                } else {
                    theme::color::TEXT_SECONDARY
                },
            );
            c.text(
                x + 58.0,
                139.0,
                &fit(summary, 19),
                theme::type_scale::MICRO,
                if *active {
                    theme::color::SUCCESS
                } else {
                    theme::color::TEXT_MUTED
                },
            );
            c.icon(
                "chevron_right",
                x + 190.0,
                125.0,
                18.0,
                if focused {
                    theme::color::ACCENT_GOLD
                } else {
                    theme::color::TEXT_MUTED
                },
            );
        }
        c.panel(
            16.0,
            188.0,
            220.0,
            84.0,
            theme::color::SURFACE,
            theme::color::SURFACE_BORDER,
        );
        c.display(
            30.0,
            201.0,
            "Paired Devices",
            theme::type_scale::SECONDARY,
            theme::color::TEXT_PRIMARY,
        );
        c.text(
            30.0,
            229.0,
            &fit(&ui.bluetooth_summary(), 25),
            theme::type_scale::MICRO,
            theme::color::TEXT_SECONDARY,
        );
        c.text(
            30.0,
            248.0,
            "View All",
            theme::type_scale::MICRO,
            theme::color::TEXT_MUTED,
        );
        c.icon("chevron_right", 83.0, 246.0, 14.0, theme::color::TEXT_MUTED);
        c.panel(
            244.0,
            188.0,
            220.0,
            84.0,
            theme::color::SURFACE,
            theme::color::SURFACE_BORDER,
        );
        c.display(
            258.0,
            201.0,
            "Network & Sync",
            theme::type_scale::SECONDARY,
            theme::color::TEXT_PRIMARY,
        );
        c.text(
            258.0,
            229.0,
            &fit(&ui.wifi_summary(), 25),
            theme::type_scale::MICRO,
            theme::color::TEXT_SECONDARY,
        );
        c.text(
            258.0,
            248.0,
            "Manage networks",
            theme::type_scale::MICRO,
            theme::color::TEXT_MUTED,
        );
        c.icon(
            "chevron_right",
            425.0,
            246.0,
            14.0,
            theme::color::TEXT_MUTED,
        );
        for (index, (label, icon)) in [
            ("Airplane", "display"),
            ("Do Not Disturb", "repeat"),
            ("Brightness", "display"),
            ("System", "settings"),
        ]
        .iter()
        .enumerate()
        {
            let x = 16.0 + index as f32 * 112.0;
            c.panel(
                x,
                280.0,
                104.0,
                34.0,
                theme::color::SURFACE,
                theme::color::SURFACE_BORDER,
            );
            c.icon(icon, x + 10.0, 289.0, 15.0, theme::color::TEXT_SECONDARY);
            c.text(
                x + 31.0,
                291.0,
                label,
                theme::type_scale::MICRO,
                theme::color::TEXT_SECONDARY,
            );
        }
        c.centered(
            240.0,
            348.0,
            "LISTEN DEEPER / REBORN",
            theme::type_scale::MICRO,
            theme::color::TEXT_MUTED,
        );
        return;
    }
    c.panel(
        16.0,
        42.0,
        448.0,
        52.0,
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
        50.0,
        section(m.screen),
        theme::type_scale::SECTION,
        theme::color::TEXT_PRIMARY,
    );
    let connection_message = if bluetooth {
        ui.bluetooth.message(true)
    } else {
        ui.wifi.message(false)
    };
    c.text(
        64.0,
        75.0,
        &fit(&connection_message, 45),
        theme::type_scale::MICRO,
        theme::color::TEXT_SECONDARY,
    );
    let rows = ui.rows(m, &[]);
    for (index, row) in rows.iter().enumerate().take(5) {
        components::list_row(
            c,
            row,
            16.0,
            108.0 + index as f32 * 44.0,
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
        77.0,
        "System health at a glance",
        theme::type_scale::SECONDARY,
        theme::color::TEXT_SECONDARY,
    );
    for (index, (label, state)) in [
        ("Audio", health),
        ("Storage", "OK"),
        ("Bluetooth", "OK"),
        ("Wi-Fi", "OK"),
        ("System", "OK"),
    ]
    .iter()
    .enumerate()
    {
        let y = 105.0 + index as f32 * 39.0;
        c.focus_panel(16.0, y, 448.0, 32.0, index == m.navigation.focus);
        c.icon(
            "info",
            28.0,
            y + 7.0,
            17.0,
            if index == m.navigation.focus {
                theme::color::ACCENT_GOLD
            } else {
                theme::color::TEXT_SECONDARY
            },
        );
        c.text(
            62.0,
            y + 8.0,
            label,
            theme::type_scale::ROW,
            theme::color::TEXT_PRIMARY,
        );
        c.text(
            388.0,
            y + 9.0,
            if state.is_empty() { "OK" } else { state },
            theme::type_scale::MICRO,
            theme::color::SUCCESS,
        );
    }
}

fn draw_modal(c: &mut Canvas, ui: &Ui, m: &AppModel) {
    let rows = ui.modal_rows_public(m);
    let (title, body) = ui.modal_copy(m);
    components::dialog(c, title, body, &rows, m.navigation.modal_focus);
}

fn draw_pairing(c: &mut Canvas, device: &str) {
    c.panel(
        54.0,
        80.0,
        372.0,
        190.0,
        theme::color::BG_RAISED,
        theme::color::ACCENT_GOLD,
    );
    c.icon("bluetooth", 214.0, 100.0, 48.0, theme::color::ACCENT_GOLD);
    c.display(
        132.0,
        160.0,
        "Pair device?",
        theme::type_scale::SECTION,
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
        theme::type_scale::MICRO,
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
    c.panel(
        58.0,
        146.0,
        364.0,
        42.0,
        theme::color::SURFACE,
        theme::color::SURFACE_BORDER,
    );
    c.text(
        74.0,
        158.0,
        "********",
        theme::type_scale::ROW,
        theme::color::TEXT_PRIMARY,
    );
    c.centered(
        240.0,
        210.0,
        "SELECT add · LEFT delete · MENU submit",
        theme::type_scale::MICRO,
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
        c.artwork(156.0, 35.0, 168.0, true);
        c.display(
            240.0,
            205.0,
            &fit(&track.title, 25),
            theme::type_scale::SECTION,
            theme::color::TEXT_PRIMARY,
        );
        c.centered(
            240.0,
            230.0,
            &fit(&track.artist, 25),
            theme::type_scale::BODY,
            theme::color::TEXT_SECONDARY,
        );
        c.centered(
            240.0,
            249.0,
            &fit(&track.album, 25),
            theme::type_scale::SECONDARY,
            theme::color::TEXT_MUTED,
        );
        c.progress(
            104.0,
            263.0,
            272.0,
            progress(m.position_ms, track.duration_ms),
        );
        c.text(
            104.0,
            276.0,
            &time(m.position_ms),
            theme::type_scale::MICRO,
            theme::color::TEXT_SECONDARY,
        );
        c.text(
            378.0,
            276.0,
            &format!("-{}", time(track.duration_ms.saturating_sub(m.position_ms))),
            theme::type_scale::MICRO,
            theme::color::TEXT_MUTED,
        );
        c.icon("volume", 42.0, 302.0, 18.0, theme::color::TEXT_PRIMARY);
        c.progress(84.0, 311.0, 60.0, m.settings.volume as f32 / 100.0);
        c.icon("headphones", 338.0, 302.0, 18.0, theme::color::TEXT_PRIMARY);
        c.text(
            366.0,
            304.0,
            &components::output_label(&m.output),
            theme::type_scale::MICRO,
            theme::color::TEXT_SECONDARY,
        );
        c.text(
            366.0,
            320.0,
            "4.4 mm",
            theme::type_scale::MICRO,
            theme::color::TEXT_MUTED,
        );
        components::now_playing_controls(&mut c, m);
    }
    c.centered(
        240.0,
        347.0,
        "LISTEN DEEPER / REBORN",
        theme::type_scale::MICRO,
        theme::color::TEXT_MUTED,
    );
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
    c.rect(140.0, 240.0, 94.0, 4.0, theme::color::ACCENT_GOLD_BRIGHT);
    c.centered(
        240.0,
        253.0,
        "INITIALIZING MUSIC EXPERIENCE",
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
        Screen::SettingsAudio => "Playback Settings",
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

fn settings_description(screen: Screen) -> &'static str {
    match screen {
        Screen::SettingsAudio => "Audio settings for a purer listening experience.",
        Screen::SettingsPlayback => "How music moves between tracks.",
        Screen::SettingsLibrary => "Sources and library maintenance.",
        Screen::SettingsBluetooth => "Pair and manage wireless headphones.",
        Screen::SettingsWifi => "Connect to networks and saved Wi-Fi.",
        Screen::SettingsDisplay => "Brightness and screen timeout.",
        Screen::SettingsPower => "Safe power controls.",
        Screen::SettingsSystem => "About, diagnostics and safe actions.",
        _ => "Choose a category.",
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
    let bits = if track.codec.to_ascii_lowercase().contains("flac") {
        "24"
    } else {
        "16"
    };
    let khz = track.sample_rate / 1000;
    format!("{bits}-bit / {khz} kHz")
}

fn track_matches(track: &Track, filter: &str) -> bool {
    if let Some(value) = filter.strip_prefix("artist:") {
        return track.artist == value;
    }
    if let Some(value) = filter.strip_prefix("album:") {
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
