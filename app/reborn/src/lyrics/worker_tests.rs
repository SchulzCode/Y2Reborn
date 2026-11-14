use super::*;
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

#[test]
fn replacement_is_bounded_stale_results_are_rejected_and_sleep_waits_for_active_work() {
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let loaded = seen.clone();
    let mut worker = Worker::spawn_using(move |job, _| {
        loaded.lock().unwrap().push(job.track.id);
        if job.track.id == 1 {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }
        State::Missing
    })
    .unwrap();
    let track = |id| Track {
        id,
        path: format!("/music/{id}.mp3").into(),
        ..Track::default()
    };
    let request = |worker: &mut Worker, id| {
        let track = track(id);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !worker.request(Key::new(&track, &[]), track.clone(), "/music".into()) {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    };
    request(&mut worker, 1);
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        !worker.quiesce(),
        "active work prevents a sleep acknowledgement"
    );
    request(&mut worker, 2);
    request(&mut worker, 3);
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let key = loop {
        if let Some((key, _)) = worker.reply() {
            break key;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(key.path, track(3).path);
    assert_eq!(*seen.lock().unwrap(), [1, 3]);
    assert!(worker.quiesce());
}
