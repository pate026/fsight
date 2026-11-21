//! State of the interactive explorer (implemented in phase 8).

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// How the entries of a directory are ordered in the explorer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum SortMode {
    /// Largest first.
    #[default]
    Size,
    /// Alphabetical.
    Name,
    /// Most recently modified first.
    Modified,
}
