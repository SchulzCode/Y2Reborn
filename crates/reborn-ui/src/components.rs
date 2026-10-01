//! Reborn primitives. Bounds are expressed in pixels, with one proportional
//! licensed sans atlas and one immutable icon atlas shared by native and preview.
use crate::theme::{color, radius, type_scale};
use crate::{glyphs, Item, Ui};
use reborn_core::{
    platform::{ChargingState, LowBattery},
    AppModel, AudioOutput,
};
use reborn_graphics::Quad;

pub struct Canvas {
    pub draw: Vec<Quad>,
}
impl Canvas {
    pub fn new() -> Self {
        Self {
            draw: Vec::with_capacity(1800),
        }
    }
    pub fn finish(self) -> Vec<Quad> {
        self.draw
    }
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, fill: u32) {
        if w > 0. && h > 0. {
            self.draw.push(Quad::rect(x, y, w, h, fill));
        }
    }
    pub fn rounded(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, fill: u32) {
        self.rect(x + r, y, w - r * 2., h, fill);
        self.rect(x, y + r, w, h - r * 2., fill);
        for row in 0..r.ceil() as usize {
            let dy = r - row as f32 - 0.5;
            let inset = r - (r * r - dy * dy).max(0.).sqrt();
            self.rect(x + inset, y + row as f32, w - 2. * inset, 1., fill);
            self.rect(x + inset, y + h - row as f32 - 1., w - 2. * inset, 1., fill);
        }
    }
    pub fn focus_panel(&mut self, x: f32, y: f32, w: f32, h: f32, focused: bool) {
        if focused {
            let i = self.draw.len();
            self.rounded(x, y, w, h, radius::SMALL, color::ACCENT_GOLD_BRIGHT);
            self.draw[i].focus_target = true;
            self.rounded(
                x + 2.,
                y + 2.,
                w - 4.,
                h - 4.,
                radius::SMALL - 2.,
                color::FOCUS_FILL,
            );
        }
    }
    pub fn text(&mut self, x: f32, y: f32, value: &str, scale: f32, tint: u32) {
        self.text_box(x, y, 464. - x, value, scale, tint);
    }
    pub fn text_box(&mut self, x: f32, y: f32, width: f32, value: &str, scale: f32, tint: u32) {
        let size = 8. * scale;
        let value = fit_pixels(value, width, size);
        let mut cursor = x;
        for ch in value.chars() {
            let glyph = glyphs::index(ch);
            let ratio = size / 24.;
            let mut q = Quad::rect(
                cursor - 2. * ratio,
                y - 6. * ratio,
                32. * ratio,
                32. * ratio,
                tint,
            );
            q.glyph = Some(glyph);
            self.draw.push(q);
            cursor += glyphs::ADVANCE[glyph as usize] * ratio;
        }
    }
    pub fn centered(&mut self, x: f32, y: f32, value: &str, scale: f32, tint: u32) {
        let value = fit_pixels(value, (x.min(480. - x) - 16.) * 2., scale * 8.);
        self.text(
            x - text_width(&value, scale * 8.) / 2.,
            y,
            &value,
            scale,
            tint,
        );
    }
    pub fn icon(&mut self, name: &str, x: f32, y: f32, size: f32, tint: u32) {
        let i = match name {
            "albums" => 0,
            "artist" => 1,
            "back" => 2,
            "battery" => 3,
            "bluetooth" => 4,
            "chevron_right" => 5,
            "display" | "brightness" => 6,
            "eq" => 7,
            "headphones" => 9,
            "heart" => 10,
            "info" => 11,
            "menu" => 13,
            "next" => 14,
            "pause" => 15,
            "play" => 16,
            "previous" => 17,
            "repeat" => 18,
            "sd" => 19,
            "settings" => 20,
            "shuffle" => 21,
            "songs" => 22,
            "storage" => 23,
            "volume" => 24,
            "wifi" => 25,
            _ => 13,
        };
        let mut q = Quad::rect(x, y, size, size, tint);
        q.icon = Some(i);
        self.draw.push(q);
    }
    pub fn artwork(&mut self, x: f32, y: f32, size: f32, has_art: bool) {
        if has_art {
            let mut q = Quad::rect(x, y, size, size, color::TEXT_PRIMARY);
            q.artwork = true;
            self.draw.push(q);
        } else {
            self.rounded(x, y, size, size, 6., color::SURFACE);
            self.icon(
                "albums",
                x + size * 0.3,
                y + size * 0.3,
                size * 0.4,
                color::TEXT_MUTED,
            );
        }
    }
    pub fn progress(&mut self, x: f32, y: f32, width: f32, value: f32) {
        self.rect(x, y, width, 4., color::TRACK);
        self.rect(x, y, width * value.clamp(0., 1.), 4., color::ACCENT_GOLD);
    }
}
pub fn text_width(value: &str, size: f32) -> f32 {
    value
        .chars()
        .map(|ch| glyphs::ADVANCE[glyphs::index(ch) as usize] * size / 24.)
        .sum()
}
pub fn fit_pixels(value: &str, width: f32, size: f32) -> String {
    let clean: String = value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(1024)
        .collect();
    if text_width(&clean, size) <= width {
        return clean;
    }
    let mut out = String::new();
    let mut used = text_width("…", size);
    for ch in clean.chars() {
        used += text_width(&ch.to_string(), size);
        if used > width {
            break;
        }
        out.push(ch);
    }
    if width >= text_width("…", size) {
        out.push('…');
    }
    out
}
pub fn time(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, (ms / 1000) % 60)
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
        AudioOutput::Wired => "Headphone jack".into(),
        AudioOutput::Bluetooth(_) => "Bluetooth".into(),
    }
}
pub fn status_bar(c: &mut Canvas, ui: &Ui, m: &AppModel) {
    c.rect(0., 0., 480., 28., color::BG_RAISED);
    if m.playback == reborn_core::PlaybackState::Playing {
        c.icon("play", 16., 7., 14., color::ACCENT_GOLD);
    }
    let b = m.platform.battery;
    let low = matches!(
        b.level,
        LowBattery::Low | LowBattery::Critical | LowBattery::ShuttingDown
    );
    let tint = if low {
        color::DANGER
    } else {
        color::TEXT_SECONDARY
    };
    let mut x = 438.;
    c.icon("battery", x, 4., 22., tint);
    if matches!(b.charging, ChargingState::Charging | ChargingState::Full) {
        c.text(466., 7., "+", type_scale::BODY, color::ACCENT_GOLD);
    }
    if let Some(percent) = b.percent.filter(|p| *p <= 100) {
        let text = format!("{percent}%");
        let w = text_width(&text, 12.);
        x -= w + 6.;
        c.text(x, 8., &text, type_scale::SECONDARY, tint);
    }
    if ui.bluetooth.powered {
        x -= 26.;
        let connected = ui.bluetooth.devices.iter().any(|d| d.connected);
        c.icon(
            "bluetooth",
            x,
            5.,
            18.,
            if connected {
                color::TEXT_PRIMARY
            } else {
                color::TEXT_MUTED
            },
        );
    }
    if ui.wifi.powered {
        x -= 26.;
        let connected = matches!(ui.wifi.status, crate::WifiStatus::Connected(_));
        c.icon(
            "wifi",
            x,
            5.,
            18.,
            if connected {
                color::TEXT_PRIMARY
            } else {
                color::TEXT_MUTED
            },
        );
    }
}
pub fn title(c: &mut Canvas, name: &str, detail: &str) {
    let value = fit_pixels(detail, 130., 12.);
    let width = text_width(&value, 12.);
    c.text_box(
        16.,
        40.,
        440. - width,
        name,
        type_scale::SCREEN_TITLE,
        color::TEXT_PRIMARY,
    );
    c.text(
        464. - width,
        47.,
        &value,
        type_scale::SECONDARY,
        color::TEXT_MUTED,
    );
}
pub fn row(c: &mut Canvas, item: &Item, y: f32, focused: bool, trailing: &str, playing: bool) {
    c.focus_panel(16., y, 448., 46., focused && item.enabled);
    let x = if playing { 44. } else { 28. };
    if playing {
        c.icon("play", 26., y + 16., 13., color::ACCENT_GOLD);
    }
    let tail_width = if trailing.is_empty() {
        0.
    } else {
        text_width(trailing, 12.) + 20.
    };
    let has_secondary = !item.secondary.is_empty();
    c.text_box(
        x,
        y + if has_secondary { 6. } else { 14. },
        428. - x - tail_width,
        &item.label,
        type_scale::ROW,
        if item.enabled {
            color::TEXT_PRIMARY
        } else {
            color::TEXT_MUTED
        },
    );
    if has_secondary {
        c.text_box(
            x,
            y + 27.,
            428. - x - tail_width,
            &item.secondary,
            type_scale::SECONDARY,
            color::TEXT_SECONDARY,
        );
    }
    if !trailing.is_empty() {
        c.text(
            450. - text_width(trailing, 12.),
            y + 16.,
            trailing,
            type_scale::SECONDARY,
            color::TEXT_SECONDARY,
        );
    }
}
/// A thin position rail beside long lists.
pub fn scroll_indicator(
    c: &mut Canvas,
    top: f32,
    height: f32,
    scroll: usize,
    visible: usize,
    count: usize,
) {
    if count == 0 {
        return;
    }
    let thumb = (height * visible as f32 / count as f32).max(16.);
    let y = top + (height - thumb) * scroll as f32 / (count - visible).max(1) as f32;
    c.rect(470., top, 2., height, color::SURFACE);
    c.rect(470., y, 2., thumb, color::TEXT_MUTED);
}
pub fn footer(c: &mut Canvas, m: &AppModel, has_art: bool) {
    c.rect(0., 324., 480., 36., color::BG_RAISED);
    if let Some(track) = m.current() {
        c.artwork(16., 329., 26., has_art);
        c.text_box(
            52.,
            329.,
            320.,
            crate::track_title(track),
            type_scale::SECONDARY,
            color::TEXT_PRIMARY,
        );
        c.text_box(
            52.,
            345.,
            320.,
            crate::display_or_unknown(&track.artist),
            type_scale::SECONDARY,
            color::TEXT_SECONDARY,
        );
        c.icon(
            if m.playback == reborn_core::PlaybackState::Playing {
                "play"
            } else {
                "pause"
            },
            444.,
            333.,
            18.,
            color::TEXT_SECONDARY,
        );
    }
}
/// One calm line of radio progress or a problem in the footer strip.
pub fn status_strip(c: &mut Canvas, message: &str) {
    c.rect(0., 324., 480., 36., color::BG_RAISED);
    c.text_box(
        16.,
        336.,
        448.,
        message,
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
}
/// A modal sheet sized to its content and centred vertically. Up to six
/// rows are visible; longer sheets scroll with the focus.
pub fn dialog(c: &mut Canvas, title: &str, body: &str, rows: &[Item], focus: usize) {
    const PITCH: f32 = 40.;
    let lines = crate::screens::wrap_lines(body, 376., 14., 3);
    let header = 54. + lines.len() as f32 * 20. + if lines.is_empty() { 0. } else { 10. };
    let visible = rows.len().clamp(1, 6);
    let height = (header + visible as f32 * PITCH + 14.).min(328.);
    let top_edge = ((360. - height) / 2.).max(16.);
    c.rect(0., 0., 480., 360., color::SCRIM_STRONG);
    c.rounded(32., top_edge, 416., height, 10., color::SURFACE);
    c.text_box(
        52.,
        top_edge + 18.,
        376.,
        title,
        type_scale::SCREEN_TITLE,
        color::TEXT_PRIMARY,
    );
    let mut y = top_edge + 52.;
    for line in &lines {
        c.text_box(52., y, 376., line, type_scale::BODY, color::TEXT_SECONDARY);
        y += 20.;
    }
    let top = top_edge + header;
    let start = focus.saturating_sub(visible - 1);
    for (index, item) in rows.iter().enumerate().skip(start).take(visible) {
        let y = top + (index - start) as f32 * PITCH;
        c.focus_panel(48., y, 384., 38., index == focus && item.enabled);
        let tail = fit_pixels(&item.secondary, 150., 12.);
        let tail_w = text_width(&tail, 12.);
        c.text_box(
            62.,
            y + 11.,
            350. - tail_w,
            &item.label,
            type_scale::ROW,
            if item.enabled {
                color::TEXT_PRIMARY
            } else {
                color::TEXT_MUTED
            },
        );
        if !tail.is_empty() {
            c.text(
                418. - tail_w,
                y + 13.,
                &tail,
                type_scale::SECONDARY,
                color::TEXT_SECONDARY,
            );
        }
    }
}
pub fn toast(c: &mut Canvas, message: &str) {
    c.rounded(16., 284., 448., 34., 6., color::SURFACE_HOVER);
    c.text_box(
        28.,
        294.,
        424.,
        message,
        type_scale::BODY,
        color::TEXT_PRIMARY,
    );
}
