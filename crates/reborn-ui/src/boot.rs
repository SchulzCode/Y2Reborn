//! Boot and shutdown screens: the Reborn wordmark, one thin white bar and one
//! short status line on the product background. The early splash in Y2Linux
//! draws the same group from this module's layout and phase table
//! (`reborn-preview` exports both), so the hand-off frame is identical.
use crate::{
    components::Canvas,
    theme::{color, type_scale},
};
use reborn_graphics::Quad;

/// One real startup milestone. `token` is the name the producers report: the
/// initramfs stage names, Reborn's own `startup_phase` names and `ready`.
/// `label` is what is happening from this milestone on; `fill_permille` is a
/// coarse ordinal position of the bar, not a time or work estimate, and it is
/// never shown as a number.
pub struct BootPhase {
    pub token: &'static str,
    pub label: &'static str,
    pub fill_permille: u16,
}

const fn phase(token: &'static str, label: &'static str, fill_permille: u16) -> BootPhase {
    BootPhase {
        token,
        label,
        fill_permille,
    }
}

/// Startup milestones in the order the boot actually reaches them. Fills are
/// strictly increasing, so the bar can only move forward.
pub const BOOT_PHASES: &[BootPhase] = &[
    phase("start", "Starting system", 0),
    phase("storage_discovery", "Preparing storage", 60),
    phase("rescue_update", "Preparing storage", 100),
    phase("storage_preflight", "Preparing storage", 140),
    phase("fsck_start", "Preparing storage", 180),
    phase("fsck_complete", "Preparing storage", 250),
    phase("root_data_mounted", "Starting system", 340),
    phase("switch_root", "Starting system", 400),
    phase("model_restored", "Starting system", 470),
    phase("graphics_ready", "Loading music library", 600),
    phase("storage_ready", "Loading music library", 660),
    phase("library_workers_ready", "Starting audio", 730),
    phase("core_services_ready", "Starting audio", 780),
    phase("audio_ready", "Starting connectivity", 840),
    phase("radio_workers_ready", "Starting Reborn", 900),
    phase("runtime_ready", "Starting Reborn", 950),
    phase("ready", "Starting Reborn", 1000),
];

/// A stage that means startup cannot continue (the initramfs rescue shell).
pub const BOOT_FAILURE_TOKENS: &[&str] = &["rescue"];
/// Shown instead of the bar when startup failed or timed out.
pub const BOOT_FAILURE_LABELS: [&str; 2] = ["Could not start", "Restart the player"];
/// The status line of the last phase; the dissolve into the UI keeps it.
pub const BOOT_FINAL_LABEL: &str = "Starting Reborn";

pub const MARK_SCALE: f32 = 4.25;
pub const MARK_Y: f32 = 140.;
pub const BAR_X: f32 = 168.;
pub const BAR_Y: f32 = 190.;
pub const BAR_W: f32 = 144.;
pub const BAR_H: f32 = 2.;
pub const LABEL_Y: f32 = 206.;
pub const FAILURE_LINE_PITCH: f32 = 20.;
const LABEL_SCALE: f32 = type_scale::SECONDARY;
const LABEL_COLOR: u32 = color::TEXT_MUTED;
pub const BAR_TRACK: u32 = color::SURFACE_BORDER;
pub const BAR_FILL: u32 = color::TEXT_PRIMARY;

/// Frames of the dissolve from the boot screen into the first UI frame
/// (≈0.24 s at the UI's 34 ms cadence). The first frame is the full boot
/// screen, so the hand-off is seamless; the last is the UI alone.
pub const BOOT_FADE_FRAMES: u8 = 7;

/// Shutdown schedule at the UI's 34 ms cadence (≈0.8 s): the UI dissolves into
/// the shutdown screen showing "Saving"; the caller then saves and continues
/// at `SHUTDOWN_CLOSE_FRAME`, where the bar drains and the screen dims.
pub const SHUTDOWN_CLOSE_FRAME: usize = 7;
const SHUTDOWN_DRAIN_FRAMES: usize = 10;
const SHUTDOWN_DIM_FRAMES: usize = 6;
pub const SHUTDOWN_FRAMES: usize =
    SHUTDOWN_CLOSE_FRAME + SHUTDOWN_DRAIN_FRAMES + SHUTDOWN_DIM_FRAMES;
pub const SAVING_LABEL: &str = "Saving";

fn tint(value: u32, opacity: f32) -> u32 {
    let alpha = (opacity.clamp(0., 1.) * f32::from((value & 0xFF) as u8)).round() as u32;
    (value & 0xFFFF_FF00) | alpha
}

/// The wordmark alone.
pub fn mark(c: &mut Canvas, opacity: f32) {
    c.centered(
        240.,
        MARK_Y,
        "Reborn",
        MARK_SCALE,
        tint(color::TEXT_PRIMARY, opacity),
    );
}

/// The thin track with `fill` (0..=1) drawn from the left.
fn bar(c: &mut Canvas, fill: f32, opacity: f32) {
    c.rect(BAR_X, BAR_Y, BAR_W, BAR_H, tint(BAR_TRACK, opacity));
    c.rect(
        BAR_X,
        BAR_Y,
        (BAR_W * fill.clamp(0., 1.)).round(),
        BAR_H,
        tint(BAR_FILL, opacity),
    );
}

fn label(c: &mut Canvas, y: f32, text: &str, opacity: f32) {
    c.centered(240., y, text, LABEL_SCALE, tint(LABEL_COLOR, opacity));
}

fn group(c: &mut Canvas, fill: f32, text: &str, opacity: f32) {
    mark(c, opacity);
    bar(c, fill, opacity);
    label(c, LABEL_Y, text, opacity);
}

fn background() -> Canvas {
    let mut c = Canvas::new();
    c.rect(0., 0., 480., 360., color::BG);
    c
}

/// The complete boot screen at `fill` (0..=1) with the status `text`.
pub fn boot_screen(fill: f32, text: &str) -> Vec<Quad> {
    let mut c = background();
    group(&mut c, fill, text, 1.);
    c.finish()
}

/// The splash's failure screen: the wordmark and two plain lines, no bar.
pub fn boot_failure_screen() -> Vec<Quad> {
    let mut c = background();
    mark(&mut c, 1.);
    for (i, line) in BOOT_FAILURE_LABELS.iter().enumerate() {
        label(&mut c, LABEL_Y + FAILURE_LINE_PITCH * i as f32, line, 1.);
    }
    c.finish()
}

/// Wordmark only; the splash decodes it from this frame.
pub fn boot_mark_frame() -> Vec<Quad> {
    let mut c = background();
    mark(&mut c, 1.);
    c.finish()
}

/// One status line only at `y`; the splash decodes it from this frame.
pub fn boot_label_frame(text: &str, y: f32) -> Vec<Quad> {
    let mut c = background();
    label(&mut c, y, text, 1.);
    c.finish()
}

/// Dissolve from the boot screen into the first UI frame. `remaining` runs
/// from 1 (all boot screen, bar full) to 0 (all UI); no frame is ever blank.
pub fn boot_transition(ui: Vec<Quad>, remaining: f32) -> Vec<Quad> {
    let mut quads = with_overlay(ui, remaining);
    let mut c = Canvas::new();
    group(&mut c, 1., BOOT_FINAL_LABEL, remaining);
    quads.extend(c.finish());
    quads
}

/// Cover `quads` with the background at `alpha` (0 transparent, 1 opaque).
pub fn with_overlay(mut quads: Vec<Quad>, alpha: f32) -> Vec<Quad> {
    let a = (alpha.clamp(0., 1.) * 255.).round() as u32;
    quads.push(Quad::rect(
        0.,
        0.,
        480.,
        360.,
        (color::BG & 0xFFFF_FF00) | a,
    ));
    for q in &mut quads {
        if alpha >= 1. {
            q.focus_target = false;
        }
    }
    quads
}

/// What the player is doing once its data is saved.
pub fn closing_label(restart: bool, low_battery: bool) -> &'static str {
    if low_battery {
        "Battery empty"
    } else if restart {
        "Restarting"
    } else {
        "Shutting down"
    }
}

/// One shutdown frame. Frames before `SHUTDOWN_CLOSE_FRAME` dissolve `ui` (the
/// last UI frame) into the shutdown screen with a full bar and "Saving". From
/// `SHUTDOWN_CLOSE_FRAME` the status is `closing`, the bar drains right to
/// left and the screen dims to the background.
pub fn shutdown_frame(ui: &[Quad], frame: usize, closing: &str) -> Vec<Quad> {
    let frame = frame.min(SHUTDOWN_FRAMES - 1);
    if frame < SHUTDOWN_CLOSE_FRAME {
        let alpha = (frame + 1) as f32 / SHUTDOWN_CLOSE_FRAME as f32;
        let mut quads = with_overlay(ui.to_vec(), alpha);
        let mut c = Canvas::new();
        group(&mut c, 1., SAVING_LABEL, alpha);
        quads.extend(c.finish());
        return quads;
    }
    let step = frame - SHUTDOWN_CLOSE_FRAME;
    let drained = ((step + 1).min(SHUTDOWN_DRAIN_FRAMES)) as f32 / SHUTDOWN_DRAIN_FRAMES as f32;
    let mut c = background();
    group(&mut c, 1. - drained, closing, 1.);
    let mut quads = c.finish();
    let dim = (step + 1).saturating_sub(SHUTDOWN_DRAIN_FRAMES);
    if dim > 0 {
        quads = with_overlay(quads, dim as f32 / SHUTDOWN_DIM_FRAMES as f32);
    }
    quads
}

/// The final frame before the backlight turns off.
pub fn black_frame() -> Vec<Quad> {
    vec![Quad::rect(0., 0., 480., 360., color::BLACK)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(q: &Quad) -> (u32, u32, u32, u32, u32, Option<u32>) {
        (
            q.x.to_bits(),
            q.y.to_bits(),
            q.w.to_bits(),
            q.h.to_bits(),
            q.color,
            q.glyph.map(u32::from),
        )
    }

    #[test]
    fn phases_are_ordered_unique_and_user_facing() {
        assert_eq!(BOOT_PHASES[0].fill_permille, 0);
        assert_eq!(BOOT_PHASES.last().unwrap().fill_permille, 1000);
        assert_eq!(BOOT_PHASES.last().unwrap().token, "ready");
        for pair in BOOT_PHASES.windows(2) {
            assert!(
                pair[0].fill_permille < pair[1].fill_permille,
                "{} must come after {}",
                pair[1].token,
                pair[0].token
            );
            assert_ne!(pair[0].token, pair[1].token);
        }
        let mut tokens = BOOT_PHASES.iter().map(|p| p.token).collect::<Vec<_>>();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), BOOT_PHASES.len());
        let labels = BOOT_PHASES
            .iter()
            .map(|p| p.label)
            .chain(BOOT_FAILURE_LABELS)
            .chain([SAVING_LABEL, closing_label(false, false)])
            .chain([closing_label(true, false), closing_label(false, true)]);
        for text in labels {
            let lower = text.to_ascii_lowercase();
            for technical in [
                "switch_root",
                "systemd",
                "drm",
                "alsa",
                "kms",
                "mount",
                "/",
                "_",
                ".ko",
                "init",
                "fsck",
                "%",
            ] {
                assert!(!lower.contains(technical), "{text:?} exposes {technical:?}");
            }
            assert!(text.chars().count() <= 24, "{text:?} is not short");
            assert!(!text.chars().any(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn bar_geometry_is_centered_thin_and_white() {
        assert_eq!(BAR_X * 2. + BAR_W, 480.);
        let quads = boot_screen(0.5, "Starting audio");
        let fill = quads
            .iter()
            .find(|q| q.color == BAR_FILL && q.h == BAR_H)
            .expect("bar fill");
        assert_eq!((fill.x, fill.w), (BAR_X, BAR_W / 2.));
        assert!(fill.h <= 2., "thin bar");
        let red = (BAR_FILL >> 24) & 0xFF;
        let blue = (BAR_FILL >> 8) & 0xFF;
        assert!(red > 0xE0 && blue > 0xD0, "near-white fill");
        // No yellow/gold accent anywhere on the boot or shutdown screens.
        let accents = [
            color::ACCENT_GOLD,
            color::ACCENT_GOLD_BRIGHT,
            color::ACCENT_GOLD_DIM,
        ];
        let ui = vec![Quad::rect(0., 0., 480., 360., color::BG)];
        let mut all = quads;
        all.extend(boot_failure_screen());
        all.extend(boot_transition(ui.clone(), 0.5));
        for frame in 0..SHUTDOWN_FRAMES {
            all.extend(shutdown_frame(&ui, frame, "Shutting down"));
        }
        assert!(all.iter().all(|q| !accents.contains(&q.color)));
    }

    #[test]
    fn bar_fills_left_to_right_and_never_exceeds_its_track() {
        let widths = [0., 0.25, 0.6, 1., 7.]
            .into_iter()
            .map(|f| {
                boot_screen(f, "x")
                    .iter()
                    .find(|q| q.color == BAR_FILL && q.h == BAR_H)
                    .map_or(0., |q| {
                        assert_eq!(q.x, BAR_X, "fills from the left edge");
                        q.w
                    })
            })
            .collect::<Vec<_>>();
        assert_eq!(widths, [0., 36., 86., 144., 144.]);
    }

    #[test]
    fn dissolve_starts_as_the_boot_screen_and_ends_as_the_ui() {
        let ui = vec![Quad::rect(0., 0., 480., 360., color::SURFACE)];
        let first = boot_transition(ui.clone(), 1.);
        let overlay = first.iter().find(|q| q.w == 480. && q.color == color::BG);
        assert!(
            overlay.is_some(),
            "opaque cover on the first dissolve frame"
        );
        assert!(first.iter().all(|q| !q.focus_target));
        // The first dissolve frame draws the same group as the splash's last.
        let screen = boot_screen(1., BOOT_FINAL_LABEL);
        let first_shapes = first.iter().map(shape).collect::<Vec<_>>();
        assert!(screen
            .iter()
            .skip(1)
            .all(|q| first_shapes.contains(&shape(q))));
        let mut previous = 2.;
        for step in (1..=BOOT_FADE_FRAMES).rev() {
            let remaining = f32::from(step) / f32::from(BOOT_FADE_FRAMES);
            assert!(remaining < previous);
            previous = remaining;
            let quads = boot_transition(ui.clone(), remaining);
            let cover = quads
                .iter()
                .find(|q| q.w == 480. && q.color & 0xFFFF_FF00 == color::BG & 0xFFFF_FF00)
                .unwrap();
            assert_eq!(
                cover.color & 0xFF,
                (remaining * 255.).round() as u32,
                "cover alpha follows the fade"
            );
        }
    }

    #[test]
    fn shutdown_is_saving_then_closing_and_ends_dark() {
        let ui = vec![Quad::rect(0., 0., 480., 360., color::SURFACE)];
        let text = |frame| {
            shutdown_frame(&ui, frame, "Shutting down")
                .iter()
                .filter(|q| q.glyph.is_some())
                .count()
        };
        // "Saving" and "Reborn" (+ "Shutting down" later): glyph counts differ.
        let saving = text(SHUTDOWN_CLOSE_FRAME - 1);
        let closing = text(SHUTDOWN_CLOSE_FRAME);
        assert_eq!(saving, "Reborn".len() + SAVING_LABEL.len());
        assert_eq!(closing, "Reborn".len() + "Shutting down".chars().count());
        // The bar is full while saving, then drains monotonically to empty.
        let fill = |frame| {
            shutdown_frame(&ui, frame, "x")
                .iter()
                .find(|q| q.color & 0xFFFF_FF00 == BAR_FILL & 0xFFFF_FF00 && q.h == BAR_H)
                .map_or(0., |q| q.w)
        };
        assert_eq!(fill(0), BAR_W);
        assert_eq!(fill(SHUTDOWN_CLOSE_FRAME - 1), BAR_W);
        let mut last = BAR_W + 1.;
        for frame in SHUTDOWN_CLOSE_FRAME..SHUTDOWN_CLOSE_FRAME + SHUTDOWN_DRAIN_FRAMES {
            assert!(fill(frame) < last);
            last = fill(frame);
        }
        assert_eq!(last, 0.);
        // The last frame is fully covered by the background; then black.
        let end = shutdown_frame(&ui, SHUTDOWN_FRAMES - 1, "x");
        assert_eq!(end.last().unwrap().color, color::BG);
        assert_eq!(black_frame()[0].color, color::BLACK);
        // Out-of-range frames clamp to the last real one.
        let clamped = shutdown_frame(&ui, 999, "x");
        assert_eq!(
            clamped.iter().map(shape).collect::<Vec<_>>(),
            end.iter().map(shape).collect::<Vec<_>>()
        );
    }

    #[test]
    fn closing_labels_follow_the_real_action() {
        assert_eq!(closing_label(false, false), "Shutting down");
        assert_eq!(closing_label(true, false), "Restarting");
        assert_eq!(closing_label(true, true), "Battery empty");
    }
}
