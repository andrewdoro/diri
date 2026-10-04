// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components

//! The Files surface's file tree: lazily loaded directories flattened into
//! virtual rows, Git status and ignore state per path, a fuzzy filter that
//! reshapes the tree around matching files, and keyboard steps over rows.
//!
//! Everything here is pure; `code_viewer` owns loading (on a worker) and
//! paints the rows in a `uniform_list`, so only visible rows are built.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::code_intelligence::DirectoryEntry;

/// A path's state in Git.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GitStatus {
    Renamed,
    Added,
    Untracked,
    Modified,
    Deleted,
    Conflicted,
}

impl GitStatus {
    pub fn letter(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Untracked => "U",
            Self::Renamed => "R",
            Self::Conflicted => "!",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Modified => "Modified",
            Self::Added => "Added",
            Self::Deleted => "Deleted",
            Self::Untracked => "Untracked",
            Self::Renamed => "Renamed",
            Self::Conflicted => "Conflicted",
        }
    }

    /// From a porcelain v1 `XY` pair; `None` for clean and ignored entries.
    fn from_porcelain(x: u8, y: u8) -> Option<Self> {
        Some(match (x, y) {
            (b'?', b'?') => Self::Untracked,
            (b'!', b'!') => return None,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => Self::Conflicted,
            (b'R', _) | (_, b'R') | (b'C', _) => Self::Renamed,
            (b'A', _) => Self::Added,
            (b'D', _) | (_, b'D') => Self::Deleted,
            (b'M', _) | (_, b'M') | (b'T', _) | (_, b'T') => Self::Modified,
            _ => return None,
        })
    }
}

/// What `git status --porcelain=v1 -z --ignored` said about the workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitSnapshot {
    files: HashMap<PathBuf, GitStatus>,
    /// The strongest status beneath each folder.
    folders: HashMap<PathBuf, GitStatus>,
    /// Untracked folders Git reported whole (`dir/`).
    untracked_folders: HashSet<PathBuf>,
    ignored: HashSet<PathBuf>,
}

impl GitSnapshot {
    pub fn parse(output: &[u8]) -> Self {
        let mut snapshot = Self::default();
        let mut records = output
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty());
        while let Some(record) = records.next() {
            if record.len() < 4 {
                continue;
            }
            let (x, y) = (record[0], record[1]);
            let raw = String::from_utf8_lossy(&record[3..]).into_owned();
            // A rename or copy is followed by its source path.
            if matches!(x, b'R' | b'C') {
                records.next();
            }
            let folder = raw.ends_with('/');
            let path = PathBuf::from(raw.trim_end_matches('/'));
            if (x, y) == (b'!', b'!') {
                snapshot.ignored.insert(path);
                continue;
            }
            let Some(status) = GitStatus::from_porcelain(x, y) else {
                continue;
            };
            if folder {
                snapshot.untracked_folders.insert(path.clone());
                snapshot.folders.insert(path.clone(), status);
            } else {
                snapshot.files.insert(path.clone(), status);
            }
            for ancestor in path.ancestors().skip(1) {
                if ancestor.as_os_str().is_empty() {
                    break;
                }
                let slot = snapshot
                    .folders
                    .entry(ancestor.to_path_buf())
                    .or_insert(status);
                *slot = (*slot).max(status);
            }
        }
        snapshot
    }

    /// A file's own status, or a folder's strongest beneath it; files inside
    /// an untracked folder are untracked.
    pub fn status(&self, path: &Path, is_dir: bool) -> Option<GitStatus> {
        if is_dir {
            if let Some(status) = self.folders.get(path) {
                return Some(*status);
            }
        } else if let Some(status) = self.files.get(path) {
            return Some(*status);
        }
        path.ancestors()
            .skip(1)
            .any(|ancestor| self.untracked_folders.contains(ancestor))
            .then_some(GitStatus::Untracked)
    }

    /// The path, or a folder holding it, is ignored by `.gitignore`.
    pub fn is_ignored(&self, path: &Path) -> bool {
        !self.ignored.is_empty()
            && path
                .ancestors()
                .any(|ancestor| self.ignored.contains(ancestor))
    }
}

/// One visible row of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub status: Option<GitStatus>,
    pub ignored: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// Bytes of `name` the filter matched, for emphasis.
    pub matched: Vec<Range<usize>>,
}

/// The fuzzy filter's answer: matching files, best first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterResults {
    pub query: String,
    pub paths: Vec<PathBuf>,
}

/// Where a key moves the tree's cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Move {
    To(usize),
    Expand(PathBuf),
    Collapse(PathBuf),
    Activate(usize),
}

#[derive(Debug, Default)]
pub struct FileTree {
    directories: HashMap<PathBuf, Vec<DirectoryEntry>>,
    expanded: HashSet<PathBuf>,
    loading: HashSet<PathBuf>,
    errors: HashMap<PathBuf, String>,
    selected: Option<PathBuf>,
    git: Option<GitSnapshot>,
    show_ignored: bool,
    filter: Option<FilterResults>,
    /// Paths with unsaved edits in the editor.
    dirty: HashSet<PathBuf>,
    rows: RefCell<Option<Rc<Vec<TreeRow>>>>,
}

impl FileTree {
    fn changed(&self) {
        self.rows.borrow_mut().take();
    }

    pub fn clear(&mut self) {
        *self = Self {
            show_ignored: self.show_ignored,
            ..Self::default()
        };
    }

    /// Forgets loaded directories (keeping what is expanded) so they reload.
    pub fn forget_loaded(&mut self) {
        self.directories.clear();
        self.loading.clear();
        self.errors.clear();
        self.changed();
    }

    pub fn is_loading(&self, path: &Path) -> bool {
        self.loading.contains(path)
    }

    pub fn has_loaded_anything(&self) -> bool {
        !self.directories.is_empty() || !self.loading.is_empty() || !self.errors.is_empty()
    }

    pub fn root_error(&self) -> Option<&str> {
        self.errors.get(Path::new("")).map(String::as_str)
    }

    /// Marks a directory as loading; false when it already is.
    pub fn begin_load(&mut self, path: &Path) -> bool {
        if !self.loading.insert(path.to_path_buf()) {
            return false;
        }
        self.errors.remove(path);
        self.changed();
        true
    }

    pub fn finish_load(&mut self, path: PathBuf, result: Result<Vec<DirectoryEntry>, String>) {
        self.loading.remove(&path);
        match result {
            Ok(entries) => {
                self.directories.insert(path, entries);
            }
            Err(error) => {
                self.errors.insert(path, error);
            }
        }
        self.changed();
    }

    pub fn expanded(&self) -> impl Iterator<Item = &PathBuf> {
        self.expanded.iter()
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    /// Opens or closes a folder. Returns true when it opened and its
    /// children still need loading.
    pub fn set_expanded(&mut self, path: &Path, open: bool) -> bool {
        let changed = if open {
            self.expanded.insert(path.to_path_buf())
        } else {
            self.expanded.remove(path)
        };
        if changed {
            self.changed();
        }
        open && !self.directories.contains_key(path) && !self.loading.contains(path)
    }

    /// Closes every folder.
    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.changed();
    }

    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    pub fn select(&mut self, path: Option<PathBuf>) {
        if self.selected != path {
            self.selected = path;
            self.changed();
        }
    }

    /// Selects `path` and opens every folder above it. Returns the folders
    /// that still need loading, outermost first.
    pub fn reveal(&mut self, path: &Path) -> Vec<PathBuf> {
        self.select(Some(path.to_path_buf()));
        let mut missing = Vec::new();
        let mut ancestors: Vec<&Path> = path.ancestors().skip(1).collect();
        ancestors.reverse();
        for ancestor in ancestors {
            if !ancestor.as_os_str().is_empty() {
                self.expanded.insert(ancestor.to_path_buf());
            }
            if !self.directories.contains_key(ancestor) && !self.loading.contains(ancestor) {
                missing.push(ancestor.to_path_buf());
            }
        }
        self.changed();
        missing
    }

    pub fn set_git(&mut self, git: Option<GitSnapshot>) {
        if self.git != git {
            self.git = git;
            self.changed();
        }
    }

    pub fn show_ignored(&self) -> bool {
        self.show_ignored
    }

    pub fn set_show_ignored(&mut self, show: bool) {
        self.show_ignored = show;
        self.changed();
    }

    pub fn set_dirty(&mut self, dirty: HashSet<PathBuf>) {
        if self.dirty != dirty {
            self.dirty = dirty;
            self.changed();
        }
    }

    pub fn is_dirty(&self, path: &Path) -> bool {
        self.dirty.contains(path)
    }

    pub fn filter(&self) -> Option<&FilterResults> {
        self.filter.as_ref()
    }

    pub fn set_filter(&mut self, filter: Option<FilterResults>) {
        if self.filter != filter {
            self.filter = filter;
            self.changed();
        }
    }

    fn ignored(&self, path: &Path) -> bool {
        self.git.as_ref().is_some_and(|git| git.is_ignored(path))
    }

    fn status(&self, path: &Path, is_dir: bool) -> Option<GitStatus> {
        self.git.as_ref().and_then(|git| git.status(path, is_dir))
    }

    /// The rows in view, depth first through open folders, or the filter's
    /// matches gathered under their folders. Cached until the tree changes.
    pub fn rows(&self) -> Rc<Vec<TreeRow>> {
        if let Some(rows) = self.rows.borrow().as_ref() {
            return rows.clone();
        }
        let rows = Rc::new(match &self.filter {
            Some(filter) => self.filtered_rows(filter),
            None => {
                let mut rows = Vec::new();
                self.visit(Path::new(""), 0, &mut rows);
                rows
            }
        });
        *self.rows.borrow_mut() = Some(rows.clone());
        rows
    }

    fn row(&self, path: &Path, depth: usize, is_dir: bool, expanded: bool) -> TreeRow {
        TreeRow {
            path: path.to_path_buf(),
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            depth,
            is_dir,
            expanded,
            status: self.status(path, is_dir),
            ignored: self.ignored(path),
            loading: self.loading.contains(path),
            error: self.errors.get(path).cloned(),
            matched: Vec::new(),
        }
    }

    fn visit(&self, path: &Path, depth: usize, rows: &mut Vec<TreeRow>) {
        let Some(entries) = self.directories.get(path) else {
            return;
        };
        for entry in entries {
            let ignored = self.ignored(&entry.relative_path);
            if ignored && !self.show_ignored {
                continue;
            }
            let expanded = entry.is_dir && self.expanded.contains(&entry.relative_path);
            rows.push(self.row(&entry.relative_path, depth, entry.is_dir, expanded));
            if expanded {
                self.visit(&entry.relative_path, depth + 1, rows);
            }
        }
    }

    fn filtered_rows(&self, filter: &FilterResults) -> Vec<TreeRow> {
        #[derive(Default)]
        struct Folder {
            folders: BTreeMap<String, Folder>,
            files: Vec<(String, PathBuf)>,
        }
        let mut root = Folder::default();
        for path in &filter.paths {
            let mut folder = &mut root;
            let mut components: Vec<String> = path
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            let Some(file) = components.pop() else {
                continue;
            };
            for part in components {
                folder = folder.folders.entry(part).or_default();
            }
            folder.files.push((file, path.clone()));
        }
        fn walk(
            tree: &FileTree,
            folder: &Folder,
            prefix: &Path,
            depth: usize,
            query: &str,
            rows: &mut Vec<TreeRow>,
        ) {
            let mut folders: Vec<_> = folder.folders.iter().collect();
            folders.sort_by_key(|(name, _)| name.to_lowercase());
            for (name, inner) in folders {
                let path = prefix.join(name);
                rows.push(tree.row(&path, depth, true, true));
                walk(tree, inner, &path, depth + 1, query, rows);
            }
            let mut files = folder.files.clone();
            files.sort_by_key(|(name, _)| name.to_lowercase());
            for (name, path) in files {
                let mut row = tree.row(&path, depth, false, false);
                row.matched = fuzzy_positions(query, &name);
                rows.push(row);
            }
        }
        let mut rows = Vec::new();
        walk(self, &root, Path::new(""), 0, &filter.query, &mut rows);
        rows
    }
}

/// Bytes of `name` a fuzzy query picks out: a contiguous match when there is
/// one, else the first subsequence, as merged ranges.
pub fn fuzzy_positions(query: &str, name: &str) -> Vec<Range<usize>> {
    let query = query.trim().rsplit('/').next().unwrap_or("").to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let lower = name.to_lowercase();
    if lower.len() == name.len()
        && let Some(at) = lower.find(&query)
    {
        return std::iter::once(at..at + query.len()).collect();
    }
    let mut wanted = query.chars().peekable();
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (at, ch) in name.char_indices() {
        let Some(&next) = wanted.peek() else {
            break;
        };
        if ch.to_lowercase().eq(next.to_lowercase()) {
            wanted.next();
            let end = at + ch.len_utf8();
            match ranges.last_mut() {
                Some(last) if last.end == at => last.end = end,
                _ => ranges.push(at..end),
            }
        }
    }
    if wanted.peek().is_some() {
        return Vec::new();
    }
    ranges
}

/// Where `key` takes the cursor from row `at`.
pub fn step(rows: &[TreeRow], at: usize, key: &str, page: usize) -> Option<Move> {
    if rows.is_empty() {
        return None;
    }
    let at = at.min(rows.len() - 1);
    let row = &rows[at];
    let last = rows.len() - 1;
    match key {
        "up" => Some(Move::To(at.saturating_sub(1))),
        "down" => Some(Move::To((at + 1).min(last))),
        "home" => Some(Move::To(0)),
        "end" => Some(Move::To(last)),
        "pageup" => Some(Move::To(at.saturating_sub(page.max(1)))),
        "pagedown" => Some(Move::To((at + page.max(1)).min(last))),
        "enter" | "space" => Some(Move::Activate(at)),
        "right" if row.is_dir && !row.expanded => Some(Move::Expand(row.path.clone())),
        "right" if row.is_dir => rows
            .get(at + 1)
            .filter(|child| child.depth > row.depth)
            .map(|_| Move::To(at + 1)),
        "right" => None,
        "left" if row.is_dir && row.expanded => Some(Move::Collapse(row.path.clone())),
        "left" => rows[..at]
            .iter()
            .rposition(|candidate| candidate.depth < row.depth)
            .map(Move::To),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn entry(path: &str, is_dir: bool) -> DirectoryEntry {
        DirectoryEntry {
            relative_path: PathBuf::from(path),
            is_dir,
        }
    }

    fn tree() -> FileTree {
        let mut tree = FileTree::default();
        tree.finish_load(
            PathBuf::new(),
            Ok(vec![
                entry("src", true),
                entry("target", true),
                entry("README.md", false),
            ]),
        );
        tree.finish_load(
            PathBuf::from("src"),
            Ok(vec![entry("src/ui", true), entry("src/lib.rs", false)]),
        );
        tree
    }

    fn names(rows: &[TreeRow]) -> Vec<(String, usize)> {
        rows.iter()
            .map(|row| (row.name.clone(), row.depth))
            .collect()
    }

    #[test]
    fn rows_walk_only_through_open_folders_and_cache_until_changed() {
        let mut tree = tree();
        assert_eq!(
            names(&tree.rows()),
            [
                ("src".into(), 0),
                ("target".into(), 0),
                ("README.md".into(), 0)
            ]
        );
        assert!(!tree.set_expanded(Path::new("src"), true), "already loaded");
        let first = tree.rows();
        assert!(Rc::ptr_eq(&first, &tree.rows()), "cached");
        assert_eq!(
            names(&first),
            [
                ("src".into(), 0),
                ("ui".into(), 1),
                ("lib.rs".into(), 1),
                ("target".into(), 0),
                ("README.md".into(), 0)
            ]
        );
        assert!(
            tree.set_expanded(Path::new("src/ui"), true),
            "an unloaded folder asks to load"
        );
        assert!(!Rc::ptr_eq(&first, &tree.rows()), "a change rebuilds");
        assert!(tree.begin_load(Path::new("src/ui")));
        assert!(!tree.begin_load(Path::new("src/ui")), "one load at a time");
        assert!(tree.rows()[1].loading);
    }

    #[test]
    fn git_snapshot_parses_statuses_renames_untracked_folders_and_ignores() {
        let output = b" M src/lib.rs\0R  src/new.rs\0src/old.rs\0?? scratch/\0!! target/\0!! .env\0A  src/ui/added.rs\0UU conflict.rs\0";
        let git = GitSnapshot::parse(output);
        assert_eq!(
            git.status(Path::new("src/lib.rs"), false),
            Some(GitStatus::Modified)
        );
        assert_eq!(
            git.status(Path::new("src/new.rs"), false),
            Some(GitStatus::Renamed)
        );
        assert_eq!(
            git.status(Path::new("src/old.rs"), false),
            None,
            "a rename source is not listed"
        );
        assert_eq!(
            git.status(Path::new("scratch/notes.txt"), false),
            Some(GitStatus::Untracked)
        );
        assert_eq!(
            git.status(Path::new("scratch"), true),
            Some(GitStatus::Untracked)
        );
        assert_eq!(
            git.status(Path::new("src"), true),
            Some(GitStatus::Modified),
            "the strongest beneath"
        );
        assert_eq!(
            git.status(Path::new("conflict.rs"), false),
            Some(GitStatus::Conflicted)
        );
        assert!(git.is_ignored(Path::new("target")));
        assert!(git.is_ignored(Path::new("target/debug/app")));
        assert!(git.is_ignored(Path::new(".env")));
        assert!(!git.is_ignored(Path::new("src/lib.rs")));
    }

    #[test]
    fn ignored_entries_hide_until_shown_and_carry_their_status() {
        let mut tree = tree();
        tree.set_git(Some(GitSnapshot::parse(b"!! target/\0?? README.md\0")));
        assert_eq!(
            names(&tree.rows()),
            [("src".into(), 0), ("README.md".into(), 0)]
        );
        assert_eq!(tree.rows()[1].status, Some(GitStatus::Untracked));
        tree.set_show_ignored(true);
        let rows = tree.rows();
        assert!(rows.iter().any(|row| row.name == "target" && row.ignored));
    }

    #[test]
    fn reveal_opens_every_folder_above_and_reports_what_to_load() {
        let mut tree = tree();
        let missing = tree.reveal(Path::new("src/ui/button.rs"));
        assert_eq!(missing, vec![PathBuf::from("src/ui")]);
        assert!(tree.is_expanded(Path::new("src")) && tree.is_expanded(Path::new("src/ui")));
        assert_eq!(tree.selected(), Some(Path::new("src/ui/button.rs")));
    }

    #[test]
    fn a_filter_reshapes_the_tree_around_matching_files() {
        let mut tree = tree();
        tree.set_filter(Some(FilterResults {
            query: "btn".into(),
            paths: vec![
                "src/ui/button.rs".into(),
                "README.md".into(),
                "src/bin/tool.rs".into(),
            ],
        }));
        let rows = tree.rows();
        assert_eq!(
            names(&rows),
            [
                ("src".into(), 0),
                ("bin".into(), 1),
                ("tool.rs".into(), 2),
                ("ui".into(), 1),
                ("button.rs".into(), 2),
                ("README.md".into(), 0)
            ]
        );
        assert!(rows[0].expanded && rows[0].is_dir);
        assert_eq!(rows[4].matched, vec![0..1, 2..3, 5..6], "b·t·n in button");
    }

    #[test]
    fn fuzzy_positions_prefer_contiguous_runs() {
        assert_eq!(fuzzy_positions("view", "code_viewer.rs"), [5..9].to_vec());
        assert_eq!(fuzzy_positions("cv", "code_viewer.rs"), vec![0..1, 5..6]);
        assert_eq!(
            fuzzy_positions("src/cv", "code_viewer.rs"),
            vec![0..1, 5..6],
            "the last segment counts"
        );
        assert!(fuzzy_positions("zz", "code_viewer.rs").is_empty());
    }

    #[test]
    fn keys_walk_open_close_climb_and_page() {
        let mut tree = tree();
        tree.set_expanded(Path::new("src"), true);
        let rows = tree.rows();
        assert_eq!(
            step(&rows, 0, "right", 10),
            Some(Move::To(1)),
            "an open folder enters its first child"
        );
        assert_eq!(
            step(&rows, 1, "right", 10),
            Some(Move::Expand("src/ui".into()))
        );
        assert_eq!(
            step(&rows, 0, "left", 10),
            Some(Move::Collapse("src".into()))
        );
        assert_eq!(
            step(&rows, 2, "left", 10),
            Some(Move::To(0)),
            "a child climbs to its parent"
        );
        assert_eq!(
            step(&rows, 2, "right", 10),
            None,
            "a file has nowhere to go"
        );
        assert_eq!(step(&rows, 4, "down", 10), Some(Move::To(4)));
        assert_eq!(step(&rows, 0, "end", 10), Some(Move::To(4)));
        assert_eq!(step(&rows, 0, "pagedown", 3), Some(Move::To(3)));
        assert_eq!(step(&rows, 4, "pageup", 3), Some(Move::To(1)));
        assert_eq!(step(&rows, 2, "enter", 3), Some(Move::Activate(2)));
        assert_eq!(step(&[], 0, "down", 3), None);
    }
}
