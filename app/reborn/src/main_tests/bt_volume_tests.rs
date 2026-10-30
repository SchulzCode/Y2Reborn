use super::{avrcp_to_percent, percent_to_avrcp, takeover_level};

#[test]
fn levels_round_trip_and_a_takeover_never_gets_louder() {
    for percent in 0..=100 {
        assert_eq!(avrcp_to_percent(percent_to_avrcp(percent)), percent);
    }
    assert_eq!((percent_to_avrcp(0), percent_to_avrcp(100)), (0, 127));
    // Software at 100 % (the #22 report): the headphones' own level stays.
    assert_eq!(takeover_level(40, 100), 40);
    // Software at 30 % with the headphones at full: 30 % on the headphones.
    assert_eq!(takeover_level(127, 30), 38);
    assert_eq!(takeover_level(0, 100), 0);
    assert!((0..=127).all(|r| takeover_level(r, 55) <= r));
}
