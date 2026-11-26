//! Walks a folder on all CPU cores and builds a tree of what takes the space.

use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rayon::prelude::*;

/// A file or folder, with the size of everything in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub name: String,
    /// Bytes, for a folder everything inside it.
    pub size: u64,
    /// Files inside (1 for a file).
    pub files: u64,
    pub is_dir: bool,
    /// The folder couldn't be read (no access), or only partly.
    pub unreadable: bool,
    pub children: Vec<Node>,
}

impl Node {
    pub fn file(name: String, size: u64) -> Self {
        Node { name, size, files: 1, is_dir: false, unreadable: false, children: Vec::new() }
    }

    pub fn dir(name: String, children: Vec<Node>) -> Self {
        let mut node = Node { name, size: 0, files: 0, is_dir: true, unreadable: false, children };
        node.add_up();
        node
    }

    /// Set the size and file count from the children.
    pub fn add_up(&mut self) {
        self.size = self.children.iter().map(|c| c.size).sum();
        self.files = self.children.iter().map(|c| c.files).sum();
    }

    /// Folders that couldn't be read, anywhere inside.
    pub fn unreadable_count(&self) -> usize {
        usize::from(self.unreadable) + self.children.iter().map(Node::unreadable_count).sum::<usize>()
    }
}

/// Shared with the screen while a scan runs.
#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    pub cancel: AtomicBool,
    /// A folder being read now, to show something moving.
    pub current: Mutex<String>,
}

/// Scan `path` (a folder). The root node is named after the whole path.
pub fn scan(path: &Path, progress: &Progress) -> Node {
    scan_dir(path, path.display().to_string(), progress)
}

fn scan_dir(path: &Path, name: String, progress: &Progress) -> Node {
    if progress.cancel.load(Ordering::Relaxed) {
        return Node::dir(name, Vec::new());
    }
    if let Ok(mut current) = progress.current.try_lock() {
        *current = path.display().to_string();
    }
    let Ok(entries) = fs::read_dir(path) else {
        let mut node = Node::dir(name, Vec::new());
        node.unreadable = true;
        return node;
    };
    let mut children = Vec::new();
    let mut dirs = Vec::new();
    let mut partly = false;
    for entry in entries {
        let Ok(entry) = entry else {
            partly = true;
            continue;
        };
        // On Windows this comes with the directory listing: no extra disk access.
        let Ok(meta) = entry.metadata() else {
            partly = true;
            continue;
        };
        // Symbolic links and junctions point to things counted where they really are.
        if meta.file_type().is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if meta.is_dir() {
            dirs.push((entry.path(), name));
        } else {
            let size = if online_only(&meta) { 0 } else { meta.len() };
            progress.files.fetch_add(1, Ordering::Relaxed);
            progress.bytes.fetch_add(size, Ordering::Relaxed);
            children.push(Node::file(name, size));
        }
    }
    let subdirs: Vec<Node> = dirs.into_par_iter().map(|(path, name)| scan_dir(&path, name, progress)).collect();
    children.extend(subdirs);
    sort_by_size(&mut children);
    let mut node = Node::dir(name, children);
    node.unreadable = partly;
    node
}

/// Files kept in the cloud (OneDrive "online only") take no room on the disk.
#[cfg(windows)]
fn online_only(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS};
    meta.file_attributes() & (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | FILE_ATTRIBUTE_OFFLINE) != 0
}

#[cfg(not(windows))]
fn online_only(_meta: &fs::Metadata) -> bool {
    false
}

/// Biggest first; same size by name.
pub fn sort_by_size(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

/// Folders first, then by name.
pub fn sort_by_name(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

/// The node at `path` (names from the root).
pub fn node_at<'a>(root: &'a Node, path: &[String]) -> Option<&'a Node> {
    let mut node = root;
    for name in path {
        node = node.children.iter().find(|c| &c.name == name)?;
    }
    Some(node)
}

pub fn node_at_mut<'a>(root: &'a mut Node, path: &[String]) -> Option<&'a mut Node> {
    let mut node = root;
    for name in path {
        node = node.children.iter_mut().find(|c| &c.name == name)?;
    }
    Some(node)
}

/// Take `name` out of the folder at `path`, and its size out of every folder above.
pub fn remove(root: &mut Node, path: &[String], name: &str) -> Option<Node> {
    let dir = node_at_mut(root, path)?;
    let index = dir.children.iter().position(|c| c.name == name)?;
    let removed = dir.children.remove(index);
    for_each_on_path(root, path, |node| {
        node.size -= removed.size;
        node.files -= removed.files;
    });
    Some(removed)
}

/// Put a new scan of the node at `path` in place of the old one, and fix
/// the sizes above it. With an empty path, the root is replaced.
pub fn replace(root: &mut Node, path: &[String], mut new: Node) {
    let Some((last, parent)) = path.split_last() else {
        new.name = root.name.clone();
        *root = new;
        return;
    };
    let Some(old) = node_at_mut(root, path) else { return };
    new.name = last.clone();
    let (old_size, old_files) = (old.size, old.files);
    let (new_size, new_files) = (new.size, new.files);
    *old = new;
    for_each_on_path(root, parent, |node| {
        node.size = node.size - old_size + new_size;
        node.files = node.files - old_files + new_files;
    });
}

/// Call `f` on the root and every folder down to `path`.
fn for_each_on_path(root: &mut Node, path: &[String], mut f: impl FnMut(&mut Node)) {
    let mut node = root;
    f(node);
    for name in path {
        let Some(next) = node.children.iter_mut().find(|c| &c.name == name) else { return };
        node = next;
        f(node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Node {
        Node::dir(
            "root".into(),
            vec![
                Node::dir("a".into(), vec![Node::file("x".into(), 100), Node::file("y".into(), 50)]),
                Node::file("z".into(), 10),
            ],
        )
    }

    #[test]
    fn scans_a_real_folder() {
        let dir = std::env::temp_dir().join(format!("diskman_scan_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("big/inner")).unwrap();
        fs::write(dir.join("big/inner/f1"), vec![0u8; 3000]).unwrap();
        fs::write(dir.join("big/f2"), vec![0u8; 1000]).unwrap();
        fs::write(dir.join("small"), vec![0u8; 10]).unwrap();
        let progress = Progress::default();
        let root = scan(&dir, &progress);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(root.size, 4010);
        assert_eq!(root.files, 3);
        assert_eq!(progress.files.load(Ordering::Relaxed), 3);
        // Biggest first.
        assert_eq!(root.children[0].name, "big");
        assert_eq!(root.children[0].size, 4000);
        assert_eq!(root.children[0].children[0].name, "inner");
    }

    #[test]
    fn removing_updates_every_folder_above() {
        let mut root = sample();
        let removed = remove(&mut root, &["a".into()], "x").unwrap();
        assert_eq!(removed.size, 100);
        assert_eq!(root.size, 60);
        assert_eq!(root.files, 2);
        assert_eq!(node_at(&root, &["a".into()]).unwrap().size, 50);
        assert!(remove(&mut root, &[], "nope").is_none());
    }

    #[test]
    fn replacing_a_rescanned_folder() {
        let mut root = sample();
        replace(&mut root, &["a".into()], Node::dir("whatever".into(), vec![Node::file("x".into(), 1000)]));
        assert_eq!(root.size, 1010);
        assert_eq!(root.files, 2);
        assert_eq!(node_at(&root, &["a".into()]).unwrap().name, "a");
        replace(&mut root, &[], Node::dir("new".into(), vec![]));
        assert_eq!((root.name.as_str(), root.size), ("root", 0));
    }
}
