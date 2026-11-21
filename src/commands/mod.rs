//! The subcommands, and the glue that connects parsed arguments, the
//! configuration and the command logic.
//!
//! Each submodule exposes a `run` function that takes plain option structs
//! and returns a plain report struct. Nothing in those modules knows about
//! `clap`; this file translates between the two worlds, shows progress, and
//! writes the reports to stdout.

pub mod dupes;
pub mod find;
pub mod grep;
pub mod scan;
pub mod stats;

use std::io::{self, BufWriter, IsTerminal, Write};
use std::sync::atomic::AtomicBool;

use clap::CommandFactory;
use serde::Serialize;

use crate::cli::{Cli, Command, DupesArgs, FindArgs, GlobalArgs, GrepArgs, ScanArgs, StatsArgs};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::output::color::{Palette, Role};
use crate::output::{Format, RenderText, emit, format_count, plural};
use crate::progress::{Noun, Progress, spinner_enabled, with_spinner};
use crate::walk::{WalkOptions, WalkSummary};

/// How a command ended; mapped to the process exit status by `main.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Everything worked (and, for `find`/`grep`, something matched).
    Success,
    /// `find` or `grep` ran fine but found nothing.
    NoMatches,
    /// The user pressed Ctrl-C; partial results were printed.
    Interrupted,
}

impl Outcome {
    /// The conventional exit status: 0, 1, or 130 (128 + SIGINT).
    pub fn exit_code(self) -> u8 {
        match self {
            Outcome::Success => 0,
            Outcome::NoMatches => 1,
            Outcome::Interrupted => 130,
        }
    }
}

/// What the command logic needs from its environment.
///
/// The lifetime `'a` says that a `Context` borrows the cancellation flag and
/// the progress counters: it cannot outlive them.
pub struct Context<'a> {
    /// Which entries to visit.
    pub walk: WalkOptions,
    /// Set by the Ctrl-C handler; long-running loops check it and stop early.
    pub cancel: &'a AtomicBool,
    /// Counters for the progress spinner.
    pub progress: &'a Progress,
}

/// How reports are printed.
#[derive(Debug, Clone, Copy)]
struct Presentation {
    format: Format,
    palette: Palette,
    spinner: bool,
    /// Palette for warnings on stderr (colored only if stderr is a terminal).
    stderr_palette: Palette,
}

impl Presentation {
    fn new(global: &GlobalArgs, config: &Config) -> Self {
        let format = Format::from_flags(global.json, global.compact);
        let stdout_colors =
            !format.is_json() && config.color.should_color(io::stdout().is_terminal());
        Self {
            format,
            palette: Palette::new(stdout_colors),
            spinner: spinner_enabled(global.quiet, format.is_json(), global.verbose),
            stderr_palette: Palette::new(config.color.should_color(io::stderr().is_terminal())),
        }
    }
}

/// Runs the subcommand selected on the command line.
///
/// # Errors
///
/// Returns an error if the command cannot run at all (bad path, invalid
/// pattern, output cannot be written, ...). Problems with individual files
/// are counted and reported as warnings instead.
pub fn run(cli: &Cli, config: &Config, cancel: &AtomicBool) -> Result<Outcome> {
    let Some(command) = &cli.command else {
        return Ok(Outcome::Success);
    };
    let progress = Progress::default();
    let ctx = Context {
        walk: walk_options(&cli.global, config),
        cancel,
        progress: &progress,
    };
    let presentation = Presentation::new(&cli.global, config);

    match command {
        Command::Scan(args) => run_scan(&ctx, args, config, presentation),
        Command::Find(args) => run_find(&ctx, args, presentation),
        Command::Grep(args) => run_grep(&ctx, args, presentation),
        Command::Dupes(args) => run_dupes(&ctx, args, presentation),
        Command::Stats(args) => run_stats(&ctx, args, presentation),
        Command::Completions(args) => print_completions(args.shell),
        Command::Tui(_) => {
            let _ = writeln!(
                io::stderr(),
                "fsight: this subcommand is not implemented yet"
            );
            Ok(Outcome::Success)
        }
    }
}

/// The walk options shared by all commands, from flags and configuration.
fn walk_options(global: &GlobalArgs, config: &Config) -> WalkOptions {
    WalkOptions {
        hidden: config.hidden,
        respect_ignore: !config.no_ignore,
        follow_links: global.follow,
        one_file_system: global.one_file_system,
        max_depth: None,
        threads: config.threads,
        globs: Vec::new(),
    }
}

fn run_scan(
    ctx: &Context<'_>,
    args: &ScanArgs,
    config: &Config,
    presentation: Presentation,
) -> Result<Outcome> {
    let options = scan::ScanOptions {
        depth: config.scan.depth,
        top: config.scan.top,
        apparent: args.apparent_size,
        min_size: args.min_size.unwrap_or(0),
    };
    let report = with_progress(ctx, presentation, ("entry", "entries"), || {
        scan::run(ctx, &args.path, &options)
    })?;
    print_report(&report, presentation)?;
    Ok(finish(report.summary, Outcome::Success, presentation))
}

fn run_find(ctx: &Context<'_>, args: &FindArgs, presentation: Presentation) -> Result<Outcome> {
    let case = match (args.case_sensitive, args.ignore_case) {
        (true, _) => find::CaseMode::Sensitive,
        (false, true) => find::CaseMode::Insensitive,
        (false, false) => find::CaseMode::Smart,
    };
    let options = find::FindOptions {
        pattern: args.pattern.clone(),
        syntax: if args.regex {
            find::PatternSyntax::Regex
        } else {
            find::PatternSyntax::Glob
        },
        case,
        full_path: args.full_path,
        types: args.types.clone(),
        extensions: args.extensions.clone(),
        sizes: args.size.clone(),
        newer: args.newer,
        older: args.older,
        max_depth: args.max_depth,
        long: args.long,
        with_metadata: presentation.format.is_json(),
    };
    let report = with_progress(ctx, presentation, ("entry", "entries"), || {
        find::run(ctx, &args.path, &options)
    })?;
    print_report(&report, presentation)?;
    let outcome = if report.matches.is_empty() {
        Outcome::NoMatches
    } else {
        Outcome::Success
    };
    Ok(finish(report.summary, outcome, presentation))
}

fn run_grep(ctx: &Context<'_>, args: &GrepArgs, presentation: Presentation) -> Result<Outcome> {
    let mode = if args.count {
        grep::GrepMode::Count
    } else if args.files_with_matches {
        grep::GrepMode::FilesWithMatches
    } else {
        grep::GrepMode::Lines
    };
    let options = grep::GrepOptions {
        pattern: args.pattern.clone(),
        ignore_case: args.ignore_case,
        word: args.word_regexp,
        fixed: args.fixed_strings,
        before: args.before_context.or(args.context).unwrap_or(0),
        after: args.after_context.or(args.context).unwrap_or(0),
        mode,
        globs: args.globs.clone(),
        max_filesize: args.max_filesize,
        binary: args.binary,
    };
    let style = grep::TextStyle {
        mode,
        separators: options.before > 0 || options.after > 0,
    };
    // Compile first, so an invalid pattern fails before any walking.
    let regex = grep::build_regex(&options)?;

    let (files, walk) = with_progress(ctx, presentation, ("entry", "entries"), || {
        grep::collect_files(ctx, &args.path, &options)
    })?;
    ctx.progress.start_stage("Searching");

    let mut collected = Vec::new();
    let mut summary = if presentation.format.is_json() {
        grep::search_files(ctx, &args.path, &files, &regex, &options, &mut |file| {
            collected.push(file);
            Ok(())
        })?
    } else {
        let stdout = io::stdout();
        let mut out = BufWriter::new(stdout.lock());
        let mut printer = grep::Printer::new(&mut out, presentation.palette, style);
        // Results stream to stdout while the search runs; a spinner on the
        // same terminal would garble them, so only show it when redirected.
        let spinner = presentation.spinner && !io::stdout().is_terminal();
        with_spinner(spinner, ctx.progress, ("file", "files"), || {
            grep::search_files(ctx, &args.path, &files, &regex, &options, &mut |file| {
                printer.print(&file)
            })
        })?
    };
    summary.errors += walk.errors;
    summary.interrupted |= walk.interrupted;

    if presentation.format.is_json() {
        let report = grep::GrepReport {
            files: collected,
            summary,
            style,
        };
        print_report(&report, presentation)?;
    }
    let outcome = if summary.files_with_matches > 0 {
        Outcome::Success
    } else {
        Outcome::NoMatches
    };
    Ok(finish(summary.walk_summary(), outcome, presentation))
}

fn run_dupes(ctx: &Context<'_>, args: &DupesArgs, presentation: Presentation) -> Result<Outcome> {
    let options = dupes::DupesOptions {
        min_size: args.min_size,
        extensions: args.extensions.clone(),
        include_empty: args.include_empty,
    };
    let report = with_progress(ctx, presentation, ("file", "files"), || {
        dupes::run(ctx, &args.path, &options)
    })?;
    print_report(&report, presentation)?;
    Ok(finish(report.summary, Outcome::Success, presentation))
}

fn run_stats(ctx: &Context<'_>, args: &StatsArgs, presentation: Presentation) -> Result<Outcome> {
    let report = with_progress(ctx, presentation, ("entry", "entries"), || {
        stats::run(ctx, &args.path)
    })?;
    print_report(&report, presentation)?;
    Ok(finish(report.summary, Outcome::Success, presentation))
}

/// Prints a completion script for `shell`.
fn print_completions(shell: clap_complete::Shell) -> Result<Outcome> {
    // Generate into memory first: clap_complete panics if writing fails,
    // and `fsight completions bash | head` must not panic.
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), "fsight", &mut script);
    let mut out = io::stdout().lock();
    out.write_all(&script)
        .and_then(|()| out.flush())
        .map_err(Error::Output)?;
    Ok(Outcome::Success)
}

/// Runs `work` with a spinner if the presentation allows one.
fn with_progress<R>(
    ctx: &Context<'_>,
    presentation: Presentation,
    noun: Noun,
    work: impl FnOnce() -> R,
) -> R {
    with_spinner(presentation.spinner, ctx.progress, noun, work)
}

/// Writes a report to stdout (buffered) in the selected format.
fn print_report<T: Serialize + RenderText>(report: &T, presentation: Presentation) -> Result<()> {
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    emit(&mut out, report, presentation.format, presentation.palette).map_err(Error::Output)?;
    out.flush().map_err(Error::Output)
}

/// Prints the end-of-run warnings and picks the final outcome.
fn finish(summary: WalkSummary, outcome: Outcome, presentation: Presentation) -> Outcome {
    let palette = presentation.stderr_palette;
    let mut stderr = io::stderr().lock();
    // Failing to print a warning is not worth failing the command for.
    if summary.errors > 0 {
        let _ = writeln!(
            stderr,
            "{} {} {} could not be read (run with -v for details)",
            palette.paint(Role::Warning, "warning:"),
            format_count(summary.errors),
            plural(summary.errors, "entry", "entries"),
        );
    }
    if summary.interrupted {
        let _ = writeln!(
            stderr,
            "{} interrupted — the results above are incomplete",
            palette.paint(Role::Warning, "warning:"),
        );
        return Outcome::Interrupted;
    }
    outcome
}
