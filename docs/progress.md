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
