# Architecture

fsight is a library crate (`src/lib.rs` and everything below it) with a thin
binary (`src/main.rs`). This document explains how the pieces fit together.
For *why* things are the way they are, see [decisions.md](decisions.md).

## Module overview

```text
src/
├── main.rs            binary: parse args, logging, Ctrl-C, exit status
├── lib.rs             module tree, crate-wide lint settings
├── cli.rs             clap definitions (flags, subcommands, help texts)
├── config.rs          defaults < config file < environment < flags
├── error.rs           thiserror error types
├── walk.rs            the shared parallel directory walker
├── progress.rs        atomic progress counters + stderr spinner
├── size.rs            "10K" ⇄ 10240, "12.4 GiB", size filters
├── duration.rs        "2d" → Duration, ages, UTC timestamps
├── output/
│   ├── mod.rs         RenderText trait, text-or-JSON `emit`
│   ├── color.rs       color policy and the Palette
│   ├── tree.rs        scan tree rendering
│   └── bars.rs        meters, histogram bars, percentages
├── commands/
│   ├── mod.rs         glue: args + config → options → run → print
│   ├── scan.rs        DiskTree (arena) and the scan report
│   ├── find.rs        name/metadata matching
│   ├── grep.rs        parallel content search, ordered output
│   ├── dupes.rs       three-stage duplicate detection
│   └── stats.rs       statistics and histograms
└── tui/
    ├── mod.rs         terminal setup/teardown, scanner thread, event loop
    ├── app.rs         explorer state and transitions (pure)
    ├── events.rs      key → transition (pure)
    └── ui.rs          drawing with ratatui
```

Dependencies between modules point one way:

```text
main.rs ──▶ commands::run ──▶ scan / find / grep / dupes / stats / tui
                │                     │
                │                     ├──▶ walk ──▶ progress
                │                     ├──▶ size, duration
                │                     └──▶ output (rendering)
                └──▶ config ◀── cli
```

The command modules never import `cli`. They take plain option structs
(`ScanOptions`, `FindOptions`, …), which `commands/mod.rs` builds from the
parsed arguments and the merged configuration. So the logic does not depend on
how it is invoked, and tests can call it directly.

## Data flow of one command

```text
argv ─▶ clap (cli.rs) ─▶ Cli
                          │
config file ─┐            ▼
environment ─┼─▶ Config::load (config.rs) ─▶ Config
flags ───────┘            │
                          ▼
              commands::run (commands/mod.rs)
                          │  builds Context { walk options, cancel flag, progress }
                          ▼
            command::run(ctx, path, options)  ── on worker threads ──▶ Walker
                          │
                          ▼
                 Report (plain struct, derives Serialize)
                          │
              ┌───────────┴───────────┐
              ▼                       ▼
     RenderText (text, colors)   serde_json (--json)
              └───────────┬───────────┘
                          ▼
                 BufWriter<stdout>          warnings ─▶ stderr
                          │
                          ▼
           Outcome ─▶ main.rs ─▶ exit status 0 / 1 / 2 / 130
```

* **Reports are data.** `ScanReport`, `FindReport`, `GrepReport`,
  `DupeReport` and `StatsReport` derive `Serialize` and implement
  `RenderText`. `output::emit` picks JSON or text, so `--json` costs nothing
  per command.
* **Errors** are `fsight::Error` (thiserror) inside the library. `main.rs` wraps
  them in `anyhow` with context, prints the whole chain, and maps them to
  exit status 2. A broken pipe is recognised and ends the program quietly.
* **Problems with single entries** (permission denied, vanished files,
  symlink loops) are not errors. The walker counts them in an `ErrorLog`
  (atomic counter) and logs each one at warning level (`-v`). The command then
  prints one summary line on stderr.

## The shared walker

`walk::Walker` wraps `ignore::WalkParallel` (the walker inside ripgrep) and
exposes one method:

```rust
walker.run(map, consume) -> WalkSummary
// map:     Fn(&Entry) -> Option<T> + Sync   (runs on the worker threads)
// consume: FnMut(T)                         (runs on the calling thread)
```

```text
            ignore's worker threads                    calling thread
   ┌────────────────────────────────────┐          ┌───────────────────────┐
   │ read directory → for each entry:   │          │ for item in channel:  │
   │   check Ctrl-C flag                │ bounded  │     consume(item)     │
   │   count it for progress            │ channel  │                       │
   │   map(entry) ── Some(item) ───────────────────▶ (owns the result:     │
   │   errors → ErrorLog (atomic)       │ (4096)   │  tree, list, totals)  │
   └────────────────────────────────────┘          └───────────────────────┘
```

Each command chooses what is parallel (`map`) and what is collected (`consume`):

| Command | `map` (parallel) | `consume` (sequential) |
|---|---|---|
| scan, tui | `stat` the entry → `ScanItem` | `TreeBuilder::add` (arena tree, hard-link dedup) |
| find | type/extension/name checks, `stat` only if needed | push to a `Vec`, sorted at the end |
| grep | keep regular files (`stat` only for `--max-filesize`) | push to a `Vec`; the search happens afterwards |
| dupes | extension and size filters | push candidates; then the hashing stages |
| stats | `stat` → `StatItem` | `Accumulator::add` (counters, maps, top-k heaps) |

## Threading model

Everything uses *scoped threads* (`std::thread::scope`) or rayon. Threads
borrow data from the stack instead of needing `Arc`, and the compiler checks
that they finish before the borrowed data goes away.

* **Walking:** `ignore` runs N worker threads (`-j`, default: logical CPUs).
  The calling thread consumes results from a bounded crossbeam channel,
  which gives back-pressure.
* **grep:** after the walk, N scoped worker threads take files from a shared
  `AtomicUsize` cursor (`fetch_add` hands out each index exactly once). They
  send `(index, result)` over a channel. The calling thread keeps early results
  in a `BTreeMap` until all earlier files are done and then prints them in
  order. Output is sorted and streams, and a broken pipe stops the workers
  through a shared stop flag.
* **dupes:** stages 2 and 3 use rayon's `par_iter` (the global pool is sized
  with `-j` in `main.rs`). Groups are hashed in parallel and the files inside a
  group in parallel too.
* **Progress:** workers bump `AtomicU64` counters (`Ordering::Relaxed`, since
  they are independent statistics). A scoped spinner thread reads them every
  100 ms. It stops when its channel disconnects, which also happens if the
  work panics.
* **Cancellation:** `main.rs` creates an `Arc<AtomicBool>` and gives one
  clone to the Ctrl-C handler (which needs a `'static` value) and a reference
  to the commands. Walkers, grep workers and hashers check it and stop early.
  Commands report `interrupted: true`, and the exit status is 130.
* **TUI:** a scanner thread builds the tree (on a nested scoped thread) and
  sends `Progress` every 100 ms and `Finished` at the end over a channel. The
  UI thread drains the channel, draws, and waits at most 100 ms for a key.
  Quitting sets the cancel flag so the scan stops quickly.

```text
TUI threads
  ┌──────────── UI thread ────────────┐      ┌──── scanner thread ────┐
  │ loop {                            │      │ spawn build_tree ──────┼──▶ walker threads
  │   for msg in rx.try_iter() {..}   │◀─────│ every 100 ms: Progress │
  │   terminal.draw(ui::draw)         │      │ at the end:  Finished  │
  │   poll key (≤100 ms) → events.rs  │      └────────────────────────┘
  │ }                                 │
  └───────────────────────────────────┘
```

## Memory

* `scan` stores nodes only down to the display depth. Deeper entries are added
  to their ancestor's size. The TUI stores the full tree, like ncdu.
* `find` keeps matching paths only; `grep` keeps the list of files to search
  plus the results waiting for an earlier file; `stats` keeps counters and two
  bounded heaps; `dupes` keeps one record per candidate file.
* No command holds file contents. grep reads line by line (64 KiB buffer) and
  BLAKE3 hashes in 64 KiB chunks.

## Testing strategy

| Level | Where | What it covers |
|---|---|---|
| Unit tests | `#[cfg(test)] mod tests` in each module | parsing, matching rules, the three dupes stages (with fake hashers that count calls), tree building, histograms, config precedence (fake environment), TUI state, key handling and rendering (ratatui `TestBackend`) |
| Property tests | `size.rs`, `duration.rs` (proptest) | round trips, "never panics" for any input |
| Integration tests | `tests/cli_*.rs` | the real binary on generated fixture trees: flags, JSON, exit codes, hidden/ignore, symlink loops, hard links, sparse files, unreadable directories, invalid UTF-8, broken pipes, config precedence |
| Snapshot tests | `tests/snapshots/` (insta) | `scan` and `stats` text output and every `--help` screen |

The fixture helper (`tests/common/mod.rs`) gives every spawned `fsight` an
empty fake `$HOME` and `$XDG_CONFIG_HOME` and clears `FSIGHT_*`, so neither the
developer's config file nor a global gitignore can change test results.
