//! Local time without a zone database: the system clock is UTC (kept by
//! NTP and the RTC), and a zone's POSIX TZ rule (`zones::ZONES`, from the tz
//! database) turns it into local time, daylight saving included.

/// A zone's offsets and daylight-saving rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Standard time, seconds east of UTC.
    pub standard: i32,
    dst: Option<Dst>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Dst {
    /// Daylight time, seconds east of UTC.
    offset: i32,
    start: (Day, i32),
    end: (Day, i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Day {
    /// `Jn`: day 1..=365, February 29 never counted.
    NoLeap(u16),
    /// `n`: day 0..=365, February 29 counted.
    Leap(u16),
    /// `Mm.w.d`: weekday d (0 = Sunday) of week w (5 = last) of month m.
    Month(u8, u8, u8),
}

/// A moment in local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
}

/// The rule for a tz database zone name, e.g. "America/New_York".
pub fn zone(name: &str) -> Option<Rule> {
    crate::zones::ZONES
        .iter()
        .find(|(n, _)| *n == name)
        .and_then(|(_, rule)| parse(rule))
}

/// A country of the tz database: ISO code, English name and its zones.
pub type Country = (&'static str, &'static str, &'static [&'static str]);

/// Every country with at least one zone, A to Z by name.
pub fn countries() -> &'static [Country] {
    crate::countries::COUNTRIES
}

/// The country a zone belongs to (none for UTC).
pub fn country_of(zone: &str) -> Option<&'static Country> {
    countries()
        .iter()
        .find(|(_, _, zones)| zones.contains(&zone))
}

/// Every zone name with its rule.
pub fn zones() -> impl Iterator<Item = (&'static str, Rule)> {
    crate::zones::ZONES
        .iter()
        .filter_map(|(name, rule)| Some((*name, parse(rule)?)))
}

/// A zone name for people: "New York", with the region ("America").
pub fn zone_label(name: &str) -> (String, &str) {
    let (region, city) = name.rsplit_once('/').unwrap_or(("", name));
    let region = region.split('/').next().unwrap_or(region);
    (city.replace('_', " "), region)
}

/// "UTC-5", "UTC+5:30" for an offset in seconds (ASCII minus: the UI
/// fonts have no U+2212).
pub fn offset_label(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let minutes = seconds.unsigned_abs() / 60;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "UTC".into(),
        (h, 0) => format!("UTC{sign}{h}"),
        (h, m) => format!("UTC{sign}{h}:{m:02}"),
    }
}

impl Rule {
    pub const UTC: Rule = Rule {
        standard: 0,
        dst: None,
    };

    /// Seconds east of UTC at the instant `utc` (unix seconds).
    pub fn offset_at(&self, utc: i64) -> i32 {
        let Some(dst) = &self.dst else {
            return self.standard;
        };
        let year = civil(utc + i64::from(self.standard)).year;
        let start =
            day_of(dst.start.0, year) * 86_400 + i64::from(dst.start.1) - i64::from(self.standard);
        let end = day_of(dst.end.0, year) * 86_400 + i64::from(dst.end.1) - i64::from(dst.offset);
        let summer = if start < end {
            utc >= start && utc < end
        } else {
            // Southern hemisphere: daylight time spans the new year.
            !(utc >= end && utc < start)
        };
        if summer {
            dst.offset
        } else {
            self.standard
        }
    }

    pub fn local(&self, utc: i64) -> Local {
        civil(utc + i64::from(self.offset_at(utc)))
    }

    /// The instant (unix seconds) a local time names: the inverse of
    /// `local`. Around a daylight-saving change a time is read as standard
    /// time: one skipped in spring (02:30) lands an hour later, one repeated
    /// in autumn is its second instance.
    pub fn utc_of(&self, t: &Local) -> i64 {
        let local = t.seconds();
        let guess = local - i64::from(self.offset_at(local - i64::from(self.standard)));
        local - i64::from(self.offset_at(guess))
    }
}

impl Local {
    /// Seconds since 1970 of these calendar fields (no zone applied): the
    /// inverse of `civil`.
    pub fn seconds(&self) -> i64 {
        days_from(self.year, self.month, self.day) * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
    }
}

/// The days of a month (1 = January).
pub fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        2 if leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Calendar fields of a count of seconds since 1970 (no zone applied).
pub fn civil(seconds: i64) -> Local {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = from_days(days);
    Local {
        year,
        month,
        day,
        hour: (rest / 3600) as u8,
        minute: (rest % 3600 / 60) as u8,
    }
}

/// "9:41 PM" (12-hour) or "21:41". The status bar showed 12-hour times without
/// AM or PM (Y2Linux #72).
pub fn clock_text(t: &Local, twelve_hour: bool) -> String {
    if twelve_hour {
        let h = match t.hour % 12 {
            0 => 12,
            h => h,
        };
        let half = if t.hour < 12 { "AM" } else { "PM" };
        format!("{h}:{:02} {half}", t.minute)
    } else {
        format!("{:02}:{:02}", t.hour, t.minute)
    }
}

/// The short date skins show (`{date}`): "Sat 31 Oct", or "Sat, Oct 31" where
/// the zone reads a 12-hour clock.
pub fn date_text(t: &Local, twelve_hour: bool) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // 1970-01-01 (day 0) was a Thursday.
    let weekday = DAYS[(days_from(t.year, t.month, t.day) + 4).rem_euclid(7) as usize];
    let month = MONTHS[usize::from(t.month.clamp(1, 12)) - 1];
    if twelve_hour {
        format!("{weekday}, {month} {}", t.day)
    } else {
        format!("{weekday} {} {month}", t.day)
    }
}

/// Zones whose people mostly read a 12-hour clock.
pub fn twelve_hour(zone: &str) -> bool {
    const TWELVE: &[&str] = &[
        "America/",
        "Pacific/Honolulu",
        "Asia/Kolkata",
        "Asia/Manila",
        "Asia/Karachi",
        "Asia/Dhaka",
        "Australia/",
        "Pacific/Auckland",
        "Africa/Cairo",
    ];
    const TWENTY_FOUR: &[&str] = &[
        "America/Sao_Paulo",
        "America/Argentina",
        "America/Bogota",
        "America/Lima",
        "America/Santiago",
        "America/Caracas",
        "America/Montevideo",
        "America/Asuncion",
        "America/La_Paz",
        "America/Havana",
        "America/Cayenne",
        "America/Paramaribo",
    ];
    TWELVE.iter().any(|p| zone.starts_with(p)) && !TWENTY_FOUR.iter().any(|p| zone.starts_with(p))
}

// Howard Hinnant's civil-date algorithms.
fn days_from(year: i32, month: u8, day: u8) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    ((yoe + era * 400 + i64::from(month <= 2)) as i32, month, day)
}

fn leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// The day (days since 1970) a rule day falls on in `year`.
fn day_of(day: Day, year: i32) -> i64 {
    let jan1 = days_from(year, 1, 1);
    match day {
        Day::NoLeap(n) => {
            let n = i64::from(n.clamp(1, 365));
            jan1 + n - 1 + i64::from(leap(year) && n >= 60)
        }
        Day::Leap(n) => jan1 + i64::from(n.min(365)),
        Day::Month(m, w, d) => {
            let first = days_from(year, m, 1);
            let wd = (first + 4).rem_euclid(7);
            let mut at = first + (i64::from(d) - wd).rem_euclid(7) + 7 * (i64::from(w) - 1);
            let next = if m == 12 {
                days_from(year + 1, 1, 1)
            } else {
                days_from(year, m + 1, 1)
            };
            while at >= next {
                at -= 7;
            }
            at
        }
    }
}

/// Parses a POSIX TZ rule such as `EST5EDT,M3.2.0,M11.1.0` or
/// `<+1030>-10:30<+11>-11,M10.1.0,M4.1.0`.
pub fn parse(rule: &str) -> Option<Rule> {
    let mut s = rule;
    name(&mut s)?;
    let standard = -offset(&mut s)?;
    if s.is_empty() {
        return Some(Rule {
            standard,
            dst: None,
        });
    }
    name(&mut s)?;
    let daylight = if s.starts_with(',') {
        standard + 3600
    } else {
        -offset(&mut s)?
    };
    let rules = s.strip_prefix(',')?;
    let (start, end) = rules.split_once(',')?;
    Some(Rule {
        standard,
        dst: Some(Dst {
            offset: daylight,
            start: transition(start)?,
            end: transition(end)?,
        }),
    })
}

fn name(s: &mut &str) -> Option<()> {
    let len = if let Some(rest) = s.strip_prefix('<') {
        rest.find('>')? + 2
    } else {
        s.find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(s.len())
    };
    if len < 3 {
        return None;
    }
    *s = &s[len..];
    Some(())
}

/// `[+-]hh[:mm[:ss]]` in seconds, as written (west of UTC is positive).
fn offset(s: &mut &str) -> Option<i32> {
    let end = s
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, ':' | '+' | '-')))
        .unwrap_or(s.len());
    let value = hms(&s[..end])?;
    *s = &s[end..];
    Some(value)
}

fn hms(text: &str) -> Option<i32> {
    let (sign, digits) = match text.as_bytes().first()? {
        b'-' => (-1, &text[1..]),
        b'+' => (1, &text[1..]),
        _ => (1, text),
    };
    let mut parts = digits.split(':');
    let h: i32 = parts.next()?.parse().ok()?;
    let m: i32 = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let sec: i32 = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    Some(sign * (h * 3600 + m * 60 + sec))
}

fn transition(text: &str) -> Option<(Day, i32)> {
    let (date, time) = match text.split_once('/') {
        Some((d, t)) => (d, hms(t)?),
        None => (text, 7200),
    };
    let day = if let Some(m) = date.strip_prefix('M') {
        let mut p = m.split('.').map(|v| v.parse::<u8>().ok());
        let (m, w, d) = (p.next()??, p.next()??, p.next()??);
        if !(1..=12).contains(&m) || !(1..=5).contains(&w) || d > 6 {
            return None;
        }
        Day::Month(m, w, d)
    } else if let Some(n) = date.strip_prefix('J') {
        Day::NoLeap(n.parse().ok()?)
    } else {
        Day::Leap(date.parse().ok()?)
    };
    Some((day, time))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u8, d: u8, h: i64, mi: i64) -> i64 {
        days_from(y, mo, d) * 86_400 + h * 3600 + mi * 60
    }

    #[test]
    fn every_generated_zone_parses() {
        for (name, rule) in crate::zones::ZONES {
            assert!(parse(rule).is_some(), "{name}: {rule}");
        }
        assert!(crate::zones::ZONES.len() > 300);
        assert_eq!(zone("UTC"), Some(Rule::UTC));
    }

    #[test]
    fn civil_dates_round_trip() {
        for days in [-1_000_000, -1, 0, 59, 11_016, 20_738, 1_000_000] {
            let (y, m, d) = from_days(days);
            assert_eq!(days_from(y, m, d), days);
        }
        let t = civil(at(2026, 10, 12, 3, 31));
        assert_eq!(
            (t.year, t.month, t.day, t.hour, t.minute),
            (2026, 10, 12, 3, 31)
        );
    }

    #[test]
    fn new_york_follows_daylight_saving() {
        let ny = zone("America/New_York").unwrap();
        // 2026: DST from Sunday March 8 07:00 UTC to Sunday November 1 06:00 UTC.
        assert_eq!(ny.offset_at(at(2026, 3, 8, 6, 59)), -5 * 3600);
        assert_eq!(ny.offset_at(at(2026, 3, 8, 7, 0)), -4 * 3600);
        assert_eq!(ny.offset_at(at(2026, 11, 1, 5, 59)), -4 * 3600);
        assert_eq!(ny.offset_at(at(2026, 11, 1, 6, 0)), -5 * 3600);
        let t = ny.local(at(2026, 10, 12, 3, 31));
        assert_eq!((t.day, t.hour, t.minute), (11, 23, 31));
        assert_eq!(clock_text(&t, true), "11:31 PM");
        assert_eq!(clock_text(&t, false), "23:31");
        assert_eq!(date_text(&t, true), "Sun, Oct 11");
        assert_eq!(date_text(&t, false), "Sun 11 Oct");
    }

    #[test]
    fn twelve_hour_clock_reads_midnight_and_noon_as_twelve() {
        let at = |hour| Local {
            year: 2026,
            month: 10,
            day: 31,
            hour,
            minute: 5,
        };
        assert_eq!(clock_text(&at(0), true), "12:05 AM");
        assert_eq!(clock_text(&at(11), true), "11:05 AM");
        assert_eq!(clock_text(&at(12), true), "12:05 PM");
        assert_eq!(clock_text(&at(0), false), "00:05");
        assert_eq!(date_text(&at(0), false), "Sat 31 Oct");
    }

    #[test]
    fn southern_and_half_hour_zones() {
        let lh = zone("Australia/Lord_Howe").unwrap();
        assert_eq!(lh.offset_at(at(2026, 1, 15, 0, 0)), 11 * 3600);
        assert_eq!(lh.offset_at(at(2026, 7, 15, 0, 0)), 10 * 3600 + 1800);
        let syd = zone("Australia/Sydney").unwrap();
        assert_eq!(syd.offset_at(at(2026, 1, 15, 0, 0)), 11 * 3600);
        assert_eq!(syd.offset_at(at(2026, 7, 15, 0, 0)), 10 * 3600);
        let india = zone("Asia/Kolkata").unwrap();
        assert_eq!(india.offset_at(at(2026, 7, 15, 0, 0)), 5 * 3600 + 1800);
        assert_eq!(offset_label(india.standard), "UTC+5:30");
        assert_eq!(offset_label(-5 * 3600), "UTC-5");
        let london = zone("Europe/London").unwrap();
        assert_eq!(london.offset_at(at(2026, 3, 29, 0, 59)), 0);
        assert_eq!(london.offset_at(at(2026, 3, 29, 1, 0)), 3600);
    }

    #[test]
    fn local_times_turn_back_into_instants() {
        let ny = zone("America/New_York").unwrap();
        let berlin = zone("Europe/Berlin").unwrap();
        for utc in [
            at(2026, 1, 15, 12, 0),
            at(2026, 7, 15, 23, 59),
            at(2026, 12, 31, 23, 30),
        ] {
            for rule in [&ny, &berlin, &Rule::UTC] {
                assert_eq!(rule.utc_of(&rule.local(utc)), utc);
            }
        }
        // 02:30 on 8 March 2026 does not exist in New York: 03:30 daylight time.
        let skipped = Local {
            year: 2026,
            month: 3,
            day: 8,
            hour: 2,
            minute: 30,
        };
        assert_eq!(ny.utc_of(&skipped), at(2026, 3, 8, 7, 30));
        assert_eq!(ny.local(ny.utc_of(&skipped)).hour, 3);
        // 01:30 on 1 November 2026 happens twice: the second one.
        let twice = Local {
            year: 2026,
            month: 11,
            day: 1,
            hour: 1,
            minute: 30,
        };
        assert_eq!(ny.utc_of(&twice), at(2026, 11, 1, 6, 30));
        assert_eq!(civil(twice.seconds()), twice);
        assert_eq!(days_in_month(2028, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2026, 11), 30);
        assert_eq!(days_in_month(2026, 12), 31);
    }

    #[test]
    fn names_and_clock_styles() {
        assert_eq!(
            zone_label("America/Argentina/Buenos_Aires"),
            ("Buenos Aires".into(), "America")
        );
        assert_eq!(zone_label("UTC"), ("UTC".into(), ""));
        assert!(twelve_hour("America/New_York"));
        assert!(!twelve_hour("America/Sao_Paulo"));
        assert!(!twelve_hour("Europe/Berlin"));
        assert!(!twelve_hour("UTC"));
        assert!(zone("Mars/Olympus").is_none());
        assert_eq!(parse("UTC0"), Some(Rule::UTC));
    }
}
