# fsight

**fsight** is a fast, read-only filesystem insight tool: one binary that
combines the most useful parts of `dust` (disk usage), `fd` (find),
`ripgrep` (content search) and `fdupes` (duplicates), plus file statistics and
an interactive terminal explorer. Every command walks the directory tree in
parallel and can print JSON.

It answers questions like:

* What is taking up disk space? → `fsight scan`, `fsight tui`
* Where is the file I'm looking for? → `fsight find`
* Which files contain this text? → `fsight grep`
* Which files are duplicates, and how much space do they waste? → `fsight dupes`
* What kinds of files live here, and how old are they? → `fsight stats`

fsight never writes, moves or deletes anything it scans. There is no delete
option anywhere, not even in `dupes` or the TUI.

## Contents

* [Install](#install)
* [Commands](#commands): [scan](#fsight-scan) · [find](#fsight-find) ·
  [grep](#fsight-grep) · [dupes](#fsight-dupes) · [stats](#fsight-stats) ·
  [tui](#fsight-tui) · [completions](#fsight-completions)
* [Global flags](#global-flags) · [Configuration](#configuration) ·
  [Exit status](#exit-status) · [JSON output](#json-output)
* [Comparison with dust, fd, ripgrep and fdupes](#comparison)
* [Development](#development)

## Install

You need a recent stable Rust toolchain (fsight was developed with Rust 1.100,
edition 2024).

```sh
cargo install --path .
```

This builds an optimized binary and copies it to `~/.cargo/bin/fsight`. To
try it without installing, use `cargo run --release -- scan .`.

Shell completions (optional):

```sh
fsight completions bash > ~/.local/share/bash-completion/completions/fsight
fsight completions zsh  > ~/.zfunc/_fsight          # with fpath+=~/.zfunc
fsight completions fish > ~/.config/fish/completions/fsight.fish
```

## Commands

Every command takes an optional `PATH` (default: the current directory) and
the [global flags](#global-flags). `fsight <command> --help` shows all options
with examples. The outputs below are real, captured on the development machine.

**Hidden files and files matched by `.gitignore`/`.ignore` are skipped by
default.** Add `--hidden` and/or `--no-ignore` to include them. For
example, `fsight scan --hidden --no-ignore` gives the same totals as `du`.

### `fsight scan`

A disk usage tree, largest entries first. Each line shows the size, the
percentage of the parent directory, and a bar.

```text
$ fsight scan /usr -d 1 -n 6 -x
  9.7 GiB 100%  ██████████  /usr
  3.4 GiB  35%  ███░░░░░░░  ├── lib64
  3.2 GiB  32%  ███░░░░░░░  ├── share
  2.2 GiB  22%  ██░░░░░░░░  ├── lib
646.7 MiB   6%  ░░░░░░░░░░  ├── bin
178.0 MiB   1%  ░░░░░░░░░░  ├── libexec
 98.4 MiB   0%  ░░░░░░░░░░  ├── local
 12.9 MiB   0%  ░░░░░░░░░░  └── (5 other items)
warning: 6 entries could not be read (run with -v for details)
```

```text
$ fsight scan . -d 2 -n 4
536.0 KiB 100%  ██████████  .
324.0 KiB  60%  ██████░░░░  ├── src
136.0 KiB  41%  ████░░░░░░  │   ├── commands
 52.0 KiB  16%  █░░░░░░░░░  │   ├── tui
 28.0 KiB   8%  ░░░░░░░░░░  │   ├── output
 20.0 KiB   6%  ░░░░░░░░░░  │   ├── cli.rs
 88.0 KiB  27%  ██░░░░░░░░  │   └── (8 other items)
100.0 KiB  18%  █░░░░░░░░░  ├── tests
 ...
```

| Option | Meaning |
|---|---|
| `-d, --depth <N>` | Levels to display (default 3). Sizes always include the whole tree. |
| `-n, --top <N>` | Show the N largest entries per directory (default 10, `0` = all); the rest collapse into `(N other items)`. |
| `--apparent-size` | Count file lengths instead of allocated disk blocks. |
| `--min-size <SIZE>` | Hide entries smaller than SIZE (`200B`, `10K`, `5M`, `1.5G`; 1K = 1024). |

Details: each hard-linked file is counted once (credited to the path that
sorts first). Unreadable entries never abort the scan: they are counted and
summarized on stderr. With `--hidden --no-ignore` the totals match
`du -s --block-size=1` exactly.

### `fsight find`

Find files and directories by name, type, size and age. One path per line,
sorted, with the match highlighted.

```text
$ fsight find report -t f /usr/share/doc
/usr/share/doc/git/git-bugreport.adoc
/usr/share/doc/git/git-bugreport.html
/usr/share/doc/groff-base/BUG-REPORT
/usr/share/doc/nodejs22/html/api/report.html
/usr/share/doc/nodejs22/html/api/report.json
```

```text
$ fsight find '*' src -e rs --size +16K -l
  17.6 KiB  2026-11-21 06:48  src/cli.rs
  21.3 KiB  2026-11-21 06:33  src/commands/dupes.rs
  19.5 KiB  2026-11-21 06:32  src/commands/find.rs
  24.6 KiB  2026-11-21 06:27  src/commands/grep.rs
  ...
```

* `PATTERN` is a **glob** (`*.rs`, `report-??.pdf`, `{a,b}.txt`), or a
  **regex** with `-r`. A pattern without wildcards matches anywhere in the
  name: `fsight find report` finds `annual-report.pdf`. With no pattern,
  everything matches.
* **Smart case:** a pattern without uppercase letters is case-insensitive.
  `-s` forces case-sensitive and `-i` forces case-insensitive matching.
* The first positional argument is always the pattern, so to list
  everything in another directory use `'*'`: `fsight find '*' ~/Videos`.

| Option | Meaning |
|---|---|
| `-t, --type <f\|d\|l>` | File, directory or symlink (repeatable). |
| `-e, --ext <EXT>` | Extension, case-insensitive, repeatable (`-e jpg -e png`, also `-e tar.gz`). |
| `--size <+SIZE\|-SIZE\|SIZE>` | At least, at most, or exactly SIZE (files only, repeatable: `--size +1M --size -10M`). |
| `--newer <DURATION>` / `--older <DURATION>` | Modified within / before, e.g. `30m`, `3h`, `2d`, `1w`, `1y`, `1h30m`. |
| `--max-depth <N>` | Descend at most N levels (1 = direct children). |
| `-p, --full-path` | Match against the whole displayed path (`*` stops at `/`, use `**`). |
| `-l, --long` | Show size and modification time (UTC). |

### `fsight grep`

Search file contents with a regular expression. The output is grouped by file
and sorted by path, with line numbers and highlighted matches; context lines use
`-`, matching lines `:`.

```text
$ fsight grep -w TopK src -C 1
src/commands/stats.rs
73-#[derive(Debug)]
74:struct TopK<T: Ord> {
75-    heap: BinaryHeap<Reverse<T>>,
--
78-
79:impl<T: Ord> TopK<T> {
80-    fn new(k: usize) -> Self {
```

```text
$ fsight grep -c 'unwrap\(\)' -g '*.rs'
src/cli.rs:7
src/commands/dupes.rs:7
src/commands/find.rs:9
...
```

| Option | Meaning |
|---|---|
| `-i` / `-w` / `-F` | Ignore case / whole words / fixed string (no regex). |
| `-C <N>`, `-A <N>`, `-B <N>` | Context lines around, after, before each match. |
| `-c` / `-l` | Count matching lines per file / only list file names. |
| `-g <GLOB>` | Only search matching files; `!` excludes (`-g '*.rs' -g '!target/**'`); repeatable. |
| `--max-filesize <SIZE>` | Skip larger files. |
| `--binary` | Also search binary files (by default a NUL byte in the first 8 KiB means "binary, skip"). |

Files are read line by line and never loaded whole into memory. Invalid UTF-8 is
shown with `�`.

### `fsight dupes`

Duplicate files, most wasted space first. **Read-only**: it reports and
never deletes anything.

```text
$ fsight dupes /usr/share/licenses
25.9 KiB × 87 copies (2.2 MiB wasted)
  /usr/share/licenses/7zip/copying.txt
  /usr/share/licenses/NetworkManager/COPYING.LGPL
  /usr/share/licenses/at-spi2-atk/COPYING
  ...

Found 192 duplicate groups, 956 files, 14.5 MiB wasted
```

The work is done in three stages, each applied to fewer files:

1. Group by exact size; unique sizes are dropped without reading anything.
2. Hash the first 16 KiB with BLAKE3; unique prefixes are dropped.
3. Hash whole files with BLAKE3, in parallel. Files of at most 16 KiB skip
   this stage, because stage 2 already hashed them completely.

Hard links to the same file are not duplicates. They are shown as
`(+1 hard link: ...)` and not counted. Options: `--min-size <SIZE>` (default
1 byte), `-e/--ext`, `--include-empty`.

### `fsight stats`

Totals, the top 15 extensions by size, size and age histograms, and the
largest and oldest files.

```text
$ fsight stats src
Totals
  Files              23
  Directories         3
  Symlinks            0
  Total size  271.8 KiB

Top extensions by size
  extension      files        size       %
  rs                23   271.8 KiB  100.0%  ██████████████████████████████

File sizes
  size         files       %        size
  <1K              0    0.0%         0 B
  1K–100K         23  100.0%   271.8 KiB  ██████████████████████████████
  100K–1M          0    0.0%         0 B
  ...
File ages (by modification time)
  age           files       %        size
  <1 day           23  100.0%   271.8 KiB  ██████████████████████████████
  ...
Largest files
    24.6 KiB  src/commands/grep.rs
    22.7 KiB  src/commands/stats.rs
    ...
```

Sizes here are file lengths. Files without an extension (including dotfiles)
are grouped as `(none)`. Hard-linked files count once.

### `fsight tui`

An interactive disk usage explorer. The scan runs in the background, so the
screen shows live progress and stays responsive. Then you can browse the tree:

```text
 fsight src  273.0 KiB of 273.0 KiB  sort: size
 121.3 KiB  44%  ████░░░░░░  commands/
  43.5 KiB  15%  █░░░░░░░░░  tui/
  19.1 KiB   6%  ░░░░░░░░░░  output/
  17.6 KiB   6%  ░░░░░░░░░░  walk.rs
  17.6 KiB   6%  ░░░░░░░░░░  cli.rs
  16.8 KiB   6%  ░░░░░░░░░░  config.rs
  10.8 KiB   3%  ░░░░░░░░░░  size.rs
┌ Info ──────────────────────────────────────────────────────────────────────┐
│Path:     src/tui                                                           │
│Type:     directory                                                         │
│Size:     43.5 KiB (44,584 bytes)                                           │
│Files:    4                                                                 │
│Modified: 2026-11-21 06:50 UTC (3m ago)                                     │
└────────────────────────────────────────────────────────────────────────────┘
 ↑↓ move  Enter open  ← up  s sort  / filter  i info  ? help  q quit
```

| Keys | Action |
|---|---|
| `↑`/`↓`, `k`/`j` | Move the cursor (`PgUp`/`PgDn`, `g`/`G` for pages and ends) |
| `Enter`, `→`, `l` | Open the selected directory |
| `Backspace`, `←`, `h` | Go to the parent directory |
| `s` | Cycle sorting: size → name → modified |
| `/` | Filter by name (live); `Enter` keeps the filter, `Esc` clears it |
| `i` | Show/hide details: full path, size, file count, modification time |
| `?` | Help popup |
| `q`, `Ctrl-C` | Quit |

Options: `--sort <size|name|modified>`, `--apparent-size`. The terminal is
restored on exit, on errors and on panics, and tiny terminal sizes are handled.

### `fsight completions`

`fsight completions <bash|zsh|fish|elvish|powershell>` prints a completion
script (see [Install](#install)).

## Global flags

These work with every command, before or after its name.

| Flag | Meaning |
|---|---|
| `--json`, `--compact` | Print one JSON document (pretty, or on one line with `--compact`, which implies `--json`). |
| `--no-color`, `--color <auto\|always\|never>` | Colors; `auto` (default) colors only on a terminal. `NO_COLOR` is respected. |
| `-j, --threads <N>` | Worker threads (default: number of logical CPUs). |
| `--hidden` / `--no-hidden` | Include / skip hidden files. |
| `--no-ignore` / `--ignore` | Ignore / respect `.gitignore`, `.ignore` and git exclude files. |
| `-L, --follow` | Follow symbolic links (loops are detected and skipped). |
| `-x, --one-file-system` | Do not cross into other file systems (like `du -x`). |
| `-q, --quiet` | No progress spinner. |
| `-v, --verbose` | More logs on stderr (repeatable: warnings, info, debug, trace). |

Progress is shown on stderr only when it is a terminal and neither `--quiet`
nor `--json` is given. Ctrl-C stops the work, prints the partial results with
a notice and exits with status 130. A second Ctrl-C exits immediately.

## Configuration

Defaults can be set in `$XDG_CONFIG_HOME/fsight/config.toml` (usually
`~/.config/fsight/config.toml`). Every setting is optional:

```toml
threads = 4           # worker threads
hidden = false        # include hidden files
no_ignore = false     # ignore .gitignore/.ignore rules
color = "auto"        # auto | always | never

[scan]
depth = 3             # levels to display
top = 10              # entries per directory (0 = all)

[tui]
sort = "size"         # size | name | modified
```

Environment variables override the file: `FSIGHT_THREADS`, `FSIGHT_HIDDEN`,
`FSIGHT_NO_IGNORE`, `FSIGHT_COLOR`, `FSIGHT_SCAN_DEPTH`, `FSIGHT_SCAN_TOP` and
`FSIGHT_TUI_SORT` (booleans accept `true/false`, `1/0`, `yes/no`, `on/off`).
`NO_COLOR` disables colors. Command-line flags override everything:

```text
built-in defaults  <  config file  <  environment  <  command-line flags
```

`fsight --print-config` prints the merged result (which is itself a valid
config file). Errors are specific:

```text
$ fsight scan
error: invalid configuration: invalid config file /home/me/.config/fsight/config.toml:
TOML parse error at line 3, column 9
  |
3 | depth = "deep"
  |         ^^^^^^
invalid type: string "deep", expected usize
```

Unknown keys are reported too (`unknown field 'hiden'`), so typos don't go
unnoticed.

## Exit status

| Status | Meaning |
|---|---|
| 0 | Success (for `find`/`grep`: something matched) |
| 1 | `find` or `grep` found nothing |
| 2 | Error: invalid arguments, missing path, invalid pattern, bad configuration |
| 130 | Interrupted by Ctrl-C (partial results were printed) |

Unreadable entries during a walk are warnings, not errors. A closed pipe
(`fsight find | head`) ends fsight quietly.

## JSON output

Every command except `tui` and `completions` accepts `--json`:

```sh
fsight scan --json | jq '.tree.children[] | {name, size}'
fsight find '*.log' --older 4w --json | jq -r '.matches[].path'
fsight grep TODO --json | jq '.files[] | {path, matched_lines}'
fsight dupes --json | jq '.wasted'
fsight stats --json | jq '.extensions[:3]'
```

Paths are strings (invalid UTF-8 is replaced with `�`), sizes are bytes, and
times are Unix seconds.

## Comparison

fsight is a learning project that combines several tools in one binary. The
originals are more mature and have more options; this is an honest summary.
See [docs/benchmarks.md](docs/benchmarks.md) for measured speed.

| | fsight | the original |
|---|---|---|
| Disk usage (`dust`) | `scan` tree with percentages and bars, `--depth`, `--top`, `--min-size`, apparent or allocated sizes, hard links counted once, plus an interactive `tui`. **Skips hidden and git-ignored files by default** (use `--hidden --no-ignore`). | dust counts everything by default and has more display options (reverse order, file-type views). |
| Find (`fd`) | Globs by default (regex with `-r`), smart case, type/extension/size/age filters. Results are **sorted** and printed when the walk ends. | fd uses regex by default, streams results while it walks, colors with `LS_COLORS`, and can run commands on results (`-x`). |
| Content search (`ripgrep`) | Parallel regex search, context, counts, globs, binary detection. Output is **sorted by path** and streams in order. | ripgrep is far more optimized (SIMD literal search, memory maps), supports multiline search, encodings, replacements, many more flags. Its output order is unsorted unless `--sort` (which makes it single-threaded). |
| Duplicates (`fdupes`) | Size → 16 KiB BLAKE3 → full BLAKE3, in parallel; hard-link aware; **never deletes**. | fdupes can delete or hard-link duplicates and verifies byte by byte. |
| Statistics | `stats`: extensions, size and age histograms, largest and oldest files. | No direct equivalent in these tools. |

Notes for spinning disks (HDD): parallel reads of many files make the disk seek.
If `dupes` or `grep` feel slow on an HDD, try `-j 1` or `-j 2`.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

There are about 190 tests: unit tests next to the code, property tests
(`proptest`), integration tests that run the binary on generated fixture trees
(`assert_cmd` + `tempfile`), and `insta` snapshots of the text output and help
screens.

Documentation:

* [docs/architecture.md](docs/architecture.md): modules, data flow, threading.
* [docs/learning-notes.md](docs/learning-notes.md): how the Rust concepts
  are used in this code, for beginners, with exercises.
* [docs/decisions.md](docs/decisions.md): design decisions and their reasons.
* [docs/progress.md](docs/progress.md): what was built, phase by phase.

No license has been chosen yet; add one before publishing the code.
