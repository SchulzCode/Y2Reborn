//! Radio's pages: the choice of FM and internet radio, the internet stations' menu, station lists,
//! countries and genres, Station Info, and FM's search.
use super::*;

/// Internet radio: the station on air, then the owner's lists, then the
/// community directory. Without Wi-Fi a first row leads to Wi-Fi; favorites
/// and own lists stay usable.
pub(super) fn radio_menu(ui: &Ui, internet: bool) -> Page {
    let r = &ui.radio;
    let favorites = match r.favorites.len() {
        0 => "No stations yet".to_owned(),
        n => crate::components::count(n, "station", "stations"),
    };
    let mut rows = vec![];
    if let Some(now) = &r.now {
        rows.push(row(&now.name, "radio_now", "On Air"));
    }
    // FM needs no internet: the player's own tuner, wired headphones as aerial.
    let fm = match r.fm {
        crate::FmView::Hidden => None,
        crate::FmView::NeedsFirmware => Some("Needs the tuner firmware".to_owned()),
        crate::FmView::NotScanned => Some("Search for stations".to_owned()),
        crate::FmView::Scanning { progress } => Some(format!("Searching… {progress}%")),
        crate::FmView::Ready { stations: 0 } => Some("No stations found".to_owned()),
        crate::FmView::Ready { stations } => {
            Some(crate::components::count(stations, "station", "stations"))
        }
    };
    // A player with FM chooses between its two kinds of radio first; without FM, Radio is the
    // internet stations' menu itself.
    if let Some(detail) = fm.filter(|_| !internet) {
        rows.push(row("FM Radio", "radio_fm", detail));
        rows.push(row(
            "Internet Radio",
            "radio_internet",
            "Stations from around the world",
        ));
        return Page::rows(rows);
    }
    if !r.online {
        rows.push(row(
            "Connect to Wi-Fi",
            "radio_wifi",
            "Radio needs the internet",
        ));
    }
    rows.extend([
        row("Favorites", "radio_favorites", favorites),
        row("Popular", "radio_popular", "Most listened worldwide"),
        row(
            "Countries",
            "radio_countries",
            "Stations from every country",
        ),
        row("Genres", "radio_genres", "Jazz, rock, news and more"),
        row("Search", "radio_search", "Find a station by name"),
        row("My Stations", "radio_own", "Your .m3u and .pls lists"),
    ]);
    Page::rows(rows)
}

/// One station's details; its actions are the rows.
pub(super) fn radio_info(ui: &Ui) -> Page {
    let Some(station) = &ui.radio.info else {
        return Page::hero("No Station", "Choose a station first.").with_rows(radio_back());
    };
    let playing = ui
        .radio
        .now
        .as_ref()
        .is_some_and(|now| now.key() == station.key());
    // Skins show the station as a page: its name, the directory's facts and
    // the two actions (Reborn's own Station Info draws these itself).
    let mut state = vec![];
    if playing {
        state.push("On Air");
    }
    if ui.radio.is_favorite(station) {
        state.push("Favorite");
    }
    let facts = crate::views_radio::station_facts(station)
        .into_iter()
        .map(|(label, value)| (label.to_owned(), value))
        .collect();
    Page {
        hero: Some(Hero {
            title: station.name.clone(),
            body: state.join(" · "),
        }),
        facts,
        ..Page::rows(vec![
            row(
                if playing { "Now Playing" } else { "Play" },
                if playing {
                    "radio_now"
                } else {
                    "radio_info_play"
                },
                "",
            ),
            row(
                if ui.radio.is_favorite(station) {
                    "Remove from Favorites"
                } else {
                    "Add to Favorites"
                },
                "radio_info_favorite",
                "",
            ),
        ])
    }
}

fn radio_back() -> Vec<Item> {
    vec![row("Back to Radio", "radio_back", "")]
}

/// FM: the tuner's stations by frequency, the search and its pages.
fn fm_stations(ui: &Ui) -> Page {
    let r = &ui.radio;
    match r.fm {
        crate::FmView::NeedsFirmware => {
            return Page::hero(
                "FM Needs the Tuner Firmware",
                "Copy system.img from the Innioasis Y2 firmware package into a folder named \
                 Firmware on Internal Storage or the SD card. The player takes the tuner's \
                 two small files from it.",
            )
            .with_rows(radio_back())
        }
        crate::FmView::Scanning { progress } => {
            return Page::hero(
                format!("Searching for Stations… {progress}%"),
                "Wired headphones are the antenna: keep them plugged in.",
            )
            .with_rows(vec![row("Stop", "radio_fm_cancel", "")])
        }
        _ => {}
    }
    if r.stations.is_empty() {
        let searched = r.fm == crate::FmView::Ready { stations: 0 };
        return Page::hero(
            if searched {
                "No FM Stations Found"
            } else {
                "No FM Stations Yet"
            },
            if searched {
                "Plug in wired headphones – their cable is the antenna – stretch the cable \
                 out, then search again."
            } else {
                "Plug in wired headphones – their cable is the antenna – then search."
            },
        )
        .with_rows(if searched {
            // A player that cannot receive (the first Y2 revision) gets FM out of the way here.
            vec![
                row("Search Again", "radio_fm_scan", ""),
                row("Hide FM Radio", "radio_fm_hide", ""),
            ]
        } else {
            vec![row("Search for Stations", "radio_fm_scan", "")]
        });
    }
    let mut rows: Vec<Item> = r
        .stations
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let detail = if r.is_favorite(s) { "Favorite" } else { "" };
            row(&s.name, &format!("station:{i}"), detail)
        })
        .collect();
    rows.push(row("Search Again", "radio_fm_scan", "Find stations here"));
    Page::rows(rows)
}

pub(super) fn radio_stations(ui: &Ui) -> Page {
    let r = &ui.radio;
    if r.last_request == Some(reborn_core::RadioRequest::Fm) {
        return fm_stations(ui);
    }
    if r.loading {
        return Page::hero("Loading Stations…", "Asking the station directory.")
            .with_rows(radio_back());
    }
    if let Some(error) = &r.error {
        return Page::hero("Couldn't Load Stations", error).with_rows(vec![row(
            "Try Again",
            "radio_retry",
            "",
        )]);
    }
    if r.stations.is_empty() {
        return match r.last_request {
            Some(reborn_core::RadioRequest::Favorites) => Page::hero(
                "No Favorites Yet",
                "Hold Select on a station, or on Now Playing, and choose Add to Favorites.",
            ),
            Some(reborn_core::RadioRequest::Own) => Page::hero(
                "No Stations of Your Own",
                "Copy .m3u or .pls station lists into a folder named Radio on Internal \
                 Storage or the SD card.",
            ),
            _ => Page::hero("No Stations Found", "Try another name, country or genre."),
        }
        .with_rows(radio_back());
    }
    Page::rows(
        r.stations
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut detail = crate::views_radio::station_detail(s);
                if r.is_favorite(s) {
                    detail = if detail.is_empty() {
                        "Favorite".into()
                    } else {
                        format!("Favorite · {detail}")
                    };
                }
                row(&s.name, &format!("station:{i}"), detail)
            })
            .collect(),
    )
}

pub(super) fn radio_categories(ui: &Ui) -> Page {
    let r = &ui.radio;
    if r.loading {
        return Page::hero("Loading…", "Asking the station directory.").with_rows(radio_back());
    }
    if r.categories.is_empty() && r.error.is_none() {
        return Page::hero("Nothing Here", "The directory sent an empty list.")
            .with_rows(radio_back());
    }
    if let Some(error) = &r.error {
        return Page::hero("Couldn't Load the List", error).with_rows(vec![row(
            "Try Again",
            "radio_retry",
            "",
        )]);
    }
    Page::rows(
        r.categories
            .iter()
            .enumerate()
            .map(|(i, c)| {
                row(
                    &c.name,
                    &format!("category:{i}"),
                    format!("{} stations", c.stations),
                )
            })
            .collect(),
    )
}
