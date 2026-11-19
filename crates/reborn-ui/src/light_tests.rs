use super::*;

fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[test]
fn the_tokens_turn_light_and_keep_their_alpha() {
    assert_eq!(light(color::BG), BG);
    assert_eq!(light(color::TEXT), 0x15171CFF);
    assert_eq!(light(color::SHEET) & 0xFF, color::SHEET & 0xFF);
    // Type on the accent stays white; a see-through accent fill gets more solid.
    assert_eq!(light(color::ON_ACCENT), 0xFFFFFFFF);
    assert_eq!(light(crate::theme::alpha(color::ACCENT, 120)), 0x2563D9F0);
    // Text on a light pill (ink on TEXT) becomes light on dark.
    assert_eq!(light(color::INK), BG);
    for token in [
        color::TEXT,
        color::MUTED,
        color::DIM,
        color::ACCENT,
        color::DANGER,
    ] {
        assert!(contrast(light(token), BG) >= 4.5, "{token:08X}");
    }
}

#[test]
fn glass_turns_dark_and_shadows_soften() {
    assert_eq!(light(color::GLASS), 0x0000_0019);
    assert_eq!(light(color::RULE) >> 8, 0);
    assert_eq!(light(0x0000_0090), 0x0000_0039, "a shadow, softer");
    assert_eq!(light(0x0000_00FF), 0x0000_0066);
}

#[test]
fn greys_turn_around_and_bright_colours_get_readable() {
    // A grey between text and background stays between them.
    let mid = crate::theme::mix(color::TEXT, color::BG, 0.5);
    let shown = light(mid);
    assert!(luminance(shown) > luminance(light(color::TEXT)));
    assert!(luminance(shown) < luminance(BG));
    // A bright cover accent keeps its hue and reads on the light background.
    let accent = light(0xFFB04AFF);
    assert!(contrast(accent, BG) >= 4.5);
    let (h, _, _) = hsl(accent);
    assert!((h - hsl(0xFFB04AFF).0).abs() < 4., "hue kept: {h}");
    // A dark placeholder tile becomes a pale one.
    assert!(luminance(light(0x2A3040FF)) > 0.5);
}

#[test]
fn only_flat_colours_type_and_icons_change() {
    let mut frame = vec![
        Quad::rect(0., 0., 10., 10., color::BG),
        Quad {
            tex: Tex::Thumb(7),
            ..Quad::rect(0., 0., 10., 10., color::TEXT)
        },
        Quad {
            tex: Tex::Art,
            ..Quad::rect(0., 0., 10., 10., color::TEXT)
        },
        Quad {
            tex: Tex::Font(0),
            ..Quad::rect(0., 0., 10., 10., color::TEXT)
        },
    ];
    apply(&mut frame);
    assert_eq!(frame[0].color, BG);
    assert_eq!(frame[1].color, color::TEXT, "covers keep their tint");
    assert_eq!(frame[2].color, color::TEXT);
    assert_eq!(frame[3].color, 0x15171CFF);
}
