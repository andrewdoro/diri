// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//
//! The rows a diff view draws, as indices into a [`DiffSnapshot`].
//!
//! A snapshot is a flat list of file, hunk, and line rows. The view lays it out
//! inline (one line per row, old and new numbers side by side) or split (each
//! removal beside the addition that replaced it), skips the bodies of
//! collapsed files, and opens the omitted-untracked notice into file names.
//! The list is rebuilt only when one of those inputs changes, never per frame,
//! so a 100k-line diff costs one linear pass per toggle.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::diff::{DiffRowKind, DiffSnapshot, is_file_header_meta};

/// How the review lays a diff out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum DiffLayout {
    /// One column: removed then added lines, both line numbers in the gutter.
    #[default]
    Inline,
    /// Two columns: the old file on the left, the new file on the right.
    Split,
}

/// One row of the virtualized list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewRow {
    /// Breathing room above a file header (never the first row).
    Gap,
    /// A snapshot row drawn across the whole width: a file or hunk header, a
    /// notice, or (inline) a line.
    Row(usize),
    /// A split line: the old side and the new side, either of which may be
    /// missing for a pure removal or addition.
    Pair {
        left: Option<usize>,
        right: Option<usize>,
    },
    /// The nth name under the expanded omitted-untracked notice.
    OmittedPath(usize),
}

impl ViewRow {
    /// Whether this row shows snapshot row `row`.
    #[must_use]
    pub(crate) fn shows(self, row: usize) -> bool {
        match self {
            Self::Row(index) => index == row,
            Self::Pair { left, right } => left == Some(row) || right == Some(row),
            Self::Gap | Self::OmittedPath(_) => false,
        }
    }
}

/// Builds the rows for `snapshot`. `collapsed` names files whose bodies are
/// hidden; `omitted_open` appends the omitted-untracked names after the
/// notice row.
#[must_use]
pub(crate) fn build_rows(
    snapshot: &DiffSnapshot,
    layout: DiffLayout,
    collapsed: &HashSet<PathBuf>,
    omitted_open: bool,
) -> Vec<ViewRow> {
    let rows = &snapshot.rows;
    let mut out = Vec::with_capacity(rows.len() + 2 * snapshot.file_diffs.len());
    let mut files = snapshot.file_diffs.iter().peekable();
    let mut index = 0;
    while index < rows.len() {
        if let Some(file) = files.peek()
            && file.row_range.start == index
        {
            let file = files.next().expect("peeked file");
            if !out.is_empty() {
                out.push(ViewRow::Gap);
            }
            out.push(ViewRow::Row(index));
            let body = index + 1..file.row_range.end.max(index + 1);
            if !collapsed.contains(&file.path) {
                push_body(&mut out, snapshot, body.clone(), layout);
            }
            index = body.end;
            continue;
        }
        out.push(ViewRow::Row(index));
        index += 1;
    }
    if omitted_open && snapshot.omitted_untracked_notice_row().is_some() {
        out.extend((0..snapshot.omitted_untracked_paths.len()).map(ViewRow::OmittedPath));
    }
    out
}

fn push_body(
    out: &mut Vec<ViewRow>,
    snapshot: &DiffSnapshot,
    body: std::ops::Range<usize>,
    layout: DiffLayout,
) {
    let rows = &snapshot.rows;
    // Git's `index …`/mode/similarity lines are already said by the file
    // header's status badge.
    let header_meta = |index: usize| {
        rows[index].kind == DiffRowKind::Meta && is_file_header_meta(&rows[index].text)
    };
    if layout == DiffLayout::Inline {
        out.extend(body.filter(|index| !header_meta(*index)).map(ViewRow::Row));
        return;
    }
    let mut index = body.start;
    while index < body.end {
        match rows[index].kind {
            DiffRowKind::Context => {
                out.push(ViewRow::Pair {
                    left: Some(index),
                    right: Some(index),
                });
                index += 1;
            }
            DiffRowKind::Deletion | DiffRowKind::Addition => {
                let removed = index;
                while index < body.end && rows[index].kind == DiffRowKind::Deletion {
                    index += 1;
                }
                let added = index;
                while index < body.end && rows[index].kind == DiffRowKind::Addition {
                    index += 1;
                }
                let (removed_count, added_count) = (added - removed, index - added);
                for offset in 0..removed_count.max(added_count) {
                    out.push(ViewRow::Pair {
                        left: (offset < removed_count).then_some(removed + offset),
                        right: (offset < added_count).then_some(added + offset),
                    });
                }
            }
            DiffRowKind::Meta if header_meta(index) => index += 1,
            DiffRowKind::File | DiffRowKind::Hunk | DiffRowKind::Meta => {
                out.push(ViewRow::Row(index));
                index += 1;
            }
        }
    }
}

/// The list position that shows snapshot row `row`, if it is visible.
#[must_use]
pub(crate) fn position_of(rows: &[ViewRow], row: usize) -> Option<usize> {
    rows.iter().position(|view| view.shows(row))
}

/// The widest line number in a snapshot, for sizing the gutter.
#[must_use]
pub(crate) fn line_number_digits(snapshot: &DiffSnapshot) -> usize {
    let widest = snapshot
        .rows
        .iter()
        .flat_map(|row| [row.old_line, row.new_line])
        .flatten()
        .max()
        .unwrap_or(1);
    widest.to_string().len().max(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_unified_diff;

    const PATCH: &str = "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1,4 +1,4 @@\n keep\n-a\n-b\n+A\n tail\ndiff --git a/two.rs b/two.rs\n--- a/two.rs\n+++ b/two.rs\n@@ -1 +1,2 @@\n x\n+y\n";

    #[test]
    fn inline_rows_follow_the_snapshot_with_a_gap_between_files() {
        let snapshot = parse_unified_diff(PATCH);
        let rows = build_rows(&snapshot, DiffLayout::Inline, &HashSet::new(), false);
        let expected: Vec<ViewRow> = (0..7)
            .map(ViewRow::Row)
            .chain([ViewRow::Gap])
            .chain((7..11).map(ViewRow::Row))
            .collect();
        assert_eq!(rows, expected);
    }

    #[test]
    fn split_rows_pair_removals_with_additions() {
        let snapshot = parse_unified_diff(PATCH);
        let rows = build_rows(&snapshot, DiffLayout::Split, &HashSet::new(), false);
        assert_eq!(
            &rows[..6],
            [
                ViewRow::Row(0),
                ViewRow::Row(1),
                ViewRow::Pair {
                    left: Some(2),
                    right: Some(2)
                },
                ViewRow::Pair {
                    left: Some(3),
                    right: Some(5)
                },
                ViewRow::Pair {
                    left: Some(4),
                    right: None
                },
                ViewRow::Pair {
                    left: Some(6),
                    right: Some(6)
                },
            ]
        );
        assert_eq!(
            rows.last(),
            Some(&ViewRow::Pair {
                left: None,
                right: Some(10)
            })
        );
        assert_eq!(position_of(&rows, 5), Some(3));
        assert_eq!(position_of(&rows, 4), Some(4));
    }

    #[test]
    fn collapsed_files_keep_only_their_header() {
        let snapshot = parse_unified_diff(PATCH);
        let collapsed = HashSet::from([PathBuf::from("one.rs")]);
        let rows = build_rows(&snapshot, DiffLayout::Inline, &collapsed, false);
        assert_eq!(
            rows,
            [
                ViewRow::Row(0),
                ViewRow::Gap,
                ViewRow::Row(7),
                ViewRow::Row(8),
                ViewRow::Row(9),
                ViewRow::Row(10)
            ]
        );
        assert_eq!(position_of(&rows, 3), None);
    }

    #[test]
    fn git_file_header_lines_are_left_to_the_status_badge() {
        let snapshot = parse_unified_diff(
            "diff --git a/n b/n\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/n\n@@ -0,0 +1 @@\n+x\n\\ No newline at end of file\n",
        );
        for layout in [DiffLayout::Inline, DiffLayout::Split] {
            let rows = build_rows(&snapshot, layout, &HashSet::new(), false);
            let shown: Vec<&str> = rows
                .iter()
                .filter_map(|row| match row {
                    ViewRow::Row(index) if snapshot.rows[*index].kind != DiffRowKind::Addition => {
                        Some(snapshot.rows[*index].text.as_str())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                shown,
                ["n", "@@ -0,0 +1 @@", "\\ No newline at end of file"]
            );
        }
    }

    #[test]
    fn line_numbers_size_the_gutter() {
        let snapshot = parse_unified_diff(PATCH);
        assert_eq!(line_number_digits(&snapshot), 2);
        let wide = parse_unified_diff(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -12345 +12345 @@\n-x\n+y\n",
        );
        assert_eq!(line_number_digits(&wide), 5);
    }
}
