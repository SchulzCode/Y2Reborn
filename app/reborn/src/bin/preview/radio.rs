//! Radio and Wi-Fi Share fixtures of the preview renderer: the internet stations of its lists, the
//! FM pages and the share page.
use super::Setup;
use reborn_core::{AppModel, Screen};

/// Three stations of the directory, as the lists show them.
pub fn stations() -> Vec<reborn_core::RadioStation> {
    vec![
        reborn_core::RadioStation {
            uuid: "a".into(),
            name: "Radio Swiss Jazz".into(),
            url: "http://a/".into(),
            country: "Switzerland".into(),
            codec: "MP3".into(),
            bitrate: 128,
            tags: "jazz,smooth jazz,swiss".into(),
            favicon: "http://a/logo.png".into(),
            homepage: "https://www.radioswissjazz.ch".into(),
            language: "german,english".into(),
            countrycode: "CH".into(),
            votes: 4210,
            clicks: 2380,
        },
        reborn_core::RadioStation {
            uuid: "b".into(),
            name: "FIP".into(),
            url: "http://b/".into(),
            country: "France".into(),
            codec: "AAC".into(),
            bitrate: 192,
            tags: "eclectic,jazz,world music".into(),
            favicon: "http://b/logo.png".into(),
            homepage: "https://www.radiofrance.fr/fip".into(),
            language: "french".into(),
            countrycode: "FR".into(),
            votes: 9120,
            clicks: 5021,
        },
        reborn_core::RadioStation {
            uuid: "c".into(),
            name: "SomaFM Groove Salad".into(),
            url: "http://c/".into(),
            country: "USA".into(),
            codec: "MP3".into(),
            bitrate: 128,
            tags: "ambient,chillout".into(),
            favicon: String::new(),
            homepage: "https://somafm.com".into(),
            language: "english".into(),
            countrycode: "US".into(),
            votes: 15400,
            clicks: 3900,
        },
        reborn_core::RadioStation {
            uuid: "d".into(),
            name: "BBC World Service".into(),
            url: "http://d/".into(),
            country: "UK".into(),
            codec: "MP3".into(),
            bitrate: 56,
            tags: "news".into(),
            favicon: String::new(),
            homepage: "https://www.bbc.co.uk/worldservice".into(),
            language: "english".into(),
            countrycode: "GB".into(),
            votes: 7300,
            clicks: 8800,
        },
    ]
}

fn case(
    name: &'static str,
    model: AppModel,
    setup: impl Fn(&mut reborn_ui::Ui, &mut AppModel) + 'static,
) -> (&'static str, AppModel, Setup) {
    (name, model, Box::new(setup))
}

/// Radio's two kinds, the internet stations' menu below them, FM's pages, and the Wi-Fi Share
/// pages after them.
pub fn fm_cases(at: fn(Screen, &str) -> AppModel) -> Vec<(&'static str, AppModel, Setup)> {
    let mut cases = fm_pages(at);
    cases.extend(share_cases(at));
    cases
}

fn fm_pages(at: fn(Screen, &str) -> AppModel) -> Vec<(&'static str, AppModel, Setup)> {
    use reborn_core::RadioRequest::Fm;
    use reborn_ui::FmView;
    let fm = "FM Radio";
    vec![
        case("67-radio-with-fm", at(Screen::Radio, ""), |ui, _| {
            ui.radio.online = true;
            ui.radio.fm = FmView::Ready { stations: 4 };
        }),
        case(
            "67b-radio-internet",
            at(Screen::Radio, reborn_ui::RADIO_INTERNET),
            |ui, _| {
                ui.radio.online = true;
                ui.radio.fm = FmView::Ready { stations: 4 };
                ui.radio.favorites = vec!["a".into(), "b".into()];
            },
        ),
        case("68-fm-stations", at(Screen::RadioStations, fm), |ui, m| {
            ui.radio.fm = FmView::Ready { stations: 4 };
            ui.radio.last_request = Some(Fm);
            let found = [88_300, 94_300, 100_600, 104_000];
            let stations = found.map(reborn_platform::fm::station).to_vec();
            ui.radio.set_stations_in_order(stations);
            m.navigation.focus = 1;
        }),
        case(
            "68b-fm-searching",
            at(Screen::RadioStations, fm),
            |ui, _| {
                ui.radio.fm = FmView::Scanning { progress: 40 };
                ui.radio.last_request = Some(Fm);
            },
        ),
        case(
            "68c-fm-nothing-found",
            at(Screen::RadioStations, fm),
            |ui, _| {
                ui.radio.fm = FmView::Ready { stations: 0 };
                ui.radio.last_request = Some(Fm);
            },
        ),
    ]
}

/// Settings → Library with its Wi-Fi Share row, and the share page running and without Wi-Fi.
fn share_cases(at: fn(Screen, &str) -> AppModel) -> Vec<(&'static str, AppModel, Setup)> {
    let page = || at(Screen::SettingsLibrary, reborn_ui::SHARE);
    vec![
        case("69-library", at(Screen::SettingsLibrary, ""), |_, m| {
            m.navigation.focus = 2;
        }),
        case("69b-wifi-share", page(), |ui, _| {
            ui.share.address = "http://192.168.1.23".into();
            ui.share.code = "482913".into();
            ui.share.received = 12;
        }),
        case("69c-wifi-share-no-wifi", page(), |ui, _| {
            ui.wifi.status = reborn_ui::WifiStatus::Disconnected;
        }),
    ]
}
