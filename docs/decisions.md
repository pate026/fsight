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
