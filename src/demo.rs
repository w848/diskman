//! Draws the README's demo GIF: diskman's own screens, with made-up drives,
//! folders and programs, so no real files or programs show. Each screen is
//! drawn to an image with a Windows font, and ffmpeg puts them together.
//!
//! `cargo run --release --features demo -- --demo-gif docs/demo.gif`

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use fontdue::{Font, FontSettings, Metrics};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier};

use crate::app::App;
use crate::cleanup::{Action, Cleanup, Item, Target};
use crate::drives::Drive;
use crate::programs::{Program, Programs};
use crate::scan::Node;
use crate::ui;

const COLUMNS: u16 = 104;
const ROWS: u16 = 26;
const FONT_PX: f32 = 17.0;
const PAD: usize = 14;
const FG: [u8; 3] = [0xd7, 0xda, 0xe0];
const BG: [u8; 3] = [0x1b, 0x1f, 0x24];

fn gb(n: f64) -> u64 {
    (n * (1u64 << 30) as f64) as u64
}

fn mb(n: f64) -> u64 {
    (n * (1u64 << 20) as f64) as u64
}

pub fn record(out: &Path) -> io::Result<()> {
    let mut terminal = Terminal::new(TestBackend::new(COLUMNS, ROWS)).expect("a terminal in memory");
    let mut frames: Vec<(Buffer, u32)> = Vec::new();
    let mut shot = |app: &mut App, ms: u32| {
        terminal.draw(|frame| ui::draw(frame, app)).expect("drawing in memory");
        frames.push((terminal.backend().buffer().clone(), ms));
    };
    let press = |app: &mut App, code: KeyCode| app.key(KeyEvent::new(code, KeyModifiers::NONE));

    // The drives.
    let mut app = App::demo(drives());
    shot(&mut app, 1600);
    press(&mut app, KeyCode::Down);
    shot(&mut app, 450);
    press(&mut app, KeyCode::Up);
    shot(&mut app, 450);

    // Scanning C:, then what takes its space.
    let c = PathBuf::from(r"C:\");
    app.demo_scanning(c.clone(), 182_344, gb(61.0), 1, r"C:\Users\demo\AppData\Local\Packages");
    shot(&mut app, 450);
    app.demo_scanning(c.clone(), 624_108, gb(198.4), 2, r"C:\Program Files\PixelForge\assets");
    shot(&mut app, 450);
    app.demo_scanning(c, 1_208_455, gb(371.9), 4, r"C:\Windows\WinSxS\amd64_microsoft-windows-shell");
    shot(&mut app, 450);
    app.demo_scanned(c_drive(), 4.6);
    shot(&mut app, 2200);

    // Into Games, and an old game to the Recycle Bin.
    press(&mut app, KeyCode::Down);
    shot(&mut app, 600);
    press(&mut app, KeyCode::Enter);
    shot(&mut app, 1500);
    press(&mut app, KeyCode::Down);
    shot(&mut app, 350);
    press(&mut app, KeyCode::Down);
    shot(&mut app, 600);
    press(&mut app, KeyCode::Char('d'));
    shot(&mut app, 2000);
    press(&mut app, KeyCode::Esc);
    app.demo_recycled();
    shot(&mut app, 2000);
    press(&mut app, KeyCode::Backspace);
    app.status.clear();
    shot(&mut app, 800);

    // The Windows Update cleanup.
    app.demo_cleanup(Cleanup::demo(cleanup_items()));
    shot(&mut app, 2400);
    press(&mut app, KeyCode::Char('D'));
    shot(&mut app, 2600);
    press(&mut app, KeyCode::Esc);
    if let Some(cleanup) = &mut app.cleanup {
        for item in cleanup.items.iter_mut().filter(|i| i.ticked) {
            item.ticked = false;
            if item.target.action != Action::Dism {
                item.size = Some(0);
            }
        }
    }
    app.status = "Freed 14.6 GB".into();
    shot(&mut app, 2000);
    press(&mut app, KeyCode::Esc);
    app.status.clear();

    // The programs, least recently used first, and one to uninstall.
    app.demo_programs(Programs::demo(programs()));
    shot(&mut app, 2800);
    press(&mut app, KeyCode::Down);
    shot(&mut app, 400);
    press(&mut app, KeyCode::Down);
    shot(&mut app, 700);
    press(&mut app, KeyCode::Enter);
    shot(&mut app, 2600);
    press(&mut app, KeyCode::Esc);
    shot(&mut app, 1400);

    write_gif(&frames, out)
}

fn drives() -> Vec<Drive> {
    let drive = |root: &str, label: &str, kind, total, free| Drive { root: root.into(), label: label.into(), kind, total, free };
    vec![
        drive(r"C:\", "Windows", "local disk", gb(476.9), gb(52.3)),
        drive(r"D:\", "Data", "local disk", gb(1863.0), gb(1210.4)),
        drive(r"E:\", "Backup", "removable", gb(931.5), gb(402.0)),
    ]
}

fn c_drive() -> Node {
    let folder = |name: &str, size: u64, files: u64| Node { name: name.into(), size, files, is_dir: true, unreadable: false, children: Vec::new() };
    let file = |name: &str, size: u64| Node::file(name.into(), size);
    let with = |name: &str, children: Vec<Node>| Node::dir(name.into(), children);
    with(
        r"C:\",
        vec![
            with("Users", vec![folder("demo", gb(158.2), 412_904), folder("Public", gb(1.2), 2_310), folder("Default", gb(0.3), 1_120)]),
            with(
                "Games",
                vec![
                    folder("Flight Simulator", gb(61.4), 18_230),
                    folder("Racing League", gb(28.7), 9_412),
                    folder("Old RPG", gb(14.2), 6_120),
                    folder("Puzzle Pack", gb(2.1), 840),
                ],
            ),
            folder("Program Files", gb(58.3), 214_552),
            folder("Windows", gb(31.6), 412_008),
            folder("Program Files (x86)", gb(23.9), 98_337),
            file("hiberfil.sys", gb(12.7)),
            folder("ProgramData", gb(11.8), 160_421),
            file("pagefile.sys", gb(8.0)),
        ],
    )
}

fn cleanup_items() -> Vec<Item> {
    let item = |label, path: &str, action, safe, size: Option<u64>| Item {
        target: Target { label, path: path.into(), action, safe },
        ticked: safe,
        size,
        files: 1_000,
        unreadable: false,
    };
    vec![
        item("Downloaded updates", r"C:\Windows\SoftwareDistribution\Download", Action::Empty, true, Some(gb(3.4))),
        item(
            "Delivery Optimization cache",
            r"C:\Windows\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache",
            Action::Empty,
            true,
            Some(gb(1.9)),
        ),
        item("Upgrade leftovers", r"C:\$Windows.~BT", Action::Remove, true, Some(gb(6.2))),
        item("Previous Windows (no going back after)", r"C:\Windows.old", Action::Remove, false, Some(gb(24.8))),
        item("Old versions of updated components (DISM, can take minutes)", r"C:\Windows\WinSxS", Action::Dism, true, None),
    ]
}

fn programs() -> Vec<Program> {
    let p = Program::demo;
    vec![
        p("PixelForge Studio 3", "Northlight Software", gb(2.4), 980, None, r#""C:\Program Files\PixelForge\unins000.exe""#),
        p("Retro Arcade Collection", "Arcadia Games", gb(6.1), 720, None, r#""C:\Games\Retro Arcade\uninstall.exe""#),
        p("TrackMaster Racing 2", "Velocity Interactive", gb(38.2), 640, Some(410), r#""C:\Games\TrackMaster 2\unins000.exe""#),
        p("WaveShaper Audio Suite", "Soundfield Labs", gb(1.3), 900, Some(230), "MsiExec.exe /X{6F1E2A3B-9C4D-4E5F-8A7B-1C2D3E4F5A6B}"),
        p("PhotoFrame Viewer", "Lumen Apps", mb(96.0), 500, Some(150), r#""C:\Program Files\PhotoFrame\uninstall.exe""#),
        p("ZipVault", "Small Tools Co.", mb(14.0), 1100, Some(97), r#""C:\Program Files\ZipVault\Uninstall.exe""#),
        p("SecureTunnel VPN", "Tunnelworks", mb(122.0), 300, Some(31), r#""C:\Program Files\SecureTunnel\uninst.exe""#),
        p("Game Hub Launcher", "Hubworks", mb(910.0), 400, Some(12), r#""C:\Program Files\Game Hub\uninstall.exe""#),
        p("CloudBox Sync", "CloudBox Inc.", mb(350.0), 800, Some(3), r#""C:\Program Files\CloudBox\unins000.exe""#),
        p("Quill Code Editor", "Quill Project", mb(412.0), 200, Some(0), r#""C:\Program Files\Quill\unins000.exe""#),
        p("Horizon Web Browser", "Horizon Foundation", mb(640.0), 600, Some(0), r#""C:\Program Files\Horizon\uninstall.exe""#),
    ]
}

/// Draw each frame to an image, then let ffmpeg make the GIF.
fn write_gif(frames: &[(Buffer, u32)], out: &Path) -> io::Result<()> {
    let dir = std::env::temp_dir().join(format!("diskman_demo_{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let mut painter = Painter::new()?;
    let mut list = String::new();
    for (i, (buffer, ms)) in frames.iter().enumerate() {
        let name = format!("frame{i:03}.ppm");
        painter.paint(buffer, &dir.join(&name))?;
        list.push_str(&format!("file '{name}'\nduration {:.3}\n", f64::from(*ms) / 1000.0));
    }
    // ffmpeg's concat wants the last file again for its duration to count.
    list.push_str(&format!("file 'frame{:03}.ppm'\n", frames.len() - 1));
    fs::write(dir.join("frames.txt"), list)?;
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    let out = std::path::absolute(out)?;
    let status = Command::new("ffmpeg")
        .current_dir(&dir)
        .args(["-y", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i", "frames.txt"])
        .args(["-vf", "split[a][b];[a]palettegen=max_colors=96:stats_mode=full[p];[b][p]paletteuse=dither=none"])
        .args(["-fps_mode", "vfr", "-loop", "0"])
        .arg(&out)
        .status()
        .map_err(|e| io::Error::other(format!("could not run ffmpeg (is it installed?): {e}")))?;
    fs::remove_dir_all(&dir)?;
    if !status.success() {
        return Err(io::Error::other("ffmpeg failed"));
    }
    println!("{} frames -> {}", frames.len(), out.display());
    Ok(())
}

/// Draws a terminal screen as a terminal would.
struct Painter {
    /// Cascadia Mono first; others for characters it lacks.
    fonts: Vec<Font>,
    cell_w: usize,
    cell_h: usize,
    ascent: i32,
    glyphs: HashMap<char, Option<(Metrics, Vec<u8>)>>,
}

impl Painter {
    fn new() -> io::Result<Self> {
        let dir = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts");
        let fonts: Vec<Font> = ["CascadiaMono.ttf", "consola.ttf", "seguisym.ttf"]
            .iter()
            .filter_map(|name| fs::read(dir.join(name)).ok())
            .filter_map(|bytes| Font::from_bytes(bytes, FontSettings::default()).ok())
            .collect();
        let Some(main) = fonts.first() else {
            return Err(io::Error::other("no Cascadia Mono or Consolas font in Windows\\Fonts"));
        };
        let line = main.horizontal_line_metrics(FONT_PX).expect("a font with line metrics");
        Ok(Painter {
            cell_w: main.metrics('M', FONT_PX).advance_width.round() as usize,
            cell_h: (line.new_line_size + 2.0).ceil() as usize,
            ascent: (line.ascent + 1.0).round() as i32,
            fonts,
            glyphs: HashMap::new(),
        })
    }

    fn glyph(&mut self, c: char) -> Option<&(Metrics, Vec<u8>)> {
        let fonts = &self.fonts;
        self.glyphs
            .entry(c)
            .or_insert_with(|| fonts.iter().find(|f| f.lookup_glyph_index(c) != 0).map(|f| f.rasterize(c, FONT_PX)))
            .as_ref()
    }

    fn paint(&mut self, buffer: &Buffer, path: &Path) -> io::Result<()> {
        let (cols, rows) = (buffer.area.width as usize, buffer.area.height as usize);
        let (w, h) = (cols * self.cell_w + 2 * PAD, rows * self.cell_h + 2 * PAD);
        let mut image = Canvas { w, h, pixels: BG.repeat(w * h) };
        for y in 0..rows {
            for x in 0..cols {
                let Some(cell) = buffer.cell((x as u16, y as u16)) else { continue };
                let (mut fg, mut bg) = (color(cell.fg, FG), color(cell.bg, BG));
                if cell.modifier.contains(Modifier::REVERSED) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let (x0, y0) = (PAD + x * self.cell_w, PAD + y * self.cell_h);
                image.fill(x0, y0, self.cell_w, self.cell_h, bg);
                let Some(c) = cell.symbol().chars().next() else { continue };
                if c == ' ' || self.shape(&mut image, c, x0, y0, fg) {
                    continue;
                }
                let bold = cell.modifier.contains(Modifier::BOLD);
                let ascent = self.ascent;
                let Some((metrics, coverage)) = self.glyph(c).cloned() else { continue };
                let gx = x0 as i32 + metrics.xmin;
                let gy = y0 as i32 + ascent - (metrics.height as i32 + metrics.ymin);
                for offset in if bold { 0..2 } else { 0..1 } {
                    image.blend(gx + offset, gy, &metrics, &coverage, fg);
                }
            }
        }
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        out.extend_from_slice(&image.pixels);
        fs::write(path, out)
    }

    /// Blocks and box lines, drawn to fill the cell so bars and borders join up.
    fn shape(&self, image: &mut Canvas, c: char, x0: usize, y0: usize, fg: [u8; 3]) -> bool {
        let (w, h) = (self.cell_w, self.cell_h);
        let eighths = match c {
            '█' => 8,
            '▉' => 7,
            '▊' => 6,
            '▋' => 5,
            '▌' => 4,
            '▍' => 3,
            '▎' => 2,
            '▏' => 1,
            _ => 0,
        };
        if eighths > 0 {
            image.fill(x0, y0, (w * eighths).div_ceil(8), h, fg);
            return true;
        }
        let (left, right, up, down) = match c {
            '─' => (true, true, false, false),
            '│' => (false, false, true, true),
            '╭' | '┌' => (false, true, false, true),
            '╮' | '┐' => (true, false, false, true),
            '╰' | '└' => (false, true, true, false),
            '╯' | '┘' => (true, false, true, false),
            _ => return false,
        };
        let t = 2;
        let (cx, cy) = (x0 + w / 2 - t / 2, y0 + h / 2 - t / 2);
        if left {
            image.fill(x0, cy, cx - x0 + t, t, fg);
        }
        if right {
            image.fill(cx, cy, x0 + w - cx, t, fg);
        }
        if up {
            image.fill(cx, y0, t, cy - y0 + t, fg);
        }
        if down {
            image.fill(cx, cy, t, y0 + h - cy, fg);
        }
        true
    }
}

struct Canvas {
    w: usize,
    h: usize,
    pixels: Vec<u8>,
}

impl Canvas {
    fn fill(&mut self, x0: usize, y0: usize, w: usize, h: usize, rgb: [u8; 3]) {
        for y in y0..(y0 + h).min(self.h) {
            for x in x0..(x0 + w).min(self.w) {
                let i = (y * self.w + x) * 3;
                self.pixels[i..i + 3].copy_from_slice(&rgb);
            }
        }
    }

    fn blend(&mut self, gx: i32, gy: i32, metrics: &Metrics, coverage: &[u8], rgb: [u8; 3]) {
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let (x, y) = (gx + col as i32, gy + row as i32);
                if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
                    continue;
                }
                let a = u16::from(coverage[row * metrics.width + col]);
                let i = (y as usize * self.w + x as usize) * 3;
                for k in 0..3 {
                    let under = u16::from(self.pixels[i + k]);
                    self.pixels[i + k] = ((under * (255 - a) + u16::from(rgb[k]) * a) / 255) as u8;
                }
            }
        }
    }
}

/// A terminal's colors, as Windows Terminal's dark theme shows them.
fn color(color: Color, default: [u8; 3]) -> [u8; 3] {
    match color {
        Color::Rgb(r, g, b) => [r, g, b],
        Color::Black => [0x16, 0x19, 0x1e],
        Color::Red | Color::LightRed => [0xe0, 0x6c, 0x75],
        Color::Green | Color::LightGreen => [0x98, 0xc3, 0x79],
        Color::Yellow | Color::LightYellow => [0xe5, 0xc0, 0x7b],
        Color::Blue | Color::LightBlue => [0x61, 0xaf, 0xef],
        Color::Magenta | Color::LightMagenta => [0xc6, 0x78, 0xdd],
        Color::Cyan | Color::LightCyan => [0x56, 0xb6, 0xc2],
        Color::Gray | Color::White => [0xe6, 0xe6, 0xe6],
        Color::DarkGray => [0x5c, 0x63, 0x70],
        _ => default,
    }
}
