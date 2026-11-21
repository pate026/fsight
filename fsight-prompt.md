# Project brief: `fsight` — a fast filesystem insight tool in Rust

You are building a complete, production-quality Rust command-line application from scratch in the current (empty) directory. Work autonomously through every phase below. Do not stop to ask questions: when something is unclear, choose the most reasonable option, record it in `docs/decisions.md`, and keep going.

The owner of this project is a beginner Rust learner. The code must be idiomatic and clean enough to learn from, and you will also write learning notes (see Phase 9).

---

## 1. What `fsight` is

`fsight` is a single binary that combines the best parts of `dust`, `fd`, `ripgrep` and `fdupes` into one tool, plus an interactive terminal UI. It analyzes a directory tree **in parallel** and answers questions like:

- What is taking up disk space?
- Where is the file I'm looking for?
- Which files contain this text?
- Which files are duplicates and how much space do they waste?
- What kinds of files live here and how old are they?

### Subcommands

| Command | Purpose |
|---|---|
| `fsight scan [PATH]` | Disk usage tree, sorted by size |
| `fsight find [PATTERN] [PATH]` | Find files by name and metadata filters |
| `fsight grep <REGEX> [PATH]` | Search file contents |
| `fsight dupes [PATH]` | Detect duplicate files (read-only, never deletes) |
| `fsight stats [PATH]` | Statistics: by extension, size buckets, age |
| `fsight tui [PATH]` | Interactive disk-usage explorer |
| `fsight completions <SHELL>` | Print shell completions (bash, zsh, fish) |

`PATH` defaults to the current directory everywhere.

### Global flags (apply to every subcommand)

- `--json` — machine-readable JSON output to stdout (one JSON document, pretty-printed unless `--compact`)
- `--compact` — compact JSON
- `--no-color` — disable colors (also respect the `NO_COLOR` env var and non-TTY stdout)
- `-j, --threads <N>` — number of worker threads (default: number of logical CPUs)
- `--hidden` — include hidden files (default: skip)
- `--no-ignore` — do not respect `.gitignore` / `.ignore` files (default: respect them)
- `--follow` — follow symlinks (default: do not; must detect and avoid symlink loops)
- `-q, --quiet` — no progress output
- `-v, --verbose` — repeatable, increases log level (use `tracing`, logs go to stderr)

---

## 2. Detailed feature specifications

### 2.1 `scan`

- Walk the tree in parallel and compute the total size of every directory (sum of descendants).
- Options:
  - `-d, --depth <N>` — how many levels to display (default 3). Sizes are always computed for the full tree.
  - `-n, --top <N>` — show only the N largest children per directory (default 10), rest collapsed into an `(N other items)` line.
  - `--apparent-size` — use file length instead of allocated disk blocks (default: allocated blocks via `std::os::unix::fs::MetadataExt::blocks() * 512` on Unix, fallback to length elsewhere).
  - `--min-size <SIZE>` — hide entries smaller than this (accepts `10K`, `5M`, `1.5G`, `200B`; binary units, 1K = 1024).
- Output: an indented tree with human-readable size, percentage of parent, and a small Unicode bar (e.g. `█████░░░░░`). Example:

  ```
   12.4 GiB 100%  ██████████  .
    8.1 GiB  65%  ██████░░░░  ├── videos
    3.0 GiB  24%  ██░░░░░░░░  ├── projects
  ```

- Hard links: count each inode only once (track `(dev, ino)` pairs).
- Errors (permission denied, vanished files) must **not** abort the scan. Count them and print a summary line on stderr at the end: `warning: 14 entries could not be read (run with -v for details)`.

### 2.2 `find`

- `PATTERN` is a glob by default (`*.rs`, `report-??.pdf`); `-r, --regex` makes it a regex. No pattern = match all.
- Case-insensitive by default if the pattern is all lowercase ("smart case"), otherwise case-sensitive. `-s` forces sensitive, `-i` forces insensitive.
- Filters (all combinable):
  - `-t, --type <f|d|l>` file / directory / symlink
  - `-e, --ext <EXT>` repeatable
  - `--size <+SIZE|-SIZE>` larger / smaller than
  - `--newer <DURATION>` / `--older <DURATION>` modified within / before e.g. `2d`, `3h`, `1w`, `30m`
  - `--max-depth <N>`
- `--full-path` matches against the whole path instead of only the file name.
- Output: one path per line, match highlighted in color. With `-l, --long`: size, modification time, path.

### 2.3 `grep`

- Regex search across file contents in parallel.
- Skip binary files (detect a NUL byte in the first 8 KiB) unless `--binary`.
- Options: `-i` ignore case, `-w` whole word, `-F` fixed string, `-C/-A/-B <N>` context lines, `-c` count per file, `-l` file names only, `-g <GLOB>` restrict to files matching glob (repeatable), `--max-filesize <SIZE>`.
- Output grouped by file, with colored file name, line numbers, and highlighted matches — similar to ripgrep's default output.
- Must handle very large files without loading the whole file into memory at once (read line-by-line with a buffered reader or use memory mapping with a clear justification in `docs/decisions.md`).
- Invalid UTF-8 must not crash the program; use lossy conversion for display.
- Exit codes: `0` = matches found, `1` = no matches, `2` = error. (Apply the same convention to `find`.)

### 2.4 `dupes`

- **Read-only. Never delete, move, or modify any file. There is no delete option.**
- Algorithm (must be implemented in exactly these stages for efficiency, and explained in code comments):
  1. Group files by exact size; discard groups of one. Skip empty files unless `--include-empty`.
  2. Within each group, hash the first 16 KiB with BLAKE3; discard unique partial hashes.
  3. Full BLAKE3 hash of the remaining candidates, in parallel.
- Hard links to the same inode are not duplicates of each other — report them separately or skip them.
- Options: `--min-size <SIZE>` (default 1 byte), `-e, --ext` filter.
- Output: groups sorted by wasted space descending; each group shows the size, number of copies, and paths. Final summary: `Found 37 duplicate groups, 112 files, 4.2 GiB wasted`.

### 2.5 `stats`

- Totals: files, directories, symlinks, total size.
- Top 15 extensions by total size (with count and percentage). Files with no extension grouped as `(none)`.
- Size buckets histogram: `<1K`, `1K–100K`, `100K–1M`, `1M–100M`, `100M–1G`, `>1G`.
- Age histogram by modification time: `<1 day`, `<1 week`, `<1 month`, `<1 year`, `older`.
- Largest 10 files and 10 oldest files.
- Render histograms as horizontal bar charts in the terminal.

### 2.6 `tui`

Interactive disk explorer built with `ratatui` + `crossterm`.

- Scan runs in a **background thread**; the UI shows a live progress indicator (files scanned, bytes counted) and stays responsive during the scan. Use channels to communicate between the scanner and the UI.
- Main view: list of the current directory's children with size, percentage, bar, and name; sorted by size (default) or name or modification time.
- Keybindings:
  - `↑/↓` or `j/k` move, `Enter`/`→`/`l` open directory, `Backspace`/`←`/`h` go up
  - `s` cycle sort mode, `/` filter by name (live), `Esc` clear filter
  - `i` show info panel for selected entry (full path, size, file count, modified time)
  - `?` help popup listing all keys, `q` or `Ctrl-C` quit
- Header shows the current path and total size; footer shows key hints.
- The terminal must always be restored correctly on exit, on error, **and on panic** (install a panic hook that restores the terminal before printing the panic).
- Must not crash when the terminal is resized to very small sizes.
- The TUI is read-only too: no deleting.

### 2.7 Configuration

- Optional config file at `$XDG_CONFIG_HOME/fsight/config.toml` (fallback `~/.config/fsight/config.toml`). Use the `directories` crate or equivalent.
- Can set defaults for: `threads`, `hidden`, `no_ignore`, `color`, `scan.depth`, `scan.top`, `tui.sort`.
- Precedence: CLI flags > environment variables (`FSIGHT_THREADS` etc.) > config file > built-in defaults.
- `fsight --print-config` prints the effective merged config as TOML.
- A malformed config file produces a clear error message with the file path and the parse error location.

### 2.8 Progress and UX

- Long operations show a spinner/progress on **stderr** using `indicatif`, only when stderr is a TTY and `--quiet`/`--json` is not set.
- `Ctrl-C` during a non-TUI operation stops cleanly and prints partial results notice (use an `AtomicBool` cancellation flag checked by workers; the `ctrlc` crate is fine).
- `--help` for every subcommand must be well written with examples (use clap's `after_help`).
- Broken pipe (e.g. `fsight find | head`) must exit quietly, not panic.

---

## 3. Technical requirements

### Language and tooling

- Latest stable Rust, **edition 2024**. Check the installed version with `rustc --version` first.
- No `unsafe` code. Add `#![forbid(unsafe_code)]` to `lib.rs` and `main.rs`.
- No `.unwrap()` / `.expect()` in non-test code, except `expect` with a message on truly impossible states (justify each in a comment).
- Add dependencies with `cargo add` so you get current versions. Do not hand-write version numbers from memory.

### Suggested crates (use these unless you have a strong documented reason not to)

- `clap` (derive feature) + `clap_complete` — CLI parsing and completions
- `ignore` — parallel directory walking with gitignore support (from the ripgrep project)
- `rayon` — data parallelism
- `crossbeam-channel` — channels between threads
- `regex`, `globset` — pattern matching
- `blake3` — hashing
- `serde` + `serde_json` + `toml` — serialization and config
- `ratatui` + `crossterm` — TUI
- `indicatif` — progress bars
- `owo-colors` or `anstream`/`anstyle` — colors (must respect `--no-color`, `NO_COLOR`, non-TTY)
- `thiserror` (library errors), `anyhow` (binary error handling)
- `tracing` + `tracing-subscriber` — logging
- `directories` — config paths
- `ctrlc` — cancellation
- `humantime` or own parser — durations
- Dev-dependencies: `assert_cmd`, `predicates`, `tempfile`, `proptest`, `insta` (snapshot tests)

### Architecture

Split into a library crate and a thin binary:

```
Cargo.toml
src/
  main.rs            # parse args, set up logging, call into lib, map errors to exit codes
  lib.rs             # public API, module declarations
  cli.rs             # clap definitions
  config.rs          # config loading and merging
  error.rs           # error types (thiserror)
  walk.rs            # shared parallel walker built on `ignore`, with filter options
  size.rs            # size parsing and human formatting
  duration.rs        # duration parsing ("2d", "3h")
  output/
    mod.rs           # Output trait / text vs json rendering
    color.rs
    tree.rs          # tree rendering for scan
    bars.rs          # bar and histogram rendering
  commands/
    mod.rs
    scan.rs
    find.rs
    grep.rs
    dupes.rs
    stats.rs
  tui/
    mod.rs           # terminal setup/teardown, panic hook, event loop
    app.rs           # app state (pure, testable without a terminal)
    ui.rs            # drawing
    events.rs        # key handling -> state transitions
tests/
  cli_scan.rs
  cli_find.rs
  cli_grep.rs
  cli_dupes.rs
  cli_stats.rs
  cli_config.rs
  common/mod.rs      # helpers to build temporary directory fixtures
docs/
  decisions.md
  progress.md
  learning-notes.md
  architecture.md
readme.md
```

Rules:

- Each command's core logic returns a **plain data structure** (e.g. `ScanReport`, `DupeReport`) that derives `Serialize`. Rendering to text or JSON is a separate step. This keeps logic testable and makes `--json` trivial.
- TUI state transitions (`app.rs`, `events.rs`) must be pure functions/methods that can be unit-tested without a real terminal.
- Keep functions small. Prefer iterators and pattern matching where they make code clearer, not cleverer.
- Use lowercase file and folder names everywhere except where tooling requires otherwise (`Cargo.toml`, `Cargo.lock`, `LICENSE`).

### Performance constraints

The development machine is modest: **8 GB RAM, HDD, Fedora Linux**. Therefore:

- Keep the dependency tree reasonable; do not add heavy crates for trivial tasks.
- Use `cargo check` for quick iteration; run full builds and tests at the end of each phase.
- Add to `Cargo.toml`:

  ```toml
  [profile.dev]
  debug = "line-tables-only"

  [profile.dev.package."*"]
  opt-level = 1

  [profile.release]
  lto = "thin"
  codegen-units = 1
  strip = true
  ```

- If builds run out of memory, use `cargo build -j 2`.
- Memory use of `fsight` itself must scale reasonably: do not hold file contents in memory; for `scan` store only what is needed for the tree.

---

## 4. Quality gates (run after EVERY phase)

Every phase is only done when all of these pass:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Also add to `lib.rs`: `#![warn(clippy::pedantic)]`, allowing specific pedantic lints only with a comment explaining why.

### Testing requirements

- **Unit tests** in each module for: size parsing/formatting, duration parsing, glob/regex matching rules (including smart case), the three-stage dupe algorithm, histogram bucketing, config precedence, TUI state transitions (navigation, sorting, filtering, going up from root).
- **Property tests** (`proptest`): `parse_size(format_size(n))` round-trips within rounding tolerance; size formatting never panics for any `u64`.
- **Integration tests** (`assert_cmd` + `tempfile`): build fixture directory trees programmatically (nested dirs, hidden files, `.gitignore`, symlinks, a symlink loop, hard links, binary files, invalid UTF-8 content, duplicate files of various sizes, empty files, an unreadable directory via permissions). Test every subcommand, every major flag, `--json` validity (parse it back with `serde_json`), and exit codes.
- **Snapshot tests** (`insta`) for text output of `scan`, `stats`, and `--help` screens. Make output deterministic in tests (fixed sort order, `--no-color`).
- Target: at least 80 meaningful tests in total. Quality over quantity — no trivial tests to pad the count.

---

## 5. Phases

Work through these in order. After each phase: run the quality gates, update `docs/progress.md` (what was done, test count, anything notable), and make a git commit with a clear message (`git init` first if needed, add a proper `.gitignore`).

1. **Foundation** — `cargo init`, dependencies, module skeleton, clap CLI with all subcommands and global flags (commands can print "not implemented yet"), `error.rs`, logging, `size.rs` and `duration.rs` with full tests, `.gitignore`, empty docs files.
2. **Walker + scan** — shared parallel walker with all filter options, hidden/ignore/follow handling, symlink loop protection, hard-link dedup, error counting, cancellation. Full `scan` with tree rendering and JSON.
3. **find** — all filters, smart case, glob/regex, highlighting, long format, exit codes.
4. **grep** — parallel content search, binary detection, context lines, all flags, broken-pipe safety.
5. **dupes** — three-stage algorithm, hard-link handling, sorted report, summary.
6. **stats** — all statistics and terminal histograms.
7. **config + UX polish** — config file, env vars, precedence, `--print-config`, progress bars, colors policy, completions, help texts with examples.
8. **tui** — background scan, full navigation, sorting, filter, info panel, help popup, panic-safe terminal restore, resize safety, state unit tests.
9. **Documentation** —
   - `readme.md`: what it is, install (`cargo install --path .`), every command with real example output, config reference, comparison with dust/fd/ripgrep.
   - `docs/architecture.md`: module overview, data flow diagram (ASCII or Mermaid), threading model.
   - `docs/learning-notes.md`: **written for a Rust beginner.** Explain, with pointers to the actual files and line ranges in this project, how the following concepts are used here: ownership and borrowing, `Result`/`?` and the thiserror vs anyhow split, traits and generics, iterators and closures, enums and pattern matching, modules and visibility, `Send`/`Sync` and why the threads compile safely, channels, `Arc`/atomics, lifetimes (wherever they appear), testing in Rust. End with 5 small suggested exercises to extend the project.
10. **CI + release** — `.github/workflows/ci.yml` running fmt, clippy, and tests on Linux and macOS. Verify `cargo build --release` works and report the binary size.
11. **Optional, only if everything above is green** — a small benchmark (`criterion` or a simple timed script in `docs/benchmarks.md`) comparing `fsight scan`/`find`/`grep` against `du`, `find`, and `grep` on a large directory such as `/usr`. Report honestly, including where `fsight` is slower.

---

## 6. Safety rules

- `fsight` must never write, modify, move, or delete any file it scans. The only file it may ever write is nothing — it is purely read-only. (Tests may create files inside temporary directories only.)
- While developing, only run `fsight` against the project directory, temporary test directories, or system directories in read-only fashion (e.g. `/usr`). Do not run anything destructive on the home directory.
- Do not install system packages or change system configuration. If a tool is missing, note it in `docs/decisions.md` and continue.

---

## 7. Definition of done

- All phases 1–10 complete, all quality gates green, all commits made.
- `cargo run --release -- scan .`, `find`, `grep`, `dupes`, `stats`, `tui`, and `completions bash` all work on the real machine.
- Finish with a short final report in the terminal:
  - what was built, number of tests, lines of code (`find src tests -name '*.rs' | xargs wc -l`), release binary size
  - any shortcuts, known limitations, or decisions the owner should review
  - three ideas for next steps
