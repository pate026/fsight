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
