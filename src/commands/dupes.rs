//! `fsight dupes`: report duplicate files.
//!
//! **This command is read-only.** It opens files for reading and nothing
//! else; there is deliberately no option to delete, move or link anything.
//!
//! Comparing every file with every other file would be far too slow, so the
//! candidates are narrowed down in three stages, each one more expensive per
//! file but applied to fewer files:
//!
//! 1. **Group by exact size.** Files of different sizes cannot be equal. This
//!    needs only the metadata the walk already read. Groups of one are dropped.
//! 2. **Hash the first 16 KiB** (BLAKE3) of every remaining file. Files that
//!    differ near the start — most non-duplicates — are dropped here after
//!    reading at most 16 KiB each.
//! 3. **Hash the whole file** (BLAKE3, in parallel) for the survivors. Files
//!    of at most 16 KiB skip this stage: stage 2 already hashed all of them.
//!
//! After every stage, only groups with at least two members are kept.
//! BLAKE3 hashes are 256 bits, so two different files producing the same hash
//! is not a practical concern.

use std::collections::HashMap;
use std::fs::File;
use std::hash::Hash;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use serde::Serialize;

use super::Context;
use super::find::{display_path, has_extension, normalize_extensions};
use crate::error::Result;
use crate::output::color::{Palette, Role};
use crate::output::{RenderText, format_count, plural};
use crate::progress::Progress;
use crate::size::format_size;
use crate::walk::{EntryKind, ErrorLog, FileId, WalkSummary, Walker};

/// How much of each file stage 2 hashes.
pub const PREFIX_LEN: u64 = 16 * 1024;

/// A BLAKE3 hash.
type Digest = [u8; 32];

/// Options of `fsight dupes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupesOptions {
    /// Files smaller than this are ignored (empty files are controlled by
    /// `include_empty` instead).
    pub min_size: u64,
    pub extensions: Vec<String>,
    pub include_empty: bool,
}

/// A file found by the walk that might have duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub size: u64,
    pub file_id: Option<FileId>,
}

/// One distinct file on disk with every path (hard link) that leads to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inode {
    /// The path that sorts first.
    pub path: PathBuf,
    pub size: u64,
    /// Other hard links to the same data; they are not duplicates.
    pub links: Vec<PathBuf>,
}

/// Should a file of `size` bytes take part at all?
fn wanted_size(size: u64, options: &DupesOptions) -> bool {
    if size == 0 {
        options.include_empty
    } else {
        size >= options.min_size
    }
}

/// Stage 1: group candidates by exact size and drop sizes that occur once.
///
/// Within a size group, hard links (same device and inode) are merged into one
/// [`Inode`] first, so that a file and its hard link do not count as
/// duplicates. Returns the groups and the number of hard links merged.
pub fn group_by_size(candidates: Vec<Candidate>) -> (Vec<Vec<Inode>>, u64) {
    let mut by_size: HashMap<u64, Vec<Candidate>> = HashMap::new();
    for candidate in candidates {
        by_size.entry(candidate.size).or_default().push(candidate);
    }
    let mut merged_links = 0;
    let groups = by_size
        .into_values()
        .filter(|group| group.len() >= 2)
        .filter_map(|group| {
            let inodes = merge_hard_links(group);
            merged_links += inodes.iter().map(|i| i.links.len() as u64).sum::<u64>();
            (inodes.len() >= 2).then_some(inodes)
        })
        .collect();
    (groups, merged_links)
}

/// Turns same-sized candidates into distinct inodes.
fn merge_hard_links(mut group: Vec<Candidate>) -> Vec<Inode> {
    group.sort_by(|a, b| a.path.cmp(&b.path)); // the first path becomes the primary one
    let mut inodes: Vec<Inode> = Vec::with_capacity(group.len());
    let mut seen: HashMap<FileId, usize> = HashMap::new();
    for candidate in group {
        if let Some(index) = candidate.file_id.and_then(|id| seen.get(&id).copied()) {
            inodes[index].links.push(candidate.path);
        } else {
            if let Some(id) = candidate.file_id {
                seen.insert(id, inodes.len());
            }
            inodes.push(Inode {
                path: candidate.path,
                size: candidate.size,
                links: Vec::new(),
            });
        }
    }
    inodes
}

/// Shared state for the hashing stages.
struct Stage<'a> {
    cancel: &'a AtomicBool,
    progress: &'a Progress,
    errors: &'a ErrorLog,
}

impl Stage<'_> {
    /// Splits every group by `key` (computed in parallel) and keeps the
    /// sub-groups that still have at least two members. Used for stages 2 and 3.
    fn refine<K, F>(&self, groups: Vec<Vec<Inode>>, key: &F) -> Vec<Vec<Inode>>
    where
        K: Eq + Hash + Send,
        F: Fn(&Path) -> io::Result<K> + Sync,
    {
        groups
            .into_par_iter()
            .flat_map_iter(|group| {
                let keyed: Vec<(K, Inode)> = group
                    .into_par_iter()
                    .filter_map(|inode| self.key_of(inode, key))
                    .collect();
                split_by_key(keyed)
            })
            .collect()
    }

    /// Computes one file's key; on failure the file is dropped and the error counted.
    fn key_of<K, F>(&self, inode: Inode, key: &F) -> Option<(K, Inode)>
    where
        F: Fn(&Path) -> io::Result<K>,
    {
        if self.cancel.load(Ordering::Relaxed) {
            return None;
        }
        self.progress.add_items(1);
        match key(&inode.path) {
            Ok(value) => Some((value, inode)),
            Err(err) => {
                self.errors
                    .record(&format_args!("{}: {err}", inode.path.display()));
                None
            }
        }
    }
}

/// Buckets `(key, inode)` pairs by key and keeps buckets with two or more inodes.
fn split_by_key<K: Eq + Hash>(keyed: Vec<(K, Inode)>) -> Vec<Vec<Inode>> {
    let mut buckets: HashMap<K, Vec<Inode>> = HashMap::new();
    for (key, inode) in keyed {
        buckets.entry(key).or_default().push(inode);
    }
    buckets
        .into_values()
        .filter(|bucket| bucket.len() >= 2)
        .collect()
}

/// Runs stages 2 and 3 on the size groups from stage 1.
///
/// The hash functions are parameters so the tests can count how often each
/// stage reads a file; the real ones are [`hash_prefix`] and [`hash_full`].
fn confirm_duplicates<P, F>(
    groups: Vec<Vec<Inode>>,
    prefix_hash: &P,
    full_hash: &F,
    stage: &Stage<'_>,
) -> Vec<Vec<Inode>>
where
    P: Fn(&Path) -> io::Result<Digest> + Sync,
    F: Fn(&Path) -> io::Result<Digest> + Sync,
{
    // Stage 2: hash the first 16 KiB.
    stage
        .progress
        .start_stage("Hashing file starts (stage 2/3)");
    let groups = stage.refine(groups, prefix_hash);

    // Stage 3: hash whole files, but only where stage 2 did not already
    // cover the whole file. All files in a group have the same size.
    stage
        .progress
        .start_stage("Hashing whole files (stage 3/3)");
    let (mut complete, partial): (Vec<_>, Vec<_>) = groups
        .into_iter()
        .partition(|group| group.first().is_some_and(|inode| inode.size <= PREFIX_LEN));
    complete.extend(stage.refine(partial, full_hash));
    complete
}

/// BLAKE3 hash of the first [`PREFIX_LEN`] bytes of a file.
///
/// # Errors
///
/// Returns any error from opening or reading the file.
pub fn hash_prefix(path: &Path) -> io::Result<Digest> {
    let file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file.take(PREFIX_LEN))?;
    Ok(*hasher.finalize().as_bytes())
}

/// BLAKE3 hash of a whole file, read in 64 KiB chunks (never all at once).
///
/// # Errors
///
/// Returns any error from opening or reading the file.
pub fn hash_full(path: &Path) -> io::Result<Digest> {
    let file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file)?;
    Ok(*hasher.finalize().as_bytes())
}

/// One path in a duplicate group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DupeFile {
    pub path: String,
    /// Other names (hard links) for this same file.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hard_links: Vec<String>,
}

/// Files with identical contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DupeGroup {
    /// Size of each copy in bytes.
    pub size: u64,
    /// Bytes that could be freed by keeping only one copy.
    pub wasted: u64,
    pub files: Vec<DupeFile>,
}

/// The result of `fsight dupes`.
#[derive(Debug, Clone, Serialize)]
pub struct DupeReport {
    /// Sorted by wasted space, largest first.
    pub groups: Vec<DupeGroup>,
    /// Files that are copies (counted once per distinct file, not per hard link).
    pub duplicate_files: u64,
    pub wasted: u64,
    /// Files considered (after size and extension filters).
    pub files_checked: u64,
    /// Hard links recognised as the same file rather than duplicates.
    pub hard_links: u64,
    #[serde(flatten)]
    pub summary: WalkSummary,
}

/// Runs `fsight dupes`.
///
/// # Errors
///
/// Returns an error if `root` cannot be accessed.
pub fn run(ctx: &Context<'_>, root: &Path, options: &DupesOptions) -> Result<DupeReport> {
    let (candidates, mut summary) = collect_candidates(ctx, root, options)?;
    let files_checked = candidates.len() as u64;
    let (groups, hard_links) = group_by_size(candidates);
    let errors = ErrorLog::default();
    let stage = Stage {
        cancel: ctx.cancel,
        progress: ctx.progress,
        errors: &errors,
    };
    let groups = confirm_duplicates(groups, &hash_prefix, &hash_full, &stage);

    summary.errors += errors.count();
    summary.interrupted |= ctx.cancel.load(Ordering::Relaxed);
    Ok(build_report(
        groups,
        root,
        files_checked,
        hard_links,
        summary,
    ))
}

/// Walks `root` and returns the files that pass the size and extension filters.
fn collect_candidates(
    ctx: &Context<'_>,
    root: &Path,
    options: &DupesOptions,
) -> Result<(Vec<Candidate>, WalkSummary)> {
    let extensions = normalize_extensions(&options.extensions);
    let walker = Walker::new(root, &ctx.walk, ctx.cancel, ctx.progress)?;
    let mut candidates = Vec::new();
    let summary = walker.run(
        |entry| {
            if entry.kind() != EntryKind::File {
                return None;
            }
            let name = entry.file_name().as_encoded_bytes();
            if !extensions.is_empty() && !extensions.iter().any(|ext| has_extension(name, ext)) {
                return None;
            }
            let meta = entry.metadata()?;
            wanted_size(meta.len, options).then(|| Candidate {
                path: entry.path().to_path_buf(),
                size: meta.len,
                file_id: meta.file_id,
            })
        },
        |candidate| candidates.push(candidate),
    );
    Ok((candidates, summary))
}

fn build_report(
    groups: Vec<Vec<Inode>>,
    root: &Path,
    files_checked: u64,
    hard_links: u64,
    summary: WalkSummary,
) -> DupeReport {
    let show = |path: &Path| display_path(path, root).to_string_lossy().into_owned();
    let mut groups: Vec<DupeGroup> = groups
        .into_iter()
        .map(|mut inodes| {
            inodes.sort_by(|a, b| a.path.cmp(&b.path));
            let size = inodes.first().map_or(0, |inode| inode.size);
            let copies = inodes.len() as u64;
            DupeGroup {
                size,
                wasted: size.saturating_mul(copies.saturating_sub(1)),
                files: inodes
                    .iter()
                    .map(|inode| DupeFile {
                        path: show(&inode.path),
                        hard_links: inode.links.iter().map(|link| show(link)).collect(),
                    })
                    .collect(),
            }
        })
        .collect();
    // Most wasted space first; ties by size, then by the first path.
    groups.sort_by(|a, b| {
        b.wasted
            .cmp(&a.wasted)
            .then(b.size.cmp(&a.size))
            .then_with(|| a.files[0].path.cmp(&b.files[0].path))
    });

    DupeReport {
        duplicate_files: groups.iter().map(|g| g.files.len() as u64).sum(),
        wasted: groups.iter().map(|g| g.wasted).sum(),
        groups,
        files_checked,
        hard_links,
        summary,
    }
}

impl RenderText for DupeReport {
    fn render_text(&self, out: &mut dyn Write, palette: Palette) -> io::Result<()> {
        for group in &self.groups {
            let count = group.files.len() as u64;
            writeln!(
                out,
                "{} × {} {} ({} wasted)",
                palette.paint(Role::Size, &format_size(group.size)),
                count,
                plural(count, "copy", "copies"),
                format_size(group.wasted),
            )?;
            for file in &group.files {
                write!(out, "  {}", file.path)?;
                if !file.hard_links.is_empty() {
                    let n = file.hard_links.len() as u64;
                    let note = format!(
                        "(+{n} hard {}: {})",
                        plural(n, "link", "links"),
                        file.hard_links.join(", ")
                    );
                    write!(out, "  {}", palette.paint(Role::Dim, &note))?;
                }
                writeln!(out)?;
            }
            writeln!(out)?;
        }
        let groups = self.groups.len() as u64;
        writeln!(
            out,
            "Found {} duplicate {}, {} {}, {} wasted",
            format_count(groups),
            plural(groups, "group", "groups"),
            format_count(self.duplicate_files),
            plural(self.duplicate_files, "file", "files"),
            palette.paint(Role::Size, &format_size(self.wasted)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    /// An in-memory "file system" whose hash functions count their calls.
    struct FakeFiles {
        contents: HashMap<PathBuf, Vec<u8>>,
        prefix_calls: AtomicUsize,
        full_calls: AtomicUsize,
        full_hashed: Mutex<Vec<PathBuf>>,
    }

    impl FakeFiles {
        fn new(files: &[(&str, Vec<u8>)]) -> Self {
            Self {
                contents: files
                    .iter()
                    .map(|(path, data)| (PathBuf::from(path), data.clone()))
                    .collect(),
                prefix_calls: AtomicUsize::new(0),
                full_calls: AtomicUsize::new(0),
                full_hashed: Mutex::new(Vec::new()),
            }
        }

        fn candidates(&self) -> Vec<Candidate> {
            self.contents
                .iter()
                .map(|(path, data)| Candidate {
                    path: path.clone(),
                    size: data.len() as u64,
                    file_id: None,
                })
                .collect()
        }

        // The fake hashers never fail, but must have the same signature as the real ones.
        #[allow(clippy::unnecessary_wraps)]
        fn prefix(&self, path: &Path) -> io::Result<Digest> {
            self.prefix_calls.fetch_add(1, Ordering::Relaxed);
            let data = &self.contents[path];
            let end = data.len().min(usize::try_from(PREFIX_LEN).unwrap());
            Ok(*blake3::hash(&data[..end]).as_bytes())
        }

        #[allow(clippy::unnecessary_wraps)]
        fn full(&self, path: &Path) -> io::Result<Digest> {
            self.full_calls.fetch_add(1, Ordering::Relaxed);
            self.full_hashed.lock().unwrap().push(path.to_path_buf());
            Ok(*blake3::hash(&self.contents[path]).as_bytes())
        }

        /// Runs all three stages and returns the duplicate groups as sorted path lists.
        fn duplicates(&self) -> Vec<Vec<String>> {
            let (groups, _) = group_by_size(self.candidates());
            let (cancel, progress, errors) = (
                AtomicBool::new(false),
                Progress::default(),
                ErrorLog::default(),
            );
            let stage = Stage {
                cancel: &cancel,
                progress: &progress,
                errors: &errors,
            };
            let groups = confirm_duplicates(
                groups,
                &|p: &Path| self.prefix(p),
                &|p: &Path| self.full(p),
                &stage,
            );
            let mut paths: Vec<Vec<String>> = groups
                .into_iter()
                .map(|group| {
                    let mut names: Vec<String> =
                        group.iter().map(|i| i.path.display().to_string()).collect();
                    names.sort();
                    names
                })
                .collect();
            paths.sort();
            paths
        }
    }

    fn big(fill: u8, tail: u8) -> Vec<u8> {
        let mut data = vec![fill; 40 * 1024];
        *data.last_mut().unwrap() = tail;
        data
    }

    #[test]
    fn stage_one_drops_unique_sizes_without_reading_anything() {
        let files = FakeFiles::new(&[("a", vec![1; 10]), ("b", vec![1; 10]), ("c", vec![1; 11])]);
        let (groups, _) = group_by_size(files.candidates());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
        assert_eq!(files.duplicates(), [["a", "b"]]);
        assert_eq!(
            files.prefix_calls.load(Ordering::Relaxed),
            2,
            "c was never opened"
        );
    }

    #[test]
    fn stage_two_drops_files_whose_start_differs_before_full_hashing() {
        let files = FakeFiles::new(&[
            ("same1", big(7, 0)),
            ("same2", big(7, 0)),
            ("other_start", big(9, 0)), // same size, different first 16 KiB
            ("other_tail", big(7, 1)),  // same first 16 KiB, different end
        ]);
        assert_eq!(files.duplicates(), [["same1", "same2"]]);
        assert_eq!(files.prefix_calls.load(Ordering::Relaxed), 4);
        // Stage 3 only saw the three files that shared a prefix.
        let mut fully = files.full_hashed.lock().unwrap().clone();
        fully.sort();
        assert_eq!(
            fully,
            [PathBuf::from("other_tail"), "same1".into(), "same2".into()]
        );
    }

    #[test]
    fn stage_three_is_skipped_for_files_that_fit_in_the_prefix() {
        let small = vec![5u8; usize::try_from(PREFIX_LEN).unwrap()]; // exactly 16 KiB
        let files = FakeFiles::new(&[("x", small.clone()), ("y", small), ("z", vec![6; 100])]);
        assert_eq!(files.duplicates(), [["x", "y"]]);
        assert_eq!(files.full_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn hard_links_are_one_file_not_duplicates() {
        let id = |inode| Some(FileId { device: 1, inode });
        let candidate = |path: &str, file_id| Candidate {
            path: PathBuf::from(path),
            size: 100,
            file_id,
        };
        // Only two names for the same inode: nothing to report.
        let (groups, links) = group_by_size(vec![candidate("a", id(1)), candidate("b", id(1))]);
        assert_eq!(groups, Vec::<Vec<Inode>>::new());
        assert_eq!(links, 1);
        // A real copy plus a hard link: one group of two distinct files.
        let (groups, links) = group_by_size(vec![
            candidate("b", id(1)),
            candidate("a", id(1)),
            candidate("c", id(2)),
        ]);
        assert_eq!(links, 1);
        assert_eq!(groups.len(), 1);
        let inodes = &groups[0];
        assert_eq!(inodes.len(), 2);
        assert_eq!(
            inodes[0].path,
            Path::new("a"),
            "the first path is the primary one"
        );
        assert_eq!(inodes[0].links, [PathBuf::from("b")]);
    }

    #[test]
    fn empty_files_and_min_size() {
        let opts = |min_size, include_empty| DupesOptions {
            min_size,
            extensions: Vec::new(),
            include_empty,
        };
        assert!(!wanted_size(0, &opts(1, false)));
        assert!(wanted_size(0, &opts(1, true)));
        assert!(
            wanted_size(0, &opts(100, true)),
            "empty files ignore min-size"
        );
        assert!(!wanted_size(99, &opts(100, true)));
        assert!(wanted_size(100, &opts(100, false)));
    }

    #[test]
    fn report_sorts_groups_by_wasted_space_and_renders_a_summary() {
        let inode = |path: &str, size| Inode {
            path: PathBuf::from(path),
            size,
            links: Vec::new(),
        };
        let groups = vec![
            vec![inode("s1", 10), inode("s2", 10), inode("s3", 10)], // 20 wasted
            vec![inode("b2", 1000), inode("b1", 1000)],              // 1000 wasted
        ];
        let report = build_report(groups, Path::new("x"), 5, 0, WalkSummary::default());
        assert_eq!(report.groups[0].size, 1000);
        assert_eq!(
            report.groups[0].files[0].path, "b1",
            "paths sorted within a group"
        );
        assert_eq!((report.duplicate_files, report.wasted), (5, 1020));

        let mut out = Vec::new();
        report.render_text(&mut out, Palette::plain()).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with("1000 B × 2 copies (1000 B wasted)\n  b1\n  b2\n\n"),
            "{text}"
        );
        assert!(
            text.ends_with("Found 2 duplicate groups, 5 files, 1020 B wasted\n"),
            "{text}"
        );
    }
}
