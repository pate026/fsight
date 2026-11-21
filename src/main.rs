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
use fsight::config::Config;

fn main() -> ExitCode {
    // On invalid arguments clap prints a message and exits with status 2.
    let cli = Cli::parse();
    init_logging(cli.global.verbose);

    match run(&cli) {
        Ok(outcome) => ExitCode::from(outcome.exit_code()),
        // `fsight find | head`: the reader went away. That is not an error.
        Err(err) if is_broken_pipe(&err) => ExitCode::SUCCESS,
        Err(err) => {
            // `{:#}` prints the whole chain: "context: cause: root cause".
            let _ = writeln!(io::stderr(), "error: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> anyhow::Result<Outcome> {
    let config = Config::load(cli).context("invalid configuration")?;

    if cli.print_config {
        let text = toml::to_string(&config).context("cannot serialize the configuration")?;
        io::stdout()
            .write_all(text.as_bytes())
            .context("cannot write output")?;
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
