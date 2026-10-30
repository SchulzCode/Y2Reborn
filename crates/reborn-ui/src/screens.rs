//! Native 480×360 composition for UI v3: one screen at a time, one focus,
//! plus the overlays (sheets, dialogs, toasts, the volume line, the Power
//! hold) that sit above any screen.
use crate::{
    canvas::Canvas,
    components::{self, SheetRow, Tone},
    pages::Page,
    skin::{self, Scene, Sheet, Skin},
    text::{self, Face},
    theme::{color, HEIGHT, WIDTH},
    views_library, views_playing, views_settings, views_system, Alert, Item, Ui,
};
use reborn_core::{platform::LowBattery, AppModel, Modal, Screen, Track};
use reborn_graphics::Quad;

/// Rows visible in one window for a screen's list area.
pub fn visible_rows_for(screen: Screen) -> usize {
    match screen {
        Screen::Albums => 8,
        Screen::Album | Screen::Artist => 5,
        Screen::Folders | Screen::Queue => 5,
        Screen::Tracks | Screen::Artists | Screen::Genres => 6,
        _ => 6,
    }
}

pub fn draw(ui: &Ui, m: &AppModel, tracks: &[Track], has_art: bool) -> Vec<Quad> {
    if let Some(skin) = ui.skin.as_deref() {
        return skinned(ui, skin, m, tracks, has_art);
    }
    let mut c = Canvas::new();
    c.rect(0., 0., WIDTH, HEIGHT, color::BG);
    if matches!(
        m.platform.busy,
        Some(reborn_core::PlatformTask::UpdateApply | reborn_core::PlatformTask::UpdateRollback)
    ) {
        views_system::installing(&mut c);
        return c.finish();
    }
    if m.screen_off {
        return c.finish();
    }
    let interactive = m.navigation.modal.is_none()
        && ui.pairing.is_none()
        && ui.keyboard.is_none()
        && ui.alert.is_none();
    if ui.update_run {
        views_settings::update_overlay(&mut c, ui, m);
        overlays(&mut c, ui, m, tracks);
        crate::motion::apply(ui, m, &mut c);
        return c.finish();
    }
    // The keyboard covers the whole screen: the screen under it is not drawn.
    if ui.keyboard.is_none() {
        match m.screen {
            Screen::NowPlaying if m.current().is_some() => {
                views_playing::now_playing(&mut c, ui, m, tracks, has_art, interactive)
            }
            Screen::Home => views_library::home(&mut c, ui, m, tracks, has_art, interactive),
            Screen::Music => views_library::music(&mut c, ui, m, tracks, interactive),
            Screen::Albums
            | Screen::Artists
            | Screen::Genres
            | Screen::Tracks
            | Screen::Folders
            | Screen::Album
            | Screen::Artist
            | Screen::Audiobooks => {
                views_library::browse(&mut c, ui, m, tracks, has_art, interactive)
            }
            Screen::Search => crate::views_search::page(&mut c, ui, m, tracks, interactive),
            Screen::Queue => crate::views_queue::page(&mut c, ui, m, tracks, interactive),
            Screen::Book => crate::views_books::book_page(&mut c, ui, m, tracks, interactive),
            Screen::Settings
            | Screen::Wifi
            | Screen::Bluetooth
            | Screen::SettingsAudio
            | Screen::Equalizer
            | Screen::Crossfade
            | Screen::Balance
            | Screen::ScreenTimeout
            | Screen::Output
            | Screen::HeadphoneLevel
            | Screen::DacFilter
            | Screen::Theme
            | Screen::TimeZone
            | Screen::DateTime
            | Screen::HomeMenu
            | Screen::SettingsDisplay
            | Screen::Customize
            | Screen::Storage
            | Screen::LastFm
            | Screen::ErrorReports
            | Screen::Update => views_settings::screen(&mut c, ui, m, tracks, interactive),
            Screen::Playlists | Screen::Playlist => {
                crate::views_playlists::screen(&mut c, ui, m, tracks, has_art, interactive)
            }
            Screen::Radio | Screen::RadioStations | Screen::RadioCategories | Screen::RadioInfo => {
                crate::views_radio::screen(&mut c, ui, m, tracks, interactive)
            }
            Screen::Spotify if ui.spotify.approval.is_some() => {
                views_settings::spotify_sign_in(&mut c, ui, m, tracks, interactive)
            }
            _ => views_system::page(&mut c, ui, m, tracks, interactive),
        }
    }
    overlays(&mut c, ui, m, tracks);
    crate::motion::apply(ui, m, &mut c);
    c.finish()
}

/// Reborn's own drawing of the current screen (what a skin falls back to).
fn native_screen(
    c: &mut Canvas,
    ui: &Ui,
    m: &AppModel,
    tracks: &[Track],
    has_art: bool,
    interactive: bool,
) {
    match m.screen {
        Screen::NowPlaying if m.current().is_some() => {
            views_playing::now_playing(c, ui, m, tracks, has_art, interactive)
        }
        Screen::Home => views_library::home(c, ui, m, tracks, has_art, interactive),
        Screen::Albums
        | Screen::Artists
        | Screen::Genres
        | Screen::Tracks
        | Screen::Folders
        | Screen::Album
        | Screen::Artist
        | Screen::Audiobooks => views_library::browse(c, ui, m, tracks, has_art, interactive),
        Screen::Queue => crate::views_queue::page(c, ui, m, tracks, interactive),
        Screen::Search => crate::views_search::page(c, ui, m, tracks, interactive),
        Screen::Book => crate::views_books::book_page(c, ui, m, tracks, interactive),
        Screen::Settings
        | Screen::Wifi
        | Screen::Bluetooth
        | Screen::SettingsAudio
        | Screen::Equalizer
        | Screen::Crossfade
        | Screen::Balance
        | Screen::ScreenTimeout
        | Screen::Output
        | Screen::HeadphoneLevel
        | Screen::DacFilter
        | Screen::Theme
        | Screen::TimeZone
        | Screen::DateTime
        | Screen::HomeMenu
        | Screen::SettingsDisplay
        | Screen::Customize
        | Screen::Storage
        | Screen::LastFm
        | Screen::ErrorReports
        | Screen::Update => views_settings::screen(c, ui, m, tracks, interactive),
        Screen::Playlists | Screen::Playlist => {
            crate::views_playlists::screen(c, ui, m, tracks, has_art, interactive)
        }
        Screen::Radio | Screen::RadioStations | Screen::RadioCategories | Screen::RadioInfo => {
            crate::views_radio::screen(c, ui, m, tracks, interactive)
        }
        Screen::Spotify if ui.spotify.approval.is_some() => {
            views_settings::spotify_sign_in(c, ui, m, tracks, interactive)
        }
        _ => views_system::page(c, ui, m, tracks, interactive),
    }
}

/// The skin layout names for the current screen, most specific first.
fn layout_keys(m: &AppModel, page: Option<&Page>, empty: bool) -> &'static [&'static str] {
    let paged = page.is_some_and(|p| p.hero.is_some() || !p.facts.is_empty());
    macro_rules! keys {
        ($own:literal) => {
            if paged {
                &[$own, "page"]
            } else {
                &[$own, "list"]
            }
        };
    }
    match m.screen {
        Screen::NowPlaying if m.current().is_some() => &["now_playing"],
        Screen::Home => &["home", "list"],
        // Customize > Cover Flow: albums and audiobooks alike (8.36).
        Screen::Albums | Screen::Audiobooks if crate::cover_flow(m) && !empty => &["cover_flow"],
        Screen::Albums => &["albums", "list"],
        Screen::Artists => &["artists", "list"],
        Screen::Genres => &["genres", "list"],
        Screen::Tracks => &["songs", "list"],
        Screen::Search => &["search", "list"],
        Screen::Folders => &["folders", "list"],
        Screen::Album => &["album", "list"],
        Screen::Artist => &["artist", "list"],
        Screen::Queue => &["queue", "list"],
        Screen::Equalizer => &["equalizer"],
        Screen::TextEntry | Screen::Pairing => &[],
        Screen::ValueDetail => &["value", "page"],
        Screen::Music => keys!("music"),
        Screen::NowPlaying => keys!("now_playing"),
        Screen::TrackInfo => keys!("song_info"),
        Screen::Settings => keys!("settings"),
        Screen::Wifi => keys!("wifi"),
        Screen::Radio => keys!("radio"),
        Screen::RadioStations => keys!("radio_stations"),
        Screen::RadioCategories => keys!("radio_categories"),
        Screen::RadioInfo => keys!("radio_info"),
        Screen::Playlists => &["playlists", "list"],
        Screen::Playlist => &["playlist", "list"],
        Screen::Bluetooth => keys!("bluetooth"),
        Screen::PcTransfer => keys!("computer"),
        Screen::SettingsAudio => keys!("sound"),
        Screen::LastFm => keys!("lastfm"),
        Screen::Spotify => keys!("spotify"),
        Screen::SpotifyList => keys!("spotify_list"),
        Screen::SettingsPlayback => keys!("playback"),
        Screen::Crossfade => keys!("crossfade"),
        Screen::Balance => keys!("balance"),
        Screen::ScreenTimeout => keys!("screen_off"),
        Screen::Output => keys!("output"),
        Screen::HeadphoneLevel => keys!("headphone_level"),
        Screen::DacFilter => keys!("dac_filter"),
        Screen::ErrorReports => keys!("error_reports"),
        Screen::Theme => keys!("theme"),
        Screen::TimeZone => keys!("time_zone"),
        Screen::DateTime => keys!("date_time"),
        Screen::HomeMenu => keys!("home_menu"),
        Screen::SettingsLibrary => keys!("library"),
        Screen::SettingsDisplay => keys!("display"),
        Screen::Customize => keys!("customize"),
        Screen::Audiobooks => keys!("audiobooks"),
        Screen::Book => keys!("book"),
        Screen::SettingsSystem => keys!("service"),
        Screen::Battery => keys!("battery"),
        Screen::Sleep => keys!("sleep"),
        Screen::Storage => keys!("storage"),
        Screen::Update => keys!("updates"),
        Screen::About => keys!("about"),
        Screen::Maintenance => keys!("maintenance"),
        Screen::Diagnostics => keys!("diagnostics"),
        Screen::DiagnosticSection => keys!("diagnostic"),
    }
}

/// The page a skin layout shows for screens built from `pages`.
fn skin_page(ui: &Ui, m: &AppModel, tracks: &[Track]) -> Option<Page> {
    match m.screen {
        Screen::NowPlaying if m.current().is_some() => None,
        Screen::Home
        | Screen::Albums
        | Screen::Artists
        | Screen::Genres
        | Screen::Tracks
        | Screen::Search
        | Screen::Playlists
        | Screen::Playlist
        | Screen::Folders
        | Screen::Album
        | Screen::Artist
        | Screen::Queue
        | Screen::Diagnostics
        | Screen::DiagnosticSection
        | Screen::TextEntry
        | Screen::Pairing => None,
        Screen::ValueDetail => {
            let (title, value) = m
                .navigation
                .filter
                .split_once('\u{1f}')
                .unwrap_or(("Detail", &m.navigation.filter));
            Some(Page {
                hero: Some(crate::pages::Hero {
                    title: title.to_owned(),
                    body: value.to_owned(),
                }),
                rows: vec![Item::new("Back", "back")],
                ..Default::default()
            })
        }
        _ => Some(crate::pages::page(ui, m, tracks)),
    }
}

/// The layout a skin draws the current screen with, if it has one.
pub(crate) fn skin_layout<'s>(
    skin: &'s Skin,
    ui: &Ui,
    m: &AppModel,
    tracks: &[Track],
) -> Option<(&'static str, &'s [skin::Node], Option<Page>)> {
    // The Last.fm sign-in code is Reborn's own drawing in every skin: a
    // generic page layout would show its text without the code.
    if m.screen == Screen::LastFm && ui.lastfm.approval.is_some() {
        return None;
    }
    if m.screen == Screen::Spotify && ui.spotify.approval.is_some() {
        return None;
    }
    let page = skin_page(ui, m, tracks);
    let empty = m.screen == Screen::Albums && ui.row_count(m, tracks) == 0;
    layout_keys(m, page.as_ref(), empty)
        .iter()
        .find_map(|k| skin.layout(k).map(|l| (*k, l)))
        .map(|(k, l)| (k, l, page))
}

fn skinned(ui: &Ui, skin: &Skin, m: &AppModel, tracks: &[Track], has_art: bool) -> Vec<Quad> {
    let mut c = Canvas::new();
    c.rect(0., 0., WIDTH, HEIGHT, skin.bg());
    if matches!(
        m.platform.busy,
        Some(reborn_core::PlatformTask::UpdateApply | reborn_core::PlatformTask::UpdateRollback)
    ) {
        views_system::installing(&mut c);
        skin::remap(&mut c, 1, &skin.palette, ui.cover_accent());
        return c.finish();
    }
    if m.screen_off {
        return c.finish();
    }
    let interactive = m.navigation.modal.is_none()
        && ui.pairing.is_none()
        && ui.keyboard.is_none()
        && ui.alert.is_none();
    if ui.update_run {
        views_settings::update_overlay(&mut c, ui, m);
        skin::remap(&mut c, 1, &skin.palette, ui.cover_accent());
        skinned_overlays(&mut c, ui, skin, m, tracks);
        return c.finish();
    }
    let start = c.draw.len();
    match skin_layout(skin, ui, m, tracks) {
        Some((_, layout, page)) => {
            let native = |c: &mut Canvas| native_screen(c, ui, m, tracks, has_art, interactive);
            let scene = Scene {
                ui,
                m,
                tracks,
                skin,
                has_art,
                interactive,
                page: page.as_ref(),
                sheet: None,
                message: None,
                opacity: 1.,
                native: Some(&native),
                label_end: Default::default(),
            };
            skin::draw_layout(&mut c, &scene, layout);
        }
        None => {
            native_screen(&mut c, ui, m, tracks, has_art, interactive);
            skin::remap(&mut c, start, &skin.palette, ui.cover_accent());
        }
    }
    skinned_overlays(&mut c, ui, skin, m, tracks);
    c.finish()
}

/// `overlays` for a skin: its sheet, dialog, toast and volume layouts where
/// it has them, else Reborn's own in the skin's palette.
fn skinned_overlays(c: &mut Canvas, ui: &Ui, skin: &Skin, m: &AppModel, tracks: &[Track]) {
    let scene = |sheet, message, opacity| Scene {
        ui,
        m,
        tracks,
        skin,
        has_art: false,
        interactive: false,
        page: None,
        sheet,
        message,
        opacity,
        native: None,
        label_end: Default::default(),
    };
    // A sheet or dialog: (title, body, rows, focus, danger, is a dialog).
    let mut sheet: Option<(String, String, Vec<Item>, usize, bool, bool)> = None;
    if let Some(alert) = &ui.alert {
        sheet = Some(match alert {
            Alert::BluetoothFailed { name, .. } => (
                "Couldn't Connect".into(),
                format!("{name} did not answer. Check that it is on and in range."),
                vec![Item::new("Cancel", ""), Item::new("Try Again", "")],
                ui.alert_focus(),
                false,
                true,
            ),
            Alert::BatteryLow => (
                "Battery Low".into(),
                m.platform
                    .battery
                    .percent
                    .map(|p| format!("{p}% left. Charge soon to keep playing."))
                    .unwrap_or_else(|| "Charge soon to keep playing.".into()),
                vec![Item::new("OK", "")],
                0,
                false,
                true,
            ),
        });
    } else if let Some(pair) = &ui.pairing {
        sheet = Some((
            "Pair with This Device?".into(),
            pair.clone(),
            vec![Item::new("Pair", ""), Item::new("Cancel", "")],
            ui.pairing_focus,
            false,
            true,
        ));
    } else if let Some(k) = &ui.keyboard {
        let at = c.draw.len();
        match skin.layout("keyboard") {
            // The skin's title bar over Reborn's keys in the skin's colours.
            Some(layout) => {
                let title = k.purpose.title();
                let body = |c: &mut Canvas| {
                    let at = c.draw.len();
                    crate::keyboard::draw_body(c, ui, m, tracks, k);
                    skin::remap(c, at, &skin.palette, ui.cover_accent());
                };
                let s = Scene {
                    native: Some(&body),
                    sheet: Some(Sheet {
                        title: &title,
                        body: "",
                        rows: &[],
                        focus: 0,
                        danger: false,
                    }),
                    ..scene(None, None, 1.)
                };
                skin::draw_layout(c, &s, layout);
            }
            None => {
                crate::keyboard::draw(c, ui, m, tracks, k);
                skin::remap(c, at, &skin.palette, ui.cover_accent());
            }
        }
    } else if m.navigation.modal.is_none() {
        let at = c.draw.len();
        ui.draw_folding(c);
        skin::remap(c, at, &skin.palette, ui.cover_accent());
    }
    if ui.alert.is_none()
        && ui.pairing.is_none()
        && ui.keyboard.is_none()
        && m.navigation.modal.is_some()
    {
        let (title, body) = ui.modal_copy(m);
        let rows = ui.modal_rows_public(m, tracks);
        let focus = m.navigation.modal_focus.min(rows.len().saturating_sub(1));
        sheet = Some(match &m.navigation.modal {
            Some(Modal::Confirm(action)) => {
                let danger = crate::confirm_is_danger(*action);
                (title, body, rows, focus, danger, true)
            }
            Some(Modal::ContextMenu) => {
                let (song, subtitle) = context_song(ui, m, tracks);
                (song.unwrap_or(title), subtitle, rows, focus, false, false)
            }
            _ => (title, body, rows, focus, false, false),
        });
    }
    if let Some((title, body, rows, focus, danger, dialog)) = &sheet {
        let layout = if *dialog {
            skin.layout("dialog").or_else(|| skin.layout("sheet"))
        } else {
            skin.layout("sheet")
        };
        match layout {
            Some(layout) => {
                let s = scene(
                    Some(Sheet {
                        title,
                        body,
                        rows,
                        focus: *focus,
                        danger: *danger,
                    }),
                    None,
                    1.,
                );
                skin::draw_layout(c, &s, layout);
            }
            None => {
                let at = c.draw.len();
                let rows: Vec<SheetRow> = rows
                    .iter()
                    .map(|r| SheetRow {
                        label: &r.label,
                        detail: &r.secondary,
                        enabled: r.enabled,
                    })
                    .collect();
                if *dialog {
                    components::dialog(c, title, body, &rows, *focus, *danger);
                } else {
                    components::action_sheet(c, title, body, &rows, *focus);
                }
                skin::remap(c, at, &skin.palette, ui.cover_accent());
            }
        }
    }
    // Toasts.
    let toast = if matches!(
        m.platform.battery.level,
        LowBattery::Critical | LowBattery::ShuttingDown
    ) {
        Some((
            m.platform
                .battery
                .percent
                .map(|p| format!("Battery low · {p}%"))
                .unwrap_or_else(|| "Battery Low".into()),
            true,
        ))
    } else if !ui.notice.is_empty()
        && ui
            .notice_until
            .is_some_and(|t| t > std::time::Instant::now())
    {
        Some((ui.notice.clone(), is_problem(&ui.notice)))
    } else {
        None
    };
    if let Some((message, error)) = &toast {
        match skin.layout("toast") {
            Some(layout) => skin::draw_layout(c, &scene(None, Some((message, *error)), 1.), layout),
            None => {
                let at = c.draw.len();
                components::toast(c, message, if *error { Tone::Error } else { Tone::Info });
                skin::remap(c, at, &skin.palette, ui.cover_accent());
            }
        }
    }
    let visibility = ui.volume_visibility();
    if visibility > 0. {
        match skin.layout("volume") {
            Some(layout) => skin::draw_layout(c, &scene(None, None, visibility), layout),
            None => {
                let at = c.draw.len();
                views_playing::volume_line(c, ui, m);
                skin::remap(c, at, &skin.palette, ui.cover_accent());
            }
        }
    }
    let at = c.draw.len();
    views_playing::power_hold(c, ui);
    skin::remap(c, at, &skin.palette, ui.cover_accent());
}

/// The sheet, dialog or entry surface above the screen, then toasts and the
/// volume line.
fn overlays(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track]) {
    if let Some(alert) = &ui.alert {
        let ok = |label| SheetRow {
            label,
            detail: "",
            enabled: true,
        };
        match alert {
            Alert::BluetoothFailed { name, .. } => components::dialog(
                c,
                "Couldn't Connect",
                &format!("{name} did not answer. Check that it is on and in range."),
                &[ok("Cancel"), ok("Try Again")],
                ui.alert_focus(),
                false,
            ),
            Alert::BatteryLow => {
                let body = m
                    .platform
                    .battery
                    .percent
                    .map(|p| format!("{p}% left. Charge soon to keep playing."))
                    .unwrap_or_else(|| "Charge soon to keep playing.".into());
                components::dialog(c, "Battery Low", &body, &[ok("OK")], 0, false);
            }
        }
    } else if let Some(pair) = &ui.pairing {
        components::dialog(
            c,
            "Pair with This Device?",
            pair,
            &[
                SheetRow {
                    label: "Pair",
                    detail: "",
                    enabled: true,
                },
                SheetRow {
                    label: "Cancel",
                    detail: "",
                    enabled: true,
                },
            ],
            ui.pairing_focus,
            false,
        );
    } else if let Some(k) = &ui.keyboard {
        crate::keyboard::draw(c, ui, m, tracks, k);
    } else if m.navigation.modal.is_some() {
        modal(c, ui, m, tracks);
    } else {
        ui.draw_folding(c);
    }
    if matches!(
        m.platform.battery.level,
        LowBattery::Critical | LowBattery::ShuttingDown
    ) {
        let percent = m
            .platform
            .battery
            .percent
            .map(|p| format!("Battery low · {p}%"))
            .unwrap_or_else(|| "Battery Low".into());
        components::toast(c, &percent, Tone::Error);
    } else if !ui.notice.is_empty()
        && ui
            .notice_until
            .is_some_and(|t| t > std::time::Instant::now())
    {
        let tone = if is_problem(&ui.notice) {
            Tone::Error
        } else {
            Tone::Info
        };
        components::toast(c, &ui.notice, tone);
    }
    views_playing::volume_line(c, ui, m);
    views_playing::power_hold(c, ui);
}

fn is_problem(message: &str) -> bool {
    let lower = message.to_lowercase();
    ["can't", "couldn't", "failed", "no audio", "error"]
        .iter()
        .any(|w| lower.contains(w))
}

fn modal(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track]) {
    let (title, body) = ui.modal_copy(m);
    let rows = ui.modal_rows_public(m, tracks);
    let focus = m.navigation.modal_focus.min(rows.len().saturating_sub(1));
    let sheet: Vec<SheetRow> = rows
        .iter()
        .map(|r| SheetRow {
            label: &r.label,
            detail: &r.secondary,
            enabled: r.enabled,
        })
        .collect();
    match &m.navigation.modal {
        Some(Modal::Confirm(action)) => {
            let danger = crate::confirm_is_danger(*action);
            components::dialog(c, &title, &body, &sheet, focus, danger);
        }
        Some(Modal::ContextMenu) => {
            let (song, subtitle) = context_song(ui, m, tracks);
            let title = song.unwrap_or(title);
            components::action_sheet(c, &title, &subtitle, &sheet, focus);
        }
        _ => components::action_sheet(c, &title, &body, &sheet, focus),
    }
}

/// For a song's options sheet: the song's title and artist.
pub(crate) fn context_song(ui: &Ui, m: &AppModel, tracks: &[Track]) -> (Option<String>, String) {
    // A book playing: its title and author, not the file's tags.
    if m.screen == Screen::NowPlaying {
        if let Some((book, _)) = m.book_part() {
            return (Some(book.title.clone()), book.author.clone());
        }
    }
    // Up Next's entries and history, groups, the songs chosen.
    if m.screen != Screen::NowPlaying {
        let key = m.navigation.context_key.clone().or_else(|| {
            m.navigation
                .context_target
                .and_then(|i| ui.row_at(m, tracks, i).map(|r| r.key))
        });
        if let Some((title, line)) = key
            .filter(|k| !k.starts_with("track:"))
            .and_then(|k| ui.menu_subject(m, tracks, &k))
        {
            return (Some(title), line);
        }
    }
    let song = if m.screen == Screen::NowPlaying {
        m.current()
    } else {
        let key = m.navigation.context_key.clone().or_else(|| {
            m.navigation
                .context_target
                .and_then(|i| ui.row_at(m, tracks, i).map(|r| r.key))
        });
        key.as_deref()
            .and_then(|k| k.strip_prefix("track:"))
            .and_then(|i| i.parse::<usize>().ok())
            .and_then(|i| tracks.get(i))
    };
    match song {
        Some(t) => (
            Some(crate::track_title(t).to_owned()),
            crate::names_or_unknown(&t.artist).into_owned(),
        ),
        None => (None, String::new()),
    }
}

/// The screen's title in the status bar.
pub fn screen_title(m: &AppModel) -> String {
    match m.screen {
        Screen::Home => String::new(),
        Screen::Music => "Music".into(),
        Screen::Albums => match m.navigation.filter.strip_prefix("genre:") {
            Some("") => "No Genre".into(),
            Some(genre) => genre.into(),
            None => "Albums".into(),
        },
        Screen::Genres => "Genres".into(),
        Screen::Artists => "Artists".into(),
        Screen::Tracks => "Songs".into(),
        // One section alone (See All): its name.
        Screen::Search => crate::catalog::search_parts(&m.navigation.filter)
            .1
            .map_or("Search", crate::catalog::Section::name)
            .into(),
        Screen::Folders => {
            if m.navigation.filter.is_empty() {
                "Folders".into()
            } else {
                crate::folder_name(m, m.navigation.filter.trim_start_matches("folder:"))
            }
        }
        Screen::Album => "Album".into(),
        Screen::Artist => "Artist".into(),
        Screen::NowPlaying => "Now Playing".into(),
        Screen::Queue => "Up Next".into(),
        Screen::TrackInfo => "Song Info".into(),
        Screen::Settings => "Settings".into(),
        Screen::Wifi => "Wi-Fi".into(),
        Screen::Radio => "Radio".into(),
        Screen::RadioStations | Screen::RadioCategories if !m.navigation.filter.is_empty() => {
            m.navigation.filter.clone()
        }
        Screen::RadioStations => "Stations".into(),
        Screen::RadioCategories => "Radio".into(),
        Screen::RadioInfo => "Station Info".into(),
        Screen::Playlists => "Playlists".into(),
        Screen::Playlist => std::path::Path::new(&m.navigation.filter)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Playlist".into()),
        Screen::Bluetooth => "Bluetooth".into(),
        Screen::PcTransfer => "Computer".into(),
        Screen::SettingsAudio => "Sound".into(),
        Screen::LastFm => "Last.fm".into(),
        Screen::Spotify => "Spotify".into(),
        Screen::SpotifyList => crate::spotify_filter(&m.navigation.filter).1.into(),
        Screen::Equalizer => "Equalizer".into(),
        Screen::SettingsPlayback => "Playback".into(),
        Screen::Crossfade => "Crossfade".into(),
        Screen::Balance => "Balance".into(),
        Screen::ScreenTimeout => "Screen Off After".into(),
        Screen::Output => "Output".into(),
        Screen::HeadphoneLevel => "Headphone Level".into(),
        Screen::DacFilter => "DAC Filter".into(),
        Screen::ErrorReports if m.navigation.filter == "save" => "Save Report".into(),
        Screen::ErrorReports => "Error Reports".into(),
        Screen::Theme => "Skin".into(),
        Screen::TimeZone => crate::time_zone::title(m).unwrap_or("Time Zone").into(),
        Screen::DateTime => "Date & Time".into(),
        Screen::HomeMenu => "Home Menu".into(),
        Screen::SettingsLibrary => "Library".into(),
        Screen::SettingsDisplay => "Display".into(),
        Screen::Customize => "Customize".into(),
        Screen::Audiobooks => "Audiobooks".into(),
        Screen::Book => "Audiobook".into(),
        Screen::SettingsSystem => "Service".into(),
        Screen::Battery => "Battery".into(),
        Screen::Sleep => "Sleep".into(),
        Screen::Storage => "Storage".into(),
        Screen::Update => "About & Updates".into(),
        Screen::About => "About".into(),
        Screen::Maintenance => "Reset & Maintenance".into(),
        Screen::Diagnostics => "Diagnostics".into(),
        Screen::DiagnosticSection => crate::diagnostics::title(&m.navigation.filter).into(),
        Screen::ValueDetail | Screen::TextEntry | Screen::Pairing => "Reborn".into(),
    }
}

/// Split `text` into at most `lines` lines that fit `width` at `size` px.
pub(crate) fn wrap_lines(text: &str, width: f32, face: Face, lines: usize) -> Vec<String> {
    text::wrap(face, text, width, lines)
}

/// The number of songs, as the Home list shows it.
pub(crate) fn count_label(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            // A no-break space: the thin space (U+2009) is not in the fonts.
            out.push('\u{a0}');
        }
        out.push(ch);
    }
    out
}

/// A song row's duration, or nothing for rows that are not songs.
pub(crate) fn row_track<'a>(row: &Item, tracks: &'a [Track]) -> Option<(usize, &'a Track)> {
    row.key
        .strip_prefix("track:")
        .and_then(|s| s.parse::<usize>().ok())
        .and_then(|i| tracks.get(i).map(|t| (i, t)))
}

pub(crate) fn is_current(m: &AppModel, t: &Track) -> bool {
    m.current()
        .is_some_and(|c| c.id == t.id && c.source_id == t.source_id)
}
