#![forbid(unsafe_code)]
use reborn_core::{MediaSource, Source};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
#[derive(Clone, Debug)]
pub struct Mount {
    pub device: String,
    pub path: PathBuf,
    pub filesystem: String,
    pub options: String,
}
fn unescape(s: &str) -> String {
    s.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}
pub fn parse_mounts(s: &str) -> Vec<Mount> {
    s.lines()
        .filter_map(|l| {
            let v = l.split_whitespace().collect::<Vec<_>>();
            if v.len() < 4 {
                return None;
            }
            Some(Mount {
                device: unescape(v[0]),
                path: unescape(v[1]).into(),
                filesystem: v[2].into(),
                options: v[3].into(),
            })
        })
        .collect()
}
pub fn parse_mountinfo(s: &str) -> Vec<(u64, PathBuf)> {
    s.lines()
        .filter_map(|line| {
            let (mount, _) = line.split_once(" - ")?;
            let fields = mount.split_whitespace().collect::<Vec<_>>();
            Some((
                fields.get(0)?.parse().ok()?,
                unescape(fields.get(4)?).into(),
            ))
        })
        .collect()
}
pub fn mounts() -> Vec<Mount> {
    parse_mounts(&fs::read_to_string("/proc/mounts").unwrap_or_default())
}
pub fn sources(music: &Path) -> Vec<Source> {
    let m = mounts();
    let mountinfo =
        parse_mountinfo(&fs::read_to_string("/proc/self/mountinfo").unwrap_or_default());
    let mut result = vec![];
    for (mount_path, internal) in [("/data", true), ("/media/sd", false)] {
        if let Some(m) = m.iter().find(|m| m.path == Path::new(mount_path)) {
            if !internal && !Path::new(&m.device).exists() {
                continue;
            }
            let uuid =
                super::filesystem_uuid(&m.device).unwrap_or_else(|| format!("device:{}", m.device));
            let observed_mount = mountinfo
                .iter()
                .find_map(|(id, path)| (path == Path::new(mount_path)).then_some(*id));
            if !internal && Path::new("/etc/y2linux/capabilities.json").exists() {
                let claim = fs::read_to_string("/run/y2/media-mount.json")
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
                let boot =
                    fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
                let instance = claim.as_ref().and_then(|v| {
                    let device = v["device_id"].as_str()?;
                    if device.len() > 24 || !device.bytes().all(|b| b.is_ascii_digit() || b == b':')
                    {
                        return None;
                    }
                    use std::os::unix::fs::MetadataExt;
                    let meta = fs::metadata(format!("/sys/dev/block/{device}")).ok()?;
                    Some(format!(
                        "{}:{}",
                        meta.ino(),
                        meta.ctime() as i128 * 1_000_000_000 + meta.ctime_nsec() as i128
                    ))
                });
                if !sd_claim_matches(
                    claim.as_ref(),
                    &uuid,
                    observed_mount,
                    boot.trim(),
                    instance.as_deref(),
                ) {
                    continue;
                }
            }
            result.push(Source {
                id: if internal {
                    "internal".into()
                } else {
                    format!("uuid:{uuid}")
                },
                kind: if internal {
                    MediaSource::Internal
                } else {
                    MediaSource::SdCard(uuid)
                },
                root: if internal {
                    music.into()
                } else {
                    mount_path.into()
                },
                online: true,
                mount: m.device.clone(),
                mount_id: observed_mount,
            });
        }
    }
    result
}
fn sd_claim_matches(
    claim: Option<&serde_json::Value>,
    uuid: &str,
    mount: Option<u64>,
    boot: &str,
    instance: Option<&str>,
) -> bool {
    claim.is_some_and(|c| {
        c["schema"].as_u64() == Some(1)
            && !boot.is_empty()
            && c["boot_id"].as_str() == Some(boot)
            && c["uuid"].as_str() == Some(uuid)
            && mount.is_some()
            && c["mount_id"].as_u64() == mount
            && c["state"].as_str() == Some("Ready")
            && instance.is_some()
            && c["source_instance"].as_str() == instance
    })
}
pub fn platform_manages_media() -> bool {
    Path::new("/etc/y2linux/platform-contract").exists()
}
pub fn data_ready() -> bool {
    mounts()
        .iter()
        .any(|m| m.path == Path::new("/data") && m.filesystem == "ext4")
        && fs::read_to_string("/data/.y2data-schema").is_ok_and(|s| s.trim() == "1")
}
pub fn unmount_sd() -> Result<(), String> {
    helper("unmount")
}
pub fn mount_sd() -> Result<(), String> {
    if mounts().iter().any(|m| m.path == Path::new("/media/sd")) {
        return Ok(());
    }
    helper("mount")
}
fn helper(action: &str) -> Result<(), String> {
    // The platform owns filesystem selection/partition policy; no arbitrary shell or device argument.
    let mut child = Command::new("/usr/sbin/y2-media")
        .arg(action)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err("no unique supported SD filesystem".into())
            };
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            return Err("mount helper timeout".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}
pub fn sd_present() -> bool {
    fs::read_dir("/sys/class/block")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|e| {
            fs::canonicalize(e.path()).is_ok_and(|p| p.to_string_lossy().contains("/11240000.mmc/"))
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mount_names_not_block_numbers() {
        let m = parse_mounts(
            "/dev/mmcblk1p7 /data ext4 rw 0 0\n/dev/mmcblk0p1 /media/sd vfat rw 0 0\n",
        );
        assert_eq!(m[0].path, Path::new("/data"));
        assert_eq!(m[1].device, "/dev/mmcblk0p1");
    }
    #[test]
    fn escaped_mount() {
        assert_eq!(
            parse_mounts("/dev/a /media/a\\040b ext4 rw 0 0")[0].path,
            Path::new("/media/a b")
        );
    }

    #[test]
    fn mountinfo_keeps_mount_generation_and_decodes_mountpoint() {
        let parsed = parse_mountinfo(
            "36 25 179:7 / /media/sd\\040card rw,relatime shared:4 - vfat /dev/mmcblk0p1 rw\n",
        );
        assert_eq!(parsed, vec![(36, PathBuf::from("/media/sd card"))]);
    }
    #[test]
    fn new_platform_requires_matching_card_claim_and_boot() {
        let claim = serde_json::json!({"schema":1,"state":"Ready","uuid":"card-one","boot_id":"boot-one","mount_id":42,"source_instance":"1:2"});
        assert!(!sd_claim_matches(
            Some(&claim),
            "card-one",
            Some(42),
            "boot-one",
            Some("3:4")
        ));
        assert!(sd_claim_matches(
            Some(&claim),
            "card-one",
            Some(42),
            "boot-one",
            Some("1:2")
        ));
        assert!(!sd_claim_matches(
            Some(&claim),
            "card-two",
            Some(42),
            "boot-one",
            Some("1:2")
        ));
        assert!(!sd_claim_matches(
            Some(&claim),
            "card-one",
            Some(43),
            "boot-one",
            Some("1:2")
        ));
        assert!(!sd_claim_matches(
            Some(&claim),
            "card-one",
            Some(42),
            "boot-two",
            Some("1:2")
        ));
        assert!(!sd_claim_matches(
            None,
            "card-one",
            Some(42),
            "boot-one",
            Some("1:2")
        ));
    }
}
