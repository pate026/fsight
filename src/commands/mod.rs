//! The subcommands, and the glue that connects parsed arguments, the
//! configuration and the command logic.
//!
//! Each submodule exposes a `run` function that takes plain option structs
//! and returns a plain report struct. Nothing in those modules knows about
//! `clap`; this file translates between the two worlds.

pub mod dupes;
pub mod find;
pub mod grep;
pub mod scan;
pub mod stats;

use std::io::{self, Write};
use std::sync::atomic::AtomicBool;

use crate::cli::{Cli, Command};
use crate::config::Config;
use crate::error::Result;

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

/// Runs the subcommand selected on the command line.
///
/// # Errors
///
/// Returns an error if the command cannot run at all (bad path, invalid
/// pattern, output cannot be written, ...). Problems with individual files
/// are counted and reported as warnings instead.
pub fn run(cli: &Cli, _config: &Config, _cancel: &AtomicBool) -> Result<Outcome> {
    let name = match &cli.command {
        Some(Command::Scan(_)) => "scan",
        Some(Command::Find(_)) => "find",
        Some(Command::Grep(_)) => "grep",
        Some(Command::Dupes(_)) => "dupes",
        Some(Command::Stats(_)) => "stats",
        Some(Command::Tui(_)) => "tui",
        Some(Command::Completions(_)) => "completions",
        None => return Ok(Outcome::Success),
    };
    let _ = writeln!(io::stderr(), "fsight {name}: not implemented yet");
    Ok(Outcome::Success)
}
