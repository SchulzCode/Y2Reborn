//! Shared widgets of the UI v4 visual system: status bar, the focus row,
//! grouped cards, switches, buttons, empty states, covers, sheets and toasts.
//! One component per idea; screens do not draw their own variants.
use crate::{
    canvas::{Canvas, Icon, Moving},
    text::{self, Face},
    theme::{alpha, color, fade, mix, HEIGHT, WIDTH},
    Ui,
};
use reborn_core::{
    platform::{ChargingState, LowBattery, UsbTransfer},
    AppModel, AudioOutput, Track,
};

/// `m:ss`, or `h:mm:ss` from an hour on (audiobooks).
pub fn time(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub fn progress(position: u64, duration: u64) -> f32 {
    if duration == 0 {
        0.
    } else {
        (position as f32 / duration as f32).clamp(0., 1.)
    }
}

pub fn output_label(output: &AudioOutput) -> String {
    match output {
        AudioOutput::Wired => "Headphone Jack".into(),
        AudioOutput::Bluetooth(_) => "Bluetooth".into(),
    }
}

/// A stable identity for the cover shared by every track of one album. A song
/// without an album is its own cover (#43): songs of one folder or one artist
/// can each carry a different picture.
pub fn art_key(track: &Track) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    if track.album.is_empty() {
        feed(track.source_id.as_bytes());
        feed(track.path.to_string_lossy().as_bytes());
    } else {
        feed(track.album.as_bytes());
        feed(if track.album_artist.is_empty() {
            track.artist.as_bytes()
        } else {
            track.album_artist.as_bytes()
        });
    }
    hash | 1
}

// ---------------------------------------------------------------- status bar

/// The top strip (Beta 7.5): optional back arrow, the screen's name beside
/// it and the status cluster (clock, radios, charging, battery) on the right.
pub fn status_bar(c: &mut Canvas, ui: &Ui, m: &AppModel, title: &str, back: bool) {
    if back {
        c.icon(Icon::Back, 22., 18., 22., color::TEXT);
    }
    // The title and the status cluster: a message takes their place.
    let start = c.draw.len();
    let left = status_cluster(c, ui, m, color::MUTED, true);
    if !title.is_empty() {
        let x = if back { 44. } else { 16. };
        c.text_mid(x, 18., left - 14. - x, title, Face::S15, color::TEXT);
    }
    let end = c.draw.len();
    c.mark_moving(Moving::StatusBar { start, end });
}

/// Home's strip: the player's name and version where other screens have
/// their name.
pub fn home_bar(c: &mut Canvas, ui: &Ui, m: &AppModel) {
    let start = c.draw.len();
    let left = status_cluster(c, ui, m, color::MUTED, true);
    let x = 16.
        + c.text(
            16.,
            18. + Face::S15.mid_offset(),
            "Reborn",
            Face::S15,
            color::TEXT,
        );
    let version = crate::pages::short_version(reborn_core::VERSION);
    c.text_mid(
        x + 7.,
        18.,
        left - 14. - x - 7.,
        &version,
        Face::R13,
        color::MUTED,
    );
    let end = c.draw.len();
    c.mark_moving(Moving::StatusBar { start, end });
}

/// The clock, radios, charging and battery, right-aligned; returns the left
/// edge of what it drew. `roomy` is Reborn's own bar (Beta 7.5: larger
/// numbers and icons); skins place the compact one in their own layouts.
pub(crate) fn status_cluster(c: &mut Canvas, ui: &Ui, m: &AppModel, tint: u32, roomy: bool) -> f32 {
    let b = m.platform.battery;
    let low = matches!(
        b.level,
        LowBattery::Low | LowBattery::Critical | LowBattery::ShuttingDown
    ) || b.percent.is_some_and(|p| p <= 15);
    let fill = if low { color::DANGER } else { color::TEXT };
    // Sizes: battery, numbers, clock, icons (Wi-Fi and Bluetooth look
    // smaller than the USB plug at the same size) and their spacing.
    let (bw, bh, number, clock, icon, radio, step): (f32, f32, Face, Face, f32, f32, f32) = if roomy
    {
        (25., 13., Face::S13, Face::S15, 18., 22., 24.)
    } else {
        (22., 12., Face::S10, Face::S10, 16., 16., 20.)
    };
    let text_tint = if roomy { color::TEXT } else { tint };
    // Battery body, nub and charge level.
    let (x0, y0) = (465. - bw, (18. - bh / 2.).floor());
    c.rrect_outline(x0, y0, bw, bh, 3., 1., tint);
    c.rect(x0 + bw + 1., y0 + (bh / 2. - 2.).floor(), 2., 4., tint);
    if let Some(p) = b.percent.filter(|p| *p <= 100) {
        let w = ((bw - 4.) * f32::from(p) / 100.)
            .round()
            .max(if p > 0 { 2. } else { 0. });
        c.rect(x0 + 2., y0 + 2., w, bh - 4., fill);
    }
    let mut right = x0 - 8.;
    if let Some(p) = b.percent.filter(|p| *p <= 100) {
        let text = p.to_string();
        let w = text::width(number, &text);
        c.text(
            right - w,
            18. + number.mid_offset(),
            &text,
            number,
            if low { color::DANGER } else { text_tint },
        );
        right -= w + 8.;
    }
    if matches!(b.charging, ChargingState::Charging) {
        c.icon(Icon::Bolt, right - 5., 18., icon - 2., color::SUCCESS);
        right -= step - 4.;
    }
    // A computer has the drives (Beta 7.3: instead of a screen that covers
    // the player); the accent while files are copied.
    let usb = m.platform.snapshot.usb;
    if matches!(usb, UsbTransfer::Ready | UsbTransfer::Copying) {
        let tint = if usb == UsbTransfer::Copying {
            color::ACCENT
        } else {
            color::TEXT
        };
        c.icon(Icon::Usb, right - step / 2. + 3., 18., icon, tint);
        right -= step;
    }
    if ui.bluetooth.powered {
        let connected = ui.bluetooth.devices.iter().any(|d| d.connected);
        c.icon(
            Icon::Bluetooth,
            right - step / 2. + 3.,
            18.,
            radio,
            if connected { color::TEXT } else { color::DIM },
        );
        right -= step;
    }
    if ui.wifi.powered {
        let connected = matches!(ui.wifi.status, crate::WifiStatus::Connected(_));
        c.icon(
            Icon::Wifi,
            right - step / 2. + 3.,
            18.,
            radio,
            if connected { color::TEXT } else { color::DIM },
        );
        right -= step;
    }
    if let Some(time) = ui.clock(m) {
        let w = text::width(clock, &time);
        right -= w + 2.;
        c.text(right, 18. + clock.mid_offset(), &time, clock, text_tint);
    }
    right
}

// --------------------------------------------------------- highlights, cards

/// The focused row of every list, settings page and sheet (UI v4): neutral
/// glass. Focus never uses the accent; the accent marks state (playing,
/// checked, on).
pub fn focus_row(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32) {
    focus_row_on(c, x, y, w, h, 0);
}

/// The focus row on `layer` (0 the screen, 1 a sheet): it glides between
/// rows (Beta 7.2).
pub(crate) fn focus_row_on(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, layer: u8) {
    let start = c.draw.len();
    c.rrect(x, y, w, h, 12., FOCUS_FILL);
    c.mark_focus(start);
    let end = c.draw.len();
    c.mark_moving(Moving::Focus {
        start,
        end,
        y,
        layer,
    });
}

/// Round off a square cover (an artist) on a list row: its corners take
/// the colour under them, the focus row's or the page's, and keep doing so
/// while the focus glides past (UI motion).
pub fn round_cover(c: &mut Canvas, x: f32, y: f32, size: f32, selected: bool) {
    let start = c.draw.len();
    c.round_off(
        x,
        y,
        size,
        size / 2.,
        if selected { FOCUS_OVER_BG } else { color::BG },
    );
    let end = c.draw.len();
    c.mark_moving(Moving::Corners {
        start,
        end,
        on: FOCUS_OVER_BG,
        off: color::BG,
    });
}

/// A row's place in Select Songs: a ring, filled with a check once chosen.
pub fn select_mark(c: &mut Canvas, cx: f32, cy: f32, on: bool, accent: u32) {
    if on {
        c.disc(cx, cy, 11., accent);
        c.icon(Icon::Check, cx, cy, 16., color::TEXT);
    } else {
        c.ring(cx, cy, 11., 1.5, color::DIM);
    }
}

/// The focus row's fill, and the opaque colour it gives over `BG` (for
/// masking the corners of round artwork drawn on it).
pub const FOCUS_FILL: u32 = color::GLASS;
pub const FOCUS_OVER_BG: u32 = 0x292A2DFF; // GLASS (0x24 white) over BG

/// A grouped card behind a run of settings rows.
pub fn card(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32) {
    c.rrect(x, y, w, h, 16., color::GLASS_FAINT);
}

pub fn section_label(c: &mut Canvas, x: f32, cy: f32, label: &str) {
    c.text_tracked(
        x,
        cy + Face::S10.mid_offset(),
        &label.to_uppercase(),
        Face::S10,
        color::MUTED,
        1.4,
    );
}

pub fn toggle(c: &mut Canvas, right: f32, cy: f32, on: bool) {
    let (w, h) = (44., 26.);
    let x = right - w;
    let y = cy - h / 2.;
    let start = c.draw.len();
    c.rrect(
        x,
        y,
        w,
        h,
        13.,
        if on {
            color::ACCENT
        } else {
            color::GLASS_STRONG
        },
    );
    let knob = c.draw.len();
    c.disc(x + if on { 31. } else { 13. }, cy, 10., color::TEXT);
    let end = c.draw.len();
    // The knob slides and the track takes its colour (Beta 7.2).
    c.mark_moving(Moving::Switch {
        start,
        knob,
        end,
        on,
        x: right,
        y: cy,
    });
}

pub fn check(c: &mut Canvas, cx: f32, cy: f32, tint: u32) {
    c.icon(Icon::Check, cx, cy, 20., tint);
}

pub fn chevron(c: &mut Canvas, right: f32, cy: f32) {
    c.icon(Icon::Chevron, right - 8., cy, 16., color::DIM);
}

/// Four signal bars, `level` of them lit.
pub fn signal_bars(c: &mut Canvas, right: f32, cy: f32, level: u8) {
    for i in 0..4u8 {
        let h = 4. + f32::from(i) * 3.;
        let x = right - 22. + f32::from(i) * 6.;
        let lit = i < level;
        c.rrect(
            x,
            cy + 7. - h,
            4.,
            h,
            1.,
            if lit { color::TEXT } else { color::TRACK },
        );
    }
}

/// A rounded pill button.
#[allow(clippy::too_many_arguments)]
pub fn pill(
    c: &mut Canvas,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    label: &str,
    face: Face,
    fill: u32,
    ink: u32,
) {
    c.rrect(x, y, w, h, h / 2., fill);
    c.text_center(x + w / 2., y + h / 2. + face.mid_offset(), label, face, ink);
}

/// A menu entry (Music, Radio): a coloured icon tile, the name, what it holds
/// and a chevron, `h` tall from `y`.
pub fn menu_entry(
    c: &mut Canvas,
    row: &crate::Item,
    y: f32,
    h: f32,
    tint: u32,
    icon: Icon,
    selected: bool,
) {
    if selected {
        focus_row(c, 12., y + 2., WIDTH - 24., h - 4.);
    }
    let tile = 34.;
    icon_tile(c, 22., y + (h - tile) / 2., tile, 10., tint, icon, 20.);
    let x = 70.;
    let face = if selected { Face::S15 } else { Face::R15 };
    if row.secondary.is_empty() {
        c.text_mid(x, y + h / 2., 300., &row.label, face, color::TEXT);
    } else {
        c.text_mid(x, y + h / 2. - 7., 300., &row.label, face, color::TEXT);
        c.text_mid(
            x,
            y + h / 2. + 10.,
            330.,
            &row.secondary,
            Face::R11,
            color::MUTED,
        );
    }
    c.icon(Icon::Chevron, WIDTH - 36., y + h / 2., 16., color::DIM);
}

/// The one button of the UI (UI v4): glass when idle, white with dark text
/// when focused, in page rows, dialogs and empty states alike.
#[allow(clippy::too_many_arguments)]
pub fn button(
    c: &mut Canvas,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    label: &str,
    face: Face,
    focused: bool,
) {
    let at = c.draw.len();
    if focused {
        pill(c, x, y, w, h, label, face, BUTTON_FOCUS, color::INK);
        c.mark_focus(at);
    } else {
        pill(c, x, y, w, h, label, face, color::GLASS, color::TEXT);
    }
}

/// A focused button's fill.
pub const BUTTON_FOCUS: u32 = 0xF2F3F5FF;

/// A thin outlined tag such as `LOSSLESS 24/96`.
pub fn tag(c: &mut Canvas, x: f32, cy: f32, label: &str, tint: u32) -> f32 {
    let w = text::width_tracked(Face::S10, label, 0.5) + 20.;
    c.rrect_outline(x, cy - 10., w, 20., 10., 1., alpha(tint, 170));
    c.text_tracked(
        x + 10.,
        cy + Face::S10.mid_offset(),
        label,
        Face::S10,
        tint,
        0.5,
    );
    w
}

/// A thin horizontal track with its lit part and an optional knob.
pub fn slider(c: &mut Canvas, x: f32, cy: f32, w: f32, value: f32, lit: u32, knob: bool) {
    let h = 4.;
    c.rrect(x, cy - h / 2., w, h, 2., color::TRACK);
    let filled = (w * value.clamp(0., 1.)).round();
    c.rrect(x, cy - h / 2., filled, h, 2., lit);
    if knob {
        c.disc(x + filled, cy, 6., color::TEXT);
    }
}

/// The Now Playing progress as a wave (Beta 7.2): the played part waves,
/// the rest is the thin track. Not a perfect sine: a second, faster wave
/// rides on the first and the height breathes along the line; it tapers in
/// at the start and settles into the knob. `phase` moves it while music
/// plays; `amp` (pixels) eases to 0 when paused, which lays it flat.
///
/// Beta 8.38: it moves with the music (`levels`: louder is taller, bass
/// swells it, highs ripple it; without levels it waves as before) and is
/// drawn as one ribbon whose joints are shared, with edges that fade out
/// over a pixel: smooth without MSAA, and no beading where pieces meet.
#[allow(clippy::too_many_arguments)]
pub fn wave_progress(
    c: &mut Canvas,
    x: f32,
    cy: f32,
    w: f32,
    value: f32,
    lit: u32,
    phase: f32,
    amp: f32,
    knob: bool,
    levels: Option<crate::Levels>,
) {
    let head = x + (w * value.clamp(0., 1.)).round();
    if head < x + w {
        c.rrect(head, cy - 2., x + w - head, 4., 2., color::TRACK);
    }
    // Laid flat (paused): a clean line, as before the wave.
    if amp < 0.25 {
        if head > x {
            c.rrect(x, cy - 2., head - x, 4., 2., lit);
        }
        if knob {
            c.disc(head, cy, 6., color::TEXT);
        }
        return;
    }
    // Typical music (about -18 dB) waves as tall as before; loud passages
    // up to a third more, quiet ones calmer.
    let (scale, swell, sparkle) = match levels {
        Some(l) => (
            0.35 + 0.85 * (0.6 * l.mid + 0.4 * l.bass),
            l.bass - 0.5,
            0.08 + 0.22 * l.high,
        ),
        None => (1., 0., 0.22),
    };
    let y = |px: f32| -> f32 {
        let s = px - x;
        let k = std::f32::consts::TAU / 34.;
        let main = (k * s - phase).sin() * 0.78;
        let ripple = (2.3 * k * s + 1.7 - 1.6 * phase).sin() * sparkle;
        let breathe = 0.88 + 0.12 * (s / 57. + 0.6 * phase).sin() + 0.2 * swell;
        let smooth = |e: f32| {
            let e = e.clamp(0., 1.);
            e * e * (3. - 2. * e)
        };
        let taper = smooth(s / 10.) * smooth((head - px) / 14.);
        cy + amp * scale * taper * breathe * (main + ripple)
    };
    // The curve every 2 px, with its normal at each point.
    let mut points = vec![];
    let mut px = x;
    while px < head {
        points.push((px, y(px)));
        px += 2.;
    }
    points.push((head, y(head)));
    if points.len() < 2 {
        return;
    }
    let normal = |i: usize| {
        let (a, b) = (
            points[i.saturating_sub(1)],
            points[(i + 1).min(points.len() - 1)],
        );
        let (tx, ty) = (b.0 - a.0, b.1 - a.1);
        let len = (tx * tx + ty * ty).sqrt().max(1e-3);
        (-ty / len, tx / len)
    };
    let normals: Vec<(f32, f32)> = (0..points.len()).map(normal).collect();
    let (core, feather) = (1.3, 1.0);
    let clear = lit & !0xFF;
    let at = |i: usize, d: f32| {
        [
            points[i].0 + normals[i].0 * d,
            points[i].1 + normals[i].1 * d,
        ]
    };
    for i in 0..points.len() - 1 {
        let j = i + 1;
        // The core, then a pixel fading out on either side of it.
        for (outer, inner, colors) in [
            (core, -core, [lit; 4]),
            (core + feather, core, [clear, clear, lit, lit]),
            (-core - feather, -core, [clear, clear, lit, lit]),
        ] {
            ribbon(
                c,
                [at(i, outer), at(j, outer), at(j, inner), at(i, inner)],
                colors,
            );
        }
    }
    if knob {
        c.disc(head, cy, 6., color::TEXT);
    }
}

/// One piece of a ribbon: a quad by its corners, coloured at each corner.
fn ribbon(c: &mut Canvas, corners: [[f32; 2]; 4], colors: [u32; 4]) {
    let xs = corners.map(|p| p[0]);
    let ys = corners.map(|p| p[1]);
    let (min_x, max_x) = (
        xs.iter().copied().fold(f32::MAX, f32::min),
        xs.iter().copied().fold(f32::MIN, f32::max),
    );
    let (min_y, max_y) = (
        ys.iter().copied().fold(f32::MAX, f32::min),
        ys.iter().copied().fold(f32::MIN, f32::max),
    );
    c.draw.push(reborn_graphics::Quad {
        warp: Some(reborn_graphics::Warp { corners, colors }),
        ..reborn_graphics::Quad::rect(min_x, min_y, max_x - min_x, max_y - min_y, colors[3])
    });
}

// ------------------------------------------------------------------- covers

/// Where the cover of a track comes from for the current frame.
pub fn cover(c: &mut Canvas, ui: &Ui, track: Option<(usize, &Track)>, x: f32, y: f32, size: f32) {
    cover_rounded(c, ui, track, x, y, size, (size * 0.11).max(4.));
}

pub fn cover_rounded(
    c: &mut Canvas,
    ui: &Ui,
    track: Option<(usize, &Track)>,
    x: f32,
    y: f32,
    size: f32,
    radius: f32,
) {
    let Some((index, t)) = track else {
        placeholder(c, x, y, size, radius, 0);
        return;
    };
    let key = art_key(t);
    if ui.thumb_resident(key) {
        c.draw.push(reborn_graphics::Quad {
            tex: reborn_graphics::Tex::Thumb(key),
            ..reborn_graphics::Quad::rect(x, y, size, size, color::TEXT)
        });
    } else {
        placeholder(c, x, y, size, radius, key);
        ui.want_thumb(key, index);
    }
}

/// The calm tint of a missing or loading cover with identity `key`.
pub fn placeholder_color(key: u64) -> u32 {
    let hue = [
        0x3A4560u32,
        0x4A3A58,
        0x2F5058,
        0x58463A,
        0x3A5846,
        0x50405A,
    ];
    mix(
        hue[(key >> 8) as usize % hue.len()] << 8 | 0xFF,
        color::BG,
        0.35,
    )
}

/// A calm gradient tile standing in for a cover that is missing or loading.
pub fn placeholder(c: &mut Canvas, x: f32, y: f32, size: f32, radius: f32, key: u64) {
    c.rrect(x, y, size, size, radius, placeholder_color(key));
    if size >= 40. {
        c.icon(
            Icon::Note,
            x + size / 2.,
            y + size / 2.,
            (size * 0.34).round(),
            fade(color::TEXT, 0.35),
        );
    }
}

/// A soft shadow under a raised cover.
pub fn cover_shadow(c: &mut Canvas, x: f32, y: f32, size: f32) {
    c.glow(
        x + size / 2.,
        y + size * 0.62,
        size * 0.78,
        size * 0.72,
        0x00000090,
    );
}

// ---------------------------------------------------------- sheets and toasts

/// A dimmed backdrop for a modal sheet.
pub fn scrim(c: &mut Canvas, strong: bool) {
    c.rect(
        0.,
        0.,
        WIDTH,
        HEIGHT,
        if strong {
            color::SCRIM_STRONG
        } else {
            color::SCRIM
        },
    );
}

pub struct SheetRow<'a> {
    pub label: &'a str,
    pub detail: &'a str,
    pub enabled: bool,
}

/// A bottom sheet of actions, as used for song options. Its focused row is
/// the focus row of every list.
pub fn action_sheet(c: &mut Canvas, title: &str, subtitle: &str, rows: &[SheetRow], focus: usize) {
    let scrim_at = c.draw.len();
    scrim(c, false);
    let body = c.draw.len();
    let row_h = 31.;
    let head = if subtitle.is_empty() { 40. } else { 52. };
    let visible = rows.len().min(6);
    let h = head + visible as f32 * row_h + 14.;
    let (x, w) = (8., WIDTH - 16.);
    let y = HEIGHT - h - 8.;
    c.rrect(x, y, w, h, 18., color::SHEET);
    c.rrect_outline(x, y, w, h, 18., 1., color::GLASS);
    c.text_mid(x + 20., y + 22., w - 40., title, Face::S15, color::TEXT);
    if !subtitle.is_empty() {
        c.text_mid(x + 20., y + 40., w - 40., subtitle, Face::R11, color::MUTED);
    }
    let start = focus.saturating_sub(visible.saturating_sub(1));
    for (i, row) in rows.iter().enumerate().skip(start).take(visible) {
        let ry = y + head + (i - start) as f32 * row_h;
        let selected = i == focus && row.enabled;
        if selected {
            focus_row_on(c, x + 8., ry, w - 16., row_h - 3., 1);
        }
        let ink = if row.enabled { color::TEXT } else { color::DIM };
        let face = if selected { Face::S15 } else { Face::R15 };
        // The value takes the room the label leaves (Up Next names the whole
        // next song, #60), never running into it.
        let room = w - 44.;
        let detail = if row.detail.is_empty() {
            String::new()
        } else {
            text::fit(
                Face::R11,
                row.detail,
                (room - text::width(face, row.label) - 24.).max(room * 0.25),
            )
        };
        let label_room =
            room - text::width(Face::R11, &detail) - if detail.is_empty() { 0. } else { 16. };
        c.text_mid(
            x + 22.,
            ry + (row_h - 3.) / 2.,
            label_room,
            row.label,
            face,
            ink,
        );
        if !detail.is_empty() {
            c.text_mid_right(
                x + w - 22.,
                ry + (row_h - 3.) / 2.,
                &detail,
                Face::R11,
                color::MUTED,
            );
        }
    }
    // It rises from the bottom over its fading scrim (Beta 7.2).
    let end = c.draw.len();
    c.mark_moving(Moving::Sheet {
        scrim: scrim_at,
        start: body,
        end,
        height: h,
    });
}

/// A centred dialog with a message and a short row of choices.
pub fn dialog(
    c: &mut Canvas,
    title: &str,
    body: &str,
    rows: &[SheetRow],
    focus: usize,
    danger: bool,
) {
    let scrim_at = c.draw.len();
    scrim(c, true);
    let start = c.draw.len();
    let lines = text::wrap(Face::R13, body, 300., 4);
    let h = 74. + lines.len() as f32 * 19. + rows.len() as f32 * 38. + 10.;
    let (w, x) = (340., 70.);
    let y = ((HEIGHT - h) / 2.).max(12.);
    c.rrect(x, y, w, h, 18., color::SHEET);
    c.rrect_outline(x, y, w, h, 18., 1., color::GLASS);
    c.text_center(WIDTH / 2., y + 34., title, Face::S17, color::TEXT);
    let mut ty = y + 62.;
    for line in &lines {
        c.text_center(WIDTH / 2., ty, line, Face::R13, color::MUTED);
        ty += 19.;
    }
    let mut ry = ty + 10.;
    for (i, row) in rows.iter().enumerate() {
        let selected = i == focus && row.enabled;
        if selected && danger && i > 0 {
            // A destructive choice keeps its warning colour when focused.
            let at = c.draw.len();
            pill(
                c,
                x + 24.,
                ry,
                w - 48.,
                32.,
                row.label,
                Face::S13,
                color::DANGER,
                color::INK,
            );
            c.mark_focus(at);
        } else if row.enabled {
            button(c, x + 24., ry, w - 48., 32., row.label, Face::S13, selected);
        } else {
            pill(
                c,
                x + 24.,
                ry,
                w - 48.,
                32.,
                row.label,
                Face::S13,
                color::GLASS_FAINT,
                color::DIM,
            );
        }
        ry += 38.;
    }
    let end = c.draw.len();
    c.mark_moving(Moving::Dialog {
        scrim: scrim_at,
        start,
        end,
    });
}

/// What stands above an empty, info or error state's title.
#[derive(Clone, Copy)]
pub enum Mark {
    /// An icon in a quiet ring.
    Icon(Icon),
    /// A ring with a dot: nothing here yet.
    Empty,
    /// Work in progress: the accent spinner at `phase`.
    Busy(f32),
}

/// The one empty, info and error state (UI v4): the mark centred at `cy`, a
/// title and up to three lines below it. Returns the y under the text, where
/// a page puts its rows or `empty_button` its button.
pub fn empty_state(c: &mut Canvas, cy: f32, mark: Mark, title: &str, body: &str) -> f32 {
    match mark {
        Mark::Icon(icon) => {
            c.ring(240., cy, 30., 1.5, color::GLASS_STRONG);
            c.icon(icon, 240., cy, 26., color::DIM);
        }
        Mark::Empty => {
            c.ring(240., cy, 30., 1.5, color::GLASS_STRONG);
            c.ring(240., cy, 9., 1.5, color::GLASS_STRONG);
        }
        Mark::Busy(phase) => c.spinner(240., cy, 30., color::ACCENT, color::RING_TRACK, phase),
    }
    let mut y = cy + 50.;
    for line in text::wrap(Face::S20, title, 420., 2) {
        c.text_center(
            240.,
            y + Face::S20.mid_offset(),
            &line,
            Face::S20,
            color::TEXT,
        );
        y += 26.;
    }
    y += 2.;
    for line in text::wrap(Face::R13, body, 380., 3) {
        c.text_center(
            240.,
            y + Face::R13.mid_offset(),
            &line,
            Face::R13,
            color::MUTED,
        );
        y += 19.;
    }
    y
}

/// The single button under an empty state, centred at `y`.
pub fn empty_button(c: &mut Canvas, y: f32, label: &str, focused: bool) {
    let w = text::width(Face::S13, label) + 44.;
    button(c, (WIDTH - w) / 2., y, w, 32., label, Face::S13, focused);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Error,
}

/// A short message (Beta 7.4): a slim pill in the status bar, which gives
/// way while it shows (the back arrow stays), with an icon for what it is
/// about. It covers no content and nothing peeks out beside it.
pub fn toast(c: &mut Canvas, message: &str, tone: Tone) {
    let bar = c.moving.iter().rev().find_map(|k| match *k {
        Moving::StatusBar { start, end } => Some((start, end)),
        _ => None,
    });
    if let Some((s, e)) = bar {
        for q in &mut c.draw[s..e] {
            q.color &= 0xFFFF_FF00;
        }
    }
    let start = c.draw.len();
    // A message too long for one line takes a second one instead of being
    // cut ("Couldn't reach the update server. Try again in a m…", #55).
    let lines = text::wrap(Face::S15, message, WIDTH - 154., 2);
    let h = 10. + 20. * lines.len().max(1) as f32;
    let w = if lines.len() > 1 {
        WIDTH - 100.
    } else {
        (text::width(Face::S15, message) + 60.).min(WIDTH - 100.)
    };
    let x = (WIDTH - w) / 2.;
    let y = 3.;
    c.rrect(x, y, w, h, 15., 0x1A1D24FA);
    c.rrect_outline(x, y, w, h, 15., 1., color::GLASS);
    let (icon, tint) = toast_icon(message, tone);
    c.icon(icon, x + 22., y + h / 2., 18., tint);
    for (i, line) in lines.iter().enumerate() {
        c.text_mid(
            x + 40.,
            y + 15. + 20. * i as f32,
            w - 54.,
            line,
            Face::S15,
            color::TEXT,
        );
    }
    let end = c.draw.len();
    c.mark_moving(Moving::Toast { start, end });
}

/// The icon of a message: what it is about, or a warning for a problem.
fn toast_icon(message: &str, tone: Tone) -> (Icon, u32) {
    if tone == Tone::Error {
        return (Icon::Warning, color::DANGER);
    }
    let lower = message.to_lowercase();
    let icon = if lower.contains("computer") {
        Icon::Usb
    } else if lower.contains("bluetooth") {
        Icon::Bluetooth
    } else if lower.contains("wi-fi") {
        Icon::Wifi
    } else if lower.contains("update") {
        Icon::Download
    } else if lower.contains("favorite") {
        Icon::HeartOn
    } else if lower.contains("song") || lower.contains("music") || lower.contains("playlist") {
        Icon::Note
    } else {
        Icon::Info
    };
    (icon, color::ACCENT)
}

/// A thin position rail beside a long list.
pub fn scroll_rail(
    c: &mut Canvas,
    top: f32,
    height: f32,
    position: usize,
    count: usize,
    visible: usize,
    accent: u32,
) {
    if count <= visible {
        return;
    }
    let thumb = (height * visible as f32 / count as f32).clamp(18., height);
    let y = top + (height - thumb) * position as f32 / (count - visible).max(1) as f32;
    c.rrect(WIDTH - 14., top, 2., height, 1., color::TRACK);
    c.rrect(WIDTH - 14., y.round(), 2., thumb.round(), 1., accent);
}

/// The red of a live broadcast (the LIVE badge).
pub const LIVE_RED: u32 = 0xFF4B5CFF;

/// A station's logo, or a tile with its initials until (or unless) a logo
/// arrives. Logos are asked for here; the player fetches and decodes them.
pub fn station_logo(
    c: &mut Canvas,
    ui: &Ui,
    station: &reborn_core::RadioStation,
    x: f32,
    y: f32,
    size: f32,
) {
    if let Some(key) = station.logo_key() {
        if ui.thumb_resident(key) {
            c.draw.push(reborn_graphics::Quad {
                tex: reborn_graphics::Tex::Thumb(key),
                ..reborn_graphics::Quad::rect(x, y, size, size, color::TEXT)
            });
            return;
        }
        ui.want_logo(key, &station.favicon);
    }
    initials_tile(c, &station.name, x, y, size);
}

/// A station without a logo: its initials on a tile tinted from its name.
pub fn initials_tile(c: &mut Canvas, name: &str, x: f32, y: f32, size: f32) {
    let radius = (size * 0.11).max(4.);
    let key = reborn_core::logo_key(name);
    let tints = [
        0x3B5BDBu32,
        0xC2255C,
        0x0B7285,
        0x5F3DC4,
        0xE8590C,
        0x2B8A3E,
        0x1864AB,
        0xA61E4D,
    ];
    let tint = tints[(key >> 5) as usize % tints.len()] << 8 | 0xFF;
    c.rrect(x, y, size, size, radius, mix(tint, color::BG, 0.45));
    // A soft light from the top left gives the tile some depth.
    c.glow(
        x + size * 0.3,
        y + size * 0.25,
        size * 0.7,
        size * 0.6,
        alpha(tint, 0x60),
    );
    let initials: String = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !["the", "radio"].contains(&w.to_lowercase().as_str()))
        .filter_map(|w| w.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    let initials = if initials.is_empty() {
        name.chars().take(1).flat_map(char::to_uppercase).collect()
    } else {
        initials
    };
    let face = if size >= 120. && initials.chars().all(|c| c.is_ascii_alphanumeric()) {
        Face::S36
    } else if size >= 60. {
        Face::S25
    } else if size >= 36. {
        Face::S15
    } else {
        Face::S13
    };
    c.text_mid_center(
        x + size / 2.,
        y + size / 2.,
        &initials,
        face,
        fade(color::TEXT, 0.92),
    );
}

/// A red LIVE badge with its dot; returns its width.
pub fn live_pill(c: &mut Canvas, x: f32, cy: f32, label: &str) -> f32 {
    source_pill(c, x, cy, label, LIVE_RED, color::TEXT)
}

/// Spotify's green, for the pill that says a song streams from Spotify.
pub const SPOTIFY_GREEN: u32 = 0x1ED760FF;

/// A rounded label with a dot: where the sound comes from (LIVE, SPOTIFY).
pub fn source_pill(c: &mut Canvas, x: f32, cy: f32, label: &str, fill: u32, ink: u32) -> f32 {
    let w = text::width_tracked(Face::S10, label, 1.2) + 26.;
    c.rrect(x, cy - 9., w, 18., 9., fill);
    c.disc(x + 9.5, cy, 3., ink);
    c.text_tracked(
        x + 17.,
        cy + Face::S10.mid_offset(),
        label,
        Face::S10,
        ink,
        1.2,
    );
    w
}

/// "1 song", "12 songs", "5 124 stations": a count with its noun.
pub fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", crate::screens::count_label(n))
    }
}

/// The rows a list draws: its window, plus (with UI motion) one row above
/// and below that slide into view as it scrolls. Returns (first, count).
pub(crate) fn scroll_rows(ui: &Ui, scroll: usize, visible: usize, total: usize) -> (usize, usize) {
    if !ui.ui_motion {
        return (scroll, visible);
    }
    let from = scroll.saturating_sub(1);
    let to = (scroll + visible + 1).min(total.max(scroll + visible));
    (from, to - from)
}

/// The first row shown so the focused one is in view among `visible`.
pub fn list_window(m: &AppModel, focus: usize, visible: usize) -> usize {
    m.navigation
        .scroll
        .min(focus)
        .max((focus + 1).saturating_sub(visible))
}

/// A rounded square tinted with `tint`, `icon` in the middle in that colour
/// (Radio's categories, Playlists' New, Favorites and empty mosaics).
#[allow(clippy::too_many_arguments)]
pub fn icon_tile(
    c: &mut Canvas,
    x: f32,
    y: f32,
    size: f32,
    radius: f32,
    tint: u32,
    icon: Icon,
    icon_size: f32,
) {
    c.rrect(x, y, size, size, radius, alpha(tint, 0x30));
    c.icon(icon, x + size / 2., y + size / 2., icon_size, tint);
}

// ---------------------------------------------------------- header pages

/// An album's, a playlist's and a book's header (cover, name, Play and
/// Shuffle) stays; only the list under it scrolls, one row a step with the
/// list glide, as every list on the player (Beta 8.37).
pub const PAGE_LIST_TOP: f32 = 178.;
/// A row under the header.
pub const PAGE_ROW: f32 = 53.;
/// Rows wholly in view under the header (the next one shows its top).
pub const PAGE_ROWS: usize = 3;

/// The first row (counted from the first after the header's `lead`
/// buttons) shown under the header: kept until the focused row reaches an
/// edge. A button focused leaves the list where it is.
pub fn header_list_first(focus: usize, lead: usize, count: usize, scroll: usize) -> usize {
    let rows = count.saturating_sub(lead);
    let scroll = scroll.min(rows.saturating_sub(PAGE_ROWS));
    if focus < lead {
        return scroll;
    }
    let at = focus - lead;
    scroll.min(at).max((at + 1).saturating_sub(PAGE_ROWS))
}

/// Where row `index` (the buttons are the first `lead`) is drawn under a
/// header whose list starts at row `first`.
pub fn header_list_y(index: usize, lead: usize, first: usize) -> f32 {
    PAGE_LIST_TOP + (index as f32 - (lead + first) as f32) * PAGE_ROW
}

/// The list's rows to draw: those in view and the one peeking below, and
/// while UI motion glides them one more above.
pub fn header_list_rows(
    ui: &Ui,
    first: usize,
    lead: usize,
    count: usize,
) -> std::ops::Range<usize> {
    let from = lead + first.saturating_sub(usize::from(ui.ui_motion && first > 0));
    from..(lead + first + PAGE_ROWS + 1).min(count)
}

/// The glide of a header page's list and its rail.
pub fn header_list_motion(c: &mut Canvas, start: usize, first: usize, rows: usize, accent: u32) {
    c.mark_moving(Moving::List {
        start,
        end: c.draw.len(),
        top: PAGE_LIST_TOP,
        bottom: HEIGHT,
        row_h: PAGE_ROW,
        first,
    });
    scroll_rail(
        c,
        PAGE_LIST_TOP + 4.,
        HEIGHT - PAGE_LIST_TOP - 8.,
        first,
        rows,
        PAGE_ROWS,
        accent,
    );
}

/// The rail of a list `total` tall from `top` down, scrolled by `offset`
/// (the search's rows of differing heights).
pub fn page_rail(c: &mut Canvas, top: f32, offset: f32, total: f32, accent: u32) {
    let view = HEIGHT - top;
    if total <= view {
        return;
    }
    let thumb = (view * view / total).clamp(18., view - 12.);
    let rail_top = top + 6.;
    let rail_h = view - 12.;
    let at = rail_top + (rail_h - thumb) * (offset / (total - view)).min(1.);
    c.rrect(WIDTH - 14., rail_top, 2., rail_h, 1., color::TRACK);
    c.rrect(WIDTH - 14., at.round(), 2., thumb.round(), 1., accent);
}
