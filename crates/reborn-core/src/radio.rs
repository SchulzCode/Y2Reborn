//! Radio as the screens and the player speak of it: a station (of the internet directory, an owner's
//! list, or the FM tuner) and what the Radio screens ask for.
use crate::*;

/// An internet radio station from the community directory or an owner's
/// own playlist.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RadioStation {
    /// radio-browser station UUID; empty for the owner's own playlists.
    #[serde(default)]
    pub uuid: String,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub codec: String,
    #[serde(default)]
    pub bitrate: u32,
    #[serde(default)]
    pub tags: String,
    /// The station's logo (an http(s) picture address from the directory).
    #[serde(default)]
    pub favicon: String,
    #[serde(default)]
    pub homepage: String,
    /// Languages as the directory names them ("german,english").
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub countrycode: String,
    /// Directory votes and plays (its popularity).
    #[serde(default)]
    pub votes: u32,
    #[serde(default)]
    pub clicks: u32,
}
impl RadioStation {
    /// An FM station of the player's own tuner (`fm:<kHz>`), not a stream.
    pub fn is_fm(&self) -> bool {
        self.url.starts_with("fm:")
    }
    /// The station's genres from its tags, capitalised, at most `n`.
    pub fn genres(&self, n: usize) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        for tag in self
            .tags
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            let name = capitalise(tag);
            if !out.iter().any(|o| o.eq_ignore_ascii_case(&name)) {
                out.push(name);
            }
            if out.len() == n {
                break;
            }
        }
        out
    }
    /// "AAC · 128 kbps", "MP3", or empty.
    pub fn format(&self) -> String {
        match (self.codec.is_empty(), self.bitrate) {
            (false, b) if b > 0 => format!("{} · {b} kbps", self.codec),
            (false, _) => self.codec.clone(),
            (true, b) if b > 0 => format!("{b} kbps"),
            _ => String::new(),
        }
    }
    /// Languages, capitalised ("German, English").
    pub fn languages(&self) -> String {
        self.language
            .split(',')
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(3)
            .map(capitalise)
            .collect::<Vec<_>>()
            .join(", ")
    }
    /// The logo's thumbnail identity (never equal to a cover's: top bit set),
    /// or `None` without a logo address.
    pub fn logo_key(&self) -> Option<u64> {
        (!self.favicon.is_empty()).then(|| logo_key(&self.favicon))
    }
    /// One line under the station name: "Germany · MP3 128 kbps · jazz".
    pub fn summary(&self) -> String {
        let mut parts = vec![];
        if !self.country.is_empty() {
            parts.push(self.country.clone());
        }
        match (self.codec.is_empty(), self.bitrate) {
            (false, b) if b > 0 => parts.push(format!("{} {b} kbps", self.codec)),
            (false, _) => parts.push(self.codec.clone()),
            (true, b) if b > 0 => parts.push(format!("{b} kbps")),
            _ => {}
        }
        if let Some(tag) = self.tags.split(',').map(str::trim).find(|t| !t.is_empty()) {
            parts.push(tag.to_owned());
        }
        parts.join(" · ")
    }
    /// Identity for favorites: the directory UUID, else the stream URL.
    pub fn key(&self) -> &str {
        if self.uuid.is_empty() {
            &self.url
        } else {
            &self.uuid
        }
    }
}

/// What the Radio screens ask the player for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RadioRequest {
    Favorites,
    Own,
    Popular,
    Countries,
    Genres,
    Country {
        code: String,
        name: String,
    },
    Genre(String),
    Search(String),
    /// Play the station at this index of the list on screen.
    Play(usize),
    ToggleFavorite(usize),
    /// Add or remove the station playing now.
    ToggleFavoriteCurrent,
    /// Play, or add or remove, the station Station Info shows.
    PlayStation(Box<RadioStation>),
    ToggleFavoriteStation(Box<RadioStation>),
    /// FM: the stations the tuner found, a new search, and stopping it.
    Fm,
    FmScan,
    FmCancel,
    /// Settings → Customize → Browsing → FM Radio: whether Radio shows the tuner.
    FmShow(bool),
}
