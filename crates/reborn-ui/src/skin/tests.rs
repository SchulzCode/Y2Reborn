//! The skin compiler and interpreter over in-memory packages (the UI crate
//! performs no I/O; reading folders is tested in the app's `skins` module).
use super::*;
use crate::{Item, Ui};
use reborn_core::{AppModel, Effect, PlaybackState, Screen, Source, Track};
use std::collections::HashMap;

/// A skin package in memory. Pictures are PNG files (read by size, filled
/// with one colour) or text "WxH".
#[derive(Default)]
struct Mem(HashMap<String, Vec<u8>>);

impl Mem {
    fn with(mut self, name: &str, bytes: &[u8]) -> Self {
        self.0.insert(name.into(), bytes.to_vec());
        self
    }
}

impl Files for Mem {
    fn read(&self, name: &str, limit: u64) -> Result<Vec<u8>, String> {
        let b = self.0.get(name).ok_or("is missing")?;
        if b.len() as u64 > limit {
            return Err("is too large".into());
        }
        Ok(b.clone())
    }
    fn picture(&self, name: &str, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let b = self.0.get(name).ok_or("is missing")?;
        let (w, h) = if b.len() >= 24 && &b[1..4] == b"PNG" {
            (
                u32::from_be_bytes(b[16..20].try_into().unwrap()),
                u32::from_be_bytes(b[20..24].try_into().unwrap()),
            )
        } else {
            let text = std::str::from_utf8(b).map_err(|_| "not a picture")?;
            let (w, h) = text.trim().split_once('x').ok_or("not a picture")?;
            (w.parse().unwrap(), h.parse().unwrap())
        };
        if w > max_w || h > max_h {
            return Err("too large".into());
        }
        Ok((w, h, [10, 20, 30, 255].repeat((w * h) as usize)))
    }
}

const CLASSIC: &str = include_str!("../../../../skins/classic/skin.json");

fn classic_files() -> Mem {
    macro_rules! file {
        ($m:expr, $name:literal) => {
            $m.with(
                $name,
                include_bytes!(concat!("../../../../skins/classic/", $name)),
            )
        };
    }
    let m = Mem::default();
    let m = file!(m, "fonts/OpenSans-Bold.ttf");
    let m = file!(m, "fonts/OpenSans-Regular.ttf");
    let m = file!(m, "fonts/OpenSans-SemiBold.ttf");
    let m = file!(m, "images/titlebar.png");
    let m = file!(m, "images/highlight.png");
    let m = file!(m, "images/track.png");
    let m = file!(m, "images/fill.png");
    let m = file!(m, "images/battery.png");
    let m = file!(m, "images/battery_fill.png");
    file!(m, "images/battery_low.png")
}

fn classic() -> Skin {
    compile("classic", CLASSIC, &classic_files()).expect("Classic compiles")
}

/// A small valid skin with `screens` (a JSON object) as its layouts.
fn manifest(screens: &str) -> String {
    format!(
        r##"{{"format": 1, "name": "Test", "author": "Tester",
  "palette": {{"bg": "#FFFFFF", "text": "#000000", "accent": "#2A69D2"}},
  "colors": {{"white": "#FFFFFF"}},
  "images": {{"bar": "images/bar.png"}},
  "parts": {{"bar": [{{"type": "image", "x": 0, "y": 0, "w": 480, "h": 30, "image": "bar"}}]}},
  "screens": {screens}}}"##
    )
}

const LIST: &str = r##"{"list": [
  {"type": "part", "name": "bar"},
  {"type": "text", "x": 0, "y": 0, "w": 480, "h": 30, "text": "{screen}", "align": "center"},
  {"type": "list", "x": 0, "y": 30, "w": 480, "rows": 4, "row_height": 40,
   "row": [{"type": "text", "x": 10, "y": 0, "w": 300, "h": 40, "text": "{label}"}],
   "focused": [{"type": "rect", "x": 0, "y": 0, "w": 480, "h": 40, "color": "accent"},
               {"type": "text", "x": 10, "y": 0, "w": 300, "h": 40, "text": "{label}", "color": "white"}]}
]}"##;

fn files() -> Mem {
    Mem::default().with("images/bar.png", b"8x30")
}

fn compile_test(screens: &str) -> Result<Skin, String> {
    compile("test", &manifest(screens), &files())
}

fn model(screen: Screen) -> AppModel {
    let mut m = AppModel {
        screen,
        ..Default::default()
    };
    m.sources = vec![Source {
        id: "internal".into(),
        kind: reborn_core::MediaSource::Internal,
        root: "/data/music".into(),
        online: true,
        mount: "internal".into(),
        mount_id: None,
    }];
    m
}

fn library(n: usize) -> Vec<Track> {
    (0..n)
        .map(|i| Track {
            id: i as i64,
            source_id: "internal".into(),
            title: format!("Song {i:02}"),
            artist: "Artist".into(),
            album: "Album".into(),
            path: format!("/data/music/a/{i}.flac").into(),
            duration_ms: 200_000,
            online: true,
            ..Default::default()
        })
        .collect()
}

fn skinned(skin: Skin) -> Ui {
    Ui {
        skin: Some(std::rc::Rc::new(skin)),
        ..Default::default()
    }
}

#[test]
fn the_classic_skin_compiles_cleanly_and_fits_its_atlas() {
    let skin = classic();
    assert_eq!(skin.name, "Classic");
    assert!(skin.warnings.is_empty(), "{:?}", skin.warnings);
    for key in [
        "home",
        "list",
        "page",
        "now_playing",
        "sheet",
        "toast",
        "volume",
        "cover_flow",
    ] {
        assert!(skin.layout(key).is_some(), "{key}");
    }
    assert_eq!(skin.atlas_width, 1024);
    assert!(skin.atlas_height <= 1024 && skin.atlas_height.is_power_of_two());
    assert_eq!(skin.atlas.len(), skin.atlas_width * skin.atlas_height * 4);
    assert_eq!(skin.fonts.len(), 6);
    for font in &skin.fonts {
        for g in font.glyphs.values() {
            assert!(usize::from(g.src[0] + g.src[2]) <= skin.atlas_width);
            assert!(usize::from(g.src[1] + g.src[3]) <= skin.atlas_height);
        }
        assert!(font.glyphs.contains_key(&'é') && font.glyphs.contains_key(&'…'));
    }
}

#[test]
fn strict_validation_names_the_problem() {
    let cases = [
        (
            r##"{"list": [{"type": "rect", "w": 1, "h": 1, "color": "text", "image": "bar"}]}"##,
            "has no field \"image\"",
        ),
        (
            r##"{"list": [{"type": "rect", "w": 1, "h": 1, "colour": "text"}]}"##,
            "unknown field `colour`",
        ),
        (
            r##"{"list": [{"type": "sparkle"}]}"##,
            "unknown type \"sparkle\"",
        ),
        (
            r##"{"list": [{"type": "rect", "w": 1, "h": 1, "color": "#12345"}]}"##,
            "unknown colour",
        ),
        (
            r##"{"list": [{"type": "rect", "w": 1, "h": 1, "color": "text", "if": "playing & dancing"}]}"##,
            "unknown condition \"dancing\"",
        ),
        (
            r##"{"list": [{"type": "rect", "w": 1, "h": 1, "color": "text", "if": "focused"}]}"##,
            "only known inside list rows",
        ),
        (
            r##"{"list": [{"type": "text", "w": 1, "h": 1, "text": "{label}"}]}"##,
            "only known inside list rows",
        ),
        (
            r##"{"list": [{"type": "text", "w": 1, "h": 1, "text": "{lyrics}"}]}"##,
            "unknown value \"{lyrics}\"",
        ),
        (
            r##"{"list": [{"type": "text", "w": 1, "h": 1, "text": "x", "font": "Comic"}]}"##,
            "no font named \"Comic\"",
        ),
        (
            r##"{"list": [{"type": "image", "w": 1, "h": 1, "image": "nope"}]}"##,
            "no image named \"nope\"",
        ),
        (
            r##"{"list": [{"type": "part", "name": "nope"}]}"##,
            "no part named \"nope\"",
        ),
        (
            r##"{"list": [{"type": "icon", "size": 10, "icon": "rocket"}]}"##,
            "unknown icon",
        ),
        (
            r##"{"list": [{"type": "list", "w": 10, "rows": 0, "row_height": 20, "row": []}]}"##,
            "\"rows\" is 1 to 24",
        ),
        (
            r##"{"list": [{"type": "list", "w": 10, "rows": 2, "row_height": 20, "row": []}]}"##,
            "needs at least one element",
        ),
        (
            r##"{"list": [{"type": "art", "size": 10, "source": "row"}]}"##,
            "only for list rows",
        ),
        (r##"{"lyrics": []}"##, "unknown screen \"lyrics\""),
        (
            r##"{"list": [{"type": "rect", "w": -1, "h": 1, "color": "text"}]}"##,
            "negative size",
        ),
        (
            r##"{"list": [{"type": "image", "w": 9, "h": 9, "image": "bar", "slice": [4, 1, 4, 1]}]}"##,
            "leaves no middle",
        ),
        (
            r##"{"list": [{"type": "rect", "w": "wide", "h": 1, "color": "text"}]}"##,
            "invalid type",
        ),
    ];
    for (screens, needle) in cases {
        let error = compile_test(screens).expect_err(screens);
        assert!(error.contains(needle), "{needle:?} not in {error:?}");
    }
    assert!(compile_test(LIST).is_ok());
}

#[test]
fn files_are_named_inside_the_skin_and_within_limits() {
    for (file, needle) in [
        ("../outside.png", "must be a path inside"),
        ("/etc/passwd", "must be a path inside"),
        ("images/./bar.png", "must be a path inside"),
        ("images/missing.png", "is missing"),
        ("images/huge.png", "cannot be used"),
    ] {
        let text = manifest(LIST).replace("images/bar.png", file);
        let error = compile("t", &text, &files().with("images/huge.png", b"481x10")).unwrap_err();
        assert!(error.contains(needle), "{file}: {error}");
    }
    assert!(relative_name("fonts/a.ttf") && !relative_name("a//b") && !relative_name(""));
}

#[test]
fn unreadable_palettes_and_other_formats_are_refused() {
    let text = manifest(LIST);
    let with = |a: &str, b: &str| compile("t", &text.replace(a, b), &files());
    assert!(with("\"#000000\"", "\"#EEEEEE\"")
        .unwrap_err()
        .contains("contrast"));
    let warned = with("\"#000000\"", "\"#777777\"").unwrap();
    assert!(warned.warnings.iter().any(|w| w.contains("contrast")));
    assert!(with("\"format\": 1", "\"format\": 2")
        .unwrap_err()
        .contains("format 2"));
    assert!(with("\"bg\"", "\"wallpaper\"")
        .unwrap_err()
        .contains("unknown token"));
}

#[test]
fn parts_cannot_include_themselves() {
    let text = manifest(r##"{"list": [{"type": "part", "name": "loop"}]}"##).replace(
        r##""parts": {"##,
        r##""parts": {"loop": [{"type": "part", "name": "loop"}], "##,
    );
    assert!(compile("t", &text, &files())
        .unwrap_err()
        .contains("includes itself"));
}

#[test]
fn describe_reads_the_header_and_flags_other_formats() {
    let info = describe(
        "neon",
        "/x/neon".into(),
        Ok(r#"{"format": 9, "name": "Neon Nights", "author": "A", "screens": {}}"#),
    );
    assert_eq!(info.name, "Neon Nights");
    assert!(info.problem.unwrap().contains("format 9"));
    let info = describe("broken", "/x".into(), Err("skin.json is missing".into()));
    assert_eq!(info.name, "broken");
    assert!(info.problem.is_some());
    assert!(valid_id("Classic 2") && !valid_id("../x") && !valid_id(".hidden"));
}

#[test]
fn a_skin_draws_its_layout_with_one_focus_and_atlas_bounded_quads() {
    let mut ui = skinned(classic());
    let skin = ui.skin.clone().unwrap();
    let tracks = library(30);
    for screen in [
        Screen::Home,
        Screen::Tracks,
        Screen::Settings,
        Screen::Update,
        Screen::Theme,
    ] {
        let mut m = model(screen);
        m.navigation.focus = 1;
        let quads = ui.draw(&m, &tracks, false);
        assert_eq!(crate::focus_target_count(&quads), 1, "{screen:?}");
        for q in quads.iter().filter(|q| q.tex == reborn_graphics::Tex::Skin) {
            assert!(usize::from(q.src[0] + q.src[2]) <= skin.atlas_width);
            assert!(usize::from(q.src[1] + q.src[3]) <= skin.atlas_height);
        }
        assert!(
            quads.iter().any(|q| q.tex == reborn_graphics::Tex::Skin),
            "{screen:?}"
        );
    }
    // The background is the skin's, and Classic's fixed accent replaces the cover's.
    let quads = ui.draw(&model(Screen::Home), &tracks, false);
    assert_eq!(quads[0].color, 0xFFFF_FFFF);
    ui.accent = 0xFF00_00FF;
    assert_eq!(ui.accent(), 0x2A69_D2FF);
}

#[test]
fn the_list_scrolls_by_the_skins_row_count() {
    let mut ui = skinned(compile_test(LIST).unwrap());
    let tracks = library(30);
    let mut m = model(Screen::Tracks);
    for _ in 0..6 {
        ui.action(&mut m, &tracks, reborn_core::Action::NavigateDown);
    }
    assert_eq!(m.navigation.focus, 6);
    // Four rows: the window starts three above the focus.
    assert_eq!(m.navigation.scroll, 3);
    let quads = ui.draw(&m, &tracks, false);
    assert_eq!(crate::focus_target_count(&quads), 1);
}

#[test]
fn screens_a_skin_leaves_out_are_drawn_by_reborn_in_its_palette() {
    let ui = skinned(compile_test(LIST).unwrap());
    let mut m = model(Screen::NowPlaying);
    m.replace_queue(library(2), 0).unwrap();
    m.playback = PlaybackState::Playing;
    let quads = ui.draw(&m, &[], false);
    let text_rgb = theme::color::TEXT >> 8;
    assert!(
        !quads
            .iter()
            .any(|q| q.tex != reborn_graphics::Tex::Art && q.color >> 8 == text_rgb),
        "Reborn's text colour is replaced by the skin's"
    );
    assert!(
        quads.iter().any(|q| q.color == 0x0000_00FF),
        "black text on white"
    );
    assert_eq!(crate::focus_target_count(&quads), 1);
}

#[test]
fn cover_flow_shelf_fades_into_a_light_skin_background() {
    // Y2Reborn #57: the shelf under the covers is a warped quad drawn with
    // its corner colours; they follow the palette as plain quads do.
    let ui = Ui {
        motion_fixed: true,
        ..skinned(compile_test(LIST).unwrap())
    };
    let tracks = library(3);
    let quads = ui.draw(&model(Screen::Albums), &tracks, false);
    let shelves: Vec<_> = quads
        .iter()
        .filter(|q| q.tex == reborn_graphics::Tex::Solid)
        .filter_map(|q| q.warp)
        .collect();
    assert!(!shelves.is_empty(), "Cover Flow draws its shelf");
    let bg_rgb = theme::color::BG >> 8;
    for w in &shelves {
        assert!(
            w.colors.iter().all(|c| c >> 8 != bg_rgb),
            "no corner keeps Reborn's dark background: {:08X?}",
            w.colors
        );
    }
    assert!(
        shelves
            .iter()
            .any(|w| w.colors[2] == 0xFFFF_FFFF && w.colors[0] == 0xFFFF_FF00),
        "the shelf fades from clear to the skin's white background"
    );
    // Artwork keeps its own tint.
    let mut c = crate::canvas::Canvas::default();
    c.draw.push(reborn_graphics::Quad {
        tex: reborn_graphics::Tex::Art,
        warp: Some(reborn_graphics::Warp {
            corners: [[0., 0.]; 4],
            colors: [theme::color::BG; 4],
        }),
        ..reborn_graphics::Quad::rect(0., 0., 1., 1., theme::color::BG)
    });
    remap(&mut c, 0, &ui.skin.as_ref().unwrap().palette, 0x2A69_D2FF);
    assert_eq!(c.draw[0].warp.unwrap().colors, [theme::color::BG; 4]);
}

#[test]
fn without_a_skin_nothing_changes() {
    let ui = Ui::default();
    let tracks = library(5);
    let quads = ui.draw(&model(Screen::Home), &tracks, false);
    assert_eq!(quads[0].color, theme::color::BG);
    assert!(!quads.iter().any(|q| q.tex == reborn_graphics::Tex::Skin));
}

#[test]
fn the_picker_lists_skins_and_selecting_one_asks_for_it() {
    let mut ui = Ui {
        themes: vec![
            SkinInfo {
                id: "classic".into(),
                name: "Classic".into(),
                author: "Reborn".into(),
                ..Default::default()
            },
            SkinInfo {
                id: "broken".into(),
                name: "Broken".into(),
                problem: Some("Made for skin format 9".into()),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let tracks = library(1);
    // Beta 7.5: Skin is a place of Settings itself, no longer in Display.
    let mut m = model(Screen::SettingsDisplay);
    assert!(!ui.rows(&m, &tracks).iter().any(|r| r.key == "theme"));
    m.screen = Screen::Settings;
    let rows = ui.rows(&m, &tracks);
    let theme_row = rows
        .iter()
        .position(|r| r.key == "theme")
        .expect("Skin row");
    assert_eq!(rows[theme_row].label, "Skin");
    assert_eq!(rows[theme_row].secondary, "Reborn");
    m.navigation.focus = theme_row;
    assert!(matches!(
        ui.action(&mut m, &tracks, reborn_core::Action::Select),
        Effect::ScanThemes
    ));
    assert_eq!(m.screen, Screen::Theme);
    let rows: Vec<Item> = ui.rows(&m, &tracks);
    assert_eq!(rows[0].key, "theme_set:");
    assert_eq!(rows[0].kind, crate::RowKind::Check(true));
    // No author line (UI v4): the picker shows each look's swatch instead.
    assert_eq!(rows[1].secondary, "");
    assert!(!rows[2].enabled && rows[2].secondary.contains("format 9"));
    m.navigation.focus = 1;
    match ui.action(&mut m, &tracks, reborn_core::Action::Select) {
        Effect::SetTheme(id) => assert_eq!(id, "classic"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_overlays_use_the_skins_layouts() {
    let mut ui = skinned(classic());
    let tracks = library(3);
    let mut m = model(Screen::Settings);
    m.navigation.modal = Some(reborn_core::Modal::Confirm(
        reborn_core::ConfirmAction::ResetSettings,
    ));
    let quads = ui.draw(&m, &tracks, false);
    assert_eq!(crate::focus_target_count(&quads), 1);
    // The Classic dialog box is white, not recoloured.
    assert!(quads
        .iter()
        .any(|q| q.color == 0xFFFF_FFFF && q.w >= 300. && q.h >= 200.));
    m.navigation.modal = None;
    ui.flash("Settings reset");
    let quads = ui.draw(&m, &tracks, false);
    assert!(
        quads.iter().any(|q| q.color == 0x2B2E_33EB),
        "Classic toast"
    );
}

#[test]
fn display_fonts_can_carry_only_ascii_and_text_can_be_lowercased() {
    let files = classic_files().with("images/bar.png", b"8x30");
    let text = manifest(
        r##"{"list": [{"type": "text", "w": 400, "h": 60, "text": "{screen}", "font": "huge", "case": "lower"}]}"##,
    )
    .replace(
        r##""images":"##,
        r##""fonts": {"huge": {"file": "fonts/OpenSans-Bold.ttf", "size": 64, "charset": "ascii"}}, "images":"##,
    );
    let skin = compile("t", &text, &files).unwrap();
    let huge = &skin.fonts[0];
    assert!(huge.glyphs.contains_key(&'a') && !huge.glyphs.contains_key(&'é'));
    let quads = skinned(skin).draw(&model(Screen::Settings), &library(1), false);
    assert!(quads.iter().any(|q| q.tex == reborn_graphics::Tex::Skin));
    let bad = text.replace("\"ascii\"", "\"klingon\"");
    assert!(compile("t", &bad, &files)
        .unwrap_err()
        .contains("latin or ascii"));
    let bad = text.replace("\"lower\"", "\"title\"");
    assert!(compile("t", &bad, &files)
        .unwrap_err()
        .contains("normal, upper or lower"));
}

#[test]
fn up_next_marks_the_song_playing_not_the_row_at_the_play_position() {
    // A marker beside the row playing; the history and headings come first (#60).
    let screens = LIST.replace(
        r#""row": [{"type": "text""#,
        r#""row": [{"type": "rect", "if": "current", "x": 400, "y": 0, "w": 10, "h": 40, "color": "accent"},
                   {"type": "text""#,
    );
    let ui = skinned(compile_test(&screens).unwrap());
    let tracks = library(8);
    let mut m = model(Screen::Queue);
    m.queue = tracks[2..8].to_vec();
    m.queue_entry_ids = (0..6).map(|i| reborn_core::QueueEntryId(100 + i)).collect();
    m.queue_position = 1;
    m.history = tracks[..2]
        .iter()
        .map(|t| reborn_core::queue::HistoryEntry {
            source_id: t.source_id.clone(),
            path: t.path.clone(),
        })
        .collect();
    m.navigation.focus = 0;
    let marks: Vec<f32> = ui
        .draw(&m, &tracks, false)
        .iter()
        .filter(|q| q.x == 400. && q.w == 10.)
        .map(|q| q.y)
        .collect();
    // The History heading, its two songs, then the song playing: row 3.
    assert_eq!(marks, vec![30. + 3. * 40.]);
}

#[test]
fn skins_say_when_the_sleep_timer_runs() {
    // #29: `{sleep}` and `"if": "sleep"`; every shipped skin joins it to its output line.
    let screens = r##"{"now_playing": [
  {"type": "text", "if": "!sleep", "x": 0, "y": 300, "w": 480, "h": 30, "text": "{output}"},
  {"type": "text", "if": "sleep", "x": 0, "y": 300, "w": 480, "h": 30, "text": "{output} · {sleep}"}
]}"##;
    let ui = skinned(compile_test(screens).unwrap());
    let tracks = library(4);
    let mut m = model(Screen::NowPlaying);
    m.queue = tracks.clone();
    let text = |ui: &Ui, m: &AppModel| crate::tests::drawn_text(&ui.draw(m, &tracks, false));
    assert!(!text(&ui, &m).contains("Sleeps"));
    m.sleep_timer = Some(reborn_core::SleepTimer {
        choice: reborn_core::SleepChoice::Minutes(30),
        until: Some(std::time::Instant::now() + std::time::Duration::from_secs(9 * 60 + 10)),
        item: m.sleep_item(),
    });
    assert!(
        text(&ui, &m).contains("Sleeps in 10 min"),
        "{}",
        text(&ui, &m)
    );
    for json in [
        include_str!("../../../../skins/big-type/skin.json"),
        include_str!("../../../../skins/bunny-bow/skin.json"),
        include_str!("../../../../skins/cassette/skin.json"),
        include_str!("../../../../skins/classic/skin.json"),
        include_str!("../../../../skins/mini-blue/skin.json"),
        include_str!("../../../../skins/mini-gold/skin.json"),
        include_str!("../../../../skins/mini-green/skin.json"),
        include_str!("../../../../skins/mini-pink/skin.json"),
        include_str!("../../../../skins/mini-silver/skin.json"),
        include_str!("../../../../skins/nano-glass/skin.json"),
        include_str!("../../../../skins/neon/skin.json"),
        include_str!("../../../../skins/paper/skin.json"),
        include_str!("../../../../skins/terminal/skin.json"),
        include_str!("../../../../skins/walkman/skin.json"),
        include_str!("../../../../skins/winamp/skin.json"),
        include_str!("../../../../skins/zune/skin.json"),
    ] {
        assert!(
            json.contains("{output} · {sleep}"),
            "a shipped skin without the timer"
        );
    }
}

#[test]
fn indicators_show_charging_and_the_radios_right_to_left_and_close_up() {
    // Y2Linux #70, #73: skins showed only the battery and the clock.
    let screens = r##"{"now_playing": [
  {"type": "indicators", "x": 440, "y": 7, "size": 16, "gap": 4,
   "color": "#101010", "dim": "#888888", "charge": "#00AA00"}
]}"##;
    let mut ui = skinned(compile_test(screens).unwrap());
    let mut m = model(Screen::NowPlaying);
    // The centre of the icon drawn in one colour, if any.
    let centre = |ui: &Ui, m: &AppModel, colour: u32| {
        let quads: Vec<_> = ui
            .draw(m, &[], false)
            .into_iter()
            .filter(|q| q.color == colour)
            .collect();
        (!quads.is_empty())
            .then(|| quads.iter().map(|q| q.x + q.w / 2.).sum::<f32>() / quads.len() as f32)
    };
    let (on, dim, charge) = (0x1010_10FF, 0x8888_88FF, 0x00AA_00FF);
    assert_eq!(
        centre(&ui, &m, on),
        None,
        "radios off, not charging: nothing"
    );
    m.platform.battery.charging = reborn_core::platform::ChargingState::Charging;
    ui.bluetooth.powered = true;
    ui.bluetooth.devices = vec![crate::BluetoothDeviceView {
        connected: true,
        ..Default::default()
    }];
    ui.wifi.powered = true;
    ui.wifi.status = crate::WifiStatus::Disconnected;
    let (bolt, bluetooth, wifi) = (
        centre(&ui, &m, charge).unwrap(),
        centre(&ui, &m, on).unwrap(),
        centre(&ui, &m, dim).unwrap(),
    );
    assert!((bolt - 432.).abs() < 2., "{bolt}");
    assert!((bluetooth - 412.).abs() < 2., "{bluetooth}");
    assert!((wifi - 392.).abs() < 2., "{wifi}");
    // Not charging: Bluetooth moves into the bolt's place.
    m.platform.battery.charging = reborn_core::platform::ChargingState::OnBattery;
    assert!((centre(&ui, &m, on).unwrap() - 432.).abs() < 2.);
}
