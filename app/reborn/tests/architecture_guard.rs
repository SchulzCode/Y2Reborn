//! Architecture guard: Y2-specific platform access lives behind
//! `reborn-platform`. New direct sysfs/procfs/device paths, Y2Linux runtime
//! files or subprocesses anywhere else fail this test until they are either
//! moved behind the platform client or explicitly justified below.
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Literal prefixes that address the platform rather than the application.
const PLATFORM_LITERALS: [&str; 9] = [
    "\"/sys/",
    "\"/proc/",
    "\"/dev/",
    "\"/run/y2",
    "\"/etc/y2linux",
    "\"/usr/sbin/y2",
    "\"/usr/bin/y2",
    "\"/data/system",
    "\"/run/wpa_supplicant",
];

/// Justified exceptions outside the platform crate: (file, literal, reason).
const ALLOWED: [(&str, &str, &str); 10] = [
    (
        "crates/reborn-observability/src/lib.rs",
        "\"/proc/",
        "standard Linux process/memory/kernel identity in diagnostic snapshots",
    ),
    (
        "crates/reborn-observability/src/lib.rs",
        "\"/sys/",
        "standard cpufreq/DRM inventory in diagnostic snapshots",
    ),
    (
        "crates/reborn-observability/src/lib.rs",
        "\"/dev/",
        "test-only invalid log directories",
    ),
    (
        "crates/reborn-library/src/lib.rs",
        "\"/proc/",
        "standard mountinfo identity and pinned directory handles for scanning",
    ),
    (
        "crates/reborn-library/src/lib.rs",
        "\"/dev/",
        "test fixtures for SD mount identity",
    ),
    (
        "crates/reborn-media/src/native.rs",
        "\"/dev/",
        "test-only invalid decoder input",
    ),
    (
        "app/reborn/src/main.rs",
        "\"/proc/",
        "own process status in the control-socket snapshot",
    ),
    (
        "app/reborn/src/main.rs",
        "\"/dev/",
        "bounded kernel error excerpt (/dev/kmsg) in diagnostic bundles",
    ),
    (
        "crates/reborn-library/src/benchmark.rs",
        "\"/proc/",
        "own process memory in explicit library benchmark results",
    ),
    (
        "app/reborn/src/bin/reborn-bench.rs",
        "\"/proc/",
        "host benchmark tool, never installed as UI",
    ),
];

/// Crates whose code must stay free of I/O: product state and presentation.
const PURE_CRATES: [&str; 2] = ["crates/reborn-ui/src", "crates/reborn-core/src"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn relative(path: &Path) -> String {
    path.strip_prefix(root())
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn y2_specific_access_stays_behind_the_platform_client() {
    let mut files = vec![];
    sources(&root().join("crates"), &mut files);
    sources(&root().join("app"), &mut files);
    let mut violations = vec![];
    for file in files {
        let name = relative(&file);
        if name.starts_with("crates/reborn-platform/") || name.starts_with("app/reborn/tests/") {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap();
        for (number, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for literal in PLATFORM_LITERALS {
                if code.contains(literal)
                    && !ALLOWED.iter().any(|(f, l, _)| *f == name && *l == literal)
                {
                    violations.push(format!("{name}:{}: {}", number + 1, line.trim()));
                }
            }
            let ui_or_app = !name.starts_with("crates/reborn-audio")
                && !name.starts_with("crates/reborn-media")
                && !name.starts_with("crates/reborn-library")
                && !name.starts_with("crates/reborn-graphics")
                && !name.starts_with("crates/reborn-observability")
                && !name.starts_with("crates/reborn-control");
            if ui_or_app && code.contains("Command::new") {
                violations.push(format!(
                    "{name}:{}: subprocess outside platform: {}",
                    number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "move these behind reborn-platform or justify them in ALLOWED:\n{}",
        violations.join("\n")
    );
}

#[test]
fn ui_and_core_perform_no_platform_io() {
    for dir in PURE_CRATES {
        let mut files = vec![];
        sources(&root().join(dir), &mut files);
        for file in files {
            let name = relative(&file);
            if name == "crates/reborn-ui/src/tests.rs" {
                // Host test module: may create and remove temporary files.
                continue;
            }
            let full = fs::read_to_string(&file).unwrap();
            // Production code only; host test modules may use temporary files.
            let text = full.split("#[cfg(test)]").next().unwrap_or("");
            // reborn-core owns the session file (application state); the UI owns nothing.
            let banned: &[&str] = if name.starts_with("crates/reborn-core") {
                &[
                    "std::process",
                    "Command::new",
                    "UnixStream",
                    "serde_json::Value",
                ]
            } else {
                &[
                    "std::fs",
                    "std::process",
                    "Command::new",
                    "UnixStream",
                    "serde_json::Value",
                    "json!(",
                ]
            };
            for term in banned {
                assert!(
                    !text.contains(term),
                    "{name} uses {term}; presentation and product state receive typed models only"
                );
            }
        }
    }
}
