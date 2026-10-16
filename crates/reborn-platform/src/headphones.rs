//! Wired headphone load measurement through the CS43131 driver's `y2_hp`
//! sysfs group (Y2Linux #38): writing `hpload_measure` measures the load in
//! the jack, `hpload_dc` reports it ("L R raw l r", or "none") and `hp_gain`
//! the output level the codec chose for it (low, mid, high). Reborn owns the
//! stream the driver arbitrates against, as it owns the backlight it writes.
use reborn_core::platform::{HeadphoneLevel, HeadphoneLoad};
use std::{fs, io::Write, path::PathBuf};

fn group() -> Option<PathBuf> {
    fs::read_dir("/sys/bus/i2c/drivers/cs43130")
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("y2_hp"))
        .find(|path| path.is_dir())
}

/// What the codec offers now: nothing, no result yet, or the last result.
pub fn state() -> HeadphoneLoad {
    let Some(group) = group() else {
        return HeadphoneLoad::Unavailable;
    };
    match fs::read_to_string(group.join("hpload_dc"))
        .ok()
        .as_deref()
        .and_then(parse_load)
    {
        Some(ohms) => HeadphoneLoad::Measured {
            ohms,
            level: fs::read_to_string(group.join("hp_gain"))
                .ok()
                .as_deref()
                .and_then(parse_level),
        },
        None => HeadphoneLoad::Unmeasured,
    }
}

/// Measure the load in the jack (about a second, blocking). The driver
/// refuses while a stream owns the codec's clocks and when its plug detector
/// sees nothing in the jack.
pub fn measure() -> Result<HeadphoneLoad, String> {
    let group = group().ok_or("Headphone measurement is not available.")?;
    fs::OpenOptions::new()
        .write(true)
        .open(group.join("hpload_measure"))
        .and_then(|mut file| file.write_all(b"1\n"))
        .map_err(|error| refusal(error.raw_os_error()))?;
    match state() {
        measured @ HeadphoneLoad::Measured { .. } => Ok(measured),
        _ => Err(refusal(None)),
    }
}

/// The driver's answer in the owner's words.
fn refusal(errno: Option<i32>) -> String {
    match errno {
        Some(16) => "Pause music, then measure again.", // EBUSY: a stream runs
        Some(6) => "No headphones in the jack.",        // ENXIO: nothing plugged
        _ => "Couldn't measure. Push the plug fully in and try again.", // EIO, out of range
    }
    .into()
}

/// "19 18 raw 121 120" is 19 ohms left, 18 right (series resistance removed).
fn parse_load(text: &str) -> Option<(u16, u16)> {
    let mut parts = text.split_whitespace().map(str::parse::<u16>);
    Some((parts.next()?.ok()?, parts.next()?.ok()?))
}

/// "low", "mid" or "high"; a raw register value ("raw:30") is no level.
fn parse_level(text: &str) -> Option<HeadphoneLevel> {
    match text.trim() {
        "low" => Some(HeadphoneLevel::Low),
        "mid" => Some(HeadphoneLevel::Mid),
        "high" => Some(HeadphoneLevel::High),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_records_and_refusals_read_as_meant() {
        assert_eq!(parse_load("19 18 raw 121 120\n"), Some((19, 18)));
        assert_eq!(parse_load("none\n"), None);
        assert_eq!(parse_load(""), None);
        assert_eq!(parse_level("low\n"), Some(HeadphoneLevel::Low));
        assert_eq!(parse_level("high\n"), Some(HeadphoneLevel::High));
        assert_eq!(parse_level("raw:30\n"), None);
        assert!(refusal(Some(16)).starts_with("Pause music"));
        assert_eq!(refusal(Some(6)), "No headphones in the jack.");
        assert!(refusal(Some(5)).starts_with("Couldn't measure"));
    }
}
