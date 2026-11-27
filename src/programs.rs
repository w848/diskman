//! Installed programs, when they were last used, and their uninstallers.
//!
//! The programs come from the registry's Uninstall keys, like Windows' own
//! list of apps (Store apps aren't there). When a program was last used is a
//! best guess from what Windows records: the launch history of the Start menu,
//! taskbar and Explorer (UserAssist), and, for an administrator, Prefetch,
//! which notes every program run lately. "No record" doesn't prove it's never
//! used: a program started some other way may leave no trace in either.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::widgets::TableState;
use rayon::prelude::*;

use crate::scan::{self, Progress};

/// Not used in this long: worth a look.
pub const UNUSED_DAYS: u64 = 90;

#[derive(Clone)]
pub struct Program {
    pub name: String,
    pub publisher: String,
    pub version: String,
    /// From the registry, when the installer wrote it down.
    pub installed: Option<SystemTime>,
    /// Bytes: what the installer says, or else what its folder takes.
    pub size: Option<u64>,
    /// Its folder, when there's one that's surely its own.
    pub folder: Option<PathBuf>,
    pub uninstall: String,
    pub quiet_uninstall: Option<String>,
    location: Option<PathBuf>,
    icon: Option<PathBuf>,
    pub last_used: Option<SystemTime>,
}

impl Program {
    /// Not used (or no record of it) in UNUSED_DAYS, and not just installed.
    pub fn unused(&self, now: SystemTime) -> bool {
        let old = |t: Option<SystemTime>| t.is_none_or(|t| days_between(t, now) >= UNUSED_DAYS);
        old(self.last_used) && old(self.installed)
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Sort {
    /// Least recently used first.
    LastUsed,
    Size,
    Name,
}

/// The Programs screen.
pub struct Programs {
    pub list: Vec<Program>,
    pub state: TableState,
    pub sort: Sort,
    pub elevated: bool,
    /// Checking when each was used, and folder sizes.
    checking: Option<Receiver<Vec<Program>>>,
}

impl Programs {
    pub fn open() -> Self {
        let mut programs = Programs {
            list: Vec::new(),
            state: TableState::default().with_selected(Some(0)),
            sort: Sort::LastUsed,
            elevated: crate::cleanup::elevated(),
            checking: None,
        };
        programs.reload();
        programs
    }

    /// Read the list again (after an uninstall), then check it.
    pub fn reload(&mut self) {
        self.list = installed();
        self.sort_list(None);
        let (tx, rx) = mpsc::channel();
        let list = self.list.clone();
        std::thread::spawn(move || {
            let _ = tx.send(check(list));
        });
        self.checking = Some(rx);
    }

    pub fn checking(&self) -> bool {
        self.checking.is_some()
    }

    pub fn selected(&self) -> Option<&Program> {
        self.list.get(self.state.selected()?)
    }

    pub fn poll(&mut self) {
        if let Some(rx) = &self.checking
            && let Ok(list) = rx.try_recv()
        {
            self.checking = None;
            let selected = self.selected().map(|p| p.name.clone());
            self.list = list;
            self.sort_list(selected.as_deref());
        }
    }

    pub fn next_sort(&mut self) {
        self.sort = match self.sort {
            Sort::LastUsed => Sort::Size,
            Sort::Size => Sort::Name,
            Sort::Name => Sort::LastUsed,
        };
        let selected = self.selected().map(|p| p.name.clone());
        self.sort_list(selected.as_deref());
    }

    fn sort_list(&mut self, keep: Option<&str>) {
        let by_name = |a: &Program, b: &Program| a.name.to_lowercase().cmp(&b.name.to_lowercase());
        match self.sort {
            // No record first, then the longest unused; same: the oldest installed.
            Sort::LastUsed => self.list.sort_by(|a, b| {
                (a.last_used, a.installed).cmp(&(b.last_used, b.installed)).then_with(|| by_name(a, b))
            }),
            Sort::Size => self.list.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| by_name(a, b))),
            Sort::Name => self.list.sort_by(by_name),
        }
        let index = keep.and_then(|name| self.list.iter().position(|p| p.name == name)).unwrap_or(0);
        self.state.select(Some(index.min(self.list.len().saturating_sub(1))));
    }

    /// The programs not used in UNUSED_DAYS, and what they take.
    pub fn unused(&self, now: SystemTime) -> (usize, u64) {
        self.list.iter().filter(|p| p.unused(now)).fold((0, 0), |(n, size), p| (n + 1, size + p.size.unwrap_or(0)))
    }
}

/// The installed programs, as Windows lists them in its apps.
#[cfg(windows)]
fn installed() -> Vec<Program> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};

    const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
    let roots = [
        (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY),
        (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY),
        (HKEY_CURRENT_USER, KEY_WOW64_64KEY),
    ];
    let mut seen = std::collections::HashSet::new();
    let mut list = Vec::new();
    for (root, view) in roots {
        let Ok(uninstall) = RegKey::predef(root).open_subkey_with_flags(UNINSTALL, KEY_READ | view) else { continue };
        for name in uninstall.enum_keys().flatten() {
            let Ok(key) = uninstall.open_subkey_with_flags(&name, KEY_READ | view) else { continue };
            let text = |value: &str| key.get_value::<String, _>(value).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
            let number = |value: &str| key.get_value::<u32, _>(value).ok();
            // Hidden parts of other programs, and updates: Windows doesn't list them either.
            if number("SystemComponent") == Some(1) || text("ParentKeyName").is_some() {
                continue;
            }
            if text("ReleaseType").is_some_and(|t| ["update", "hotfix", "security update", "service pack"].contains(&t.to_lowercase().as_str())) {
                continue;
            }
            let (Some(name), Some(uninstall)) = (text("DisplayName"), text("UninstallString")) else { continue };
            let version = text("DisplayVersion").unwrap_or_default();
            if !seen.insert((name.to_lowercase(), version.clone())) {
                continue;
            }
            list.push(Program {
                publisher: text("Publisher").unwrap_or_default(),
                installed: text("InstallDate").and_then(|d| parse_date(&d)),
                size: number("EstimatedSize").filter(|&kb| kb > 0).map(|kb| u64::from(kb) * 1024),
                folder: None,
                uninstall: expand_env(&uninstall),
                quiet_uninstall: text("QuietUninstallString").map(|s| expand_env(&s)),
                location: text("InstallLocation").map(|s| PathBuf::from(expand_env(s.trim_matches('"')))),
                icon: text("DisplayIcon").and_then(|s| exe_of_icon(&expand_env(&s))),
                last_used: None,
                name,
                version,
            });
        }
    }
    list
}

#[cfg(not(windows))]
fn installed() -> Vec<Program> {
    Vec::new()
}

/// Find each program's folder, size and last use.
fn check(mut list: Vec<Program>) -> Vec<Program> {
    let usage = Usage::read();
    let generic = generic_folders();
    list.par_iter_mut().for_each(|program| {
        program.folder = own_folder(program, &generic);
        let mut exes: Vec<String> = Vec::new();
        if let Some(icon) = &program.icon
            && let Some(name) = icon.file_name()
        {
            exes.push(name.to_string_lossy().to_lowercase());
        }
        if let Some(folder) = &program.folder {
            find_exes(folder, 0, &mut exes, &mut 0);
            if program.size.is_none() {
                program.size = Some(scan::scan(folder, &Progress::default()).size).filter(|&s| s > 0);
            }
        }
        exes.retain(|name| !helper(name));
        program.last_used = usage.last_use(program, &exes);
    });
    list
}

/// What Windows recorded of programs started.
#[derive(Default)]
struct Usage {
    /// Full paths of programs started from the Start menu, taskbar or Explorer, lowercase.
    paths: Vec<(String, SystemTime)>,
    /// Their file names, and those Prefetch saw run, lowercase.
    exes: HashMap<String, SystemTime>,
    /// Shortcuts started, by name without ".lnk", lowercase.
    shortcuts: HashMap<String, SystemTime>,
}

fn note(map: &mut HashMap<String, SystemTime>, key: String, time: SystemTime) {
    let entry = map.entry(key).or_insert(time);
    *entry = (*entry).max(time);
}

impl Usage {
    fn read() -> Self {
        let mut usage = Usage::default();
        #[cfg(windows)]
        usage.read_user_assist();
        usage.read_prefetch();
        usage
    }

    /// The launch history of the Start menu, taskbar and Explorer: names in ROT13,
    /// with the last time started at bytes 60..68.
    #[cfg(windows)]
    fn read_user_assist(&mut self) {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;
        let Ok(root) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Explorer\UserAssist")
        else {
            return;
        };
        for guid in root.enum_keys().flatten() {
            let Ok(count) = root.open_subkey(format!(r"{guid}\Count")) else { continue };
            for (name, value) in count.enum_values().flatten() {
                let bytes = &value.bytes[..];
                let Some(time) = bytes.get(60..68).and_then(|b| from_filetime(u64::from_le_bytes(b.try_into().ok()?))) else {
                    continue;
                };
                let path = resolve_known_folder(&rot13(&name)).to_lowercase();
                let file = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned();
                if path.ends_with(".exe") {
                    self.paths.push((path, time));
                    note(&mut self.exes, file, time);
                } else if let Some(stem) = file.strip_suffix(".lnk") {
                    note(&mut self.shortcuts, stem.to_owned(), time);
                }
            }
        }
    }

    /// Prefetch files are named after the program, "CODE.EXE-1A2B3C4D.pf",
    /// and rewritten each time it runs. Only an administrator can read them.
    fn read_prefetch(&mut self) {
        let windows = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        let Ok(entries) = fs::read_dir(windows.join("Prefetch")) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let Some((exe, _)) = name.strip_suffix(".pf").and_then(|s| s.rsplit_once('-')) else { continue };
            if let Ok(time) = entry.metadata().and_then(|m| m.modified()) {
                note(&mut self.exes, exe.to_owned(), time);
            }
        }
    }

    /// The last time any of the program was seen started.
    fn last_use(&self, program: &Program, exes: &[String]) -> Option<SystemTime> {
        let mut last: Option<SystemTime> = None;
        let mut seen = |time: SystemTime| last = Some(last.map_or(time, |l| l.max(time)));
        if let Some(folder) = &program.folder {
            let prefix = format!("{}\\", folder.to_string_lossy().trim_end_matches('\\').to_lowercase());
            for (path, time) in &self.paths {
                if path.starts_with(&prefix) && !helper(path.rsplit('\\').next().unwrap_or(path)) {
                    seen(*time);
                }
            }
        }
        for exe in exes {
            if let Some(time) = self.exes.get(exe) {
                seen(*time);
            }
        }
        // A shortcut named like the program: "Some Editor" for "Some Editor 2.1".
        let name = program.name.to_lowercase();
        for (shortcut, time) in &self.shortcuts {
            if shortcut.len() >= 4 && name.starts_with(shortcut.as_str()) {
                seen(*time);
            }
        }
        last
    }
}

/// Uninstallers, updaters, crash reporters and services start without the
/// person using the program: they don't count as use.
fn helper(exe: &str) -> bool {
    ["unins", "update", "crash", "setup", "install", "helper", "report", "service", "elevat", "telemetry"]
        .iter()
        .any(|word| exe.contains(word))
}

/// The program files in `folder`, a few levels down.
fn find_exes(folder: &Path, depth: usize, exes: &mut Vec<String>, looked: &mut usize) {
    let Ok(entries) = fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        *looked += 1;
        if *looked > 5000 {
            return;
        }
        let Ok(kind) = entry.file_type() else { continue };
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if kind.is_dir() && depth < 3 {
            find_exes(&entry.path(), depth + 1, exes, looked);
        } else if kind.is_file() && name.ends_with(".exe") {
            exes.push(name);
        }
    }
}

/// Folders shared by many programs: never taken as one program's own.
fn generic_folders() -> Vec<String> {
    let env = |name: &str| std::env::var(name).ok();
    let mut folders: Vec<String> = [
        env("ProgramFiles"),
        env("ProgramFiles(x86)"),
        env("ProgramW6432"),
        env("ProgramData"),
        env("LOCALAPPDATA"),
        env("LOCALAPPDATA").map(|l| format!(r"{l}\Programs")),
        env("APPDATA"),
        env("USERPROFILE"),
        env("CommonProgramFiles"),
        env("CommonProgramFiles(x86)"),
    ]
    .into_iter()
    .flatten()
    .collect();
    for letter in b'A'..=b'Z' {
        folders.push(format!("{}:", letter as char));
    }
    folders.iter().map(|f| f.trim_end_matches('\\').to_lowercase()).collect()
}

/// The program's own folder: where it says it's installed, or else where its icon
/// or uninstaller is, if that's not a folder shared with others.
fn own_folder(program: &Program, generic: &[String]) -> Option<PathBuf> {
    let windows = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()).to_lowercase();
    let plausible = |path: &Path| {
        let lower = path.to_string_lossy().trim_end_matches('\\').to_lowercase();
        path.is_absolute()
            && !generic.contains(&lower)
            && !lower.starts_with(&windows)
            && !["package cache", "installer", "\\temp"].iter().any(|w| lower.contains(w))
            && path.is_dir()
    };
    let uninstaller = split_command(&program.uninstall).0;
    let uninstaller = Path::new(&uninstaller);
    let from_uninstaller = (!uninstaller.to_string_lossy().to_lowercase().contains("msiexec")).then(|| uninstaller.parent()).flatten();
    [program.location.as_deref(), program.icon.as_deref().and_then(Path::parent), from_uninstaller]
        .into_iter()
        .flatten()
        .find(|path| plausible(path))
        .map(Path::to_path_buf)
}

/// "C:\App\app.exe,0" or "\"C:\App\app.exe\"": the program file, if it's one.
fn exe_of_icon(icon: &str) -> Option<PathBuf> {
    let path = icon.trim().trim_matches('"');
    let path = match path.rsplit_once(',') {
        Some((path, index)) if index.trim().parse::<i32>().is_ok() => path.trim_matches('"'),
        _ => path,
    };
    path.to_lowercase().ends_with(".exe").then(|| PathBuf::from(path))
}

/// Split a command line into the program and its arguments. Paths with spaces
/// aren't always quoted in the registry: "C:\Program Files\App\uninst.exe /S".
pub fn split_command(command: &str) -> (String, String) {
    let command = command.trim();
    if let Some(rest) = command.strip_prefix('"')
        && let Some((file, args)) = rest.split_once('"')
    {
        return (file.to_owned(), args.trim().to_owned());
    }
    if let Some(end) = command.to_lowercase().find(".exe") {
        let (file, args) = command.split_at(end + 4);
        return (file.to_owned(), args.trim().to_owned());
    }
    match command.split_once(' ') {
        Some((file, args)) => (file.to_owned(), args.trim().to_owned()),
        None => (command.to_owned(), String::new()),
    }
}

/// Start the program's own uninstaller. Windows asks for administrator rights
/// itself when the uninstaller needs them.
#[cfg(windows)]
pub fn run_uninstaller(command: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let (file, args) = split_command(command);
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (verb, file_w, args_w) = (wide("open"), wide(&file), wide(&args));
    // SAFETY: every string is nul-terminated and outlives the call.
    let result = unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file_w.as_ptr(), args_w.as_ptr(), std::ptr::null(), SW_SHOWNORMAL)
    } as isize;
    match result {
        r if r > 32 => Ok(()),
        2 | 3 => Err(format!("the uninstaller isn't there anymore: {file}")),
        5 => Err("access denied (or the administrator prompt was declined)".into()),
        r => Err(format!("Windows error {r} starting {file}")),
    }
}

#[cfg(not(windows))]
pub fn run_uninstaller(_command: &str) -> Result<(), String> {
    Err("uninstalling is for Windows".into())
}

/// Save the list, with the uninstall commands, as a CSV file in Documents.
pub fn export(list: &[Program], now: SystemTime) -> io::Result<PathBuf> {
    let path = documents().join(format!("diskman-programs-{}.csv", date(now)));
    let field = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    // The byte order mark tells Excel it's UTF-8.
    let mut out = String::from("\u{feff}");
    out.push_str("Program,Publisher,Version,Installed,Last used,Days since use,Not used in 90+ days,Size (MB),Uninstall command,Quiet uninstall command,Folder\r\n");
    for p in list {
        let row = [
            field(&p.name),
            field(&p.publisher),
            field(&p.version),
            p.installed.map(date).unwrap_or_default(),
            p.last_used.map_or_else(|| "no record".into(), date),
            p.last_used.map(|t| days_between(t, now).to_string()).unwrap_or_default(),
            (if p.unused(now) { "yes" } else { "no" }).into(),
            p.size.map(|s| format!("{:.0}", s as f64 / 1e6)).unwrap_or_default(),
            field(&p.uninstall),
            field(p.quiet_uninstall.as_deref().unwrap_or("")),
            field(&p.folder.as_ref().map(|f| f.display().to_string()).unwrap_or_default()),
        ];
        out.push_str(&row.join(","));
        out.push_str("\r\n");
    }
    fs::File::create(&path)?.write_all(out.as_bytes())?;
    Ok(path)
}

/// The Documents folder, wherever it really is (OneDrive may hold it).
#[cfg(windows)]
fn documents() -> PathBuf {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath};
    let mut raw: *mut u16 = std::ptr::null_mut();
    // SAFETY: Windows gives a nul-terminated string, freed once read.
    unsafe {
        if SHGetKnownFolderPath(&FOLDERID_Documents, 0, std::ptr::null_mut(), &mut raw) == 0 && !raw.is_null() {
            let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
            let path = String::from_utf16_lossy(std::slice::from_raw_parts(raw, len));
            CoTaskMemFree(raw.cast());
            return PathBuf::from(path);
        }
        CoTaskMemFree(raw.cast());
    }
    std::env::var_os("USERPROFILE").map_or_else(|| PathBuf::from("."), |p| PathBuf::from(p).join("Documents"))
}

#[cfg(not(windows))]
fn documents() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// "%ProgramFiles%\App" with the variables filled in.
fn expand_env(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        let Some(len) = rest[start + 1..].find('%') else { break };
        let name = &rest[start + 1..start + 1 + len];
        out.push_str(&rest[..start]);
        match std::env::var(name) {
            Ok(value) if !name.is_empty() => out.push_str(&value),
            _ => out.push_str(&rest[start..start + len + 2]),
        }
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    out
}

fn rot13(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A' + 13) % 26) + b'A') as char,
            _ => c,
        })
        .collect()
}

/// "{7C5A40EF-...}\App\app.exe" with the known folder's real path.
fn resolve_known_folder(path: &str) -> String {
    const FOLDERS: [(&str, &str, &str); 12] = [
        ("{6D809377-6AF0-444B-8957-A3773F02200E}", "ProgramW6432", ""),
        ("{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}", "ProgramFiles(x86)", ""),
        ("{905E63B6-C1BF-494E-B29C-65B732D3D21A}", "ProgramFiles", ""),
        ("{F38BF404-1D43-42F2-9305-67DE0B28FC23}", "SystemRoot", ""),
        ("{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}", "SystemRoot", r"\System32"),
        ("{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}", "SystemRoot", r"\SysWOW64"),
        ("{F1B32785-6FBA-4FCF-9D55-7B8E7F157091}", "LOCALAPPDATA", ""),
        ("{3EB685DB-65F9-4CF6-A03A-E3EF65729F3D}", "APPDATA", ""),
        ("{62AB5D82-FDC1-4DC3-A9DD-070D1D495D97}", "ProgramData", ""),
        ("{A77F5D77-2E2B-44C3-A6A2-ABA601054A51}", "APPDATA", r"\Microsoft\Windows\Start Menu\Programs"),
        ("{0139D44E-6AFE-49F2-8690-3DAFCAE6FFB8}", "ProgramData", r"\Microsoft\Windows\Start Menu\Programs"),
        ("{9E3995AB-1F9C-4F13-B827-48B24B6C7174}", "APPDATA", r"\Microsoft\Internet Explorer\Quick Launch\User Pinned"),
    ];
    for (guid, var, sub) in FOLDERS {
        if let Some(rest) = path.get(..guid.len()).filter(|p| p.eq_ignore_ascii_case(guid)).map(|_| &path[guid.len()..])
            && let Ok(base) = std::env::var(var)
        {
            return format!("{base}{sub}{rest}");
        }
    }
    path.to_owned()
}

/// A Windows FILETIME (100 ns since 1601); 0 means never.
fn from_filetime(filetime: u64) -> Option<SystemTime> {
    const TO_UNIX: u64 = 116_444_736_000_000_000;
    (filetime > TO_UNIX).then(|| UNIX_EPOCH + Duration::from_nanos((filetime - TO_UNIX) * 100))
}

pub fn days_between(earlier: SystemTime, later: SystemTime) -> u64 {
    later.duration_since(earlier).map_or(0, |d| d.as_secs() / 86_400)
}

/// "today", "3 days ago", "5 months ago", "2 years ago".
pub fn ago(time: SystemTime, now: SystemTime) -> String {
    match days_between(time, now) {
        0 => "today".into(),
        1 => "yesterday".into(),
        d @ 2..60 => format!("{d} days ago"),
        d @ 60..730 => format!("{} months ago", d / 30),
        d => format!("{} years ago", d / 365),
    }
}

/// "20230415" from the registry.
fn parse_date(text: &str) -> Option<SystemTime> {
    let digits = text.get(..8).filter(|d| d.bytes().all(|b| b.is_ascii_digit()))?;
    let (y, m, d): (i64, u32, u32) = (digits[..4].parse().ok()?, digits[4..6].parse().ok()?, digits[6..].parse().ok()?);
    if !(1990..=2100).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Days since 1970-01-01, after Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(days).ok()? * 86_400))
}

/// "2023-04-15".
pub fn date(time: SystemTime) -> String {
    let days = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(feature = "demo")]
impl Program {
    /// A made-up program, for the README's demo GIF.
    pub fn demo(name: &str, publisher: &str, size: u64, installed_days_ago: u64, used_days_ago: Option<u64>, uninstall: &str) -> Self {
        let now = SystemTime::now();
        let ago = |days: u64| now - Duration::from_secs(days * 86_400 + 3_600);
        Program {
            name: name.into(),
            publisher: publisher.into(),
            version: String::new(),
            installed: Some(ago(installed_days_ago)),
            size: Some(size),
            folder: None,
            uninstall: uninstall.into(),
            quiet_uninstall: None,
            location: None,
            icon: None,
            last_used: used_days_ago.map(ago),
        }
    }
}

#[cfg(feature = "demo")]
impl Programs {
    pub fn demo(list: Vec<Program>) -> Self {
        let mut programs =
            Programs { list, state: TableState::default().with_selected(Some(0)), sort: Sort::LastUsed, elevated: true, checking: None };
        programs.sort_list(None);
        programs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitting_uninstall_commands() {
        let split = |c: &str| split_command(c);
        assert_eq!(split(r#""C:\Program Files\App\unins000.exe" /SILENT"#), (r"C:\Program Files\App\unins000.exe".into(), "/SILENT".into()));
        assert_eq!(split(r"C:\Program Files\App\uninst.exe /S"), (r"C:\Program Files\App\uninst.exe".into(), "/S".into()));
        assert_eq!(split("MsiExec.exe /X{1234-5678}"), ("MsiExec.exe".into(), "/X{1234-5678}".into()));
        assert_eq!(split(r#""C:\App\remove.exe""#), (r"C:\App\remove.exe".into(), String::new()));
    }

    #[test]
    fn dates_both_ways() {
        let t = parse_date("20230415").unwrap();
        assert_eq!(date(t), "2023-04-15");
        assert_eq!(date(parse_date("20240229").unwrap()), "2024-02-29");
        assert_eq!(date(UNIX_EPOCH), "1970-01-01");
        assert!(parse_date("2023").is_none() && parse_date("20231340").is_none());
        let later = t + Duration::from_secs(45 * 86_400);
        assert_eq!(days_between(t, later), 45);
        assert_eq!(ago(t, later), "45 days ago");
        assert_eq!(ago(t, t + Duration::from_secs(400 * 86_400)), "13 months ago");
    }

    #[test]
    fn reading_windows_records() {
        assert_eq!(rot13("P:\\Cebtenz Svyrf\\Ncc.rkr"), "C:\\Program Files\\App.exe");
        assert_eq!(from_filetime(0), None);
        assert_eq!(from_filetime(116_444_736_000_000_000 + 10_000_000 * 86_400), Some(UNIX_EPOCH + Duration::from_secs(86_400)));
        assert_eq!(exe_of_icon(r#""C:\App\app.exe",0"#), Some(PathBuf::from(r"C:\App\app.exe")));
        assert_eq!(exe_of_icon(r"C:\App\app.ico"), None);
        assert!(helper("unins000.exe") && helper("googleupdate.exe") && !helper("editor.exe"));
        // SAFETY: tests here don't read this variable from other threads.
        unsafe { std::env::set_var("DISKMAN_TEST_DIR", r"C:\Test") };
        assert_eq!(expand_env(r"%DISKMAN_TEST_DIR%\app.exe /x 100%"), r"C:\Test\app.exe /x 100%");
    }

    #[test]
    fn unused_means_old_and_not_just_installed() {
        let now = parse_date("20260101").unwrap();
        let program = |installed: &str, used: Option<&str>| Program {
            name: "App".into(),
            publisher: String::new(),
            version: String::new(),
            installed: parse_date(installed),
            size: None,
            folder: None,
            uninstall: String::new(),
            quiet_uninstall: None,
            location: None,
            icon: None,
            last_used: used.and_then(parse_date),
        };
        assert!(program("20240101", None).unused(now));
        assert!(program("20240101", Some("20250601")).unused(now));
        assert!(!program("20240101", Some("20251215")).unused(now));
        // Installed last week: no record yet isn't a sign.
        assert!(!program("20251225", None).unused(now));
    }
}
