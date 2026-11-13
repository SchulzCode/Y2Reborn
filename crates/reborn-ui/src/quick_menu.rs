//! The Quick Menu: holding Power opens it on any screen (Y2Linux #85). Wi-Fi, Bluetooth,
//! Brightness, the sleep timer, Restart and Power Off, in the wheel's order over the panel's tiles
//! (`views_quick`); holding on turns the player off as before.
use crate::*;
use reborn_core::platform::ChargingState;

/// How long Power is held, with the menu open, before the turn-off ring appears; the ring then
/// fills over `POWER_HOLD`. The hardware's own reset (10 s) stays well beyond the two.
pub const QUICK_MENU_HOLD: Duration = Duration::from_secs(3);

impl Ui {
    /// Power was held: open the menu where it can show, and start the turn-off clock. Over the
    /// keyboard, a pairing question or a message the ring starts at once, as before.
    pub(crate) fn power_held(&mut self, m: &mut AppModel) {
        let menu = self.keyboard.is_none() && self.pairing.is_none() && self.alert.is_none();
        if menu {
            self.adjusting_brightness = false;
            Self::open_modal(m, Modal::QuickMenu);
            // On Brightness, the control people come for (#85); Power Off is never first.
            let rows = self.quick_menu_rows(m);
            m.navigation.modal_focus = rows
                .iter()
                .position(|r| r.key == "quick:brightness")
                .unwrap_or(0);
        }
        let ring = if menu {
            QUICK_MENU_HOLD
        } else {
            Duration::ZERO
        };
        self.power_hold = Some(Instant::now() + ring);
    }

    /// A short press of Power closes an open Quick Menu rather than sleeping.
    pub(crate) fn close_quick_menu(&mut self, m: &mut AppModel) -> bool {
        if m.navigation.modal != Some(Modal::QuickMenu) {
            return false;
        }
        self.adjusting_brightness = false;
        Self::close_modal(m);
        true
    }

    /// The menu's rows; a radio without its firmware (or not yet started) cannot be switched.
    pub(crate) fn quick_menu_rows(&self, m: &AppModel) -> Vec<Item> {
        let mut rows = Vec::new();
        let wifi = match (&self.wifi, self.wifi.status.network()) {
            (w, _) if !w.available => "Unavailable".to_owned(),
            (w, _) if !w.powered => "Off".to_owned(),
            (_, Some(network)) => network.to_owned(),
            _ => "Not connected".to_owned(),
        };
        let mut row = Item::new("Wi-Fi", "quick:wifi").with_secondary(wifi);
        row.enabled = self.wifi.available;
        rows.push(row);
        let bt = &self.bluetooth;
        let device = bt
            .devices
            .iter()
            .find(|d| d.connected)
            .map(|d| d.name.clone());
        let bluetooth = match device {
            _ if !bt.available => "Unavailable".to_owned(),
            _ if !bt.powered => "Off".to_owned(),
            Some(name) => name,
            None => "On".to_owned(),
        };
        let mut row = Item::new("Bluetooth", "quick:bluetooth").with_secondary(bluetooth);
        row.enabled = bt.available;
        rows.push(row);
        if m.platform.brightness_available {
            let level = format!("{}%", m.settings.brightness);
            rows.push(Item::new("Brightness", "quick:brightness").with_secondary(level));
        }
        let timer = match (m.sleep_timer, sleep_minutes_left(m)) {
            (None, _) => "Off".to_owned(),
            (Some(_), Some(left)) => format!("{} left", minutes_label(left)),
            (Some(t), None) => sleep_choice_label(t.choice, m.book_part().is_some()),
        };
        rows.push(Item::new("Sleep Timer", "quick:sleep").with_secondary(timer));
        // A layout of one's own: one Select back to Reborn Default (the Quick Menu itself always
        // answers as Reborn Default, and Power, which opens it, is in no layout).
        if m.settings.buttons.active != reborn_core::buttons::REBORN_DEFAULT {
            rows.push(Item::new("Buttons", "quick:buttons").with_secondary("Use Default"));
        }
        // Not while the platform works (an update, a repair): it owns the restart.
        for (label, key) in [
            ("Restart", "quick:restart"),
            ("Power Off", "quick:power_off"),
        ] {
            let mut row = Item::new(label, key);
            row.enabled = m.platform.busy.is_none();
            rows.push(row);
        }
        rows
    }

    /// The title and the line under it: the battery, or how to set the brightness.
    pub(crate) fn quick_menu_copy(&self, m: &AppModel) -> (String, String) {
        if self.adjusting_brightness {
            let hint = "Turn the wheel, then Select";
            return ("Quick Menu".into(), hint.into());
        }
        let b = m.platform.battery;
        let line = match (b.percent, b.charging) {
            (Some(p), ChargingState::Charging) => format!("Battery {p}% · Charging"),
            (Some(p), ChargingState::Full) if b.cable => format!("Battery {p}% · Charged"),
            (Some(p), _) => format!("Battery {p}%"),
            (None, _) => String::new(),
        };
        ("Quick Menu".into(), line)
    }

    /// Holding Select on a tile opens its settings page, as on a phone (Y2Reborn #101): Wi-Fi
    /// to choose a network, Bluetooth a device, Brightness the Display page. The menu closes;
    /// Back returns to the screen it was opened on. `false`: the menu is not open.
    pub(crate) fn quick_menu_hold(&mut self, m: &mut AppModel) -> bool {
        if m.navigation.modal != Some(Modal::QuickMenu) {
            return false;
        }
        let rows = self.quick_menu_rows(m);
        let focused = rows.get(m.navigation.modal_focus).filter(|r| r.enabled);
        let screen = match focused.map(|r| r.key.as_str()) {
            Some("quick:wifi") => Screen::Wifi,
            Some("quick:bluetooth") => Screen::Bluetooth,
            Some("quick:brightness") => Screen::SettingsDisplay,
            Some("quick:buttons") => Screen::Customize,
            _ => return true,
        };
        self.adjusting_brightness = false;
        Self::close_modal(m);
        let filter = if screen == Screen::Customize {
            crate::buttons_page::BUTTONS
        } else {
            ""
        };
        Self::go(m, screen, filter);
        true
    }

    /// Select on a row. The switches keep the menu open so their new state shows.
    pub(crate) fn quick_menu_action(&mut self, m: &mut AppModel, key: &str) -> Effect {
        match key {
            "quick:brightness" => {
                self.adjusting_brightness = true;
                Effect::None
            }
            "quick:wifi" => Effect::WifiPower,
            "quick:bluetooth" => Effect::BluetoothPower,
            "quick:buttons" => {
                Self::close_modal(m);
                self.run_command(m, &[], reborn_core::buttons::Command::RebornDefault)
            }
            "quick:sleep" => {
                Self::open_modal(m, Modal::SleepTimer);
                Effect::None
            }
            "quick:restart" => {
                Self::close_modal(m);
                Effect::Reboot
            }
            "quick:power_off" => {
                Self::close_modal(m);
                Effect::PowerOff
            }
            _ => Effect::None,
        }
    }
}
