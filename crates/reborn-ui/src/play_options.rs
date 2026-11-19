//! Every way Reborn plays a group of songs, in one place (Y2Reborn #163).
//!
//! - **Header rows** at the top of a list: Play All and Shuffle All (an album, Songs and the
//!   lists made from it, a genre's songs), Play All alone on an artist's page (holding Select on
//!   it offers the rest), Shuffle All on a folder and on a genre's albums.
//! - **The group menu** (hold Select): Play, Shuffle, Shuffle Albums (where there are several
//!   albums), Play Next, Add to Queue, Add to Playlist…. The same menu for a group's row
//!   (`group_*`), an album's or artist's own page (`collection_*`) and a playlist (`pl_*`).
//! - **What each choice starts** (`Ui::play_choice`). Play follows the Shuffle setting
//!   (Now Playing's menu: Off, Songs, Albums; one album always plays in order, #113),
//!   Shuffle and Shuffle Albums mix whatever the setting says.
//!
//! Music's own lists beside the library's (Recently Added, Recently Played, Most Played) are
//! Songs with a filter, so they get the same rows.
use crate::*;
use reborn_core::{QueueOrigin, ShuffleMode};

/// The header rows' keys.
pub(crate) const PLAY_ALL: &str = "play:collection";
pub(crate) const SHUFFLE_ALL: &str = "shuffle:collection";

/// Music's lists made from the play history and the files' dates: (Songs filter, title).
pub(crate) const RECENTLY_ADDED: &str = "recent:added";
pub(crate) const RECENTLY_PLAYED: &str = "recent:played";
pub(crate) const MOST_PLAYED: &str = "most:played";
pub(crate) const LISTS: [(&str, &str); 3] = [
    (RECENTLY_ADDED, "Recently Added"),
    (RECENTLY_PLAYED, "Recently Played"),
    (MOST_PLAYED, "Most Played"),
];

/// The title of one of Music's lists, by its filter.
pub(crate) fn list_title(filter: &str) -> Option<&'static str> {
    LISTS.iter().find(|(f, _)| *f == filter).map(|(_, t)| *t)
}

/// The songs of one of Music's lists, in its order (`None`: not such a list).
pub(crate) fn list_songs(m: &AppModel, tracks: &[Track], filter: &str) -> Option<Vec<usize>> {
    Some(match filter {
        RECENTLY_ADDED => reborn_core::play_stats::recently_added(tracks),
        RECENTLY_PLAYED => m.play_stats.recently_played(tracks),
        MOST_PLAYED => m.play_stats.most_played(tracks),
        _ => return None,
    })
}

/// What a group of songs can be played as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Play,
    Shuffle,
    ShuffleAlbums,
    PlayNext,
    AddToQueue,
    AddToPlaylist,
}
/// Every choice in menu order: (choice, label, key suffix).
const CHOICES: [(Choice, &str, &str); 6] = [
    (Choice::Play, "Play", "play"),
    (Choice::Shuffle, "Shuffle", "shuffle"),
    (Choice::ShuffleAlbums, "Shuffle Albums", "shuffle_albums"),
    (Choice::PlayNext, "Play Next", "next"),
    (Choice::AddToQueue, "Add to Queue", "queue"),
    (Choice::AddToPlaylist, "Add to Playlist…", "playlist"),
];
impl Choice {
    /// How a choice that plays now orders the songs; `None` for one that queues them.
    fn shuffle(self) -> Option<ShuffleMode> {
        match self {
            Self::Play => Some(ShuffleMode::Off),
            Self::Shuffle => Some(ShuffleMode::Songs),
            Self::ShuffleAlbums => Some(ShuffleMode::Albums),
            Self::PlayNext | Self::AddToQueue | Self::AddToPlaylist => None,
        }
    }
}

/// Whose menu it is (its keys' prefix).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Family {
    /// A row standing for a group: an album, artist, genre or folder, the search's Play All.
    Group,
    /// An album's or artist's own page (its Play All row).
    Page,
    /// A playlist (its row, or its page's Play and Shuffle).
    Playlist,
}
impl Family {
    fn prefix(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Page => "collection",
            Self::Playlist => "pl",
        }
    }
}

/// The group menu: Shuffle Albums only where there are several albums to mix, Add to Playlist
/// not for a playlist (its own menu goes on with Sort, Rename…).
pub(crate) fn menu(family: Family, albums: bool) -> Vec<Item> {
    CHOICES
        .iter()
        .filter(|(choice, _, _)| match choice {
            Choice::ShuffleAlbums => albums,
            Choice::AddToPlaylist => family != Family::Playlist,
            _ => true,
        })
        .map(|(_, label, suffix)| Item::new(*label, format!("{}_{suffix}", family.prefix())))
        .collect()
}

/// The choice a menu row's key stands for.
pub(crate) fn choice(family: Family, key: &str) -> Option<Choice> {
    let suffix = key.strip_prefix(family.prefix())?.strip_prefix('_')?;
    CHOICES
        .iter()
        .find(|(_, _, s)| *s == suffix)
        .map(|(choice, _, _)| *choice)
}

/// The header rows a list page starts with (callers add them only when it has songs).
pub(crate) fn header_rows(screen: Screen) -> Vec<Item> {
    let rows: &[(&str, &str)] = match screen {
        Screen::Album | Screen::Tracks => &[("Play All", PLAY_ALL), ("Shuffle All", SHUFFLE_ALL)],
        Screen::Artist => &[("Play All", PLAY_ALL)],
        Screen::Folders | Screen::Albums => &[("Shuffle All", SHUFFLE_ALL)],
        _ => &[],
    };
    rows.iter()
        .map(|(label, key)| Item::new(*label, *key))
        .collect()
}

/// A header row's choice.
pub(crate) fn header_choice(key: &str) -> Option<Choice> {
    match key {
        PLAY_ALL => Some(Choice::Play),
        SHUFFLE_ALL => Some(Choice::Shuffle),
        _ => None,
    }
}

impl Ui {
    /// Starts `choice` for `members` (library indices, in the group's order): playing goes to Now
    /// Playing, queueing stays where it was, Add to Playlist opens the picker.
    pub(crate) fn play_choice(
        &mut self,
        m: &mut AppModel,
        choice: Choice,
        members: Vec<usize>,
        source: String,
        origin: Option<QueueOrigin>,
    ) -> Effect {
        let Some(&selected) = members.first() else {
            return Effect::None;
        };
        if let Some(shuffle) = choice.shuffle() {
            Self::go(m, Screen::NowPlaying, "");
            return Effect::PlayCollection {
                selected,
                members,
                shuffle,
                source,
                origin,
            };
        }
        Self::close_modal(m);
        match choice {
            Choice::PlayNext => Effect::PlayNextCollection(members),
            Choice::AddToQueue => Effect::AddToQueueCollection(members),
            _ => {
                self.open_picker(m, members);
                Effect::None
            }
        }
    }

    /// `choice` for the page on screen: its songs in its own order (a folder with its folders).
    pub(crate) fn play_page(
        &mut self,
        m: &mut AppModel,
        tracks: &[Track],
        choice: Choice,
    ) -> Effect {
        let members = match m.navigation.filter.clone() {
            folder if m.screen == Screen::Folders && folder.starts_with("folder:") => {
                self.group_members(m, tracks, &folder)
            }
            _ => self.filtered_indices(m, tracks),
        };
        let (source, origin) = (source_label(m), page_origin(m));
        self.play_choice(m, choice, members, source, origin)
    }
}

#[cfg(test)]
#[path = "play_options_tests.rs"]
mod tests;
