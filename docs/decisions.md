# Design decisions

Choices made while building `fsight`, especially where the brief left room
for interpretation. Each entry says what was decided and why, so the owner can
review (and reverse) them deliberately. Entries are grouped by the phase in
which they were made.

## Phase 1 — Foundation

### D1. Library crate plus a thin binary
All logic lives in the library (`src/lib.rs` and its modules). `src/main.rs`
only parses arguments, sets up logging and Ctrl-C handling, calls
`commands::run`, and maps the outcome to an exit status. This keeps the logic
testable without spawning processes.

### D2. No `directories` crate
The config path is `$XDG_CONFIG_HOME/fsight/config.toml`, falling back to
`~/.config/fsight/config.toml`. That is a few lines with
`std::env::home_dir()`, which is no longer deprecated since Rust 1.87
(verified: it compiles warning-free with `-D warnings` on Rust 1.100). Using
std avoids a dependency and also gives macOS the same `~/.config` path the
brief specifies (the `directories` crate would pick `~/Library/Application
Support` there).

### D3. Own duration parser instead of `humantime`
`src/duration.rs` parses `30m`, `3h`, `2d`, `1w`, `1y` and combinations such as
`1h30m`. It is about 60 lines, gives specific error messages (for example
"use whole numbers, e.g. 90m instead of 1.5h"), and shows beginners how a
small parser is written. A year is 365 days; months are deliberately not
supported (`m` means minutes, and "a month" has no fixed length).

### D4. Timestamps are shown in UTC
`find --long`, `stats` and the TUI info panel show modification times as
`YYYY-MM-DD HH:MM` in **UTC**. Correct local time needs a time-zone database
(`jiff` or `chrono`), which is a heavy dependency for one column of output.
UTC is unambiguous, identical on every machine (which makes the output testable),
and computed with a 20-line, well-known algorithm (`civil_from_days`). JSON
output uses Unix seconds, which any tool can convert. *Owner may want to review:*
adding `jiff` later would be a small, local change in `src/duration.rs`.

### D5. Sizes: binary units, integer arithmetic, saturation
`1K` = 1024 bytes as the brief requires, and `K`, `KB` and `KiB` are all
accepted with the same meaning. Parsing and formatting use integer arithmetic
(`u128` for headroom) instead of `f64`. That keeps the results exact and avoids
lossy casts. Sizes above `u64::MAX` (16 EiB) saturate rather than fail, so
`parse_size(format_size(n))` round-trips for every `u64`, as the property tests
check.

### D6. Lean dependency features
* `ratatui` is used with `default-features = false, features = ["crossterm"]`.
  The defaults pull in the calendar widget and with it the `time` crate,
  which fsight does not need.
* `crossterm` is pinned to the version `ratatui` itself uses (0.29), so only
  one copy is compiled and the event types are the same on both sides.
* `tracing-subscriber` keeps its default `fmt` feature but not `env-filter`;
  verbosity comes from `-v` flags.

### D7. Exit status
`0` success, `1` no matches (`find` and `grep` only), `2` error (bad
arguments, missing path, invalid pattern, unwritable output), `130`
interrupted by Ctrl-C. Entries that cannot be read during a walk (permission
denied, vanished files) do **not** change the exit status. They are counted
and summarized on stderr instead. In large trees a few unreadable entries are
normal, and failing on them would make `fsight find ... && next-step`
unreliable.

### D8. Extra flags beyond the brief
* `--compact` implies `--json`, because `fsight scan --compact` has only one
  sensible meaning.
* `--color <auto|always|never>` complements `--no-color`. `always` is needed
  for `fsight grep x --color always | less -R`.
* `--no-hidden` and `--ignore` negate `--hidden` and `--no-ignore`.
  Without them, a config file that sets `hidden = true` could not be overridden
  from the command line, and the precedence "CLI > env > config" requires that.
* `-L` as the short form of `--follow`, matching fd and ripgrep.

### D9. Verbosity levels
No flag shows only errors. `-v` adds warnings (for example, every unreadable
entry with its path), `-vv` info, `-vvv` debug and `-vvvv` trace. When `-v` is
given, progress spinners are disabled so log lines and the spinner do not
garble each other on stderr.

### D10. `NO_COLOR` is an environment-level setting
Color precedence is `--color`/`--no-color`, then `FSIGHT_COLOR`, then
`NO_COLOR`, then `color` in the config file, then `auto`. The no-color.org
convention says config files should override `NO_COLOR`. The brief's general
rule (environment beats config file) is applied consistently instead.

### D11. Ctrl-C
The first Ctrl-C sets an `AtomicBool` that all workers check. They stop, and
fsight prints the partial results with a notice and exits with status 130.
A second Ctrl-C exits immediately.

### D12. Housekeeping
* The project brief (`fsight-prompt.md`) is committed. It documents the
  requirements this code was built against.
* No license file was added: choosing a license is the owner's decision.

## Phase 2 — Walker and scan

### D13. One walker, "parallel map, sequential fold"
`walk::Walker::run(map, consume)` runs `map` on the `ignore` crate's worker
threads and sends the results through a bounded crossbeam channel to `consume`,
which runs on the calling thread. Commands put the parallel work (metadata,
matching) in `map` and build their results in `consume` without any locks.
The bounded channel provides back-pressure, so memory cannot grow without limit.

### D14. `.gitignore` is honored outside git repositories
`ignore` normally applies `.gitignore` only inside a git repository
(`require_git(true)`, also ripgrep's and fd's default). fsight sets
`require_git(false)`: if a `.gitignore` is present, it is respected. This is
more predictable for a disk tool (a downloaded source tree behaves the same
whether or not it was cloned), and fixtures do not need `git init`.

### D15. `scan` follows the global filters, unlike `du`
The brief makes "skip hidden" and "respect ignore files" global defaults, so
`fsight scan` by default does **not** count hidden or git-ignored files (for
example `target/` in a Rust project). This differs from `du` and `dust`.
`fsight scan --hidden --no-ignore` gives the du-like total; the help text and
readme say so, and the config file can make it the default. *Owner may want to
review:* a scan-specific default could be argued for.

### D16. Apparent size counts file contents only
With `--apparent-size`, directories contribute 0 bytes of their own. A
directory's own length is a file-system detail (4096 on ext4, variable on
btrfs), not content. This keeps totals identical across file systems (and
therefore testable), and on this machine it equals `du -s --apparent-size`
exactly. In the default allocated mode, directories' own blocks are counted,
as `du` does.

### D17. Hard links: counted once, credited deterministically
A file with several hard links is counted once. Its size is credited to the
link whose relative path sorts first. "Whichever the walk saw first" would
change from run to run, because the parallel walk has no fixed order. With
`--follow`, every entry is deduplicated by (device, inode), because the same
file can then also be reached through different symlinks.

### D18. Memory stays proportional to the displayed tree
`scan` stores only nodes down to the display depth. Deeper entries are added
to their ancestor at that depth. The TUI asks for unlimited depth because the
user can navigate anywhere.

### D19. Verified against `du`
Allocated totals were compared with `du -s --block-size=1` on
`/usr/share/doc`, `/usr/lib64` (3.6 GB, 22 848 entries) and `/usr/include`,
using `--hidden --no-ignore`: identical to the byte.

### D20. Small presentation rules
* Percentages and bars round down, so a bar is only full at 100%. The root is
  always shown as 100%.
* The `(N other items)` row also contains entries hidden by `--min-size`. The
  row itself is hidden when its total is below `--min-size`.
* `--top 0` shows all children.
* Ties in size are broken by name, so the output is stable.

### D21. Extra flag `-x/--one-file-system`
Like `du -x`. Without it, `fsight scan /` or `fsight tui /` descends into
`/proc`, `/sys` and other virtual file systems.

### D22. `src/progress.rs` added to the module layout
It holds the atomic progress counters (updated by workers) and the
indicatif spinner (a scoped thread that shows them on stderr). The spinner
thread stops when a channel is disconnected, which happens even when the work
panics, so it can never be left running.

## Phase 3 — find

### D23. A plain word matches anywhere in the name
A glob without wildcards (`*?[{`) is treated as a substring. So `fsight find
report` finds `annual-report.pdf`, which is what people expect (it is also
fd's default behaviour). A real glob such as `report*` still has to match the
whole name.

### D24. Glob semantics are globset's
Globs are compiled by `globset` (as in ripgrep and fd) into a regex that
matches raw bytes, so file names that are not valid UTF-8 can still match.
Consequences: `?` matches one *byte* (so `?.txt` does not match `ä.txt`), and
case-insensitive globs fold ASCII letters only. Regexes (`--regex`) are
Unicode-aware. With `--full-path`, `*` does not cross `/` (use `**`), and the
pattern is matched against the path as displayed (for example `src/main.rs`
when searching `.`), not the absolute path.

### D25. Smart case ignores escaped characters
"Case-insensitive if the pattern is all lowercase" skips the character after
a backslash, so `\W` and `\D` (regex classes) do not make a pattern
case-sensitive. `\p{Lu}` would; this simple rule is good enough here and
easy to read.

### D26. `--size` is inclusive and applies to regular files only
`+SIZE` means "at least", `-SIZE` "at most", and a bare `SIZE` "exactly"
(fd's semantics). Directories and symlinks never pass a size filter, because
their "size" is not meaningful content. Several `--size` filters combine with
AND, so `--size +1M --size -10M` selects a range.

### D27. Results are collected and sorted, not streamed
`find` collects matches and prints them sorted by path. Output is then
deterministic (the parallel walk has no stable order) and matches the
"report struct, then render" architecture. The cost: nothing is printed until
the walk finishes. The spinner on stderr shows that work is happening.
Memory is proportional to the number of *matches* (paths only).

### D28. Extension filter
`-e` compares the end of the name ASCII-case-insensitively, so `-e tar.gz`
works. Dotfiles such as `.bashrc` have no extension. A leading dot in the
argument is optional (`-e .rs` = `-e rs`).

### D29. The search root is not a result
Like fd, `fsight find` lists what is *inside* PATH, not PATH itself. Paths
below `.` are shown without the `./` prefix.

### D30. JSON always includes size and modification time
In JSON mode, metadata is read for every match (one extra system call each),
so consumers always get `size` and `modified` (Unix seconds). In text mode
the system call is only made when a filter or `--long` needs it.

## Phase 4 — grep

### D31. Collect, then search in parallel with ordered, streaming output
`grep` first collects the list of files (shared walker, sorted), then N worker
threads claim files through a shared `AtomicUsize` cursor and send results,
tagged with the file's index, over a channel. The calling thread reorders them
with a small `BTreeMap` and prints each file as soon as all earlier files are
done. Benefits: deterministic output sorted by path (ripgrep's is unordered
unless `--sort`, which makes it single-threaded), output starts before the
search ends, and memory holds only results that wait for an earlier file.
Trade-off: if one early file is very slow (a huge log), later results queue up
behind it. The search also cannot start until the (fast, metadata-only) walk
has finished.

### D32. Buffered line-by-line reading, no memory mapping
Files are read through a 64 KiB `BufReader` with `read_until(b'\n')`. Only the
current line, the `-B` context lines (in a small ring buffer that reuses its
allocations) and the matching lines are kept. Memory mapping would need
`unsafe` (forbidden here) and offers little for line-oriented search. The
known limit: a single enormous line (for example a minified 50 MB file) is
held in memory while it is checked.

### D33. Binary detection without reading twice
The first 8 KiB are read into a buffer and checked for a NUL byte. The search
then reads `Cursor(head).chain(file)`, so those bytes are not read again.
Skipped binary files are logged at debug level (`-vvv`) and counted as
`binary_skipped` in the JSON output.

### D34. Flag semantics
* `-w` wraps the pattern as `\b(?:PATTERN)\b`. This is simpler than ripgrep's
  rule and behaves differently only for patterns that start or end with a
  non-word character.
* `-c` counts matching *lines* (like grep and rg). `-l` stops reading a file at
  its first match. `-c` and `-l` cannot be combined.
* `-A`/`-B` override `-C` for their side.
* `-g` uses ripgrep's override semantics (via `ignore::overrides`): globs are
  whitelists, and a leading `!` excludes.

### D35. Output format and progress
Output always uses the grouped "heading" format, also when piped. For
machine-readable output use `-c`, `-l` or `--json`. ripgrep switches to
`path:line:` when piped, but one stable format is easier to learn and test.
While results stream to a terminal, the spinner is hidden, because it would
garble the output. It is shown when stdout is redirected.

### D36. Verified against ripgrep
Counts of files and matching lines equal `rg -c -uu` on `/usr/include`
(`TODO|FIXME`) and `/usr/share/doc` (`-i -w error`: 1 707 files, 24 015 lines).

## Phase 5 — dupes

### D37. The three stages, and where hard links are handled
Exactly as the brief requires: (1) group by size, (2) BLAKE3 of the first
16 KiB, (3) full BLAKE3, with groups of one dropped after every stage.
Stages 2 and 3 share one generic function (`Stage::refine`) that splits groups
by a key computed in parallel with rayon. Files of at most 16 KiB skip
stage 3, because stage 2 already hashed them completely. Hard links are merged
in stage 1, before anything is read: a file and its hard links become one
"inode" with several names, shown as `(+1 hard link: ...)`, and they are never
counted as duplicates of each other.

### D38. The hash functions are parameters
`confirm_duplicates` takes the prefix and full hash functions as generic
parameters. The real ones read files, and the unit tests pass in-memory fakes
that count their calls. That is how the tests prove each stage really
eliminates candidates before the next, more expensive stage.

### D39. Counting rules
* "files" in the summary counts distinct files (inodes) in duplicate groups.
  Hard links are shown but not counted.
* "wasted" is `size × (copies − 1)` per group.
* Empty files are skipped unless `--include-empty`, whatever `--min-size` says.
  When included, all empty files form one group with 0 bytes wasted.
* Groups are sorted by wasted space, then size, then first path.

### D40. Parallel hashing on a hard disk
Hashing runs on all worker threads, as the brief asks. On an HDD, parallel
reads of different files cause seeks and can be slower than reading one file
at a time. `-j 1` gives sequential hashing. This is noted in the readme.

### D41. Verified against a naive implementation
A Python script that fully hashes every file and merges hard links found
the same groups, file counts and wasted bytes on `/usr/share/licenses`
(192 groups, 956 files, 15.2 MB) and `/usr/share/doc` (95, 253, 3.5 MB).

## Phase 6 — stats

### D42. What stats counts
* Sizes are file lengths (apparent size): the question is "what kinds of
  files are here". Use `scan` for disk usage.
* The root directory itself is not counted, only what is inside it.
* A file with several hard links is counted once, under the link whose path
  sorts first. Hard-linked files are held back until the walk ends to make
  that choice deterministic. Extra links appear as "(N extra hard links
  counted once)". This keeps every number consistent: histogram counts add up
  to "Files", and extension sizes add up to "Total size".
* Directories, symlinks and other entries are counted in the totals but not in
  the histograms or the extension table.

### D43. Buckets and labels
Size buckets have exclusive upper bounds (`<1K` is 0–1023 bytes, and `>1G`
starts at exactly 1 GiB), using the brief's labels. Age buckets use 1, 7, 30 and
365 days. Files whose modification time lies in the future (clock skew) count
as `<1 day`. Extensions are lowercased (`JPG` = `jpg`). Files without an
extension, including dotfiles, are grouped as `(none)`. Extensions beyond the
top 15 are combined into one "(N other extensions)" row.

### D44. Histograms
Bars are scaled so the fullest bucket fills 30 cells, using eighth-block
characters for smooth lengths. A non-zero bucket always shows at least a thin
sliver. Each table has a dim heading row. Percentages have one decimal and are
computed with integers.

### D45. Bounded memory
`stats` never keeps a list of all files. It keeps counters, a map of
extensions, and two "top 10" min-heaps (`TopK`), each costing O(log 10) per file.
