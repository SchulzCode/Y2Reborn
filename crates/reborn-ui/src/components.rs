//! Reborn primitives. Bounds are expressed in pixels, with one proportional
//! licensed sans atlas and one immutable icon atlas shared by native and preview.
use crate::theme::{color, radius, type_scale};
use crate::{glyphs, Item, PowerView, RadioView};
use reborn_core::{AppModel, AudioOutput};
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
    output_short(output).into()
}
pub fn output_short(output: &AudioOutput) -> &'static str {
    match output {
        AudioOutput::Wired => "Wired",
        AudioOutput::Bluetooth(_) => "Bluetooth",
    }
}
pub fn status_bar(
    c: &mut Canvas,
    m: &AppModel,
    power: PowerView,
    wifi: &RadioView,
    bt: &RadioView,
) {
    c.rect(0., 0., 480., 28., color::BG_RAISED);
    c.text(
        16.,
        8.,
        "Reborn",
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
    c.text_box(
        240.,
        8.,
        105.,
        output_short(&m.output),
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
    if wifi.powered {
        c.icon("wifi", 350., 5., 18., color::TEXT_SECONDARY);
    }
    if bt.powered {
        c.icon("bluetooth", 378., 5., 18., color::TEXT_SECONDARY);
    }
    c.icon("battery", 438., 4., 22., color::TEXT_SECONDARY);
    if let Some(percent) = power.percent.filter(|p| *p <= 100) {
        c.text_box(
            400.,
            8.,
            36.,
            &format!("{percent}%"),
            type_scale::SECONDARY,
            color::TEXT_SECONDARY,
        );
    }
    if power.charging {
        c.text(466., 7., "+", type_scale::BODY, color::ACCENT_GOLD);
    }
}
pub fn title(c: &mut Canvas, name: &str, detail: &str) {
    c.text_box(
        16.,
        42.,
        345.,
        name,
        type_scale::SCREEN_TITLE,
        color::TEXT_PRIMARY,
    );
    let value = fit_pixels(detail, 90., 12.);
    let width = text_width(&value, 12.);
    c.text(
        464. - width,
        49.,
        &value,
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
}
pub fn row(c: &mut Canvas, item: &Item, y: f32, focused: bool, trailing: &str, playing: bool) {
    c.focus_panel(16., y, 448., 46., focused && item.enabled);
    let x = if playing { 42. } else { 28. };
    if playing {
        c.icon("play", 25., y + 15., 14., color::TEXT_PRIMARY);
    }
    let tail_width = if trailing.is_empty() {
        0.
    } else {
        text_width(trailing, 12.) + 20.
    };
    c.text_box(
        x,
        y + if item.secondary.is_empty() { 15. } else { 7. },
        424. - x - tail_width,
        &item.label,
        type_scale::ROW,
        if item.enabled {
            color::TEXT_PRIMARY
        } else {
            color::TEXT_MUTED
        },
    );
    if !item.secondary.is_empty() {
        c.text_box(
            x,
            y + 27.,
            416. - x,
            &item.secondary,
            type_scale::SECONDARY,
            color::TEXT_SECONDARY,
        );
    }
    if !trailing.is_empty() {
        c.text(
            446. - text_width(trailing, 12.),
            y + 10.,
            trailing,
            type_scale::SECONDARY,
            color::TEXT_SECONDARY,
        );
    }
}
pub fn footer(c: &mut Canvas, m: &AppModel, has_art: bool, hint: &str) {
    c.rect(0., 324., 480., 36., color::BG_RAISED);
    if let Some(track) = m.current() {
        c.artwork(16., 329., 26., has_art);
        c.text_box(
            52.,
            329.,
            246.,
            if track.title.is_empty() {
                &track.filename
            } else {
                &track.title
            },
            type_scale::SECONDARY,
            color::TEXT_PRIMARY,
        );
        c.text_box(
            52.,
            345.,
            246.,
            &track.artist,
            type_scale::SECONDARY,
            color::TEXT_SECONDARY,
        );
    } else {
        c.text_box(
            16.,
            337.,
            290.,
            hint,
            type_scale::SECONDARY,
            color::TEXT_SECONDARY,
        );
    }
    c.icon("volume", 380., 332., 18., color::TEXT_SECONDARY);
    c.text(
        406.,
        335.,
        &m.settings.volume.to_string(),
        type_scale::BODY,
        color::TEXT_PRIMARY,
    );
}
pub fn dialog(c: &mut Canvas, title: &str, body: &str, rows: &[Item], focus: usize) {
    c.rect(0., 0., 480., 360., color::SCRIM_STRONG);
    c.rounded(36., 32., 408., 296., 10., color::SURFACE);
    c.text_box(
        56.,
        51.,
        368.,
        title,
        type_scale::SCREEN_TITLE,
        color::TEXT_PRIMARY,
    );
    // Consequences and pairing codes must remain fully readable, not an
    // ellipsized one-line subtitle. Compact action sheets retain four rows.
    let detailed = rows.len() <= 3;
    crate::screens::wrap(c, 56., 83., 368., body, 12., if detailed { 4 } else { 1 });
    let visible = if detailed { 3 } else { 4 };
    let start = focus.saturating_sub(visible - 1);
    for (index, item) in rows.iter().enumerate().skip(start).take(visible) {
        let y = if detailed { 154. } else { 110. } + (index - start) as f32 * 48.;
        c.focus_panel(52., y, 376., 44., index == focus && item.enabled);
        c.text_box(
            66.,
            y + 14.,
            346.,
            &item.label,
            type_scale::ROW,
            if item.enabled {
                color::TEXT_PRIMARY
            } else {
                color::TEXT_MUTED
            },
        );
    }
    c.text(
        56.,
        309.,
        "Back  Close",
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
    c.text(
        346.,
        309.,
        &format!("{}/{}", focus + 1, rows.len()),
        type_scale::SECONDARY,
        color::TEXT_SECONDARY,
    );
}
pub fn toast(c: &mut Canvas, message: &str) {
    c.rounded(16., 30., 448., 38., 6., color::SURFACE_HOVER);
    c.text_box(
        28.,
        42.,
        424.,
        message,
        type_scale::BODY,
        color::TEXT_PRIMARY,
    );
}
