//! Small, reusable presentation primitives shared by every Reborn screen.

use crate::{Item, PowerView, RadioView};
use reborn_core::{AppModel, AudioOutput, PlaybackState};
use reborn_graphics::Quad;

use crate::theme::{color, layout, radius, space, stroke, type_scale};

const FONT_ADVANCE_RATIO: f32 = 0.64;

pub struct Canvas {
    pub draw: Vec<Quad>,
}

impl Canvas {
    pub fn new() -> Self {
        Self {
            draw: Vec::with_capacity(4096),
        }
    }

    pub fn finish(self) -> Vec<Quad> {
        self.draw
    }

    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, fill: u32) {
        self.draw.push(Quad::rect(x, y, w, h, fill));
    }

    pub fn panel(&mut self, x: f32, y: f32, w: f32, h: f32, fill: u32, border: u32) {
        rounded_rect(&mut self.draw, x, y, w, h, radius::MEDIUM, border);
        rounded_rect(
            &mut self.draw,
            x + stroke::HAIRLINE,
            y + stroke::HAIRLINE,
            w - stroke::HAIRLINE * 2.0,
            h - stroke::HAIRLINE * 2.0,
            (radius::MEDIUM - stroke::HAIRLINE).max(0.0),
            fill,
        );
    }

    pub fn focus_panel(&mut self, x: f32, y: f32, w: f32, h: f32, focused: bool) {
        self.panel(
            x,
            y,
            w,
            h,
            if focused {
                color::FOCUS_FILL
            } else {
                color::SURFACE
            },
            if focused {
                color::ACCENT_GOLD_BRIGHT
            } else {
                color::SURFACE_BORDER
            },
        );
    }

    pub fn text(&mut self, x: f32, y: f32, value: &str, scale: f32, tint: u32) {
        text(&mut self.draw, x, y, value, scale, tint, false);
    }

    pub fn display(&mut self, x: f32, y: f32, value: &str, scale: f32, tint: u32) {
        text(&mut self.draw, x, y, value, scale, tint, true);
    }

    pub fn micro(&mut self, x: f32, y: f32, value: &str, tint: u32) {
        let mut cursor = x;
        for ch in value.chars().take(48) {
            let upper = ch.to_ascii_uppercase().to_string();
            self.text(cursor, y, &upper, type_scale::MICRO, tint);
            cursor += 8.0 * type_scale::MICRO * FONT_ADVANCE_RATIO;
        }
    }

    pub fn centered(&mut self, x: f32, y: f32, value: &str, scale: f32, tint: u32) {
        let width = value.chars().count() as f32 * 8.0 * scale * FONT_ADVANCE_RATIO;
        self.text(x - width / 2.0, y, value, scale, tint);
    }

    pub fn icon(&mut self, name: &str, x: f32, y: f32, size: f32, tint: u32) {
        if let Some(index) = icon_index(name) {
            let mut q = Quad::rect(x, y, size, size, tint);
            q.icon = Some(index);
            self.draw.push(q);
        }
    }

    pub fn artwork(&mut self, x: f32, y: f32, size: f32, has_art: bool) {
        if has_art {
            let mut q = Quad::rect(x, y, size, size, color::TEXT_PRIMARY);
            q.artwork = true;
            self.draw.push(q);
        } else {
            rounded_rect(
                &mut self.draw,
                x,
                y,
                size,
                size,
                radius::SMALL,
                color::BG_RAISED,
            );
            self.icon(
                "albums",
                x + size * 0.25,
                y + size * 0.25,
                size * 0.5,
                color::ACCENT_GOLD_DIM,
            );
        }
    }

    pub fn badge(&mut self, x: f32, y: f32, width: f32, label: &str, tint: u32) {
        rounded_rect(
            &mut self.draw,
            x,
            y,
            width,
            20.0,
            radius::SMALL,
            color::SURFACE_HOVER,
        );
        rounded_rect(
            &mut self.draw,
            x,
            y,
            width,
            stroke::HAIRLINE,
            radius::SMALL,
            tint,
        );
        self.centered(x + width / 2.0, y + 5.0, label, type_scale::MICRO, tint);
    }

    pub fn toggle(&mut self, x: f32, y: f32, on: bool) {
        rounded_rect(
            &mut self.draw,
            x,
            y,
            34.0,
            18.0,
            9.0,
            if on {
                color::ACCENT_GOLD
            } else {
                color::SURFACE_BORDER
            },
        );
        circle(
            &mut self.draw,
            if on { x + 26.0 } else { x + 8.0 },
            y + 9.0,
            6.0,
            if on {
                color::TEXT_PRIMARY
            } else {
                color::TEXT_MUTED
            },
        );
    }

    pub fn progress(&mut self, x: f32, y: f32, width: f32, value: f32) {
        self.rect(x, y, width, 4.0, color::TRACK);
        self.rect(x, y, width * value.clamp(0.0, 1.0), 4.0, color::ACCENT_GOLD);
        circle(
            &mut self.draw,
            x + width * value.clamp(0.0, 1.0),
            y + 2.0,
            4.0,
            color::ACCENT_GOLD_BRIGHT,
        );
    }
}

pub fn screen_background(c: &mut Canvas, has_art: bool, artwork_allowed: bool) {
    if has_art && artwork_allowed {
        let mut backdrop = Quad::rect(0.0, layout::STATUS_H, 480.0, 288.0, color::TEXT_PRIMARY);
        backdrop.artwork = true;
        c.draw.push(backdrop);
        c.rect(0.0, layout::STATUS_H, 480.0, 288.0, color::SCRIM);
        c.rect(0.0, 222.0, 480.0, 138.0, color::SCRIM_STRONG);
    } else {
        c.rect(0.0, 0.0, 480.0, 360.0, color::BG);
    }
}

pub fn status_bar(
    c: &mut Canvas,
    m: &AppModel,
    power: PowerView,
    section: &str,
    wifi: &RadioView,
    bt: &RadioView,
) {
    c.rect(0.0, 0.0, 480.0, layout::STATUS_H, color::BG_RAISED);
    c.text(
        space::LG,
        7.0,
        "Reborn",
        type_scale::SECONDARY,
        color::TEXT_PRIMARY,
    );
    c.rect(70.0, 8.0, 1.0, 14.0, color::SURFACE_BORDER);
    c.text(82.0, 7.0, "Y2", type_scale::SECONDARY, color::TEXT_MUTED);
    c.centered(
        240.0,
        6.0,
        section,
        type_scale::SECONDARY,
        color::ACCENT_GOLD,
    );
    let underline = (section.chars().count() as f32 * 8.0).clamp(28.0, 72.0);
    c.rect(
        240.0 - underline / 2.0,
        27.0,
        underline,
        2.0,
        color::ACCENT_GOLD,
    );

    c.icon("headphones", 316.0, 7.0, 15.0, color::TEXT_PRIMARY);
    c.text(
        336.0,
        8.0,
        output_short(&m.output),
        type_scale::MICRO,
        color::TEXT_SECONDARY,
    );
    if bt.powered {
        c.icon("bluetooth", 375.0, 7.0, 15.0, color::ACCENT_GOLD);
    }
    if wifi.powered {
        c.icon("wifi", 396.0, 7.0, 15.0, color::SUCCESS);
    }
    c.icon(
        "battery",
        420.0,
        7.0,
        15.0,
        if power.charging {
            color::ACCENT_GOLD
        } else {
            color::TEXT_PRIMARY
        },
    );
    c.text(
        440.0,
        8.0,
        &power
            .battery_percent
            .map(|value| format!("{value}%"))
            .unwrap_or_else(|| "--%".into()),
        type_scale::MICRO,
        color::TEXT_SECONDARY,
    );
    c.rect(
        0.0,
        layout::STATUS_H - 1.0,
        480.0,
        1.0,
        color::SURFACE_BORDER,
    );
}

pub fn bottom_info(c: &mut Canvas, m: &AppModel, has_art: bool) {
    c.rect(0.0, 300.0, 480.0, 60.0, color::BG_RAISED);
    c.rect(space::LG, 300.0, 448.0, 1.0, color::SURFACE_BORDER);
    if let Some(track) = m.current() {
        c.artwork(space::LG, 308.0, 38.0, has_art);
        c.text(
            64.0,
            308.0,
            &fit(&track.title, 24),
            type_scale::SECONDARY,
            color::TEXT_PRIMARY,
        );
        c.text(
            64.0,
            326.0,
            &fit(&track.artist, 24),
            type_scale::MICRO,
            color::TEXT_MUTED,
        );
        c.rect(190.0, 306.0, 1.0, 37.0, color::SURFACE_BORDER);
        c.icon("headphones", 206.0, 310.0, 15.0, color::TEXT_PRIMARY);
        c.text(
            228.0,
            308.0,
            &output_label(&m.output),
            type_scale::MICRO,
            color::TEXT_SECONDARY,
        );
        c.text(228.0, 327.0, "4.4 mm", type_scale::MICRO, color::TEXT_MUTED);
        c.rect(340.0, 306.0, 1.0, 37.0, color::SURFACE_BORDER);
        c.icon("gain", 357.0, 311.0, 15.0, color::TEXT_PRIMARY);
        c.text(
            378.0,
            308.0,
            "High Gain",
            type_scale::MICRO,
            color::TEXT_SECONDARY,
        );
        c.text(
            378.0,
            327.0,
            "Class AB",
            type_scale::MICRO,
            color::TEXT_MUTED,
        );
    } else {
        c.centered(
            240.0,
            319.0,
            "Choose music to begin",
            type_scale::SECONDARY,
            color::TEXT_MUTED,
        );
    }
    c.centered(
        240.0,
        349.0,
        "LISTEN DEEPER / REBORN",
        type_scale::MICRO,
        color::TEXT_MUTED,
    );
}

pub fn list_row(
    c: &mut Canvas,
    row: &Item,
    x: f32,
    y: f32,
    width: f32,
    focused: bool,
    icon_name: &str,
) {
    c.focus_panel(x, y, width, layout::ROW_H, focused);
    c.icon(
        icon_name,
        x + space::MD,
        y + 13.0,
        18.0,
        if focused {
            color::ACCENT_GOLD
        } else {
            color::TEXT_SECONDARY
        },
    );
    c.text(
        x + 42.0,
        y + 9.0,
        &fit(&row.label, 24),
        type_scale::ROW,
        if focused {
            color::TEXT_PRIMARY
        } else {
            color::TEXT_SECONDARY
        },
    );
    if !row.secondary.is_empty() {
        c.text(
            x + 42.0,
            y + 27.0,
            &fit(&row.secondary, 25),
            type_scale::MICRO,
            if focused {
                color::ACCENT_GOLD
            } else {
                color::TEXT_MUTED
            },
        );
    }
    c.icon(
        "chevron_right",
        x + width - 27.0,
        y + 13.0,
        18.0,
        if focused {
            color::ACCENT_GOLD_BRIGHT
        } else {
            color::TEXT_MUTED
        },
    );
}

pub fn setting_row(c: &mut Canvas, row: &Item, x: f32, y: f32, width: f32, focused: bool) {
    c.focus_panel(x, y, width, layout::ROW_H, focused);
    c.icon(
        row_icon(&row.key),
        x + space::MD,
        y + 13.0,
        18.0,
        if focused {
            color::ACCENT_GOLD
        } else {
            color::TEXT_SECONDARY
        },
    );
    let is_toggle = matches!(row.secondary.as_str(), "On" | "Off");
    c.text(
        x + 42.0,
        y + 9.0,
        &fit(&row.label, if is_toggle { 18 } else { 12 }),
        type_scale::ROW,
        if focused {
            color::TEXT_PRIMARY
        } else {
            color::TEXT_SECONDARY
        },
    );
    if is_toggle {
        c.toggle(x + width - 55.0, y + 13.0, row.secondary == "On");
    } else if !row.secondary.is_empty() {
        c.text(
            x + width - 126.0,
            y + 15.0,
            &fit(&row.secondary, 13),
            type_scale::MICRO,
            if focused {
                color::ACCENT_GOLD
            } else {
                color::TEXT_MUTED
            },
        );
    }
    c.icon(
        "chevron_right",
        x + width - 27.0,
        y + 13.0,
        18.0,
        if focused {
            color::ACCENT_GOLD_BRIGHT
        } else {
            color::TEXT_MUTED
        },
    );
}

pub fn empty_state(c: &mut Canvas, x: f32, y: f32, width: f32, title: &str, message: &str) {
    c.panel(x, y, width, 112.0, color::BG_RAISED, color::SURFACE_BORDER);
    c.icon("albums", x + 20.0, y + 29.0, 28.0, color::ACCENT_GOLD_DIM);
    c.text(
        x + 62.0,
        y + 28.0,
        title,
        type_scale::SECTION,
        color::TEXT_PRIMARY,
    );
    c.text(
        x + 62.0,
        y + 58.0,
        &fit(message, 34),
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
}

pub fn dialog(c: &mut Canvas, title: &str, body: &str, rows: &[Item], focus: usize) {
    c.rect(0.0, 0.0, 480.0, 360.0, color::SCRIM_STRONG);
    c.panel(
        76.0,
        91.0,
        328.0,
        178.0,
        color::BG_RAISED,
        color::ACCENT_GOLD,
    );
    c.display(
        100.0,
        111.0,
        title,
        type_scale::SECTION,
        color::TEXT_PRIMARY,
    );
    c.text(
        100.0,
        144.0,
        &fit(body, 38),
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
    for (index, row) in rows.iter().enumerate() {
        let y = 178.0 + index as f32 * 37.0;
        c.focus_panel(100.0, y, 280.0, 32.0, index == focus);
        c.text(
            118.0,
            y + 8.0,
            &row.label,
            type_scale::ROW,
            if index == focus {
                color::TEXT_PRIMARY
            } else {
                color::TEXT_SECONDARY
            },
        );
    }
}

pub fn toast(c: &mut Canvas, message: &str) {
    c.panel(
        64.0,
        272.0,
        352.0,
        34.0,
        color::BG_RAISED,
        color::ACCENT_GOLD,
    );
    c.centered(
        240.0,
        282.0,
        &fit(message, 46),
        type_scale::SECONDARY,
        color::TEXT_PRIMARY,
    );
}

pub fn volume_overlay(c: &mut Canvas, volume: u8) {
    c.panel(
        146.0,
        246.0,
        188.0,
        55.0,
        color::BG_RAISED,
        color::ACCENT_GOLD,
    );
    c.icon("volume", 162.0, 263.0, 18.0, color::ACCENT_GOLD);
    c.text(190.0, 258.0, "Volume", type_scale::MICRO, color::TEXT_MUTED);
    c.text(
        190.0,
        274.0,
        &format!("{volume}%"),
        type_scale::ROW,
        color::TEXT_PRIMARY,
    );
    c.progress(244.0, 271.0, 70.0, volume as f32 / 100.0);
}

pub fn now_playing_controls(c: &mut Canvas, m: &AppModel) {
    let active = matches!(
        m.playback,
        PlaybackState::Playing | PlaybackState::Buffering
    );
    c.icon("shuffle", 74.0, 256.0, 25.0, color::TEXT_SECONDARY);
    c.icon("previous", 148.0, 254.0, 28.0, color::TEXT_PRIMARY);
    circle(&mut c.draw, 240.0, 268.0, 31.0, color::ACCENT_GOLD_BRIGHT);
    circle(&mut c.draw, 240.0, 268.0, 29.0, color::BG);
    c.icon(
        if active { "pause" } else { "play" },
        228.0,
        256.0,
        24.0,
        color::TEXT_PRIMARY,
    );
    c.icon("next", 304.0, 254.0, 28.0, color::TEXT_PRIMARY);
    c.icon("repeat", 378.0, 256.0, 25.0, color::TEXT_SECONDARY);
}

pub fn output_label(output: &AudioOutput) -> String {
    match output {
        AudioOutput::Wired => "Balanced Output".into(),
        AudioOutput::Bluetooth(_) => "Bluetooth Output".into(),
    }
}

pub fn output_short(output: &AudioOutput) -> &'static str {
    match output {
        AudioOutput::Wired => "BAL",
        AudioOutput::Bluetooth(_) => "BT",
    }
}

pub fn fit(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= max_chars {
        return if value.is_empty() {
            "Unknown".into()
        } else {
            value.into()
        };
    }
    if max_chars < 4 {
        return value.chars().take(max_chars).collect();
    }
    format!(
        "{}...",
        value
            .chars()
            .take(max_chars.saturating_sub(3))
            .collect::<String>()
    )
}

pub fn time(ms: u64) -> String {
    format!("{:02}:{:02}", ms / 60_000, (ms / 1_000) % 60)
}

pub fn progress(position: u64, duration: u64) -> f32 {
    if duration == 0 {
        0.0
    } else {
        (position as f32 / duration as f32).clamp(0.0, 1.0)
    }
}

pub fn row_icon(key: &str) -> &'static str {
    if key.starts_with("bt") || key == "bluetooth" {
        "bluetooth"
    } else if key.starts_with("wifi") || key == "wifi" {
        "wifi"
    } else if key.contains("power") || key == "reboot" {
        "display"
    } else if key.contains("storage") || key == "internal" || key == "sd" {
        "storage"
    } else if key.contains("scan") || key.contains("library") {
        "albums"
    } else if key.contains("eq") || key == "equalizer" {
        "eq"
    } else if key.contains("gain") || key == "replay_gain" {
        "gain"
    } else if key.contains("output") {
        "headphones"
    } else if key.contains("display") || key == "timeout" {
        "display"
    } else if key.contains("shuffle") {
        "shuffle"
    } else if key.contains("repeat") {
        "repeat"
    } else if key.contains("queue") {
        "songs"
    } else if key.contains("artist") {
        "artist"
    } else if key.contains("album") {
        "albums"
    } else if key.contains("track") || key.contains("song") {
        "songs"
    } else {
        "settings"
    }
}

fn icon_index(name: &str) -> Option<u8> {
    Some(match name {
        "albums" => 0,
        "artist" => 1,
        "back" => 2,
        "battery" => 3,
        "bluetooth" => 4,
        "chevron_right" => 5,
        "display" => 6,
        "eq" => 7,
        "gain" => 8,
        "headphones" => 9,
        "heart" => 10,
        "info" => 11,
        "link" => 12,
        "menu" => 13,
        "next" => 14,
        "pause" => 15,
        "play" => 16,
        "previous" => 17,
        "repeat" => 18,
        "sd_card" | "sd" => 19,
        "settings" => 20,
        "shuffle" => 21,
        "songs" | "song" => 22,
        "storage" => 23,
        "volume" => 24,
        "wifi" => 25,
        _ => return None,
    })
}

fn text(d: &mut Vec<Quad>, x: f32, y: f32, value: &str, scale: f32, tint: u32, display_font: bool) {
    let advance = 8.0 * scale * FONT_ADVANCE_RATIO;
    let glyph_width = advance;
    let max = ((480.0 - x - space::LG) / advance).max(0.0) as usize;
    for (index, ch) in value.chars().take(max).enumerate() {
        let mut q = Quad::rect(
            x + index as f32 * advance,
            y,
            glyph_width,
            8.0 * scale,
            tint,
        );
        q.glyph = Some(glyph_index(ch));
        q.display_font = display_font;
        d.push(q);
    }
}

fn glyph_index(ch: char) -> u8 {
    let codepoint = ch as u32;
    if codepoint <= u8::MAX as u32 {
        codepoint as u8
    } else {
        b'?'
    }
}

fn rounded_rect(d: &mut Vec<Quad>, x: f32, y: f32, w: f32, h: f32, r: f32, fill: u32) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let r = r.min(w / 2.0).min(h / 2.0);
    rect(d, x + r, y, w - r * 2.0, h, fill);
    rect(d, x, y + r, w, h - r * 2.0, fill);
    let rows = r.ceil() as i32;
    for row in 0..rows {
        let dy = r - row as f32 - 0.5;
        let half = (r * r - dy * dy).max(0.0).sqrt();
        let inset = (r - half).max(0.0);
        rect(d, x + inset, y + row as f32, w - inset * 2.0, 1.0, fill);
        rect(
            d,
            x + inset,
            y + h - row as f32 - 1.0,
            w - inset * 2.0,
            1.0,
            fill,
        );
    }
}

fn rect(d: &mut Vec<Quad>, x: f32, y: f32, w: f32, h: f32, fill: u32) {
    d.push(Quad::rect(x, y, w, h, fill));
}

fn circle(d: &mut Vec<Quad>, cx: f32, cy: f32, radius: f32, fill: u32) {
    let r = radius.ceil() as i32;
    let rr = r * r;
    for dy in -r..=r {
        let span = (rr - dy * dy).max(0) as f32;
        let width = span.sqrt() * 2.0 + 1.0;
        rect(d, cx - width / 2.0, cy + dy as f32, width, 1.0, fill);
    }
}
