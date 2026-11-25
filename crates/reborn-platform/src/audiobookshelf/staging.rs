//! Keep incomplete downloads out of the media scanner and publish them once.
use super::{identifier, DownloadPlan};
use rustix::fs::{flock, renameat_with, FlockOperation, RenameFlags};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const RESERVE: u64 = 64 * 1024 * 1024;

/// An exclusive, resumable staging directory on the destination filesystem.
pub struct Stage {
    root: File,
    root_path: PathBuf,
    mount: u64,
    stage: File,
    content: File,
    book: File,
    destination: File,
    _lock: File,
    key: String,
    folder: String,
    plan: DownloadPlan,
}

impl Stage {
    /// `key` identifies the server and book and contains no credentials.
    pub fn begin(root: &Path, key: &str, title: &str, plan: DownloadPlan) -> Result<Self, String> {
        Self::begin_verified(root, None, key, title, plan)
    }

    pub fn begin_verified(
        root: &Path,
        expected_mount: Option<u64>,
        key: &str,
        title: &str,
        plan: DownloadPlan,
    ) -> Result<Self, String> {
        identifier(key).map_err(str::to_owned)?;
        validate_plan(&plan)?;
        let root_dir = open_dir(root)?;
        let mount = crate::storage::descriptor_mount(&root_dir)
            .map_err(|_| "Couldn't identify audiobook storage")?;
        if expected_mount.is_some_and(|expected| expected != mount) {
            return Err("Audiobook storage changed".into());
        }
        let audiobooks = child_dir(&root_dir, "Audiobooks")?;
        let destination = child_dir(&audiobooks, "Audiobookshelf")?;
        match fs::symlink_metadata(child_path(&destination, key)) {
            Ok(_) => return Err("This book is already downloaded".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Couldn't check downloaded books".into()),
        }
        let staging_root = child_dir(&root_dir, ".RebornDownloads")?;
        let (stage, created) = stage_dir(&staging_root, key)?;
        let folder = book_folder(title);
        if !created {
            check_manifest(&stage, &folder, &plan, false)?;
        }
        let lock = open_file(&stage, "lock", true, false)?;
        lock_stage(&lock)?;
        check_manifest(&stage, &folder, &plan, created)?;
        let content = child_dir(&stage, "content")?;
        let book = child_dir(&content, &folder)?;
        clear_parts(&book, &plan)?;
        let free = crate::free_bytes(&fd_path(&root_dir))
            .map_err(|_| "Couldn't check storage space".to_owned())?;
        if free < plan.bytes.saturating_add(RESERVE) {
            return Err("Not enough storage for this book".into());
        }
        Ok(Self {
            root: root_dir,
            root_path: root.to_path_buf(),
            mount,
            stage,
            content,
            book,
            destination,
            _lock: lock,
            key: key.into(),
            folder,
            plan,
        })
    }

    /// Start the part again; unlinking a prior partial never follows a link.
    pub fn start_part(&self, index: u32) -> Result<File, String> {
        let part = self.part(index)?;
        let temporary = format!("{}.part", part.name);
        for name in [&part.name, &temporary] {
            match fs::remove_file(child_path(&self.book, name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("Couldn't restart the incomplete download".into()),
            }
        }
        open_file(&self.book, &temporary, true, true)
    }

    /// Finish a part only after it has the expected length on disk.
    pub fn finish_part(&self, index: u32) -> Result<(), String> {
        let part = self.part(index)?;
        let temporary = format!("{}.part", part.name);
        let file = open_file(&self.book, &temporary, false, false)?;
        if !file
            .metadata()
            .map_err(|_| "Couldn't check the download")?
            .is_file()
            || file
                .metadata()
                .map_err(|_| "Couldn't check the download")?
                .len()
                != part.bytes
        {
            return Err("Downloaded file is incomplete".into());
        }
        file.sync_all().map_err(|_| "Couldn't save the download")?;
        renameat_with(
            &self.book,
            temporary.as_str(),
            &self.book,
            part.name.as_str(),
            RenameFlags::NOREPLACE,
        )
        .map_err(|_| "Couldn't finish the download".to_owned())?;
        Ok(())
    }

    /// A missing or unsupported cover does not make a book unplayable.
    pub fn save_cover(&self, bytes: &[u8]) -> Result<(), String> {
        let Some(extension) = cover_extension(bytes) else {
            return Ok(());
        };
        for name in ["cover.jpg", "cover.png", "cover.webp"] {
            match fs::remove_file(child_path(&self.book, name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("Couldn't save the cover".into()),
            }
        }
        let mut file = open_file(&self.book, &format!("cover.{extension}"), true, true)?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "Couldn't save the cover".into())
    }

    /// Atomically expose the verified directory without replacing another book.
    pub fn publish(&self) -> Result<(), String> {
        self.verify_root()?;
        verify_content(&self.book, &self.plan, true)?;
        verify_container(&self.content, &self.book, &self.folder)?;
        self.book
            .sync_all()
            .map_err(|_| "Couldn't save the download")?;
        self.content
            .sync_all()
            .map_err(|_| "Couldn't save the download")?;
        self.verify_root()?;
        renameat_with(
            &self.stage,
            "content",
            &self.destination,
            self.key.as_str(),
            RenameFlags::NOREPLACE,
        )
        .map_err(|error| {
            if error == rustix::io::Errno::EXIST {
                "This book is already downloaded".to_owned()
            } else {
                "Couldn't publish the downloaded book".to_owned()
            }
        })?;
        self.destination
            .sync_all()
            .map_err(|_| "Couldn't save the downloaded book")?;
        Ok(())
    }

    fn part(&self, index: u32) -> Result<&super::DownloadPart, String> {
        self.plan
            .parts
            .iter()
            .find(|part| part.index == index)
            .ok_or_else(|| "Unknown audiobook part".into())
    }

    fn verify_root(&self) -> Result<(), String> {
        let current = open_dir(&self.root_path).map_err(|_| "Audiobook storage changed")?;
        let old = self
            .root
            .metadata()
            .map_err(|_| "Audiobook storage changed")?;
        let now = current
            .metadata()
            .map_err(|_| "Audiobook storage changed")?;
        if old.dev() != now.dev()
            || old.ino() != now.ino()
            || crate::storage::descriptor_mount(&current).ok() != Some(self.mount)
        {
            return Err("Audiobook storage changed".into());
        }
        Ok(())
    }
}

fn verify_content(book: &File, plan: &DownloadPlan, sync: bool) -> Result<(), String> {
    let mut remaining: HashSet<_> = plan.parts.iter().map(|part| part.name.as_str()).collect();
    let entries = fs::read_dir(fd_path(book)).map_err(|_| "Couldn't check the download")?;
    let mut cover_seen = false;
    for entry in entries {
        let entry = entry.map_err(|_| "Couldn't check the download")?;
        let name = entry.file_name();
        let name = name.to_str().ok_or("Unexpected file in download")?;
        if matches!(name, "cover.jpg" | "cover.png" | "cover.webp") {
            if cover_seen || !valid_cover(book, name)? {
                return Err("Invalid cover in download".into());
            }
            cover_seen = true;
            continue;
        }
        if !remaining.remove(name) {
            return Err("Unexpected or incomplete file in download".into());
        }
        let part = plan
            .parts
            .iter()
            .find(|part| part.name == name)
            .ok_or("Unexpected file in download")?;
        let file = open_file(book, name, false, false)?;
        let metadata = file.metadata().map_err(|_| "Couldn't check the download")?;
        if !metadata.is_file() || metadata.len() != part.bytes {
            return Err("Downloaded file is incomplete".into());
        }
        if sync {
            file.sync_all().map_err(|_| "Couldn't save the download")?;
        }
    }
    if !remaining.is_empty() {
        return Err("Download is incomplete".into());
    }
    Ok(())
}

/// A published directory is only marked downloaded while its recorded parts
/// are still present and have their expected lengths.
pub fn downloaded(root: &Path, key: &str) -> Result<bool, String> {
    identifier(key).map_err(str::to_owned)?;
    let root = open_dir(root)?;
    let Some(staging_root) = optional_dir(&root, ".RebornDownloads")? else {
        return Ok(false);
    };
    let Some(stage) = optional_dir(&staging_root, key)? else {
        return Ok(false);
    };
    let saved = match read_manifest(&stage) {
        Some(saved) => saved,
        None => return Ok(false),
    };
    if saved.folder != book_folder(&saved.folder) || validate_plan(&saved.plan).is_err() {
        return Ok(false);
    }
    let Some(audiobooks) = optional_dir(&root, "Audiobooks")? else {
        return Ok(false);
    };
    let Some(shelf) = optional_dir(&audiobooks, "Audiobookshelf")? else {
        return Ok(false);
    };
    let Some(content) = optional_dir(&shelf, key)? else {
        return Ok(false);
    };
    let Some(book) = optional_dir(&content, &saved.folder)? else {
        return Ok(false);
    };
    Ok(verify_container(&content, &book, &saved.folder).is_ok()
        && verify_content(&book, &saved.plan, false).is_ok())
}

fn verify_container(content: &File, book: &File, folder: &str) -> Result<(), String> {
    let mut entries = fs::read_dir(fd_path(content)).map_err(|_| "Couldn't check the download")?;
    let entry = entries
        .next()
        .ok_or("Download is incomplete")?
        .map_err(|_| "Couldn't check the download")?;
    if entry.file_name() != std::ffi::OsStr::new(folder) || entries.next().is_some() {
        return Err("Unexpected file in download".into());
    }
    let actual = fs::symlink_metadata(entry.path()).map_err(|_| "Couldn't check the download")?;
    let pinned = book.metadata().map_err(|_| "Couldn't check the download")?;
    if !actual.is_dir() || actual.ino() != pinned.ino() || actual.dev() != pinned.dev() {
        return Err("Download folder changed".into());
    }
    Ok(())
}

fn cover_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() > 4 * 1024 * 1024 {
        return None;
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

fn valid_cover(book: &File, name: &str) -> Result<bool, String> {
    let file = open_file(book, name, false, false)?;
    let metadata = file.metadata().map_err(|_| "Couldn't check the cover")?;
    if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
        return Ok(false);
    }
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Couldn't read the cover")?;
    Ok(cover_extension(&bytes).is_some_and(|ext| name == format!("cover.{ext}")))
}

fn fd_path(dir: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()))
}

fn lock_stage(file: &File) -> Result<(), String> {
    for attempt in 0..10 {
        match flock(file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(()),
            Err(rustix::io::Errno::AGAIN) if attempt < 9 => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(rustix::io::Errno::AGAIN) => return Err("This book is already downloading".into()),
            Err(_) => return Err("Couldn't lock the download".into()),
        }
    }
    Err("This book is already downloading".into())
}

fn child_path(dir: &File, name: &str) -> PathBuf {
    fd_path(dir).join(name)
}

fn open_dir(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "Couldn't open audiobook storage".into())
}

fn child_dir(parent: &File, name: &str) -> Result<File, String> {
    let path = child_path(parent, name);
    match fs::create_dir(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err("Couldn't create audiobook storage".into()),
    }
    let child = open_dir(&path)?;
    same_mount(parent, &child)?;
    Ok(child)
}

fn optional_dir(parent: &File, name: &str) -> Result<Option<File>, String> {
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(child_path(parent, name))
    {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("Couldn't check audiobook storage".into()),
    }
}

fn stage_dir(parent: &File, name: &str) -> Result<(File, bool), String> {
    let path = child_path(parent, name);
    let created = match fs::create_dir(&path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(_) => return Err("Couldn't create audiobook storage".into()),
    };
    let child = open_dir(&path)?;
    same_mount(parent, &child)?;
    Ok((child, created))
}

fn clear_parts(book: &File, plan: &DownloadPlan) -> Result<(), String> {
    for part in &plan.parts {
        let temporary = format!("{}.part", part.name);
        for name in [&part.name, &temporary] {
            match fs::remove_file(child_path(book, name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("Couldn't restart the incomplete download".into()),
            }
        }
    }
    Ok(())
}

fn open_file(dir: &File, name: &str, create: bool, exclusive: bool) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .create_new(exclusive)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(child_path(dir, name))
        .map_err(|_| "Couldn't open the download file".to_owned())?;
    if !file
        .metadata()
        .map_err(|_| "Couldn't check the download file")?
        .is_file()
    {
        return Err("Invalid download file".into());
    }
    same_mount(dir, &file)?;
    Ok(file)
}

fn same_mount(parent: &File, child: &File) -> Result<(), String> {
    let parent =
        crate::storage::descriptor_mount(parent).map_err(|_| "Couldn't identify storage")?;
    let child = crate::storage::descriptor_mount(child).map_err(|_| "Couldn't identify storage")?;
    if parent != child {
        return Err("Download crosses a storage mount".into());
    }
    Ok(())
}

fn validate_plan(plan: &DownloadPlan) -> Result<(), String> {
    let mut names = HashSet::new();
    let mut indexes = HashSet::new();
    let mut bytes = 0u64;
    for part in &plan.parts {
        if part.bytes == 0
            || part.name.is_empty()
            || part.name.len() > 120
            || part
                .name
                .chars()
                .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
            || part.name.starts_with('.')
            || part.name.ends_with([' ', '.'])
            || part.name.to_ascii_lowercase().ends_with(".part")
            || !names.insert(&part.name)
            || !indexes.insert(part.index)
        {
            return Err("Invalid audiobook download plan".into());
        }
        bytes = bytes.checked_add(part.bytes).ok_or("Book is too large")?;
    }
    if bytes == 0 || bytes != plan.bytes {
        return Err("Invalid audiobook download plan".into());
    }
    Ok(())
}

/// A portable folder name that cannot contain a path or reserved FAT name.
pub(super) fn book_folder(title: &str) -> String {
    let mut name = String::new();
    for c in title.chars() {
        let c = if c.is_control() || "\\/:*?\"<>|".contains(c) {
            ' '
        } else {
            c
        };
        if name.len() + c.len_utf8() > 100 {
            break;
        }
        name.push(c);
    }
    let name = name.trim_matches(|c| c == ' ' || c == '.');
    if name.is_empty() {
        return "Audiobook".into();
    }
    let upper = name.to_ascii_uppercase();
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if reserved.contains(&upper.split('.').next().unwrap_or("")) {
        format!("_{name}")
    } else {
        name.into()
    }
}

#[derive(serde::Serialize)]
struct ManifestRef<'a> {
    folder: &'a str,
    plan: &'a DownloadPlan,
}

#[derive(serde::Deserialize)]
struct Manifest {
    folder: String,
    plan: DownloadPlan,
}

fn check_manifest(
    stage: &File,
    folder: &str,
    plan: &DownloadPlan,
    created: bool,
) -> Result<(), String> {
    if created {
        let mut file = open_file(stage, "manifest.json", true, true)?;
        serde_json::to_writer(&mut file, &ManifestRef { folder, plan })
            .map_err(|_| "Couldn't save download state")?;
        file.flush()
            .and_then(|_| file.sync_all())
            .map_err(|_| "Couldn't save download state")?;
        stage
            .sync_all()
            .map_err(|_| "Couldn't save download state")?;
    } else {
        if !read_manifest(stage).is_some_and(|saved| saved.folder == folder && saved.plan == *plan)
        {
            return Err("An incomplete download has different file details".into());
        }
    }
    Ok(())
}

fn read_manifest(stage: &File) -> Option<Manifest> {
    let mut bytes = Vec::new();
    open_file(stage, "manifest.json", false, false)
        .ok()?
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 256 * 1024 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audiobookshelf::DownloadPart;

    fn plan() -> DownloadPlan {
        DownloadPlan {
            bytes: 7,
            parts: vec![
                DownloadPart {
                    index: 1,
                    inode: "1".into(),
                    bytes: 3,
                    name: "0001.mp3".into(),
                },
                DownloadPart {
                    index: 2,
                    inode: "2".into(),
                    bytes: 4,
                    name: "0002.mp3".into(),
                },
            ],
        }
    }

    #[test]
    fn folder_names_are_portable_and_keep_unicode() {
        assert_eq!(book_folder("../A Book"), "A Book");
        assert_eq!(book_folder("CON.txt"), "_CON.txt");
        assert_eq!(book_folder("星の本"), "星の本");
        assert_eq!(book_folder("..."), "Audiobook");
    }

    #[test]
    fn interrupted_parts_restart_and_only_complete_books_are_published() {
        let root = std::env::temp_dir().join(format!("reborn-stage-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let stage = Stage::begin(&root, "server-book", "A Book", plan()).unwrap();
        stage
            .save_cover(include_bytes!(
                "../../../../assets/fixtures/artwork-external.png"
            ))
            .unwrap();
        assert!(stage.publish().is_err());
        stage.start_part(1).unwrap().write_all(b"partial").unwrap();
        assert!(stage.finish_part(1).is_err());
        drop(stage);
        let stage = Stage::begin(&root, "server-book", "A Book", plan()).unwrap();
        stage.start_part(1).unwrap().write_all(b"one").unwrap();
        stage.finish_part(1).unwrap();
        assert!(stage.publish().is_err());
        stage.start_part(2).unwrap().write_all(b"two!").unwrap();
        stage.finish_part(2).unwrap();
        stage.publish().unwrap();
        assert_eq!(
            fs::read(root.join("Audiobooks/Audiobookshelf/server-book/A Book/0001.mp3")).unwrap(),
            b"one"
        );
        assert!(root
            .join("Audiobooks/Audiobookshelf/server-book/A Book/cover.png")
            .is_file());
        assert!(downloaded(&root, "server-book").unwrap());
        fs::remove_file(root.join("Audiobooks/Audiobookshelf/server-book/A Book/0001.mp3"))
            .unwrap();
        assert!(!downloaded(&root, "server-book").unwrap());
        drop(stage);
        assert!(Stage::begin(&root, "server-book", "A Book", plan()).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_existing_book_is_never_replaced() {
        let root =
            std::env::temp_dir().join(format!("reborn-stage-conflict-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let stage = Stage::begin(&root, "server-book", "A Book", plan()).unwrap();
        stage.start_part(1).unwrap().write_all(b"one").unwrap();
        stage.finish_part(1).unwrap();
        stage.start_part(2).unwrap().write_all(b"two!").unwrap();
        stage.finish_part(2).unwrap();
        let destination = root.join("Audiobooks/Audiobookshelf/server-book");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("existing"), b"keep").unwrap();
        assert_eq!(
            stage.publish(),
            Err("This book is already downloaded".into())
        );
        assert_eq!(fs::read(destination.join("existing")).unwrap(), b"keep");
        drop(stage);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_preexisting_unowned_stage_is_not_modified() {
        let root =
            std::env::temp_dir().join(format!("reborn-stage-unowned-{}", std::process::id()));
        let existing = root.join(".RebornDownloads/server-book/content/A Book");
        fs::create_dir_all(&existing).unwrap();
        fs::write(existing.join("0001.mp3"), b"keep").unwrap();
        assert!(Stage::begin(&root, "server-book", "A Book", plan()).is_err());
        assert_eq!(fs::read(existing.join("0001.mp3")).unwrap(), b"keep");
        assert!(!root.join(".RebornDownloads/server-book/lock").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replaced_storage_is_never_published() {
        let parent =
            std::env::temp_dir().join(format!("reborn-stage-replace-{}", std::process::id()));
        let root = parent.join("media");
        let old = parent.join("old-media");
        fs::create_dir_all(&root).unwrap();
        assert!(
            Stage::begin_verified(&root, Some(u64::MAX), "server-book", "A Book", plan()).is_err()
        );
        let stage = Stage::begin(&root, "server-book", "A Book", plan()).unwrap();
        stage.start_part(1).unwrap().write_all(b"one").unwrap();
        stage.finish_part(1).unwrap();
        stage.start_part(2).unwrap().write_all(b"two!").unwrap();
        stage.finish_part(2).unwrap();
        fs::rename(&root, &old).unwrap();
        fs::create_dir(&root).unwrap();
        assert_eq!(stage.publish(), Err("Audiobook storage changed".into()));
        assert!(!old.join("Audiobooks/Audiobookshelf/server-book").exists());
        assert!(!root.join("Audiobooks/Audiobookshelf/server-book").exists());
        drop(stage);
        fs::remove_dir_all(parent).unwrap();
    }
}
