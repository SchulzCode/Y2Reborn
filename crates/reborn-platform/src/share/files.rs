//! What Wi-Fi Share may touch: paths inside a storage's music folder, and nothing reached through a
//! link or a name that starts with a dot.
use serde::Serialize;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Serialize, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub folder: bool,
    pub size: u64,
    /// Seconds since 1970.
    pub time: i64,
}

fn refused(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, what.to_owned())
}

/// A file or folder name a listener may give: one path component, nothing hidden.
pub fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0'])
        && name.trim() == name
}

/// `relative` ("/Albums/One") inside `root`, or a refusal: no `..`, no hidden names, and no link
/// anywhere on the way (the part that exists is walked; a link could lead out of the storage).
pub fn resolve(root: &Path, relative: &str) -> io::Result<PathBuf> {
    let mut path = root.to_path_buf();
    for part in relative.split('/').filter(|part| !part.is_empty()) {
        if !name_ok(part) {
            return Err(refused("name"));
        }
        path.push(part);
        match path.symlink_metadata() {
            Ok(info) if info.file_type().is_symlink() => return Err(refused("link")),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(path)
}

/// A folder's visible entries, folders first, then by name without regard to case.
pub fn list(folder: &Path) -> io::Result<Vec<Entry>> {
    let mut entries = vec![];
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(info) = entry.path().symlink_metadata() else {
            continue;
        };
        if !name_ok(&name) || !(info.is_dir() || info.is_file()) {
            continue;
        }
        entries.push(Entry {
            name,
            folder: info.is_dir(),
            size: if info.is_dir() { 0 } else { info.len() },
            time: info.mtime(),
        });
    }
    entries.sort_by_key(|e| (!e.folder, e.name.to_lowercase()));
    Ok(entries)
}

/// Free and total bytes of the file system holding `path` (asked of `df`: no call of our own into
/// the C library for one number).
pub fn space(path: &Path) -> (u64, u64) {
    let output = std::process::Command::new("df")
        .arg("-k")
        .arg(path)
        .output();
    let text = output.map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let row = text.as_ref().ok().and_then(|t| t.lines().last());
    // "Filesystem 1K-blocks Used Available Use% Mounted on": from the right, so a long device name
    // on a line of its own does not shift the columns.
    let numbers: Vec<u64> = row
        .map(|r| {
            r.split_whitespace()
                .rev()
                .filter_map(|f| f.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    match numbers[..] {
        [available, _used, total, ..] => (available * 1024, total * 1024),
        _ => (0, 0),
    }
}

/// Delete a file, or a folder with everything in it. Links are removed, never followed.
pub fn delete(path: &Path) -> io::Result<()> {
    let info = path.symlink_metadata()?;
    if info.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_storage() {
        let root = std::env::temp_dir().join(format!("share-paths-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Albums/One")).unwrap();
        fs::write(root.join("Albums/One/a.flac"), b"abc").unwrap();
        fs::write(root.join(".hidden"), b"x").unwrap();
        std::os::unix::fs::symlink("/etc", root.join("Albums/out")).unwrap();
        assert_eq!(
            resolve(&root, "/Albums/One").unwrap(),
            root.join("Albums/One")
        );
        assert_eq!(resolve(&root, "").unwrap(), root);
        // A new name is fine; what must not work does not.
        assert!(resolve(&root, "/Albums/New Folder/b.mp3").is_ok());
        for bad in [
            "/../etc",
            "/Albums/../../etc",
            "/.hidden",
            "/Albums/out/passwd",
            "/a\\b",
        ] {
            assert!(resolve(&root, bad).is_err(), "{bad}");
        }
        let names: Vec<String> = list(&root).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["Albums"]);
        let inside = list(&root.join("Albums")).unwrap();
        // The link is not listed; the folder is, with no size of its own.
        assert_eq!(inside.len(), 1);
        assert!(inside[0].folder && inside[0].size == 0);
        assert!(space(&root).1 > 0);
        delete(&root.join("Albums")).unwrap();
        assert!(fs::metadata("/etc").is_ok() && !root.join("Albums").exists());
        fs::remove_dir_all(&root).unwrap();
    }
}
