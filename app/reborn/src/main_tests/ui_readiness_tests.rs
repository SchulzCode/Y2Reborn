use super::*;
#[test]
fn authentication_never_means_connected_and_problems_are_typed() {
    let mut s = wifi::Status {
        available: true,
        enabled: true,
        ssid: "Home".into(),
        state: "COMPLETED".into(),
        ..Default::default()
    };
    assert_eq!(
        wifi_view(&s).status,
        WifiStatus::GettingAddress("Home".into())
    );
    for state in [
        "Starting",
        "Scanning",
        "Associating",
        "Authenticated",
        "AcquiringIP",
    ] {
        s.readiness = state.into();
        assert!(!matches!(wifi_view(&s).status, WifiStatus::Connected(_)));
    }
    s.readiness = "Online".into();
    assert_eq!(wifi_view(&s).status, WifiStatus::Connected("Home".into()));
    // A late platform record falls back to the supplicant: associated
    // with an address is connected, not "Not connected" (Beta 4).
    s.readiness = String::new();
    s.readiness_reason = Some("platform_readiness_stale".into());
    s.ip = "192.0.2.2".into();
    assert_eq!(wifi_view(&s).status, WifiStatus::Connected("Home".into()));
    s.ip.clear();
    s.readiness = "Failed".into();
    s.readiness_reason = Some("wrong_credentials".into());
    assert_eq!(
        wifi_view(&s).problem,
        Some(reborn_core::platform::WifiProblem::WrongPassword)
    );
    // Y2Reborn #20: the saved network whose password failed is named; a
    // join already removed again is not a problem.
    s.saved = vec![wifi::Network {
        ssid: "Home".into(),
        saved_id: Some(4),
        ..Default::default()
    }];
    s.failed_network_id = Some(4);
    assert_eq!(wifi_view(&s).problem_network.as_deref(), Some("Home"));
    s.failed_network_id = Some(9);
    assert_eq!(wifi_view(&s).problem, None);
    s.failed_network_id = None;
    // Service text can never reach the screen: problems are an enum.
    // Only the platform's exact credential reasons mean a wrong password
    // (F52: "not_authenticated" after a wake read as one).
    s.readiness_reason = Some("org.secret.service credential=abc".into());
    assert_eq!(
        wifi_view(&s).problem,
        Some(reborn_core::platform::WifiProblem::Other)
    );
    s.readiness_reason = Some("not_authenticated".into());
    assert_ne!(
        wifi_view(&s).problem,
        Some(reborn_core::platform::WifiProblem::WrongPassword)
    );
}

#[test]
fn joined_network_is_listed_first_and_saved_networks_keep_their_identity() {
    let s = wifi::Status {
        available: true,
        enabled: true,
        ssid: "Home".into(),
        readiness: "Online".into(),
        networks: vec![
            wifi::Network {
                ssid: "Cafe".into(),
                signal: -40,
                security: "[WPA2-PSK-CCMP]".into(),
                saved_id: None,
            },
            wifi::Network {
                ssid: "Home".into(),
                signal: -75,
                security: "[WPA2-PSK-CCMP]".into(),
                saved_id: None,
            },
        ],
        saved: vec![wifi::Network {
            ssid: "Home".into(),
            signal: 0,
            security: String::new(),
            saved_id: Some(4),
        }],
        ..Default::default()
    };
    let view = wifi_view(&s);
    assert_eq!(view.networks[0].ssid, "Home");
    assert_eq!(view.networks[0].saved_id, Some(4));
    assert_eq!(view.networks.len(), 2, "saved and visible are merged");
}
