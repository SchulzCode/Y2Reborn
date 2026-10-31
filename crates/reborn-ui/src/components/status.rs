//! The status bar: the screen's name, the clock, radios, charging and battery
//! (Beta 7.5), and Home's strip with the player's name.
use crate::{
    canvas::{Canvas, Icon, Moving},
    text::{self, Face},
    theme::color,
    Ui,
};
use reborn_core::{
    platform::{ChargingState, LowBattery, UsbTransfer},
    AppModel,
};

/// The top strip (Beta 7.5): optional back arrow, the screen's name beside
/// it and the status cluster (clock, radios, charging, battery) on the right.
pub fn status_bar(c: &mut Canvas, ui: &Ui, m: &AppModel, title: &str, back: bool) {
    if back {
        c.icon(Icon::Back, 22., 18., 22., color::TEXT);
    }
    // The title and the status cluster: a message takes their place.
    let start = c.draw.len();
    let left = status_cluster(c, ui, m, color::MUTED, true);
    if !title.is_empty() {
        let x = if back { 44. } else { 16. };
        c.text_mid(x, 18., left - 14. - x, title, Face::S15, color::TEXT);
    }
    let end = c.draw.len();
    c.mark_moving(Moving::StatusBar { start, end });
}

/// Home's strip: the player's name and version where other screens have
/// their name.
pub fn home_bar(c: &mut Canvas, ui: &Ui, m: &AppModel) {
    let start = c.draw.len();
    let left = status_cluster(c, ui, m, color::MUTED, true);
    let x = 16.
        + c.text(
            16.,
            18. + Face::S15.mid_offset(),
            "Reborn",
            Face::S15,
            color::TEXT,
        );
    let version = crate::pages::short_version(reborn_core::VERSION);
    c.text_mid(
        x + 7.,
        18.,
        left - 14. - x - 7.,
        &version,
        Face::R13,
        color::MUTED,
    );
    let end = c.draw.len();
    c.mark_moving(Moving::StatusBar { start, end });
}

/// The clock, radios, charging and battery, right-aligned; returns the left
/// edge of what it drew. `roomy` is Reborn's own bar (Beta 7.5: larger
/// numbers and icons); skins place the compact one in their own layouts.
pub(crate) fn status_cluster(c: &mut Canvas, ui: &Ui, m: &AppModel, tint: u32, roomy: bool) -> f32 {
    let b = m.platform.battery;
    let low = matches!(
        b.level,
        LowBattery::Low | LowBattery::Critical | LowBattery::ShuttingDown
    ) || b.percent.is_some_and(|p| p <= 15);
    let fill = if low { color::DANGER } else { color::TEXT };
    // Sizes: battery, numbers, clock, icons (Wi-Fi and Bluetooth look
    // smaller than the USB plug at the same size) and their spacing.
    let (bw, bh, number, clock, icon, radio, step): (f32, f32, Face, Face, f32, f32, f32) = if roomy
    {
        (25., 13., Face::S13, Face::S15, 18., 22., 24.)
    } else {
        (22., 12., Face::S10, Face::S10, 16., 16., 20.)
    };
    let text_tint = if roomy { color::TEXT } else { tint };
    // Battery body, nub and charge level.
    let (x0, y0) = (465. - bw, (18. - bh / 2.).floor());
    c.rrect_outline(x0, y0, bw, bh, 3., 1., tint);
    c.rect(x0 + bw + 1., y0 + (bh / 2. - 2.).floor(), 2., 4., tint);
    if let Some(p) = b.percent.filter(|p| *p <= 100) {
        let w = ((bw - 4.) * f32::from(p) / 100.)
            .round()
            .max(if p > 0 { 2. } else { 0. });
        c.rect(x0 + 2., y0 + 2., w, bh - 4., fill);
    }
    let mut right = x0 - 8.;
    if let Some(p) = b.percent.filter(|p| *p <= 100) {
        let text = p.to_string();
        let w = text::width(number, &text);
        c.text(
            right - w,
            18. + number.mid_offset(),
            &text,
            number,
            if low { color::DANGER } else { text_tint },
        );
        right -= w + 8.;
    }
    if matches!(b.charging, ChargingState::Charging) {
        c.icon(Icon::Bolt, right - 5., 18., icon - 2., color::SUCCESS);
        right -= step - 4.;
    }
    // A computer has the drives (Beta 7.3: instead of a screen that covers
    // the player); the accent while files are copied.
    let usb = m.platform.snapshot.usb;
    if matches!(usb, UsbTransfer::Ready | UsbTransfer::Copying) {
        let tint = if usb == UsbTransfer::Copying {
            color::ACCENT
        } else {
            color::TEXT
        };
        c.icon(Icon::Usb, right - step / 2. + 3., 18., icon, tint);
        right -= step;
    }
    if ui.bluetooth.powered {
        let connected = ui.bluetooth.devices.iter().any(|d| d.connected);
        c.icon(
            Icon::Bluetooth,
            right - step / 2. + 3.,
            18.,
            radio,
            if connected { color::TEXT } else { color::DIM },
        );
        right -= step;
    }
    if ui.wifi.powered {
        let connected = matches!(ui.wifi.status, crate::WifiStatus::Connected(_));
        c.icon(
            Icon::Wifi,
            right - step / 2. + 3.,
            18.,
            radio,
            if connected { color::TEXT } else { color::DIM },
        );
        right -= step;
    }
    if let Some(time) = ui.clock(m) {
        let w = text::width(clock, &time);
        right -= w + 2.;
        c.text(right, 18. + clock.mid_offset(), &time, clock, text_tint);
    }
    right
}
