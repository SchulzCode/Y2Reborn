//! Settings → Library, and its Wi-Fi Share page: the address and the code another device needs, or
//! why sharing is not running.
use super::*;

/// Settings → Library: the scan, the storages, Wi-Fi Share and Last.fm.
pub(super) fn page(ui: &Ui, m: &AppModel, tracks: &[Track]) -> Page {
    if m.navigation.filter == crate::SHARE {
        return share(ui);
    }
    // Short enough for the value column; the root row counts the songs too.
    let scan = if m.library.scanning {
        "Scanning…".to_owned()
    } else if m.library.error.is_some() {
        "Last scan interrupted".to_owned()
    } else {
        crate::components::count(ui.library_counts(m, tracks)[2], "song", "songs")
    };
    let storage = free_text(&m.platform.snapshot.storage.internal)
        .map(|t| t.split(" free").next().unwrap_or("").to_owned() + " free")
        .unwrap_or_default();
    let last = m.library.last_scan.as_ref().filter(|_| !m.library.scanning);
    Page {
        status: last.and_then(crate::components::left_out_note),
        ..Page::rows(vec![
            disabled(
                row("Scan for Music", "scan_library", scan),
                !m.library.scanning,
            ),
            row("Storage", "storage", storage).with_kind(RowKind::Chevron),
            row("Wi-Fi Share", "wifi_share", "Copy music from a phone").with_kind(RowKind::Chevron),
            row("Last.fm", "lastfm", lastfm_summary(ui, m)).with_kind(RowKind::Chevron),
            row("Audiobookshelf", "audiobookshelf", "").with_kind(RowKind::Chevron),
        ])
    }
}

fn share(ui: &Ui) -> Page {
    let share = &ui.share;
    let online = matches!(
        ui.wifi.status,
        WifiStatus::Connected(_) | WifiStatus::NoInternet(_)
    );
    if !online {
        return Page::hero(
            "Wi-Fi Share Needs Wi-Fi",
            "Connect the player to the same network as your phone or computer.",
        )
        .with_rows(vec![row("Connect to Wi-Fi", "radio_wifi", "")]);
    }
    if let Some(error) = &share.error {
        return Page::hero("Wi-Fi Share", error).with_rows(vec![row("Back", "share_stop", "")]);
    }
    if share.address.is_empty() {
        return Page::hero("Starting Wi-Fi Share…", "One moment.");
    }
    let activity = match (share.busy, share.received) {
        (true, _) => "Receiving…".to_owned(),
        (false, 0) => String::new(),
        (false, n) => crate::components::count(n as usize, "file received", "files received"),
    };
    Page {
        hero: Some(Hero {
            title: share.address.clone(),
            body: format!("Code {}", share.code),
        }),
        ..Page::rows(vec![row("Stop Sharing", "share_stop", activity)])
    }
}

/// Last.fm's row: the account, the log file, or Off.
fn lastfm_summary(ui: &Ui, m: &AppModel) -> String {
    match &ui.lastfm.user {
        Some(user) => user.clone(),
        None if m.settings.scrobble_log => "Log file".to_owned(),
        None => "Off".to_owned(),
    }
}
