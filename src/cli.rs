//! Command-line interface, declared with `clap`'s derive API.
//!
//! The structs here only *describe* the command line. Turning the parsed
//! arguments into options for the command logic happens in `commands/mod.rs`
//! (together with the configuration file and environment variables).

// The `///` comments in this file are shown by clap as `--help` text, where
// Markdown backticks would appear literally, so `doc_markdown` does not apply.
#![allow(clippy::doc_markdown)]

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgAction, Args, Parser, Subcommand};

use crate::commands::find::TypeFilter;
use crate::duration::parse_duration;
use crate::output::color::ColorChoice;
use crate::size::{SizeFilter, parse_size, parse_size_filter};
use crate::tui::app::SortMode;

/// fsight: fast filesystem insight — disk usage, find, grep, duplicates and stats.
#[derive(Debug, Parser)]
#[command(name = "fsight", version, arg_required_else_help = true, after_help = MAIN_EXAMPLES)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    /// Print the effective configuration (defaults, config file, environment
    /// and flags merged) as TOML and exit
    #[arg(long)]
    pub print_config: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Flags shared by every subcommand. `global = true` lets them appear before
/// or after the subcommand name (`fsight --json scan` or `fsight scan --json`).
#[derive(Debug, Clone, Default, Args)]
#[command(next_help_heading = "Global options")]
// Each bool is an independent on/off command-line flag; grouping them into
// enums would make the clap definitions harder to read, not easier.
#[allow(clippy::struct_excessive_bools)]
pub struct GlobalArgs {
    /// Print results as JSON (pretty-printed unless --compact)
    #[arg(long, global = true)]
    pub json: bool,

    /// Print JSON on a single line (implies --json)
    #[arg(long, global = true)]
    pub compact: bool,

    /// Disable colors (same as --color never; NO_COLOR is also respected)
    #[arg(long, global = true, overrides_with = "color")]
    pub no_color: bool,

    /// When to use colors
    #[arg(
        long,
        global = true,
        value_enum,
        value_name = "WHEN",
        overrides_with = "no_color"
    )]
    pub color: Option<ColorChoice>,

    /// Number of worker threads [default: number of logical CPUs]
    #[arg(short = 'j', long, global = true, value_name = "N")]
    pub threads: Option<NonZeroUsize>,

    /// Include hidden files and directories
    #[arg(long, global = true, overrides_with = "no_hidden")]
    pub hidden: bool,

    /// Skip hidden files and directories (overrides `hidden = true` in the config)
    #[arg(long, global = true, overrides_with = "hidden")]
    pub no_hidden: bool,

    /// Do not respect .gitignore and .ignore files
    #[arg(long, global = true, overrides_with = "ignore")]
    pub no_ignore: bool,

    /// Respect .gitignore and .ignore files (overrides `no_ignore = true` in the config)
    #[arg(long, global = true, overrides_with = "no_ignore")]
    pub ignore: bool,

    /// Follow symbolic links (symlink loops are detected and skipped)
    #[arg(short = 'L', long, global = true)]
    pub follow: bool,

    /// Do not show progress output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Show more log output on stderr (-v warnings, -vv info, -vvv debug, -vvvv trace)
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
}

impl GlobalArgs {
    /// `Some(true)` for `--hidden`, `Some(false)` for `--no-hidden`, `None` if neither was given.
    pub fn hidden_flag(&self) -> Option<bool> {
        tri_state(self.hidden, self.no_hidden)
    }

    /// `Some(true)` for `--no-ignore`, `Some(false)` for `--ignore`, `None` if neither was given.
    pub fn no_ignore_flag(&self) -> Option<bool> {
        tri_state(self.no_ignore, self.ignore)
    }

    /// The color choice given on the command line, if any.
    pub fn color_flag(&self) -> Option<ColorChoice> {
        if self.no_color {
            Some(ColorChoice::Never)
        } else {
            self.color
        }
    }
}

/// Combines a `--flag` / `--no-flag` pair into an optional bool.
fn tri_state(yes: bool, no: bool) -> Option<bool> {
    match (yes, no) {
        (true, _) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
    }
}

/// The subcommands of fsight.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show a disk usage tree, largest entries first
    #[command(after_help = SCAN_EXAMPLES)]
    Scan(ScanArgs),

    /// Find files and directories by name, type, size and age
    #[command(after_help = FIND_EXAMPLES)]
    Find(FindArgs),

    /// Search file contents with a regular expression
    #[command(after_help = GREP_EXAMPLES)]
    Grep(GrepArgs),

    /// Report duplicate files (read-only: never deletes or changes anything)
    #[command(after_help = DUPES_EXAMPLES)]
    Dupes(DupesArgs),

    /// Show statistics: file types, size and age histograms, largest and oldest files
    #[command(after_help = STATS_EXAMPLES)]
    Stats(StatsArgs),

    /// Explore disk usage interactively (read-only)
    #[command(after_help = TUI_EXAMPLES)]
    Tui(TuiArgs),

    /// Print a shell completion script to stdout
    #[command(after_help = COMPLETIONS_EXAMPLES)]
    Completions(CompletionsArgs),
}

/// Arguments of `fsight scan`.
#[derive(Debug, Args)]
pub struct ScanArgs {
    /// Directory to analyze
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Number of levels to display below PATH; sizes always include the whole tree [default: 3]
    #[arg(short, long, value_name = "N")]
    pub depth: Option<usize>,

    /// Show only the N largest entries per directory, 0 for all [default: 10]
    #[arg(short = 'n', long, value_name = "N")]
    pub top: Option<usize>,

    /// Count file lengths instead of the disk space actually allocated
    #[arg(long)]
    pub apparent_size: bool,

    /// Hide entries smaller than SIZE (e.g. 200B, 10K, 5M, 1.5G)
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub min_size: Option<u64>,
}

/// Arguments of `fsight find`.
#[derive(Debug, Args)]
// Independent command-line switches, see `GlobalArgs`.
#[allow(clippy::struct_excessive_bools)]
pub struct FindArgs {
    /// Glob (or regex with --regex) matched against names; omit to list everything
    pub pattern: Option<String>,

    /// Directory to search
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Treat PATTERN as a regular expression instead of a glob
    #[arg(short, long)]
    pub regex: bool,

    /// Match case-sensitively (default: smart case — insensitive unless PATTERN has uppercase)
    #[arg(short = 's', long, overrides_with = "ignore_case")]
    pub case_sensitive: bool,

    /// Match case-insensitively
    #[arg(short = 'i', long, overrides_with = "case_sensitive")]
    pub ignore_case: bool,

    /// Only show entries of this type: f (file), d (directory), l (symlink); repeatable
    #[arg(short = 't', long = "type", value_enum, value_name = "TYPE")]
    pub types: Vec<TypeFilter>,

    /// Only show entries with this extension (case-insensitive); repeatable
    #[arg(short = 'e', long = "ext", value_name = "EXT")]
    pub extensions: Vec<String>,

    /// Only files of this size: +SIZE at least, -SIZE at most, SIZE exactly; repeatable
    #[arg(long, value_name = "[+|-]SIZE", allow_hyphen_values = true, value_parser = parse_size_filter)]
    pub size: Vec<SizeFilter>,

    /// Only entries modified within the last DURATION (e.g. 30m, 3h, 2d, 1w)
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    pub newer: Option<Duration>,

    /// Only entries modified more than DURATION ago
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    pub older: Option<Duration>,

    /// Descend at most N levels below PATH (1 = direct children only)
    #[arg(long, value_name = "N")]
    pub max_depth: Option<usize>,

    /// Match PATTERN against the whole displayed path instead of just the name
    #[arg(short = 'p', long)]
    pub full_path: bool,

    /// Long listing: size, modification time (UTC) and path
    #[arg(short, long)]
    pub long: bool,
}

/// Arguments of `fsight grep`.
#[derive(Debug, Args)]
// Independent command-line switches, see `GlobalArgs`.
#[allow(clippy::struct_excessive_bools)]
pub struct GrepArgs {
    /// Regular expression to search for (a literal string with -F)
    pub pattern: String,

    /// Directory or file to search
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Search case-insensitively
    #[arg(short = 'i', long)]
    pub ignore_case: bool,

    /// Only match whole words
    #[arg(short = 'w', long)]
    pub word_regexp: bool,

    /// Treat PATTERN as a literal string, not a regular expression
    #[arg(short = 'F', long)]
    pub fixed_strings: bool,

    /// Show N lines before and after each match
    #[arg(short = 'C', long, value_name = "N")]
    pub context: Option<usize>,

    /// Show N lines after each match
    #[arg(short = 'A', long, value_name = "N")]
    pub after_context: Option<usize>,

    /// Show N lines before each match
    #[arg(short = 'B', long, value_name = "N")]
    pub before_context: Option<usize>,

    /// Only print the number of matching lines in each file
    #[arg(short = 'c', long, conflicts_with = "files_with_matches")]
    pub count: bool,

    /// Only print the names of files that contain a match
    #[arg(short = 'l', long)]
    pub files_with_matches: bool,

    /// Only search files matching GLOB; prefix with ! to exclude; repeatable
    #[arg(short = 'g', long = "glob", value_name = "GLOB")]
    pub globs: Vec<String>,

    /// Skip files larger than SIZE (e.g. 10M)
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub max_filesize: Option<u64>,

    /// Search binary files too (files with a NUL byte in the first 8 KiB are skipped by default)
    #[arg(long)]
    pub binary: bool,
}

/// Arguments of `fsight dupes`.
#[derive(Debug, Args)]
pub struct DupesArgs {
    /// Directory to analyze
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Ignore files smaller than SIZE
    #[arg(long, value_name = "SIZE", value_parser = parse_size, default_value = "1")]
    pub min_size: u64,

    /// Only consider files with this extension (case-insensitive); repeatable
    #[arg(short = 'e', long = "ext", value_name = "EXT")]
    pub extensions: Vec<String>,

    /// Also report empty files (all empty files are identical)
    #[arg(long)]
    pub include_empty: bool,
}

/// Arguments of `fsight stats`.
#[derive(Debug, Args)]
pub struct StatsArgs {
    /// Directory to analyze
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

/// Arguments of `fsight tui`.
#[derive(Debug, Args)]
pub struct TuiArgs {
    /// Directory to explore
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Initial sort order [default: size]
    #[arg(long, value_enum, value_name = "ORDER")]
    pub sort: Option<SortMode>,
}

/// Arguments of `fsight completions`.
#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

const MAIN_EXAMPLES: &str = "\
Examples:
  fsight scan ~/Downloads        Disk usage tree, largest entries first
  fsight find '*.rs' -t f        Find Rust source files
  fsight grep -i 'todo|fixme'    Search file contents
  fsight dupes ~/Pictures        Report duplicate files (never deletes)
  fsight stats                   File types, sizes and ages at a glance
  fsight tui /                   Explore disk usage interactively
  fsight --print-config          Show the effective configuration

Hidden files and files ignored by .gitignore/.ignore are skipped unless
--hidden / --no-ignore is given. Defaults can be set in
$XDG_CONFIG_HOME/fsight/config.toml (usually ~/.config/fsight/config.toml).";

const SCAN_EXAMPLES: &str = "\
Examples:
  fsight scan                       Current directory, 3 levels, top 10 per directory
  fsight scan ~ -d 1                Only the direct children of your home directory
  fsight scan /var -n 5 --min-size 100M
                                    Five largest entries per directory, at least 100 MiB
  fsight scan . --hidden --no-ignore
                                    Include hidden and git-ignored files (like du)
  fsight scan --apparent-size       File lengths instead of allocated disk space
  fsight scan --json | jq '.tree.size'";

const FIND_EXAMPLES: &str = "\
Examples:
  fsight find '*.rs'                    Rust files (smart case: '*.rs' matches main.RS)
  fsight find report -t f               Files whose name contains 'report'
  fsight find -r '^test_.*\\.py$'        Regex instead of glob
  fsight find '*' ~/Videos --size +1G   Files of at least 1 GiB
  fsight find -e jpg -e png --newer 2d  Images changed in the last two days
  fsight find '*.log' --older 4w -l     Old log files with size and date
  fsight find -p '**/src/**/*.rs'       Match against the full path

A pattern without glob characters matches anywhere in the name.
Exit status: 0 if something was found, 1 if nothing matched, 2 on error.";

const GREP_EXAMPLES: &str = "\
Examples:
  fsight grep 'fn main'                 Search the current directory
  fsight grep -i error /var/log -C 2    Case-insensitive, two lines of context
  fsight grep -w TODO -g '*.rs'         Whole word, only in Rust files
  fsight grep -F 'a.b(c)' -l            Literal string, list matching files
  fsight grep unsafe -g '!target/**' -c Count matches, excluding target/

Binary files (a NUL byte in the first 8 KiB) are skipped unless --binary.
Exit status: 0 if a match was found, 1 if nothing matched, 2 on error.";

const DUPES_EXAMPLES: &str = "\
Examples:
  fsight dupes ~/Pictures               Duplicate files, most wasted space first
  fsight dupes . --min-size 1M          Ignore files smaller than 1 MiB
  fsight dupes -e jpg -e jpeg           Only JPEG files
  fsight dupes --json > dupes.json      Machine-readable report

Files are compared by size, then by a BLAKE3 hash of their first 16 KiB, then
by a full BLAKE3 hash. Hard links to the same file are not duplicates. This
command only reads files; it never deletes, moves or modifies anything.";

const STATS_EXAMPLES: &str = "\
Examples:
  fsight stats                    Statistics for the current directory
  fsight stats ~/Documents        Statistics for another directory
  fsight stats --hidden --json    Include hidden files, print JSON

Sizes in this report are file lengths (apparent sizes).";

const TUI_EXAMPLES: &str = "\
Examples:
  fsight tui                  Explore the current directory
  fsight tui / --sort name    Explore the whole system, sorted by name

Keys: ↑/↓ or j/k move, Enter/→/l open, Backspace/←/h go up, s sort,
/ filter, Esc clear filter, i info, ? help, q quit. Nothing is ever deleted.";

const COMPLETIONS_EXAMPLES: &str = "\
Examples:
  fsight completions bash > ~/.local/share/bash-completion/completions/fsight
  fsight completions zsh > ~/.zfunc/_fsight
  fsight completions fish > ~/.config/fish/completions/fsight.fish";

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_is_consistent() {
        // Catches conflicting flags, bad defaults etc. at test time instead of runtime.
        Cli::command().debug_assert();
    }

    #[test]
    fn global_flags_work_before_and_after_the_subcommand() {
        let before = Cli::try_parse_from(["fsight", "--json", "--hidden", "scan"]).unwrap();
        let after = Cli::try_parse_from(["fsight", "scan", "--json", "--hidden"]).unwrap();
        for cli in [before, after] {
            assert!(cli.global.json);
            assert_eq!(cli.global.hidden_flag(), Some(true));
        }
    }

    #[test]
    fn negation_flags_override_each_other_last_one_wins() {
        let cli = Cli::try_parse_from(["fsight", "--hidden", "--no-hidden", "stats"]).unwrap();
        assert_eq!(cli.global.hidden_flag(), Some(false));
        let cli =
            Cli::try_parse_from(["fsight", "--no-color", "--color", "always", "stats"]).unwrap();
        assert_eq!(cli.global.color_flag(), Some(ColorChoice::Always));
        let cli =
            Cli::try_parse_from(["fsight", "--color", "always", "--no-color", "stats"]).unwrap();
        assert_eq!(cli.global.color_flag(), Some(ColorChoice::Never));
        let cli = Cli::try_parse_from(["fsight", "stats"]).unwrap();
        assert_eq!(cli.global.no_ignore_flag(), None);
    }

    #[test]
    fn find_size_accepts_leading_minus_and_values_are_parsed() {
        let cli = Cli::try_parse_from([
            "fsight", "find", "--size", "-10K", "--size", "+1K", "--newer", "2d",
        ])
        .unwrap();
        let Some(Command::Find(args)) = cli.command else {
            panic!("expected the find subcommand");
        };
        assert_eq!(
            args.size,
            [SizeFilter::AtMost(10 * 1024), SizeFilter::AtLeast(1024)]
        );
        assert_eq!(args.newer, Some(Duration::from_hours(48)));
    }

    #[test]
    fn invalid_values_are_rejected_by_the_parser() {
        assert!(Cli::try_parse_from(["fsight", "scan", "--min-size", "10X"]).is_err());
        assert!(Cli::try_parse_from(["fsight", "find", "--newer", "soon"]).is_err());
        assert!(Cli::try_parse_from(["fsight", "-j", "0", "scan"]).is_err());
        assert!(Cli::try_parse_from(["fsight", "grep", "x", "-c", "-l"]).is_err());
    }
}
