//! Native 480×360 screen composition. All screen actions follow one wheel path.
use crate::{
    components::{self, progress, time, Canvas},
    platform,
    theme::{color, type_scale},
    Item, PowerView, Ui,
};
use reborn_core::{AppModel, PlaybackState, Screen, Track};
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
pub fn preview(
    ui: &Ui,
    mut m: AppModel,
    tracks: &[Track],
    power: PowerView,
    screen: PreviewScreen,
) -> Vec<Quad> {
    m.screen = match screen {
        PreviewScreen::NowPlaying | PreviewScreen::Lock => Screen::NowPlaying,
        PreviewScreen::Library => Screen::Albums,
        PreviewScreen::Artist => Screen::Artist,
        PreviewScreen::Queue => Screen::Queue,
        PreviewScreen::Settings => Screen::SettingsAudio,
        PreviewScreen::QuickSettings => Screen::QuickSettings,
        PreviewScreen::Boot => return boot("Starting music services"),
    };
    draw(ui, &m, tracks, "ok", true, power)
}
pub fn boot(message: &str) -> Vec<Quad> {
    let mut c = Canvas::new();
    c.rect(0., 0., 480., 360., color::BG);
    c.centered(240., 143., "Reborn", 4.25, color::TEXT_PRIMARY);
    c.centered(
        240.,
        193.,
        "MUSIC LIVES ON",
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
    c.rect(218., 230., 44., 2., color::ACCENT_GOLD);
    c.centered(240., 263., message, type_scale::BODY, color::TEXT_SECONDARY);
    c.finish()
}
pub fn draw(
    ui: &Ui,
    m: &AppModel,
    tracks: &[Track],
    health: &str,
    has_art: bool,
    power: PowerView,
) -> Vec<Quad> {
    if health == "starting" {
        return boot("Starting music services");
    }
    let mut c = Canvas::new();
    c.rect(0., 0., 480., 360., color::BG);
    if matches!(
        m.platform.busy,
        Some(reborn_core::PlatformTask::UpdateApply | reborn_core::PlatformTask::UpdateRollback)
    ) {
        components::title(&mut c, "Preparing System Restart", "");
        wrap(&mut c, 24., 105., 432., "The platform updater is preparing the verified root operation. Keep external power connected.", 18., 5);
        c.text(
            24.,
            277.,
            "Navigation resumes if the operation fails.",
            type_scale::BODY,
            color::TEXT_SECONDARY,
        );
        return c.finish();
    }
    if m.screen_off {
        return c.finish();
    }
    components::status_bar(&mut c, m, power, &ui.wifi, &ui.bluetooth);
    let interactive = m.navigation.modal.is_none() && ui.pairing.is_none() && !ui.text_entry;
    if m.screen == Screen::NowPlaying {
        now_playing(&mut c, m, has_art, interactive);
    } else if m.screen == Screen::Platform && m.navigation.filter.starts_with("value:") {
        value_detail(&mut c, m, interactive);
    } else {
        list(&mut c, ui, m, tracks, has_art, interactive);
    }
    if let Some(pair) = &ui.pairing {
        components::dialog(
            &mut c,
            "Pair Bluetooth Device",
            pair,
            &[
                Item::new("Confirm Pairing", "yes"),
                Item::new("Cancel — press Back", "no"),
            ],
            ui.pairing_focus,
        );
    } else if ui.text_entry {
        entry(&mut c, ui);
    } else if m.navigation.modal.is_some() {
        let (title, body) = ui.modal_copy(m);
        let rows = ui.modal_rows_public(m, tracks);
        components::dialog(
            &mut c,
            title,
            body,
            &rows,
            m.navigation.modal_focus.min(rows.len().saturating_sub(1)),
        );
    }
    if interactive
        && !ui.notice.is_empty()
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
        Screen::Home => "Your music".into(),
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
        Screen::Queue => "Queue".into(),
        Screen::TrackInfo => "Track Information".into(),
        Screen::LibraryIndex => "Jump to Letter".into(),
        Screen::Connectivity => "Connectivity".into(),
        Screen::QuickSettings => "Quick Settings".into(),
        Screen::Wifi | Screen::SettingsWifi => "Wi-Fi".into(),
        Screen::Bluetooth | Screen::SettingsBluetooth => "Bluetooth".into(),
        Screen::Settings => "Settings".into(),
        Screen::SettingsAudio => "Audio".into(),
        Screen::SettingsPlayback => "Playback".into(),
        Screen::SettingsLibrary => "Library".into(),
        Screen::SettingsDisplay => "Display".into(),
        Screen::SettingsPower => "Power".into(),
        Screen::SettingsSystem => "System".into(),
        Screen::Platform | Screen::Diagnostics => platform::title(&m.navigation.filter).into(),
        Screen::Album => "Album".into(),
        Screen::Artist => "Artist".into(),
        _ => "Reborn".into(),
    }
}
fn list(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], has_art: bool, interactive: bool) {
    let count = ui.row_count(m, tracks);
    let focus = m.navigation.focus.min(count.saturating_sub(1));
    let collection = matches!(m.screen, Screen::Album | Screen::Artist);
    let mut top = 76.;
    let mut visible = 5;
    let detail = if count > 0 {
        format!("{} / {}", focus + 1, count)
    } else {
        String::new()
    };
    components::title(c, &screen_title(m), &detail);
    if collection {
        visible = 3;
        top = 170.;
        let t = ui.collection_track(m, tracks);
        let art_matches = t.zip(m.current()).is_some_and(|(t, current)| {
            t.album == current.album && t.album_artist == current.album_artist
        });
        let collection_art = t.is_some_and(|t| {
            ui.collection_art
                .as_ref()
                .is_some_and(|(source, id)| source == &t.source_id && *id == t.id)
        });
        c.artwork(16., 77., 78., collection_art || (has_art && art_matches));
        if collection_art {
            if let Some(quad) = c.draw.last_mut() {
                quad.collection_artwork = true;
            }
        }
        let (name, sub) = if let Some(t) = t {
            if m.screen == Screen::Artist {
                (t.artist.as_str(), "All songs and albums".into())
            } else {
                (
                    t.album.as_str(),
                    format!(
                        "{} · {} tracks",
                        crate::display_or_unknown(if t.album_artist.is_empty() {
                            &t.artist
                        } else {
                            &t.album_artist
                        }),
                        count.saturating_sub(1)
                    ),
                )
            }
        } else {
            ("Unavailable", "This collection is offline".into())
        };
        c.text_box(
            110.,
            83.,
            354.,
            crate::display_or_unknown(name),
            type_scale::SECTION,
            color::TEXT_PRIMARY,
        );
        c.text_box(
            110.,
            111.,
            354.,
            &sub,
            type_scale::BODY,
            color::TEXT_SECONDARY,
        );
        c.text(
            110.,
            139.,
            "Hold Select  Collection / track actions",
            type_scale::SECONDARY,
            color::TEXT_SECONDARY,
        );
    }
    if count == 0 {
        let (title, body) = if m.screen == Screen::Queue {
            ("Queue is empty", "Choose a song or album from Music.")
        } else if m.library.scanning {
            (
                "Finding your music",
                "Your library will appear when scanning finishes.",
            )
        } else if m.library.error.is_some() {
            (
                "Library scan interrupted",
                "Existing entries are kept. Check storage and rescan.",
            )
        } else {
            (
                "No music here",
                "Check your music source, then scan the library.",
            )
        };
        c.text(28., 121., title, type_scale::SECTION, color::TEXT_PRIMARY);
        wrap(c, 28., 156., 424., body, 14., 3);
        let item = Item::new(
            if m.screen == Screen::Queue {
                "Open Music"
            } else {
                "Scan Library"
            },
            "empty",
        );
        components::row(c, &item, 235., interactive, "", false);
    } else {
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
                top + i as f32 * 48.,
                interactive && absolute == focus,
                &tail,
                playing,
            );
        }
    }
    let hint = if m.screen == Screen::Home {
        "Wheel  Browse   •   Select  Open"
    } else {
        "Back  Parent   •   Hold Back  Home"
    };
    components::footer(c, m, has_art, hint);
    if m.screen == Screen::Queue {
        c.rect(0., 324., 480., 36., color::BG_RAISED);
        c.text(
            16.,
            336.,
            &format!(
                "Shuffle {}   •   Repeat {}   •   Hold Select: actions",
                if m.settings.shuffle { "On" } else { "Off" },
                crate::repeat_label(m.settings.repeat)
            ),
            type_scale::SECONDARY,
            color::TEXT_SECONDARY,
        );
    }
    if matches!(
        m.screen,
        Screen::Wifi | Screen::SettingsWifi | Screen::Bluetooth | Screen::SettingsBluetooth
    ) {
        let radio = if matches!(m.screen, Screen::Wifi | Screen::SettingsWifi) {
            &ui.wifi
        } else {
            &ui.bluetooth
        };
        c.rect(0., 324., 480., 36., color::BG_RAISED);
        c.text_box(
            16.,
            336.,
            448.,
            &radio.message(matches!(
                m.screen,
                Screen::Bluetooth | Screen::SettingsBluetooth
            )),
            type_scale::SECONDARY,
            color::TEXT_SECONDARY,
        );
    }
    if matches!(m.screen, Screen::Platform | Screen::Diagnostics) {
        c.rect(0., 324., 480., 36., color::BG_RAISED);
        let message = if m.platform.busy.is_some() {
            "Working…  Please wait"
        } else if m.platform.failure.is_some() {
            "Operation unavailable — open Result for details"
        } else {
            "Select  Full value   •   Back  Parent"
        };
        c.text(
            16.,
            336.,
            message,
            type_scale::SECONDARY,
            color::TEXT_SECONDARY,
        );
    }
}
fn now_playing(c: &mut Canvas, m: &AppModel, has_art: bool, interactive: bool) {
    let Some(t) = m.current() else {
        components::title(c, "Now Playing", "");
        c.artwork(34., 99., 112., false);
        c.text(
            170.,
            119.,
            "Nothing playing",
            type_scale::SECTION,
            color::TEXT_PRIMARY,
        );
        c.text(
            170.,
            150.,
            "Choose music to begin",
            type_scale::BODY,
            color::TEXT_SECONDARY,
        );
        components::row(
            c,
            &Item::new("Open Music", "music"),
            257.,
            interactive,
            "",
            false,
        );
        components::footer(c, m, false, "Play / Pause works on every screen");
        return;
    };
    c.artwork(16., 62., 164., has_art);
    c.text(
        196.,
        47.,
        "NOW PLAYING",
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
    wrap(
        c,
        196.,
        78.,
        268.,
        if t.title.is_empty() {
            &t.filename
        } else {
            &t.title
        },
        26.,
        2,
    );
    c.text_box(
        196.,
        149.,
        268.,
        crate::display_or_unknown(&t.artist),
        type_scale::SECTION,
        color::TEXT_PRIMARY,
    );
    c.text_box(
        196.,
        178.,
        268.,
        crate::display_or_unknown(&t.album),
        type_scale::BODY,
        color::TEXT_SECONDARY,
    );
    let source_offline =
        !m.sources.is_empty() && !m.sources.iter().any(|s| s.id == t.source_id && s.online);
    let state = if source_offline {
        "Music source offline"
    } else {
        match m.playback {
            PlaybackState::Playing => "Playing",
            PlaybackState::Paused => "Paused",
            PlaybackState::Buffering => "Starting audio…",
            PlaybackState::Error => "Output unavailable",
            PlaybackState::Stopped => "Stopped",
        }
    };
    c.icon(
        if m.playback == PlaybackState::Playing {
            "play"
        } else {
            "pause"
        },
        196.,
        207.,
        18.,
        color::TEXT_PRIMARY,
    );
    c.text(222., 209., state, type_scale::BODY, color::TEXT_SECONDARY);
    if source_offline || m.playback == PlaybackState::Error {
        c.text(
            196.,
            230.,
            if source_offline {
                "Check storage or reinsert the SD card"
            } else {
                "Check the output, then press Play"
            },
            type_scale::SECONDARY,
            color::TEXT_SECONDARY,
        );
    }
    c.progress(16., 251., 448., progress(m.position_ms, t.duration_ms));
    c.text(
        16.,
        266.,
        &time(m.position_ms),
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
    let remaining = format!("-{}", time(t.duration_ms.saturating_sub(m.position_ms)));
    let width = components::text_width(&remaining, 12.);
    c.text(
        464. - width,
        266.,
        &remaining,
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
    for (i, label) in ["Options", "Queue", "Audio Info"].iter().enumerate() {
        let x = 16. + i as f32 * 152.;
        c.focus_panel(
            x,
            291.,
            144.,
            29.,
            interactive && m.navigation.focus.min(2) == i,
        );
        c.text_box(
            x + 12.,
            299.,
            120.,
            label,
            type_scale::BODY,
            color::TEXT_PRIMARY,
        );
    }
    c.rect(0., 324., 480., 36., color::BG_RAISED);
    c.icon("volume", 16., 332., 18., color::TEXT_SECONDARY);
    c.text(
        42.,
        335.,
        &m.settings.volume.to_string(),
        type_scale::BODY,
        color::TEXT_PRIMARY,
    );
    c.centered(
        240.,
        337.,
        "Play / Pause  ·  Previous / Next",
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
}
pub(crate) fn wrap(
    c: &mut Canvas,
    x: f32,
    mut y: f32,
    width: f32,
    text: &str,
    size: f32,
    lines: usize,
) {
    let mut remaining = text.trim();
    for line in 0..lines {
        if remaining.is_empty() {
            break;
        }
        if line == lines - 1 {
            c.text_box(x, y, width, remaining, size / 8., color::TEXT_PRIMARY);
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
        c.text_box(
            x,
            y,
            width,
            &remaining[..split],
            size / 8.,
            color::TEXT_PRIMARY,
        );
        remaining = remaining[split..].trim_start();
        y += size + 6.;
    }
}
fn value_detail(c: &mut Canvas, m: &AppModel, interactive: bool) {
    let v = m.navigation.filter.strip_prefix("value:").unwrap_or("");
    let (title, value) = v.split_once('\u{1f}').unwrap_or(("Detail", v));
    components::title(c, title, "");
    wrap(c, 24., 90., 432., value, 16., 8);
    components::row(c, &Item::new("Back", "back"), 270., interactive, "", false);
}
fn entry(c: &mut Canvas, ui: &Ui) {
    c.rect(0., 28., 480., 332., color::BG);
    components::title(c, "Wi-Fi Password", "");
    c.text_box(
        16.,
        79.,
        448.,
        &ui.ssid,
        type_scale::BODY,
        color::TEXT_SECONDARY,
    );
    c.text(
        16.,
        112.,
        &format!("{} characters entered", ui.password.len()),
        type_scale::BODY,
        color::TEXT_PRIMARY,
    );
    let count = crate::LETTERS.len() + 3;
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
    c.focus_panel(120., 157., 240., 65., true);
    c.centered(
        240.,
        177.,
        &label,
        type_scale::SCREEN_TITLE,
        color::TEXT_PRIMARY,
    );
    c.centered(
        240.,
        242.,
        &format!("{} / {count}", focus + 1),
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
    c.text(
        16.,
        285.,
        "Wheel  Choose   •   Select  Add / action",
        type_scale::BODY,
        color::TEXT_SECONDARY,
    );
    c.text(
        16.,
        329.,
        "Hold Select  Connect   •   Back  Cancel",
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
}
