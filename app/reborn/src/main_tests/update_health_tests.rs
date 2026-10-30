use super::update_health;
use serde_json::json;

#[test]
fn an_incomplete_scan_is_healthy_and_a_failed_database_is_not() {
    let scanner = json!({"subsystems":{"scanner":{"state":"degraded","required":true,"detail":"scan incomplete"}}});
    assert_eq!(update_health(false, true, 30, &scanner), (true, vec![]));
    let database =
        json!({"subsystems":{"database":{"state":"failed","required":true,"detail":"x"}}});
    assert_eq!(
        update_health(false, true, 30, &database).1,
        vec!["database failed"]
    );
    let stuck =
        json!({"subsystems":{"worker_scanner":{"state":"failed","required":true,"detail":"x"}}});
    assert!(!update_health(false, true, 30, &stuck).0);
    assert_eq!(
        update_health(false, false, 5, &json!({})).1,
        vec!["graphics unavailable", "starting"]
    );
    assert!(update_health(true, false, 30, &json!({})).0);
}
