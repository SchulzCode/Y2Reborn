use super::LogoFetcher;

/// A logo that cannot be fetched is settled: asking again sends nothing
/// and finishing it asks for no new frame (the redraw loop of 5.1–6).
#[test]
fn a_failed_logo_is_never_asked_for_again() {
    let cache = std::env::temp_dir().join(format!("reborn-logos-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    let mut fetcher = LogoFetcher::spawn(cache.clone()).unwrap();
    let key = reborn_core::logo_key("not a web address");
    fetcher.want(key, "not a web address");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut frames = 0;
    while !fetcher.is_settled(key) && std::time::Instant::now() < deadline {
        frames += fetcher.finished();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(fetcher.is_settled(key));
    assert_eq!(frames, 0, "a failure asks for no frame");
    fetcher.want(key, "not a web address");
    assert!(fetcher.pending.is_empty(), "never sent again");
    let _ = std::fs::remove_dir_all(cache);
}
