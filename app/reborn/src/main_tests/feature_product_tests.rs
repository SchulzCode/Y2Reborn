use super::*;
#[test]
fn paused_player_can_change_codec_without_a_hidden_stop_action() {
    for state in [PlaybackState::Stopped, PlaybackState::Paused] {
        assert!(codec_change_allowed(state, false));
        assert!(!codec_change_allowed(state, true));
    }
    for state in [
        PlaybackState::Playing,
        PlaybackState::Buffering,
        PlaybackState::Error,
    ] {
        assert!(!codec_change_allowed(state, false));
    }
}
