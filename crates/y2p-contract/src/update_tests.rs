// SPDX-License-Identifier: MIT
use super::*;
use serde_json::json;

#[test]
fn reborn_reads_back_what_update_status_prints() {
    let printed = Status {
        journal: CoreJournal {
            rollback_allowed: Some(true),
            ..CoreJournal::stand_in("Failed", Some("update_cancelled"))
        },
        records: Some(Records {
            download: Some(Download {
                release_version: Some(json!("9.1")),
                download_bytes: Some(57),
                ..Download::state("Checked")
            }),
            last_check: None,
            channel: Some("stable".into()),
            full_system: true,
            physical_qualification: false,
        }),
    };
    let read: Status = serde_json::from_value(serde_json::to_value(&printed).unwrap()).unwrap();
    assert_eq!(read.journal.state.as_deref(), Some("Failed"));
    assert_eq!(read.failure(), Some("update_cancelled"));
    assert_eq!(read.journal.rollback_allowed, Some(true));
    let download = read.download().unwrap();
    assert!(download.is("Checked"));
    assert_eq!(download.release(), Some("9.1"));
    assert_eq!(download.download_bytes, Some(57));
}

#[test]
fn an_unreadable_journal_is_shown_without_records() {
    let printed = Status {
        journal: CoreJournal::stand_in("Failed", Some("journal_unreadable")),
        records: None,
    };
    let text = serde_json::to_value(&printed).unwrap();
    assert_eq!(
        text,
        json!({"schema": 1, "state": "Failed", "failure": "journal_unreadable"})
    );
    let read: Status = serde_json::from_value(text).unwrap();
    assert!(read.download().is_none());
}

#[test]
fn a_journal_field_of_another_type_reads_as_none() {
    let read: Status = serde_json::from_value(json!({"state": 3, "failure": null,
        "rollback_allowed": "yes", "backup": {"bytes": 1}}))
    .unwrap();
    assert_eq!(read.journal.state, None);
    assert_eq!(read.failure(), None);
    assert_eq!(read.journal.rollback_allowed, None);
    assert!(!read.journal.terminal());
}

#[test]
fn the_live_record_keeps_its_order() {
    let progress = DownloadProgress {
        schema: 1,
        phase: DownloadPhase::Verify,
        release_version: json!("9.1"),
        bytes_done: 5,
        bytes_total: 10,
        speed_bytes_per_s: None,
        seconds_left: None,
    };
    let text = serde_json::to_string(&progress).unwrap();
    assert_eq!(
        text,
        r#"{"schema":1,"phase":"verify","release_version":"9.1","bytes_done":5,"bytes_total":10,"speed_bytes_per_s":null,"seconds_left":null}"#
    );
    assert_eq!(
        serde_json::from_str::<DownloadProgress>(&text).unwrap(),
        progress
    );
}
