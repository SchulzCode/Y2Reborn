// SPDX-License-Identifier: MIT
use super::*;

#[test]
fn read_only_reasons_round_trip() {
    for reason in [
        ReadOnly::ReadWrite,
        ReadOnly::Protected,
        ReadOnly::Checking,
        ReadOnly::FilesystemErrors,
        ReadOnly::CheckTimedOut,
    ] {
        assert_eq!(ReadOnly::of(&reason.value()), Some(reason));
    }
    assert_eq!(ReadOnly::of(&json!("mounted")), None);
    assert_eq!(ReadOnly::of(&json!(1)), None);
    assert!(ReadOnly::damaged(Some(ReadOnly::CheckTimedOut)));
    assert!(!ReadOnly::damaged(Some(ReadOnly::Protected)) && !ReadOnly::damaged(None));
}
