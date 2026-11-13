//! Customize > Buttons, a layer's face page: the player drawn on the left (its wheel with the four
//! buttons and the centre, Power and the volume buttons on its side), the button in focus lit with
//! a glow that glides from button to button, buttons that differ from Reborn Default marked with
//! the accent; on the right a card with what the button does, sliding in for each button. The
//! drawing grows in when the page opens. Its rows and what Select does: `buttons_page.rs`.
use crate::{
    canvas::Canvas,
    motion,
    pages::Page,
    text::Face,
    theme::{color, fade, HEIGHT, WIDTH},
    Ui,
};
use reborn_core::{
    buttons::{Command, Control, Gesture, Layer},
    AppModel,
};

/// The wheel's centre and radius, and the player's body.
const WHEEL: (f32, f32, f32) = (122., 240., 68.);
const BODY: (f32, f32, f32, f32) = (34., 44., 176., 280.);
const SIDE_X: f32 = 210.;

/// Where a control sits on the drawing: its centre.
fn spot(control: Control) -> (f32, f32) {
    let (cx, cy, r) = WHEEL;
    let ring = r - 22.;
    match control {
        Control::Back => (cx, cy - ring),
        Control::Next => (cx + ring, cy),
        Control::PlayPause => (cx, cy + ring),
        Control::Previous => (cx - ring, cy),
        Control::Select => (cx, cy),
        Control::WheelForward => (cx + r * 0.72, cy - r * 0.72),
        Control::WheelBack => (cx - r * 0.72, cy - r * 0.72),
        Control::VolumeUp => (SIDE_X + 3., 146.),
        Control::VolumeDown => (SIDE_X + 3., 180.),
    }
}

/// The small mark printed on each button.
fn glyph(control: Control) -> &'static str {
    match control {
        Control::Back => "MENU",
        Control::Next => ">>",
        Control::PlayPause => "> ||",
        Control::Previous => "<<",
        Control::Select => "",
        Control::WheelForward => "+",
        Control::WheelBack => "-",
        Control::VolumeUp => "+",
        Control::VolumeDown => "-",
    }
}

pub(crate) fn face(
    c: &mut Canvas,
    ui: &Ui,
    m: &AppModel,
    page: &Page,
    focus: usize,
    interactive: bool,
) {
    let Some(layer) = crate::buttons_page::face(m) else {
        return;
    };
    let accent = ui.accent();
    let layout = ui.editing(m);
    let changed = layout.changed(layer);
    let focused = Control::ALL.get(focus).copied().unwrap_or(Control::Back);
    // The drawing grows in from 90 % when the page opens.
    let shown = ui.ease(
        &format!("buttons.open.{}", layer.key()),
        Some(0.),
        1.,
        motion::CALM,
        0.004,
    );
    let start = c.draw.len();
    player(c, ui, layer, focused, &changed, accent);
    let scale = 0.9 + 0.1 * shown;
    let (cx, cy) = (BODY.0 + BODY.2 / 2., BODY.1 + BODY.3 / 2.);
    for q in &mut c.draw[start..] {
        q.x = cx + (q.x - cx) * scale;
        q.y = cy + (q.y - cy) * scale;
        q.w *= scale;
        q.h *= scale;
    }
    motion::fade_all(&mut c.draw[start..], shown);
    let at = c.draw.len();
    card(c, ui, m, layer, focused, changed.contains(&focused), accent);
    if interactive {
        c.mark_focus(at);
    }
    if let Some(line) = &page.status {
        c.text_center(WIDTH / 2., HEIGHT - 10., line, Face::R13, color::MUTED);
    }
}

/// The player: body, screen, wheel, buttons, side buttons, the glow on the focused control.
fn player(
    c: &mut Canvas,
    ui: &Ui,
    layer: Layer,
    focused: Control,
    changed: &[Control],
    accent: u32,
) {
    let (bx, by, bw, bh) = BODY;
    let dark = layer == Layer::ScreenOff;
    c.rrect(bx, by, bw, bh, 26., 0x1B1D23FF);
    c.rrect_outline(bx, by, bw, bh, 26., 1., color::GLASS);
    // The screen shows the layer: lit for the menus and Now Playing, dark with the screen off.
    let screen = if dark { 0x07080AFF } else { 0x10131AFF };
    c.rrect(bx + 18., by + 16., bw - 36., 100., 10., screen);
    let (title, line) = match layer {
        Layer::Menus => ("Menus", "lists and pages"),
        Layer::NowPlaying => ("Now Playing", "while it plays"),
        Layer::ScreenOff => ("Screen Off", "in the pocket"),
    };
    let ink = if dark { color::DIM } else { color::TEXT };
    c.text_mid_center(bx + bw / 2., by + 56., title, Face::S15, ink);
    c.text_mid_center(bx + bw / 2., by + 80., line, Face::R11, color::DIM);
    // Power, fixed: drawn, never lit.
    c.rrect(SIDE_X, 92., 6., 34., 3., 0x2C2F37FF);
    c.text(SIDE_X + 12., 114., "Power", Face::R11, color::DIM);
    for control in [Control::VolumeUp, Control::VolumeDown] {
        let (_, y) = spot(control);
        c.rrect(SIDE_X, y - 13., 6., 26., 3., 0x353943FF);
        c.text(
            SIDE_X + 12.,
            y + 4.,
            glyph(control),
            Face::S13,
            color::MUTED,
        );
    }
    // The wheel, its ring lit while a turn is in focus.
    let (cx, cy, r) = WHEEL;
    c.disc(cx, cy, r, 0x2A2D35FF);
    let turning = focused.is_wheel();
    let ring_light = ui.ease(
        "buttons.ring",
        None,
        f32::from(u8::from(turning)),
        motion::QUICK,
        0.004,
    );
    if ring_light > 0.01 {
        c.ring(cx, cy, r + 2., 4., fade(accent, ring_light));
        // The direction of the turn sweeps round the ring.
        let sweep = ui.ease(
            &format!("buttons.sweep.{}", focused.key()),
            Some(0.),
            if turning { 0.35 } else { 0. },
            motion::CALM,
            0.002,
        );
        c.arc(cx, cy, r + 2., sweep, fade(accent, ring_light));
    }
    c.disc(cx, cy, 27., 0x1B1D23FF);
    // The glow glides to the focused button.
    let (fx, fy) = spot(focused);
    let gx = ui.ease("buttons.glow.x", None, fx, motion::QUICK, 0.3);
    let gy = ui.ease("buttons.glow.y", None, fy, motion::QUICK, 0.3);
    if !turning {
        c.glow(gx, gy, 34., 34., fade(accent, 0.55));
        c.ring(gx, gy, 17., 2., accent);
    }
    for control in Control::ALL {
        let (x, y) = spot(control);
        if control.is_wheel() {
            c.text_mid_center(x, y, glyph(control), Face::S17, color::MUTED);
        } else if !matches!(control, Control::VolumeUp | Control::VolumeDown) {
            let ink = if control == focused {
                color::TEXT
            } else {
                color::MUTED
            };
            c.text_mid_center(x, y, glyph(control), Face::S10, ink);
        }
        // Changed from Reborn Default: an accent dot beside it.
        if changed.contains(&control) {
            c.disc(x + 13., y - 13., 4., accent);
        }
    }
}

/// The card on the right: the button, what a press (or turn) and a hold do. It slides up into
/// place for each button.
fn card(
    c: &mut Canvas,
    ui: &Ui,
    m: &AppModel,
    layer: Layer,
    control: Control,
    changed: bool,
    accent: u32,
) {
    let layout = ui.editing(m);
    let rise = ui.ease(
        &format!("buttons.card.{}", control.key()),
        Some(14.),
        0.,
        motion::CALM,
        0.2,
    );
    let alpha = 1. - rise / 14.;
    let (x, y, w, h) = (262., 52. + rise, 204., 262.);
    let start = c.draw.len();
    c.rrect(x, y, w, h, 16., color::GLASS_FAINT);
    c.rrect_outline(
        x,
        y,
        w,
        h,
        16.,
        1.,
        if changed { accent } else { color::GLASS },
    );
    c.text(x + 16., y + 34., control.label(), Face::S17, color::TEXT);
    let lines: &[(Gesture, &str)] = if control.is_wheel() {
        &[(Gesture::Press, "Turn")]
    } else {
        &[(Gesture::Press, "Press"), (Gesture::Hold, "Hold")]
    };
    let mut row_y = y + 74.;
    for &(gesture, name) in lines {
        let command = layout.resolve(layer, control, gesture);
        let inherited = layer == Layer::NowPlaying && layout.get(layer, control, gesture).is_none();
        c.text(x + 16., row_y, name, Face::R13, color::DIM);
        let ink = if command == Command::Nothing {
            color::DIM
        } else {
            color::TEXT
        };
        c.text(x + 16., row_y + 26., command.label(), Face::S17, ink);
        if inherited {
            c.text(x + 16., row_y + 46., "as in Menus", Face::R11, color::DIM);
        }
        row_y += 76.;
    }
    if changed {
        c.text(x + 16., y + h - 18., "Changed", Face::R13, accent);
    }
    c.text_right(
        x + w - 16.,
        y + h - 18.,
        "Select to change",
        Face::R11,
        color::MUTED,
    );
    motion::fade_all(&mut c.draw[start..], alpha);
}
