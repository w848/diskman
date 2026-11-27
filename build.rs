//! Puts the icon (and the name and version from Cargo.toml) into diskman.exe,
//! so Explorer, the taskbar and Scoop show it.

fn main() {
    println!("cargo:rerun-if-changed=assets/diskman.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/diskman.ico");
        resource.set("FileDescription", "diskman: see what takes the space on your drives");
        resource.compile().expect("putting the icon into diskman.exe");
    }
}
