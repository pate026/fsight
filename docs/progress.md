# Progress log

One entry per phase: what was done, the test count, and anything notable.

## Phase 1 — Foundation

* `cargo init` (edition 2024, Rust 1.100), all runtime and dev dependencies
  added with `cargo add`, dev/release profiles from the brief.
* Module skeleton for the whole architecture (`cli`, `config`, `error`,
  `walk`, `size`, `duration`, `output/*`, `commands/*`, `tui/*`).
* Complete `clap` CLI: all subcommands, global flags (usable before or after
  the subcommand), value parsers for sizes and durations, and help texts with
  examples. Subcommands print "not implemented yet".
* `error.rs` (thiserror enums), logging via `tracing` to stderr, Ctrl-C flag,
  exit-status mapping in `main.rs`.
* `size.rs` and `duration.rs` fully implemented and tested, including
  property tests (round-trips, never panicking on arbitrary input).
* Output helpers: color policy and palette, bar and histogram rendering,
  JSON writer, digit grouping.
* Test helpers for the integration tests (`tests/common/mod.rs`) with an
  isolated environment (fake `$HOME`/`$XDG_CONFIG_HOME`).

**Tests:** 46 in total — 40 unit tests (7 of them property tests), 3 doc tests, 3 integration tests.

**Notes:** In this phase Cargo reports some dependencies as unused. They are
used from phase 2 onwards. The warnings are informational, and clippy still
exits successfully.

## Phase 2 — Walker and scan

* `walk.rs`: parallel walker on `ignore` with hidden, ignore-file,
  follow-symlinks, one-file-system, max-depth and glob options. Map on worker
  threads, fold on the calling thread over a bounded channel. Unreadable
  entries and symlink loops are counted and logged, and the Ctrl-C flag is
  checked for every entry.
* `progress.rs`: atomic counters plus an indicatif spinner on stderr (only on
  a TTY, not with `--quiet`, `--json` or `-v`).
* `commands/scan.rs`: arena-based `DiskTree` with out-of-order insertion,
  folding below the display depth, deterministic hard-link deduplication,
  one-pass size roll-up, and a report with top-N and min-size collapsing.
* `output/tree.rs`: aligned columns (size, percentage of parent, bar) and
  tree-drawing prefixes. JSON comes from the same report struct.
* Dispatcher in `commands/mod.rs`: options from flags and config, spinner,
  buffered stdout, and warnings on stderr ("N entries could not be read",
  "interrupted"), with exit status 130 after Ctrl-C.
* Checked manually: totals equal `du` on several `/usr` subtrees. Ctrl-C
  prints partial results and exits with 130.

**Tests:** 77 in total — 57 unit, 3 doc, 17 integration (14 for scan,
including 2 insta snapshots, an unreadable directory, symlink loops, hard links,
sparse files and non-UTF-8 names).

## Phase 3 — find

* `commands/find.rs`: glob (via globset) or regex patterns compiled to
  `regex::bytes::Regex`; smart case, `-s`/`-i`, substring matching for plain
  words, `--full-path` with literal separators; type, extension, size, newer,
  older and max-depth filters. Metadata is read only when needed.
* Output: one path per line, sorted, with the match highlighted (never inside a
  UTF-8 character). `--long` adds size and UTC time. JSON includes type, size
  and Unix mtime.
* Exit status 0/1/2. A broken pipe (`fsight find | head`) exits quietly with
  status 0, checked both by a test and manually on `/usr/share`.
* Checked manually: `fsight find '*' /usr/share` returns the same 109 570
  entries as GNU find with hidden paths excluded.

**Tests:** 104 in total — 68 unit, 3 doc, 33 integration (16 for find).

## Phase 4 — grep

* `commands/grep.rs`: file collection with the shared walker (`-g` globs,
  `--max-filesize`), parallel search with an atomic cursor and an in-order
  reorder buffer, a line-by-line `BufReader` search with before/after context
  (no duplicates), binary detection on the first 8 KiB, lossy UTF-8 display
  with exact match spans, and Ctrl-C checks inside long files.
* Modes: lines (ripgrep-style headings, `N:` matches, `N-` context, `--`
  separators), `-c` counts, `-l` file names. JSON report with spans and totals.
* Exit status 0/1/2; broken pipe exits quietly (tested).
* Checked manually: identical counts to ripgrep on two system directories.

**Tests:** 127 in total — 76 unit, 3 doc, 48 integration (15 for grep).

## Phase 5 — dupes

* `commands/dupes.rs`: read-only duplicate detection in the three required
  stages (size → BLAKE3 of the first 16 KiB → full BLAKE3), each stage in
  parallel with rayon. Hard links are merged before hashing and reported
  separately. Results sorted by wasted space, with the summary line
  `Found N duplicate groups, M files, X wasted`.
* Size, extension and empty-file filters; JSON report; unreadable files are
  counted as warnings; Ctrl-C stops hashing and reports partial results.
* Checked manually: identical results to a naive "hash everything" script on two
  system directories.

**Tests:** 140 in total — 82 unit, 3 doc, 55 integration (7 for dupes,
including a check that no file is modified).

## Phase 6 — stats

* `commands/stats.rs`: one walk folded into an accumulator. It produces totals
  by type, the top 15 extensions by size (with an "other" row), size and age
  histograms, and the 10 largest and 10 oldest files (bounded heaps).
  Hard-linked files are counted once, deterministically.
* Text rendering: aligned tables with heading rows and eighth-block bars;
  timestamps in UTC. JSON uses the same report struct.
* Insta snapshot of the full text report, built from a fixture with fixed
  sizes and dates. Sparse files fill the 100M–1G and >1G buckets without using
  disk space.

**Tests:** 152 in total — 90 unit, 3 doc, 59 integration (4 for stats, including a snapshot).

## Phase 7 — Configuration and UX polish

* `config.rs`: config file at `$XDG_CONFIG_HOME/fsight/config.toml` (fallback
  `~/.config/...`), `FSIGHT_*` environment variables and `NO_COLOR`, merged in
  layers (CLI > env > file > defaults). Clear errors with file path, line and
  column, or the variable name. Unknown keys are rejected.
* `--print-config` prints the merged settings as valid TOML, with a comment
  naming the config file.
* `fsight completions <shell>` (bash, zsh, fish, elvish, PowerShell), safe on
  a broken pipe.
* Red `error:` prefix; spinner checked on a pseudo-terminal; help texts with
  examples for every subcommand, under insta snapshot tests.

**Tests:** 169 in total — 98 unit, 3 doc, 68 integration (12 for config and
CLI, including 8 help-screen snapshots).

## Phase 8 — TUI

* `tui/app.rs`: pure state with navigation, three sort modes, live filter,
  info panel, help and status messages, plus a cached visible list and a pure
  scroll function.
* `tui/events.rs`: key mapping (arrows and vim keys, PgUp/PgDn, g/G, Enter/l,
  Backspace/h, s, /, Esc, i, ?, q, Ctrl-C) with a separate filter mode.
* `tui/ui.rs`: header (path, sizes, sort, filter), rows with size,
  percentage, bar and name, an info panel, a help popup, and scanning/failure
  screens. Saturating layout math.
* `tui/mod.rs`: background scanner thread with progress over a channel, a
  responsive event loop, terminal restore through a guard, a panic hook and an
  idempotent restore.
* Checked on a pseudo-terminal: navigation, sort, filter, info, help, resizes
  down to 1×1 and back, quitting, and an injected panic all leave the terminal
  restored.

**Tests:** 190 in total — 118 unit (20 for the TUI: state, keys, and rendering
on an in-memory terminal from 0×0 up), 3 doc, 69 integration.

## Phase 9 — Documentation

* `readme.md`: what fsight is, installation, every command with real output
  captured on this machine (including a TUI frame rendered by the real UI
  code), options tables, global flags, configuration reference, exit codes,
  JSON recipes, and an honest comparison with dust, fd, ripgrep and fdupes.
* `docs/architecture.md`: module map and dependency direction, data flow of a
  command, the shared walker (map/consume), the threading model of every
  command and of the TUI, memory use, testing strategy.
* `docs/learning-notes.md`: eleven sections for a Rust beginner (ownership,
  errors, traits and generics, iterators and closures, enums and matching,
  modules, Send/Sync, channels, Arc/atomics, lifetimes, testing), each
  pointing to exact files and line ranges, which a script checked. The
  Send/Sync section shows the real compiler error from an experiment, which
  was removed again afterwards. It ends with five exercises.
* Before writing, checked the brief's hard rules over all of `src/`: no
  `unwrap`/`expect`/`panic!` outside tests, no `unsafe`,
  `forbid(unsafe_code)` in both crate roots, and no file-writing calls
  outside tests.

**Tests:** 190 (unchanged; documentation only).

## Phase 10 — CI and release

* `.github/workflows/ci.yml`: fmt, clippy (`-D warnings`), tests and a release
  build on Ubuntu and macOS (YAML validated locally; GitHub runs it).
* `cargo build --release`: 2 min 08 s, peak memory about 400 MB, binary
  **4 670 016 bytes (4.5 MiB)**, stripped, thin LTO.
* Checked with `cargo run --release`: `scan .`, `find`, `grep`, `dupes`,
  `stats`, `tui` (driven on a pseudo-terminal) and `completions bash` all work
  on this machine.

**Tests:** 190 (unchanged).
