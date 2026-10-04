// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//
//! The review surface's git presentation: a diff viewer (inline or split,
//! with word-level emphasis) and a commit history with its graph.
//!
//! The design follows Ely's `git` components (DiffViewer, InlineDiff,
//! CommitGraph, CommitList, CommitItem, DiffStat, GitStatusBadge), ported to
//! diri's GPUI revision, palette, and icons. Loading stays in `crate::diff` and
//! `crate::git_review`; the inspector owns the session and the actions. This
//! module owns how the review looks and the view state behind it.

pub(crate) mod commit_list;
pub(crate) mod diff_view;
pub(crate) mod graph;
pub(crate) mod palette;
pub(crate) mod rows;
pub(crate) mod words;

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use gpui::{Task, UniformListScrollHandle};

pub(crate) use commit_list::LoadedHistory;
pub(crate) use palette::DiffPalette;
pub(crate) use rows::{DiffLayout, ViewRow};

use crate::diff::DiffSnapshot;

/// Panels narrower than this keep the inline layout even when split is
/// chosen: two columns of code would each be too narrow to read.
pub(crate) const SPLIT_MIN_WIDTH: f32 = 620.0;

/// Which half of the review is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ReviewMode {
    /// The working tree, index, or branch diff.
    #[default]
    Changes,
    /// The branch's commits, and the diff of the one picked.
    Commits,
}

#[derive(Clone, Debug, Default)]
pub(crate) enum HistoryLoad {
    #[default]
    Idle,
    Loading,
    Ready(Arc<LoadedHistory>),
    Error(String),
}

#[derive(Clone, Debug)]
pub(crate) enum CommitDiffLoad {
    Loading,
    Ready(Arc<DiffSnapshot>),
    Error(String),
}

/// What the rows were last built from. The snapshot is held weakly so its
/// address cannot be reused by a different snapshot while it is the key.
struct RowsCache {
    snapshot: Weak<DiffSnapshot>,
    layout: DiffLayout,
    collapsed_generation: u64,
    omitted_open: bool,
    rows: Arc<Vec<ViewRow>>,
    digits: usize,
}

/// The rows of one diff as the view draws them, and its gutter's digit count.
#[derive(Clone)]
pub(crate) struct BuiltRows {
    pub rows: Arc<Vec<ViewRow>>,
    pub digits: usize,
}

/// The review's view state: layout choice, collapsed files, the commit list,
/// and the commit diff. Reset when the session (context) changes.
pub(crate) struct ReviewUi {
    pub mode: ReviewMode,
    /// The layout the user chose. Split only applies while the panel is wide
    /// enough (see [`ReviewUi::effective_layout`]).
    pub layout: DiffLayout,
    /// Whether the diff body was last measured at least [`SPLIT_MIN_WIDTH`].
    pub split_fits: Rc<std::cell::Cell<bool>>,
    collapsed: HashSet<PathBuf>,
    collapsed_shared: Arc<HashSet<PathBuf>>,
    collapsed_generation: u64,
    cache: Option<RowsCache>,
    pub history: HistoryLoad,
    pub history_generation: u64,
    pub history_task: Option<Task<()>>,
    pub selected_commit: Option<String>,
    pub commit_diff: Option<CommitDiffLoad>,
    pub commit_diff_generation: u64,
    pub commit_diff_task: Option<Task<()>>,
    pub commit_scroll: UniformListScrollHandle,
}

impl Default for ReviewUi {
    fn default() -> Self {
        Self {
            mode: ReviewMode::Changes,
            layout: DiffLayout::Inline,
            split_fits: Rc::new(std::cell::Cell::new(false)),
            collapsed: HashSet::new(),
            collapsed_shared: Arc::new(HashSet::new()),
            collapsed_generation: 0,
            cache: None,
            history: HistoryLoad::Idle,
            history_generation: 0,
            history_task: None,
            selected_commit: None,
            commit_diff: None,
            commit_diff_generation: 0,
            commit_diff_task: None,
            commit_scroll: UniformListScrollHandle::new(),
        }
    }
}

impl ReviewUi {
    /// The layout the diff draws with right now.
    #[must_use]
    pub(crate) fn effective_layout(&self) -> DiffLayout {
        match self.layout {
            DiffLayout::Split if self.split_fits.get() => DiffLayout::Split,
            _ => DiffLayout::Inline,
        }
    }

    /// Collapses or expands a file's body. Survives refreshes, keyed by path.
    pub(crate) fn toggle_collapsed(&mut self, path: PathBuf) {
        if !self.collapsed.remove(&path) {
            self.collapsed.insert(path);
        }
        self.collapsed_shared = Arc::new(self.collapsed.clone());
        self.collapsed_generation = self.collapsed_generation.wrapping_add(1);
    }

    #[must_use]
    pub(crate) fn collapsed(&self) -> Arc<HashSet<PathBuf>> {
        Arc::clone(&self.collapsed_shared)
    }

    /// The view rows for `snapshot`, rebuilt only when the snapshot, layout,
    /// collapsed files, or omitted-name expansion changed.
    pub(crate) fn rows_for(
        &mut self,
        snapshot: &Arc<DiffSnapshot>,
        omitted_open: bool,
    ) -> BuiltRows {
        let layout = self.effective_layout();
        if let Some(cache) = &self.cache
            && std::ptr::eq(cache.snapshot.as_ptr(), Arc::as_ptr(snapshot))
            && cache.layout == layout
            && cache.collapsed_generation == self.collapsed_generation
            && cache.omitted_open == omitted_open
        {
            return BuiltRows {
                rows: Arc::clone(&cache.rows),
                digits: cache.digits,
            };
        }
        let rows = Arc::new(rows::build_rows(
            snapshot,
            layout,
            &self.collapsed,
            omitted_open,
        ));
        let digits = rows::line_number_digits(snapshot);
        self.cache = Some(RowsCache {
            snapshot: Arc::downgrade(snapshot),
            layout,
            collapsed_generation: self.collapsed_generation,
            omitted_open,
            rows: Arc::clone(&rows),
            digits,
        });
        BuiltRows { rows, digits }
    }

    /// The commit diff on screen, if one is loaded.
    #[must_use]
    pub(crate) fn commit_snapshot(&self) -> Option<&Arc<DiffSnapshot>> {
        match (&self.mode, &self.commit_diff) {
            (ReviewMode::Commits, Some(CommitDiffLoad::Ready(snapshot))) => Some(snapshot),
            _ => None,
        }
    }

    #[must_use]
    pub(crate) fn loaded_history(&self) -> Option<&Arc<LoadedHistory>> {
        match &self.history {
            HistoryLoad::Ready(loaded) => Some(loaded),
            _ => None,
        }
    }

    /// Forgets everything tied to the previous session's repository. The
    /// chosen mode and layout are the user's and stay.
    pub(crate) fn reset_for_context(&mut self) {
        self.collapsed.clear();
        self.collapsed_shared = Arc::new(HashSet::new());
        self.collapsed_generation = self.collapsed_generation.wrapping_add(1);
        self.cache = None;
        self.history = HistoryLoad::Idle;
        self.history_generation = self.history_generation.wrapping_add(1);
        self.history_task = None;
        self.clear_commit();
        self.commit_scroll = UniformListScrollHandle::new();
    }

    pub(crate) fn clear_commit(&mut self) {
        self.selected_commit = None;
        self.commit_diff = None;
        self.commit_diff_generation = self.commit_diff_generation.wrapping_add(1);
        self.commit_diff_task = None;
    }
}

/// Whether a patch creates its file, which discarding must never undo.
#[must_use]
pub(crate) fn patch_creates_file(patch: &[u8]) -> bool {
    patch
        .windows(b"--- /dev/null".len())
        .any(|window| window == b"--- /dev/null")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_unified_diff;

    #[test]
    fn rows_are_reused_until_an_input_changes() {
        let snapshot = Arc::new(parse_unified_diff(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n",
        ));
        let mut ui = ReviewUi::default();
        let first = ui.rows_for(&snapshot, false).rows;
        assert!(Arc::ptr_eq(&first, &ui.rows_for(&snapshot, false).rows));

        ui.toggle_collapsed(PathBuf::from("a"));
        let collapsed = ui.rows_for(&snapshot, false).rows;
        assert_eq!(collapsed.len(), 1);

        ui.layout = DiffLayout::Split;
        assert_eq!(ui.effective_layout(), DiffLayout::Inline, "too narrow");
        ui.split_fits.set(true);
        assert_eq!(ui.effective_layout(), DiffLayout::Split);
        ui.toggle_collapsed(PathBuf::from("a"));
        let split = ui.rows_for(&snapshot, false).rows;
        assert!(split.iter().any(|row| matches!(row, ViewRow::Pair { .. })));

        let refreshed = Arc::new((*snapshot).clone());
        assert!(!Arc::ptr_eq(&split, &ui.rows_for(&refreshed, false).rows));
    }

    #[test]
    fn a_context_change_keeps_the_users_choices() {
        let mut ui = ReviewUi {
            mode: ReviewMode::Commits,
            layout: DiffLayout::Split,
            selected_commit: Some("abc".to_owned()),
            ..ReviewUi::default()
        };
        ui.toggle_collapsed(PathBuf::from("a"));
        ui.reset_for_context();
        assert_eq!(ui.mode, ReviewMode::Commits);
        assert_eq!(ui.layout, DiffLayout::Split);
        assert!(ui.selected_commit.is_none());
        assert!(ui.collapsed().is_empty());
    }

    #[test]
    fn creation_patches_are_recognized() {
        assert!(patch_creates_file(b"--- /dev/null\n+++ b/new\n"));
        assert!(!patch_creates_file(b"--- a/old\n+++ b/old\n"));
    }
}
