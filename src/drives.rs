//! The drives of the PC, with their size and free space.

pub struct Drive {
    /// "C:\".
    pub root: String,
    pub label: String,
    pub kind: &'static str,
    pub total: u64,
    pub free: u64,
}

impl Drive {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
}

#[cfg(windows)]
pub fn list() -> Vec<Drive> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let mask = unsafe { GetLogicalDrives() };
    let mut drives = Vec::new();
    for i in 0..26 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + i) as char);
        let root_w = wide(&root);
        let kind = match unsafe { GetDriveTypeW(root_w.as_ptr()) } {
            2 => "removable",
            3 => "local disk",
            4 => "network",
            5 => "CD/DVD",
            6 => "RAM disk",
            _ => continue,
        };
        let (mut free, mut total, mut total_free) = (0u64, 0u64, 0u64);
        // Fails for an empty card reader or DVD drive: not listed.
        if unsafe { GetDiskFreeSpaceExW(root_w.as_ptr(), &mut free, &mut total, &mut total_free) } == 0 {
            continue;
        }
        let mut name = [0u16; 261];
        let ok = unsafe {
            GetVolumeInformationW(
                root_w.as_ptr(),
                name.as_mut_ptr(),
                name.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        let label = if ok != 0 {
            let len = name.iter().position(|&c| c == 0).unwrap_or(0);
            String::from_utf16_lossy(&name[..len])
        } else {
            String::new()
        };
        drives.push(Drive { root, label, kind, total, free: total_free });
    }
    drives
}

#[cfg(not(windows))]
pub fn list() -> Vec<Drive> {
    vec![Drive { root: "/".into(), label: String::new(), kind: "disk", total: 0, free: 0 }]
}

/// The drive a path is on.
pub fn of<'a>(drives: &'a [Drive], path: &std::path::Path) -> Option<&'a Drive> {
    let path = path.to_string_lossy().to_uppercase();
    drives.iter().find(|d| path.starts_with(&d.root.to_uppercase()) || path.starts_with(&d.root[..2]))
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_the_system_drive() {
        let drives = super::list();
        let windir = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let drive = super::of(&drives, std::path::Path::new(&windir)).expect("the Windows drive");
        assert!(drive.total > 0 && drive.free <= drive.total);
    }
}
