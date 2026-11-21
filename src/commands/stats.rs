//! `fsight stats`: what kinds of files live here, how big and how old are they?
//!
//! One walk feeds an [`Accumulator`] on the calling thread, which keeps only
//! running totals, a few small maps, and two bounded "top 10" heaps — never a
//! list of all files.
//!
//! Sizes are file lengths (apparent sizes). A file with several hard links
//! is counted once, under the link whose path sorts first.

use std::cmp::Reverse;
use std::collections::hash_map::Entry as MapEntry;
use std::collections::{BinaryHeap, HashMap};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use super::Context;
use super::find::display_path;
use crate::duration::{format_timestamp, unix_seconds};
use crate::error::Result;
use crate::output::bars::histogram_bar;
use crate::output::color::{Palette, Role};
use crate::output::{RenderText, format_count, plural};
use crate::size::format_size;
use crate::walk::{Entry, EntryKind, FileId, WalkSummary, Walker};

/// How many extensions are listed individually.
pub const TOP_EXTENSIONS: usize = 15;
/// How many files the "largest" and "oldest" lists show.
pub const TOP_FILES: usize = 10;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;
const DAY: Duration = Duration::from_hours(24);

/// Upper bounds (exclusive) of the size buckets; the last bucket is open-ended.
const SIZE_BOUNDS: [u64; 5] = [KIB, 100 * KIB, MIB, 100 * MIB, GIB];
const SIZE_LABELS: [&str; 6] = ["<1K", "1K–100K", "100K–1M", "1M–100M", "100M–1G", ">1G"];

/// Upper bounds (exclusive) of the age buckets, in days.
const AGE_BOUNDS_DAYS: [u32; 4] = [1, 7, 30, 365];
const AGE_LABELS: [&str; 5] = ["<1 day", "<1 week", "<1 month", "<1 year", "older"];

/// Which size bucket a file of `size` bytes belongs to.
pub fn size_bucket(size: u64) -> usize {
    SIZE_BOUNDS
        .iter()
        .position(|&bound| size < bound)
        .unwrap_or(SIZE_BOUNDS.len())
}

/// Which age bucket a file modified at `modified` belongs to. Files with a
/// modification time in the future (clock skew) count as new.
pub fn age_bucket(modified: SystemTime, now: SystemTime) -> usize {
    let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
    AGE_BOUNDS_DAYS
        .iter()
        .position(|&days| age < DAY * days)
        .unwrap_or(AGE_BOUNDS_DAYS.len())
}

/// The lowercase extension of a file name, or `None` (dotfiles have none).
pub fn extension_of(name: &Path) -> Option<String> {
    name.extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
}

/// Keeps the `k` best items seen so far, using a min-heap: the worst kept
/// item sits on top, so each new item costs O(log k) and memory stays O(k).
#[derive(Debug)]
struct TopK<T: Ord> {
    heap: BinaryHeap<Reverse<T>>,
    k: usize,
}

impl<T: Ord> TopK<T> {
    fn new(k: usize) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(k + 1),
            k,
        }
    }

    fn push(&mut self, item: T) {
        self.heap.push(Reverse(item));
        if self.heap.len() > self.k {
            self.heap.pop(); // drop the worst
        }
    }

    /// The kept items, best first.
    fn into_sorted(self) -> Vec<T> {
        // `into_sorted_vec` sorts ascending by `Reverse<T>`, i.e. best `T` first.
        self.heap
            .into_sorted_vec()
            .into_iter()
            .map(|Reverse(item)| item)
            .collect()
    }
}

/// What a worker thread extracts from one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatItem {
    Dir,
    Symlink,
    Other,
    File(FileStat),
}

/// The details of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub extension: Option<String>,
    /// Set only for files with more than one hard link.
    pub linked: Option<FileId>,
}

impl StatItem {
    fn from_entry(entry: &Entry<'_>) -> Option<Self> {
        match entry.kind() {
            // The root directory itself is the subject, not part of the statistics.
            EntryKind::Dir if entry.depth() == 0 => None,
            EntryKind::Dir => Some(StatItem::Dir),
            EntryKind::Symlink => Some(StatItem::Symlink),
            EntryKind::Other => Some(StatItem::Other),
            EntryKind::File => {
                let meta = entry.metadata()?;
                Some(StatItem::File(FileStat {
                    path: entry.path().to_path_buf(),
                    size: meta.len,
                    modified: meta.modified,
                    extension: extension_of(Path::new(entry.file_name())),
                    linked: meta.file_id.filter(|_| meta.links > 1),
                }))
            }
        }
    }
}

/// Totals by entry type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Totals {
    pub files: u64,
    pub directories: u64,
    pub symlinks: u64,
    /// Sockets, FIFOs, devices...
    pub other: u64,
    /// Sum of file lengths in bytes.
    pub size: u64,
    /// Extra hard links to files already counted (not included above).
    pub hard_links: u64,
}

/// Files and bytes for one extension (or one histogram bucket).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Group {
    pub label: String,
    pub files: u64,
    pub size: u64,
}

/// One entry of the "largest" or "oldest" list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileInfo {
    pub path: String,
    pub size: u64,
    /// Unix seconds.
    pub modified: Option<i64>,
    #[serde(skip)]
    modified_time: Option<SystemTime>,
}

/// The result of `fsight stats`.
#[derive(Debug, Clone, Serialize)]
pub struct StatsReport {
    pub totals: Totals,
    /// The largest extensions by total size, `(none)` for files without one.
    pub extensions: Vec<Group>,
    /// Everything beyond the first [`TOP_EXTENSIONS`] extensions, combined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other_extensions: Option<Group>,
    pub size_histogram: Vec<Group>,
    pub age_histogram: Vec<Group>,
    pub largest: Vec<FileInfo>,
    pub oldest: Vec<FileInfo>,
    #[serde(flatten)]
    pub summary: WalkSummary,
}

/// Ranking keys: larger is better. Ties are broken by path, smallest first.
type LargestKey = (u64, Reverse<PathBuf>);
type OldestKey = (Reverse<SystemTime>, Reverse<PathBuf>, u64);

/// Folds [`StatItem`]s into running totals.
struct Accumulator {
    now: SystemTime,
    totals: Totals,
    extensions: HashMap<Option<String>, (u64, u64)>,
    sizes: [(u64, u64); 6],
    ages: [(u64, u64); 5],
    largest: TopK<LargestKey>,
    oldest: TopK<OldestKey>,
    /// Hard-linked files, kept back until the walk ends so that the link
    /// counted is the one whose path sorts first, whatever the walk order.
    linked: HashMap<FileId, FileStat>,
}

impl Accumulator {
    fn new(now: SystemTime) -> Self {
        Self {
            now,
            totals: Totals::default(),
            extensions: HashMap::new(),
            sizes: [(0, 0); 6],
            ages: [(0, 0); 5],
            largest: TopK::new(TOP_FILES),
            oldest: TopK::new(TOP_FILES),
            linked: HashMap::new(),
        }
    }

    fn add(&mut self, item: StatItem) {
        match item {
            StatItem::Dir => self.totals.directories += 1,
            StatItem::Symlink => self.totals.symlinks += 1,
            StatItem::Other => self.totals.other += 1,
            StatItem::File(file) => match file.linked {
                Some(id) => self.hold_back_link(id, file),
                None => self.add_file(file),
            },
        }
    }

    fn hold_back_link(&mut self, id: FileId, file: FileStat) {
        self.totals.hard_links += 1; // corrected in `finish` for the link that is kept
        match self.linked.entry(id) {
            MapEntry::Vacant(slot) => {
                slot.insert(file);
            }
            MapEntry::Occupied(mut slot) => {
                if file.path < slot.get().path {
                    slot.insert(file);
                }
            }
        }
    }

    fn add_file(&mut self, file: FileStat) {
        let totals = &mut self.totals;
        totals.files += 1;
        totals.size = totals.size.saturating_add(file.size);

        let ext = self.extensions.entry(file.extension).or_default();
        ext.0 += 1;
        ext.1 = ext.1.saturating_add(file.size);

        let bucket = &mut self.sizes[size_bucket(file.size)];
        bucket.0 += 1;
        bucket.1 = bucket.1.saturating_add(file.size);

        if let Some(modified) = file.modified {
            let bucket = &mut self.ages[age_bucket(modified, self.now)];
            bucket.0 += 1;
            bucket.1 = bucket.1.saturating_add(file.size);
            self.oldest
                .push((Reverse(modified), Reverse(file.path.clone()), file.size));
        }
        self.largest.push((file.size, Reverse(file.path)));
    }

    fn finish(mut self, root: &Path, summary: WalkSummary) -> StatsReport {
        let held_back: Vec<FileStat> = std::mem::take(&mut self.linked).into_values().collect();
        for file in held_back {
            self.totals.hard_links -= 1; // this link is counted as the file itself
            self.add_file(file);
        }

        let (extensions, other_extensions) = rank_extensions(self.extensions);
        let show = |path: &Path| display_path(path, root).to_string_lossy().into_owned();
        let info = |path: &Path, size: u64, modified: Option<SystemTime>| FileInfo {
            path: show(path),
            size,
            modified: modified.map(unix_seconds),
            modified_time: modified,
        };
        StatsReport {
            totals: self.totals,
            extensions,
            other_extensions,
            size_histogram: buckets(&SIZE_LABELS, &self.sizes),
            age_histogram: buckets(&AGE_LABELS, &self.ages),
            largest: self
                .largest
                .into_sorted()
                .into_iter()
                .map(|(size, Reverse(path))| info(&path, size, None))
                .collect(),
            oldest: self
                .oldest
                .into_sorted()
                .into_iter()
                .map(|(Reverse(modified), Reverse(path), size)| info(&path, size, Some(modified)))
                .collect(),
            summary,
        }
    }
}

/// Sorts extensions by total size and splits off everything beyond the top ones.
fn rank_extensions(map: HashMap<Option<String>, (u64, u64)>) -> (Vec<Group>, Option<Group>) {
    let mut all: Vec<Group> = map
        .into_iter()
        .map(|(ext, (files, size))| Group {
            label: ext.unwrap_or_else(|| "(none)".to_string()),
            files,
            size,
        })
        .collect();
    all.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then(b.files.cmp(&a.files))
            .then_with(|| a.label.cmp(&b.label))
    });
    let rest = all.split_off(all.len().min(TOP_EXTENSIONS));
    let other = (!rest.is_empty()).then(|| Group {
        label: format!(
            "({} other {})",
            rest.len(),
            plural(rest.len() as u64, "extension", "extensions")
        ),
        files: rest.iter().map(|g| g.files).sum(),
        size: rest.iter().map(|g| g.size).sum(),
    });
    (all, other)
}

fn buckets(labels: &[&str], counts: &[(u64, u64)]) -> Vec<Group> {
    labels
        .iter()
        .zip(counts)
        .map(|(label, &(files, size))| Group {
            label: (*label).to_string(),
            files,
            size,
        })
        .collect()
}

/// Runs `fsight stats`.
///
/// # Errors
///
/// Returns an error if `root` cannot be accessed.
pub fn run(ctx: &Context<'_>, root: &Path) -> Result<StatsReport> {
    let walker = Walker::new(root, &ctx.walk, ctx.cancel, ctx.progress)?;
    let mut accumulator = Accumulator::new(SystemTime::now());
    let summary = walker.run(StatItem::from_entry, |item| {
        if let StatItem::File(file) = &item {
            ctx.progress.add_bytes(file.size);
        }
        accumulator.add(item);
    });
    Ok(accumulator.finish(root, summary))
}

// ---------------------------------------------------------------------------
// Text rendering

/// Width of the histogram bars, in terminal cells.
const BAR_WIDTH: usize = 30;

/// `part / whole` as a percentage with one decimal, e.g. `45.2%`.
fn percent_text(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0.0%".to_string();
    }
    let permille = u128::from(part) * 1000 / u128::from(whole);
    format!("{}.{}%", permille / 10, permille % 10)
}

/// Width of the widest label, in characters (labels contain `–`, which is
/// three bytes but one character).
fn label_width(groups: &[Group]) -> usize {
    groups
        .iter()
        .map(|g| g.label.chars().count())
        .max()
        .unwrap_or(0)
}

impl RenderText for StatsReport {
    fn render_text(&self, out: &mut dyn Write, palette: Palette) -> io::Result<()> {
        self.render_totals(out, palette)?;
        self.render_extensions(out, palette)?;
        render_histogram(
            out,
            palette,
            ("File sizes", "size"),
            &self.size_histogram,
            self.totals.files,
        )?;
        render_histogram(
            out,
            palette,
            ("File ages (by modification time)", "age"),
            &self.age_histogram,
            self.totals.files,
        )?;
        render_file_list(out, palette, "Largest files", &self.largest, |file| {
            format!("{:>10}", format_size(file.size))
        })?;
        render_file_list(out, palette, "Oldest files (UTC)", &self.oldest, |file| {
            file.modified_time
                .map_or_else(|| "-".to_string(), format_timestamp)
        })
    }
}

impl StatsReport {
    fn render_totals(&self, out: &mut dyn Write, palette: Palette) -> io::Result<()> {
        let t = &self.totals;
        writeln!(out, "{}", palette.paint(Role::Header, "Totals"))?;
        let rows = [
            ("Files", format_count(t.files)),
            ("Directories", format_count(t.directories)),
            ("Symlinks", format_count(t.symlinks)),
            ("Total size", format_size(t.size)),
        ];
        let width = rows.iter().map(|(_, value)| value.len()).max().unwrap_or(0);
        for (name, value) in rows {
            writeln!(out, "  {name:<12}{value:>width$}")?;
        }
        if t.other > 0 {
            writeln!(out, "  {:<12}{:>width$}", "Other", format_count(t.other))?;
        }
        if t.hard_links > 0 {
            let note = format!(
                "({} extra hard {} counted once)",
                format_count(t.hard_links),
                plural(t.hard_links, "link", "links")
            );
            writeln!(out, "  {}", palette.paint(Role::Dim, &note))?;
        }
        Ok(())
    }

    fn render_extensions(&self, out: &mut dyn Write, palette: Palette) -> io::Result<()> {
        writeln!(out)?;
        writeln!(
            out,
            "{}",
            palette.paint(Role::Header, "Top extensions by size")
        )?;
        if self.extensions.is_empty() {
            return writeln!(out, "  (no files)");
        }
        let mut rows: Vec<&Group> = self.extensions.iter().collect();
        rows.extend(&self.other_extensions);
        let width = rows
            .iter()
            .map(|g| g.label.chars().count())
            .chain(["extension".len()])
            .max()
            .unwrap_or(0);
        let heading = format!(
            "{:<width$}  {:>9}  {:>10}  {:>6}",
            "extension", "files", "size", "%"
        );
        writeln!(out, "  {}", palette.paint(Role::Dim, &heading))?;
        let max = self.extensions.first().map_or(0, |g| g.size);
        for group in rows {
            write!(
                out,
                "  {:<width$}  {:>9}  {:>10}  {:>6}",
                group.label,
                format_count(group.files),
                format_size(group.size),
                percent_text(group.size, self.totals.size),
            )?;
            write_bar(out, palette, group.size, max)?;
        }
        Ok(())
    }
}

/// A histogram table: one row per bucket, with a bar scaled to the fullest bucket.
fn render_histogram(
    out: &mut dyn Write,
    palette: Palette,
    (title, label_heading): (&str, &str),
    groups: &[Group],
    total_files: u64,
) -> io::Result<()> {
    writeln!(out)?;
    writeln!(out, "{}", palette.paint(Role::Header, title))?;
    let width = label_width(groups).max(label_heading.len());
    let heading = format!(
        "{label_heading:<width$}  {:>9}  {:>6}  {:>10}",
        "files", "%", "size"
    );
    writeln!(out, "  {}", palette.paint(Role::Dim, &heading))?;
    let max = groups.iter().map(|g| g.files).max().unwrap_or(0);
    for group in groups {
        write!(
            out,
            "  {:<width$}  {:>9}  {:>6}  {:>10}",
            group.label,
            format_count(group.files),
            percent_text(group.files, total_files),
            format_size(group.size),
        )?;
        write_bar(out, palette, group.files, max)?;
    }
    Ok(())
}

/// Ends a table row with its bar (nothing for an empty bucket, so no trailing spaces).
fn write_bar(out: &mut dyn Write, palette: Palette, value: u64, max: u64) -> io::Result<()> {
    let bar = histogram_bar(value, max, BAR_WIDTH);
    if bar.is_empty() {
        writeln!(out)
    } else {
        writeln!(out, "  {}", palette.paint(Role::Bar, &bar))
    }
}

fn render_file_list(
    out: &mut dyn Write,
    palette: Palette,
    title: &str,
    files: &[FileInfo],
    column: impl Fn(&FileInfo) -> String,
) -> io::Result<()> {
    writeln!(out)?;
    writeln!(out, "{}", palette.paint(Role::Header, title))?;
    if files.is_empty() {
        return writeln!(out, "  (no files)");
    }
    for file in files {
        writeln!(
            out,
            "  {}  {}",
            palette.paint(Role::Size, &column(file)),
            file.path
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn size_buckets_have_exclusive_upper_bounds() {
        assert_eq!(size_bucket(0), 0);
        assert_eq!(size_bucket(KIB - 1), 0);
        assert_eq!(size_bucket(KIB), 1);
        assert_eq!(size_bucket(100 * KIB - 1), 1);
        assert_eq!(size_bucket(100 * KIB), 2);
        assert_eq!(size_bucket(MIB), 3);
        assert_eq!(size_bucket(100 * MIB), 4);
        assert_eq!(size_bucket(GIB - 1), 4);
        assert_eq!(size_bucket(GIB), 5);
        assert_eq!(size_bucket(u64::MAX), 5);
    }

    #[test]
    fn age_buckets_by_days_and_future_times_count_as_new() {
        let now = UNIX_EPOCH + DAY * 1000;
        let ago = |d: Duration| now - d;
        assert_eq!(age_bucket(now, now), 0);
        assert_eq!(age_bucket(ago(Duration::from_secs(86_399)), now), 0);
        assert_eq!(age_bucket(ago(DAY), now), 1);
        assert_eq!(age_bucket(ago(DAY * 6), now), 1);
        assert_eq!(age_bucket(ago(DAY * 7), now), 2);
        assert_eq!(age_bucket(ago(DAY * 30), now), 3);
        assert_eq!(age_bucket(ago(DAY * 364), now), 3);
        assert_eq!(age_bucket(ago(DAY * 365), now), 4);
        assert_eq!(age_bucket(now + DAY, now), 0, "future: clock skew");
    }

    #[test]
    fn extensions_are_lowercase_and_dotfiles_have_none() {
        assert_eq!(extension_of(Path::new("Photo.JPG")).as_deref(), Some("jpg"));
        assert_eq!(
            extension_of(Path::new("archive.tar.gz")).as_deref(),
            Some("gz")
        );
        assert_eq!(extension_of(Path::new("Makefile")), None);
        assert_eq!(extension_of(Path::new(".bashrc")), None);
    }

    #[test]
    fn top_k_keeps_the_best_items_in_order() {
        let mut top = TopK::new(3);
        for n in [5, 1, 9, 3, 7, 9] {
            top.push(n);
        }
        assert_eq!(top.into_sorted(), [9, 9, 7]);
    }

    fn file(path: &str, size: u64, ext: Option<&str>) -> StatItem {
        StatItem::File(FileStat {
            path: PathBuf::from(path),
            size,
            modified: Some(UNIX_EPOCH + Duration::from_secs(size)),
            extension: ext.map(str::to_string),
            linked: None,
        })
    }

    #[test]
    fn accumulator_counts_everything_and_ranks_extensions() {
        let mut acc = Accumulator::new(UNIX_EPOCH + DAY * 10_000);
        acc.add(StatItem::Dir);
        acc.add(StatItem::Symlink);
        acc.add(file("a.rs", 100, Some("rs")));
        acc.add(file("b.rs", 300, Some("rs")));
        acc.add(file("c.md", 500, Some("md")));
        acc.add(file("Makefile", 50, None));
        let report = acc.finish(Path::new("x"), WalkSummary::default());

        assert_eq!(report.totals.files, 4);
        assert_eq!((report.totals.directories, report.totals.symlinks), (1, 1));
        assert_eq!(report.totals.size, 950);
        let labels: Vec<&str> = report.extensions.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(labels, ["md", "rs", "(none)"]);
        assert_eq!(report.extensions[1].files, 2);
        assert_eq!(report.size_histogram[0].files, 4, "all below 1 KiB");
        assert_eq!(report.largest[0].path, "c.md");
        assert_eq!(
            report.oldest[0].path, "Makefile",
            "smallest mtime in this fixture"
        );
    }

    #[test]
    fn extensions_beyond_the_top_are_combined() {
        let mut acc = Accumulator::new(UNIX_EPOCH);
        for i in 0..20u64 {
            let ext = format!("e{i:02}");
            acc.add(file(&format!("f.{ext}"), 100 + i, Some(&ext)));
        }
        let report = acc.finish(Path::new("x"), WalkSummary::default());
        assert_eq!(report.extensions.len(), TOP_EXTENSIONS);
        assert_eq!(report.extensions[0].label, "e19");
        let other = report.other_extensions.unwrap();
        assert_eq!(other.label, "(5 other extensions)");
        assert_eq!(other.files, 5);
        assert_eq!(other.size, 100 + 101 + 102 + 103 + 104);
    }

    #[test]
    fn a_hard_linked_file_counts_once_under_its_first_path() {
        let linked = |path: &str| {
            StatItem::File(FileStat {
                path: PathBuf::from(path),
                size: 1000,
                modified: None,
                extension: None,
                linked: Some(FileId {
                    device: 1,
                    inode: 42,
                }),
            })
        };
        let mut acc = Accumulator::new(UNIX_EPOCH);
        acc.add(linked("z/link"));
        acc.add(linked("a/original"));
        acc.add(linked("m/third"));
        let report = acc.finish(Path::new("x"), WalkSummary::default());
        assert_eq!((report.totals.files, report.totals.size), (1, 1000));
        assert_eq!(report.totals.hard_links, 2);
        assert_eq!(report.largest[0].path, "a/original");
    }

    #[test]
    fn percentages_have_one_decimal() {
        assert_eq!(percent_text(1, 3), "33.3%");
        assert_eq!(percent_text(2, 3), "66.6%");
        assert_eq!(percent_text(5, 5), "100.0%");
        assert_eq!(percent_text(1, 0), "0.0%");
    }
}
