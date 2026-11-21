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
