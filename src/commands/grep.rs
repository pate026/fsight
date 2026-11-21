//! `fsight grep`: search file contents in parallel.
//!
//! The work happens in two steps:
//!
//! 1. **Collect** the files to search with the shared walker (honoring hidden
//!    files, ignore files and `-g` globs), then sort them.
//! 2. **Search** them with N worker threads. Each worker takes the next file
//!    index from a shared [`AtomicUsize`] "cursor", searches the file, and sends
//!    the result (tagged with its index) over a channel. The calling thread
//!    puts results back in order and hands each one to a *sink* (which prints
//!    it, or collects it for JSON) as soon as all earlier files are done.
//!
//! The output is therefore sorted and identical on every run, it starts before
//! the search finishes, and memory holds only results that wait for an earlier,
//! slower file.
//!
//! Files are read line by line through a buffered reader; only matching lines
//! (and requested context lines) are kept, never whole files.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use regex::bytes::{Regex, RegexBuilder};
use serde::Serialize;

use super::Context;
use super::find::display_path;
use crate::error::{Error, Result};
use crate::output::RenderText;
use crate::output::color::{Palette, Role};
use crate::walk::{EntryKind, ErrorLog, WalkOptions, WalkSummary, Walker};

/// A file with a NUL byte in its first 8 KiB is considered binary.
const BINARY_PROBE_LEN: usize = 8 * 1024;

/// Size of the read buffer for each file.
const READ_BUFFER: usize = 64 * 1024;

/// How often (in lines) a long file checks whether Ctrl-C was pressed.
const CANCEL_CHECK_INTERVAL: u64 = 4096;

/// What to print for each file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrepMode {
    /// Matching lines, with line numbers and optional context.
    Lines,
    /// Only the number of matching lines (`-c`).
    Count,
    /// Only the file name (`-l`).
    FilesWithMatches,
}

/// Options of `fsight grep`.
#[derive(Debug, Clone, PartialEq, Eq)]
// Independent switches mirroring the command-line flags.
#[allow(clippy::struct_excessive_bools)]
pub struct GrepOptions {
    pub pattern: String,
    pub ignore_case: bool,
    pub word: bool,
    pub fixed: bool,
    /// Context lines before each match.
    pub before: usize,
    /// Context lines after each match.
    pub after: usize,
    pub mode: GrepMode,
    /// ripgrep-style globs (`-g`).
    pub globs: Vec<String>,
    pub max_filesize: Option<u64>,
    /// Search binary files too.
    pub binary: bool,
}

/// Builds the regex for `options`: `-F` escapes the pattern, `-w` wraps it in
/// word boundaries, `-i` makes it case-insensitive.
///
/// # Errors
///
/// Returns [`Error::Pattern`] if the regex is invalid.
pub fn build_regex(options: &GrepOptions) -> Result<Regex> {
    let mut pattern = if options.fixed {
        regex::escape(&options.pattern)
    } else {
        options.pattern.clone()
    };
    if options.word {
        pattern = format!(r"\b(?:{pattern})\b");
    }
    Ok(RegexBuilder::new(&pattern)
        .case_insensitive(options.ignore_case)
        .build()?)
}

/// A byte range inside [`Line::text`] that matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// Whether a printed line matched or is context around a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LineKind {
    Match,
    Context,
}

/// One printed line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Line {
    /// 1-based line number.
    pub number: u64,
    pub kind: LineKind,
    /// The line without its line terminator, converted lossily to UTF-8.
    pub text: String,
    /// Where the pattern matched (empty for context lines).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<Span>,
}

impl Line {
    fn context(number: u64, bytes: &[u8]) -> Self {
        Self {
            number,
            kind: LineKind::Context,
            text: String::from_utf8_lossy(bytes).into_owned(),
            matches: Vec::new(),
        }
    }

    /// A matching line. The text is converted to UTF-8 piece by piece
    /// (between and inside matches), so the spans stay correct even when
    /// invalid bytes are replaced by `�`.
    fn matched(number: u64, bytes: &[u8], regex: &Regex) -> Self {
        let mut text = String::with_capacity(bytes.len());
        let mut matches = Vec::new();
        let mut last = 0;
        for found in regex.find_iter(bytes) {
            if found.is_empty() {
                continue; // e.g. `^` or `x*`: matches, but there is nothing to highlight
            }
            text.push_str(&String::from_utf8_lossy(&bytes[last..found.start()]));
            let start = text.len();
            text.push_str(&String::from_utf8_lossy(found.as_bytes()));
            matches.push(Span {
                start,
                end: text.len(),
            });
            last = found.end();
        }
        text.push_str(&String::from_utf8_lossy(&bytes[last..]));
        Self {
            number,
            kind: LineKind::Match,
            text,
            matches,
        }
    }
}

/// The matches found in one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileMatches {
    pub path: String,
    /// Number of lines that matched.
    pub matched_lines: u64,
    /// The lines to print (empty with `-c` and `-l`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<Line>,
}

/// What searching one stream found (before it is attached to a path).
#[derive(Debug, Default, PartialEq, Eq)]
struct Found {
    matched_lines: u64,
    lines: Vec<Line>,
    bytes_read: u64,
}

/// Searches `reader` line by line.
///
/// Memory use is bounded by the longest line plus the `before` context lines;
/// the reader is never loaded as a whole.
fn search_lines<R: BufRead>(
    mut reader: R,
    regex: &Regex,
    options: &GrepOptions,
    cancel: &AtomicBool,
) -> io::Result<Found> {
    let mut found = Found::default();
    // The last `before` non-printed lines, kept in case a match follows.
    let mut before: VecDeque<(u64, Vec<u8>)> = VecDeque::with_capacity(options.before);
    let mut after_left = 0;
    let mut buffer = Vec::new();
    let mut number = 0u64;

    loop {
        buffer.clear();
        let read = reader.read_until(b'\n', &mut buffer)?;
        if read == 0 {
            break;
        }
        found.bytes_read += read as u64;
        number += 1;
        if number.is_multiple_of(CANCEL_CHECK_INTERVAL) && cancel.load(Ordering::Relaxed) {
            break;
        }
        let line = trim_line_end(&buffer);

        if regex.is_match(line) {
            found.matched_lines += 1;
            match options.mode {
                GrepMode::FilesWithMatches => break, // one match answers the question
                GrepMode::Count => continue,
                GrepMode::Lines => {}
            }
            found
                .lines
                .extend(before.drain(..).map(|(n, text)| Line::context(n, &text)));
            found.lines.push(Line::matched(number, line, regex));
            after_left = options.after;
        } else if options.mode == GrepMode::Lines {
            if after_left > 0 {
                after_left -= 1;
                found.lines.push(Line::context(number, line));
            } else if options.before > 0 {
                // Reuse the oldest buffer instead of allocating a new one per line.
                let mut slot = if before.len() == options.before {
                    before.pop_front().map_or_else(Vec::new, |(_, bytes)| bytes)
                } else {
                    Vec::new()
                };
                slot.clear();
                slot.extend_from_slice(line);
                before.push_back((number, slot));
            }
        }
    }
    Ok(found)
}

/// Strips a trailing `\n` or `\r\n`.
fn trim_line_end(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// The result of searching one file.
#[derive(Debug)]
enum FileOutcome {
    Matches(FileMatches, u64),
    NoMatch(u64),
    Binary,
    Failed(io::Error),
}

/// Opens `path` and searches it; `Ok(None)` means "skipped: binary".
fn open_and_search(
    path: &Path,
    regex: &Regex,
    options: &GrepOptions,
    cancel: &AtomicBool,
) -> io::Result<Option<Found>> {
    let mut file = File::open(path)?;
    // Read the first 8 KiB to look for a NUL byte...
    let mut head = Vec::with_capacity(BINARY_PROBE_LEN);
    (&mut file)
        .take(BINARY_PROBE_LEN as u64)
        .read_to_end(&mut head)?;
    if !options.binary && head.contains(&0) {
        return Ok(None);
    }
    // ...then search those bytes followed by the rest of the file, so
    // nothing is read twice.
    let reader = BufReader::with_capacity(READ_BUFFER, io::Cursor::new(head).chain(file));
    search_lines(reader, regex, options, cancel).map(Some)
}

/// Searches one file and classifies the outcome.
fn search_file(
    path: &Path,
    shown: String,
    regex: &Regex,
    options: &GrepOptions,
    cancel: &AtomicBool,
) -> FileOutcome {
    match open_and_search(path, regex, options, cancel) {
        Ok(None) => FileOutcome::Binary,
        Ok(Some(found)) if found.matched_lines == 0 => FileOutcome::NoMatch(found.bytes_read),
        Ok(Some(found)) => FileOutcome::Matches(
            FileMatches {
                path: shown,
                matched_lines: found.matched_lines,
                lines: found.lines,
            },
            found.bytes_read,
        ),
        Err(err) => FileOutcome::Failed(err),
    }
}

/// Totals of a grep run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct GrepSummary {
    pub files_searched: u64,
    pub files_with_matches: u64,
    pub matched_lines: u64,
    pub binary_skipped: u64,
    /// Unreadable entries during the walk plus files that failed to read.
    pub errors: u64,
    pub interrupted: bool,
}

impl GrepSummary {
    /// The part that is common to all commands.
    pub fn walk_summary(&self) -> WalkSummary {
        WalkSummary {
            errors: self.errors,
            interrupted: self.interrupted,
        }
    }
}

/// The result of `fsight grep` as one document (used for `--json`).
#[derive(Debug, Clone, Serialize)]
pub struct GrepReport {
    pub files: Vec<FileMatches>,
    #[serde(flatten)]
    pub summary: GrepSummary,
    /// How the files are rendered as text.
    #[serde(skip)]
    pub style: TextStyle,
}

/// Step 1: the files to search, sorted, plus the walk's summary.
///
/// # Errors
///
/// Returns an error if `root` cannot be accessed or a `-g` glob is invalid.
pub fn collect_files(
    ctx: &Context<'_>,
    root: &Path,
    options: &GrepOptions,
) -> Result<(Vec<PathBuf>, WalkSummary)> {
    let walk = WalkOptions {
        globs: options.globs.clone(),
        ..ctx.walk.clone()
    };
    let walker = Walker::new(root, &walk, ctx.cancel, ctx.progress)?;
    let mut files = Vec::new();
    let summary = walker.run(
        |entry| {
            if entry.kind() != EntryKind::File {
                return None;
            }
            // A let-chain (Rust 2024): only read metadata when a limit is set.
            if let Some(limit) = options.max_filesize
                && entry.metadata()?.len > limit
            {
                return None;
            }
            Some(entry.path().to_path_buf())
        },
        |path| files.push(path),
    );
    files.sort_unstable();
    Ok((files, summary))
}

/// Step 2: searches `files` in parallel and passes every file with matches to
/// `sink`, in the order of `files`.
///
/// # Errors
///
/// Returns [`Error::Output`] if `sink` fails (for example on a broken pipe).
pub fn search_files(
    ctx: &Context<'_>,
    root: &Path,
    files: &[PathBuf],
    regex: &Regex,
    options: &GrepOptions,
    sink: &mut dyn FnMut(FileMatches) -> io::Result<()>,
) -> Result<GrepSummary> {
    let cursor = AtomicUsize::new(0);
    // Set when the sink fails, so the workers stop too.
    let stop = AtomicBool::new(false);
    let errors = ErrorLog::default();
    let workers = ctx.walk.threads.clamp(1, files.len().max(1));
    let (tx, rx) = crossbeam_channel::bounded::<(usize, FileOutcome)>(workers * 4);
    let mut summary = GrepSummary::default();

    let sink_result = std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (cursor, stop) = (&cursor, &stop);
            scope.spawn(move || {
                while !stop.load(Ordering::Relaxed) && !ctx.cancel.load(Ordering::Relaxed) {
                    // Claim the next file. `fetch_add` returns the old value, so
                    // every index is handed out exactly once across all threads.
                    let index = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = files.get(index) else { break };
                    let shown = display_path(path, root).to_string_lossy().into_owned();
                    let outcome = search_file(path, shown, regex, options, ctx.cancel);
                    if tx.send((index, outcome)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx); // only the workers' clones remain; `rx` ends when they finish

        // Results arrive in any order; keep the early birds until it is their turn.
        let mut waiting: BTreeMap<usize, FileOutcome> = BTreeMap::new();
        let mut next = 0;
        for (index, outcome) in rx {
            waiting.insert(index, outcome);
            while let Some(outcome) = waiting.remove(&next) {
                let path = &files[next];
                next += 1;
                if let Err(err) = record(outcome, path, &mut summary, &errors, ctx, sink) {
                    stop.store(true, Ordering::Relaxed);
                    return Err(err);
                }
            }
        }
        Ok(())
    });

    summary.errors += errors.count();
    summary.interrupted = ctx.cancel.load(Ordering::Relaxed);
    sink_result.map_err(Error::Output)?;
    Ok(summary)
}

/// Updates the totals for one file and passes matches on to the sink.
fn record(
    outcome: FileOutcome,
    path: &Path,
    summary: &mut GrepSummary,
    errors: &ErrorLog,
    ctx: &Context<'_>,
    sink: &mut dyn FnMut(FileMatches) -> io::Result<()>,
) -> io::Result<()> {
    ctx.progress.add_items(1);
    match outcome {
        FileOutcome::Matches(file, bytes) => {
            ctx.progress.add_bytes(bytes);
            summary.files_searched += 1;
            summary.files_with_matches += 1;
            summary.matched_lines += file.matched_lines;
            sink(file)?;
        }
        FileOutcome::NoMatch(bytes) => {
            ctx.progress.add_bytes(bytes);
            summary.files_searched += 1;
        }
        FileOutcome::Binary => {
            summary.binary_skipped += 1;
            tracing::debug!("skipping binary file {}", path.display());
        }
        FileOutcome::Failed(err) => {
            errors.record(&format_args!("{}: {err}", path.display()));
        }
    }
    Ok(())
}

/// How grep results look as text (everything but the data itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextStyle {
    pub mode: GrepMode,
    /// Print `--` between non-adjacent groups of lines (when context is on).
    pub separators: bool,
}

/// Prints file results one at a time, ripgrep style: a heading with the path,
/// then `number:line` for matches and `number-line` for context lines.
pub struct Printer<'w> {
    out: &'w mut dyn Write,
    palette: Palette,
    style: TextStyle,
    printed_any: bool,
}

impl<'w> Printer<'w> {
    pub fn new(out: &'w mut dyn Write, palette: Palette, style: TextStyle) -> Self {
        Self {
            out,
            palette,
            style,
            printed_any: false,
        }
    }

    /// Prints one file's results.
    ///
    /// # Errors
    ///
    /// Returns any error from writing to the output.
    pub fn print(&mut self, file: &FileMatches) -> io::Result<()> {
        let palette = self.palette;
        let path = palette.paint(Role::Path, &file.path);
        match self.style.mode {
            GrepMode::FilesWithMatches => writeln!(self.out, "{path}")?,
            GrepMode::Count => writeln!(self.out, "{path}:{}", file.matched_lines)?,
            GrepMode::Lines => {
                if self.printed_any {
                    writeln!(self.out)?;
                }
                writeln!(self.out, "{path}")?;
                self.lines(&file.lines)?;
            }
        }
        self.printed_any = true;
        self.out.flush()
    }

    fn lines(&mut self, lines: &[Line]) -> io::Result<()> {
        let palette = self.palette;
        let mut previous: Option<u64> = None;
        for line in lines {
            if self.style.separators && previous.is_some_and(|p| line.number > p + 1) {
                writeln!(self.out, "{}", palette.paint(Role::Dim, "--"))?;
            }
            previous = Some(line.number);
            let separator = if line.kind == LineKind::Match {
                ':'
            } else {
                '-'
            };
            write!(
                self.out,
                "{}{separator}",
                palette.paint(Role::LineNumber, &line.number)
            )?;
            let mut last = 0;
            for span in &line.matches {
                write!(
                    self.out,
                    "{}{}",
                    &line.text[last..span.start],
                    palette.paint(Role::Match, &line.text[span.start..span.end])
                )?;
                last = span.end;
            }
            writeln!(self.out, "{}", &line.text[last..])?;
        }
        Ok(())
    }
}

impl RenderText for GrepReport {
    fn render_text(&self, out: &mut dyn Write, palette: Palette) -> io::Result<()> {
        let mut printer = Printer::new(out, palette, self.style);
        self.files.iter().try_for_each(|file| printer.print(file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(pattern: &str) -> GrepOptions {
        GrepOptions {
            pattern: pattern.to_string(),
            ignore_case: false,
            word: false,
            fixed: false,
            before: 0,
            after: 0,
            mode: GrepMode::Lines,
            globs: Vec::new(),
            max_filesize: None,
            binary: false,
        }
    }

    fn search(text: &str, options: &GrepOptions) -> Found {
        let regex = build_regex(options).unwrap();
        search_lines(text.as_bytes(), &regex, options, &AtomicBool::new(false)).unwrap()
    }

    /// `(number, kind)` pairs of the printed lines, for compact assertions.
    fn shape(found: &Found) -> Vec<(u64, LineKind)> {
        found.lines.iter().map(|l| (l.number, l.kind)).collect()
    }

    const TEXT: &str = "one\ntwo match\nthree\nfour\nfive match\nsix\nseven\neight\nnine match\n";

    #[test]
    fn finds_matching_lines_with_numbers_and_spans() {
        let found = search(TEXT, &options("match"));
        assert_eq!(found.matched_lines, 3);
        assert_eq!(found.lines[0].number, 2);
        assert_eq!(found.lines[0].text, "two match");
        assert_eq!(found.lines[0].matches, [Span { start: 4, end: 9 }]);
        assert_eq!(found.bytes_read, TEXT.len() as u64);
    }

    #[test]
    fn context_lines_before_and_after_without_duplicates() {
        use LineKind::{Context as C, Match as M};
        let opts = GrepOptions {
            before: 1,
            after: 1,
            ..options("match")
        };
        let found = search(TEXT, &opts);
        assert_eq!(
            shape(&found),
            [
                (1, C),
                (2, M),
                (3, C),
                (4, C),
                (5, M),
                (6, C),
                (8, C),
                (9, M)
            ]
        );
        // Overlapping context: line 3 is "after" line 2 and must not be repeated.
        let opts = GrepOptions {
            before: 2,
            after: 2,
            ..options("match")
        };
        let numbers: Vec<u64> = search(TEXT, &opts).lines.iter().map(|l| l.number).collect();
        assert_eq!(numbers, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn ignore_case_whole_word_and_fixed_strings() {
        let text = "Error here\nerrors everywhere\nan error.\n";
        let insensitive = GrepOptions {
            ignore_case: true,
            ..options("ERROR")
        };
        assert_eq!(search(text, &insensitive).matched_lines, 3);
        let word = GrepOptions {
            word: true,
            ..options("error")
        };
        assert_eq!(
            search(text, &word).matched_lines,
            1,
            "'errors' is not the word 'error'"
        );
        let fixed = GrepOptions {
            fixed: true,
            ..options("error.")
        };
        assert_eq!(
            search(text, &fixed).matched_lines,
            1,
            "the dot is literal with -F"
        );
        assert_eq!(
            search(text, &options("error.")).matched_lines,
            2,
            "as a regex it is not"
        );
    }

    #[test]
    fn count_and_files_with_matches_modes_do_not_store_lines() {
        let count = GrepOptions {
            mode: GrepMode::Count,
            ..options("match")
        };
        let found = search(TEXT, &count);
        assert_eq!((found.matched_lines, found.lines.len()), (3, 0));
        let files = GrepOptions {
            mode: GrepMode::FilesWithMatches,
            ..options("match")
        };
        let found = search(TEXT, &files);
        assert_eq!(found.matched_lines, 1, "stops at the first match");
        assert!(found.bytes_read < TEXT.len() as u64);
    }

    #[test]
    fn invalid_utf8_is_shown_lossily_with_correct_spans() {
        let regex = build_regex(&options("world")).unwrap();
        let line = Line::matched(1, b"hello \xff\xfe world", &regex);
        assert_eq!(line.text, "hello \u{fffd}\u{fffd} world");
        let span = line.matches[0];
        assert_eq!(&line.text[span.start..span.end], "world");
    }

    #[test]
    fn crlf_line_endings_are_trimmed_and_empty_matches_are_not_highlighted() {
        let found = search("a\r\nb\r\n", &options("^"));
        assert_eq!(found.matched_lines, 2);
        assert_eq!(found.lines[0].text, "a");
        assert_eq!(found.lines[0].matches, []);
    }

    /// A reader that produces many lines without ever holding them all.
    struct ManyLines {
        remaining: u64,
        tail: &'static [u8],
    }

    impl Read for ManyLines {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            const LINE: &[u8] = b"just an ordinary line of text without the magic word\n";
            if self.remaining == 0 {
                let n = self.tail.len().min(buf.len());
                buf[..n].copy_from_slice(&self.tail[..n]);
                self.tail = &self.tail[n..];
                return Ok(n);
            }
            let n = LINE.len().min(buf.len());
            buf[..n].copy_from_slice(&LINE[..n]);
            self.remaining -= 1;
            Ok(n)
        }
    }

    #[test]
    fn streams_large_inputs_line_by_line() {
        // About 27 MB of input, produced on the fly one line at a time. The
        // search only ever sees a `BufRead`, so it cannot depend on having the
        // whole input in memory; it holds one line (plus context) at a time.
        let reader = BufReader::new(ManyLines {
            remaining: 500_000,
            tail: b"the needle is here\n",
        });
        let opts = options("needle");
        let regex = build_regex(&opts).unwrap();
        let found = search_lines(reader, &regex, &opts, &AtomicBool::new(false)).unwrap();
        assert_eq!(found.matched_lines, 1);
        assert_eq!(found.lines[0].number, 500_001);
    }

    #[test]
    fn printer_uses_headings_line_numbers_and_separators() {
        let opts = GrepOptions {
            after: 1,
            ..options("match")
        };
        let found = search(TEXT, &opts);
        let file = FileMatches {
            path: "notes.txt".into(),
            matched_lines: found.matched_lines,
            lines: found.lines,
        };
        let mut out = Vec::new();
        let style = TextStyle {
            mode: GrepMode::Lines,
            separators: true,
        };
        let mut printer = Printer::new(&mut out, Palette::plain(), style);
        printer.print(&file).unwrap();
        printer.print(&file).unwrap();
        let text = String::from_utf8(out).unwrap();
        let expected_one =
            "notes.txt\n2:two match\n3-three\n--\n5:five match\n6-six\n--\n9:nine match\n";
        assert_eq!(text, format!("{expected_one}\n{expected_one}"));
    }
}
