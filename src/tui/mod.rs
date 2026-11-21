//! The interactive disk usage explorer (`fsight tui`).
//!
//! * `app.rs` — the state and its transitions (pure, unit-tested).
//! * `events.rs` — key presses → transitions (pure, unit-tested).
//! * `ui.rs` — drawing with ratatui (tested on an in-memory terminal).
//! * this file — the terminal itself, the background scan and the event loop.
//!
//! # Threads
//!
//! ```text
//!   scanner thread                              UI thread (this one)
//!   ┌──────────────────────────┐   channel   ┌──────────────────────────┐
//!   │ build the DiskTree       │  Progress   │ loop:                    │
//!   │ (walker threads below)   │ ──────────▶ │   apply scanner messages │
//!   │ every 100 ms: Progress   │  Finished   │   draw a frame           │
//!   │ at the end:   Finished   │ ──────────▶ │   wait ≤100 ms for a key │
//!   └──────────────────────────┘             └──────────────────────────┘
//! ```
//!
//! The UI never waits for the scan, so it stays responsive (and `q` works)
//! while the scan runs. Quitting sets the cancellation flag, which stops the
//! walker quickly.
//!
//! # Restoring the terminal
//!
//! Raw mode and the alternate screen must be undone however we leave:
//! normally, through an error (`?`), or by a panic. A guard value restores
//! the terminal in its `Drop` (which also runs while a panic unwinds), and a
//! panic hook restores it *before* the panic message is printed, so the
//! message is readable.

pub mod app;
pub mod events;
pub mod ui;

use std::io::{self, IsTerminal, Stdout};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use self::app::{App, SortMode};
use crate::commands::Context;
use crate::commands::scan::{self, DiskTree};
use crate::error::{Error, Result};
use crate::progress::ProgressSnapshot;
use crate::walk::Walker;

/// How often the scanner reports progress and the UI checks for keys.
const TICK: Duration = Duration::from_millis(100);

/// Is the terminal currently in raw mode on the alternate screen? A `static`
/// is shared by the whole program, including the panic hook; an atomic makes
/// it safe to read and write from any thread without `unsafe`.
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Messages from the scanner thread to the UI thread.
enum ScanEvent {
    Progress(ProgressSnapshot),
    Finished(std::result::Result<DiskTree, String>),
}

/// Options of `fsight tui`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TuiOptions {
    pub sort: SortMode,
    pub apparent: bool,
    /// Use colors (bold and reverse video are used either way).
    pub colors: bool,
}

/// Runs the explorer until the user quits.
///
/// # Errors
///
/// Returns an error if stdout is not a terminal, `root` cannot be accessed,
/// or the terminal cannot be set up.
pub fn run(ctx: &Context<'_>, root: &Path, options: TuiOptions) -> Result<()> {
    if !io::stdout().is_terminal() {
        return Err(Error::Terminal(io::Error::other(
            "the explorer needs an interactive terminal (stdout is redirected)",
        )));
    }
    // Check the path before switching screens, so errors are printed normally.
    Walker::new(root, &ctx.walk, ctx.cancel, ctx.progress)?;

    install_panic_hook();
    let mut terminal = setup_terminal().map_err(Error::Terminal)?;
    let _guard = RestoreOnDrop;

    let mut app = App::new(root, options.sort);
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::scope(|scope| {
        scope.spawn(|| scan_in_background(ctx, root, options.apparent, &tx));
        let result = event_loop(&mut terminal, &mut app, &rx, options.colors);
        // Leaving (normally or with an error): stop the scan if it still runs,
        // so the scope can join the scanner thread quickly.
        ctx.cancel.store(true, Ordering::Relaxed);
        result
    })
    .map_err(Error::Terminal)
}

/// The scanner thread: builds the tree on a nested thread and reports
/// progress every [`TICK`] until it is done.
fn scan_in_background(ctx: &Context<'_>, root: &Path, apparent: bool, tx: &Sender<ScanEvent>) {
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| scan::build_tree(ctx, root, apparent, usize::MAX));
        while !handle.is_finished() {
            // A send error means the UI has quit; the walk stops via the cancel flag.
            let _ = tx.send(ScanEvent::Progress(ctx.progress.snapshot()));
            std::thread::sleep(TICK);
        }
        let result = match handle.join() {
            Ok(Ok(tree)) => Ok(tree),
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err("the scanner thread panicked".to_string()),
        };
        let _ = tx.send(ScanEvent::Finished(result));
    });
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    rx: &Receiver<ScanEvent>,
    colors: bool,
) -> io::Result<()> {
    while !app.should_quit {
        for message in rx.try_iter() {
            match message {
                ScanEvent::Progress(snapshot) => app.on_progress(snapshot),
                ScanEvent::Finished(result) => app.on_scan_finished(result),
            }
        }
        terminal.draw(|frame| ui::draw(frame, app, colors))?;
        // Wait for input, but not longer than one tick, so progress keeps updating.
        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    events::handle_key(app, key);
                }
                // A resize needs no handling: the next `draw` uses the new size.
                _ => {}
            }
        }
        app.tick();
    }
    Ok(())
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    terminal::enable_raw_mode()?;
    TERMINAL_ACTIVE.store(true, Ordering::SeqCst);
    execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
    Terminal::new(CrosstermBackend::new(io::stdout()))
}

/// Undoes `setup_terminal`, once. After a panic both the panic hook and the
/// drop guard call this; only the first call does anything, so the panic
/// message printed in between is not disturbed.
fn restore_terminal() -> io::Result<()> {
    // `swap` returns the previous value: only the caller that sees `true` restores.
    if !TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
        return Ok(());
    }
    terminal::disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, cursor::Show)
}

/// Restores the terminal when it goes out of scope — including when a `?`
/// returns early or a panic unwinds through `run`.
struct RestoreOnDrop;

impl Drop for RestoreOnDrop {
    fn drop(&mut self) {
        let _ = restore_terminal();
    }
}

/// Makes panics restore the terminal before the message is printed.
/// Without this, the message would land on the alternate screen and vanish.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        previous(info);
    }));
}
