use super::{seek_step, shuffle_from};
use std::time::Duration;

#[test]
fn shuffles_differ_by_seed_and_keep_every_song() {
    let orders: Vec<Vec<u32>> = (1..=8u64)
        .map(|seed| {
            let mut v: Vec<u32> = (0..40).collect();
            shuffle_from(&mut v, 0, seed.wrapping_mul(0x1234_5678_9abc_def1));
            v
        })
        .collect();
    for order in &orders {
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..40).collect::<Vec<_>>());
    }
    for (i, a) in orders.iter().enumerate() {
        for b in &orders[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

#[test]
fn a_picked_song_stays_first_and_every_song_can_lead() {
    let mut first = [0u32; 10];
    for seed in 0..10_000u64 {
        let mut kept: Vec<u32> = (0..10).collect();
        shuffle_from(&mut kept, 1, seed);
        assert_eq!(kept[0], 0);
        let mut all: Vec<u32> = (0..10).collect();
        shuffle_from(&mut all, 0, seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        first[all[0] as usize] += 1;
    }
    // About 1000 each; a seed-derived order once always led with the same song.
    assert!(first.iter().all(|&n| (800..1200).contains(&n)), "{first:?}");
}

#[test]
fn held_seek_starts_small_and_grows_within_the_song() {
    let song = 4 * 60_000;
    assert_eq!(seek_step(Duration::ZERO, song), 1_000);
    assert_eq!(seek_step(Duration::from_secs(2), song), 3_000);
    assert_eq!(seek_step(Duration::from_secs(5), song), 4_000); // 1/60 of 4 min
    assert_eq!(seek_step(Duration::from_secs(5), 60 * 60_000), 10_000);
    assert_eq!(seek_step(Duration::from_secs(5), 30_000), 1_000);
}
