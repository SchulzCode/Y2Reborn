//! Playlists are .m3u/.m3u8 files on Internal Storage or the SD card; the file
//! is the playlist (no copy in a database). These are its parts as read, so
//! an edit on the player writes the file back with everything it held:
//! entries the player cannot find, comments, other tools' tags, its text
//! encoding.

use std::path::PathBuf;

/// The text encoding a playlist was read in, and is written back in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextEncoding {
    #[default]
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// Older Windows exports (.m3u without a byte order mark).
    Windows1252,
}

/// One entry: where the song is (as written in the file), its `#EXTINF`
/// line if any, and other lines that came before it (kept as they are).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaylistEntry {
    pub location: String,
    pub info: Option<String>,
    pub extra: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Playlist {
    pub path: PathBuf,
    /// The file name without its extension.
    pub name: String,
    pub entries: Vec<PlaylistEntry>,
    /// Lines after the last entry.
    pub trailer: Vec<String>,
    pub encoding: TextEncoding,
    /// Size and modification time when read: an edit checks that the file
    /// was not changed since (by a computer over USB).
    pub stamp: (u64, i64),
    /// Too large or with lines too long to rewrite safely: shown and played,
    /// never written.
    pub read_only: bool,
}

/// A change made on the player. Entries are named by position and location,
/// so an edit never lands on a different song if the file moved under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaylistEdit {
    Append(Vec<PlaylistEntry>),
    Remove {
        index: usize,
        location: String,
    },
    Move {
        from: usize,
        to: usize,
        location: String,
    },
    /// Every entry whose location is one of these (Favorites' heart).
    RemoveLocations(Vec<String>),
    /// Every entry: the playlist stays, empty (Y2Linux #103).
    Clear,
}

impl Playlist {
    /// Apply an edit; an entry that is not where the edit expects it is an
    /// error ("changed on the computer").
    pub fn apply(&mut self, edit: &PlaylistEdit) -> Result<(), String> {
        if self.read_only {
            return Err("This playlist is too large to change on the player.".into());
        }
        let check = |entries: &[PlaylistEntry], index: usize, location: &str| {
            entries
                .get(index)
                .is_some_and(|e| e.location == location)
                .then_some(())
                .ok_or_else(|| "The playlist changed. Open it again and retry.".to_owned())
        };
        match edit {
            PlaylistEdit::Append(entries) => {
                if self.entries.len() + entries.len() > MAX_ENTRIES {
                    return Err(format!("Playlists hold at most {MAX_ENTRIES} songs."));
                }
                self.entries.extend(entries.iter().cloned());
            }
            PlaylistEdit::Remove { index, location } => {
                check(&self.entries, *index, location)?;
                self.entries.remove(*index);
            }
            PlaylistEdit::Move { from, to, location } => {
                check(&self.entries, *from, location)?;
                let entry = self.entries.remove(*from);
                self.entries.insert((*to).min(self.entries.len()), entry);
            }
            PlaylistEdit::RemoveLocations(locations) => {
                self.entries.retain(|e| !locations.contains(&e.location));
            }
            PlaylistEdit::Clear => self.entries.clear(),
        }
        Ok(())
    }
}

/// A playlist holds at most this many entries.
pub const MAX_ENTRIES: usize = 20_000;
