//! Folders' index (Beta 8.39): every folder's songs, its folders and how
//! many songs are in it and below it, made once per library. A folder then
//! opens, counts and plays without a pass over the whole library (a card of
//! 120 000 songs took a quarter of a second per folder on the player, and
//! seconds to count or play it whole).
use reborn_core::Track;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[derive(Default)]
struct Folder {
    songs: Vec<usize>,
    folders: Vec<PathBuf>,
    /// Songs in it and in the folders below it.
    total: usize,
    /// Songs and folders are in Folders' order (sorted when first asked).
    sorted: bool,
}

#[derive(Default)]
pub(crate) struct FolderIndex {
    stamp: Option<(usize, usize)>,
    folders: HashMap<PathBuf, Folder>,
}

impl FolderIndex {
    /// Made again only when the library changes.
    pub(crate) fn ensure(&mut self, tracks: &[Track]) {
        let stamp = crate::library_stamp(tracks);
        if self.stamp == Some(stamp) {
            return;
        }
        self.stamp = Some(stamp);
        self.folders.clear();
        for (i, t) in tracks.iter().enumerate().filter(|(_, t)| t.is_music()) {
            let Some(parent) = t.path.parent() else {
                continue;
            };
            if let Some(folder) = self.folders.get_mut(parent) {
                folder.songs.push(i);
                continue;
            }
            self.folders.insert(
                parent.to_path_buf(),
                Folder {
                    songs: vec![i],
                    ..Default::default()
                },
            );
            // A new folder joins the ones above it, as far as they are new.
            let mut dir = parent;
            while let Some(up) = dir.parent() {
                let known = self.folders.contains_key(up);
                self.folders
                    .entry(up.to_path_buf())
                    .or_default()
                    .folders
                    .push(dir.to_path_buf());
                if known {
                    break;
                }
                dir = up;
            }
        }
        // Totals from the deepest folders up.
        let mut paths: Vec<PathBuf> = self.folders.keys().cloned().collect();
        paths.sort_by_cached_key(|p| std::cmp::Reverse(p.components().count()));
        for path in paths {
            let folder = &self.folders[&path];
            let total = folder.songs.len()
                + folder
                    .folders
                    .iter()
                    .map(|c| self.folders.get(c).map_or(0, |f| f.total))
                    .sum::<usize>();
            if let Some(folder) = self.folders.get_mut(&path) {
                folder.total = total;
            }
        }
    }

    /// Songs in `path` and below it.
    pub(crate) fn total(&self, path: &Path) -> usize {
        self.folders.get(path).map_or(0, |f| f.total)
    }

    fn sort(&mut self, tracks: &[Track], path: &Path) {
        let Some(folder) = self.folders.get_mut(path) else {
            return;
        };
        if folder.sorted {
            return;
        }
        folder.sorted = true;
        let name = |p: &Path| {
            p.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        };
        folder
            .folders
            .sort_by_cached_key(|f| crate::catalog::name_key(&name(f)));
        folder
            .songs
            .sort_by_cached_key(|&i| crate::catalog::name_key(&tracks[i].filename));
    }

    /// A folder as Folders lists it: its folders (with their song counts),
    /// then its songs, both by name.
    pub(crate) fn listing(
        &mut self,
        tracks: &[Track],
        path: &Path,
    ) -> (Vec<(PathBuf, usize)>, Vec<usize>) {
        self.sort(tracks, path);
        let Some(folder) = self.folders.get(path) else {
            return (vec![], vec![]);
        };
        let folders = folder
            .folders
            .iter()
            .map(|f| (f.clone(), self.total(f)))
            .collect();
        (folders, folder.songs.clone())
    }

    /// Every song below `root` in Folders' order: in each folder its
    /// folders first, then its songs. Playing a folder plays this.
    pub(crate) fn play_order(&mut self, tracks: &[Track], root: &Path) -> Vec<usize> {
        let mut order = Vec::with_capacity(self.total(root));
        // (folder, its folders are done): the songs follow the folders.
        let mut stack = vec![(root.to_path_buf(), false)];
        while let Some((dir, done)) = stack.pop() {
            self.sort(tracks, &dir);
            let Some(folder) = self.folders.get(&dir) else {
                continue;
            };
            if done {
                order.extend_from_slice(&folder.songs);
                continue;
            }
            let below: Vec<PathBuf> = folder.folders.iter().rev().cloned().collect();
            stack.push((dir, true));
            stack.extend(below.into_iter().map(|f| (f, false)));
        }
        order
    }
}
