//! Settings → System → Diagnostics. The one deliberately technical subtree.
//! Sections arrive as typed facts from the platform client and application;
//! this module only lists them and the explicit, bounded checks.
use crate::Item;
use reborn_core::{
    platform::{Fact, HealthLevel},
    AppModel,
};

pub const SECTIONS: [(&str, &str); 11] = [
    ("battery", "Battery"),
    ("storage", "Storage"),
    ("network", "Network"),
    ("bluetooth", "Bluetooth"),
    ("audio", "Audio"),
    ("cpu", "CPU & Power"),
    ("usb", "USB"),
    ("update", "Update"),
    ("boot", "Boot & Services"),
    ("build", "Build Information"),
    ("capabilities", "Capabilities"),
];

pub fn title(id: &str) -> &'static str {
    match id {
        "health" => "Health",
        "result" => "Latest Result",
        _ => SECTIONS
            .iter()
            .find(|(key, _)| *key == id)
            .map(|(_, title)| *title)
            .unwrap_or("Diagnostics"),
    }
}

fn action(label: &str, key: &str, secondary: &str, enabled: bool) -> Item {
    let mut item = Item::new(label, key).with_secondary(secondary);
    item.enabled = enabled;
    item
}

pub fn root(m: &AppModel) -> Vec<Item> {
    let idle = m.platform.busy.is_none();
    let health = match m.platform.snapshot.health {
        HealthLevel::Ok => "All checks passed",
        HealthLevel::Degraded => "Some checks need attention",
        HealthLevel::Failed => "A check failed",
        HealthLevel::Unknown => "Run read-only checks",
    };
    let mut rows = vec![Item::new("Health", "diag:health").with_secondary(health)];
    rows.extend(
        SECTIONS
            .iter()
            .map(|(id, label)| Item::new(*label, format!("diag:{id}"))),
    );
    rows.extend([
        action(
            "Export Diagnostic Report",
            "task:diagnostics_export",
            "Redacted report for sharing",
            idle,
        ),
        action("Network Check", "task:network", "Read-only readiness", idle),
        action(
            "Storage Benchmark",
            "confirm:storage_benchmark",
            "16 MiB private scratch file",
            idle,
        ),
        action(
            "Library Benchmark",
            "confirm:library_benchmark",
            "1,000 synthetic tracks",
            idle,
        ),
        action(
            "Export Player Data",
            "task:export",
            "Private backup; keep it to yourself",
            idle,
        ),
        action(
            "Restore Previous System",
            "confirm:update_rollback",
            "Only after a verified update",
            idle && m.platform.snapshot.update.can_rollback,
        ),
        Item::new("Latest Result", "diag:result"),
    ]);
    rows
}

/// Facts of one section, including application-owned observations.
pub fn facts(m: &AppModel, id: &str) -> Vec<Fact> {
    let mut facts: Vec<Fact> = match id {
        "audio" => m.platform.audio_facts.clone(),
        "bluetooth" => m.platform.bluetooth_facts.clone(),
        "result" => {
            let mut f = vec![Fact::new(
                "Operation",
                if m.platform.busy.is_some() {
                    "In progress".into()
                } else if let Some(failure) = &m.platform.failure {
                    failure.clone()
                } else if let Some(result) = &m.platform.result {
                    if result.succeeded {
                        "Completed".into()
                    } else {
                        "Did not finish".into()
                    }
                } else {
                    "No operation has run".to_string()
                },
            )];
            if let Some(result) = &m.platform.result {
                f.extend(result.facts.iter().cloned());
            }
            return f;
        }
        _ => vec![],
    };
    if let Some(section) = m.platform.snapshot.section(id) {
        facts.extend(section.facts.iter().cloned());
    }
    if facts.is_empty() {
        facts.push(Fact::new(
            "Status",
            if m.platform.snapshot.observed {
                "Nothing reported"
            } else {
                "Reading platform status…"
            },
        ));
    }
    facts
}

pub fn section_rows(m: &AppModel, id: &str) -> Vec<Item> {
    let mut rows: Vec<Item> = facts(m, id)
        .into_iter()
        .map(|f| {
            Item::new(&f.label, format!("value:{}\u{1f}{}", f.label, f.value))
                .with_secondary(f.value)
        })
        .collect();
    let idle = m.platform.busy.is_none();
    rows.push(if id == "health" {
        action("Run Checks", "task:health", "Read-only", idle)
    } else {
        action("Refresh", "task:refresh", "", idle)
    });
    rows
}
