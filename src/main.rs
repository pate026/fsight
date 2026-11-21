//! The `fsight` binary: parse arguments, set up logging and Ctrl-C handling,
//! call into the library, and map the result to an exit status.
//!
//! Exit status: 0 success, 1 no matches (`find`/`grep`), 2 error,
//! 130 interrupted by Ctrl-C.

#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]

use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use clap::{CommandFactory, Parser};
use tracing_subscriber::filter::LevelFilter;

use fsight::cli::Cli;
use fsight::commands::{self, Outcome};
use fsight::config::{self, Config};
use fsight::output::color::{Palette, Role};

fn main() -> ExitCode {
    // On invalid arguments clap prints a message and exits with status 2.
    let cli = Cli::parse();
    init_logging(cli.global.verbose);

    match run(&cli) {
        Ok(outcome) => ExitCode::from(outcome.exit_code()),
        // `fsight find | head`: the reader went away. That is not an error.
        Err(err) if is_broken_pipe(&err) => ExitCode::SUCCESS,
        Err(err) => {
            let palette = Palette::new(stderr_colors(&cli));
            // `{:#}` prints the whole chain: "context: cause: root cause".
            let _ = writeln!(
                io::stderr(),
                "{} {err:#}",
                palette.paint(Role::Error, "error:")
            );
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> anyhow::Result<Outcome> {
    let config = Config::load(cli).context("invalid configuration")?;

    if cli.print_config {
        print_config(&config)?;
        return Ok(Outcome::Success);
    }
    if cli.command.is_none() {
        Cli::command()
            .error(
                clap::error::ErrorKind::MissingSubcommand,
                "a subcommand is required (try --help)",
            )
            .exit();
    }

    // Size rayon's global pool once; the walker and grep workers use the same count.
    rayon::ThreadPoolBuilder::new()
        .num_threads(config.threads)
        .build_global()
        .context("cannot start the worker thread pool")?;

    let cancel = Arc::new(AtomicBool::new(false));
    install_ctrlc_handler(Arc::clone(&cancel))?;

    Ok(commands::run(cli, &config, &cancel)?)
}

/// Prints the effective configuration, preceded by a comment saying which
/// config file (if any) contributed to it.
fn print_config(config: &Config) -> anyhow::Result<()> {
    let source = match config::config_path(&|name| std::env::var_os(name)) {
        Some(path) if path.is_file() => format!("# config file: {}\n", path.display()),
        Some(path) => format!("# config file: {} (not present)\n", path.display()),
        None => "# no config file location (HOME is not set)\n".to_string(),
    };
    let text = source
        + "# effective settings (defaults < config file < environment < flags)\n\n"
        + &config.to_toml()?;
    io::stdout()
        .write_all(text.as_bytes())
        .context("cannot write output")
}

/// Colors for our own error messages: honor `--color`/`--no-color` if given,
/// otherwise only on a terminal and without `NO_COLOR`.
fn stderr_colors(cli: &Cli) -> bool {
    let terminal = io::stderr().is_terminal();
    match cli.global.color_flag() {
        Some(choice) => choice.should_color(terminal),
        None => terminal && std::env::var_os("NO_COLOR").is_none(),
    }
}

/// First Ctrl-C: ask the workers to stop and print partial results.
/// Second Ctrl-C: exit immediately.
fn install_ctrlc_handler(cancel: Arc<AtomicBool>) -> anyhow::Result<()> {
    ctrlc::set_handler(move || {
        if cancel.swap(true, Ordering::SeqCst) {
            std::process::exit(130);
        }
    })
    .context("cannot install the Ctrl-C handler")
}

/// Sends `tracing` logs to stderr. Without `-v` only errors are shown.
fn init_logging(verbosity: u8) {
    let level = match verbosity {
        0 => LevelFilter::ERROR,
        1 => LevelFilter::WARN,
        2 => LevelFilter::INFO,
        3 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    };
    let use_ansi = io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    // `try_init` only fails if a logger is already installed, which cannot happen here.
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(io::stderr)
        .with_ansi(use_ansi)
        .with_target(verbosity >= 3)
        .without_time()
        .try_init();
}

/// Is any error in the chain a "broken pipe" while writing output?
fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<fsight::Error>()
            .is_some_and(fsight::Error::is_broken_pipe)
            || cause
                .downcast_ref::<io::Error>()
                .is_some_and(|io_err| io_err.kind() == io::ErrorKind::BrokenPipe)
    })
}
