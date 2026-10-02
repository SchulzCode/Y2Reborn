//! Boot hand-off guard. The early splash keeps the display, showing real
//! startup progress, until Reborn has rendered its first complete UI frame.
//! That frame is presented under the full boot cover and dissolves at once.
//! Reborn must never present a static logo of its own and then keep it up while
//! the rest of startup runs: that is what held the logo for seconds before.
use std::{fs, path::Path};

fn main_source() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs")).unwrap()
}

fn at(source: &str, needle: &str) -> usize {
    source
        .find(needle)
        .unwrap_or_else(|| panic!("main.rs no longer contains {needle:?}"))
}

#[test]
fn every_reported_milestone_is_a_known_boot_phase() {
    let source = main_source();
    let known = reborn_ui::BOOT_PHASES
        .iter()
        .map(|p| p.token)
        .collect::<Vec<_>>();
    let mut reported = vec![];
    for line in source.lines() {
        if let Some(rest) = line
            .trim()
            .strip_prefix("startup_phase(&log, process_started, \"")
        {
            reported.push(rest.split('"').next().unwrap().to_owned());
        }
    }
    for phase in [
        "model_restored",
        "graphics_ready",
        "storage_ready",
        "library_workers_ready",
        "core_services_ready",
        "audio_ready",
        "radio_workers_ready",
        "runtime_ready",
    ] {
        assert!(
            reported.iter().any(|r| r == phase),
            "{phase} is not reported"
        );
    }
    for token in &reported {
        assert!(
            known.contains(&token.as_str()),
            "{token} is reported but missing from reborn_ui::BOOT_PHASES"
        );
    }
    // Reported in the order the table (and so the bar) expects.
    let order = reported
        .iter()
        .map(|t| known.iter().position(|k| k == t).unwrap())
        .collect::<Vec<_>>();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{reported:?}");
    assert!(source.contains("boot_milestone(\"ready\")"));
}

#[test]
fn first_presented_frame_is_the_real_ui_under_the_boot_cover() {
    let source = main_source();
    for forbidden in ["boot_screen(", "boot_failure_screen(", "boot_mark_frame("] {
        assert!(
            !source.contains(forbidden),
            "main.rs must not present a static boot screen itself ({forbidden})"
        );
    }
    let start = at(&source, "let mut first_frame_presented = false;");
    let frame = at(&source, "let mut draw = rt.ui.draw(");
    let cover = at(&source, "reborn_ui::boot_transition(");
    let render = at(&source, "g.render(&draw)");
    let ready = at(&source, "contract::application_ready()");
    assert!(start < frame && frame < cover && cover < render && render < ready);
    // The workers are all started before the first frame, never after the hand-off.
    assert!(at(&source, "\"runtime_ready\"") < render);
    // Platform readiness is only announced after the first frame was presented.
    assert_eq!(source.matches("contract::application_ready()").count(), 1);
    // With a renderer, a fade step is consumed only after its frame was presented.
    let consume = source.rfind("rt.boot_fade -= 1;").unwrap();
    assert!(render < consume && consume < ready);
    assert!(source[render..consume].contains("} else {"));
}

#[test]
fn the_dissolve_is_short_and_not_a_fixed_hold() {
    let ms = u64::from(reborn_ui::BOOT_FADE_FRAMES) * 34;
    assert!((150..=500).contains(&ms), "{ms} ms");
    let source = main_source();
    assert!(source.contains("boot_fade: reborn_ui::BOOT_FADE_FRAMES"));
    assert!(!source.contains("boot_fade: 6"));
}
