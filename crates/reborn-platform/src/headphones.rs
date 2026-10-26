//! Wired headphone load measurement through the CS43131 driver's `y2_hp`
//! sysfs group (Y2Linux #38): writing `hpload_measure` measures the load in
//! the jack and `hpload_dc` reports it ("L R raw l r", or "none"). Since
//! Beta 8.5 the measurement leaves the output level alone: that stays the
//! Headphone Level Reborn sets through "HP Output Level". Reborn owns the
//! stream the driver arbitrates against, as it owns the backlight it writes.
use reborn_core::platform::HeadphoneLoad;
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
        Some(ohms) => HeadphoneLoad::Measured { ohms },
        None => HeadphoneLoad::Unmeasured,
    }
}

/// Measure the load in the jack (about a second, blocking). The driver
/// refuses while a stream is open and when its plug detector sees nothing in
/// the jack.
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
        Some(16) => "Pause music, then measure again.", // EBUSY: a stream is open
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_records_and_refusals_read_as_meant() {
        assert_eq!(parse_load("19 18 raw 121 120\n"), Some((19, 18)));
        assert_eq!(parse_load("none\n"), None);
        assert_eq!(parse_load(""), None);
        assert!(refusal(Some(16)).starts_with("Pause music"));
        assert_eq!(refusal(Some(6)), "No headphones in the jack.");
        assert!(refusal(Some(5)).starts_with("Couldn't measure"));
    }
}
