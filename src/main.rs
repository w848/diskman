//! diskman: see what takes the space on your drives, and free it.
//!
//! `diskman` starts on the list of drives; `diskman <folder>` scans that folder.

mod app;
mod cleanup;
mod drives;
mod programs;
mod scan;
mod size;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use ratatui::crossterm::execute;

use app::App;

fn main() -> std::io::Result<()> {
    let start = std::env::args_os().nth(1).map(PathBuf::from);
    if let Some(path) = &start
        && !path.is_dir()
    {
        eprintln!("Not a folder: {}", path.display());
        std::process::exit(1);
    }
    let start = start.map(|p| std::path::absolute(&p).unwrap_or(p));
    let mut app = App::new(start);
    give_back_mouse_on_crash();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = ratatui::run(|terminal| run(terminal, &mut app));
    execute!(std::io::stdout(), DisableMouseCapture)?;
    result
}

/// If diskman crashes, the terminal must get the mouse back: otherwise
/// clicks stay captured and the mouse seems dead until the terminal is closed.
/// (ratatui puts the screen back by itself, not the mouse.)
fn give_back_mouse_on_crash() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // A background scan or delete that fails doesn't end diskman: keep the mouse then.
        if std::thread::current().name() == Some("main") {
            let _ = execute!(std::io::stdout(), DisableMouseCapture);
        }
        previous(info);
    }));
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    while !app.quit {
        app.poll();
        terminal.draw(|frame| ui::draw(frame, app))?;
        // While something runs in the background, redraw often to show it.
        let wait = if app.busy() { Duration::from_millis(100) } else { Duration::from_millis(500) };
        if event::poll(wait)? {
            match event::read()? {
                // Windows also reports key releases.
                Event::Key(key) if key.kind != KeyEventKind::Release => app.key(key),
                Event::Mouse(mouse) => app.mouse(mouse),
                _ => {}
            }
        }
    }
    Ok(())
}
