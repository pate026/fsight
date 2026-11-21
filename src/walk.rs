//! The shared parallel directory walker.
//!
//! Every command that looks at a directory tree uses [`Walker`]. It is built
//! on the `ignore` crate (the walker inside ripgrep), which reads directories
//! on several threads at once and understands `.gitignore` and `.ignore`.
//!
//! # Threading model
//!
//! ```text
//!   ignore's worker threads                     the calling thread
//!   ┌──────────────────────────┐   bounded    ┌────────────────────────┐
//!   │ map(&Entry) -> Option<T> │   channel    │ consume(T)             │
//!   │ map(&Entry) -> Option<T> │ ───────────▶ │ owns all the results,  │
//!   │ ...                      │  (crossbeam) │ so it needs no locks   │
//!   └──────────────────────────┘              └────────────────────────┘
//! ```
//!
//! `map` does the per-entry work that benefits from parallelism (reading
//! metadata, matching patterns). `consume` folds the results into whatever
//! the command builds (a tree, a list, a histogram). The channel is bounded,
//! so fast workers cannot fill memory faster than `consume` empties it.

use std::ffi::OsStr;
use std::fmt::Display;
use std::fs::{self, FileType, Metadata};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::SystemTime;

use crossbeam_channel::Sender;
use ignore::overrides::{Override, OverrideBuilder};
use ignore::{DirEntry, WalkBuilder, WalkState};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::progress::Progress;

/// How many results may wait in the channel between `map` and `consume`.
const CHANNEL_CAPACITY: usize = 4096;

/// Which entries the walker visits.
#[derive(Debug, Clone, PartialEq, Eq)]
// Four independent switches that map one-to-one onto `ignore::WalkBuilder`
// settings; an enum or bitflags would only hide that correspondence.
#[allow(clippy::struct_excessive_bools)]
pub struct WalkOptions {
    /// Visit hidden files and directories (names starting with `.`).
    pub hidden: bool,
    /// Skip entries matched by `.gitignore`, `.ignore` and git's exclude files.
    pub respect_ignore: bool,
    /// Follow symbolic links. Loops are detected by `ignore` and reported.
    pub follow_links: bool,
    /// Stay on the file system of the root (like `du -x`).
    pub one_file_system: bool,
    /// Do not descend deeper than this (0 = only the root itself).
    pub max_depth: Option<usize>,
    /// Number of walker threads.
    pub threads: usize,
    /// ripgrep-style globs (`grep -g`): `*.rs` includes, `!target/**` excludes.
    pub globs: Vec<String>,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            hidden: false,
            respect_ignore: true,
            follow_links: false,
            one_file_system: false,
            max_depth: None,
            threads: 1,
            globs: Vec::new(),
        }
    }
}

/// What kind of file system object an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    /// Sockets, FIFOs, device files and the like.
    Other,
}

impl EntryKind {
    /// Classifies a [`FileType`].
    pub fn from_file_type(file_type: FileType) -> Self {
        if file_type.is_dir() {
            EntryKind::Dir
        } else if file_type.is_file() {
            EntryKind::File
        } else if file_type.is_symlink() {
            EntryKind::Symlink
        } else {
            EntryKind::Other
        }
    }
}

/// The identity of a file on disk. Two paths with the same `FileId` are hard
/// links to the same data (or the same file reached through symlinks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId {
    pub device: u64,
    pub inode: u64,
}

/// The metadata fsight needs, extracted from [`std::fs::Metadata`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryMeta {
    pub kind: EntryKind,
    /// Length in bytes ("apparent size").
    pub len: u64,
    /// Disk space actually allocated, in bytes (equals `len` where unknown).
    pub allocated: u64,
    pub modified: Option<SystemTime>,
    /// Device and inode, where the platform provides them.
    pub file_id: Option<FileId>,
    /// Number of hard links to this file (1 where unknown).
    pub links: u64,
}

impl EntryMeta {
    /// Extracts the fields fsight uses from `metadata`.
    pub fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            kind: EntryKind::from_file_type(metadata.file_type()),
            len: metadata.len(),
            allocated: allocated_size(metadata),
            modified: metadata.modified().ok(),
            file_id: file_id(metadata),
            links: link_count(metadata),
        }
    }
}

// Platform-specific helpers. On Unix the inode, link count and number of
// allocated 512-byte blocks are available; elsewhere we fall back gracefully.

#[cfg(unix)]
fn allocated_size(metadata: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    // `st_blocks` always counts 512-byte units, whatever the file system's block size.
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_size(metadata: &Metadata) -> u64 {
    metadata.len()
}

#[cfg(unix)]
// Always `Some` on Unix, but the signature must match the non-Unix version.
#[allow(clippy::unnecessary_wraps)]
fn file_id(metadata: &Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some(FileId {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn file_id(_metadata: &Metadata) -> Option<FileId> {
    None
}

#[cfg(unix)]
fn link_count(metadata: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink()
}

#[cfg(not(unix))]
fn link_count(_metadata: &Metadata) -> u64 {
    1
}

/// Counts (and logs) problems such as unreadable directories. Shared by all
/// worker threads, hence the atomic counter.
#[derive(Debug, Default)]
pub struct ErrorLog {
    count: AtomicU64,
}

impl ErrorLog {
    /// Records one problem. The details are logged as a warning (visible with `-v`).
    pub fn record(&self, err: &dyn Display) {
        self.count.fetch_add(1, Ordering::Relaxed);
        tracing::warn!("{err}");
    }

    /// Number of problems recorded so far.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
}

/// One file system entry, as seen by `map`.
///
/// It borrows the walker's [`ErrorLog`] (that is the `'w` lifetime) so that
/// [`Entry::metadata`] can record failures itself.
pub struct Entry<'w> {
    inner: DirEntry,
    errors: &'w ErrorLog,
}

impl Entry<'_> {
    /// The path, starting with the root exactly as it was given.
    pub fn path(&self) -> &Path {
        self.inner.path()
    }

    /// The final component of the path (the whole path for the root).
    pub fn file_name(&self) -> &OsStr {
        self.inner.file_name()
    }

    /// Depth below the root (the root itself has depth 0).
    pub fn depth(&self) -> usize {
        self.inner.depth()
    }

    /// The kind of entry, known without an extra system call.
    pub fn kind(&self) -> EntryKind {
        self.inner
            .file_type()
            .map_or(EntryKind::Other, EntryKind::from_file_type)
    }

    /// Reads the entry's metadata (one system call).
    ///
    /// Returns `None` if that fails — for example because the file vanished
    /// during the walk. The failure is counted as an unreadable entry.
    pub fn metadata(&self) -> Option<EntryMeta> {
        match self.inner.metadata() {
            Ok(metadata) => Some(EntryMeta::from_metadata(&metadata)),
            Err(err) => {
                self.errors.record(&err);
                None
            }
        }
    }
}

/// Totals that every walk reports alongside its results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct WalkSummary {
    /// Entries that could not be read.
    pub errors: u64,
    /// `true` if the walk was stopped early by Ctrl-C.
    pub interrupted: bool,
}

/// A configured walk over one root directory.
pub struct Walker<'a> {
    root: &'a Path,
    options: &'a WalkOptions,
    overrides: Option<Override>,
    cancel: &'a AtomicBool,
    progress: &'a Progress,
    errors: ErrorLog,
}

impl<'a> Walker<'a> {
    /// Prepares a walk of `root`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Path`] if `root` does not exist or cannot be accessed,
    /// and [`Error::Pattern`] if one of `options.globs` is invalid.
    pub fn new(
        root: &'a Path,
        options: &'a WalkOptions,
        cancel: &'a AtomicBool,
        progress: &'a Progress,
    ) -> Result<Self> {
        check_root(root, options.follow_links)?;
        let overrides = build_overrides(root, &options.globs)?;
        Ok(Self {
            root,
            options,
            overrides,
            cancel,
            progress,
            errors: ErrorLog::default(),
        })
    }

    /// Walks the tree. `map` runs on the worker threads for every entry;
    /// every `Some` it returns is handed to `consume` on the calling thread.
    ///
    /// The generic parameters say exactly what each closure must allow:
    /// * `T: Send` — results are sent from worker threads to this thread.
    /// * `M: Fn + Sync` — all workers call the same `map` at the same time,
    ///   through shared references, so it must be safe to share (`Sync`) and
    ///   must not need `&mut` access to anything (`Fn`, not `FnMut`).
    /// * `C: FnMut` — `consume` runs on one thread only, so it may mutate
    ///   whatever it captured (for example a tree being built).
    pub fn run<T, M, C>(&self, map: M, mut consume: C) -> WalkSummary
    where
        T: Send,
        M: Fn(&Entry<'_>) -> Option<T> + Sync,
        C: FnMut(T),
    {
        let (tx, rx) = crossbeam_channel::bounded::<T>(CHANNEL_CAPACITY);
        let parallel = self.builder().build_parallel();
        let map = &map;

        // A scope lets the threads borrow `self` and `map`: Rust knows they
        // finish before `scope` returns, so the borrows cannot dangle.
        std::thread::scope(|scope| {
            scope.spawn(move || {
                // `ignore` calls this closure once per worker thread to create
                // that thread's visitor; each visitor gets its own sender.
                parallel.run(|| {
                    let tx = tx.clone();
                    Box::new(move |result| self.visit(result, map, &tx))
                });
                // The original `tx` is dropped when this thread ends. Once all
                // senders are gone, the receiving loop below finishes.
            });
            for item in rx {
                consume(item);
            }
        });

        WalkSummary {
            errors: self.errors.count(),
            interrupted: self.cancel.load(Ordering::Relaxed),
        }
    }

    /// Handles one result from `ignore` on a worker thread.
    fn visit<T, M>(
        &self,
        result: std::result::Result<DirEntry, ignore::Error>,
        map: &M,
        tx: &Sender<T>,
    ) -> WalkState
    where
        M: Fn(&Entry<'_>) -> Option<T>,
    {
        if self.cancel.load(Ordering::Relaxed) {
            return WalkState::Quit;
        }
        let dir_entry = match result {
            Ok(dir_entry) => dir_entry,
            Err(err) => {
                // Unreadable directory, vanished file, symlink loop, ...
                self.errors.record(&err);
                return WalkState::Continue;
            }
        };
        if let Some(err) = dir_entry.error() {
            // A non-fatal problem such as an unparsable .gitignore line.
            tracing::debug!("{err}");
        }
        self.progress.add_items(1);

        let entry = Entry {
            inner: dir_entry,
            errors: &self.errors,
        };
        let Some(item) = map(&entry) else {
            return WalkState::Continue;
        };
        // Sending only fails if the consumer has stopped; then there is no point going on.
        if tx.send(item).is_err() {
            WalkState::Quit
        } else {
            WalkState::Continue
        }
    }

    /// Translates [`WalkOptions`] into an `ignore::WalkBuilder`.
    fn builder(&self) -> WalkBuilder {
        let options = self.options;
        let respect = options.respect_ignore;
        let mut builder = WalkBuilder::new(self.root);
        builder
            .hidden(!options.hidden)
            .ignore(respect)
            .git_ignore(respect)
            .git_exclude(respect)
            .git_global(respect)
            .parents(respect)
            // Honor .gitignore files even outside a git repository; see docs/decisions.md.
            .require_git(false)
            .follow_links(options.follow_links)
            .same_file_system(options.one_file_system)
            .max_depth(options.max_depth)
            .threads(options.threads.max(1));
        if let Some(overrides) = &self.overrides {
            builder.overrides(overrides.clone());
        }
        builder
    }
}

/// Fails early, with a clear message, if the root cannot be used.
fn check_root(root: &Path, follow_links: bool) -> Result<()> {
    let result = if follow_links {
        fs::metadata(root)
    } else {
        fs::symlink_metadata(root)
    };
    result.map(|_| ()).map_err(|source| Error::Path {
        path: root.to_path_buf(),
        source,
    })
}

/// Compiles `-g` globs into an `ignore` override matcher.
fn build_overrides(root: &Path, globs: &[String]) -> Result<Option<Override>> {
    if globs.is_empty() {
        return Ok(None);
    }
    let mut builder = OverrideBuilder::new(root);
    for glob in globs {
        builder
            .add(glob)
            .map_err(|err| Error::Pattern(format!("invalid glob '{glob}': {err}")))?;
    }
    let overrides = builder
        .build()
        .map_err(|err| Error::Pattern(err.to_string()))?;
    Ok(Some(overrides))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn tree() -> TempDir {
        let dir = TempDir::new().unwrap();
        for path in [
            "a/one.txt",
            "a/b/two.txt",
            ".hidden/three.txt",
            "skipped/four.txt",
        ] {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "x").unwrap();
        }
        fs::write(dir.path().join(".gitignore"), "skipped/\n").unwrap();
        dir
    }

    /// Walks `root` and returns the relative paths of all files, sorted.
    fn files(root: &Path, options: &WalkOptions) -> Vec<PathBuf> {
        let cancel = AtomicBool::new(false);
        let progress = Progress::default();
        let walker = Walker::new(root, options, &cancel, &progress).unwrap();
        let mut found = Vec::new();
        walker.run(
            |entry| {
                (entry.kind() == EntryKind::File)
                    .then(|| entry.path().strip_prefix(root).unwrap().to_path_buf())
            },
            |path| found.push(path),
        );
        found.sort();
        found
    }

    #[test]
    fn skips_hidden_and_ignored_entries_by_default() {
        let dir = tree();
        let options = WalkOptions {
            threads: 2,
            ..WalkOptions::default()
        };
        assert_eq!(
            files(dir.path(), &options),
            [PathBuf::from("a/b/two.txt"), PathBuf::from("a/one.txt")]
        );
    }

    #[test]
    fn hidden_and_no_ignore_options_include_everything() {
        let dir = tree();
        let options = WalkOptions {
            hidden: true,
            respect_ignore: false,
            threads: 2,
            ..WalkOptions::default()
        };
        let found = files(dir.path(), &options);
        assert_eq!(found.len(), 5, "{found:?}"); // 4 files + .gitignore
        assert!(found.contains(&PathBuf::from(".hidden/three.txt")));
        assert!(found.contains(&PathBuf::from("skipped/four.txt")));
    }

    #[test]
    fn max_depth_and_globs_restrict_the_walk() {
        let dir = tree();
        let shallow = WalkOptions {
            max_depth: Some(2),
            ..WalkOptions::default()
        };
        assert_eq!(files(dir.path(), &shallow), [PathBuf::from("a/one.txt")]);

        let globbed = WalkOptions {
            globs: vec!["two.*".into()],
            ..WalkOptions::default()
        };
        assert_eq!(files(dir.path(), &globbed), [PathBuf::from("a/b/two.txt")]);
    }

    #[test]
    fn a_cancelled_walk_stops_and_says_so() {
        let dir = tree();
        let cancel = AtomicBool::new(true);
        let progress = Progress::default();
        let options = WalkOptions::default();
        let walker = Walker::new(dir.path(), &options, &cancel, &progress).unwrap();
        let mut seen = 0;
        let summary = walker.run(|_| Some(()), |()| seen += 1);
        assert_eq!(seen, 0);
        assert!(summary.interrupted);
    }

    #[test]
    fn missing_root_and_bad_globs_are_errors() {
        let cancel = AtomicBool::new(false);
        let progress = Progress::default();
        let options = WalkOptions::default();
        let missing = Path::new("/definitely/not/here");
        assert!(matches!(
            Walker::new(missing, &options, &cancel, &progress),
            Err(Error::Path { .. })
        ));
        let dir = tree();
        let bad = WalkOptions {
            globs: vec!["a[".into()],
            ..WalkOptions::default()
        };
        assert!(matches!(
            Walker::new(dir.path(), &bad, &cancel, &progress),
            Err(Error::Pattern(_))
        ));
    }
}
