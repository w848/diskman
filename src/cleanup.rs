//! Windows Update files that are safe to delete: downloaded updates, the
//! Delivery Optimization cache, upgrade leftovers, the previous Windows, and
//! the component store (WinSxS), which only DISM may clean.
//!
//! Never deleted by hand: WinSxS (its files are shared with Windows itself),
//! Installer (needed to repair and uninstall programs) and
//! SoftwareDistribution\DataStore (the update history).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};

use ratatui::widgets::TableState;

use crate::scan::{self, Progress};
use crate::size;

#[derive(Clone, Copy, PartialEq)]
pub enum Action {
    /// Empty the folder but keep it: Windows expects it there.
    Empty,
    /// Delete the folder.
    Remove,
    /// Let DISM clean it (WinSxS): old versions of updated components.
    Dism,
}

pub struct Target {
    pub label: &'static str,
    pub path: PathBuf,
    pub action: Action,
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
            action: Action::Empty,
            safe: true,
        },
        Target {
            label: "Delivery Optimization cache",
            path: windows.join(r"ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache"),
            action: Action::Empty,
            safe: true,
        },
        Target { label: "Upgrade leftovers", path: drive.join("$Windows.~BT"), action: Action::Remove, safe: true },
        Target { label: "Upgrade leftovers", path: drive.join("$Windows.~WS"), action: Action::Remove, safe: true },
        Target {
            label: "Previous Windows (no going back after)",
            path: drive.join("Windows.old"),
            action: Action::Remove,
            safe: false,
        },
        Target {
            label: "Old versions of updated components (DISM, can take minutes)",
            path: windows.join("WinSxS"),
            action: Action::Dism,
            safe: true,
        },
    ];
    // Not readable without administrator rights still means it's there.
    all.into_iter().filter(|t| !matches!(t.path.try_exists(), Ok(false))).collect()
}

pub struct Item {
    pub target: Target,
    pub ticked: bool,
    /// None until measured; always for DISM, which can't tell beforehand.
    pub size: Option<u64>,
    pub files: u64,
    /// Some of it couldn't be read, so the size may be short.
    pub unreadable: bool,
}

enum Msg {
    Measured { index: usize, size: u64, files: u64, unreadable: bool },
    Cleaned { errors: Errors },
    /// DISM is done: what the drive got back.
    Freed { index: usize, bytes: u64 },
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

    fn message(&mut self, message: String) {
        self.count += 1;
        self.first.get_or_insert(message);
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

    /// DISM is ticked: what it frees comes on top of the ticked size.
    pub fn dism_ticked(&self) -> bool {
        self.ticked().any(|i| i.target.action == Action::Dism)
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
        // WinSxS isn't measured: it's mostly hard links to Windows' own files,
        // so adding up its files would show far more than it really takes.
        let paths: Vec<Option<PathBuf>> =
            self.items.iter().map(|i| (i.target.action != Action::Dism).then(|| i.target.path.clone())).collect();
        std::thread::spawn(move || {
            for (index, path) in paths.iter().enumerate() {
                if progress.cancel.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(path) = path {
                    send_size(&tx, index, path, &progress);
                }
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
        let mut work: Vec<(usize, PathBuf, Action)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.ticked)
            .map(|(index, i)| (index, i.target.path.clone(), i.target.action))
            .collect();
        // The quick deletes first, DISM (minutes) last.
        work.sort_by_key(|(_, _, action)| *action == Action::Dism);
        std::thread::spawn(move || {
            for (index, path, action) in work {
                let mut errors = Errors::default();
                if action == Action::Dism {
                    let before = free_space(&path);
                    if let Err(e) = run_dism(&progress) {
                        errors.message(e);
                    }
                    let bytes = free_space(&path).zip(before).map_or(0, |(after, before)| after.saturating_sub(before));
                    let _ = tx.send(Msg::Cleaned { errors });
                    let _ = tx.send(Msg::Freed { index, bytes });
                } else {
                    remove_tree(&path, action == Action::Empty, &progress, &mut errors);
                    let _ = tx.send(Msg::Cleaned { errors });
                    send_size(&tx, index, &path, &Progress::default());
                }
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
                Msg::Freed { index, bytes } => {
                    if let Job::Cleaning { freed, .. } = job {
                        *freed += bytes;
                    }
                    self.items[index].ticked = false;
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
                                "Freed {}; {} (files in use go after a restart). First: {first}",
                                size::format(freed),
                                size::counted(errors.count, "problem"),
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

/// Free space on the drive of `path`.
fn free_space(path: &Path) -> Option<u64> {
    crate::drives::of(&crate::drives::list(), path).map(|d| d.free)
}

/// Run DISM's component cleanup, the one Windows runs on its own now and then:
/// old versions of updated components go. Not /ResetBase, so installed updates
/// can still be uninstalled. Shows DISM's percentage as it goes.
fn run_dism(progress: &Progress) -> Result<(), String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let windows = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let mut command = Command::new(windows.join(r"System32\Dism.exe"));
    command
        .args(["/Online", "/Cleanup-Image", "/StartComponentCleanup"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let set_current = |text: String| {
        if let Ok(mut current) = progress.current.lock() {
            *current = text;
        }
    };
    set_current("Component store (DISM): starting".into());
    let mut child = command.spawn().map_err(|e| format!("Could not start DISM: {e}"))?;
    let mut output = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        let mut buffer = [0u8; 512];
        while let Ok(n @ 1..) = stdout.read(&mut buffer) {
            output.extend_from_slice(&buffer[..n]);
            if let Some(percent) = last_percent(&output) {
                set_current(format!("Component store (DISM): {percent:.1}%"));
            }
        }
    }
    let status = child.wait().map_err(|e| format!("DISM: {e}"))?;
    set_current(String::new());
    if status.success() {
        return Ok(());
    }
    let code = status.code().unwrap_or(-1);
    let why = match code {
        740 => "it needs administrator rights".to_owned(),
        _ => last_line(&output).unwrap_or_else(|| "no message".into()),
    };
    Err(format!("DISM failed (error {:#x}): {why}", code as u32))
}

/// The last percentage in DISM's output: "[=====  42.3%  ]" gives 42.3.
fn last_percent(output: &[u8]) -> Option<f32> {
    let end = output.iter().rposition(|&b| b == b'%')?;
    let start = output[..end]
        .iter()
        .rposition(|b| !(b.is_ascii_digit() || *b == b'.' || *b == b','))
        .map_or(0, |i| i + 1);
    std::str::from_utf8(&output[start..end]).ok()?.replace(',', ".").parse().ok()
}

/// The last line of DISM's output that says something (not a progress bar).
fn last_line(output: &[u8]) -> Option<String> {
    String::from_utf8_lossy(output)
        .split(['\r', '\n'])
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with('['))
        .map(str::to_owned)
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

#[cfg(feature = "demo")]
impl Cleanup {
    /// Made-up places and sizes, for the README's demo GIF.
    pub fn demo(items: Vec<Item>) -> Self {
        Cleanup { items, state: TableState::default().with_selected(Some(0)), elevated: true, progress: Arc::default(), job: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_dism_output() {
        let output = b"Deployment Image Servicing and Management tool\r\n\r\n[==    10.0%    ]\r[=====    42.3%    ]\r";
        assert_eq!(last_percent(output), Some(42.3));
        assert_eq!(last_percent(b"[==   7,5%  ]"), Some(7.5));
        assert_eq!(last_percent(b"no progress yet"), None);
        let failed = b"[==== 20.0% ====]\r\nError: 0x800f0806\r\n\r\nThe operation could not be completed due to pending operations.\r\n";
        assert_eq!(last_line(failed).as_deref(), Some("The operation could not be completed due to pending operations."));
    }

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
