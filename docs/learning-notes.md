# Learning notes: the Rust in fsight

These notes are for someone learning Rust. Each section explains one concept
and points to the places in this project where it is used, as
`file:first-last` line ranges, so you can open the file and read the real code
next to the explanation. Excerpts are copied from the code. Comments are
sometimes shortened.

A good way to read: pick a section, open the referenced lines, then run the
related tests (`cargo test <name>`) and change something to see what the
compiler or the tests say.

**A first tour of one command.** `fsight scan` goes through these steps:

1. `main` parses the arguments ([src/main.rs:24-44](../src/main.rs)).
2. It loads the configuration and calls `commands::run`
   ([src/main.rs:46-72](../src/main.rs)).
3. `commands::run` picks the subcommand ([src/commands/mod.rs:90-118](../src/commands/mod.rs)).
4. `scan::build_tree` walks the directory ([src/commands/scan.rs:338-359](../src/commands/scan.rs))
   with the shared walker ([src/walk.rs:293-335](../src/walk.rs)).
5. The result is a plain struct that is rendered as text
   ([src/output/tree.rs:24-34](../src/output/tree.rs)) or as JSON.

---

## 1. Ownership and borrowing

Every value has exactly one owner. When the owner goes out of scope, the
value is dropped. You can *move* ownership, or *borrow* the value: any
number of shared borrows (`&T`) or exactly one mutable borrow (`&mut T`), never
both at once.

**Consuming `self` to end a phase.** A `TreeBuilder` collects entries, and
`finish` turns it into a `DiskTree`:

```rust
pub fn add(&mut self, item: &ScanItem) -> u64 { ... }       // borrows the builder mutably
pub fn finish(mut self, summary: WalkSummary) -> DiskTree { ... }  // takes ownership
```

`add` ([src/commands/scan.rs:189-208](../src/commands/scan.rs)) needs to change
the builder, so it takes `&mut self`. It only reads the item, so it takes
`&ScanItem`. `finish` ([src/commands/scan.rs:310-335](../src/commands/scan.rs))
takes `self` by value: after `builder.finish(...)` the builder is gone, so
nobody can add to a finished tree by mistake. The compiler enforces this.

**Moving instead of copying.** `group_by_size(candidates: Vec<Candidate>)`
([src/commands/dupes.rs:84-105](../src/commands/dupes.rs)) takes the vector
by value and *moves* each `Candidate` into a bucket of a `HashMap`. No path is
copied. Compare `merge_hard_links` (lines 107-127), which also consumes its
input and moves `candidate.path` into either a new `Inode` or a `links` list.

**Taking a field out of a struct.** In `Accumulator::finish`
([src/commands/stats.rs:278-313](../src/commands/stats.rs)), line 279 does
`std::mem::take(&mut self.linked)`. It moves the map out and leaves an empty
one behind, so the code can then call `self.add_file(...)` (which needs
`&mut self`) while iterating over the map it took.

**Clone when you really need a copy.** `find::run` needs the walk options
with one field changed. It writes
`WalkOptions { max_depth: options.max_depth, ..ctx.walk.clone() }`
([src/commands/find.rs:337-376](../src/commands/find.rs)). The `..` "struct
update" syntax fills the other fields from a clone. Cloning is explicit in Rust,
so you always see where copies happen.

**Borrows end when they are last used.** In `App::open_selected`
([src/tui/app.rs:187-204](../src/tui/app.rs)), `let
Some(tree) = self.tree() else { return; };` borrows `self`, and a few lines
later the code assigns `self.cwd = id`. This is allowed because `tree` is not
used after that point ("non-lexical lifetimes").

> Try it: in `open_selected`, use `tree` again after `self.cwd = id;` and
> read the error: you cannot assign to `self.cwd` while `self` is borrowed.

## 2. `Result`, `?`, and the thiserror / anyhow split

Rust has no exceptions. A function that can fail returns `Result<T, E>`, and the
caller must handle it. `?` means "if this is an error, return it from the
current function (converting it with `From` if needed), otherwise unwrap
the value".

**Library errors are precise enums (thiserror).**
[src/error.rs:18-27](../src/error.rs) defines `SizeError`, and lines 44-75
define the main `Error`:

```rust
#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot access '{}'", path.display())]
    Path { path: PathBuf, #[source] source: io::Error },
    #[error("invalid pattern: {0}")]
    Pattern(String),
    ...
}
```

`#[derive(Error)]` writes the `Display` and `std::error::Error` impls for you.
`#[source]` records the underlying cause without repeating it in the message.
`impl From<regex::Error> for Error` (lines 85-89) is what lets
`RegexBuilder::new(..).build()?` work inside a function that returns
`fsight::Result` ([src/commands/find.rs:116-152](../src/commands/find.rs)): `?`
calls `From::from` on the error.

**The binary uses anyhow.** An application mostly wants to *report* errors,
with context. In [src/main.rs:46-72](../src/main.rs):

```rust
let config = Config::load(cli).context("invalid configuration")?;
```

`.context(...)` wraps any error in an `anyhow::Error` with an extra message.
`main` prints the whole chain with `{err:#}` (lines 24-44), which gives
messages like `error: invalid configuration: invalid value 'lots' for
FSIGHT_THREADS: ...`. `is_broken_pipe` (lines 131-141) walks the chain and
`downcast_ref`s each cause to find out whether the error was "the reader closed
the pipe".

**`?` also works on `Option`.** In `Filter::check`
([src/commands/find.rs:266-309](../src/commands/find.rs)), the function returns
`Option<FoundEntry>`, and `let meta = entry.metadata()?;` returns `None` early
when the metadata cannot be read.

**Instead of `unwrap`.** Non-test code never calls `.unwrap()` or `.expect()`.
Instead it uses fallbacks that make sense:

* `u64::try_from(bytes).unwrap_or(u64::MAX)` saturates
  ([src/size.rs:18-70](../src/size.rs)).
* `lock().unwrap_or_else(PoisonError::into_inner)` keeps using a mutex after
  another thread panicked ([src/progress.rs:63](../src/progress.rs)).
* `Option::transpose` turns `Option<Result<T>>` into `Result<Option<T>>` so `?`
  can be used ([src/config.rs:272-287](../src/config.rs)).

## 3. Traits and generics

A *trait* is a set of methods a type promises to have. *Generics* let one
function work for many types, as long as they implement the required traits
(the *bounds*).

**Our own trait.** Every report can be printed as text:

```rust
pub trait RenderText {
    fn render_text(&self, out: &mut dyn Write, palette: Palette) -> io::Result<()>;
}
```

([src/output/mod.rs:47-58](../src/output/mod.rs)). It is implemented for
`ScanReport` ([src/output/tree.rs:24-34](../src/output/tree.rs)), `FindReport`,
`GrepReport`, `DupeReport` and `StatsReport`. Then one generic function handles
both output formats for all of them:

```rust
pub fn emit<T>(out: &mut dyn Write, report: &T, format: Format, palette: Palette) -> io::Result<()>
where
    T: Serialize + RenderText,
```

([src/output/mod.rs:60-74](../src/output/mod.rs)). `T: Serialize + RenderText`
means "any type that can be turned into JSON *and* rendered as text".

**Static and dynamic dispatch.** `emit<T>` is *generic*: the compiler
generates a copy for each report type (static dispatch, no runtime cost).
`out: &mut dyn Write` is a *trait object*: one function works with any writer
chosen at runtime (dynamic dispatch). That is why the tests can render into a
`Vec<u8>` and the real program into stdout.

**Implementing standard traits.**

* `Display` for `Painted` ([src/output/color.rs:117-131](../src/output/color.rs))
  lets `format!("{}", palette.paint(Role::Match, "abc"))` add color codes.
* `Drop` for `RestoreOnDrop` ([src/tui/mod.rs:177-185](../src/tui/mod.rs)) runs
  cleanup code when the value goes out of scope (see section 11).
* `Default` for `Config` ([src/config.rs:66-79](../src/config.rs)) defines
  the built-in defaults.

**A generic data structure.** `TopK<T: Ord>`
([src/commands/stats.rs:71-103](../src/commands/stats.rs)) keeps the k largest
items of *any* ordered type. Stats uses it with tuples like
`(u64, Reverse<PathBuf>)`. Tuples compare element by element, and `Reverse`
flips an ordering, so "largest size, then smallest path" needs no custom
comparison code.

**Generic over closures.** `Walker::run<T, M, C>`
([src/walk.rs:293-335](../src/walk.rs)) takes two closures and describes
what they must allow with bounds (`M: Fn(&Entry<'_>) -> Option<T> + Sync`,
`C: FnMut(T)`); section 7 explains why. `Stage::refine<K, F>`
([src/commands/dupes.rs:137-154](../src/commands/dupes.rs)) takes the hash
function as a parameter, which is how the tests can pass a fake one.

**Generic over library traits.** `search_lines<R: BufRead>`
([src/commands/grep.rs:184-244](../src/commands/grep.rs)) works with a real
file, with a `&[u8]` in tests, and with a custom reader that produces data on
the fly (lines 703-707). `parse_enum<T: ValueEnum>`
([src/config.rs:308-318](../src/config.rs)) parses any of clap's value enums
and even lists their possible values in the error message.

## 4. Iterators and closures

A *closure* is an anonymous function that can capture variables from where it
is written. An *iterator* produces values one at a time. Adapters like `map`
and `filter` build up a pipeline that does nothing until something consumes it
(`collect`, `sum`, `for`, …).

**`fold` instead of a loop.** Parsing the digits after the decimal point in
[src/size.rs:60](../src/size.rs):

```rust
let (fraction_value, scale) = fraction
    .bytes()
    .fold((0u128, 1u128), |(value, scale), digit| {
        (value * 10 + u128::from(digit - b'0'), scale * 10)
    });
```

`fold` carries a running state (here a tuple) through all items.

**A pipeline with sorting.** `rank_extensions`
([src/commands/stats.rs:316-343](../src/commands/stats.rs)) maps a `HashMap`
into `Group`s, collects them, sorts by several keys
(`b.size.cmp(&a.size).then(...).then_with(...)`), and `split_off`s everything
after the top 15.

**Filtering and sorting with cached keys.** `visible_children`
([src/tui/app.rs:293-325](../src/tui/app.rs)) filters a directory's children
by name and sorts them. `sort_by_cached_key` computes each lowercase name only
once.

**Building a path from an iterator.**
`item.rel_path.components().take(self.max_depth).collect()`
([src/commands/scan.rs:197](../src/commands/scan.rs)) keeps the first N parts
of a path. `collect` can build a `PathBuf` because `PathBuf` implements
`FromIterator`.

**Closures that capture.** In `build_tree`
([src/commands/scan.rs:338-359](../src/commands/scan.rs)):

```rust
let summary = walker.run(
    |entry| ScanItem::from_entry(entry, root, apparent),   // captures root, apparent
    |item| {
        let credited = builder.add(&item);                // captures builder mutably
        ctx.progress.add_bytes(credited);
    },
);
```

The first closure only reads what it captures (it is `Fn`). The second
mutates `builder` (it is `FnMut`). Stats even passes a plain function where a
closure is expected: `walker.run(StatItem::from_entry, ...)`
([src/commands/stats.rs:365](../src/commands/stats.rs)).

**Parallel iterators.** With rayon, `into_iter()` becomes `into_par_iter()`
and the work is spread over threads
([src/commands/dupes.rs:137-154](../src/commands/dupes.rs)). `partition`
(line 215) splits one iterator into two collections by a condition.

**Small helpers worth knowing:** `bool::then` / `then_some` turn a bool into
an `Option` ([src/output/color.rs:112](../src/output/color.rs)),
`Option::is_none_or` ([src/commands/find.rs:156](../src/commands/find.rs)),
`usize::saturating_add_signed` ([src/tui/app.rs:170](../src/tui/app.rs)).

## 5. Enums and pattern matching

Rust enums can carry data in each variant, and `match` must handle every
variant. When you add one, the compiler shows you every place to update.

**Enums with data.** grep classifies each searched file:

```rust
enum FileOutcome {
    Matches(FileMatches, u64),
    NoMatch(u64),
    Binary,
    Failed(io::Error),
}
```

([src/commands/grep.rs:252-259](../src/commands/grep.rs)). It is matched in
`record` (lines 439-470). Other examples: `ScanState` in the TUI
([src/tui/app.rs:51-60](../src/tui/app.rs)), and `SizeFilter` with a `matches`
method ([src/size.rs:132-152](../src/size.rs)).

**Matching on tuples** turns flag combinations into a readable table
([src/commands/mod.rs:153](../src/commands/mod.rs)):

```rust
let case = match (args.case_sensitive, args.ignore_case) {
    (true, _) => find::CaseMode::Sensitive,
    (false, true) => find::CaseMode::Insensitive,
    (false, false) => find::CaseMode::Smart,
};
```

Also `tri_state` in [src/cli.rs:127-134](../src/cli.rs).

**Guards.** `MapEntry::Occupied(mut slot) if candidate.rel_path < slot.get().rel_path`
([src/commands/scan.rs:299](../src/commands/scan.rs), inside
`credited_size`, lines 277-308) matches only when the condition holds as well.

**`let ... else`** unpacks a value or leaves early:

```rust
let Some(item) = map(&entry) else {
    return WalkState::Continue;
};
```

([src/walk.rs:337-377](../src/walk.rs), and
[src/commands/find.rs:130](../src/commands/find.rs)).

**`matches!`** is a match that returns a bool
([src/error.rs:77-83](../src/error.rs),
[src/commands/find.rs:43-52](../src/commands/find.rs)).

**Let chains** (new in Rust 2024) combine `if let` with conditions:
`if let Some(limit) = options.max_filesize && entry.metadata()?.len > limit`
([src/commands/grep.rs:361](../src/commands/grep.rs)).

## 6. Modules and visibility

`mod name;` declares a module, and its code lives in `name.rs` or
`name/mod.rs`. Everything is private to its module unless it is marked `pub`.

* The module tree is declared in [src/lib.rs:24-35](../src/lib.rs). Folders
  such as `commands/` have a `mod.rs` that declares their children
  ([src/commands/mod.rs:9-13](../src/commands/mod.rs)).
* `pub use error::{Error, Result};` (lib.rs line 35) *re-exports* the error
  types, so users write `fsight::Error` instead of `fsight::error::Error`.
* Paths: `crate::` starts at the crate root, `super::` at the parent module
  (`use super::Context;` in [src/commands/find.rs:21](../src/commands/find.rs)).
* Helpers stay private, for example `fn has_uppercase_literal`
  ([src/commands/find.rs:171-185](../src/commands/find.rs)). `display_path` is
  `pub` because grep, dupes and stats use it too.
* `pub(crate)` means "visible anywhere in this crate, but not outside". The
  TUI's test helpers live in `pub(crate) mod tests`
  ([src/tui/app.rs:347](../src/tui/app.rs)) so the tests in `events.rs` and
  `ui.rs` can reuse them.
* Conditional compilation: `#[cfg(unix)]` and `#[cfg(not(unix))]` give two
  versions of the platform helpers ([src/walk.rs:140-180](../src/walk.rs)).
  On Unix the inode and block count come from `MetadataExt`.
* Crate-wide settings sit at the top of lib.rs (lines 18-22):
  `#![forbid(unsafe_code)]` and `#![warn(clippy::pedantic)]`.
* `main.rs` is a *separate crate* that uses the library like any other
  dependency (`use fsight::cli::Cli;`). That is why everything `main` needs is
  `pub`.

## 7. `Send` and `Sync`: why the threads compile safely

Two marker traits make Rust's threads safe:

* `Send`: a value can be **moved** to another thread.
* `Sync`: a value can be **shared** between threads through `&T` (`T: Sync`
  means `&T: Send`).

The compiler works these out for every type. `Rc` (non-atomic reference
counting) and `RefCell` (unchecked runtime borrowing) are neither, while
`Arc`, `Mutex` and the atomics are.

**The walker's bounds** ([src/walk.rs:293-335](../src/walk.rs)):

```rust
pub fn run<T, M, C>(&self, map: M, mut consume: C) -> WalkSummary
where
    T: Send,                                  // results travel to another thread
    M: Fn(&Entry<'_>) -> Option<T> + Sync,    // all workers call it at once
    C: FnMut(T),                              // runs on this thread only
```

* `T: Send`: each result is created on a worker thread and *moved* through the
  channel to the calling thread.
* `M: Sync`: every worker holds a `&M` and calls it at the same time, so `map`
  must be safe to share. `Fn` (not `FnMut`) means calling it cannot change its
  captured state, so there is no data race.
* `C` needs neither. `consume` stays on one thread, so it can be an `FnMut`
  that mutates whatever it captured, like the tree builder.

That is the design: work that needs mutation happens on one thread (in
`consume`), and shared work is read-only (in `map`). If you break this rule,
the program does not compile. Here is what happens if `map` tries to push into
an `Rc<RefCell<Vec<_>>>` (this error was produced by actually trying):

```text
error[E0277]: `Rc<RefCell<Vec<usize>>>` cannot be shared between threads safely
note: required by a bound in `walk::Walker::<'a>::run`
```

**What the compiler checks for the boxed visitor.** Inside `run`, each worker
gets `Box::new(move |result| self.visit(result, map, &tx))`, and `ignore`
requires that box to be `Send`. The closure captures a `Sender<T>` (Send when
`T: Send`), `&Walker` (Send because `Walker` is Sync: its fields are references
to Sync data plus an `AtomicU64`), and `&M` (Send because `M: Sync`). Each
link of that chain is checked at compile time.

**Scoped threads may borrow.** `std::thread::scope` (walk.rs line 315, grep's
`search_files` at [src/commands/grep.rs:374-437](../src/commands/grep.rs))
guarantees that the threads finish before the scope ends. So they can borrow
local variables (`&files`, `&regex`, `&cursor`) without `Arc`. A `Regex` is
`Sync`, so all grep workers can use the same one.

## 8. Channels

A channel connects threads: senders put values in, a receiver takes them out
in order. fsight uses `crossbeam-channel`.

* **Map → consume** ([src/walk.rs:309-327](../src/walk.rs)). The channel is
  *bounded* (4096 slots): when the consumer is slower, the workers wait instead
  of filling memory. `for item in rx` ends when **every** sender has been
  dropped. Each worker gets a `tx.clone()`, and the original `tx` is moved
  into the spawned thread and dropped when it ends.
* **Results out of order, printed in order** (grep.rs lines 398-431):

  ```rust
  let index = cursor.fetch_add(1, Ordering::Relaxed);   // claim a file
  ...
  tx.send((index, outcome))                              // report it with its index
  ...
  let mut waiting: BTreeMap<usize, FileOutcome> = BTreeMap::new();
  for (index, outcome) in rx {
      waiting.insert(index, outcome);
      while let Some(outcome) = waiting.remove(&next) { ... next += 1; ... }
  }
  ```

* **A channel as a "done" signal** ([src/progress.rs:105-127](../src/progress.rs)).
  The spinner thread waits on `recv_timeout`. Nobody ever sends anything; the
  work side simply drops its sender (`drop(done_tx)`, line 124). That happens
  even if the work panics, so the spinner always stops.
* **Background scan → UI** ([src/tui/mod.rs:110-156](../src/tui/mod.rs)). The
  scanner sends `ScanEvent::Progress` and `ScanEvent::Finished(tree)`. The UI
  uses `try_iter()`, which never blocks, so it keeps drawing.

## 9. `Arc` and atomics

**`Arc<T>`** ("atomically reference counted") is shared ownership: cloning
an `Arc` makes another handle to the *same* value, and the value is freed when
the last handle goes. Use it when you cannot prove how long a value is needed.

The Ctrl-C handler runs on a thread owned by the `ctrlc` crate that may live
as long as the program, so it needs a `'static` value it can own. A borrowed
reference would not do. Hence ([src/main.rs:68-69](../src/main.rs) and
lines 100-109):

```rust
let cancel = Arc::new(AtomicBool::new(false));
install_ctrlc_handler(Arc::clone(&cancel))?;   // one handle for the handler
...
ctrlc::set_handler(move || {
    if cancel.swap(true, Ordering::SeqCst) {   // swap returns the *old* value
        std::process::exit(130);               // second Ctrl-C: really quit
    }
})
```

Everything else just borrows: `Context` holds `cancel: &'a AtomicBool`
([src/commands/mod.rs:53-64](../src/commands/mod.rs)), because scoped threads
can borrow.

**Atomics** are integers and booleans that many threads can update without a
lock:

* Progress counters use `fetch_add(n, Ordering::Relaxed)`
  ([src/progress.rs:48-76](../src/progress.rs)), and so does the error counter
  (`ErrorLog`, [src/walk.rs:182-200](../src/walk.rs)). `Relaxed` is enough
  because each counter is an independent statistic; no other memory depends
  on its value.
* The grep cursor: `fetch_add` returns the previous value, so every index is
  handed out exactly once ([src/commands/grep.rs:404](../src/commands/grep.rs)).
* A `static` atomic: `static TERMINAL_ACTIVE: AtomicBool`
  ([src/tui/mod.rs:58-61](../src/tui/mod.rs)) is shared by the whole program,
  including the panic hook. `restore_terminal` (lines 165-175) uses
  `swap(false)` so that only the first caller restores the terminal.

A `Mutex` is used once, for the progress stage label
([src/progress.rs:19-25](../src/progress.rs)), because a `&'static str` does not
fit in an atomic.

## 10. Lifetimes

A lifetime is the compiler's name for "how long a borrow is valid". Usually
it is inferred ("elided"). You write it when a struct holds a reference, or
when a function returns one and the compiler cannot tell which input it
comes from.

**Structs that hold borrows.**

* `Context<'a>` ([src/commands/mod.rs:53-64](../src/commands/mod.rs)) borrows
  the cancel flag and the progress counters. A `Context` cannot outlive them.
* `Walker<'a>` ([src/walk.rs:258-266](../src/walk.rs)) borrows its root path,
  options, flag and counters.
* `Entry<'w>` ([src/walk.rs:202-247](../src/walk.rs)) borrows the walker's
  `ErrorLog`, so `entry.metadata()` can record a failure itself.
* `Printer<'w>` ([src/commands/grep.rs:480-487](../src/commands/grep.rs)) and
  `TreeRenderer<'a>` ([src/output/tree.rs:46-51](../src/output/tree.rs)) borrow
  their output writer.
* `Painted<'a, T: ?Sized>` ([src/output/color.rs:105-131](../src/output/color.rs))
  borrows the text it will color, so painting allocates nothing. `?Sized`
  allows `T = str`.

**Functions that return borrows.**

```rust
pub fn display_path<'p>(path: &'p Path, root: &Path) -> &'p Path
```

([src/commands/find.rs:209-216](../src/commands/find.rs)). The result is a
part of `path` (with `./` removed), never of `root`, and the `'p` tells the
compiler exactly that. Similarly, `parse_part<'a>(text: &'a str, whole_input:
&str) -> ... &'a str` ([src/duration.rs:55-87](../src/duration.rs)) returns
the rest of `text`.

**`'_` and `'static`.** `impl Entry<'_>` and `Context<'_>` mean "some
lifetime, I don't need to name it". `'static` means "valid for the whole
program": string literals like the help texts in `cli.rs`, the
`&'static str` stage names in `Progress`, and the Ctrl-C closure above.

## 11. Testing in Rust

`cargo test` runs four kinds of tests here (about 190 in total).

**Unit tests** sit next to the code in a `#[cfg(test)] mod tests` block
(compiled only for tests), with `use super::*;` to reach private items. See
the end of [src/size.rs](../src/size.rs) or any other module. Run one module's
tests with `cargo test size::`.

**Property tests** (proptest) check a rule for hundreds of random inputs
instead of a few hand-picked ones ([src/size.rs:278-304](../src/size.rs)):
"parsing a formatted size gives back the original within rounding",
"formatting never panics for any `u64`".

**Testing through seams.** Code that touches the outside world takes it as a
parameter, so tests can pass a fake:

* The dupes stages take their hash functions as parameters. `FakeFiles`
  ([src/commands/dupes.rs:428-434](../src/commands/dupes.rs)) hashes in-memory
  data and counts calls. The test at lines 529-546 proves that stage 2 stopped
  a file from being fully hashed.
* Configuration reads the environment through a function parameter. The tests
  pass a `HashMap` ([src/config.rs:326-333](../src/config.rs)) and check the
  precedence rules (lines 359-383). This matters because changing the real
  environment is `unsafe` in Rust 2024.
* The TUI draws on ratatui's in-memory `TestBackend`
  ([src/tui/ui.rs:291-304](../src/tui/ui.rs)), including at absurd sizes such
  as 0×0 (lines 352-372).
* grep's streaming test uses a custom `Read` implementation that produces
  27 MB of lines without ever storing them
  ([src/commands/grep.rs:703-707](../src/commands/grep.rs)).

**Integration tests** in `tests/` are separate crates that run the compiled
binary with `assert_cmd`. The `Fixture` helper
([tests/common/mod.rs:27-33](../tests/common/mod.rs)) builds a temporary
directory tree, and `cmd()` (lines 109-120) isolates the environment. An RAII
guard (`impl Drop for Unreadable`, lines 184-190) restores permissions even if
an assertion fails. A broken-pipe test reads one line and closes the pipe
([tests/cli_find.rs:232-258](../tests/cli_find.rs)).

**Snapshot tests** (insta) compare the whole output with a stored file in
`tests/snapshots/` ([tests/cli_scan.rs:39-44](../tests/cli_scan.rs)). When the
output changes on purpose, run the tests with `INSTA_UPDATE=always` (or install
`cargo-insta` and run `cargo insta review`), check the diff, and commit the new
`.snap` file.

**Doc tests**: the ```` ``` ```` examples in doc comments (for example on
`parse_size`, [src/size.rs:18-70](../src/size.rs)) are compiled and run by
`cargo test`, so the documentation cannot go stale.

---

## Exercises

Each exercise touches several of the concepts above. Run the three quality
gates after each one (see the readme).

1. **"Newest files" in `stats`.** Add a list of the 10 most recently modified
   files, next to "Oldest files". *Hints:* `Accumulator` already has a
   `TopK` for the oldest files ([src/commands/stats.rs:71-103](../src/commands/stats.rs));
   think about which key ordering makes "newest" the "largest". Add a field to
   `StatsReport`, render it, and update the snapshot test.

2. **`fsight find --sort size`.** Add a `--sort <path|size|modified>` flag to
   `find`. *Hints:* the flag goes in `FindArgs` (`src/cli.rs`) and the option
   in `FindOptions`. `run_find` in `src/commands/mod.rs` connects them.
   Sorting by size needs metadata (see `needs_metadata`). Write an
   integration test in `tests/cli_find.rs` using `file_of_size`.

3. **Directories-only view in the TUI.** Make the `d` key show only
   directories. *Hints:* add a `bool` to `App`, use it in `visible_children`
   ([src/tui/app.rs:293-325](../src/tui/app.rs)), map the key in
   `normal_key` ([src/tui/events.rs:30-48](../src/tui/events.rs)), and show the
   mode in the header. Write
   a unit test in the style of `filter_is_live_case_insensitive_and_can_be_cleared`.

4. **`fsight scan --exclude <GLOB>`.** Skip matching entries (repeatable).
   *Hints:* `WalkOptions.globs` already supports ripgrep-style globs, and a
   glob starting with `!` excludes (`build_overrides`,
   [src/walk.rs:417-432](../src/walk.rs)). Test that an excluded directory's
   size is not counted.

5. **Streaming JSON for grep (`--json-lines`).** Print one JSON object per
   file as soon as it is ready, instead of one document at the end. *Hints:*
   `grep::search_files` already takes a *sink* closure
   ([src/commands/grep.rs:374-437](../src/commands/grep.rs)); write a sink that
   serializes each `FileMatches` with `serde_json::to_writer` followed by a
   newline. Test it by parsing every output line separately.

*Bonus:* show local times instead of UTC by adding the `jiff` crate and
changing `format_timestamp` in [src/duration.rs](../src/duration.rs) (and see
decision D4 for why it is UTC today).
