//! Now Playing, the volume line and the Power hold.
use crate::{
    canvas::{Canvas, Icon},
    components::{self, cover_shadow},
    pages,
    text::{self, Face},
    theme::{alpha, color, fade, HEIGHT, WIDTH},
    Ui,
};
use reborn_core::{platform::SdCard, AppModel, PlaybackState, RepeatMode, Track};
use reborn_graphics::{Quad, Tex};

pub fn now_playing(
    c: &mut Canvas,
    ui: &Ui,
    m: &AppModel,
    tracks: &[Track],
    has_art: bool,
    interactive: bool,
) {
    let Some(t) = m.current() else {
        return;
    };
    // Now Playing is the one screen that wears the cover's colour (UI v4).
    let accent = ui.cover_accent();
    // The cover's colour washes the top of the screen; the bottom stays dark.
    c.glow(150., 90., 300., 210., alpha(accent, 0x3C));
    c.glow(420., 60., 160., 120., alpha(accent, 0x1E));
    c.fade_from_bottom(0., 120., WIDTH, HEIGHT - 120., 0x06070ADC);
    components::status_bar(c, ui, m, "", true);

    let radio = t.is_radio();
    let playing = matches!(
        m.playback,
        PlaybackState::Playing | PlaybackState::Buffering
    );
    // Beta 7.2: a new song fades in; a new play state pulses the button.
    let (new_song, new_state) = ui.np_changes(t.id, playing);
    let shown = ui.ease(
        &format!("np:song:{}", t.id),
        new_song.then_some(0.),
        1.,
        crate::motion::CALM,
        0.01,
    );
    let song_from = c.draw.len();
    cover_shadow(c, 26., 46., 176.);
    if has_art {
        c.draw.push(Quad {
            tex: Tex::Art,
            ..Quad::rect(26., 46., 176., 176., color::TEXT)
        });
    } else if radio {
        // A station without a logo (yet): its initials, as in the lists.
        components::initials_tile(c, crate::track_title(t), 26., 46., 176.);
    } else {
        components::placeholder(c, 26., 46., 176., 15., 0);
    }

    if radio {
        let w = components::live_pill(c, 222., 61., "LIVE");
        c.text_tracked(
            222. + w + 8.,
            61. + Face::S10.mid_offset(),
            &text::fit(Face::S10, &crate::track_title(t).to_uppercase(), 228. - w),
            Face::S10,
            accent,
            1.4,
        );
    }
    let song = ui.radio.song.as_ref().filter(|_| radio);
    // A book: the chapter as the title, then the book and its author.
    let book = crate::views_books::now(m);
    let (title, second, third) = match &book {
        Some(b) => (b.chapter.clone(), b.book.clone(), b.author.clone()),
        None => lines(ui, t),
    };
    let lines = text::wrap(Face::S25, &title, 236., 2);
    // A song needs no label saying it plays: its title takes the label's place.
    // A station keeps the LIVE line above its title.
    let mut baseline = if radio { 107. } else { 84. };
    for line in &lines {
        c.text(222., baseline, line, Face::S25, color::TEXT);
        baseline += 28.;
    }
    // `baseline` now sits one pitch below the last title line.
    let artist_baseline = baseline - 1.;
    c.text(
        222.,
        artist_baseline,
        &text::fit(Face::R15, &second, 236.),
        Face::R15,
        0xDEE1E8FF,
    );
    c.text(
        222.,
        artist_baseline + 20.,
        &text::fit(Face::R11, &third, 236.),
        Face::R11,
        color::MUTED,
    );
    let info_cy = artist_baseline + 45.;
    let source_offline = !t.is_live()
        && !m.sources.is_empty()
        && !m.sources.iter().any(|s| s.id == t.source_id && s.online);
    if source_offline
        || m.playback == PlaybackState::Error
        || m.playback == PlaybackState::Buffering
    {
        let note = if source_offline {
            "SD card removed · reinsert it"
        } else if m.playback == PlaybackState::Error && radio {
            "Station stopped · choose it again"
        } else if m.playback == PlaybackState::Error {
            "No audio output · check headphones"
        } else if radio {
            "Tuning in…"
        } else if m.playback == PlaybackState::Error && t.is_spotify() {
            "Spotify stopped · choose a song again"
        } else if t.is_spotify() {
            "Starting Spotify…"
        } else {
            "Starting…"
        };
        c.text_mid(222., info_cy, 236., note, Face::S13, color::WARNING);
    } else if radio && song.is_none() && !t.artist.is_empty() && t.artist != "Live" {
        // Connecting… / Reconnecting… (try n): how the connection is doing.
        c.text_mid(222., info_cy, 236., &t.artist, Face::S13, color::WARNING);
    } else if let Some(b) = &book {
        // How far into the book, how long it still runs.
        let info = b.left();
        let mut x = 222.;
        if (b.speed - 1.).abs() > 0.01 {
            let label = format!("{}×", crate::speed_label(b.speed));
            x += components::tag(c, x, info_cy, &label, accent) + 8.;
        }
        c.text_mid(x, info_cy, 458. - x, &info, Face::R13, color::MUTED);
    } else if !radio {
        // A song's quality tag; a station's format and country are on
        // Station Info.
        let mut x = 222.;
        if let Some(label) = quality_label(t) {
            x += components::tag(c, x, info_cy, &label, accent) + 8.;
        }
        // Favorites' heart for the song playing.
        let index = tracks
            .iter()
            .position(|x| x.id == t.id && x.source_id == t.source_id);
        if index.is_some_and(|i| ui.is_favorite_track(m, tracks, i)) {
            c.icon(Icon::HeartOn, x + 9., info_cy, 17., 0xFF5A7AFF);
        }
    }
    status_icons(c, m, 222., info_cy + 33., accent);
    // What plays next (Beta 8.33): your queue first, then the source.
    let next = m
        .queue
        .get(m.queue_position + 1)
        .filter(|_| !radio && book.is_none() && m.settings.repeat != RepeatMode::Track);
    if let Some(next) = next {
        let cy = (info_cy + 66.).min(238.);
        c.text_tracked(
            222.,
            cy + Face::S10.mid_offset(),
            "NEXT",
            Face::S10,
            color::MUTED,
            1.4,
        );
        let x = 222. + text::width(Face::S10, "NEXT") + 16.;
        c.text_mid(
            x,
            cy,
            458. - x,
            &format!(
                "{}  ·  {}",
                crate::track_title(next),
                crate::names_or_unknown(&next.artist)
            ),
            Face::R13,
            color::MUTED,
        );
    }
    if shown < 1. {
        crate::motion::fade_all(&mut c.draw[song_from..], shown);
    }

    // Progress: a wave while music plays, laid flat when paused (Beta 7.2).
    // A book shows its chapter's progress.
    let (position, length) = match &book {
        Some(b) => (b.elapsed(ui.shown_position(m)), b.chapter_ms),
        None => (ui.shown_position(m), t.duration_ms),
    };
    let (x0, x1) = (26., WIDTH - 26.);
    let amp = ui.ease("np:wave", None, if playing { 3.6 } else { 0. }, 6., 0.02);
    let phase = if ui.ui_motion {
        crate::motion::phase(0.9)
    } else {
        0.
    };
    if playing && amp > 0. {
        ui.mark_live();
    }
    // A station has no length: a full wave, listening time and LIVE.
    components::wave_progress(
        c,
        x0,
        263.,
        x1 - x0,
        if radio {
            1.
        } else {
            components::progress(position, length)
        },
        // Opaque, so the wave's joints do not bead.
        if radio {
            crate::theme::mix(accent, color::BG, 0.3)
        } else {
            color::TEXT
        },
        phase,
        amp,
        !radio,
        ui.wave_levels(),
    );
    c.text_mid(
        x0,
        281.,
        80.,
        &components::time(position),
        Face::R11,
        color::MUTED,
    );
    // A broadcast has no end; its LIVE pill says so above.
    let remaining = if radio {
        String::new()
    } else {
        format!("-{}", components::time(length.saturating_sub(position)))
    };
    c.text_mid_right(x1, 281., &remaining, Face::R11, color::MUTED);

    // Transport: the button dips and its new icon fades in on a change.
    let cy = 322.;
    let pulse = ui.ease(
        &format!("np:press:{playing}"),
        new_state.then_some(1.),
        0.,
        crate::motion::QUICK,
        0.01,
    );
    let at = c.draw.len();
    c.disc(240., cy, 25. * (1. - 0.1 * pulse), color::TEXT);
    if interactive {
        c.mark_focus(at);
    }
    let icon_at = c.draw.len();
    c.icon(
        if playing {
            Icon::PauseLarge
        } else {
            Icon::PlayLarge
        },
        241.,
        cy,
        26.,
        color::INK,
    );
    if pulse > 0. {
        crate::motion::fade_all(&mut c.draw[icon_at..], 1. - pulse);
    }
    c.icon(Icon::PrevLarge, 240. - 78., cy, 30., color::TEXT);
    c.icon(Icon::NextLarge, 240. + 78., cy, 30., color::TEXT);
    let output = pages::output_name(ui, m);
    c.text_mid(x0, cy, 96., &output, Face::R11, color::MUTED);
    // A running sleep timer, opposite the output.
    if let Some(note) = sleep_note(m) {
        c.text_mid_right(x1, cy, &note, Face::R11, color::MUTED);
    }
}

/// A running sleep timer as Now Playing says it: "Sleeps in 23 min", "Sleeps
/// after song" (skins: `{sleep}`, #29).
pub(crate) fn sleep_note(m: &AppModel) -> Option<String> {
    m.sleep_timer?;
    Some(
        match crate::sleep_label(m, m.book_part().is_some()).as_str() {
            "End of Chapter" => "Sleeps after chapter".to_owned(),
            "End of Song" => "Sleeps after song".to_owned(),
            minutes => format!("Sleeps in {minutes}"),
        },
    )
}

/// `FLAC · 96 KHZ`: what the file is, in a sentence the listener understands.
pub(crate) fn quality_label(t: &Track) -> Option<String> {
    if t.codec.is_empty() {
        return None;
    }
    let lossless = ["flac", "alac", "wav", "aiff", "ape", "wv", "pcm"]
        .iter()
        .any(|c| t.codec.eq_ignore_ascii_case(c));
    let rate = if t.sample_rate == 0 {
        String::new()
    } else {
        format!(" · {}", pages::khz(t.sample_rate))
    };
    Some(if lossless {
        format!("LOSSLESS{rate}")
    } else {
        format!("{}{rate}", t.codec.to_uppercase())
    })
}

/// Icons for the playback options that differ from the default; gapless is
/// on by default and has none.
fn status_icons(c: &mut Canvas, m: &AppModel, x: f32, cy: f32, accent: u32) {
    let s = &m.settings;
    let mut icons = vec![];
    if s.shuffle {
        icons.push(Icon::Shuffle);
    }
    match s.repeat {
        RepeatMode::All => icons.push(Icon::Repeat),
        RepeatMode::Track => icons.push(Icon::RepeatOne),
        RepeatMode::Off => {}
    }
    if s.crossfade_ms > 0 {
        icons.push(Icon::Crossfade);
    }
    if s.eq_enabled {
        icons.push(Icon::Eq);
    }
    if s.replay_gain != reborn_core::ReplayGainMode::Off {
        icons.push(Icon::Normalise);
    }
    for (i, icon) in icons.into_iter().enumerate() {
        c.icon(icon, x + 10. + i as f32 * 30., cy, 20., accent);
    }
}

/// A bare vertical line at the right edge, visible for about a second after
/// the volume changes. No icon, no number.
pub fn volume_line(c: &mut Canvas, ui: &Ui, m: &AppModel) {
    let visibility = ui.volume_visibility();
    if visibility <= 0. {
        return;
    }
    let (x, top, bottom) = (WIDTH - 15., 110., 250.);
    c.glow(
        x,
        (top + bottom) / 2.,
        20.,
        98.,
        fade(0x000000B4, visibility),
    );
    c.rrect(
        x - 2.,
        top,
        4.,
        bottom - top,
        2.,
        fade(0xFFFFFF46, visibility),
    );
    let height = ((bottom - top) * f32::from(m.settings.volume.min(100)) / 100.).round();
    if height >= 1. {
        c.rrect(
            x - 2.,
            bottom - height,
            4.,
            height,
            2.,
            fade(color::TEXT, visibility),
        );
    }
}

/// The ring that fills while Power is held: release to cancel.
pub fn power_hold(c: &mut Canvas, ui: &Ui) {
    let Some(progress) = ui.power_hold_progress() else {
        return;
    };
    let appear = (progress * 6.).min(1.);
    // Nearly opaque: the screen below must not run through the text.
    c.rect(0., 0., WIDTH, HEIGHT, alpha(color::BG, 0xF4));
    let (cx, cy) = (240., 148.);
    c.ring(cx, cy, 52., 3., fade(color::GLASS, appear));
    c.arc(cx, cy, 52., progress, fade(color::DANGER, appear));
    c.disc(cx, cy, 40., fade(0x1E2026FF, appear));
    c.icon(Icon::Power, cx, cy, 30., fade(color::TEXT, appear));
    c.text_center(
        240.,
        238. + Face::S17.mid_offset(),
        "Hold to Turn Off",
        Face::S17,
        fade(color::TEXT, appear),
    );
    c.text_center(
        240.,
        262. + Face::R13.mid_offset(),
        "Your place is saved",
        Face::R13,
        fade(color::MUTED, appear),
    );
    c.text_center(
        240.,
        306. + Face::R11.mid_offset(),
        "Release to cancel",
        Face::R11,
        fade(color::DIM, appear),
    );
    let _ = SdCard::Absent;
}

/// The three lines under the cover: title, artist, album. A station: the song
/// it announces as the title, its artist below and the station under that;
/// before a song is known, the station and its genres. Skins use the same.
pub(crate) fn lines(ui: &Ui, t: &Track) -> (String, String, String) {
    let radio = t.is_radio();
    let station = ui.radio.now.as_ref().filter(|_| radio);
    let song = ui.radio.song.as_ref().filter(|_| radio);
    match (station, song) {
        (Some(s), Some((title, artist))) => (
            title.clone(),
            if artist.is_empty() {
                s.name.clone()
            } else {
                artist.clone()
            },
            if artist.is_empty() {
                s.country.clone()
            } else {
                [s.name.clone(), s.country.clone()]
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ")
            },
        ),
        (Some(s), None) => (
            s.name.clone(),
            {
                let genres = s.genres(3).join(", ");
                if genres.is_empty() {
                    "Live".to_owned()
                } else {
                    genres
                }
            },
            [s.country.clone(), s.languages()]
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
        ),
        _ => (
            crate::track_title(t).to_owned(),
            crate::names_or_unknown(&t.artist).into_owned(),
            crate::display_or_unknown(&t.album).to_owned(),
        ),
    }
}
