//! Playlist files: finding them, reading any text a computer writes, writing
//! them back safely. Matching entries to library songs is the UI's job
//! (`reborn_ui::playlist_index`); nothing here needs the library.

use reborn_core::playlist::{Playlist, PlaylistEntry, TextEncoding, MAX_ENTRIES};
use std::path::{Path, PathBuf};

/// A playlist file larger than this is not read.
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Lines longer than this make a playlist read-only (it is never rewritten
/// without them).
const MAX_LINE_CHARS: usize = 4096;
/// At most this many playlist files are listed.
pub const MAX_PLAYLISTS: usize = 1000;
/// Device-made playlists live here on Internal Storage.
pub const FOLDER: &str = "Playlists";
pub const FAVORITES: &str = "Favorites";

pub fn is_playlist(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("m3u") || e.eq_ignore_ascii_case("m3u8"))
}

/// Whether `discover` lists this playlist file under `root`: not in a hidden
/// or system folder, not the Radio folder's station lists, not a "._" file.
pub fn listed(root: &Path, path: &Path) -> bool {
    let Ok(inside) = path.strip_prefix(root) else {
        return false;
    };
    let parts: Vec<String> = inside
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let Some((file, folders)) = parts.split_last() else {
        return false;
    };
    !file.starts_with("._")
        && is_playlist(path)
        && folders
            .iter()
            .enumerate()
            .all(|(depth, name)| !skipped(depth, name))
}

fn skipped(depth: usize, name: &str) -> bool {
    name.starts_with('.')
        || (depth == 0 && name.eq_ignore_ascii_case("Radio"))
        || [
            "System Volume Information",
            "$RECYCLE.BIN",
            "LOST.DIR",
            "lost+found",
        ]
        .iter()
        .any(|s| name.eq_ignore_ascii_case(s))
}

/// Every playlist file under the storage roots, except the Radio folder's
/// station lists, hidden folders and the system folders computers leave.
pub fn discover(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = vec![];
    for root in roots {
        let mut stack = vec![(root.clone(), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if kind.is_dir() {
                    if !skipped(depth, &name) && depth < 32 {
                        stack.push((entry.path(), depth + 1));
                    }
                } else if kind.is_file() && !name.starts_with("._") && is_playlist(&entry.path()) {
                    found.push(entry.path());
                    if found.len() >= MAX_PLAYLISTS {
                        return found;
                    }
                }
            }
        }
    }
    found
}

fn stamp(meta: &std::fs::Metadata) -> (u64, i64) {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos().min(i64::MAX as u128) as i64);
    (meta.len(), mtime)
}

/// The file's size and modification time now.
pub fn current_stamp(path: &Path) -> Option<(u64, i64)> {
    std::fs::metadata(path).ok().map(|m| stamp(&m))
}

pub fn read(path: &Path) -> Result<Playlist, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_BYTES {
        return Err("This playlist is larger than 8 MB.".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let (text, encoding) = decode(&bytes);
    let mut playlist = parse(&text);
    playlist.path = path.to_owned();
    playlist.name = name_of(path);
    playlist.encoding = encoding;
    playlist.stamp = stamp(&meta);
    Ok(playlist)
}

pub fn name_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- text

/// Bytes to text: a byte order mark decides; else valid UTF-8 is UTF-8 and
/// anything else is Windows-1252 (what older Windows tools write).
pub fn decode(bytes: &[u8]) -> (String, TextEncoding) {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return (
            String::from_utf8_lossy(rest).into_owned(),
            TextEncoding::Utf8Bom,
        );
    }
    let utf16 = |rest: &[u8], little: bool| -> String {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| {
                if little {
                    u16::from_le_bytes(c)
                } else {
                    u16::from_be_bytes(c)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return (utf16(rest, true), TextEncoding::Utf16Le);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return (utf16(rest, false), TextEncoding::Utf16Be);
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_owned(), TextEncoding::Utf8),
        Err(_) => (
            bytes.iter().map(|&b| windows_1252(b)).collect(),
            TextEncoding::Windows1252,
        ),
    }
}

const CP1252_HIGH: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
];

fn windows_1252(byte: u8) -> char {
    match byte {
        0x80..=0x9F => CP1252_HIGH[(byte - 0x80) as usize],
        _ => byte as char,
    }
}

/// Text back to bytes in the playlist's encoding. A character Windows-1252
/// cannot hold (a new song with a Japanese title) switches the file to UTF-8
/// with a byte order mark, which every player reads.
pub fn encode(text: &str, encoding: TextEncoding) -> (Vec<u8>, TextEncoding) {
    match encoding {
        TextEncoding::Utf8 => (text.as_bytes().to_vec(), encoding),
        TextEncoding::Utf8Bom => {
            let mut out = vec![0xEF, 0xBB, 0xBF];
            out.extend_from_slice(text.as_bytes());
            (out, encoding)
        }
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            let little = encoding == TextEncoding::Utf16Le;
            let mut out = if little {
                vec![0xFF, 0xFE]
            } else {
                vec![0xFE, 0xFF]
            };
            for unit in text.encode_utf16() {
                out.extend(if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            (out, encoding)
        }
        TextEncoding::Windows1252 => {
            let mut out = Vec::with_capacity(text.len());
            for c in text.chars() {
                let byte = match c as u32 {
                    0..=0x7F | 0xA0..=0xFF => Some(c as u8),
                    _ => CP1252_HIGH
                        .iter()
                        .position(|&h| h == c)
                        .map(|i| 0x80 + i as u8),
                };
                match byte {
                    Some(b) => out.push(b),
                    None => return encode(text, TextEncoding::Utf8Bom),
                }
            }
            (out, encoding)
        }
    }
}

// ---------------------------------------------------------------- m3u

/// M3U text to entries. `#EXTINF` belongs to the next location; other `#`
/// lines are kept with the entry after them; blank lines go.
pub fn parse(text: &str) -> Playlist {
    let mut playlist = Playlist::default();
    let mut info = None;
    let mut extra = vec![];
    for line in text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)) {
        let line = line.trim_start_matches('\u{FEFF}');
        if line.chars().count() > MAX_LINE_CHARS || playlist.entries.len() >= MAX_ENTRIES {
            playlist.read_only = true;
            if playlist.entries.len() >= MAX_ENTRIES {
                break;
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("#EXTM3U") {
            continue;
        }
        // `get`, not `[..8]`: byte 8 can sit inside a character (`It’s…`).
        if trimmed
            .get(..8)
            .is_some_and(|p| p.eq_ignore_ascii_case("#EXTINF:"))
        {
            info = Some(trimmed.to_owned());
        } else if trimmed.starts_with('#') {
            extra.push(trimmed.to_owned());
        } else {
            playlist.entries.push(PlaylistEntry {
                location: trimmed.to_owned(),
                info: info.take(),
                extra: std::mem::take(&mut extra),
            });
        }
    }
    playlist.trailer = extra;
    if let Some(info) = info {
        playlist.trailer.push(info);
    }
    playlist
}

pub fn serialize(playlist: &Playlist) -> String {
    let mut out = String::from("#EXTM3U\n");
    for entry in &playlist.entries {
        for line in &entry.extra {
            out.push_str(line);
            out.push('\n');
        }
        if let Some(info) = &entry.info {
            out.push_str(info);
            out.push('\n');
        }
        out.push_str(&entry.location);
        out.push('\n');
    }
    for line in &playlist.trailer {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Write a playlist so a power cut leaves either the old or the new file:
/// a hidden temporary next to it, flushed, then renamed over it. Returns the
/// new stamp and the encoding written.
pub fn write(playlist: &Playlist) -> Result<((u64, i64), TextEncoding), String> {
    if playlist.read_only {
        return Err("This playlist is too large to change on the player.".into());
    }
    let (bytes, encoding) = encode(&serialize(playlist), playlist.encoding);
    reborn_core::atomic_write(&playlist.path, &bytes)
        .and_then(|()| std::fs::metadata(&playlist.path))
        .map(|meta| (stamp(&meta), encoding))
        .map_err(|e| format!("Couldn't save the playlist: {e}"))
}

/// Characters a file name on the SD card (exFAT) or a computer cannot hold.
pub fn file_name_for(name: &str) -> Option<String> {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || r#"\/:*?"<>|"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    let cleaned: String = cleaned.chars().take(80).collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// A new, empty playlist file `<dir>/<name>.m3u8`; an existing name is an
/// error, never overwritten.
pub fn create(dir: &Path, name: &str) -> Result<Playlist, String> {
    let file = file_name_for(name).ok_or("Type a name for the playlist")?;
    let path = dir.join(format!("{file}.m3u8"));
    if path.exists() || dir.join(format!("{file}.m3u")).exists() {
        return Err(format!("A playlist named “{file}” exists already"));
    }
    let mut playlist = Playlist {
        path,
        name: file,
        ..Default::default()
    };
    let (stamp, encoding) = write(&playlist)?;
    playlist.stamp = stamp;
    playlist.encoding = encoding;
    Ok(playlist)
}

/// Rename a playlist file in its folder (keeping its extension).
pub fn rename(path: &Path, name: &str) -> Result<PathBuf, String> {
    let file = file_name_for(name).ok_or("Type a name for the playlist")?;
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "m3u8".into());
    let target = path.with_file_name(format!("{file}.{extension}"));
    if target == path {
        return Ok(target);
    }
    // A case-only rename on exFAT finds the file itself: that is fine.
    let same_file = target
        .file_name()
        .zip(path.file_name())
        .is_some_and(|(a, b)| {
            a.to_string_lossy()
                .eq_ignore_ascii_case(&b.to_string_lossy())
        });
    // Either extension counts: two lists may not show the same name.
    let taken = ["m3u8", "m3u"]
        .iter()
        .map(|e| path.with_file_name(format!("{file}.{e}")))
        .any(|p| p.exists() && !(same_file && p == target));
    if taken {
        return Err(format!("A playlist named “{file}” exists already"));
    }
    std::fs::rename(path, &target).map_err(|e| format!("Couldn't rename: {e}"))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reborn_core::playlist::PlaylistEdit;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("reborn-pl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn every_encoding_a_computer_writes_reads_and_writes_back_the_same() {
        let text = "#EXTM3U\n#EXTINF:215,Björk - Jóga\nMusic/Björk/Jóga.flac\n";
        for encoding in [
            TextEncoding::Utf8,
            TextEncoding::Utf8Bom,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
            TextEncoding::Windows1252,
        ] {
            let (bytes, written) = encode(text, encoding);
            assert_eq!(written, encoding);
            let (back, found) = decode(&bytes);
            assert_eq!(back, text, "{encoding:?}");
            assert_eq!(found, encoding);
        }
        // Windows-1252's own characters (the euro sign, curly quotes).
        let (back, _) = decode(&[0x80, b' ', 0x93, b'x', 0x94]);
        assert_eq!(back, "€ “x”");
        // A character it cannot hold switches the file to UTF-8 with a BOM.
        let (bytes, encoding) = encode("日本", TextEncoding::Windows1252);
        assert_eq!(encoding, TextEncoding::Utf8Bom);
        assert_eq!(decode(&bytes).0, "日本");
    }

    #[test]
    fn parsing_keeps_unknown_lines_and_writes_them_back() {
        let text = "#EXTM3U\r\n#PLAYLIST:Road\r\n\r\n#EXTINF:200,A - One\r\nC:\\Music\\A\\One.mp3\r\n#EXTGRP:x\r\nhttp://radio/stream\r\nrelative/Two.flac\r\n# trailing note\r\n";
        let playlist = parse(text);
        assert_eq!(playlist.entries.len(), 3);
        assert_eq!(playlist.entries[0].extra, ["#PLAYLIST:Road"]);
        assert_eq!(
            playlist.entries[0].info.as_deref(),
            Some("#EXTINF:200,A - One")
        );
        assert_eq!(playlist.entries[0].location, "C:\\Music\\A\\One.mp3");
        assert_eq!(playlist.entries[1].extra, ["#EXTGRP:x"]);
        assert_eq!(playlist.trailer, ["# trailing note"]);
        let again = parse(&serialize(&playlist));
        assert_eq!(again.entries, playlist.entries);
        assert_eq!(again.trailer, playlist.trailer);
        assert!(!playlist.read_only);
    }

    #[test]
    fn a_character_across_byte_eight_is_a_plain_line() {
        // The reported crashes: `’` at bytes 6..9 and `º` at bytes 7..9.
        let text = "#EXTM3U\nDon’t Stop.mp3\nNúmero 1º.flac\n#EXTINF:1,Ação\nÉté.mp3\n";
        let playlist = parse(text);
        let locations: Vec<_> = playlist.entries.iter().map(|e| &e.location).collect();
        assert_eq!(locations, ["Don’t Stop.mp3", "Número 1º.flac", "Été.mp3"]);
        assert_eq!(playlist.entries[2].info.as_deref(), Some("#EXTINF:1,Ação"));
        // Every split point of a multi-byte line, in every encoding a computer writes.
        for prefix in 0..10 {
            let line = format!("{}’º€.mp3", "a".repeat(prefix));
            for encoding in [TextEncoding::Utf8, TextEncoding::Windows1252] {
                let (bytes, _) = encode(&line, encoding);
                let (decoded, _) = decode(&bytes);
                assert_eq!(parse(&decoded).entries[0].location, line);
            }
        }
    }

    #[test]
    fn a_too_long_line_makes_the_playlist_read_only() {
        let text = format!("a.mp3\n{}\nb.mp3\n", "x".repeat(5000));
        let mut playlist = parse(&text);
        assert!(playlist.read_only);
        assert!(playlist.apply(&PlaylistEdit::Append(vec![])).is_err());
        assert!(write(&playlist).is_err());
    }

    #[test]
    fn edits_land_on_the_named_entry_or_fail() {
        let mut playlist = parse("a.mp3\nb.mp3\nc.mp3\nb.mp3\n");
        playlist
            .apply(&PlaylistEdit::Remove {
                index: 3,
                location: "b.mp3".into(),
            })
            .unwrap();
        let names: Vec<_> = playlist
            .entries
            .iter()
            .map(|e| e.location.as_str())
            .collect();
        assert_eq!(
            names,
            ["a.mp3", "b.mp3", "c.mp3"],
            "only that occurrence goes"
        );
        playlist
            .apply(&PlaylistEdit::Move {
                from: 2,
                to: 0,
                location: "c.mp3".into(),
            })
            .unwrap();
        let names: Vec<_> = playlist
            .entries
            .iter()
            .map(|e| e.location.as_str())
            .collect();
        assert_eq!(names, ["c.mp3", "a.mp3", "b.mp3"]);
        // The file changed on the computer: the entry is not where expected.
        assert!(playlist
            .apply(&PlaylistEdit::Remove {
                index: 0,
                location: "a.mp3".into()
            })
            .is_err());
    }

    #[test]
    fn writing_is_atomic_creating_never_overwrites_and_rename_keeps_the_extension() {
        let dir = temp_dir("write");
        let mut playlist = create(&dir, "Road Trip").unwrap();
        assert_eq!(playlist.path, dir.join("Road Trip.m3u8"));
        assert!(create(&dir, "Road Trip").is_err());
        assert!(create(&dir, "   ").is_err(), "a blank name is refused");
        playlist
            .apply(&PlaylistEdit::Append(vec![PlaylistEntry {
                location: "../Music/One.flac".into(),
                info: Some("#EXTINF:100,A - One".into()),
                extra: vec![],
            }]))
            .unwrap();
        let (stamp, _) = write(&playlist).unwrap();
        assert_eq!(current_stamp(&playlist.path), Some(stamp));
        let back = read(&playlist.path).unwrap();
        assert_eq!(back.entries, playlist.entries);
        assert!(std::fs::read_dir(&dir).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("reborn-new")));
        let renamed = rename(&playlist.path, "Drive: Home").unwrap();
        assert_eq!(renamed, dir.join("Drive_ Home.m3u8"));
        std::fs::write(dir.join("old.m3u"), "x.mp3\n").unwrap();
        assert_eq!(
            rename(&dir.join("old.m3u"), "New").unwrap(),
            dir.join("New.m3u")
        );
        assert!(
            rename(&dir.join("New.m3u"), "Drive: Home").is_err(),
            "taken"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn discovery_skips_radio_lists_hidden_and_system_folders() {
        let dir = temp_dir("discover");
        for (path, text) in [
            ("Playlists/Mix.m3u8", "a\n"),
            ("Music/Album/album.M3U", "a\n"),
            ("Radio/stations.m3u", "http://x\n"),
            (".hidden/x.m3u", "a\n"),
            ("Music/._Mix.m3u8", "mac junk"),
            ("System Volume Information/y.m3u", "a\n"),
            ("Music/notes.txt", "a\n"),
        ] {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
        let mut found: Vec<_> = discover(std::slice::from_ref(&dir))
            .into_iter()
            .map(|p| p.strip_prefix(&dir).unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();
        assert_eq!(found, ["Music/Album/album.M3U", "Playlists/Mix.m3u8"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
