//! Drawing the explorer with ratatui.
//!
//! `draw` reads the [`App`] and paints one frame. It only writes to `app`
//! to remember the scroll position and the page height. Every size computation
//! uses saturating arithmetic or ratatui's `Layout`, so tiny terminals
//! (even 0×0) cannot cause a panic.

use std::time::SystemTime;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use super::app::{App, Mode, ScanState, scroll_offset};
use crate::commands::scan::{DiskTree, NodeId};
use crate::duration::{format_age, format_timestamp};
use crate::output::bars::{meter, percent};
use crate::output::format_count;
use crate::size::format_size;
use crate::walk::EntryKind;

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const BAR_WIDTH: usize = 10;
/// Height of the info panel including its border.
const INFO_HEIGHT: u16 = 7;

/// Every key binding, shown in the help popup.
pub const KEY_HELP: [(&str, &str); 12] = [
    ("↑ ↓  k j", "move the cursor"),
    ("PgUp PgDn  g G", "page up/down, first/last"),
    ("Enter → l", "open the directory"),
    ("Backspace ← h", "go to the parent directory"),
    ("s", "sort by size → name → modified"),
    ("/", "filter by name (live)"),
    ("Esc", "clear the filter"),
    ("Enter", "finish typing a filter"),
    ("i", "show/hide details of the entry"),
    ("?", "show/hide this help"),
    ("q  Ctrl-C", "quit"),
    ("", "Nothing is ever deleted."),
];

/// Colors are optional (`--no-color`, `NO_COLOR`); bold and reverse video
/// are kept even without colors so the cursor stays visible.
fn color(style: Style, color: Color, colors: bool) -> Style {
    if colors { style.fg(color) } else { style }
}

/// Paints one frame.
pub fn draw(frame: &mut Frame, app: &mut App, colors: bool) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header, app, colors);
    match &app.state {
        ScanState::Scanning(progress) => {
            let spinner = SPINNER[usize::try_from(app.ticks % 10).unwrap_or(0)];
            let lines = vec![
                Line::from(format!("{spinner} Scanning {} …", app.root.display())),
                Line::from(format!(
                    "{} entries · {}",
                    format_count(progress.items),
                    format_size(progress.bytes)
                )),
                Line::from(""),
                Line::from("Press q to quit"),
            ];
            draw_centered_text(frame, body, lines);
        }
        ScanState::Failed(message) => {
            let lines = vec![
                Line::from(format!("Scan failed: {message}")),
                Line::from("Press q to quit"),
            ];
            draw_centered_text(frame, body, lines);
        }
        ScanState::Ready(_) => draw_listing(frame, body, app, colors),
    }
    draw_footer(frame, footer, app);
    if app.mode == Mode::Help {
        draw_help(frame, frame.area());
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App, colors: bool) {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled(" fsight ", color(bold, Color::Cyan, colors))];
    if let Some(tree) = app.tree() {
        let cwd = tree.node(app.cwd);
        spans.push(Span::styled(tree.path(app.cwd).display().to_string(), bold));
        spans.push(Span::raw(format!(
            "  {} of {}  sort: {}",
            format_size(cwd.size),
            format_size(tree.node(tree.root()).size),
            app.sort.label()
        )));
        if !app.filter.is_empty() || app.mode == Mode::Filter {
            let cursor = if app.mode == Mode::Filter { "▏" } else { "" };
            spans.push(Span::raw(format!("  filter: {}{cursor}", app.filter)));
        }
    } else {
        spans.push(Span::styled(app.root.display().to_string(), bold));
    }
    let style = Style::new().add_modifier(Modifier::REVERSED);
    frame.render_widget(Paragraph::new(Line::from(spans)).style(style), area);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let text = match (&app.status, app.mode) {
        (Some(message), _) => message.clone(),
        (None, Mode::Filter) => " type to filter · Enter keep · Esc clear".to_string(),
        (None, _) => {
            " ↑↓ move  Enter open  ← up  s sort  / filter  i info  ? help  q quit".to_string()
        }
    };
    let style = Style::new().add_modifier(Modifier::DIM);
    frame.render_widget(Paragraph::new(text).style(style), area);
}

fn draw_listing(frame: &mut Frame, area: Rect, app: &mut App, colors: bool) {
    let show_info = app.show_info && area.height > INFO_HEIGHT + 2;
    let [list_area, info_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(if show_info { INFO_HEIGHT } else { 0 }),
    ])
    .areas(area);

    // Keep the cursor on screen, scrolling as little as possible.
    let height = usize::from(list_area.height);
    app.page = height.max(1);
    app.offset = scroll_offset(app.offset, app.selected, height);

    let Some(tree) = app.tree() else {
        return;
    };
    let lines: Vec<Line> = if app.visible.is_empty() {
        let message = if app.filter.is_empty() {
            "  (empty directory)"
        } else {
            "  (nothing matches the filter)"
        };
        vec![Line::from(message)]
    } else {
        let parent_size = tree.node(app.cwd).size;
        app.visible
            .iter()
            .enumerate()
            .skip(app.offset)
            .take(height)
            .map(|(index, &id)| row(tree, id, parent_size, index == app.selected, colors))
            .collect()
    };
    frame.render_widget(Paragraph::new(lines), list_area);

    if show_info && let Some(id) = app.selected_node() {
        draw_info(frame, info_area, tree, id);
    }
}

/// One listing row: size, percentage of the current directory, bar, name.
fn row(
    tree: &DiskTree,
    id: NodeId,
    parent_size: u64,
    selected: bool,
    colors: bool,
) -> Line<'static> {
    let node = tree.node(id);
    let mut name = node.name.to_string_lossy().into_owned();
    let name_style = match node.kind {
        EntryKind::Dir => {
            name.push('/');
            color(
                Style::new().add_modifier(Modifier::BOLD),
                Color::Blue,
                colors,
            )
        }
        EntryKind::Symlink => color(Style::new(), Color::Cyan, colors),
        EntryKind::File | EntryKind::Other => Style::new(),
    };
    let line = Line::from(vec![
        Span::raw(format!(
            "{:>10} {:>3}%  ",
            format_size(node.size),
            percent(node.size, parent_size)
        )),
        Span::styled(
            meter(node.size, parent_size, BAR_WIDTH),
            color(Style::new(), Color::Green, colors),
        ),
        Span::raw("  "),
        Span::styled(name, name_style),
    ]);
    if selected {
        line.style(Style::new().add_modifier(Modifier::REVERSED))
    } else {
        line
    }
}

fn draw_info(frame: &mut Frame, area: Rect, tree: &DiskTree, id: NodeId) {
    let node = tree.node(id);
    let kind = match node.kind {
        EntryKind::Dir => "directory",
        EntryKind::File => "file",
        EntryKind::Symlink => "symbolic link",
        EntryKind::Other => "special file",
    };
    let modified = node.modified.map_or_else(
        || "unknown".to_string(),
        |time| {
            let age = SystemTime::now().duration_since(time).unwrap_or_default();
            format!("{} UTC ({} ago)", format_timestamp(time), format_age(age))
        },
    );
    let lines = vec![
        Line::from(format!("Path:     {}", tree.path(id).display())),
        Line::from(format!("Type:     {kind}")),
        Line::from(format!(
            "Size:     {} ({} bytes)",
            format_size(node.size),
            format_count(node.size)
        )),
        Line::from(format!("Files:    {}", format_count(node.files))),
        Line::from(format!("Modified: {modified}")),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Info ")),
        area,
    );
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let lines: Vec<Line> = KEY_HELP
        .iter()
        .map(|(keys, action)| Line::from(format!("{keys:>16}  {action}")))
        .collect();
    // Width: the longest line plus the border; height: lines plus the border.
    let width = lines.iter().map(Line::width).max().unwrap_or(0) + 4;
    let height = lines.len() + 2;
    let popup = centered(area, width, height);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Keys ")),
        popup,
    );
}

fn draw_centered_text(frame: &mut Frame, area: Rect, lines: Vec<Line>) {
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let top = area.height.saturating_sub(height) / 2;
    let text_area = Rect {
        y: area.y.saturating_add(top),
        height: area.height.saturating_sub(top),
        ..area
    };
    frame.render_widget(
        Paragraph::new(lines).centered().wrap(Wrap { trim: true }),
        text_area,
    );
}

/// A rectangle of (at most) `width` × `height` cells in the middle of `area`.
fn centered(area: Rect, width: usize, height: usize) -> Rect {
    let width = u16::try_from(width).unwrap_or(u16::MAX).min(area.width);
    let height = u16::try_from(height).unwrap_or(u16::MAX).min(area.height);
    Rect {
        x: area.x.saturating_add((area.width - width) / 2),
        y: area.y.saturating_add((area.height - height) / 2),
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::tests::ready_app;
    use crate::tui::app::{App, SortMode};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::Path;

    /// Draws `app` on an in-memory terminal and returns the screen as text.
    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app, true)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn shows_progress_while_scanning() {
        let mut app = App::new(Path::new("/data"), SortMode::Size);
        let screen = render(&mut app, 60, 10);
        assert!(screen.contains("Scanning /data"), "{screen}");
        assert!(screen.contains("Press q to quit"), "{screen}");
    }

    #[test]
    fn lists_entries_with_sizes_bars_and_a_header() {
        let mut app = ready_app();
        let screen = render(&mut app, 80, 12);
        assert!(screen.contains(" fsight ."), "{screen}");
        assert!(
            screen.contains("1.1 KiB of 1.1 KiB  sort: size"),
            "{screen}"
        );
        // 1000 of 1110 bytes: 90% (rounded down) and 9 of 10 bar cells.
        assert!(screen.contains("1000 B  90%  █████████░  big/"), "{screen}");
        assert!(screen.contains("Small.txt"), "{screen}");
        assert!(screen.contains("? help"), "{screen}");
    }

    #[test]
    fn info_panel_and_help_popup_are_drawn() {
        let mut app = ready_app();
        app.toggle_info();
        let screen = render(&mut app, 80, 20);
        assert!(screen.contains("Path:     ./big"), "{screen}");
        assert!(screen.contains("Files:    2"), "{screen}");
        app.toggle_help();
        let screen = render(&mut app, 80, 20);
        assert!(screen.contains("Keys"), "{screen}");
        assert!(screen.contains("Nothing is ever deleted."), "{screen}");
    }

    #[test]
    fn the_cursor_stays_visible_when_scrolling() {
        let mut app = ready_app();
        app.move_to_bottom();
        // Header + footer + 1 listing row: only the selected row fits.
        let screen = render(&mut app, 60, 3);
        assert!(screen.contains("Small.txt"), "{screen}");
        assert_eq!(app.offset, 2);
    }

    #[test]
    fn tiny_and_odd_terminal_sizes_do_not_panic() {
        for (width, height) in [
            (0, 0),
            (1, 1),
            (2, 1),
            (1, 5),
            (5, 2),
            (12, 4),
            (300, 2),
            (3, 200),
        ] {
            let mut app = ready_app();
            render(&mut app, width, height);
            app.toggle_info();
            app.toggle_help();
            render(&mut app, width, height);
            let mut scanning = App::new(Path::new("/a/very/long/path/name"), SortMode::Size);
            render(&mut scanning, width, height);
        }
    }
}
