//! `fsight find`: find entries by name and metadata.
//!
//! Matching happens on the walker's worker threads. Cheap checks (type,
//! extension, name) come first, and the metadata system call is only made
//! when a size or time filter, `--long` or `--json` needs it.
//!
//! Patterns: a glob by default, a regex with `--regex`. Both are compiled to
//! a [`regex::bytes::Regex`], which can match file names that are not valid
//! UTF-8 and tells us *where* it matched, for highlighting.

use std::io::{self, Write};
use std::ops::Range;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::ValueEnum;
use globset::GlobBuilder;
use regex::bytes::{Regex, RegexBuilder};
use serde::Serialize;

use super::Context;
use crate::duration::{format_timestamp, unix_seconds};
use crate::error::Result;
use crate::output::RenderText;
use crate::output::color::{Palette, Role};
use crate::size::{SizeFilter, format_size};
use crate::walk::{Entry, EntryKind, EntryMeta, WalkOptions, WalkSummary, Walker};

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

impl TypeFilter {
    fn matches(self, kind: EntryKind) -> bool {
        matches!(
            (self, kind),
            (TypeFilter::File, EntryKind::File)
                | (TypeFilter::Dir, EntryKind::Dir)
                | (TypeFilter::Symlink, EntryKind::Symlink)
        )
    }
}

/// How letter case is treated when matching names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseMode {
    /// Insensitive if the pattern has no uppercase letters, else sensitive.
    Smart,
    Sensitive,
    Insensitive,
}

/// How the pattern is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternSyntax {
    Glob,
    Regex,
}

/// Options of `fsight find`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindOptions {
    pub pattern: Option<String>,
    pub syntax: PatternSyntax,
    pub case: CaseMode,
    /// Match against the displayed path instead of the file name.
    pub full_path: bool,
    pub types: Vec<TypeFilter>,
    pub extensions: Vec<String>,
    pub sizes: Vec<SizeFilter>,
    pub newer: Option<Duration>,
    pub older: Option<Duration>,
    pub max_depth: Option<usize>,
    /// Long listing (size and modification time).
    pub long: bool,
    /// Read size and modification time even without `--long` (for JSON).
    pub with_metadata: bool,
}

impl Default for FindOptions {
    fn default() -> Self {
        Self {
            pattern: None,
            syntax: PatternSyntax::Glob,
            case: CaseMode::Smart,
            full_path: false,
            types: Vec::new(),
            extensions: Vec::new(),
            sizes: Vec::new(),
            newer: None,
            older: None,
            max_depth: None,
            long: false,
            with_metadata: false,
        }
    }
}

/// A compiled name pattern. `None` inside means "match everything".
#[derive(Debug, Clone)]
pub struct NameMatcher {
    regex: Option<Regex>,
}

impl NameMatcher {
    /// Compiles `pattern` (a glob or a regex) into a matcher.
    ///
    /// A glob without any wildcard characters (`*?[{`) matches anywhere in
    /// the name, so `fsight find report` finds `annual-report.pdf`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Pattern`] if the glob or regex is invalid.
    pub fn new(
        pattern: Option<&str>,
        syntax: PatternSyntax,
        case: CaseMode,
        full_path: bool,
    ) -> Result<Self> {
        let Some(pattern) = pattern.filter(|p| !p.is_empty()) else {
            return Ok(Self { regex: None });
        };
        let insensitive = match case {
            CaseMode::Sensitive => false,
            CaseMode::Insensitive => true,
            CaseMode::Smart => !has_uppercase_literal(pattern),
        };
        let source = match syntax {
            PatternSyntax::Regex => pattern.to_string(),
            PatternSyntax::Glob if !has_glob_wildcards(pattern) => regex::escape(pattern),
            PatternSyntax::Glob => GlobBuilder::new(pattern)
                // With --full-path, `*` stops at `/` and `**` crosses directories.
                .literal_separator(full_path)
                .build()?
                .regex()
                .to_string(),
        };
        let regex = RegexBuilder::new(&source)
            .case_insensitive(insensitive)
            .build()?;
        Ok(Self { regex: Some(regex) })
    }

    /// Does `text` match?
    pub fn is_match(&self, text: &[u8]) -> bool {
        self.regex.as_ref().is_none_or(|regex| regex.is_match(text))
    }

    /// Where the pattern matches in `text`, if it does and the range can be
    /// highlighted safely (both ends on UTF-8 character boundaries).
    pub fn find_in(&self, text: &str) -> Option<Range<usize>> {
        let found = self.regex.as_ref()?.find(text.as_bytes())?;
        let range = found.range();
        let printable = !range.is_empty()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end);
        printable.then_some(range)
    }
}

/// Smart case: does the pattern contain an uppercase letter?
///
/// The character after a backslash is skipped, because in a regex `\W`,
/// `\D` or `\S` are character classes, not uppercase letters.
fn has_uppercase_literal(pattern: &str) -> bool {
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if c.is_uppercase() {
            return true;
        }
    }
    false
}

fn has_glob_wildcards(pattern: &str) -> bool {
    pattern.contains(['*', '?', '[', '{'])
}

/// Does the file name end in `.ext`? `ext` must be lowercase ASCII without
/// the dot. Dotfiles such as `.bashrc` have no extension.
fn has_extension(name: &[u8], ext: &str) -> bool {
    let ext = ext.as_bytes();
    name.len() > ext.len() + 1
        && name[name.len() - ext.len() - 1] == b'.'
        && name[name.len() - ext.len()..].eq_ignore_ascii_case(ext)
}

/// Normalizes `-e` values: `.RS` and `rs` both become `rs`.
pub fn normalize_extensions(extensions: &[String]) -> Vec<String> {
    extensions
        .iter()
        .map(|ext| ext.trim_start_matches('.').to_ascii_lowercase())
        .filter(|ext| !ext.is_empty())
        .collect()
}

/// Paths below `.` are shown without the `./` prefix, like fd does.
pub fn display_path<'p>(path: &'p Path, root: &Path) -> &'p Path {
    if root == Path::new(".") {
        path.strip_prefix(".").unwrap_or(path)
    } else {
        path
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FoundEntry {
    /// The path as displayed (lossy UTF-8).
    pub path: String,
    #[serde(rename = "type")]
    pub kind: EntryKind,
    /// Size in bytes, when metadata was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Modification time in Unix seconds, when metadata was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<i64>,
    /// Byte range of `path` to highlight.
    #[serde(skip)]
    pub highlight: Option<Range<usize>>,
}

/// The result of `fsight find`.
#[derive(Debug, Clone, Serialize)]
pub struct FindReport {
    /// Matches, sorted by path.
    pub matches: Vec<FoundEntry>,
    #[serde(flatten)]
    pub summary: WalkSummary,
    /// Render the long format (size, time, path).
    #[serde(skip)]
    pub long: bool,
}

/// Everything `map` needs to judge one entry; shared by all worker threads.
struct Filter<'a> {
    matcher: NameMatcher,
    options: &'a FindOptions,
    extensions: Vec<String>,
    root: &'a Path,
    /// Entries modified at or after this instant pass `--newer`.
    newer_than: Option<SystemTime>,
    /// Entries modified before this instant pass `--older`.
    older_than: Option<SystemTime>,
}

impl Filter<'_> {
    fn needs_metadata(&self) -> bool {
        let o = self.options;
        o.long || o.with_metadata || !o.sizes.is_empty() || o.newer.is_some() || o.older.is_some()
    }

    /// The whole decision for one entry; `None` means "not a match".
    fn check(&self, entry: &Entry<'_>) -> Option<FoundEntry> {
        if entry.depth() == 0 {
            return None; // the root itself is not a search result
        }
        let kind = entry.kind();
        let types = &self.options.types;
        if !types.is_empty() && !types.iter().any(|t| t.matches(kind)) {
            return None;
        }
        let name = entry.file_name().as_encoded_bytes();
        if !self.extensions.is_empty() && !self.extensions.iter().any(|e| has_extension(name, e)) {
            return None;
        }
        let shown = display_path(entry.path(), self.root);
        let target = if self.options.full_path {
            shown.as_os_str().as_encoded_bytes()
        } else {
            name
        };
        if !self.matcher.is_match(target) {
            return None;
        }

        let meta = if self.needs_metadata() {
            let meta = entry.metadata()?;
            if !self.metadata_passes(&meta) {
                return None;
            }
            Some(meta)
        } else {
            None
        };

        let path = shown.to_string_lossy().into_owned();
        let highlight = self.highlight(&path, entry);
        Some(FoundEntry {
            kind: meta.as_ref().map_or(kind, |m| m.kind),
            size: meta.as_ref().map(|m| m.len),
            modified: meta.as_ref().and_then(|m| m.modified).map(unix_seconds),
            path,
            highlight,
        })
    }

    fn metadata_passes(&self, meta: &EntryMeta) -> bool {
        let sizes_ok = self.options.sizes.is_empty()
            || (meta.kind == EntryKind::File
                && self.options.sizes.iter().all(|f| f.matches(meta.len)));
        let newer_ok = self
            .newer_than
            .is_none_or(|cutoff| meta.modified.is_some_and(|m| m >= cutoff));
        let older_ok = self
            .older_than
            .is_none_or(|cutoff| meta.modified.is_some_and(|m| m < cutoff));
        sizes_ok && newer_ok && older_ok
    }

    /// The range of the displayed path to highlight: the match inside the
    /// file name (or inside the whole path with `--full-path`).
    fn highlight(&self, path: &str, entry: &Entry<'_>) -> Option<Range<usize>> {
        if self.options.full_path {
            return self.matcher.find_in(path);
        }
        let name = entry.file_name().to_string_lossy();
        let name_start = path.strip_suffix(name.as_ref())?.len();
        let range = self.matcher.find_in(&path[name_start..])?;
        Some(range.start + name_start..range.end + name_start)
    }
}

/// Runs `fsight find`.
///
/// # Errors
///
/// Returns an error if the pattern is invalid or `root` cannot be accessed.
pub fn run(ctx: &Context<'_>, root: &Path, options: &FindOptions) -> Result<FindReport> {
    let now = SystemTime::now();
    let cutoff = |age: Duration| now.checked_sub(age).unwrap_or(UNIX_EPOCH);
    let filter = Filter {
        matcher: NameMatcher::new(
            options.pattern.as_deref(),
            options.syntax,
            options.case,
            options.full_path,
        )?,
        options,
        extensions: normalize_extensions(&options.extensions),
        root,
        newer_than: options.newer.map(cutoff),
        older_than: options.older.map(cutoff),
    };

    let walk = WalkOptions {
        max_depth: options.max_depth,
        ..ctx.walk.clone()
    };
    let walker = Walker::new(root, &walk, ctx.cancel, ctx.progress)?;
    let mut matches = Vec::new();
    let summary = walker.run(|entry| filter.check(entry), |found| matches.push(found));

    // The parallel walk finds entries in no particular order; sort for stable output.
    matches.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    Ok(FindReport {
        matches,
        summary,
        long: options.long,
    })
}

impl RenderText for FindReport {
    fn render_text(&self, out: &mut dyn Write, palette: Palette) -> io::Result<()> {
        for entry in &self.matches {
            if self.long {
                let size = match (entry.kind, entry.size) {
                    (EntryKind::Dir, _) | (_, None) => "-".to_string(),
                    (_, Some(size)) => format_size(size),
                };
                let modified = entry.modified.map_or_else(|| "-".to_string(), timestamp);
                write!(out, "{size:>10}  {modified:16}  ")?;
            }
            write_highlighted(out, entry, palette)?;
            writeln!(out)?;
        }
        Ok(())
    }
}

fn timestamp(seconds: i64) -> String {
    let time = match u64::try_from(seconds) {
        Ok(secs) => UNIX_EPOCH + Duration::from_secs(secs),
        Err(_) => UNIX_EPOCH - Duration::from_secs(seconds.unsigned_abs()),
    };
    format_timestamp(time)
}

/// Writes the path with its matching part highlighted; directories and
/// symlinks get their usual color for the rest of the path.
fn write_highlighted(out: &mut dyn Write, entry: &FoundEntry, palette: Palette) -> io::Result<()> {
    let base = match entry.kind {
        EntryKind::Dir => Some(Role::Dir),
        EntryKind::Symlink => Some(Role::Symlink),
        EntryKind::File | EntryKind::Other => None,
    };
    let part = |out: &mut dyn Write, text: &str| match base {
        _ if text.is_empty() => Ok(()),
        Some(role) => write!(out, "{}", palette.paint(role, text)),
        None => write!(out, "{text}"),
    };
    match &entry.highlight {
        Some(range) => {
            part(out, &entry.path[..range.start])?;
            write!(
                out,
                "{}",
                palette.paint(Role::Match, &entry.path[range.clone()])
            )?;
            part(out, &entry.path[range.end..])
        }
        None => part(out, &entry.path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(pattern: &str, case: CaseMode) -> NameMatcher {
        NameMatcher::new(Some(pattern), PatternSyntax::Glob, case, false).unwrap()
    }

    fn regex(pattern: &str, case: CaseMode) -> NameMatcher {
        NameMatcher::new(Some(pattern), PatternSyntax::Regex, case, false).unwrap()
    }

    #[test]
    fn globs_match_whole_names() {
        let m = glob("*.rs", CaseMode::Smart);
        assert!(m.is_match(b"main.rs"));
        assert!(!m.is_match(b"main.rs.bak"));
        let m = glob("report-??.pdf", CaseMode::Smart);
        assert!(m.is_match(b"report-07.pdf"));
        assert!(!m.is_match(b"report-7.pdf"));
        assert!(glob("{a,b}.c", CaseMode::Smart).is_match(b"b.c"));
    }

    #[test]
    fn plain_words_match_anywhere_in_the_name() {
        let m = glob("report", CaseMode::Smart);
        assert!(m.is_match(b"annual-report.pdf"));
        assert!(!m.is_match(b"summary.pdf"));
        // Regex characters in a plain word are taken literally.
        assert!(glob("a+b", CaseMode::Smart).is_match(b"x-a+b-y"));
        assert!(!glob("a+b", CaseMode::Smart).is_match(b"aab"));
    }

    #[test]
    fn smart_case_is_insensitive_only_for_lowercase_patterns() {
        assert!(glob("*.rs", CaseMode::Smart).is_match(b"MAIN.RS"));
        assert!(!glob("*.RS", CaseMode::Smart).is_match(b"main.rs"));
        assert!(glob("*.RS", CaseMode::Smart).is_match(b"main.RS"));
        assert!(regex("readme", CaseMode::Smart).is_match(b"README.md"));
        assert!(!regex("README", CaseMode::Smart).is_match(b"readme.md"));
    }

    #[test]
    fn explicit_case_flags_override_smart_case() {
        assert!(!glob("*.rs", CaseMode::Sensitive).is_match(b"MAIN.RS"));
        assert!(glob("*.RS", CaseMode::Insensitive).is_match(b"main.rs"));
        assert!(regex("README", CaseMode::Insensitive).is_match(b"readme.md"));
    }

    #[test]
    fn escaped_regex_classes_do_not_count_as_uppercase() {
        assert!(!has_uppercase_literal(r"\W\d\S"));
        assert!(has_uppercase_literal(r"\WFoo"));
        // `\D` is "not a digit", so the pattern stays case-insensitive.
        assert!(regex(r"^x\D", CaseMode::Smart).is_match(b"XA"));
    }

    #[test]
    fn regexes_are_not_anchored_and_report_match_positions() {
        let m = regex(r"\d+", CaseMode::Smart);
        assert!(m.is_match(b"file-123.txt"));
        assert_eq!(m.find_in("file-123.txt"), Some(5..8));
        assert!(NameMatcher::new(Some("("), PatternSyntax::Regex, CaseMode::Smart, false).is_err());
    }

    #[test]
    fn full_path_globs_need_double_star_to_cross_directories() {
        let m =
            NameMatcher::new(Some("src/*.rs"), PatternSyntax::Glob, CaseMode::Smart, true).unwrap();
        assert!(m.is_match(b"src/main.rs"));
        assert!(!m.is_match(b"src/util/mod.rs"));
        let m = NameMatcher::new(
            Some("**/src/**/*.rs"),
            PatternSyntax::Glob,
            CaseMode::Smart,
            true,
        )
        .unwrap();
        assert!(m.is_match(b"src/util/mod.rs"));
        assert!(m.is_match(b"crates/core/src/lib.rs"));
    }

    #[test]
    fn empty_pattern_matches_everything() {
        for pattern in [None, Some("")] {
            let m = NameMatcher::new(pattern, PatternSyntax::Glob, CaseMode::Smart, false).unwrap();
            assert!(m.is_match(b"anything"));
            assert_eq!(m.find_in("anything"), None, "nothing to highlight");
        }
    }

    #[test]
    fn extensions_match_case_insensitively_but_not_dotfiles() {
        assert!(has_extension(b"photo.JPG", "jpg"));
        assert!(has_extension(b"archive.tar.gz", "tar.gz"));
        assert!(has_extension(b"archive.tar.gz", "gz"));
        assert!(!has_extension(b".bashrc", "bashrc"));
        assert!(!has_extension(b"jpg", "jpg"));
        assert!(!has_extension(b"notes.txt", "md"));
        assert_eq!(
            normalize_extensions(&[".RS".into(), "toml".into(), ".".into()]),
            ["rs", "toml"]
        );
    }

    #[test]
    fn highlights_never_split_a_character() {
        // `(?-u)` lets `.` match a single byte of the two-byte "ä".
        let m = regex("(?-u)^.", CaseMode::Sensitive);
        assert!(m.is_match("äx".as_bytes()));
        assert_eq!(m.find_in("äx"), None);
    }

    #[test]
    fn long_listing_shows_size_time_and_highlighted_path() {
        let report = FindReport {
            matches: vec![
                FoundEntry {
                    path: "docs/notes.txt".into(),
                    kind: EntryKind::File,
                    size: Some(2048),
                    modified: Some(1_700_000_000),
                    highlight: Some(5..10),
                },
                FoundEntry {
                    path: "docs".into(),
                    kind: EntryKind::Dir,
                    size: Some(4096),
                    modified: None,
                    highlight: None,
                },
            ],
            summary: WalkSummary::default(),
            long: true,
        };
        let mut out = Vec::new();
        report.render_text(&mut out, Palette::plain()).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "   2.0 KiB  2023-11-14 22:13  docs/notes.txt\n         -  -                 docs\n"
        );
        let mut colored = Vec::new();
        report
            .render_text(&mut colored, Palette::new(true))
            .unwrap();
        let colored = String::from_utf8(colored).unwrap();
        assert!(
            colored.contains("docs/\u{1b}["),
            "the match starts after 'docs/': {colored:?}"
        );
    }
}
