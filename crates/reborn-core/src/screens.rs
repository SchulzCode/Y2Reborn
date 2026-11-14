//! Navigation identities shared by the model and the UI.
use serde::{Deserialize, Serialize};
/// Every user-facing route. The tree is documented in
/// `docs/ui/REBORN-PRODUCT-NAVIGATION-V2.md`; Diagnostics is the only
/// technical subtree and is reached through Settings → System.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    #[default]
    #[serde(alias = "main")]
    Home,
    Music,
    Artists,
    Albums,
    /// Genres from the files' tags; each opens its albums.
    Genres,
    Tracks,
    Folders,
    Album,
    Artist,
    NowPlaying,
    Lyrics,
    Queue,
    TrackInfo,
    Settings,
    Wifi,
    Bluetooth,
    PcTransfer,
    SettingsAudio,
    Equalizer,
    SettingsPlayback,
    /// Crossfade length picker.
    Crossfade,
    /// Left/right balance picker.
    Balance,
    /// Screen-off delay picker.
    ScreenTimeout,
    /// Where music plays, and the headphone jack's settings (Settings > Sound).
    Output,
    /// Wired headphone output level picker (Settings > Sound > Output).
    HeadphoneLevel,
    /// DAC interpolation filter picker (Settings > Sound).
    DacFilter,
    /// Skin picker: the built-in look and the installed skins.
    Theme,
    /// Time zone picker (Settings > Display).
    TimeZone,
    /// The date and time set by hand (Settings > Display, #53).
    DateTime,
    /// Which Music lists also stand on Home (Settings > Customize).
    HomeMenu,
    SettingsLibrary,
    SettingsDisplay,
    /// How Reborn behaves: wrap-around lists, Home Menu, Album View.
    Customize,
    /// The audiobooks: in progress first, then new, then finished.
    Audiobooks,
    /// One audiobook (`navigation.filter` is its key): buttons and chapters.
    Book,
    /// The library's films (video files), each opened on Now Playing.
    Videos,
    SettingsSystem,
    Battery,
    Sleep,
    Storage,
    Update,
    About,
    Maintenance,
    Diagnostics,
    /// One Diagnostics section; the navigation filter holds its id.
    DiagnosticSection,
    /// A full-length value; the navigation filter holds label and text.
    ValueDetail,
    TextEntry,
    Pairing,
    /// Internet radio: favorites, own stations, the directory, search.
    Radio,
    /// A list of stations (the list's title is in the navigation filter).
    RadioStations,
    /// Countries or genres of the station directory.
    RadioCategories,
    /// Library search results; the navigation filter holds the search text.
    Search,
    /// One station's details (logo, country, language, genres, format).
    RadioInfo,
    /// Every playlist file, Favorites first.
    Playlists,
    /// Settings > Last.fm: sign-in, scrobbling and the scrobbler log.
    LastFm,
    /// One playlist; the navigation filter holds its file's path.
    Playlist,
    /// About & Updates > Error Reports; filter "info": what is sent, "save":
    /// where Save Report writes the file.
    ErrorReports,
}
