//! Windows Update files that are safe to delete: downloaded updates, the
//! Delivery Optimization cache, upgrade leftovers, and the previous Windows.
//!
//! Never WinSxS (only DISM may clean it), Installer (needed to repair and
//! uninstall programs) or SoftwareDistribution\DataStore (the update history).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};

use ratatui::widgets::TableState;

use crate::scan::{self, Progress};
use crate::size;

pub struct Target {
    pub label: &'static str,
    pub path: PathBuf,
    /// Empty the folder but keep it: Windows expects it there.
    pub contents_only: bool,
    /// Ticked when the screen opens. Windows.old isn't: it's the way back.
    pub safe: bool,
}

/// The places on the Windows drive, those that are there.
pub fn targets() -> Vec<Target> {
    let windows = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let drive = std::env::var("SystemDrive").map_or_else(|_| PathBuf::from(r"C:\"), |d| PathBuf::from(format!("{d}\\")));
    let all = [
        Target {
            label: "Downloaded updates",
            path: windows.join(r"SoftwareDistribution\Download"),
            contents_only: true,
            safe: true,
        },
        Target {
            label: "Delivery Optimization cache",
            path: windows.join(r"ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache"),
            contents_only: true,
            safe: true,
        },
        Target { label: "Upgrade leftovers", path: drive.join("$Windows.~BT"), contents_only: false, safe: true },
        Target { label: "Upgrade leftovers", path: drive.join("$Windows.~WS"), contents_only: false, safe: true },
        Target {
            label: "Previous Windows (no going back after)",
            path: drive.join("Windows.old"),
            contents_only: false,
            safe: false,
        },
    ];
    // Not readable without administrator rights still means it's there.
    all.into_iter().filter(|t| !matches!(t.path.try_exists(), Ok(false))).collect()
}

pub struct Item {
    pub target: Target,
    pub ticked: bool,
    /// None until measured.
    pub size: Option<u64>,
    pub files: u64,
    /// Some of it couldn't be read, so the size may be short.
    pub unreadable: bool,
}

enum Msg {
    Measured { index: usize, size: u64, files: u64, unreadable: bool },
    Cleaned { errors: Errors },
    Done,
}

#[derive(Default)]
struct Errors {
    count: u64,
    first: Option<String>,
}

impl Errors {
    fn add(&mut self, path: &Path, e: io::Error) {
        self.count += 1;
        if self.first.is_none() {
            self.first = Some(format!("{}: {e}", path.display()));
        }
    }
}

enum Job {
    Measuring,
    Cleaning { freed: u64, errors: Errors },
}

/// The cleanup screen: the places, their sizes, and a clean running.
pub struct Cleanup {
    pub items: Vec<Item>,
    pub state: TableState,
    pub elevated: bool,
    /// For the job running: files and bytes deleted, the folder at work.
    pub progress: Arc<Progress>,
    job: Option<(Job, Receiver<Msg>)>,
}

impl Cleanup {
    pub fn open() -> Self {
        let items = targets()
            .into_iter()
            .map(|target| Item { ticked: target.safe, target, size: None, files: 0, unreadable: false })
            .collect();
        let mut cleanup = Cleanup {
            items,
            state: TableState::default().with_selected(Some(0)),
            elevated: elevated(),
            progress: Arc::default(),
            job: None,
        };
        cleanup.measure();
        cleanup
    }

    pub fn measuring(&self) -> bool {
        matches!(self.job, Some((Job::Measuring, _)))
    }

    pub fn cleaning(&self) -> bool {
        matches!(self.job, Some((Job::Cleaning { .. }, _)))
    }

    pub fn selected(&self) -> Option<&Item> {
        self.items.get(self.state.selected()?)
    }

    pub fn ticked(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|i| i.ticked)
    }

    pub fn ticked_size(&self) -> u64 {
        self.ticked().filter_map(|i| i.size).sum()
    }

    pub fn toggle(&mut self) {
        if self.cleaning() {
            return;
        }
        if let Some(i) = self.state.selected().and_then(|i| self.items.get_mut(i)) {
            i.ticked = !i.ticked;
        }
    }

    /// Tick all the safe ones, or untick everything if they already are.
    pub fn toggle_all(&mut self) {
        if self.cleaning() {
            return;
        }
        let all_safe = self.items.iter().all(|i| i.ticked == i.target.safe);
        let any = self.items.iter().any(|i| i.ticked);
        for i in &mut self.items {
            i.ticked = if all_safe && any { false } else { i.target.safe };
        }
    }

    /// Measure every place again (after changes made outside).
    pub fn measure(&mut self) {
        if self.cleaning() {
            return;
        }
        self.cancel();
        for i in &mut self.items {
            i.size = None;
        }
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(Progress::default());
        self.progress = progress.clone();
        let paths: Vec<PathBuf> = self.items.iter().map(|i| i.target.path.clone()).collect();
        std::thread::spawn(move || {
            for (index, path) in paths.iter().enumerate() {
                if progress.cancel.load(Ordering::Relaxed) {
                    return;
                }
                send_size(&tx, index, path, &progress);
            }
            let _ = tx.send(Msg::Done);
        });
        self.job = Some((Job::Measuring, rx));
    }

    /// Why a clean can't start now, if it can't.
    pub fn cannot_clean(&self) -> Option<&'static str> {
        if !self.elevated {
            Some("Run diskman as administrator to delete Windows Update files")
        } else if self.cleaning() {
            Some("Wait for the cleanup in progress to finish")
        } else if self.measuring() {
            Some("Wait for the sizes to be measured")
        } else if self.ticked().next().is_none() {
            Some("Nothing ticked: Space ticks a line")
        } else {
            None
        }
    }

    /// Delete what's ticked, then measure what's left.
    pub fn clean(&mut self) {
        if self.cannot_clean().is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(Progress::default());
        self.progress = progress.clone();
        let work: Vec<(usize, PathBuf, bool)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.ticked)
            .map(|(index, i)| (index, i.target.path.clone(), i.target.contents_only))
            .collect();
        std::thread::spawn(move || {
            for (index, path, contents_only) in work {
                let mut errors = Errors::default();
                remove_tree(&path, contents_only, &progress, &mut errors);
                let _ = tx.send(Msg::Cleaned { errors });
                send_size(&tx, index, &path, &Progress::default());
            }
            let _ = tx.send(Msg::Done);
        });
        self.job = Some((Job::Cleaning { freed: 0, errors: Errors::default() }, rx));
    }

    /// Take in what the job did. Returns a message for the status line when it's over.
    pub fn poll(&mut self) -> Option<String> {
        loop {
            let (job, rx) = self.job.as_mut()?;
            let Ok(msg) = rx.try_recv() else { return None };
            match msg {
                Msg::Measured { index, size, files, unreadable } => {
                    let item = &mut self.items[index];
                    if let Job::Cleaning { freed, .. } = job {
                        *freed += item.size.unwrap_or(0).saturating_sub(size);
                        // Nothing left there: nothing to tick.
                        item.ticked &= size > 0;
                    }
                    item.size = Some(size);
                    item.files = files;
                    item.unreadable = unreadable;
                }
                Msg::Cleaned { errors: new } => {
                    if let Job::Cleaning { errors, .. } = job {
                        errors.count += new.count;
                        errors.first = errors.first.take().or(new.first);
                    }
                }
                Msg::Done => {
                    let (job, _) = self.job.take().expect("a job");
                    return match job {
                        Job::Measuring => None,
                        Job::Cleaning { freed, errors } => Some(match errors.first {
                            None => format!("Freed {}", size::format(freed)),
                            Some(first) => format!(
                                "Freed {}; {} could not be deleted (in use? try after a restart). First: {first}",
                                size::format(freed),
                                size::counted(errors.count, "item"),
                            ),
                        }),
                    };
                }
            }
        }
    }

    fn cancel(&mut self) {
        // A clean isn't stopped halfway: only a measure.
        if self.measuring() {
            self.progress.cancel.store(true, Ordering::Relaxed);
            self.job = None;
        }
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn send_size(tx: &Sender<Msg>, index: usize, path: &Path, progress: &Progress) {
    let msg = if matches!(path.try_exists(), Ok(false)) {
        Msg::Measured { index, size: 0, files: 0, unreadable: false }
    } else {
        let node = scan::scan(path, progress);
        Msg::Measured { index, size: node.size, files: node.files, unreadable: node.unreadable_count() > 0 }
    };
    let _ = tx.send(msg);
}

/// Delete `path` and everything in it (with `keep_root`, only what's in it).
/// Goes on past what can't be deleted, and counts it.
fn remove_tree(path: &Path, keep_root: bool, progress: &Progress, errors: &mut Errors) {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return,
        Err(e) => return errors.add(path, e),
    };
    // A link or junction (Windows.old has some): remove the link, never what it points to.
    if meta.file_type().is_symlink() {
        if let Err(e) = writable_retry(path, |p| fs::remove_dir(p).or_else(|_| fs::remove_file(p))) {
            errors.add(path, e);
        }
        return;
    }
    if !meta.is_dir() {
        match writable_retry(path, |p| fs::remove_file(p)) {
            Ok(()) => {
                progress.files.fetch_add(1, Ordering::Relaxed);
                progress.bytes.fetch_add(meta.len(), Ordering::Relaxed);
            }
            Err(e) => errors.add(path, e),
        }
        return;
    }
    if let Ok(mut current) = progress.current.try_lock() {
        *current = path.display().to_string();
    }
    match fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) => remove_tree(&entry.path(), false, progress, errors),
                    Err(e) => errors.add(path, e),
                }
            }
        }
        Err(e) => return errors.add(path, e),
    }
    if !keep_root && let Err(e) = writable_retry(path, |p| fs::remove_dir(p)) {
        errors.add(path, e);
    }
}

/// Do `op`, and if a read-only attribute is in the way, clear it and try again.
fn writable_retry(path: &Path, op: impl Fn(&Path) -> io::Result<()>) -> io::Result<()> {
    match op(path) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            if let Ok(meta) = fs::symlink_metadata(path)
                && meta.permissions().readonly()
            {
                let mut perms = meta.permissions();
                #[allow(clippy::permissions_set_readonly_false)] // Windows: just the attribute
                perms.set_readonly(false);
                if fs::set_permissions(path, perms).is_ok() {
                    return op(path);
                }
            }
            Err(e)
        }
        result => result,
    }
}

/// Is diskman running as administrator?
#[cfg(windows)]
pub fn elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: the token is ours and closed; the buffer is the size given.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
pub fn elevated() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emptying_keeps_the_folder_and_removing_takes_it() {
        let dir = std::env::temp_dir().join(format!("diskman_clean_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("cache/inner")).unwrap();
        fs::write(dir.join("cache/inner/a"), vec![0u8; 300]).unwrap();
        fs::write(dir.join("cache/b"), vec![0u8; 100]).unwrap();
        // A read-only file is deleted too.
        let mut perms = fs::metadata(dir.join("cache/b")).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(dir.join("cache/b"), perms).unwrap();

        let progress = Progress::default();
        let mut errors = Errors::default();
        remove_tree(&dir.join("cache"), true, &progress, &mut errors);
        assert_eq!(errors.count, 0, "{:?}", errors.first);
        assert!(dir.join("cache").is_dir());
        assert_eq!(fs::read_dir(dir.join("cache")).unwrap().count(), 0);
        assert_eq!(progress.files.load(Ordering::Relaxed), 2);
        assert_eq!(progress.bytes.load(Ordering::Relaxed), 400);

        remove_tree(&dir.join("cache"), false, &progress, &mut errors);
        assert!(!dir.join("cache").exists());
        // Already gone: not an error.
        remove_tree(&dir.join("cache"), false, &progress, &mut errors);
        assert_eq!(errors.count, 0);
        fs::remove_dir_all(&dir).unwrap();
    }
}
