use super::scan_toast;

#[test]
fn a_scan_the_player_starts_is_silent_unless_it_read_new_songs() {
    // The start: the card is not mounted yet, then it is, then it remounts.
    assert_eq!(scan_toast(0, 0, false), None);
    assert_eq!(scan_toast(745, 0, false), None);
    assert_eq!(scan_toast(1, 0, false), None);
    assert_eq!(
        scan_toast(745, 12, false).as_deref(),
        Some("12 new songs added")
    );
}

#[test]
fn a_scan_the_owner_asked_for_is_answered() {
    assert_eq!(
        scan_toast(0, 0, true).as_deref(),
        Some("Scan finished. No music found.")
    );
    assert_eq!(
        scan_toast(1, 0, true).as_deref(),
        Some("Scan finished. 1 song in your library.")
    );
    assert_eq!(
        scan_toast(745, 0, true).as_deref(),
        Some("Scan finished. 745 songs in your library.")
    );
    assert_eq!(
        scan_toast(745, 3, true).as_deref(),
        Some("3 new songs added")
    );
}

#[test]
fn new_songs_never_count_more_than_the_library_holds() {
    // Unreadable files were read but are not songs.
    assert_eq!(
        scan_toast(5, 9, false).as_deref(),
        Some("5 new songs added")
    );
    // A single new song is the whole library: say the count, if asked.
    assert_eq!(scan_toast(1, 1, false), None);
    assert_eq!(
        scan_toast(1, 1, true).as_deref(),
        Some("Scan finished. 1 song in your library.")
    );
}
