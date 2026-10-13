//! Settings > Sound > Headphone correction: Off, or an AutoEq profile from
//! the SD card's `EQ` folder, played ahead of the listener's own equalizer.
use crate::{Item, RowKind};
use reborn_core::{hpeq, AppModel};

/// The Sound row's value: the profile in use, or Off.
pub(crate) fn summary(m: &AppModel) -> String {
    let name = &m.settings.headphone_correction;
    if hpeq::correction(name, &m.eq_profiles).is_some() {
        name.clone()
    } else {
        "Off".into()
    }
}

pub(crate) fn rows(m: &AppModel) -> Vec<Item> {
    let chosen = m.settings.headphone_correction.as_str();
    let mut rows = vec![Item::new("Off", "hp_eq:").with_kind(RowKind::Check(
        hpeq::correction(chosen, &m.eq_profiles).is_none(),
    ))];
    rows.extend(m.eq_profiles.iter().map(|p| {
        let mut item = Item::new(&p.name, format!("hp_eq:{}", p.name))
            .with_kind(RowKind::Check(p.name == chosen && p.correction.is_ok()));
        // A file that isn't a profile is listed, dimmed, but can't be chosen.
        item.enabled = p.correction.is_ok();
        if !item.enabled {
            item.secondary = "Not an AutoEq profile".into();
        }
        item
    }));
    rows
}

/// How to fill the picker, while the card has no profiles (an empty state,
/// not a permanent hint: UI v4).
pub(crate) fn status(m: &AppModel) -> Option<String> {
    m.eq_profiles.is_empty().then(|| {
        "Put AutoEq ParametricEQ.txt files in an EQ folder on the SD card, \
         named after your headphones."
            .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reborn_core::hpeq::Profile;

    #[test]
    fn picker_offers_off_and_each_readable_profile() {
        let mut m = AppModel::default();
        assert_eq!(rows(&m).len(), 1);
        assert!(status(&m).unwrap().contains("EQ folder on the SD card"));
        m.eq_profiles = vec![
            Profile::new(
                "Example Buds",
                "Preamp: -2.5 dB\nFilter 1: ON PK Fc 3000 Hz Gain 2.0 dB Q 1.40\n",
            ),
            Profile::new("Notes", "not a profile"),
        ];
        m.settings.headphone_correction = "Example Buds".into();
        let rows = rows(&m);
        let keys: Vec<_> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["hp_eq:", "hp_eq:Example Buds", "hp_eq:Notes"]);
        assert_eq!(rows[0].kind, RowKind::Check(false));
        assert_eq!(rows[1].kind, RowKind::Check(true));
        assert!(!rows[2].enabled);
        assert_eq!(rows[2].secondary, "Not an AutoEq profile");
        assert_eq!(status(&m), None);
        assert_eq!(summary(&m), "Example Buds");
        // The file was removed: correction is off.
        m.eq_profiles.clear();
        assert_eq!(super::rows(&m)[0].kind, RowKind::Check(true));
        assert_eq!(summary(&m), "Off");
    }
}
