//! Settings > Display > Time Zone: Automatic, UTC and the countries A to Z,
//! a country's cities west to east, and the clock it gives the status bar.
use crate::{Item, RowKind, Ui};
use reborn_core::{
    clock::{self, offset_label, zone_label, Rule},
    AppModel,
};

/// Before this instant (2025-01-01) the system clock has not been set; an
/// unset clock reads 1970, so nothing is shown rather than a wrong time.
const CLOCK_SET_AFTER: i64 = 1_735_689_600;

pub(crate) fn now(ui: &Ui) -> i64 {
    ui.clock_fixed.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64)
    })
}

/// The chosen zone's rule; a name the table no longer has reads as UTC.
pub(crate) fn rule(m: &AppModel) -> Rule {
    clock::zone(&m.settings.time_zone).unwrap_or(Rule::UTC)
}

/// The status-bar clock ("9:41 PM" or "21:41") and the skins' short date.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Clock {
    pub time: String,
    pub date: String,
}

/// The clock in the chosen zone, once the system time is set. Made once a
/// minute: every other call (each main-loop pass, each skin element) reuses it.
pub(crate) fn read(ui: &Ui, m: &AppModel) -> Option<Clock> {
    let now = now(ui);
    let zone = &m.settings.time_zone;
    let minute = now.div_euclid(60);
    if let Some((z, at, clock)) = &*ui.clock_cache.borrow() {
        if z == zone && *at == minute {
            return clock.clone();
        }
    }
    let clock = (now >= CLOCK_SET_AFTER).then(|| {
        let (local, twelve) = (rule(m).local(now), clock::twelve_hour(zone));
        Clock {
            time: clock::clock_text(&local, twelve),
            date: clock::date_text(&local, twelve),
        }
    });
    *ui.clock_cache.borrow_mut() = Some((zone.clone(), minute, clock.clone()));
    clock
}

/// The Display row's value: the city, "Automatic · city", or UTC.
pub(crate) fn summary(m: &AppModel) -> String {
    let city = zone_label(&m.settings.time_zone).0;
    match (m.settings.zone_auto(), m.settings.time_zone.as_str()) {
        (true, "UTC") => "Automatic".into(),
        (true, _) => format!("Automatic · {city}"),
        (false, _) => city,
    }
}

/// The one zone a country needs, when all its zones keep the same time
/// (Germany's Berlin and Büsingen): choosing the country then sets it.
pub(crate) fn single_zone(zones: &[&'static str]) -> Option<&'static str> {
    let first = *zones.first()?;
    let rule = clock::zone(first);
    zones
        .iter()
        .all(|z| clock::zone(z) == rule)
        .then_some(first)
}

/// The country whose cities the picker shows (filter `country:<code>`).
fn country(m: &AppModel) -> Option<&'static clock::Country> {
    let code = m.navigation.filter.strip_prefix("country:")?;
    clock::countries().iter().find(|(c, _, _)| *c == code)
}

/// The picker's title: the country while its cities are shown.
pub(crate) fn title(m: &AppModel) -> Option<&'static str> {
    country(m).map(|(_, name, _)| *name)
}

pub(crate) fn rows(ui: &Ui, m: &AppModel) -> Vec<Item> {
    let key = format!(
        "{}|{}|{}",
        m.navigation.filter,
        m.settings.time_zone,
        m.settings.zone_auto()
    );
    let minute = now(ui).div_euclid(60);
    if let Some((k, at, rows)) = &*ui.zone_rows.borrow() {
        if *k == key && *at == minute {
            return rows.clone();
        }
    }
    let rows = match country(m) {
        Some(c) => city_rows(ui, m, c),
        None => country_rows(ui, m),
    };
    *ui.zone_rows.borrow_mut() = Some((key, minute, rows.clone()));
    rows
}

/// Automatic, UTC, then every country A to Z (Beta 7.1: a short list to
/// scroll instead of every zone of the world).
fn country_rows(ui: &Ui, m: &AppModel) -> Vec<Item> {
    let (chosen, auto) = (m.settings.time_zone.as_str(), m.settings.zone_auto());
    let here = clock::country_of(chosen).map(|c| c.0);
    let now = now(ui);
    let mut rows = vec![
        Item::new("Automatic", "tz_auto")
            .with_secondary(if auto && chosen != "UTC" {
                zone_label(chosen).0
            } else {
                "Uses Wi-Fi to find the zone".into()
            })
            .with_kind(RowKind::Toggle(auto)),
        Item::new("UTC", "tz_set:UTC").with_kind(RowKind::Check(!auto && chosen == "UTC")),
    ];
    rows.extend(clock::countries().iter().map(|(code, name, zones)| {
        let detail = match single_zone(zones) {
            Some(zone) => {
                clock::zone(zone).map_or(String::new(), |r| offset_label(r.offset_at(now)))
            }
            None => format!("{} time zones", zones.len()),
        };
        Item::new(*name, format!("tz_country:{code}"))
            .with_secondary(detail)
            .with_kind(RowKind::Check(!auto && here == Some(*code)))
    }));
    rows
}

/// One country's cities, west to east by their offset now.
fn city_rows(ui: &Ui, m: &AppModel, (_, _, zones): &clock::Country) -> Vec<Item> {
    let chosen = m.settings.time_zone.as_str();
    let now = now(ui);
    let mut cities: Vec<(&str, i32)> = zones
        .iter()
        .filter_map(|z| Some((*z, clock::zone(z)?.offset_at(now))))
        .collect();
    cities.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| zone_label(a.0).0.cmp(&zone_label(b.0).0))
    });
    cities
        .into_iter()
        .map(|(zone, offset)| {
            Item::new(zone_label(zone).0, format!("tz_set:{zone}"))
                .with_secondary(offset_label(offset))
                .with_kind(RowKind::Check(chosen == zone))
        })
        .collect()
}

/// The row to focus when the picker opens: the current choice.
pub(crate) fn focus(ui: &Ui, m: &AppModel) -> usize {
    let rows = rows(ui, m);
    let chosen = m.settings.time_zone.as_str();
    if country(m).is_none() {
        if m.settings.zone_auto() {
            return 0;
        }
        let here = clock::country_of(chosen).map(|c| format!("tz_country:{}", c.0));
        let key = here.unwrap_or_else(|| "tz_set:UTC".into());
        return rows.iter().position(|r| r.key == key).unwrap_or(0);
    }
    let key = format!("tz_set:{chosen}");
    rows.iter().position(|r| r.key == key).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_lists_automatic_utc_then_countries_and_a_countrys_cities() {
        let ui = Ui {
            clock_fixed: Some(1_791_775_896),
            ..Default::default()
        };
        let mut m = AppModel::default();
        let rows = rows(&ui, &m);
        assert_eq!(rows[0].key, "tz_auto");
        assert_eq!(rows[0].kind, RowKind::Toggle(true));
        assert_eq!(rows[1].key, "tz_set:UTC");
        assert_eq!(focus(&ui, &m), 0);
        assert!(rows.len() > 200 && rows.len() < 300, "{}", rows.len());
        assert!(rows[2..].windows(2).all(|w| w[0].label <= w[1].label));
        m.settings.time_zone = "America/New_York".into();
        m.settings.time_zone_auto = Some(false);
        let rows = super::rows(&ui, &m);
        let at = focus(&ui, &m);
        assert_eq!(rows[at].key, "tz_country:US");
        assert_eq!(rows[at].kind, RowKind::Check(true));
        assert!(rows[at].secondary.ends_with("time zones"));
        assert_eq!(summary(&m), "New York");
        m.navigation.filter = "country:US".into();
        let cities = super::rows(&ui, &m);
        assert_eq!(cities[focus(&ui, &m)].label, "New York");
        assert_eq!(title(&m), Some("United States"));
        assert!(cities.first().unwrap().secondary.contains("UTC-1"));
        assert!(cities.last().unwrap().secondary.contains("UTC-4"));
        m.settings.time_zone = "Europe/Berlin".into();
        m.settings.time_zone_auto = Some(true);
        assert_eq!(summary(&m), "Automatic · Berlin");
    }

    #[test]
    fn clock_reads_the_zone_and_hides_until_the_time_is_set() {
        // 2026-10-12 03:31 UTC.
        let mut ui = Ui {
            clock_fixed: Some(1_791_775_896),
            ..Default::default()
        };
        let mut m = AppModel::default();
        assert_eq!(ui.clock(&m).as_deref(), Some("03:31"));
        assert_eq!(ui.date(&m).as_deref(), Some("Mon 12 Oct"));
        m.settings.time_zone = "America/New_York".into();
        // Y2Linux #72: a 12-hour clock says AM or PM; its date reads month first.
        assert_eq!(ui.clock(&m).as_deref(), Some("11:31 PM"));
        assert_eq!(ui.date(&m).as_deref(), Some("Sun, Oct 11"));
        m.settings.time_zone = "Europe/Berlin".into();
        assert_eq!(ui.clock(&m).as_deref(), Some("05:31"));
        m.settings.time_zone = "Mars/Olympus".into();
        assert_eq!(ui.clock(&m).as_deref(), Some("03:31"));
        ui.clock_fixed = Some(86_400);
        assert_eq!(read(&ui, &m), None);
    }
}
