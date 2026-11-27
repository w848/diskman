//! Drawing the screens.

use std::sync::atomic::Ordering;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, Wrap};

use crate::app::{App, Dialog, Screen, Sort};
use crate::cleanup::Action;
use crate::programs::{self, UNUSED_DAYS};
use crate::size;

const ACCENT: Color = Color::Rgb(0xf0, 0x7a, 0x3a);
const DIR: Color = Color::Rgb(0x61, 0xaf, 0xef);
const DIM: Color = Color::Rgb(0x80, 0x86, 0x90);
const SELECTED: Color = Color::Rgb(0x3e, 0x44, 0x51);
const RED: Color = Color::Rgb(0xe0, 0x6c, 0x75);
const YELLOW: Color = Color::Rgb(0xe5, 0xc0, 0x7b);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [title, summary, list, status, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    app.list_area = list;
    match app.screen {
        Screen::Drives => draw_drives(frame, app, title, summary, list, keys),
        Screen::Tree => draw_tree(frame, app, title, summary, list, keys),
        Screen::Cleanup => draw_cleanup(frame, app, title, summary, list, keys),
        Screen::Programs => draw_programs(frame, app, title, summary, list, keys),
    }
    frame.render_widget(Paragraph::new(app.status.as_str()).fg(Color::Yellow), status);
    match &app.dialog {
        Some(Dialog::Delete { permanent }) => draw_delete(frame, app, *permanent),
        Some(Dialog::Clean) => draw_clean(frame, app),
        Some(Dialog::Uninstall) => draw_uninstall(frame, app),
        Some(Dialog::Help) => draw_help(frame),
        None => {}
    }
}

fn title_line(text: String) -> Line<'static> {
    Line::from(vec![Span::from(" diskman ").bold().fg(Color::Black).bg(ACCENT), Span::from(format!(" {text}")).bold()])
}

fn keys_line(keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (key, what) in keys {
        spans.push(Span::from(format!(" {key} ")).fg(Color::Black).bg(DIM));
        spans.push(Span::from(format!(" {what} ")).fg(DIM));
    }
    Line::from(spans)
}

/// Red when it's most of it, yellow for a good part, green otherwise.
fn bar_color(fraction: f64) -> Color {
    if fraction >= 0.5 {
        Color::Rgb(0xe0, 0x6c, 0x75)
    } else if fraction >= 0.2 {
        Color::Rgb(0xe5, 0xc0, 0x7b)
    } else {
        Color::Rgb(0x98, 0xc3, 0x79)
    }
}

fn draw_drives(frame: &mut Frame, app: &mut App, title: Rect, summary: Rect, list: Rect, keys: Rect) {
    frame.render_widget(title_line("Drives".into()), title);
    frame.render_widget(Paragraph::new("Pick a drive to see what takes its space.").fg(DIM), summary);
    let rows = app.drives.iter().map(|d| {
        let fraction = if d.total == 0 { 0.0 } else { d.used() as f64 / d.total as f64 };
        Row::new(vec![
            Cell::from(format!("{} {}", &d.root[..2], d.label)).bold(),
            Cell::from(size::bar(fraction, 24)).fg(bar_color(fraction)).bg(Color::Rgb(0x2c, 0x31, 0x3a)),
            Cell::from(format!("{:>3.0}%", fraction * 100.0)),
            Cell::from(format!("{:>9} free", size::format(d.free))),
            Cell::from(format!("{:>9} total", size::format(d.total))).fg(DIM),
            Cell::from(d.kind).fg(DIM),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(20),
            Constraint::Length(24),
            Constraint::Length(5),
            Constraint::Length(15),
            Constraint::Length(16),
            Constraint::Fill(1),
        ],
    )
    .header(Row::new(["Drive", "Used", "", "", "", ""]).fg(DIM))
    .column_spacing(2)
    .row_highlight_style(Style::new().bg(SELECTED))
    .highlight_symbol("▶ ");
    frame.render_stateful_widget(table, list, &mut app.drive_state);
    frame.render_widget(keys_line(&[("Enter", "scan"), ("w", "update files"), ("p", "programs"), ("r", "refresh"), ("?", "help"), ("q", "quit")]), keys);
}

fn draw_tree(frame: &mut Frame, app: &mut App, title: Rect, summary: Rect, list: Rect, keys: Rect) {
    frame.render_widget(title_line(app.full_path(&app.path).display().to_string()), title);

    let Some(current) = app.current() else {
        draw_scanning(frame, app, list);
        frame.render_widget(keys_line(&[("Esc", "stop"), ("q", "quit")]), keys);
        return;
    };

    // What's here, and how full the drive is.
    let mut text = vec![
        Span::from(size::format(current.size)).bold(),
        Span::from(format!(" in {}", size::counted(current.files, "file"))),
    ];
    if let Some(drive) = app.drive() {
        text.push(Span::from(format!("   drive {} {} free of {}", &drive.root[..2], size::format(drive.free), size::format(drive.total))).fg(DIM));
        // At the root of a drive: what the scan couldn't see (protected system folders).
        if app.path.is_empty() && app.root_path.as_os_str().len() <= 3 && app.scan.is_none() {
            let unseen = drive.used().saturating_sub(current.size);
            if unseen > 1 << 30 {
                text.push(Span::from(format!("   {} unreadable (system)", size::format(unseen))).fg(DIM));
            }
        }
    }
    let unreadable = current.unreadable_count();
    if unreadable > 0 {
        text.push(Span::from(format!("   {} not readable", size::counted(unreadable as u64, "folder"))).fg(DIM));
    }
    if let Some(job) = &app.scan {
        text.push(Span::from(format!("   rescanning... {} files", size::count(job.progress.files.load(Ordering::Relaxed)))).fg(Color::Yellow));
    }
    frame.render_widget(Line::from(text), summary);

    let total = current.size.max(1) as f64;
    let rows: Vec<Row> = current
        .children
        .iter()
        .map(|node| {
            let fraction = node.size as f64 / total;
            let name = if node.is_dir {
                let mut spans = vec![Span::from(format!("{}\\", node.name)).fg(DIR).bold()];
                if node.unreadable {
                    spans.push(Span::from("  (not readable)").fg(Color::Rgb(0xe0, 0x6c, 0x75)));
                } else {
                    spans.push(Span::from(format!("  {}", size::counted(node.files, "file"))).fg(DIM));
                }
                Line::from(spans)
            } else {
                Line::from(node.name.clone())
            };
            Row::new(vec![
                Cell::from(Line::from(size::format(node.size)).alignment(Alignment::Right)),
                Cell::from(size::bar(fraction, 20)).fg(bar_color(fraction)).bg(Color::Rgb(0x2c, 0x31, 0x3a)),
                Cell::from(Line::from(format!("{:.1}%", fraction * 100.0)).alignment(Alignment::Right)).fg(DIM),
                Cell::from(name),
            ])
        })
        .collect();
    let empty = rows.is_empty();
    let sort = if app.sort == Sort::Size { "Size ▼" } else { "Size" };
    let name = if app.sort == Sort::Name { "Name ▲" } else { "Name" };
    let table = Table::new(rows, [Constraint::Length(9), Constraint::Length(20), Constraint::Length(6), Constraint::Fill(1)])
        .header(Row::new([Cell::from(Line::from(sort).alignment(Alignment::Right)), Cell::from(""), Cell::from(""), Cell::from(name)]).fg(DIM))
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(SELECTED).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(table, list, &mut app.state);
    if empty {
        let area = Rect { y: list.y + 2, height: 1, ..list };
        frame.render_widget(Paragraph::new("(empty)").fg(DIM).alignment(Alignment::Center), area);
    }

    frame.render_widget(
        keys_line(&[
            ("Enter", "open"),
            ("⌫", "back"),
            ("d", "recycle"),
            ("D", "delete"),
            ("e", "explorer"),
            ("s", "sort"),
            ("r", "rescan"),
            ("w", "update files"),
            ("p", "programs"),
            ("?", "help"),
        ]),
        keys,
    );
}

fn draw_scanning(frame: &mut Frame, app: &App, area: Rect) {
    let Some(job) = &app.scan else { return };
    let current = job.progress.current.lock().map(|c| c.clone()).unwrap_or_default();
    let width = area.width.saturating_sub(10).min(80);
    let text = vec![
        Line::from(format!("Scanning {}", job.full.display())).bold(),
        Line::from(""),
        Line::from(format!(
            "{} files   {}   {:.0} s",
            size::count(job.progress.files.load(Ordering::Relaxed)),
            size::format(job.progress.bytes.load(Ordering::Relaxed)),
            job.started.elapsed().as_secs_f64()
        )),
        Line::from(""),
        Line::from(shorten(&current, width.saturating_sub(4) as usize)).fg(DIM),
    ];
    let popup = centered(area, width, 7);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text).alignment(Alignment::Center).block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(ACCENT))),
        popup,
    );
}

fn draw_delete(frame: &mut Frame, app: &App, permanent: bool) {
    let Some(node) = app.selected() else { return };
    let what = if node.is_dir {
        format!("{}\\  ({}, {})", node.name, size::format(node.size), size::counted(node.files, "file"))
    } else {
        format!("{}  ({})", node.name, size::format(node.size))
    };
    let (question, note, color) = if permanent {
        ("Delete permanently?", "This can't be undone.", Color::Rgb(0xe0, 0x6c, 0x75))
    } else {
        (
            "Move to the Recycle Bin?",
            "The space comes back once the Recycle Bin is emptied.",
            Color::Rgb(0xe5, 0xc0, 0x7b),
        )
    };
    let text = vec![
        Line::from(question).bold(),
        Line::from(""),
        Line::from(what).fg(DIR),
        Line::from(""),
        Line::from(note).fg(DIM),
        Line::from(""),
        Line::from(vec![
            Span::from(" y ").fg(Color::Black).bg(color),
            Span::from(" yes    "),
            Span::from(" n ").fg(Color::Black).bg(DIM),
            Span::from(" no"),
        ]),
    ];
    let popup = centered(frame.area(), 64, 9);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(color))),
        popup,
    );
}

fn draw_cleanup(frame: &mut Frame, app: &mut App, title: Rect, summary: Rect, list: Rect, keys: Rect) {
    frame.render_widget(title_line("Windows Update files".into()), title);
    let Some(cleanup) = &mut app.cleanup else { return };

    let text = if cleanup.cleaning() {
        let current = cleanup.progress.current.lock().map(|c| c.clone()).unwrap_or_default();
        Line::from(vec![
            Span::from(format!(
                "Cleaning... {} deleted ({})",
                size::counted(cleanup.progress.files.load(Ordering::Relaxed), "file"),
                size::format(cleanup.progress.bytes.load(Ordering::Relaxed)),
            ))
            .fg(Color::Yellow),
            Span::from(format!("   {}", shorten(&current, 60))).fg(DIM),
        ])
    } else {
        let mut spans = vec![
            Span::from(size::format(cleanup.ticked_size())).bold(),
            Span::from(format!(" to free in {} ticked", cleanup.ticked().count())),
        ];
        if cleanup.dism_ticked() {
            spans.push(Span::from(", plus what DISM frees"));
        }
        if cleanup.measuring() {
            spans.push(Span::from("   measuring...").fg(Color::Yellow));
        }
        if !cleanup.elevated {
            spans.push(Span::from("   not administrator: restart diskman as administrator to delete").fg(RED));
        }
        Line::from(spans)
    };
    frame.render_widget(text, summary);

    let rows: Vec<Row> = cleanup
        .items
        .iter()
        .map(|item| {
            let size = match item.size {
                _ if item.target.action == Action::Dism => "DISM".into(),
                Some(bytes) => size::format(bytes),
                None => "...".into(),
            };
            let mut name = vec![
                Span::from(item.target.label).bold().fg(if item.target.safe { DIR } else { YELLOW }),
                Span::from(format!("  {}", item.target.path.display())).fg(DIM),
            ];
            if item.unreadable {
                name.push(Span::from("  (not all readable)").fg(RED));
            }
            Row::new(vec![
                Cell::from(if item.ticked { "[x]" } else { "[ ]" }).fg(if item.ticked { ACCENT } else { DIM }),
                Cell::from(Line::from(size).alignment(Alignment::Right)),
                Cell::from(Line::from(name)),
            ])
        })
        .collect();
    let empty = rows.is_empty();
    let table = Table::new(rows, [Constraint::Length(3), Constraint::Length(9), Constraint::Fill(1)])
        .header(Row::new(["", "Size", "What"]).fg(DIM))
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(SELECTED).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(table, list, &mut cleanup.state);
    if empty {
        let area = Rect { y: list.y + 2, height: 1, ..list };
        frame.render_widget(Paragraph::new("No Windows Update files to clean").fg(DIM).alignment(Alignment::Center), area);
    }

    frame.render_widget(
        keys_line(&[
            ("Space", "tick"),
            ("a", "all safe"),
            ("D", "delete ticked"),
            ("e", "explorer"),
            ("r", "measure"),
            ("Esc", "back"),
        ]),
        keys,
    );
}

fn draw_clean(frame: &mut Frame, app: &App) {
    let Some(cleanup) = &app.cleanup else { return };
    let mut text = vec![Line::from("Delete the ticked Windows Update files?").bold(), Line::from("")];
    for item in cleanup.ticked() {
        let size = if item.target.action == Action::Dism { "DISM".into() } else { size::format(item.size.unwrap_or(0)) };
        text.push(Line::from(vec![
            Span::from(format!("{size:>9}  ")),
            Span::from(item.target.label).fg(if item.target.safe { DIR } else { YELLOW }),
        ]));
    }
    text.push(Line::from(""));
    text.push(Line::from(format!("{} deleted permanently (not to the Recycle Bin).", size::format(cleanup.ticked_size()))).fg(DIM));
    text.push(Line::from("Windows downloads updates again if it needs them.").fg(DIM));
    if cleanup.dism_ticked() {
        text.push(Line::from("DISM cleans WinSxS the way Windows does; it can take 5 to 30 minutes.").fg(DIM));
    }
    if cleanup.ticked().any(|i| !i.target.safe) {
        text.push(Line::from(""));
        text.push(Line::from("Windows.old ticked: you can no longer go back to the previous Windows.").fg(RED));
    }
    text.push(Line::from(""));
    text.push(Line::from(vec![
        Span::from(" y ").fg(Color::Black).bg(RED),
        Span::from(" yes    "),
        Span::from(" n ").fg(Color::Black).bg(DIM),
        Span::from(" no"),
    ]));
    let popup = centered(frame.area(), 76, text.len() as u16 + 2);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(RED))),
        popup,
    );
}

fn draw_programs(frame: &mut Frame, app: &mut App, title: Rect, summary: Rect, list: Rect, keys: Rect) {
    frame.render_widget(title_line("Programs".into()), title);
    let Some(state) = &mut app.programs else { return };
    let now = std::time::SystemTime::now();

    let (unused, unused_size) = state.unused(now);
    let mut text = vec![
        Span::from(format!("{} installed", size::counted(state.list.len() as u64, "program"))),
        Span::from(format!("   {unused} not used in {UNUSED_DAYS}+ days ({})", size::format(unused_size))).bold().fg(YELLOW),
    ];
    if state.checking() {
        text.push(Span::from("   checking when each was last used...").fg(Color::Yellow));
    } else if !state.elevated {
        text.push(Span::from("   as administrator, diskman sees more of what you run").fg(DIM));
    }
    frame.render_widget(Line::from(text), summary);

    let rows: Vec<Row> = state
        .list
        .iter()
        .map(|p| {
            let last = match p.last_used {
                _ if state.checking() => "...".into(),
                Some(time) => programs::ago(time, now),
                None => "no record".into(),
            };
            let old = !state.checking() && p.unused(now);
            let mut name = vec![Span::from(p.name.clone()).bold().fg(if old { YELLOW } else { Color::Reset })];
            if !p.publisher.is_empty() {
                name.push(Span::from(format!("  {}", p.publisher)).fg(DIM));
            }
            if let Some(installed) = p.installed {
                name.push(Span::from(format!("  installed {}", programs::date(installed))).fg(DIM));
            }
            Row::new(vec![
                Cell::from(last).fg(if old { YELLOW } else { DIM }),
                Cell::from(Line::from(p.size.map(size::format).unwrap_or_default()).alignment(Alignment::Right)),
                Cell::from(Line::from(name)),
            ])
        })
        .collect();
    let empty = rows.is_empty();
    let header = |label: &'static str, sort: programs::Sort| if state.sort == sort { format!("{label} ▲") } else { label.into() };
    let size_header = if state.sort == programs::Sort::Size { "Size ▼".to_owned() } else { "Size".into() };
    let table = Table::new(rows, [Constraint::Length(14), Constraint::Length(9), Constraint::Fill(1)])
        .header(
            Row::new([
                Cell::from(header("Last used", programs::Sort::LastUsed)),
                Cell::from(Line::from(size_header).alignment(Alignment::Right)),
                Cell::from(header("Program", programs::Sort::Name)),
            ])
            .fg(DIM),
        )
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(SELECTED).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(table, list, &mut state.state);
    if empty {
        let area = Rect { y: list.y + 2, height: 1, ..list };
        frame.render_widget(Paragraph::new("No installed programs found").fg(DIM).alignment(Alignment::Center), area);
    }

    frame.render_widget(
        keys_line(&[
            ("Enter", "uninstall"),
            ("x", "save list"),
            ("e", "folder"),
            ("s", "sort"),
            ("r", "refresh"),
            ("Esc", "back"),
        ]),
        keys,
    );
}

fn draw_uninstall(frame: &mut Frame, app: &App) {
    let Some(program) = app.programs.as_ref().and_then(|p| p.selected()) else { return };
    let now = std::time::SystemTime::now();
    let last = program.last_used.map_or_else(|| "no record of use".into(), |t| format!("last used {}", programs::ago(t, now)));
    let size = program.size.map(|s| format!(", {}", size::format(s))).unwrap_or_default();
    let text = vec![
        Line::from(format!("Uninstall {}?", program.name)).bold(),
        Line::from(""),
        Line::from(format!("{last}{size}")).fg(DIR),
        Line::from(""),
        Line::from(program.uninstall.clone()).fg(DIM),
        Line::from(""),
        Line::from("Its own uninstaller opens: follow its steps.").fg(DIM),
        Line::from(""),
        Line::from(vec![
            Span::from(" y ").fg(Color::Black).bg(YELLOW),
            Span::from(" yes    "),
            Span::from(" n ").fg(Color::Black).bg(DIM),
            Span::from(" no"),
        ]),
    ];
    let popup = centered(frame.area(), 76, 12);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(YELLOW))),
        popup,
    );
}

fn draw_help(frame: &mut Frame) {
    let keys = [
        ("↑ ↓  j k", "move"),
        ("PgUp PgDn  Home End", "move faster"),
        ("Enter  →  l", "open the folder (or scan the drive)"),
        ("Backspace  ←  h", "back to the folder above"),
        ("d  Delete", "move to the Recycle Bin"),
        ("D  Shift+Delete", "delete permanently"),
        ("e", "show in Explorer"),
        ("s", "sort by size / by name"),
        ("r", "scan this folder again"),
        ("w", "Windows Update files safe to delete"),
        ("p", "programs, least recently used first"),
        ("Esc", "stop a scan, or go back"),
        ("mouse", "click to select, double-click to open,"),
        ("", "right-click to go back, wheel to scroll"),
        ("q  Ctrl+C", "quit"),
    ];
    let mut lines = vec![Line::from("Keys").bold(), Line::from("")];
    for (key, what) in keys {
        lines.push(Line::from(vec![Span::from(format!("{key:>20}  ")).fg(ACCENT), Span::from(what)]));
    }
    let popup = centered(frame.area(), 66, lines.len() as u16 + 2);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(ACCENT))),
        popup,
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width.min(area.width))]).flex(Flex::Center).areas(area);
    let [area] = Layout::vertical([Constraint::Length(height.min(area.height))]).flex(Flex::Center).areas(area);
    area
}

/// Keep the end of a long path: "...\folder\inner".
fn shorten(path: &str, width: usize) -> String {
    let count = path.chars().count();
    if count <= width {
        return path.to_owned();
    }
    let tail: String = path.chars().skip(count - width.saturating_sub(3)).collect();
    format!("...{tail}")
}
