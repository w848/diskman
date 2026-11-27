//! What the app shows and does: the drives, a scanned folder tree, and
//! deleting what takes room. Scans and deletes run in the background so the
//! screen stays live.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;

use crate::cleanup::Cleanup;
use crate::drives::{self, Drive};
use crate::programs::{self, Programs};
use crate::scan::{self, Node, Progress};
use crate::size;

#[derive(Clone, Copy, PartialEq)]
pub enum Screen {
    Drives,
    Tree,
    /// Windows Update files safe to delete.
    Cleanup,
    /// Installed programs, least recently used first.
    Programs,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Sort {
    Size,
    Name,
}

pub enum Dialog {
    Delete { permanent: bool },
    /// Delete the ticked Windows Update files?
    Clean,
    /// Run the selected program's uninstaller?
    Uninstall,
    Help,
}

/// A scan running in the background.
pub struct ScanJob {
    pub progress: Arc<Progress>,
    rx: Receiver<Node>,
    /// The folder being scanned, as names from the root (empty: the root).
    pub path: Vec<String>,
    pub full: PathBuf,
    pub started: Instant,
}

/// A delete running in the background.
pub struct DeleteJob {
    rx: Receiver<Result<(), String>>,
    path: Vec<String>,
    pub name: String,
    size: u64,
    permanent: bool,
}

pub struct App {
    pub screen: Screen,
    pub drives: Vec<Drive>,
    pub drive_state: TableState,
    /// The scanned folder, and what's in it.
    pub root_path: PathBuf,
    pub root: Option<Node>,
    /// The folder shown, as names from the root.
    pub path: Vec<String>,
    pub state: TableState,
    pub sort: Sort,
    pub scan: Option<ScanJob>,
    pub deleting: Option<DeleteJob>,
    pub dialog: Option<Dialog>,
    pub cleanup: Option<Cleanup>,
    pub programs: Option<Programs>,
    /// The screen to go back to from the cleanup or the programs.
    back: Screen,
    pub status: String,
    pub quit: bool,
    /// Where the list was drawn, to know which row the mouse is on.
    pub list_area: Rect,
    last_click: Option<(Instant, usize)>,
}

impl App {
    pub fn new(start: Option<PathBuf>) -> Self {
        let mut app = App {
            screen: Screen::Drives,
            drives: drives::list(),
            drive_state: TableState::default().with_selected(Some(0)),
            root_path: PathBuf::new(),
            root: None,
            path: Vec::new(),
            state: TableState::default().with_selected(Some(0)),
            sort: Sort::Size,
            scan: None,
            deleting: None,
            dialog: None,
            cleanup: None,
            programs: None,
            back: Screen::Drives,
            status: String::new(),
            quit: false,
            list_area: Rect::default(),
            last_click: None,
        };
        if let Some(path) = start {
            app.start_scan(path);
        }
        app
    }

    /// Scan a whole folder or drive, from the start.
    pub fn start_scan(&mut self, path: PathBuf) {
        if let Some(job) = self.scan.take() {
            job.progress.cancel.store(true, Ordering::Relaxed);
        }
        self.root_path = path.clone();
        self.root = None;
        self.path.clear();
        self.state.select(Some(0));
        self.screen = Screen::Tree;
        self.scan = Some(spawn_scan(path, Vec::new()));
    }

    /// Scan the folder shown again (after changes made outside).
    fn rescan(&mut self) {
        if self.scan.is_some() {
            return;
        }
        let full = self.full_path(&self.path);
        self.scan = Some(spawn_scan(full, self.path.clone()));
    }

    /// The folder shown.
    pub fn current(&self) -> Option<&Node> {
        self.root.as_ref().and_then(|root| scan::node_at(root, &self.path))
    }

    pub fn selected(&self) -> Option<&Node> {
        self.current()?.children.get(self.state.selected()?)
    }

    pub fn full_path(&self, path: &[String]) -> PathBuf {
        let mut full = self.root_path.clone();
        full.extend(path);
        full
    }

    pub fn drive(&self) -> Option<&Drive> {
        drives::of(&self.drives, &self.root_path)
    }

    /// Take in what the background jobs finished.
    pub fn poll(&mut self) {
        if let Some(job) = &self.scan
            && let Ok(node) = job.rx.try_recv()
        {
            let job = self.scan.take().expect("a scan");
            let seconds = job.started.elapsed().as_secs_f64();
            let selected = self.selected().map(|n| n.name.clone());
            match &mut self.root {
                Some(root) if !job.path.is_empty() || job.full == self.root_path => {
                    scan::replace(root, &job.path, node);
                }
                _ => self.root = Some(node),
            }
            self.sort_all();
            self.select_name(selected.as_deref());
            self.status = format!("Scanned {} in {seconds:.1} s", job.full.display());
            self.drives = drives::list(); // free space may have changed
        }
        if let Some(job) = &self.deleting
            && let Ok(result) = job.rx.try_recv()
        {
            let job = self.deleting.take().expect("a delete");
            match result {
                Ok(()) => {
                    if let Some(root) = &mut self.root {
                        scan::remove(root, &job.path, &job.name);
                    }
                    self.status = if job.permanent {
                        format!("Deleted {}: {} freed", job.name, size::format(job.size))
                    } else {
                        format!(
                            "Moved {} ({}) to the Recycle Bin: empty it to get the space back",
                            job.name,
                            size::format(job.size)
                        )
                    };
                    self.clamp_selection();
                }
                Err(e) => {
                    self.status = format!("Could not delete {}: {e}", job.name);
                    // Part of it may be gone: count again.
                    self.rescan();
                }
            }
            self.drives = drives::list();
        }
        if let Some(cleanup) = &mut self.cleanup
            && let Some(message) = cleanup.poll()
        {
            self.status = message;
            self.drives = drives::list();
        }
        if let Some(programs) = &mut self.programs {
            programs.poll();
        }
    }

    /// The background jobs need the screen redrawn now and then.
    pub fn busy(&self) -> bool {
        self.scan.is_some()
            || self.deleting.is_some()
            || self.cleanup.as_ref().is_some_and(|c| c.measuring() || c.cleaning())
            || self.programs.as_ref().is_some_and(Programs::checking)
    }

    fn open_programs(&mut self) {
        if self.screen == Screen::Programs {
            return;
        }
        self.programs = Some(Programs::open());
        self.back = self.screen;
        self.screen = Screen::Programs;
    }

    fn open_cleanup(&mut self) {
        if self.screen == Screen::Cleanup {
            return;
        }
        // A clean still running is kept, to see it finish.
        if !self.cleanup.as_ref().is_some_and(Cleanup::cleaning) {
            self.cleanup = Some(Cleanup::open());
        }
        self.back = self.screen;
        self.screen = Screen::Cleanup;
    }

    fn close_cleanup(&mut self) {
        if !self.cleanup.as_ref().is_some_and(Cleanup::cleaning) {
            self.cleanup = None;
        }
        self.screen = self.back;
    }

    /// The list on the screen, and how long it is.
    fn list_state(&mut self) -> Option<(&mut TableState, usize)> {
        match self.screen {
            Screen::Drives => Some((&mut self.drive_state, self.drives.len())),
            Screen::Tree => {
                let len = self.root.as_ref().and_then(|r| scan::node_at(r, &self.path)).map_or(0, |n| n.children.len());
                Some((&mut self.state, len))
            }
            Screen::Cleanup => self.cleanup.as_mut().map(|c| (&mut c.state, c.items.len())),
            Screen::Programs => self.programs.as_mut().map(|p| (&mut p.state, p.list.len())),
        }
    }

    fn sort_all(&mut self) {
        fn sort_tree(node: &mut Node, sort: Sort) {
            match sort {
                Sort::Size => scan::sort_by_size(&mut node.children),
                Sort::Name => scan::sort_by_name(&mut node.children),
            }
            for child in &mut node.children {
                sort_tree(child, sort);
            }
        }
        if let Some(root) = &mut self.root {
            sort_tree(root, self.sort);
        }
    }

    fn select_name(&mut self, name: Option<&str>) {
        let index = name.and_then(|name| self.current()?.children.iter().position(|c| c.name == name));
        self.state.select(Some(index.unwrap_or(0)));
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        let len = self.current().map_or(0, |n| n.children.len());
        let selected = self.state.selected().unwrap_or(0).min(len.saturating_sub(1));
        self.state.select(Some(selected));
    }

    fn open_selected(&mut self) {
        let Some(node) = self.selected() else { return };
        if !node.is_dir {
            return;
        }
        self.path.push(node.name.clone());
        self.state.select(Some(0));
        *self.state.offset_mut() = 0;
    }

    fn go_up(&mut self) {
        match self.path.pop() {
            Some(left) => self.select_name(Some(&left)),
            None if self.scan.is_none() => self.screen = Screen::Drives,
            None => {}
        }
    }

    fn move_selection(&mut self, by: isize) {
        let Some((state, len)) = self.list_state() else { return };
        if len == 0 {
            return;
        }
        let selected = state.selected().unwrap_or(0) as isize + by;
        state.select(Some(selected.clamp(0, len as isize - 1) as usize));
    }

    fn page(&self) -> isize {
        (self.list_area.height as isize - 2).max(1)
    }

    pub fn key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if let Some(dialog) = &self.dialog {
            match (dialog, key.code) {
                (Dialog::Delete { permanent }, KeyCode::Char('y') | KeyCode::Enter) => {
                    let permanent = *permanent;
                    self.dialog = None;
                    self.delete_selected(permanent);
                }
                (Dialog::Clean, KeyCode::Char('y') | KeyCode::Enter) => {
                    self.dialog = None;
                    if let Some(cleanup) = &mut self.cleanup {
                        cleanup.clean();
                        self.status = "Cleaning Windows Update files...".into();
                    }
                }
                (Dialog::Uninstall, KeyCode::Char('y') | KeyCode::Enter) => {
                    self.dialog = None;
                    self.uninstall_selected();
                }
                (Dialog::Help, KeyCode::Char('?')) | (_, KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q')) => {
                    self.dialog = None;
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.dialog = Some(Dialog::Help),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-self.page()),
            KeyCode::PageDown => self.move_selection(self.page()),
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(-1_000_000_000),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(1_000_000_000),
            _ if self.screen == Screen::Cleanup => self.cleanup_key(key),
            _ if self.screen == Screen::Programs => self.programs_key(key),
            KeyCode::Char('w') => self.open_cleanup(),
            KeyCode::Char('p') => self.open_programs(),
            _ if self.screen == Screen::Drives => match key.code {
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                    if let Some(drive) = self.drive_state.selected().and_then(|i| self.drives.get(i)) {
                        let root = PathBuf::from(&drive.root);
                        self.start_scan(root);
                    }
                }
                KeyCode::Char('r') => self.drives = drives::list(),
                KeyCode::Esc => self.quit = true,
                _ => {}
            },
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_selected(),
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => self.go_up(),
            KeyCode::Esc => match &self.scan {
                // Stop a scan; with nothing scanned yet, back to the drives.
                Some(job) => {
                    job.progress.cancel.store(true, Ordering::Relaxed);
                    self.scan = None;
                    self.status = "Scan stopped".into();
                    if self.root.is_none() {
                        self.screen = Screen::Drives;
                    }
                }
                None => self.go_up(),
            },
            KeyCode::Char('s') => {
                self.sort = if self.sort == Sort::Size { Sort::Name } else { Sort::Size };
                let selected = self.selected().map(|n| n.name.clone());
                self.sort_all();
                self.select_name(selected.as_deref());
            }
            KeyCode::Char('r') => self.rescan(),
            KeyCode::Char('e') => self.show_in_explorer(),
            KeyCode::Char('d') | KeyCode::Delete if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.ask_delete(false)
            }
            KeyCode::Char('D') | KeyCode::Delete => self.ask_delete(true),
            _ => {}
        }
    }

    fn cleanup_key(&mut self, key: KeyEvent) {
        let Some(cleanup) = &mut self.cleanup else { return };
        match key.code {
            KeyCode::Char(' ') | KeyCode::Enter => cleanup.toggle(),
            KeyCode::Char('a') => cleanup.toggle_all(),
            KeyCode::Char('r') => cleanup.measure(),
            KeyCode::Char('D') | KeyCode::Char('d') | KeyCode::Delete => match cleanup.cannot_clean() {
                Some(why) => self.status = why.into(),
                None => self.dialog = Some(Dialog::Clean),
            },
            KeyCode::Char('e') => {
                if let Some(item) = cleanup.selected()
                    && let Err(e) = open_in_explorer(&item.target.path)
                {
                    self.status = format!("Could not open Explorer: {e}");
                }
            }
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('w') => {
                if cleanup.cleaning() {
                    self.status = "Wait for the cleanup to finish".into();
                } else {
                    self.close_cleanup();
                }
            }
            _ => {}
        }
    }

    fn programs_key(&mut self, key: KeyEvent) {
        let Some(list) = &mut self.programs else { return };
        match key.code {
            KeyCode::Enter | KeyCode::Char('u') | KeyCode::Delete => {
                if list.selected().is_some() {
                    self.dialog = Some(Dialog::Uninstall);
                }
            }
            KeyCode::Char('x') => {
                if list.checking() {
                    self.status = "Wait for the last use of each program to be checked".into();
                    return;
                }
                self.status = match programs::export(&list.list, std::time::SystemTime::now()) {
                    Ok(path) => {
                        let _ = open_in_explorer(&path);
                        format!("Saved the list to {}", path.display())
                    }
                    Err(e) => format!("Could not save the list: {e}"),
                };
            }
            KeyCode::Char('e') => match list.selected().and_then(|p| p.folder.clone()) {
                Some(folder) => {
                    if let Err(e) = open_in_explorer(&folder) {
                        self.status = format!("Could not open Explorer: {e}");
                    }
                }
                None => self.status = "No folder known for this program".into(),
            },
            KeyCode::Char('s') => list.next_sort(),
            KeyCode::Char('r') => list.reload(),
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('p') => {
                self.programs = None;
                self.screen = self.back;
            }
            _ => {}
        }
    }

    fn uninstall_selected(&mut self) {
        let Some(program) = self.programs.as_ref().and_then(Programs::selected) else { return };
        self.status = match programs::run_uninstaller(&program.uninstall) {
            Ok(()) => format!("The uninstaller of {} is open: when it's done, press r to refresh the list", program.name),
            Err(e) => format!("Could not start the uninstaller of {}: {e}", program.name),
        };
    }

    pub fn mouse(&mut self, mouse: MouseEvent) {
        if self.dialog.is_some() {
            return;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.move_selection(-3),
            MouseEventKind::ScrollDown => self.move_selection(3),
            MouseEventKind::Down(MouseButton::Left) => {
                let area = self.list_area;
                // The first row of the list is its header.
                if mouse.column < area.x || mouse.column >= area.right() || mouse.row <= area.y || mouse.row >= area.bottom() {
                    return;
                }
                let Some((state, len)) = self.list_state() else { return };
                let index = state.offset() + (mouse.row - area.y - 1) as usize;
                if index >= len {
                    return;
                }
                state.select(Some(index));
                // A second click on the same row soon after: open it.
                let double = self
                    .last_click
                    .is_some_and(|(at, row)| row == index && at.elapsed() < Duration::from_millis(450));
                self.last_click = Some((Instant::now(), index));
                if double {
                    self.last_click = None;
                    self.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
            }
            MouseEventKind::Down(MouseButton::Right) if self.screen == Screen::Tree => self.go_up(),
            MouseEventKind::Down(MouseButton::Right) if matches!(self.screen, Screen::Cleanup | Screen::Programs) => {
                self.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            }
            _ => {}
        }
    }

    fn ask_delete(&mut self, permanent: bool) {
        if self.deleting.is_some() {
            self.status = "Wait for the delete in progress to finish".into();
        } else if self.scan.is_some() {
            self.status = "Wait for the scan to finish (Esc stops it)".into();
        } else if self.selected().is_some() {
            self.dialog = Some(Dialog::Delete { permanent });
        }
    }

    fn delete_selected(&mut self, permanent: bool) {
        let Some(node) = self.selected() else { return };
        let (name, size, is_dir) = (node.name.clone(), node.size, node.is_dir);
        let full = self.full_path(&self.path).join(&name);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = if permanent {
                if is_dir { std::fs::remove_dir_all(&full) } else { std::fs::remove_file(&full) }
                    .map_err(|e| e.to_string())
            } else {
                trash::delete(&full).map_err(|e| e.to_string())
            };
            let _ = tx.send(result);
        });
        self.status = format!("Deleting {name}...");
        self.deleting = Some(DeleteJob { rx, path: self.path.clone(), name, size, permanent });
    }

    fn show_in_explorer(&mut self) {
        let target = match self.selected() {
            Some(node) => self.full_path(&self.path).join(&node.name),
            None => self.full_path(&self.path),
        };
        if let Err(e) = open_in_explorer(&target) {
            self.status = format!("Could not open Explorer: {e}");
        }
    }
}

fn spawn_scan(full: PathBuf, path: Vec<String>) -> ScanJob {
    let progress = Arc::new(Progress::default());
    let (tx, rx) = mpsc::channel();
    let shared = progress.clone();
    let dir = full.clone();
    std::thread::spawn(move || {
        let _ = tx.send(scan::scan(&dir, &shared));
    });
    ScanJob { progress, rx, path, full, started: Instant::now() }
}

/// Open Explorer with `path` selected.
#[cfg(windows)]
fn open_in_explorer(path: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", path.display())).spawn().map(drop)
}

#[cfg(not(windows))]
fn open_in_explorer(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn().map(drop)
}

/// Made-up states for the README's demo GIF (`--features demo`): nothing here
/// scans, deletes or reads the real system.
#[cfg(feature = "demo")]
impl App {
    pub fn demo(drives: Vec<Drive>) -> Self {
        let mut app = App::new(None);
        app.drives = drives;
        app
    }

    /// A scan of `path` that seems to be running.
    pub fn demo_scanning(&mut self, path: PathBuf, files: u64, bytes: u64, seconds: u64, current: &str) {
        let progress = Arc::new(Progress::default());
        progress.files.store(files, Ordering::Relaxed);
        progress.bytes.store(bytes, Ordering::Relaxed);
        if let Ok(mut c) = progress.current.lock() {
            *c = current.to_owned();
        }
        let (_, rx) = mpsc::channel();
        self.root_path = path.clone();
        self.root = None;
        self.path.clear();
        self.screen = Screen::Tree;
        let started = Instant::now().checked_sub(Duration::from_secs(seconds)).unwrap_or_else(Instant::now);
        self.scan = Some(ScanJob { progress, rx, path: Vec::new(), full: path, started });
    }

    pub fn demo_scanned(&mut self, root: Node, seconds: f64) {
        let full = self.root_path.clone();
        self.scan = None;
        self.root = Some(root);
        self.sort_all();
        self.state.select(Some(0));
        self.status = format!("Scanned {} in {seconds:.1} s", full.display());
    }

    /// What a finished move to the Recycle Bin of the selected item shows.
    pub fn demo_recycled(&mut self) {
        let Some(node) = self.selected() else { return };
        let (name, size) = (node.name.clone(), node.size);
        if let Some(root) = &mut self.root {
            scan::remove(root, &self.path, &name);
        }
        self.status = format!("Moved {name} ({}) to the Recycle Bin: empty it to get the space back", size::format(size));
        self.clamp_selection();
    }

    pub fn demo_cleanup(&mut self, cleanup: Cleanup) {
        self.cleanup = Some(cleanup);
        self.back = self.screen;
        self.screen = Screen::Cleanup;
    }

    pub fn demo_programs(&mut self, programs: Programs) {
        self.programs = Some(programs);
        self.back = self.screen;
        self.screen = Screen::Programs;
    }
}
