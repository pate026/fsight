//! `fsight find`: find files by name and metadata (implemented in phase 3).

use clap::ValueEnum;

/// The `-t/--type` filter of `find`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TypeFilter {
    /// Regular files.
    #[value(name = "f", alias = "file")]
    File,
    /// Directories.
    #[value(name = "d", aliases = ["dir", "directory"])]
    Dir,
    /// Symbolic links.
    #[value(name = "l", alias = "symlink")]
    Symlink,
}
