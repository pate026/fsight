//! Text rendering of the `fsight scan` tree:
//!
//! ```text
//!  64.6 KiB 100%  ██████████  .
//!  42.4 KiB  65%  ██████░░░░  ├── assets
//!  40.0 KiB  94%  █████████░  │   ├── logo.png
//! ```
//!
//! Columns: size, percentage of the parent, a bar showing that percentage,
//! then the name with tree-drawing prefixes.

use std::io::{self, Write};

use super::bars::{meter, percent};
use super::color::{Palette, Role};
use super::{RenderText, format_count, plural};
use crate::commands::scan::{Collapsed, ScanReport, TreeNode};
use crate::size::format_size;
use crate::walk::EntryKind;

/// Width of the `██████░░░░` bar.
const BAR_WIDTH: usize = 10;

impl RenderText for ScanReport {
    fn render_text(&self, out: &mut dyn Write, palette: Palette) -> io::Result<()> {
        let size_width = widest_size(&self.tree);
        let renderer = TreeRenderer {
            out,
            palette,
            size_width,
        };
        renderer.render(&self.tree)
    }
}

/// The widest formatted size in the displayed tree, so the columns line up.
fn widest_size(node: &TreeNode) -> usize {
    let own = format_size(node.size).len();
    let other = node.other.map_or(0, |other| format_size(other.size).len());
    node.children
        .iter()
        .map(widest_size)
        .fold(own.max(other), usize::max)
}

/// Holds what every row needs, so the recursive functions take fewer arguments.
struct TreeRenderer<'a> {
    out: &'a mut dyn Write,
    palette: Palette,
    size_width: usize,
}

impl TreeRenderer<'_> {
    fn render(mut self, root: &TreeNode) -> io::Result<()> {
        self.row(root.size, root.size, "", &root.name, name_role(root.kind))?;
        self.children(root, "")
    }

    /// Writes the children of `parent`; `indent` is the prefix inherited from
    /// its ancestors (a mix of `│   ` and `    `).
    fn children(&mut self, parent: &TreeNode, indent: &str) -> io::Result<()> {
        let rows = parent.children.len() + usize::from(parent.other.is_some());
        for (index, child) in parent.children.iter().enumerate() {
            let last = index + 1 == rows;
            let branch = if last { "└── " } else { "├── " };
            let prefix = format!("{indent}{branch}");
            self.row(
                child.size,
                parent.size,
                &prefix,
                &child.name,
                name_role(child.kind),
            )?;
            let child_indent = format!("{indent}{}", if last { "    " } else { "│   " });
            self.children(child, &child_indent)?;
        }
        if let Some(other) = parent.other {
            self.collapsed_row(other, parent.size, indent)?;
        }
        Ok(())
    }

    fn collapsed_row(
        &mut self,
        other: Collapsed,
        parent_size: u64,
        indent: &str,
    ) -> io::Result<()> {
        let label = format!(
            "({} other {})",
            format_count(other.count),
            plural(other.count, "item", "items")
        );
        self.row(
            other.size,
            parent_size,
            &format!("{indent}└── "),
            &label,
            Some(Role::Dim),
        )
    }

    /// One line: size, percentage, bar, prefix and name.
    fn row(
        &mut self,
        size: u64,
        parent_size: u64,
        prefix: &str,
        name: &str,
        role: Option<Role>,
    ) -> io::Result<()> {
        // The root is 100% of itself, even when it is empty.
        let (pct, bar) = if prefix.is_empty() {
            (100, meter(1, 1, BAR_WIDTH))
        } else {
            (
                percent(size, parent_size),
                meter(size, parent_size, BAR_WIDTH),
            )
        };
        let size_text = format!("{:>width$}", format_size(size), width = self.size_width);
        let palette = self.palette;
        write!(
            self.out,
            "{} {pct:>3}%  {}  {}",
            palette.paint(Role::Size, &size_text),
            palette.paint(Role::Bar, &bar),
            palette.paint(Role::Dim, prefix),
        )?;
        match role {
            Some(role) => writeln!(self.out, "{}", palette.paint(role, name)),
            None => writeln!(self.out, "{name}"),
        }
    }
}

/// Directories and symlinks are colored; plain files are not.
fn name_role(kind: EntryKind) -> Option<Role> {
    match kind {
        EntryKind::Dir => Some(Role::Dir),
        EntryKind::Symlink => Some(Role::Symlink),
        EntryKind::File | EntryKind::Other => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::scan::SizeMode;
    use crate::walk::WalkSummary;

    fn node(name: &str, kind: EntryKind, size: u64, children: Vec<TreeNode>) -> TreeNode {
        TreeNode {
            name: name.to_string(),
            kind,
            size,
            children,
            other: None,
        }
    }

    #[test]
    fn draws_branches_indentation_and_the_collapsed_row() {
        let mut src = node(
            "src",
            EntryKind::Dir,
            3072,
            vec![node("main.rs", EntryKind::File, 2048, vec![])],
        );
        src.other = Some(Collapsed {
            count: 2,
            size: 1024,
        });
        let root = node(
            ".",
            EntryKind::Dir,
            4096,
            vec![src, node("readme.md", EntryKind::File, 1024, vec![])],
        );
        let report = ScanReport {
            root: ".".into(),
            size_mode: SizeMode::Apparent,
            tree: root,
            entries: 6,
            summary: WalkSummary::default(),
        };

        let mut out = Vec::new();
        report.render_text(&mut out, Palette::plain()).unwrap();
        let expected = "\
4.0 KiB 100%  ██████████  .
3.0 KiB  75%  ███████░░░  ├── src
2.0 KiB  66%  ██████░░░░  │   ├── main.rs
1.0 KiB  33%  ███░░░░░░░  │   └── (2 other items)
1.0 KiB  25%  ██░░░░░░░░  └── readme.md
";
        assert_eq!(String::from_utf8(out).unwrap(), expected);
    }

    #[test]
    fn an_empty_root_is_still_shown_as_a_full_bar() {
        let report = ScanReport {
            root: "empty".into(),
            size_mode: SizeMode::Allocated,
            tree: node("empty", EntryKind::Dir, 0, vec![]),
            entries: 1,
            summary: WalkSummary::default(),
        };
        let mut out = Vec::new();
        report.render_text(&mut out, Palette::plain()).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "0 B 100%  ██████████  empty\n"
        );
    }
}
