//! What the Wi-Fi, Bluetooth and audio pages show of the platform's state.
use crate::*;

/// Present one Wi-Fi observation in product terms.
pub(crate) fn wifi_view(s: &wifi::Status) -> WifiView {
    let status = if s.ssid.is_empty() {
        WifiStatus::Disconnected
    } else {
        let name = s.ssid.clone();
        match (s.readiness.as_str(), s.state.as_str()) {
            ("Online", _) => WifiStatus::Connected(name),
            ("Authenticated", _)
                if s.readiness_reason
                    .as_deref()
                    .is_some_and(|r| r.contains("dns")) =>
            {
                WifiStatus::NoInternet(name)
            }
            ("AcquiringIP", _) => WifiStatus::GettingAddress(name),
            ("Associating" | "Authenticated", _) => WifiStatus::Connecting(name),
            ("Failed" | "Off" | "Unavailable", _) => WifiStatus::Disconnected,
            // Without a platform readiness record, association alone is not
            // a working connection: require at least an address.
            (_, "COMPLETED") if !s.ip.is_empty() => WifiStatus::Connected(name),
            (_, "COMPLETED") => WifiStatus::GettingAddress(name),
            (_, "ASSOCIATING" | "ASSOCIATED" | "4WAY_HANDSHAKE" | "GROUP_HANDSHAKE") => {
                WifiStatus::Connecting(name)
            }
            _ => WifiStatus::Disconnected,
        }
    };
    let bars = |signal: i32| match signal {
        s if s >= -60 => 3,
        s if s >= -70 => 2,
        s if s >= -80 => 1,
        _ => 0,
    };
    let joined = status.network().map(str::to_owned);
    let mut networks: Vec<NetworkView> = s
        .saved
        .iter()
        .filter_map(|n| {
            let visible = s.networks.iter().find(|v| v.ssid == n.ssid);
            Some(NetworkView {
                ssid: n.ssid.clone(),
                bars: visible.map(|v| bars(v.signal)).unwrap_or(0),
                secured: visible.and_then(|v| v.password_required()).or(Some(true)),
                saved_id: Some(n.saved_id?),
                visible: visible.is_some(),
            })
        })
        .collect();
    networks.extend(
        s.networks
            .iter()
            .filter(|n| !s.saved.iter().any(|saved| saved.ssid == n.ssid))
            .map(|n| NetworkView {
                ssid: n.ssid.clone(),
                bars: bars(n.signal),
                secured: n.password_required(),
                saved_id: None,
                visible: true,
            }),
    );
    networks.sort_by_key(|n| {
        (
            joined.as_deref() != Some(n.ssid.as_str()),
            n.saved_id.is_none(),
            !n.visible,
            std::cmp::Reverse(n.bars),
        )
    });
    let problem = wifi::problem(s);
    WifiView {
        available: s.available,
        powered: s.enabled,
        scan: s.scan.clone(),
        problem_network: s
            .failed_network_id
            .filter(|_| problem == Some(reborn_core::platform::WifiProblem::WrongPassword))
            .and_then(|id| s.saved.iter().find(|n| n.saved_id == Some(id)))
            .map(|n| n.ssid.clone()),
        problem,
        join: s.join.as_ref().map(|j| reborn_ui::WifiJoin {
            attempt: j.attempt,
            ssid: j.ssid.clone(),
            outcome: match j.outcome {
                wifi::JoinOutcome::Checking => reborn_ui::WifiJoinOutcome::Checking,
                wifi::JoinOutcome::Joined => reborn_ui::WifiJoinOutcome::Joined,
                wifi::JoinOutcome::WrongPassword => reborn_ui::WifiJoinOutcome::WrongPassword,
                wifi::JoinOutcome::NotFound => reborn_ui::WifiJoinOutcome::NotFound,
                wifi::JoinOutcome::Failed => reborn_ui::WifiJoinOutcome::Failed,
            },
        }),
        status,
        networks,
    }
}

/// Present one Bluetooth observation in product terms.
pub(crate) fn bluetooth_view(s: &bluetooth::Status) -> BluetoothView {
    BluetoothView {
        available: s.available,
        powered: s.powered,
        scan: s.scan.clone(),
        devices: s
            .devices
            .iter()
            .map(|d| BluetoothDeviceView {
                path: d.path.clone(),
                address: d.address.clone(),
                name: if d.name.is_empty() {
                    "Bluetooth device".into()
                } else {
                    d.name.clone()
                },
                paired: d.paired,
                bonded: d.bonded,
                connected: d.connected,
                audio_ready: s.pcms.iter().any(|pcm| pcm.is_a2dp_playback_for(&d.path)),
                codec: s.active_codec(&d.path),
                connecting: s.waiting.as_deref() == Some(d.path.as_str()),
            })
            .collect(),
        codec_choices: s.codec_choices(),
        sbc_quality_available: s.codec_policy.as_ref().is_some_and(|p| {
            p.eligibility
                .get("SBC")
                .is_some_and(|e| e.compiled_locally && e.runtime_enabled && e.mutually_usable)
        }),
        problem: s
            .error
            .as_deref()
            .map(|e| format!("Couldn't complete that. {}", bluetooth_hint(e))),
    }
}

pub(crate) fn bluetooth_hint(error: &str) -> &'static str {
    let e = error.to_ascii_lowercase();
    if e.contains("auth") || e.contains("reject") || e.contains("canceled") {
        "Put the device in pairing mode and try again."
    } else if e.contains("timeout") || e.contains("host is down") || e.contains("page") {
        "Make sure the device is on and nearby."
    } else {
        "Try again."
    }
}

/// Application-owned audio pipeline facts for Diagnostics → Audio.
pub(crate) fn audio_facts(a: &Value, model: &AppModel) -> Vec<Fact> {
    let shown = |v: &Value| match v {
        Value::Null => "Not reported".to_owned(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    vec![
        Fact::new("Source codec", shown(&a["source"]["codec"])),
        Fact::new("Source rate (Hz)", shown(&a["source"]["sample_rate"])),
        Fact::new("Source bits", shown(&a["source"]["source_bits"])),
        Fact::new("Decoded format", shown(&a["decoder_format"])),
        Fact::new("DSP format", shown(&a["internal_processing_format"])),
        Fact::new(
            "ReplayGain applied (dB)",
            shown(&a["replay_gain"]["applied_gain_db"]),
        ),
        Fact::new("Crossfade (ms)", model.settings.crossfade_ms.to_string()),
        Fact::new(
            "Sink",
            if model.playback == PlaybackState::Playing {
                "Current playback".to_owned()
            } else {
                "Last opened; playback inactive".to_owned()
            },
        ),
        Fact::new("PCM format", shown(&a["alsa"]["format"])),
        Fact::new("PCM rate (Hz)", shown(&a["alsa"]["rate"])),
        Fact::new("PCM channels", shown(&a["alsa"]["channels"])),
    ]
}
