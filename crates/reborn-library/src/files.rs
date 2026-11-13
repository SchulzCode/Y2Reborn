//! Which files the library reads.
use std::path::Path;

/// Audio and video files the library reads. macOS writes an AppleDouble companion
/// `._name.mp3` next to every file it copies to a FAT card; those hold
/// Finder metadata, not audio.
pub fn supported(path: &Path) -> bool {
    if path
        .file_name()
        .is_some_and(|n| n.as_encoded_bytes().starts_with(b"._"))
    {
        return false;
    }
    path.extension().is_some_and(|e| {
        matches!(
            e.to_string_lossy().to_ascii_lowercase().as_str(),
            "flac"
                | "mp3"
                | "aac"
                | "m4a"
                | "m4b"
                | "ogg"
                | "oga"
                | "opus"
                | "wav"
                | "aiff"
                | "aif"
                | "aifc"
                | "ape"
                | "wv"
                // Films: their sound plays like a song's, their pictures on Now Playing.
                | "mp4"
                | "m4v"
                | "mov"
                | "mkv"
                | "webm"
                | "avi"
        )
    })
}
