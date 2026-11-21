//! `fsight` — fast filesystem insight.
//!
//! This library contains everything the `fsight` binary does; `main.rs` only
//! parses arguments, sets up logging and maps errors to exit codes.
//!
//! Module map:
//!
//! * [`cli`] — command-line definitions (`clap`).
//! * [`config`] — defaults, config file, environment variables and flags merged.
//! * [`walk`] — the shared parallel directory walker.
//! * [`commands`] — `scan`, `find`, `grep`, `dupes` and `stats`.
//! * [`tui`] — the interactive explorer.
//! * [`output`] — text and JSON rendering, colors, bars.
//! * [`progress`] — progress counters and the stderr spinner.
//! * [`size`], [`duration`] — parsing and formatting helpers.
//! * [`error`] — error types.

#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
// `must_use_candidate` asks for `#[must_use]` on nearly every function that
// returns a value. In an application crate that is noise, not safety.
#![allow(clippy::must_use_candidate)]

pub mod cli;
pub mod commands;
pub mod config;
pub mod duration;
pub mod error;
pub mod output;
pub mod progress;
pub mod size;
pub mod tui;
pub mod walk;

pub use error::{Error, Result};
