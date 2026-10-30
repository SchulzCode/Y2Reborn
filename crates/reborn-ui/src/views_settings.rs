//! Settings and its pages: grouped cards, switches, pickers and the radios
//! (the password is typed on `keyboard`).
use crate::{
    canvas::{Canvas, Icon, Moving},
    components::{self, signal_bars},
    pages::{self, Page},
    screens::{screen_title, wrap_lines},
    text::{self, Face},
    theme::{alpha, color, mix, HEIGHT, WIDTH},
    Item, RowKind, Ui, WifiStatus,
};
use reborn_core::{
    platform::{SdCard, UpdatePhase},
    AppModel, Screen, Track,
};

pub fn screen(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    let page = pages::page(ui, m, tracks);
    components::status_bar(c, ui, m, &screen_title(m), true);
    let focus = m.navigation.focus.min(page.rows.len().saturating_sub(1));
    match m.screen {
        Screen::Settings => root(c, ui, &page, focus, interactive),
        Screen::Wifi => radio(c, ui, m, &page, focus, interactive, Radio::Wifi),
        Screen::Bluetooth => radio(c, ui, m, &page, focus, interactive, Radio::Bluetooth),
        Screen::SettingsAudio => sound(c, ui, m, &page, focus, interactive),
        Screen::Output => output(c, ui, &page, focus, interactive),
        Screen::Equalizer if m.navigation.filter.is_empty() => {
            equalizer(c, ui, m, &page, focus, interactive)
        }
        Screen::Equalizer => group_page(c, ui, m, &page, focus, interactive, 38.),
        Screen::Crossfade | Screen::ScreenTimeout | Screen::HeadphoneLevel | Screen::DacFilter
            if page.hero.is_none() =>
        {
            picker(c, ui, &page, focus, interactive)
        }
        Screen::Theme => themes(c, ui, m, &page, focus, interactive),
        Screen::SettingsDisplay => display(c, ui, m, &page, focus, interactive),
        Screen::Storage => storage(c, m, &page, focus, interactive),
        Screen::Update => update(c, ui, m, &page, focus, interactive),
        Screen::ErrorReports if pages::report_preparing(m).is_some() => report_progress(c, ui, m),
        Screen::LastFm if ui.lastfm.approval.is_some() => {
            sign_in_code(c, ui, &page, focus, interactive)
        }
        _ => group_page(c, ui, m, &page, focus, interactive, 38.),
    }
}

// --------------------------------------------------------------- last.fm

const QR_DARK: u32 = 0x010101FF;
const QR_LIGHT: u32 = 0xFEFEFEFF;

/// The Last.fm sign-in: the approval address as a QR code beside the steps,
/// Cancel below.
fn sign_in_code(c: &mut Canvas, ui: &Ui, page: &Page, focus: usize, interactive: bool) {
    let Some(url) = &ui.lastfm.approval else {
        return;
    };
    qr_page(
        c,
        url,
        ui.lastfm.code.as_ref(),
        "Sign in to Last.fm",
        &[
            "1  Point your phone's",
            "   camera at the code.",
            "2  Allow Y2Reborn.",
            "3  This page continues",
            "   by itself.",
        ],
        page,
        focus,
        interactive,
    );
}

/// The Spotify sign-in: as Last.fm's, with the code to type at
/// spotify.com/pair for a phone that cannot scan.
pub fn spotify_sign_in(c: &mut Canvas, ui: &Ui, m: &AppModel, tracks: &[Track], interactive: bool) {
    let page = pages::page(ui, m, tracks);
    components::status_bar(c, ui, m, &screen_title(m), true);
    let Some(url) = &ui.spotify.approval else {
        return;
    };
    let typed = format!("   {}", ui.spotify.user_code);
    qr_page(
        c,
        url,
        ui.spotify.code.as_ref(),
        "Connect Spotify",
        &[
            "1  Point your phone's",
            "   camera at the code,",
            "   or at spotify.com/pair",
            "   type:",
            &typed,
            "2  Allow, and this page",
            "   continues by itself.",
        ],
        &page,
        m.navigation.focus.min(page.rows.len().saturating_sub(1)),
        interactive,
    );
}

#[allow(clippy::too_many_arguments)]
fn qr_page(
    c: &mut Canvas,
    url: &str,
    code: Option<&crate::qr::Code>,
    heading: &str,
    steps: &[&str],
    page: &Page,
    focus: usize,
    interactive: bool,
) {
    let code = code.cloned().or_else(|| crate::qr::encode(url.as_bytes()));
    if let Some(code) = code {
        let module = (236. / (code.size as f32 + 8.)).floor().max(2.);
        let side = module * (code.size as f32 + 8.);
        let (x0, y0) = (24., 46.);
        // Near-black and near-white: a skin's palette remap changes pure
        // black and white, and a phone needs the contrast.
        c.rrect(x0, y0, side, side, 10., QR_LIGHT);
        let inner = x0 + module * 4.;
        let top = y0 + module * 4.;
        for y in 0..code.size {
            let mut x = 0;
            while x < code.size {
                if !code.dark(x, y) {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < code.size && code.dark(x, y) {
                    x += 1;
                }
                c.rect(
                    inner + start as f32 * module,
                    top + y as f32 * module,
                    (x - start) as f32 * module,
                    module,
                    QR_DARK,
                );
            }
        }
    }
    let tx = 240.;
    let mut y = 58.;
    c.text(tx, y, heading, Face::S17, color::TEXT);
    y += 30.;
    for line in steps {
        c.text(tx, y, line, Face::R13, color::MUTED);
        y += 20.;
    }
    let selected = interactive && focus == 0;
    if let Some(cancel) = page.rows.first() {
        let by = HEIGHT - 66.;
        if selected {
            components::focus_row(c, 14., by, WIDTH - 28., 44.);
        }
        c.text_mid(
            30.,
            by + 22.,
            200.,
            &cancel.label,
            if selected { Face::S17 } else { Face::R17 },
            color::TEXT,
        );
    }
}

// ------------------------------------------------------------------ root

fn root(c: &mut Canvas, ui: &Ui, page: &Page, focus: usize, interactive: bool) {
    let _ = ui;
    // Every place fits without scrolling: rows shrink from 44 px as places
    // are added (Beta 7.5: Skin, then Last.fm; Beta 8.18: Customize).
    let rows = page.rows.len().max(1);
    let row_h = ((HEIGHT - 46.) / rows as f32).floor().min(44.);
    let tile = |key: &str| match key {
        "bluetooth" => (0x3E82FFFF, Icon::Bluetooth),
        "wifi" => (0x3CBE78FF, Icon::Wifi),
        "audio" => (0xFF6E5AFF, Icon::Speaker),
        "display" => (0xFFBE3CFF, Icon::Sun),
        "theme" => (0xEE6AB4FF, Icon::Palette),
        "customize" => (0x2EB8C8FF, Icon::Sliders),
        "storage" => (0x9678FFFF, Icon::Drive),
        "lastfm" => (0xD51007FF, Icon::Note),
        _ => (0x828CA0FF, Icon::Info),
    };
    components::card(c, 10., 38., WIDTH - 20., rows as f32 * row_h + 4.);
    for (i, row) in page.rows.iter().enumerate().take(rows) {
        let y = 40. + i as f32 * row_h;
        let mid = y + row_h / 2.;
        let selected = interactive && i == focus;
        if selected {
            components::focus_row(c, 14., y + 3., WIDTH - 28., row_h - 6.);
        } else if i > 0 && !(interactive && i - 1 == focus) {
            c.rect(64., y + 1., WIDTH - 88., 1., color::RULE);
        }
        let (tint, icon) = tile(&row.key);
        c.rrect(24., mid - 13., 26., 26., 7., tint);
        c.icon(icon, 37., mid, 16., color::TEXT);
        let face = if selected { Face::S17 } else { Face::R17 };
        c.text_mid(64., mid, 230., &row.label, face, color::TEXT);
        c.text_mid_right(
            WIDTH - 42.,
            mid,
            &text::fit(Face::R11, &row.secondary, 150.),
            Face::R11,
            color::MUTED,
        );
        components::chevron(c, WIDTH - 22., mid);
    }
}

// ----------------------------------------------------------- grouped rows

/// Draw `rows` as one grouped card starting at `top`; returns its bottom edge.
/// `first` is the index of the first row in the page, for focus.
fn group(
    c: &mut Canvas,
    ui: &Ui,
    rows: &[Item],
    first: usize,
    focus: usize,
    interactive: bool,
    top: f32,
) -> f32 {
    let accent = ui.accent();
    let h = 50.;
    components::card(c, 10., top, WIDTH - 20., rows.len() as f32 * h + 4.);
    for (i, row) in rows.iter().enumerate() {
        let y = top + 2. + i as f32 * h;
        let selected = interactive && first + i == focus && row.enabled;
        if selected {
            components::focus_row(c, 14., y + 2., WIDTH - 28., h - 4.);
        } else if i + 1 < rows.len() && !(interactive && first + i + 1 == focus) {
            c.rect(28., y + h - 1., WIDTH - 52., 1., color::RULE);
        }
        let face = if selected { Face::S15 } else { Face::R15 };
        let tint = if row.enabled { color::TEXT } else { color::DIM };
        let cy = y + h / 2.;
        match row.kind {
            RowKind::Toggle(on) => {
                c.text_mid(28., cy, 300., &row.label, face, tint);
                components::toggle(c, WIDTH - 24., cy, on);
            }
            RowKind::Check(on) => {
                // A short value (a zone's region and offset) left of the check.
                let value = text::fit(Face::R11, &row.secondary, 150.);
                let mut room = 340.;
                if !value.is_empty() {
                    room -= text::width(Face::R11, &value) + 16.;
                    c.text_mid_right(WIDTH - 52., cy, &value, Face::R11, color::MUTED);
                }
                c.text_mid(28., cy, room, &row.label, face, tint);
                if on {
                    components::check(c, WIDTH - 32., cy, accent);
                }
            }
            RowKind::Chevron | RowKind::Plain => {
                let value = text::fit(Face::R11, &row.secondary, 170.);
                let right = if row.kind == RowKind::Chevron {
                    WIDTH - 40.
                } else {
                    WIDTH - 28.
                };
                let reserve = text::width(Face::R11, &value) + 16.;
                c.text_mid(28., cy, right - 28. - reserve, &row.label, face, tint);
                c.text_mid_right(right, cy, &value, Face::R11, color::MUTED);
                if row.kind == RowKind::Chevron {
                    components::chevron(c, WIDTH - 22., cy);
                }
            }
        }
    }
    top + rows.len() as f32 * h + 4.
}

fn hint(c: &mut Canvas, page: &Page) {
    if let Some(status) = &page.status {
        let lines = wrap_lines(status, 420., Face::R11, 2);
        let mut y = HEIGHT - 12. - 15. * lines.len() as f32;
        for line in lines {
            c.text_center(
                240.,
                y + Face::R11.mid_offset(),
                &line,
                Face::R11,
                color::DIM,
            );
            y += 15.;
        }
    }
}

/// A plain page: optional empty or info state, then grouped rows.
pub(crate) fn group_page(
    c: &mut Canvas,
    ui: &Ui,
    m: &AppModel,
    page: &Page,
    focus: usize,
    interactive: bool,
    top: f32,
) {
    let mut y = top;
    if let Some(h) = &page.hero {
        y = components::empty_state(c, y + 58., hero_mark(m), &h.title, &h.body) + 10.;
    }
    for (label, value) in &page.facts {
        c.text_mid(28., y + 12., 150., label, Face::R13, color::MUTED);
        c.text_mid_right(
            WIDTH - 28.,
            y + 12.,
            &text::fit(Face::R13, value, 270.),
            Face::R13,
            color::TEXT,
        );
        y += 30.;
    }
    if !page.rows.is_empty() {
        // As every list (8.37): the rows keep their place until the focus
        // reaches an edge, then glide one row (it moved on every step back).
        let visible = (((HEIGHT - 40. - y) / 50.).floor() as usize).max(1);
        let start = ui.group_window(m, focus, visible, page.rows.len());
        let end = (start + visible).min(page.rows.len());
        let rows_at = c.draw.len();
        group(c, ui, &page.rows[start..end], start, focus, interactive, y);
        c.mark_moving(Moving::List {
            start: rows_at,
            end: c.draw.len(),
            top: y,
            bottom: y + visible as f32 * 50. + 4.,
            row_h: 50.,
            first: start,
        });
    }
    hint(c, page);
}

/// The mark of a page's empty or info state: the screen's own icon.
pub(crate) fn hero_mark(m: &AppModel) -> components::Mark {
    use components::Mark;
    match m.screen {
        Screen::Wifi => Mark::Icon(Icon::Wifi),
        Screen::Bluetooth => Mark::Icon(Icon::Bluetooth),
        Screen::PcTransfer => Mark::Icon(Icon::Usb),
        Screen::NowPlaying => Mark::Icon(Icon::Note),
        Screen::Battery => Mark::Icon(Icon::Bolt),
        Screen::Update | Screen::About => Mark::Icon(Icon::Download),
        Screen::Storage | Screen::SettingsLibrary => Mark::Icon(Icon::Drive),
        Screen::Radio | Screen::RadioStations | Screen::RadioCategories | Screen::RadioInfo => {
            Mark::Icon(Icon::Broadcast)
        }
        _ => Mark::Icon(Icon::Info),
    }
}

/// A headline and explanation, left-aligned; returns the y below them.
pub fn message(c: &mut Canvas, title: &str, body: &str, y: f32) -> f32 {
    let mut cy = y + 14.;
    for line in text::wrap(Face::S20, title, 432., 2) {
        c.text(
            24.,
            cy + Face::S20.mid_offset(),
            &line,
            Face::S20,
            color::TEXT,
        );
        cy += 26.;
    }
    if !body.is_empty() {
        cy += 2.;
        for line in text::wrap(Face::R13, body, 432., 3) {
            c.text(
                24.,
                cy + Face::R13.mid_offset(),
                &line,
                Face::R13,
                color::MUTED,
            );
            cy += 19.;
        }
    }
    cy
}

// ----------------------------------------------------------------- radios

#[derive(Clone, Copy, PartialEq, Eq)]
enum Radio {
    Wifi,
    Bluetooth,
}

fn radio(
    c: &mut Canvas,
    ui: &Ui,
    m: &AppModel,
    page: &Page,
    focus: usize,
    interactive: bool,
    kind: Radio,
) {
    let accent = ui.accent();
    let Some(switch) = page
        .rows
        .first()
        .filter(|r| matches!(r.kind, RowKind::Toggle(_)))
    else {
        // Not available yet: a message only.
        group_page(c, ui, m, page, focus, interactive, 38.);
        return;
    };
    group(
        c,
        ui,
        std::slice::from_ref(switch),
        0,
        focus,
        interactive,
        38.,
    );
    if let Some(h) = page.hero.as_ref() {
        // Off: a calm empty state under the switch.
        let icon = if kind == Radio::Wifi {
            Icon::Wifi
        } else {
            Icon::Bluetooth
        };
        components::empty_state(c, 160., components::Mark::Icon(icon), &h.title, &h.body);
        return;
    }
    let busy = ui.bluetooth_busy().filter(|_| kind == Radio::Bluetooth);
    let top = if busy.is_some() { 164. } else { 100. };
    if let Some((name, operation)) = busy {
        let verb = if operation == "pair" {
            "Pairing with"
        } else {
            "Connecting to"
        };
        c.rrect(10., 100., WIDTH - 20., 56., 14., alpha(accent, 34));
        c.rrect_outline(10., 100., WIDTH - 20., 56., 14., 1., alpha(accent, 110));
        c.spinner(36., 128., 12., accent, color::RING_TRACK, ui.spin_phase());
        c.text_mid(
            60.,
            120.,
            WIDTH - 90.,
            &format!("{verb} {name}…"),
            Face::S15,
            color::TEXT,
        );
        c.text_mid(
            60.,
            140.,
            WIDTH - 90.,
            "Keep the headphones close.",
            Face::R11,
            color::MUTED,
        );
    }
    let rest: Vec<(usize, &Item)> = page.rows.iter().enumerate().skip(1).collect();
    let visible = if busy.is_some() { 3usize } else { 4usize };
    let start = focus
        .saturating_sub(visible)
        .min(rest.len().saturating_sub(visible));
    // Devices and networks are a settings list: one grouped card.
    let shown = rest.len().saturating_sub(start).min(visible);
    if shown > 0 {
        components::card(c, 10., top - 2., WIDTH - 20., shown as f32 * 56.);
    }
    for (slot, (index, row)) in rest.iter().skip(start).take(visible).enumerate() {
        let y = top + slot as f32 * 56.;
        let selected = interactive && *index == focus && row.enabled;
        if selected {
            components::focus_row(c, 14., y + 1., WIDTH - 28., 50.);
        } else if slot > 0 && !(interactive && *index == focus + 1) {
            c.rect(28., y - 2., WIDTH - 56., 1., color::RULE);
        }
        let connected = row.secondary.starts_with("Connected");
        let is_action = row.key == "wifi_scan" || row.key == "bt_scan";
        let face = if selected { Face::S15 } else { Face::R15 };
        if is_action {
            c.text_mid(
                28.,
                y + 26.,
                380.,
                &row.label,
                face,
                if row.enabled {
                    color::TEXT
                } else {
                    color::MUTED
                },
            );
            continue;
        }
        c.text_mid(
            28.,
            y + 19.,
            300.,
            &row.label,
            face,
            if row.enabled { color::TEXT } else { color::DIM },
        );
        let sub_color = if connected { accent } else { color::MUTED };
        c.text_mid(28., y + 38., 330., &row.secondary, Face::R11, sub_color);
        if kind == Radio::Wifi {
            if let Some(n) = ui.wifi.networks.iter().find(|n| n.ssid == row.label) {
                signal_bars(c, WIDTH - 24., y + 26., n.bars.min(4));
                if n.secured == Some(true) {
                    c.icon(Icon::Lock, WIDTH - 56., y + 26., 14., color::MUTED);
                }
            }
        } else if connected {
            c.icon(Icon::Check, WIDTH - 34., y + 26., 18., accent);
        }
    }
    let _ = (m, WifiStatus::Disconnected);
    if let Some(status) = &page.status {
        let lines = wrap_lines(status, 430., Face::R11, 2);
        for (i, line) in lines.iter().enumerate() {
            c.text_center(
                240.,
                332. + i as f32 * 15. + Face::R11.mid_offset(),
                line,
                Face::R11,
                color::DIM,
            );
        }
    }
}

// -------------------------------------------------------------- sound etc.

fn sound(c: &mut Canvas, ui: &Ui, m: &AppModel, page: &Page, focus: usize, interactive: bool) {
    // Rows 0–4 playback and balance, 5 the output card (its page holds the
    // jack's level and DAC filter). One row taller than the screen: as on
    // Display, the first row scrolls away once the focus reaches Balance.
    let card = page.rows.len().saturating_sub(1);
    let fits = 38. + card as f32 * 50. + 4. + 38. + 56. + 4. <= HEIGHT;
    let first = usize::from(!fits && focus + 2 > card);
    let bottom = group(
        c,
        ui,
        &page.rows[first..card],
        first,
        focus,
        interactive,
        38.,
    );
    components::section_label(c, 28., bottom + 24., "Output");
    if let Some(row) = page.rows.get(card) {
        let y = bottom + 38.;
        if interactive && focus == card {
            components::focus_row(c, 10., y, WIDTH - 20., 56.);
        } else {
            components::card(c, 10., y, WIDTH - 20., 56.);
        }
        c.text_mid(28., y + 21., 340., &row.secondary, Face::S15, color::TEXT);
        let detail = match &m.output {
            reborn_core::AudioOutput::Wired => pages::jack_summary(m),
            reborn_core::AudioOutput::Bluetooth(address) => ui
                .bluetooth
                .devices
                .iter()
                .find(|d| &d.address == address)
                .and_then(|d| d.codec.clone())
                .map(|codec| format!("{codec}  ·  Bluetooth"))
                .unwrap_or_else(|| "Bluetooth".into()),
        };
        c.text_mid(28., y + 40., 340., &detail, Face::R11, color::MUTED);
        components::chevron(c, WIDTH - 22., y + 28.);
    }
}

/// The Output page: the outputs (current one checked), then the headphone
/// jack's level and DAC filter. With many Bluetooth outputs the jack group
/// does not fit under them: once the focus reaches it the outputs scroll
/// away, as on Display.
fn output(c: &mut Canvas, ui: &Ui, page: &Page, focus: usize, interactive: bool) {
    let jack = page.rows.len().saturating_sub(2);
    let fits = 38. + jack as f32 * 50. + 4. + 34. + 104. <= HEIGHT - 4.;
    let top = if fits || focus < jack {
        group(c, ui, &page.rows[..jack], 0, focus, interactive, 38.) + 10.
    } else {
        28.
    };
    if fits || focus >= jack {
        components::section_label(c, 28., top + 14., "Headphone Jack");
        group(
            c,
            ui,
            &page.rows[jack..],
            jack,
            focus,
            interactive,
            top + 28.,
        );
    }
}

const THEME_H: f32 = 50.;
const THEME_ROWS: usize = 6;

/// The theme picker: each look's colours as a swatch, its name and a check;
/// how to add skins follows the last one.
fn themes(c: &mut Canvas, ui: &Ui, m: &AppModel, page: &Page, focus: usize, interactive: bool) {
    let accent = ui.accent();
    let first = components::list_window(m, focus, THEME_ROWS);
    for (slot, row) in page.rows.iter().skip(first).take(THEME_ROWS).enumerate() {
        let i = first + slot;
        let y = 40. + slot as f32 * THEME_H;
        let cy = y + THEME_H / 2.;
        if interactive && i == focus && row.enabled {
            components::focus_row(c, 10., y + 1., WIDTH - 24., THEME_H - 2.);
        }
        let id = row.key.strip_prefix("theme_set:").unwrap_or("");
        let (bg, tint) = if id.is_empty() {
            (color::BG, color::ACCENT)
        } else {
            ui.themes
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.swatch)
                .unwrap_or((color::GLASS_STRONG, color::MUTED))
        };
        // The look's background with its accent: a small picture of it.
        c.rrect(22., cy - 15., 30., 30., 8., bg);
        c.rrect_outline(22., cy - 15., 30., 30., 8., 1., color::GLASS);
        c.rrect(28., cy + 3., 18., 5., 2.5, tint);
        let face = if interactive && i == focus {
            Face::S15
        } else {
            Face::R15
        };
        let ink = if row.enabled { color::TEXT } else { color::DIM };
        if row.secondary.is_empty() {
            c.text_mid(66., cy, 340., &row.label, face, ink);
        } else {
            c.text_mid(66., cy - 8., 340., &row.label, face, ink);
            c.text_mid(66., cy + 10., 340., &row.secondary, Face::R11, color::MUTED);
        }
        if row.kind == RowKind::Check(true) {
            components::check(c, WIDTH - 40., cy, accent);
        }
    }
    let shown = page.rows.len().saturating_sub(first).min(THEME_ROWS);
    let below = 40. + shown as f32 * THEME_H;
    if first + shown == page.rows.len() && below < HEIGHT - 30. {
        if let Some(status) = &page.status {
            for (n, line) in wrap_lines(status, 400., Face::R11, 2).iter().enumerate() {
                c.text_center(
                    240.,
                    below + 20. + n as f32 * 15.,
                    line,
                    Face::R11,
                    color::DIM,
                );
            }
        }
    }
    if page.rows.len() > THEME_ROWS {
        components::scroll_rail(c, 44., 286., first, page.rows.len(), THEME_ROWS, accent);
    }
}

fn picker(c: &mut Canvas, ui: &Ui, page: &Page, focus: usize, interactive: bool) {
    let accent = ui.accent();
    let pitch = if page.rows.len() > 5 { 43. } else { 47. };
    for (i, row) in page.rows.iter().enumerate() {
        let y = 38. + i as f32 * pitch;
        let selected = interactive && i == focus;
        if selected {
            components::focus_row(c, 10., y, WIDTH - 20., pitch - 4.);
        }
        let face = if selected { Face::S15 } else { Face::R15 };
        c.text_mid(
            26.,
            y + (pitch - 4.) / 2.,
            340.,
            &row.label,
            face,
            color::TEXT,
        );
        if row.kind == RowKind::Check(true) {
            components::check(c, WIDTH - 32., y + (pitch - 4.) / 2., accent);
        }
    }
    hint(c, page);
}

fn equalizer(c: &mut Canvas, ui: &Ui, m: &AppModel, page: &Page, focus: usize, interactive: bool) {
    let accent = ui.accent();
    // Six rows (presets, Custom, Headphone Profile) above the curve card.
    let pitch = 38.;
    for (i, row) in page.rows.iter().enumerate() {
        let y = 38. + i as f32 * pitch;
        let selected = interactive && i == focus;
        if selected {
            components::focus_row(c, 10., y, WIDTH - 20., pitch - 4.);
        }
        let face = if selected { Face::S15 } else { Face::R15 };
        let mid = y + (pitch - 4.) / 2.;
        c.text_mid(26., mid, 330., &row.label, face, color::TEXT);
        match row.kind {
            RowKind::Check(true) => components::check(c, WIDTH - 32., mid, accent),
            RowKind::Chevron => components::chevron(c, WIDTH - 22., mid),
            _ => {}
        }
    }
    // A live curve of the focused preset (or the current settings); the
    // headphone profile shows its filters' response (without the preamp, as
    // AutoEQ draws it) at the five band frequencies.
    let focused = page.rows.get(focus).map(|r| r.key.as_str()).unwrap_or("");
    let profile = m.settings.eq_profile.as_ref().filter(|_| {
        focused == "eq_profile" || (m.settings.eq_profile_on && !focused.starts_with("eq_preset:"))
    });
    let gains: [f32; 5] = match profile {
        Some(p) => reborn_core::EQ_FREQUENCIES.map(|hz| {
            reborn_core::eq::response_db(&p.bands, f64::from(hz), 48_000.).clamp(-12., 12.) as f32
        }),
        None => pages::EQ_PRESETS
            .iter()
            .find(|(_, key, _)| focused == format!("eq_preset:{key}"))
            .map(|(_, _, g)| *g)
            .unwrap_or_else(|| pages::eq_gains(m))
            .map(f32::from),
    };
    components::card(c, 10., 266., WIDTH - 20., 76.);
    let xs: Vec<f32> = (0..5)
        .map(|i| 34. + i as f32 * ((WIDTH - 68.) / 4.))
        .collect();
    let ys: Vec<f32> = gains.iter().map(|g| 304. - g * 2.6).collect();
    for x in &xs {
        c.rect(x.round(), 276., 1., 56., color::RULE);
    }
    for i in 0..4 {
        line(c, xs[i], ys[i], xs[i + 1], ys[i + 1], accent);
    }
    for (x, y) in xs.iter().zip(&ys) {
        c.disc(*x, *y, 4., color::TEXT);
    }
}

/// A 2 px line between two points, as short axis-aligned steps.
fn line(c: &mut Canvas, x0: f32, y0: f32, x1: f32, y1: f32, tint: u32) {
    let steps = ((x1 - x0).abs()).ceil() as usize;
    for s in 0..steps {
        let t = s as f32 / steps as f32;
        let (x, y) = (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
        c.rect(x.round(), (y - 1.).round(), 2., 2.5, tint);
    }
}

fn display(c: &mut Canvas, ui: &Ui, m: &AppModel, page: &Page, focus: usize, interactive: bool) {
    let mut index = 0;
    let mut y = 38.;
    // The brightness card stays; the rows under it scroll as every list
    // (8.37: the card vanished once the focus passed the rows under it).
    if m.platform.brightness_available {
        components::card(c, 10., y, WIDTH - 20., 78.);
        if interactive && focus == 0 {
            components::focus_row(c, 14., y + 4., WIDTH - 28., 70.);
        }
        c.text_mid(28., y + 24., 250., "Brightness", Face::S15, color::TEXT);
        c.text_mid_right(
            WIDTH - 28.,
            y + 24.,
            &format!("{}%", m.settings.brightness),
            Face::R11,
            color::MUTED,
        );
        components::slider(
            c,
            28.,
            y + 54.,
            WIDTH - 56.,
            f32::from(m.settings.brightness) / 100.,
            color::TEXT,
            true,
        );
        index = 1;
        y += 92.;
    }
    let rows = page.rows.len().saturating_sub(index);
    if rows == 0 {
        return;
    }
    let visible = (((HEIGHT - 8. - y) / 50.).floor() as usize).clamp(1, rows);
    let start = ui.group_window(m, focus.saturating_sub(index), visible, rows);
    let rows_at = c.draw.len();
    let from = index + start;
    group(
        c,
        ui,
        &page.rows[from..from + visible],
        from,
        focus,
        interactive,
        y,
    );
    c.mark_moving(Moving::List {
        start: rows_at,
        end: c.draw.len(),
        top: y,
        bottom: y + visible as f32 * 50. + 4.,
        row_h: 50.,
        first: start,
    });
    components::scroll_rail(
        c,
        y + 4.,
        visible as f32 * 50. - 4.,
        start,
        rows,
        visible,
        ui.accent(),
    );
}

fn storage(c: &mut Canvas, m: &AppModel, page: &Page, focus: usize, interactive: bool) {
    let snapshot = &m.platform.snapshot;
    let volumes = [
        (
            "Internal Storage",
            // Not set up yet (STORAGE-SETUP): no bar for the small Y2DATA.
            internal_note(snapshot.internal_setup)
                .is_none()
                .then_some(&snapshot.storage.internal),
            0x6EA0FFFFu32,
        ),
        (
            "SD Card",
            match &snapshot.storage.sd {
                SdCard::Ready(space) | SdCard::ReadOnly(space) | SdCard::Unchecked(space) => {
                    Some(space)
                }
                _ => None,
            },
            0x3CC878FF,
        ),
    ];
    for (i, (name, space, tint)) in volumes.into_iter().enumerate() {
        let y = 40. + i as f32 * 84.;
        components::card(c, 10., y, WIDTH - 20., 76.);
        c.text_mid(28., y + 22., 250., name, Face::S15, color::TEXT);
        match space {
            Some(space) => {
                let free = space
                    .free_bytes
                    .map(|f| format!("{} free", reborn_core::platform::human_bytes(f)))
                    .unwrap_or_default();
                c.text_mid_right(WIDTH - 28., y + 22., &free, Face::R11, color::MUTED);
                let used = match (space.free_bytes, space.total_bytes) {
                    (Some(f), Some(t)) if t > 0 => 1. - f as f32 / t as f32,
                    _ => 0.,
                };
                c.rrect(28., y + 42., WIDTH - 56., 8., 4., color::GLASS_STRONG);
                c.rrect(
                    28.,
                    y + 42.,
                    ((WIDTH - 56.) * used.clamp(0., 1.)).max(8.),
                    8.,
                    4.,
                    tint,
                );
                if let Some(t) = space.total_bytes {
                    c.text_mid(
                        28.,
                        y + 62.,
                        250.,
                        &format!("{} total", reborn_core::platform::human_bytes(t)),
                        Face::R11,
                        color::MUTED,
                    );
                }
            }
            None => {
                let note = if i == 0 {
                    internal_note(snapshot.internal_setup).unwrap_or("Checking…")
                } else if matches!(snapshot.storage.sd, SdCard::Error) && i == 1 {
                    "Card can't be read"
                } else if matches!(snapshot.storage.sd, SdCard::NeedsRepair) && i == 1 {
                    "Needs repair · Check it on a computer"
                } else if matches!(snapshot.storage.sd, SdCard::CheckTimedOut) && i == 1 {
                    "Check took too long · Insert it again"
                } else if i == 1 {
                    "No SD card"
                } else {
                    "Checking…"
                };
                c.text_mid(28., y + 50., 300., note, Face::R13, color::MUTED);
            }
        }
    }
    let top = 40. + 2. * 84.;
    components::card(c, 10., top, WIDTH - 20., 44. * page.rows.len() as f32 + 4.);
    for (i, row) in page.rows.iter().enumerate() {
        let y = top + 2. + i as f32 * 44.;
        if interactive && i == focus && row.enabled {
            components::focus_row(c, 14., y + 1., WIDTH - 28., 42.);
        }
        let face = if interactive && i == focus {
            Face::S15
        } else {
            Face::R15
        };
        c.text_mid(
            28.,
            y + 22.,
            280.,
            &row.label,
            face,
            if row.enabled { color::TEXT } else { color::DIM },
        );
        let value = text::fit(Face::R11, &row.secondary, 190.);
        c.text_mid_right(WIDTH - 40., y + 22., &value, Face::R11, color::MUTED);
        components::chevron(c, WIDTH - 22., y + 22.);
    }
    let _ = HEIGHT;
}

/// Where the update is: Download, Check, Install (the restart). Steps done
/// carry a check, the current one the accent, the rest wait dimmed.
pub(crate) fn steps(c: &mut Canvas, current: usize, accent: u32) {
    named_steps(c, ["Download", "Check", "Install"], current, accent);
}

fn named_steps(c: &mut Canvas, names: [&str; 3], current: usize, accent: u32) {
    let (y, gap) = (64., 120.);
    let x0 = 240. - gap;
    for i in 0..2 {
        let tint = if i < current {
            color::TEXT
        } else {
            color::RULE
        };
        c.rect(x0 + i as f32 * gap + 12., y - 1., gap - 24., 2., tint);
    }
    for (i, name) in names.iter().enumerate() {
        let x = x0 + i as f32 * gap;
        let (dot, label) = match i.cmp(&current) {
            std::cmp::Ordering::Less => (color::TEXT, color::MUTED),
            std::cmp::Ordering::Equal => (accent, color::TEXT),
            std::cmp::Ordering::Greater => (color::GLASS_STRONG, color::DIM),
        };
        c.disc(x, y, 7., dot);
        if i < current {
            c.icon(Icon::Check, x, y, 11., color::INK);
        }
        let face = if i == current { Face::S13 } else { Face::R13 };
        c.text_center(x, y + 22. + face.mid_offset(), name, face, label);
    }
}

/// The running download (OTA-48): which step, a bar that glides between the
/// platform's reports, how much, how fast and how long. Before the first
/// report a short segment sweeps the track.
fn download_progress(
    c: &mut Canvas,
    ui: &Ui,
    live: Option<reborn_core::platform::DownloadLive>,
    accent: u32,
    cancel: bool,
) {
    let verifying = live.is_some_and(|l| l.verifying);
    // Step 3 is the restart to install (`views_system::installing`).
    let title = if verifying {
        "Checking the Update"
    } else {
        "Downloading Update"
    };
    steps(c, usize::from(verifying), accent);
    c.text_center(
        240.,
        128. + Face::S20.mid_offset(),
        title,
        Face::S20,
        color::TEXT,
    );
    let (x, y, w, h) = (60., 208., 360., 10.);
    c.rrect(x, y, w, h, h / 2., color::TRACK);
    match live {
        Some(l) => {
            let fill = ui.download_fill(l.fraction(), verifying);
            // The number counts with the bar instead of jumping with each
            // report; it reads the report once the bar has caught up.
            let shown = if l.fraction() - fill < 0.004 {
                l.fraction()
            } else {
                fill
            };
            let percent = (shown * 100.).floor() as u32;
            c.text_center(
                240.,
                172. + Face::S36.mid_offset(),
                &format!("{percent}%"),
                Face::S36,
                color::TEXT,
            );
            let lit = (w * fill).round();
            if lit >= h {
                c.rrect(x, y, lit, h, h / 2., accent);
                // A bright head where the bar grows.
                c.glow(x + lit - h / 2., y + h / 2., 14., 9., alpha(accent, 150));
                c.disc(
                    x + lit - h / 2.,
                    y + h / 2.,
                    h / 2. - 1.,
                    mix(accent, color::TEXT, 0.55),
                );
                // A soft highlight runs along the filled part: it is alive.
                // It fades in and out at the ends instead of jumping back.
                if lit > 48. {
                    let t = ui.spin_phase();
                    let sx = x + 20. + (lit - 40.) * t;
                    let a = (std::f32::consts::PI * t).sin();
                    c.glow(sx, y + h / 2., 22., 5., alpha(0xFFFFFFFF, (110. * a) as u8));
                }
            }
            let (detail, hint) = if verifying {
                (
                    "Making sure the download is complete and genuine.".to_owned(),
                    l.seconds_left
                        .map(time_left)
                        .unwrap_or_else(|| "This takes a few seconds.".to_owned()),
                )
            } else {
                (
                    match l.speed_bytes_per_s {
                        Some(speed) => format!(
                            "{} of {} MB · {}",
                            megabytes(l.bytes_done),
                            megabytes(l.bytes_total),
                            rate(speed)
                        ),
                        None => format!(
                            "{} of {} MB",
                            megabytes(l.bytes_done),
                            megabytes(l.bytes_total)
                        ),
                    },
                    l.seconds_left.map(time_left).unwrap_or_default(),
                )
            };
            c.text_center(
                240.,
                242. + Face::R13.mid_offset(),
                &detail,
                Face::R13,
                color::MUTED,
            );
            c.text_center(
                240.,
                261. + Face::R13.mid_offset(),
                &hint,
                Face::R13,
                color::DIM,
            );
        }
        None => {
            // Connecting: no size yet.
            let seg = 72.;
            // Back and forth, easing at each end.
            let t = ui.spin_phase();
            let swing = 0.5 - 0.5 * (std::f32::consts::TAU * t).cos();
            c.rrect(x + (w - seg) * swing, y, seg, h, h / 2., accent);
            c.text_center(
                240.,
                172. + Face::S20.mid_offset(),
                "Connecting…",
                Face::S20,
                color::MUTED,
            );
            c.text_center(
                240.,
                242. + Face::R13.mid_offset(),
                "Keep Wi-Fi on.",
                Face::R13,
                color::MUTED,
            );
        }
    }
    if cancel {
        // The only control while the update runs: it is always the focus.
        components::empty_button(c, 300., "Cancel", true);
    }
}

/// *Send Report Now* or *Save Report* running (Y2Linux 1.0 Beta 8.10): the
/// same steps, number and gliding bar as the update download, fed by the
/// platform's live record. Before its first record a segment sweeps the track.
fn report_progress(c: &mut Canvas, ui: &Ui, m: &AppModel) {
    use reborn_core::platform::ReportStep;
    let accent = ui.accent();
    let saving = match m.platform.busy {
        Some(reborn_core::PlatformTask::ReportSave(to)) => Some(to),
        _ => None,
    };
    let live = m.platform.report;
    let step = live.map_or(0, |l| match l.step {
        ReportStep::Collect => 0,
        ReportStep::Clean => 1,
        ReportStep::Finish => 2,
    });
    named_steps(
        c,
        [
            "Collect",
            "Clean",
            if saving.is_some() { "Save" } else { "Send" },
        ],
        step,
        accent,
    );
    let title = match (step, saving) {
        (0, _) => "Collecting Logs".to_owned(),
        (1, _) => "Removing Personal Details".to_owned(),
        (_, Some(to)) => format!("Saving to {}", to.label()),
        (_, None) => "Sending the Report".to_owned(),
    };
    c.text_center(
        240.,
        128. + Face::S20.mid_offset(),
        &title,
        Face::S20,
        color::TEXT,
    );
    let (x, y, w, h) = (60., 208., 360., 10.);
    c.rrect(x, y, w, h, h / 2., color::TRACK);
    let Some(l) = live else {
        let seg = 72.;
        let t = ui.spin_phase();
        let swing = 0.5 - 0.5 * (std::f32::consts::TAU * t).cos();
        c.rrect(x + (w - seg) * swing, y, seg, h, h / 2., accent);
        c.text_center(
            240.,
            172. + Face::S20.mid_offset(),
            "Starting…",
            Face::S20,
            color::MUTED,
        );
        return;
    };
    let target = l.percent / 100.;
    let fill = ui.download_fill(target, false);
    let shown = if target - fill < 0.004 { target } else { fill };
    c.text_center(
        240.,
        172. + Face::S36.mid_offset(),
        &format!("{}%", (shown * 100.).floor() as u32),
        Face::S36,
        color::TEXT,
    );
    let lit = (w * fill).round();
    if lit >= h {
        c.rrect(x, y, lit, h, h / 2., accent);
        c.glow(x + lit - h / 2., y + h / 2., 14., 9., alpha(accent, 150));
        c.disc(
            x + lit - h / 2.,
            y + h / 2.,
            h / 2. - 1.,
            mix(accent, color::TEXT, 0.55),
        );
    }
    let detail = match (step, saving, l.bytes_done, l.bytes_total) {
        (0, ..) => "The player's logs, settings and hardware details.".to_owned(),
        (1, ..) => "Wi-Fi, Bluetooth, accounts and music names are taken out.".to_owned(),
        (_, Some(_), ..) => "Into the Error Reports folder.".to_owned(),
        (_, None, Some(done), Some(total)) => {
            format!("{} of {} KB", done.div_ceil(1000), total.div_ceil(1000))
        }
        (_, None, ..) => "Over Wi-Fi, encrypted.".to_owned(),
    };
    c.text_center(
        240.,
        242. + Face::R13.mid_offset(),
        &detail,
        Face::R13,
        color::MUTED,
    );
    c.text_center(
        240.,
        261. + Face::R13.mid_offset(),
        &l.seconds_left
            .map(time_left)
            .unwrap_or_else(|| "This takes about ten seconds.".to_owned()),
        Face::R13,
        color::DIM,
    );
}

/// The one-step update (Beta 7.1) over every screen: download and check
/// with live progress; the restart to install is `views_system::installing`.
pub(crate) fn update_overlay(c: &mut Canvas, ui: &Ui, m: &AppModel) {
    if ui.update_restarting {
        crate::views_system::installing(c);
        return;
    }
    c.rect(0., 0., WIDTH, HEIGHT, color::BG);
    components::status_bar(c, ui, m, "Software Update", false);
    download_progress(
        c,
        ui,
        m.platform.download,
        ui.accent(),
        crate::update_cancellable(m),
    );
}

/// "23.4" for 23 400 000 bytes.
fn megabytes(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1e6)
}

/// "3.2 MB/s", or "640 KB/s" below a megabyte per second.
fn rate(bytes_per_s: u64) -> String {
    if bytes_per_s >= 1_000_000 {
        format!("{:.1} MB/s", bytes_per_s as f64 / 1e6)
    } else {
        format!("{} KB/s", (bytes_per_s / 1000).max(1))
    }
}

/// A calm estimate: seconds in fives, then minutes.
fn time_left(seconds: u64) -> String {
    match seconds {
        0..=9 => "A few seconds left".into(),
        10..=59 => format!("About {} seconds left", (seconds + 2) / 5 * 5),
        60..=89 => "About a minute left".into(),
        _ => format!("About {} minutes left", (seconds + 30) / 60),
    }
}

fn update(c: &mut Canvas, ui: &Ui, m: &AppModel, page: &Page, focus: usize, interactive: bool) {
    let accent = ui.accent();
    let phase = &m.platform.snapshot.update.phase;
    let (tint, icon) = match phase {
        UpdatePhase::UpToDate => (color::SUCCESS, Icon::Check),
        UpdatePhase::Available { .. } | UpdatePhase::ReadyToInstall | UpdatePhase::Downloading => {
            (0x6EA0FFFF, Icon::Download)
        }
        UpdatePhase::Failed(_) => (color::WARNING, Icon::Warning),
        _ => (color::MUTED, Icon::Info),
    };
    let downloading = *phase == UpdatePhase::Downloading
        || m.platform.busy == Some(reborn_core::PlatformTask::UpdateStage);
    if downloading {
        download_progress(c, ui, m.platform.download, accent, false);
        return;
    }
    let (title, body) = page
        .hero
        .as_ref()
        .map(|h| (h.title.clone(), h.body.clone()))
        .unwrap_or_else(|| ("Software".into(), String::new()));
    c.rrect(10., 38., WIDTH - 20., 86., 16., alpha(tint, 34));
    c.rrect_outline(10., 38., WIDTH - 20., 86., 16., 1., alpha(tint, 110));
    c.disc(46., 81., 18., tint);
    c.icon(icon, 46., 81., 20., color::INK);
    c.text_mid(76., 62., 360., &title, Face::S17, color::TEXT);
    for (i, line) in text::wrap(Face::R11, &body, 360., 3).iter().enumerate() {
        c.text_mid(
            76.,
            84. + i as f32 * 15.,
            360.,
            line,
            Face::R11,
            color::MUTED,
        );
    }
    let start_rows = 138.;
    let visible = (((HEIGHT - 10. - start_rows) / 50.).floor() as usize).max(1);
    let first = focus
        .saturating_sub(visible - 1)
        .min(page.rows.len().saturating_sub(visible));
    let end = (first + visible).min(page.rows.len());
    group(
        c,
        ui,
        &page.rows[first..end],
        first,
        focus,
        interactive,
        start_rows,
    );
}

/// The Internal Storage card while the old storage is not set up yet.
fn internal_note(setup: reborn_core::platform::InternalSetup) -> Option<&'static str> {
    use reborn_core::platform::InternalSetup;
    match setup {
        InternalSetup::Waiting { .. } => Some("Not set up · Your old files are kept"),
        InternalSetup::Pending(wait) => Some(pending_note(wait)),
        InternalSetup::Running => Some("Setting up…"),
        _ => None,
    }
}

/// A low battery keeps the setup waiting at every start until a charger is connected.
pub(crate) fn pending_note(wait: reborn_core::platform::SetupWait) -> &'static str {
    match wait {
        reborn_core::platform::SetupWait::Battery => "Connect a charger and restart",
        reborn_core::platform::SetupWait::Restart => "Restart to finish setting up",
    }
}
