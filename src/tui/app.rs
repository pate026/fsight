//! The explorer's state and its transitions.
//!
//! Nothing in this file touches the terminal: key presses become method
//! calls (see `events.rs`) and drawing reads the fields (see `ui.rs`). That
//! separation is what makes the explorer testable — a test builds an [`App`],
//! calls methods, and checks the fields.

use std::path::{Path, PathBuf};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::commands::scan::{DiskTree, Node, NodeId};
use crate::progress::ProgressSnapshot;
use crate::walk::EntryKind;

/// How the entries of a directory are ordered in the explorer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum SortMode {
    /// Largest first.
    #[default]
    Size,
    /// Alphabetical, ignoring case.
    Name,
    /// Most recently modified first.
    Modified,
}

impl SortMode {
    /// The next mode in the `s` key's cycle.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            SortMode::Size => SortMode::Name,
            SortMode::Name => SortMode::Modified,
            SortMode::Modified => SortMode::Size,
        }
    }

    /// Short name shown in the header.
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Size => "size",
            SortMode::Name => "name",
            SortMode::Modified => "modified",
        }
    }
}

/// Where the background scan is.
#[derive(Debug)]
pub enum ScanState {
    /// Still walking; the snapshot is the latest progress report.
    Scanning(ProgressSnapshot),
    /// Done: the tree can be explored.
    Ready(DiskTree),
    /// The scan could not run.
    Failed(String),
}

/// What keys currently do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Navigation keys.
    Normal,
    /// Typing a filter after `/`.
    Filter,
    /// The help popup is open.
    Help,
}

/// The whole state of the explorer.
#[derive(Debug)]
pub struct App {
    /// The directory being explored, as given on the command line.
    pub root: PathBuf,
    pub state: ScanState,
    pub mode: Mode,
    pub sort: SortMode,
    /// Case-insensitive substring filter on names (empty = no filter).
    pub filter: String,
    pub show_info: bool,
    /// The directory whose children are listed.
    pub cwd: NodeId,
    /// Children of `cwd` after filtering and sorting, recomputed by `refresh`.
    pub visible: Vec<NodeId>,
    /// Index into `visible` of the highlighted row.
    pub selected: usize,
    /// Index of the first row shown (scroll position), kept by the UI.
    pub offset: usize,
    /// How many rows fit on screen (set by the UI, used for Page Up/Down).
    pub page: usize,
    /// A one-off message for the footer, cleared by the next key press.
    pub status: Option<String>,
    /// Animation counter for the spinner.
    pub ticks: u64,
    pub should_quit: bool,
}

impl App {
    /// A new explorer for `root`, waiting for its scan.
    pub fn new(root: &Path, sort: SortMode) -> Self {
        Self {
            root: root.to_path_buf(),
            state: ScanState::Scanning(ProgressSnapshot {
                items: 0,
                bytes: 0,
                stage: "Scanning",
            }),
            mode: Mode::Normal,
            sort,
            filter: String::new(),
            show_info: false,
            cwd: NodeId::default(),
            visible: Vec::new(),
            selected: 0,
            offset: 0,
            page: 10,
            status: None,
            ticks: 0,
            should_quit: false,
        }
    }

    /// The tree, once the scan is done.
    pub fn tree(&self) -> Option<&DiskTree> {
        match &self.state {
            ScanState::Ready(tree) => Some(tree),
            ScanState::Scanning(_) | ScanState::Failed(_) => None,
        }
    }

    /// The node under the cursor, if any.
    pub fn selected_node(&self) -> Option<NodeId> {
        self.visible.get(self.selected).copied()
    }

    /// The scanner reported progress.
    pub fn on_progress(&mut self, snapshot: ProgressSnapshot) {
        if matches!(self.state, ScanState::Scanning(_)) {
            self.state = ScanState::Scanning(snapshot);
        }
    }

    /// The scan finished (or failed).
    pub fn on_scan_finished(&mut self, result: Result<DiskTree, String>) {
        match result {
            Ok(tree) => {
                self.cwd = tree.root();
                self.state = ScanState::Ready(tree);
                self.selected = 0;
                self.refresh();
            }
            Err(message) => self.state = ScanState::Failed(message),
        }
    }

    pub fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Moves the cursor by `delta` rows, stopping at the first and last row.
    pub fn move_by(&mut self, delta: isize) {
        let last = self.visible.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    pub fn move_to_top(&mut self) {
        self.selected = 0;
    }

    pub fn move_to_bottom(&mut self) {
        self.selected = self.visible.len().saturating_sub(1);
    }

    /// Moves one screen up or down.
    pub fn page_by(&mut self, pages: isize) {
        let rows = isize::try_from(self.page.max(1)).unwrap_or(isize::MAX);
        self.move_by(pages.saturating_mul(rows));
    }

    /// Enters the selected directory. Selecting a file does nothing (but says so).
    pub fn open_selected(&mut self) {
        let Some(id) = self.selected_node() else {
            return;
        };
        let Some(tree) = self.tree() else {
            return;
        };
        if tree.node(id).kind != EntryKind::Dir {
            self.status = Some("Not a directory — press i for details".to_string());
            return;
        }
        self.cwd = id;
        self.filter.clear();
        self.selected = 0;
        self.offset = 0;
        self.refresh();
    }

    /// Goes to the parent directory and puts the cursor on the directory we
    /// came from. At the root this does nothing (but says so).
    pub fn go_up(&mut self) {
        let Some(parent) = self.tree().and_then(|tree| tree.node(self.cwd).parent) else {
            if self.tree().is_some() {
                self.status = Some("Already at the top of the scan".to_string());
            }
            return;
        };
        let came_from = self.cwd;
        self.cwd = parent;
        self.filter.clear();
        self.refresh();
        self.selected = self
            .visible
            .iter()
            .position(|&id| id == came_from)
            .unwrap_or(0);
    }

    /// Switches to the next sort order, keeping the cursor on the same entry.
    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.refresh_keeping_selection();
    }

    pub fn start_filter(&mut self) {
        self.mode = Mode::Filter;
    }

    /// Adds a character to the filter; the list updates immediately.
    pub fn push_filter(&mut self, c: char) {
        self.filter.push(c);
        self.selected = 0;
        self.refresh();
    }

    pub fn pop_filter(&mut self) {
        self.filter.pop();
        self.refresh_keeping_selection();
    }

    /// Leaves filter mode but keeps the filter.
    pub fn finish_filter(&mut self) {
        self.mode = Mode::Normal;
    }

    /// Removes the filter (and leaves filter mode).
    pub fn clear_filter(&mut self) {
        self.mode = Mode::Normal;
        if !self.filter.is_empty() {
            self.filter.clear();
            self.refresh_keeping_selection();
        }
    }

    pub fn toggle_info(&mut self) {
        self.show_info = !self.show_info;
    }

    pub fn toggle_help(&mut self) {
        self.mode = if self.mode == Mode::Help {
            Mode::Normal
        } else {
            Mode::Help
        };
    }

    fn refresh_keeping_selection(&mut self) {
        let current = self.selected_node();
        self.refresh();
        if let Some(position) = current.and_then(|id| self.visible.iter().position(|&v| v == id)) {
            self.selected = position;
        }
    }

    /// Recomputes `visible` from the tree, the filter and the sort order,
    /// and keeps `selected` inside the list.
    fn refresh(&mut self) {
        self.visible = match self.tree() {
            Some(tree) => visible_children(tree, self.cwd, &self.filter, self.sort),
            None => Vec::new(),
        };
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
    }
}

/// The children of `dir` whose names contain `filter` (ignoring case), in `sort` order.
pub fn visible_children(tree: &DiskTree, dir: NodeId, filter: &str, sort: SortMode) -> Vec<NodeId> {
    let needle = filter.to_lowercase();
    let mut children: Vec<NodeId> = tree
        .node(dir)
        .children
        .iter()
        .copied()
        .filter(|&id| needle.is_empty() || lower_name(tree.node(id)).contains(&needle))
        .collect();
    match sort {
        SortMode::Size => {
            children.sort_by(|&a, &b| {
                let (a, b) = (tree.node(a), tree.node(b));
                b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name))
            });
        }
        SortMode::Name => {
            children
                .sort_by_cached_key(|&id| (lower_name(tree.node(id)), tree.node(id).name.clone()));
        }
        SortMode::Modified => {
            children.sort_by(|&a, &b| {
                let (a, b) = (tree.node(a), tree.node(b));
                // Newest first; entries without a time go last.
                b.modified
                    .cmp(&a.modified)
                    .then_with(|| a.name.cmp(&b.name))
            });
        }
    }
    children
}

fn lower_name(node: &Node) -> String {
    node.name.to_string_lossy().to_lowercase()
}

/// Scrolling: the first row to show so that `selected` is visible in a
/// window of `height` rows, moving the window as little as possible.
pub fn scroll_offset(offset: usize, selected: usize, height: usize) -> usize {
    if height == 0 {
        return selected;
    }
    if selected < offset {
        selected
    } else if selected >= offset + height {
        selected + 1 - height
    } else {
        offset
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::commands::scan::{ScanItem, TreeBuilder};
    use crate::walk::WalkSummary;
    use std::time::{Duration, UNIX_EPOCH};

    /// A small tree: `./big/{a,b}`, `./Small.txt`, `./mid/`, built without
    /// touching the file system.
    pub(crate) fn sample_tree() -> DiskTree {
        let item = |path: &str, kind, size, age: u64| ScanItem {
            rel_path: PathBuf::from(path),
            depth: Path::new(path).components().count(),
            kind,
            size,
            modified: Some(UNIX_EPOCH + Duration::from_secs(1_000_000 - age)),
            file_id: None,
            links: 1,
        };
        let mut builder = TreeBuilder::new(Path::new("."), usize::MAX, false);
        for item in [
            item("", EntryKind::Dir, 0, 0),
            item("big", EntryKind::Dir, 0, 50),
            item("big/a", EntryKind::File, 700, 10),
            item("big/b", EntryKind::File, 300, 20),
            item("Small.txt", EntryKind::File, 10, 5),
            item("mid", EntryKind::Dir, 0, 100),
            item("mid/c", EntryKind::File, 100, 30),
        ] {
            builder.add(&item);
        }
        builder.finish(WalkSummary::default())
    }

    pub(crate) fn ready_app() -> App {
        let mut app = App::new(Path::new("."), SortMode::Size);
        app.on_scan_finished(Ok(sample_tree()));
        app
    }

    pub(crate) fn names(app: &App) -> Vec<String> {
        let tree = app.tree().unwrap();
        app.visible
            .iter()
            .map(|&id| tree.node(id).name.to_string_lossy().into_owned())
            .collect()
    }

    fn selected_name(app: &App) -> String {
        let tree = app.tree().unwrap();
        tree.node(app.selected_node().unwrap())
            .name
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn starts_scanning_then_shows_the_root_sorted_by_size() {
        let mut app = App::new(Path::new("."), SortMode::Size);
        assert!(app.tree().is_none());
        app.on_progress(ProgressSnapshot {
            items: 5,
            bytes: 10,
            stage: "Scanning",
        });
        assert!(matches!(app.state, ScanState::Scanning(p) if p.items == 5));
        app.on_scan_finished(Ok(sample_tree()));
        assert_eq!(names(&app), ["big", "mid", "Small.txt"]);
    }

    #[test]
    fn cursor_moves_and_stops_at_both_ends() {
        let mut app = ready_app();
        app.move_by(1);
        assert_eq!(selected_name(&app), "mid");
        app.move_by(10);
        assert_eq!(selected_name(&app), "Small.txt");
        app.move_by(-10);
        assert_eq!(selected_name(&app), "big");
        app.move_to_bottom();
        assert_eq!(app.selected, 2);
        app.move_to_top();
        assert_eq!(app.selected, 0);
        app.page = 2;
        app.page_by(1);
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn opening_a_directory_and_going_back_up_restores_the_cursor() {
        let mut app = ready_app();
        app.move_by(1); // "mid"
        app.open_selected();
        assert_eq!(names(&app), ["c"]);
        app.go_up();
        assert_eq!(
            selected_name(&app),
            "mid",
            "cursor returns to where we came from"
        );
    }

    #[test]
    fn going_up_from_the_root_does_nothing_but_says_so() {
        let mut app = ready_app();
        let before = app.cwd;
        app.go_up();
        assert_eq!(app.cwd, before);
        assert_eq!(
            app.status.as_deref(),
            Some("Already at the top of the scan")
        );
    }

    #[test]
    fn opening_a_file_does_nothing_but_says_so() {
        let mut app = ready_app();
        app.move_to_bottom(); // Small.txt
        let before = app.cwd;
        app.open_selected();
        assert_eq!(app.cwd, before);
        assert!(app.status.unwrap().starts_with("Not a directory"));
    }

    #[test]
    fn sort_cycles_through_size_name_and_modified_keeping_the_cursor() {
        let mut app = ready_app();
        app.move_by(1); // "mid"
        app.cycle_sort();
        assert_eq!(app.sort, SortMode::Name);
        assert_eq!(names(&app), ["big", "mid", "Small.txt"], "case-insensitive");
        assert_eq!(selected_name(&app), "mid");
        app.cycle_sort();
        assert_eq!(app.sort, SortMode::Modified);
        assert_eq!(names(&app), ["Small.txt", "big", "mid"], "newest first");
        assert_eq!(selected_name(&app), "mid");
        app.cycle_sort();
        assert_eq!(app.sort, SortMode::Size);
    }

    #[test]
    fn filter_is_live_case_insensitive_and_can_be_cleared() {
        let mut app = ready_app();
        app.start_filter();
        assert_eq!(app.mode, Mode::Filter);
        app.push_filter('S');
        assert_eq!(names(&app), ["Small.txt"]);
        app.pop_filter();
        assert_eq!(names(&app).len(), 3);
        app.push_filter('i');
        assert_eq!(names(&app), ["big", "mid"]);
        app.finish_filter();
        assert_eq!((app.mode, app.filter.as_str()), (Mode::Normal, "i"));
        app.clear_filter();
        assert_eq!(names(&app).len(), 3);
        app.push_filter('z');
        assert_eq!(names(&app), Vec::<String>::new());
        assert_eq!(app.selected_node(), None);
        app.open_selected(); // nothing selected: no panic, no change
    }

    #[test]
    fn info_and_help_toggle() {
        let mut app = ready_app();
        app.toggle_info();
        assert!(app.show_info);
        app.toggle_help();
        assert_eq!(app.mode, Mode::Help);
        app.toggle_help();
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn navigation_before_the_scan_finishes_is_harmless() {
        let mut app = App::new(Path::new("."), SortMode::Size);
        app.move_by(1);
        app.open_selected();
        app.go_up();
        app.cycle_sort();
        assert_eq!(app.status, None);
        app.on_scan_finished(Err("boom".into()));
        assert!(matches!(app.state, ScanState::Failed(ref m) if m == "boom"));
    }

    #[test]
    fn scrolling_keeps_the_cursor_visible_with_minimal_movement() {
        assert_eq!(scroll_offset(0, 3, 5), 0, "already visible");
        assert_eq!(scroll_offset(0, 7, 5), 3, "scroll down just enough");
        assert_eq!(scroll_offset(3, 1, 5), 1, "scroll up just enough");
        assert_eq!(
            scroll_offset(4, 9, 0),
            9,
            "zero height does not divide by zero"
        );
    }
}
