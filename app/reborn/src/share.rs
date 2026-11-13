//! Wi-Fi Share: the file browser page other devices open (`reborn_platform::share`) runs exactly
//! while Settings → Library → Wi-Fi Share is on screen.
//!
//! The runtime's tick starts the server when that page opens and Wi-Fi is up, keeps the player
//! awake while it is open (Wi-Fi is off in sleep), and stops it when the listener leaves; if
//! anything changed on a storage, Reborn then looks for music as it does after a USB copy.
use crate::*;
use reborn_platform::share::{self, Share};
use std::cell::RefCell;

/// The port a browser uses without being told.
const PORT: u16 = 80;

thread_local! {
    static SHARE: RefCell<Option<Share>> = const { RefCell::new(None) };
}

impl Runtime {
    pub(crate) fn share_tick(&mut self) {
        let navigation = &self.model.navigation;
        let open =
            self.model.screen == Screen::SettingsLibrary && navigation.filter == reborn_ui::SHARE;
        let running = SHARE.with_borrow(Option::is_some);
        if !open {
            if running {
                self.share_stop();
            }
            return;
        }
        // The page is the reason to stay awake and lit: it shows the address and the code.
        self.last_activity = Instant::now();
        let online = matches!(
            self.ui.wifi.status,
            reborn_ui::WifiStatus::Connected(_) | reborn_ui::WifiStatus::NoInternet(_)
        );
        if !running {
            let view = if online {
                self.share_start()
            } else {
                reborn_ui::ShareView::default()
            };
            if view != self.ui.share {
                self.ui.share = view;
                self.dirty.mark_render();
            }
            return;
        }
        if !online {
            return self.share_stop();
        }
        let (received, busy) =
            SHARE.with_borrow(|s| s.as_ref().map_or((0, false), |s| (s.received(), s.busy())));
        if (received, busy) != (self.ui.share.received, self.ui.share.busy) {
            self.ui.share.received = received;
            self.ui.share.busy = busy;
            self.dirty.mark_render();
        }
    }

    fn share_start(&mut self) -> reborn_ui::ShareView {
        match Share::start(share::storages(), PORT) {
            Ok(started) => {
                let view = reborn_ui::ShareView {
                    address: started.address.clone(),
                    code: started.code().to_owned(),
                    ..Default::default()
                };
                let fields = json!({"storages": share::storages().len()});
                self.log.emit(
                    Level::Info,
                    "storage",
                    "share_started",
                    "Wi-Fi Share started",
                    None,
                    fields,
                );
                SHARE.set(Some(started));
                view
            }
            Err(error) => {
                let fields = json!({"error": error.to_string()});
                self.log.emit(
                    Level::Warn,
                    "storage",
                    "share_failed",
                    "Wi-Fi Share could not start",
                    None,
                    fields,
                );
                reborn_ui::ShareView {
                    error: Some("Wi-Fi Share couldn't start. Try again.".into()),
                    ..Default::default()
                }
            }
        }
    }

    fn share_stop(&mut self) {
        let Some(stopped) = SHARE.take() else {
            return;
        };
        let (changes, received) = (stopped.changes(), stopped.received());
        drop(stopped);
        self.ui.share = reborn_ui::ShareView::default();
        let fields = json!({"changes": changes, "received": received});
        self.log.emit(
            Level::Info,
            "storage",
            "share_stopped",
            "Wi-Fi Share stopped",
            None,
            fields,
        );
        if changes > 0 {
            let _ = self.on_scan_library();
        }
        self.dirty.mark_render();
    }
}
