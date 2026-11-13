//! The Quick Menu's panel (Y2Linux #85): centred tiles that grow in over a dimmed screen, a focus
//! frame that glides between them, Wi-Fi and Bluetooth tiles that fill with the accent while on,
//! a brightness slider and, while Power is still held, the turn-off ring on the Power Off tile.
//! Skins get the same panel in their palette (`skin::remap`). What the rows do: `quick_menu.rs`.
use crate::{
    canvas::{Canvas, Icon},
    motion,
    text::Face,
    theme::{color, fade, mix_rgba, HEIGHT, WIDTH},
    Item, Ui,
};
use reborn_core::{AppModel, Modal};
use std::time::Instant;

/// The panel and its tiles: `(x, y, w, h)`.
const PANEL: (f32, f32, f32, f32) = (28., 34., 424., 294.);
const RADIUS: f32 = 22.;
const TILE_RADIUS: f32 = 16.;
const TILE_FILL: u32 = 0xFFFFFF14;

/// `four`: the bottom row has four tiles (the Buttons tile, while a layout other than Reborn
/// Default is in use), in the rows' order: Sleep Timer, Buttons, Restart, Power Off.
fn tile(key: &str, four: bool) -> (f32, f32, f32, f32) {
    let bottom = |i: f32| {
        if four {
            (44. + i * 100.5, 234., 90.5, 80.)
        } else {
            (44. + i.min(2.) * 134., 234., 124., 80.)
        }
    };
    match key {
        "quick:wifi" => (44., 82., 192., 88.),
        "quick:bluetooth" => (244., 82., 192., 88.),
        "quick:brightness" => (44., 180., 392., 44.),
        "quick:sleep" => bottom(0.),
        "quick:buttons" => bottom(1.),
        "quick:restart" => bottom(if four { 2. } else { 1. }),
        _ => bottom(if four { 3. } else { 2. }),
    }
}

/// Whether the bottom row has the Buttons tile.
fn four(m: &AppModel) -> bool {
    m.settings.buttons.active != reborn_core::buttons::REBORN_DEFAULT
}

/// A value easing toward `target` (at once in previews and tests).
fn eased(ui: &Ui, key: &str, start: Option<f32>, target: f32, rate: f32, settle: f32) -> f32 {
    if !ui.ui_motion {
        return target;
    }
    ui.motion
        .ease(key, start, target, rate, settle, Instant::now())
}

/// A menu that draws as a sheet or a dialog: any but the Quick Menu's panel.
pub(crate) fn sheet_modal(m: &AppModel) -> bool {
    m.navigation.modal.is_some() && m.navigation.modal != Some(Modal::QuickMenu)
}

/// The panel (open, or fading out), then the full-screen ring of a Power hold without it.
pub(crate) fn overlay(c: &mut Canvas, ui: &Ui, m: &AppModel) {
    draw(c, ui, m);
    if m.navigation.modal != Some(Modal::QuickMenu) {
        crate::views_playing::power_hold(c, ui);
    }
}

/// The panel while the Quick Menu is open, and as it fades out after closing.
pub(crate) fn draw(c: &mut Canvas, ui: &Ui, m: &AppModel) {
    let open = m.navigation.modal == Some(Modal::QuickMenu);
    if m.navigation.modal.is_some() && !open {
        // Another menu opened from it (the sleep timer) takes the screen at once.
        ui.motion.reset("quick.open", 0.);
        return;
    }
    let shown = eased(
        ui,
        "quick.open",
        Some(0.),
        f32::from(u8::from(open)),
        motion::CALM,
        0.004,
    );
    if shown <= 0.01 {
        return;
    }
    c.rect(0., 0., WIDTH, HEIGHT, fade(color::SCRIM, shown));
    let start = c.draw.len();
    let (px, py, pw, ph) = PANEL;
    c.rrect(px, py, pw, ph, RADIUS, color::SHEET);
    c.rrect_outline(px, py, pw, ph, RADIUS, 1., color::GLASS);
    let (title, line) = ui.quick_menu_copy(m);
    c.text_mid(px + 20., py + 24., 180., &title, Face::S17, color::TEXT);
    let face = if ui.adjusting_brightness {
        Face::R11
    } else {
        Face::R13
    };
    c.text_mid_right(px + pw - 20., py + 24., &line, face, color::MUTED);
    let rows = ui.quick_menu_rows(m);
    let focus = m.navigation.modal_focus.min(rows.len().saturating_sub(1));
    focus_frame(c, ui, rows.get(focus), four(m));
    for (i, row) in rows.iter().enumerate() {
        let at = c.draw.len();
        match row.key.as_str() {
            "quick:wifi" | "quick:bluetooth" => radio_tile(c, ui, row),
            "quick:brightness" => brightness(c, ui, m, row),
            _ => action_tile(c, ui, row, four(m)),
        }
        // The tiles appear one after another as the panel grows in.
        let entrance = ((shown - i as f32 * 0.06) / 0.7).clamp(0., 1.);
        motion::fade_all(&mut c.draw[at..], entrance);
    }
    // Grows in from 92 % around the screen's centre, and shrinks as it fades out.
    let scale = 0.92 + 0.08 * shown;
    for q in &mut c.draw[start..] {
        q.x = WIDTH / 2. + (q.x - WIDTH / 2.) * scale;
        q.y = py + ph / 2. + (q.y - py - ph / 2.) * scale;
        q.w *= scale;
        q.h *= scale;
    }
    motion::fade_all(&mut c.draw[start..], shown);
}

/// The frame around the focused tile, gliding to the next one.
fn focus_frame(c: &mut Canvas, ui: &Ui, row: Option<&Item>, four: bool) {
    let Some(row) = row else {
        return;
    };
    let (x, y, w, h) = tile(&row.key, four);
    let glide = |key: &str, to: f32| eased(ui, key, None, to, motion::QUICK, 0.3);
    let (x, y) = (glide("quick.fx", x), glide("quick.fy", y));
    let (w, h) = (glide("quick.fw", w), glide("quick.fh", h));
    let ink = if ui.adjusting_brightness {
        ui.accent()
    } else {
        color::TEXT
    };
    c.rrect(
        x - 4.,
        y - 4.,
        w + 8.,
        h + 8.,
        TILE_RADIUS + 4.,
        fade(ink, 0.16),
    );
    c.rrect_outline(
        x - 4.,
        y - 4.,
        w + 8.,
        h + 8.,
        TILE_RADIUS + 4.,
        2.,
        fade(ink, 0.9),
    );
}

/// Wi-Fi or Bluetooth: the tile fills with the accent while the radio is on.
fn radio_tile(c: &mut Canvas, ui: &Ui, row: &Item) {
    let (x, y, w, h) = tile(&row.key, false);
    let powered = if row.key == "quick:wifi" {
        ui.wifi.powered
    } else {
        ui.bluetooth.powered
    };
    let lit = row.enabled && powered;
    let on = eased(
        ui,
        &format!("{}.on", row.key),
        None,
        f32::from(u8::from(lit)),
        motion::QUICK,
        0.004,
    );
    c.rrect(
        x,
        y,
        w,
        h,
        TILE_RADIUS,
        mix_rgba(TILE_FILL, fade(ui.accent(), 0.92), on),
    );
    let icon = if row.key == "quick:wifi" {
        Icon::Wifi
    } else {
        Icon::Bluetooth
    };
    // On the accent: white in Reborn's look; a skin's text often is its accent (Terminal's
    // green), so there it takes the skin's background.
    let lit_ink = if ui.skin.is_some() {
        color::BG
    } else {
        color::TEXT
    };
    let (ix, iy) = (x + 32., y + 30.);
    c.disc(ix, iy, 18., mix_rgba(color::GLASS, lit_ink, on));
    let ink = if row.enabled {
        mix_rgba(color::TEXT, lit_ink, on)
    } else {
        color::DIM
    };
    let icon_ink = if row.enabled { color::TEXT } else { color::DIM };
    c.icon(icon, ix, iy, 20., mix_rgba(icon_ink, ui.accent(), on));
    c.text_mid(x + 16., y + 60., w - 32., &row.label, Face::S15, ink);
    let detail = if row.enabled {
        fade(ink, 0.6 + 0.3 * on)
    } else {
        color::DIM
    };
    c.text_mid(x + 16., y + 76., w - 32., &row.secondary, Face::R11, detail);
}

/// The brightness slider: the sun grows with the level, the fill glides to it.
fn brightness(c: &mut Canvas, ui: &Ui, m: &AppModel, row: &Item) {
    let (x, y, w, h) = tile(&row.key, false);
    c.rrect(x, y, w, h, TILE_RADIUS, TILE_FILL);
    let min = f32::from(reborn_core::BRIGHTNESS_MIN);
    let target = ((f32::from(m.settings.brightness) - min) / (100. - min)).clamp(0., 1.);
    let level = eased(ui, "quick.level", None, target, motion::QUICK, 0.002);
    let cy = y + h / 2.;
    c.icon(Icon::Sun, x + 26., cy, 14. + 8. * level, color::TEXT);
    let (from, to) = (x + 52., x + w - 64.);
    c.rrect(from, cy - 3., to - from, 6., 3., color::GLASS);
    c.rrect(from, cy - 3., (to - from) * level, 6., 3., ui.accent());
    c.disc(from + (to - from) * level, cy, 7., color::TEXT);
    c.text_mid_right(x + w - 16., cy, &row.secondary, Face::S13, color::TEXT);
}

/// Sleep Timer, Restart and Power Off; Power Off carries the turn-off ring while Power is held.
fn action_tile(c: &mut Canvas, ui: &Ui, row: &Item, four: bool) {
    let (x, y, w, _) = tile(&row.key, four);
    c.rrect(x, y, w, 80., TILE_RADIUS, TILE_FILL);
    let (icon, tint) = match row.key.as_str() {
        "quick:sleep" => (Icon::Moon, color::TEXT),
        "quick:restart" => (Icon::Refresh, color::TEXT),
        "quick:buttons" => (Icon::Sliders, color::TEXT),
        _ => (Icon::Power, color::DANGER),
    };
    let ink = if row.enabled { color::TEXT } else { color::DIM };
    let (cx, cy) = (x + w / 2., y + 25.);
    c.disc(cx, cy, 16., color::GLASS);
    c.icon(
        icon,
        cx,
        cy,
        16.,
        if row.enabled { tint } else { color::DIM },
    );
    let holding = ui
        .power_hold_progress()
        .filter(|_| row.key == "quick:power_off");
    let label = if holding.is_some() {
        "Hold to Turn Off"
    } else {
        row.label.as_str()
    };
    if let Some(progress) = holding {
        c.ring(cx, cy, 20., 3., color::GLASS);
        c.arc(cx, cy, 20., progress, color::DANGER);
    }
    let face = if holding.is_some() {
        Face::R13
    } else {
        Face::S15
    };
    let detail = !row.secondary.is_empty();
    let ly = if detail { y + 53. } else { y + 60. };
    c.text_center(cx, ly + face.mid_offset(), label, face, ink);
    if detail {
        let shown = crate::text::fit(Face::R11, &row.secondary, w - 16.);
        c.text_center(
            cx,
            y + 70. + Face::R11.mid_offset(),
            &shown,
            Face::R11,
            color::MUTED,
        );
    }
}
