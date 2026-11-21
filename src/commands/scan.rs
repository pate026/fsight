//! `fsight scan`: how much space does each directory use?
//!
//! The walk produces one [`ScanItem`] per entry (on the worker threads). The
//! calling thread feeds them into a [`TreeBuilder`], which arranges them in
//! a [`DiskTree`]: an *arena* — one `Vec<Node>` in which nodes refer to each
//! other by index ([`NodeId`]) instead of by pointer. Arenas are the usual
//! way to build trees in Rust without fighting the borrow checker.
//!
//! Memory stays proportional to what is displayed: entries deeper than the
//! display depth are not stored at all; their sizes are added to their
//! ancestor at the display depth. (The TUI asks for unlimited depth, because
//! the user can navigate anywhere.)

use std::collections::HashMap;
use std::collections::hash_map::Entry as MapEntry;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use super::Context;
use crate::error::Result;
use crate::walk::{Entry, EntryKind, EntryMeta, FileId, WalkSummary, Walker};

/// Options of `fsight scan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOptions {
    /// Levels displayed below the root.
    pub depth: usize,
    /// Largest children shown per directory; 0 shows all.
    pub top: usize,
    /// Use file lengths instead of allocated blocks.
    pub apparent: bool,
    /// Hide entries smaller than this many bytes.
    pub min_size: u64,
}

/// Index of a [`Node`] inside a [`DiskTree`]. The default is the root.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(usize);

/// One file or directory in a [`DiskTree`].
#[derive(Debug, Clone)]
pub struct Node {
    /// File name (for the root: the path as given on the command line).
    pub name: OsString,
    pub kind: EntryKind,
    /// Total size of this entry and everything below it, in bytes.
    pub size: u64,
    /// Number of non-directory entries in this subtree (1 for a file).
    pub files: u64,
    pub modified: Option<SystemTime>,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

/// A disk usage tree. Built once, then only read.
#[derive(Debug, Clone)]
pub struct DiskTree {
    nodes: Vec<Node>,
    root_path: PathBuf,
    /// Number of entries visited by the walk.
    pub entries: u64,
    pub summary: WalkSummary,
}

impl DiskTree {
    /// The root node (always present).
    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// Looks up a node. Ids come from this tree, so the index is always valid.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0]
    }

    /// Number of nodes in the tree.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Always `false`: a tree has at least its root.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The full path of a node: the root path joined with every name below it.
    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut names = Vec::new();
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id);
            if node.parent.is_some() {
                names.push(node.name.as_os_str());
            }
            current = node.parent;
        }
        let mut path = self.root_path.clone();
        path.extend(names.iter().rev());
        path
    }
}

/// What a worker thread extracts from one walk entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanItem {
    /// Path relative to the root (empty for the root itself).
    pub rel_path: PathBuf,
    pub depth: usize,
    pub kind: EntryKind,
    /// Size counted for this entry alone (allocated or apparent).
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub file_id: Option<FileId>,
    pub links: u64,
}

impl ScanItem {
    /// Builds an item from a walk entry; `None` if its metadata is unreadable.
    pub fn from_entry(entry: &Entry<'_>, root: &Path, apparent: bool) -> Option<Self> {
        let meta: EntryMeta = entry.metadata()?;
        let rel_path = entry.path().strip_prefix(root).ok()?.to_path_buf();
        let size = match (apparent, meta.kind) {
            // A directory's own "length" is a file-system detail (4096 on
            // ext4, varying on btrfs), not content. In apparent mode only
            // file contents count, so results are the same everywhere.
            (true, EntryKind::Dir) => 0,
            (true, _) => meta.len,
            (false, _) => meta.allocated,
        };
        Some(Self {
            rel_path,
            depth: entry.depth(),
            kind: meta.kind,
            size,
            modified: meta.modified,
            file_id: meta.file_id,
            links: meta.links,
        })
    }
}

/// The current owner of a hard-linked file: the path that is credited with its size.
struct LinkOwner {
    rel_path: PathBuf,
    node: NodeId,
    size: u64,
}

/// Assembles [`ScanItem`]s, arriving in any order, into a [`DiskTree`].
pub struct TreeBuilder {
    nodes: Vec<Node>,
    /// Relative path of every directory node → its id. The root is `""`.
    dirs: HashMap<PathBuf, NodeId>,
    /// Entries deeper than this are folded into their ancestor at this depth.
    max_depth: usize,
    /// Files seen more than once (hard links), keyed by device and inode.
    links: HashMap<FileId, LinkOwner>,
    /// With `--follow`, the same file can be reached through different
    /// symlinks even without hard links, so every entry is deduplicated.
    dedup_everything: bool,
    entries: u64,
}

impl TreeBuilder {
    /// Starts a tree for `root`, storing nodes down to `max_depth`.
    pub fn new(root: &Path, max_depth: usize, dedup_everything: bool) -> Self {
        let root_node = Node {
            name: root.as_os_str().to_os_string(),
            kind: EntryKind::Dir,
            size: 0,
            files: 0,
            modified: None,
            parent: None,
            children: Vec::new(),
        };
        Self {
            nodes: vec![root_node],
            dirs: HashMap::from([(PathBuf::new(), NodeId(0))]),
            max_depth,
            links: HashMap::new(),
            dedup_everything,
            entries: 0,
        }
    }

    /// Adds one entry. Returns the number of bytes credited to the tree,
    /// which is 0 for an extra link to a file that was already counted.
    pub fn add(&mut self, item: &ScanItem) -> u64 {
        self.entries += 1;
        let node = if item.depth <= self.max_depth {
            self.upsert(item)
        } else {
            // Too deep to display: credit the ancestor at the display depth.
            let ancestor: PathBuf = item.rel_path.components().take(self.max_depth).collect();
            self.ensure_dir(&ancestor)
        };

        let size = self.credited_size(item, node);
        let target = &mut self.nodes[node.0];
        target.size = target.size.saturating_add(size);
        if item.kind != EntryKind::Dir {
            target.files += 1;
        }
        size
    }

    /// Creates the node for `item`, or fills in a placeholder directory that
    /// was created earlier because one of its children arrived first.
    fn upsert(&mut self, item: &ScanItem) -> NodeId {
        if let Some(&id) = self.dirs.get(&item.rel_path) {
            let node = &mut self.nodes[id.0];
            node.kind = item.kind;
            node.modified = item.modified;
            return id;
        }
        let parent_path = item.rel_path.parent().unwrap_or_else(|| Path::new(""));
        let parent = self.ensure_dir(parent_path);
        let id = self.push_node(parent, &item.rel_path, item.kind, item.modified);
        if item.kind == EntryKind::Dir {
            self.dirs.insert(item.rel_path.clone(), id);
        }
        id
    }

    /// Returns the directory node for `rel_path`, creating it (and any
    /// missing ancestors) as placeholders if needed.
    fn ensure_dir(&mut self, rel_path: &Path) -> NodeId {
        if let Some(&id) = self.dirs.get(rel_path) {
            return id;
        }
        // Walk up to the nearest existing ancestor (the root always exists)...
        let mut missing = vec![rel_path];
        let mut current = rel_path;
        let mut id = loop {
            let parent = current.parent().unwrap_or_else(|| Path::new(""));
            if let Some(&existing) = self.dirs.get(parent) {
                break existing;
            }
            missing.push(parent);
            current = parent;
        };
        // ...then create the missing directories top-down.
        for path in missing.into_iter().rev() {
            id = self.push_node(id, path, EntryKind::Dir, None);
            self.dirs.insert(path.to_path_buf(), id);
        }
        id
    }

    fn push_node(
        &mut self,
        parent: NodeId,
        rel_path: &Path,
        kind: EntryKind,
        modified: Option<SystemTime>,
    ) -> NodeId {
        let id = NodeId(self.nodes.len());
        let name = rel_path
            .file_name()
            .map_or_else(|| rel_path.as_os_str().to_os_string(), OsString::from);
        self.nodes.push(Node {
            name,
            kind,
            size: 0,
            files: 0,
            modified,
            parent: Some(parent),
            children: Vec::new(),
        });
        self.nodes[parent.0].children.push(id);
        id
    }

    /// How many bytes `item` adds. A file with several hard links is counted
    /// once, credited to whichever of its paths sorts first. Choosing by path
    /// (rather than "whichever the walk saw first") makes the result the same
    /// on every run, although the parallel walk visits entries in a random order.
    fn credited_size(&mut self, item: &ScanItem, node: NodeId) -> u64 {
        let Some(file_id) = item.file_id else {
            return item.size;
        };
        let hard_linked = item.kind != EntryKind::Dir && item.links > 1;
        if !hard_linked && !self.dedup_everything {
            return item.size;
        }
        let candidate = LinkOwner {
            rel_path: item.rel_path.clone(),
            node,
            size: item.size,
        };
        match self.links.entry(file_id) {
            MapEntry::Vacant(slot) => {
                slot.insert(candidate);
                item.size
            }
            MapEntry::Occupied(mut slot) if candidate.rel_path < slot.get().rel_path => {
                // The new path sorts first: move the size over to it.
                let previous = slot.insert(candidate);
                let old_node = &mut self.nodes[previous.node.0];
                old_node.size = old_node.size.saturating_sub(previous.size);
                item.size
            }
            MapEntry::Occupied(_) => 0,
        }
    }

    /// Finishes the tree: adds every subtree's size into its parent.
    pub fn finish(mut self, summary: WalkSummary) -> DiskTree {
        // A child is always created after its parent, so it has a larger id.
        // Visiting ids from last to first therefore handles every child
        // before its parent: one pass, no recursion.
        for index in (1..self.nodes.len()).rev() {
            let Node {
                size,
                files,
                parent,
                ..
            } = self.nodes[index];
            if let Some(parent) = parent {
                let parent = &mut self.nodes[parent.0];
                parent.size = parent.size.saturating_add(size);
                parent.files += files;
            }
        }
        let root_path = PathBuf::from(&self.nodes[0].name);
        DiskTree {
            nodes: self.nodes,
            root_path,
            entries: self.entries,
            summary,
        }
    }
}

/// Walks `root` and builds its disk usage tree, storing nodes down to `max_depth`.
///
/// # Errors
///
/// Returns an error if `root` cannot be accessed.
pub fn build_tree(
    ctx: &Context<'_>,
    root: &Path,
    apparent: bool,
    max_depth: usize,
) -> Result<DiskTree> {
    let walker = Walker::new(root, &ctx.walk, ctx.cancel, ctx.progress)?;
    let mut builder = TreeBuilder::new(root, max_depth, ctx.walk.follow_links);
    let summary = walker.run(
        |entry| ScanItem::from_entry(entry, root, apparent),
        |item| {
            let credited = builder.add(&item);
            ctx.progress.add_bytes(credited);
        },
    );
    Ok(builder.finish(summary))
}

/// Which size a report counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SizeMode {
    /// Disk space actually allocated (default).
    Allocated,
    /// File lengths (`--apparent-size`).
    Apparent,
}

/// The result of `fsight scan`.
#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    /// The scanned path, as given.
    pub root: String,
    pub size_mode: SizeMode,
    pub tree: TreeNode,
    /// Number of entries visited.
    pub entries: u64,
    #[serde(flatten)]
    pub summary: WalkSummary,
}

/// One displayed row of the tree, with its displayed children.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreeNode {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub children: Vec<TreeNode>,
    /// Children that were not shown (beyond `--top` or below `--min-size`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other: Option<Collapsed>,
}

/// The `(N other items)` row: entries collapsed into one line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Collapsed {
    pub count: u64,
    pub size: u64,
}

/// Runs `fsight scan`.
///
/// # Errors
///
/// Returns an error if `root` cannot be accessed.
pub fn run(ctx: &Context<'_>, root: &Path, options: &ScanOptions) -> Result<ScanReport> {
    let tree = build_tree(ctx, root, options.apparent, options.depth)?;
    Ok(report(&tree, options))
}

/// Turns a [`DiskTree`] into what `scan` displays: sorted, cut to `depth`,
/// `top` and `min_size`.
pub fn report(tree: &DiskTree, options: &ScanOptions) -> ScanReport {
    ScanReport {
        root: tree.root_path.display().to_string(),
        size_mode: if options.apparent {
            SizeMode::Apparent
        } else {
            SizeMode::Allocated
        },
        tree: report_node(tree, tree.root(), options.depth, options),
        entries: tree.entries,
        summary: tree.summary,
    }
}

fn report_node(tree: &DiskTree, id: NodeId, levels_left: usize, options: &ScanOptions) -> TreeNode {
    let node = tree.node(id);
    let mut children = Vec::new();
    let mut other = Collapsed::default();

    if levels_left > 0 {
        for child_id in sorted_by_size(tree, &node.children) {
            let child = tree.node(child_id);
            let fits = options.top == 0 || children.len() < options.top;
            if fits && child.size >= options.min_size {
                children.push(report_node(tree, child_id, levels_left - 1, options));
            } else {
                other.count += 1;
                other.size = other.size.saturating_add(child.size);
            }
        }
    }

    TreeNode {
        name: node.name.to_string_lossy().into_owned(),
        kind: node.kind,
        size: node.size,
        children,
        other: (other.count > 0 && other.size >= options.min_size).then_some(other),
    }
}

/// Children sorted largest first; ties are broken by name so output is stable.
pub fn sorted_by_size(tree: &DiskTree, ids: &[NodeId]) -> Vec<NodeId> {
    let mut sorted = ids.to_vec();
    sorted.sort_by(|&a, &b| {
        let (a, b) = (tree.node(a), tree.node(b));
        b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name))
    });
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic walk entry: no file system involved.
    fn item(path: &str, kind: EntryKind, size: u64) -> ScanItem {
        ScanItem {
            rel_path: PathBuf::from(path),
            depth: Path::new(path).components().count(),
            kind,
            size,
            modified: None,
            file_id: None,
            links: 1,
        }
    }

    fn file(path: &str, size: u64) -> ScanItem {
        item(path, EntryKind::File, size)
    }

    fn dir(path: &str) -> ScanItem {
        item(path, EntryKind::Dir, 0)
    }

    fn linked(path: &str, size: u64, inode: u64) -> ScanItem {
        ScanItem {
            file_id: Some(FileId { device: 1, inode }),
            links: 2,
            ..file(path, size)
        }
    }

    fn build(items: &[ScanItem], max_depth: usize) -> DiskTree {
        let mut builder = TreeBuilder::new(Path::new("."), max_depth, false);
        for item in items {
            builder.add(item);
        }
        builder.finish(WalkSummary::default())
    }

    /// Finds a node by its path relative to the root.
    fn find<'t>(tree: &'t DiskTree, path: &str) -> &'t Node {
        let mut id = tree.root();
        for name in Path::new(path).components() {
            id = *tree
                .node(id)
                .children
                .iter()
                .find(|&&child| tree.node(child).name == name.as_os_str())
                .unwrap_or_else(|| panic!("{path} not found"));
        }
        tree.node(id)
    }

    fn options(depth: usize, top: usize, min_size: u64) -> ScanOptions {
        ScanOptions {
            depth,
            top,
            apparent: true,
            min_size,
        }
    }

    #[test]
    fn sizes_and_file_counts_roll_up_to_every_ancestor() {
        let tree = build(
            &[
                dir(""),
                dir("a"),
                file("a/x", 10),
                dir("a/b"),
                file("a/b/y", 20),
                file("z", 5),
            ],
            usize::MAX,
        );
        assert_eq!(find(&tree, "a/b").size, 20);
        assert_eq!(find(&tree, "a").size, 30);
        assert_eq!(tree.node(tree.root()).size, 35);
        assert_eq!(tree.node(tree.root()).files, 3);
        assert_eq!(find(&tree, "a").files, 2);
        assert_eq!(tree.path(find_id(&tree, "a/b/y")), Path::new("./a/b/y"));
    }

    fn find_id(tree: &DiskTree, path: &str) -> NodeId {
        (0..tree.len())
            .map(NodeId)
            .find(|&id| tree.path(id) == Path::new(".").join(path))
            .unwrap()
    }

    #[test]
    fn children_may_arrive_before_their_parents() {
        // The parallel walk does not guarantee any order.
        let tree = build(
            &[file("a/b/c", 7), dir("a/b"), dir(""), dir("a")],
            usize::MAX,
        );
        assert_eq!(tree.len(), 4, "no duplicate directory nodes");
        assert_eq!(find(&tree, "a").size, 7);
        assert_eq!(find(&tree, "a/b").kind, EntryKind::Dir);
    }

    #[test]
    fn entries_below_the_display_depth_are_folded_into_their_ancestor() {
        let tree = build(
            &[
                dir(""),
                dir("a"),
                dir("a/b"),
                file("a/b/deep", 100),
                file("a/top", 1),
            ],
            1,
        );
        let a = find(&tree, "a");
        assert_eq!(a.size, 101);
        assert!(a.children.is_empty(), "nothing below depth 1 is stored");
        assert_eq!(tree.len(), 2);
    }

    #[test]
    fn hard_links_count_once_for_the_path_that_sorts_first() {
        // Same inode under two names, in both arrival orders.
        for items in [
            vec![
                dir("a"),
                dir("b"),
                linked("a/x", 50, 9),
                linked("b/x", 50, 9),
            ],
            vec![
                dir("a"),
                dir("b"),
                linked("b/x", 50, 9),
                linked("a/x", 50, 9),
            ],
        ] {
            let tree = build(&items, usize::MAX);
            assert_eq!(tree.node(tree.root()).size, 50);
            assert_eq!(find(&tree, "a").size, 50);
            assert_eq!(find(&tree, "b").size, 0);
        }
    }

    #[test]
    fn report_sorts_by_size_and_collapses_beyond_top() {
        let tree = build(
            &[
                file("small", 1),
                file("big", 100),
                file("mid", 10),
                file("mid2", 10),
            ],
            usize::MAX,
        );
        let report = report(&tree, &options(1, 2, 0));
        let names: Vec<&str> = report
            .tree
            .children
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["big", "mid"], "ties broken by name");
        assert_eq!(report.tree.other, Some(Collapsed { count: 2, size: 11 }));
    }

    #[test]
    fn min_size_hides_small_entries_and_small_collapsed_rows() {
        let tree = build(&[file("big", 100), file("tiny", 1)], usize::MAX);
        let report = report(&tree, &options(1, 0, 50));
        assert_eq!(report.tree.children.len(), 1);
        assert_eq!(
            report.tree.other, None,
            "the collapsed row is below --min-size too"
        );

        let unlimited = report_node(&tree, tree.root(), 1, &options(1, 0, 0));
        assert_eq!(unlimited.children.len(), 2, "top = 0 shows everything");
    }

    #[test]
    fn depth_limits_the_displayed_levels() {
        let tree = build(&[dir("a"), file("a/x", 3)], usize::MAX);
        assert_eq!(report(&tree, &options(0, 10, 0)).tree.children, []);
        let one = report(&tree, &options(1, 10, 0));
        assert_eq!(one.tree.children.len(), 1);
        assert_eq!(one.tree.children[0].children, []);
    }
}
