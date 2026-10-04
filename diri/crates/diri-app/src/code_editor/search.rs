// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components

//! Workspace search: a query over every indexed file, narrowed by include
//! and exclude globs, collected as per-file line hits. Blocking by design;
//! the Files surface runs it on a worker and cancels stale queries.

use std::ops::Range;
use std::path::{Path, PathBuf};

use regex::Regex;

use super::find::{FindOptions, compile};
use crate::code_intelligence::CodeIntelligence;

/// Hits past this count stop the search.
pub const HIT_LIMIT: usize = 5_000;
/// A preview keeps this many bytes of its line around the first hit.
const PREVIEW_BYTES: usize = 220;

/// Include and exclude globs, comma separated, as typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchFilters {
    pub include: String,
    pub exclude: String,
}

/// Compiled filters.
#[derive(Clone, Debug, Default)]
pub struct Filters {
    include: Vec<Regex>,
    exclude: Vec<Regex>,
}

impl Filters {
    pub fn parse(filters: &SearchFilters) -> Result<Self, String> {
        let compile_all = |patterns: &str| -> Result<Vec<Regex>, String> {
            split_patterns(patterns)
                .into_iter()
                .map(str::trim)
                .filter(|pattern| !pattern.is_empty())
                .map(|pattern| {
                    Regex::new(&glob_regex(pattern))
                        .map_err(|_| format!("Invalid glob “{pattern}”"))
                })
                .collect()
        };
        Ok(Self {
            include: compile_all(&filters.include)?,
            exclude: compile_all(&filters.exclude)?,
        })
    }

    /// A path passes when it matches some include (or there are none) and no exclude.
    pub fn passes(&self, path: &str) -> bool {
        (self.include.is_empty() || self.include.iter().any(|glob| glob.is_match(path)))
            && !self.exclude.iter().any(|glob| glob.is_match(path))
    }
}

/// Comma-separated globs, keeping commas inside `{a,b}` alternatives.
fn split_patterns(patterns: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (at, ch) in patterns.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(&patterns[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    out.push(&patterns[start..]);
    out
}

/// A glob as an anchored regular expression. `*` stays within a path
/// segment, `**` crosses them, `?` is one character, `{a,b}` is either.
/// A pattern without `/` matches a file or folder name anywhere; one with
/// `/` matches from the workspace root. A folder match covers its contents.
pub fn glob_regex(pattern: &str) -> String {
    let pattern = pattern.trim_start_matches("./").trim_start_matches('/');
    let pattern = pattern.trim_end_matches('/');
    let anchored = pattern.contains('/');
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    let mut braces = 0usize;
    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    out.push_str("(?:.*/)?");
                } else {
                    out.push_str(".*");
                }
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '{' => {
                braces += 1;
                out.push_str("(?:");
            }
            '}' if braces > 0 => {
                braces -= 1;
                out.push(')');
            }
            ',' if braces > 0 => out.push('|'),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    for _ in 0..braces {
        out.push(')');
    }
    if anchored {
        format!("^{out}(?:/.*)?$")
    } else {
        format!("(?:^|/){out}(?:/.*)?$")
    }
}

/// One line that matched, as shown: a trimmed preview and where in it the
/// query matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineHit {
    /// One-based.
    pub line: usize,
    /// One-based character column of the first match.
    pub column: usize,
    pub preview: String,
    pub ranges: Vec<Range<usize>>,
}

/// A file's hits, in line order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHits {
    pub relative_path: PathBuf,
    pub hits: Vec<LineHit>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchOutcome {
    pub files: Vec<FileHits>,
    pub total: usize,
    /// The hit limit cut the search short.
    pub truncated: bool,
}

/// The hits of `pattern` in one file's text.
pub fn hits_in(text: &str, pattern: &Regex, budget: usize) -> Vec<LineHit> {
    let mut hits = Vec::new();
    for (index, raw_line) in text.split('\n').enumerate() {
        if hits.len() >= budget {
            break;
        }
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let found: Vec<Range<usize>> = pattern
            .find_iter(line)
            .filter(|hit| !hit.is_empty())
            .map(|hit| hit.range())
            .collect();
        let Some(first) = found.first() else {
            continue;
        };
        let (preview, shift) = preview_of(line, first.start);
        let ranges = found
            .iter()
            .filter_map(|range| {
                let start = range.start.checked_sub(shift)?;
                let end = (range.end - shift).min(preview.len());
                (start < end).then_some(start..end)
            })
            .collect();
        hits.push(LineHit {
            line: index + 1,
            column: line[..first.start].chars().count() + 1,
            preview,
            ranges,
        });
    }
    hits
}

/// A line cut to a preview: leading indent dropped and, for long lines,
/// a window around the first hit. Returns the preview and how many bytes
/// were dropped from the front.
fn preview_of(line: &str, first: usize) -> (String, usize) {
    let indent = line.len() - line.trim_start().len();
    let mut start = indent.min(first);
    if first - start > PREVIEW_BYTES / 2 {
        start = first - PREVIEW_BYTES / 3;
    }
    while !line.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + PREVIEW_BYTES).min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    (line[start..end].trim_end().to_string(), start)
}

/// Searches every indexed file that passes the filters. Files are read
/// through the index's bounds (size, binary, UTF-8), so nothing outside the
/// workspace or too large to show is ever read.
pub fn run(
    intelligence: &CodeIntelligence,
    query: &str,
    options: FindOptions,
    filters: &SearchFilters,
    cancelled: impl Fn() -> bool,
) -> Result<SearchOutcome, String> {
    if query.is_empty() {
        return Ok(SearchOutcome::default());
    }
    let pattern = compile(query, options)?;
    let filters = Filters::parse(filters)?;
    let mut outcome = SearchOutcome::default();
    for relative_path in intelligence.indexed_files() {
        if cancelled() {
            break;
        }
        let display = relative_path.to_string_lossy().replace('\\', "/");
        if !filters.passes(&display) {
            continue;
        }
        let Ok(text) = intelligence.read_text(&relative_path) else {
            continue;
        };
        let budget = HIT_LIMIT - outcome.total;
        let hits = hits_in(&text, &pattern, budget);
        if hits.is_empty() {
            continue;
        }
        outcome.total += hits.len();
        outcome.files.push(FileHits {
            relative_path,
            hits,
        });
        if outcome.total >= HIT_LIMIT {
            outcome.truncated = true;
            break;
        }
    }
    Ok(outcome)
}

/// The files of an outcome flattened to rows: a header per file, then its
/// hits, unless the file is collapsed. Virtualized lists render from this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultRow {
    File(usize),
    Hit(usize, usize),
}

pub fn result_rows(
    outcome: &SearchOutcome,
    collapsed: &std::collections::HashSet<PathBuf>,
) -> Vec<ResultRow> {
    let mut rows = Vec::with_capacity(outcome.files.len() + outcome.total);
    for (file, hits) in outcome.files.iter().enumerate() {
        rows.push(ResultRow::File(file));
        if !collapsed.contains(&hits.relative_path) {
            rows.extend((0..hits.hits.len()).map(|hit| ResultRow::Hit(file, hit)));
        }
    }
    rows
}

/// A path split for display: its file name and the folder it sits in.
pub fn split_path(path: &Path) -> (String, String) {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let folder = path
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default();
    (name, folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn filters(include: &str, exclude: &str) -> Filters {
        Filters::parse(&SearchFilters {
            include: include.into(),
            exclude: exclude.into(),
        })
        .unwrap()
    }

    #[test]
    fn globs_match_names_anywhere_and_paths_from_the_root() {
        let rust = filters("*.rs", "");
        assert!(rust.passes("src/main.rs"));
        assert!(rust.passes("main.rs"));
        assert!(!rust.passes("src/main.rs.bak"));
        let folder = filters("tests", "");
        assert!(
            folder.passes("crates/app/tests/one.rs"),
            "a folder covers its contents"
        );
        assert!(!folder.passes("crates/app/src/tests.rs"));
        let rooted = filters("crates/*/src/**", "");
        assert!(rooted.passes("crates/app/src/deep/x.rs"));
        assert!(!rooted.passes("other/crates/app/src/x.rs"));
        let any_depth = filters("**/*.md", "");
        assert!(any_depth.passes("README.md") && any_depth.passes("docs/a/b.md"));
        let braces = filters("*.{ts,tsx}", "");
        assert!(braces.passes("a/b.tsx") && braces.passes("b.ts") && !braces.passes("b.js"));
        assert!(filters("", "").passes("anything"));
        let both = filters("*.rs, *.toml", "target, *_test.rs");
        assert!(both.passes("Cargo.toml"));
        assert!(!both.passes("target/debug/build.rs"));
        assert!(!both.passes("src/a_test.rs"));
        assert!(filters("?.rs", "").passes("a.rs") && !filters("?.rs", "").passes("ab.rs"));
    }

    #[test]
    fn line_hits_keep_every_match_and_a_trimmed_preview() {
        let pattern = compile("cat", FindOptions::default()).unwrap();
        let hits = hits_in("    let cat = Cat;\nnone\r\n\tcatalog cat", &pattern, 100);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[0].column, 9);
        assert_eq!(hits[0].preview, "let cat = Cat;");
        assert_eq!(hits[0].ranges, vec![4..7, 10..13]);
        assert_eq!(hits[1].line, 3);
        assert_eq!(hits[1].ranges, vec![0..3, 8..11]);
        assert_eq!(
            hits_in("cat cat\ncat", &pattern, 1).len(),
            1,
            "the budget bounds hits"
        );
    }

    #[test]
    fn long_lines_preview_a_window_around_the_hit() {
        let pattern = compile("needle", FindOptions::default()).unwrap();
        let line = format!("{}needle{}", "é".repeat(400), "x".repeat(400));
        let hits = hits_in(&line, &pattern, 10);
        let hit = &hits[0];
        assert!(hit.preview.len() <= PREVIEW_BYTES);
        assert_eq!(&hit.preview[hit.ranges[0].clone()], "needle");
    }

    #[test]
    fn rows_flatten_files_and_skip_collapsed_ones() {
        let outcome = SearchOutcome {
            files: vec![
                FileHits {
                    relative_path: "a.rs".into(),
                    hits: vec![
                        LineHit {
                            line: 1,
                            column: 1,
                            preview: "x".into(),
                            ranges: vec![],
                        },
                        LineHit {
                            line: 2,
                            column: 1,
                            preview: "x".into(),
                            ranges: vec![],
                        },
                    ],
                },
                FileHits {
                    relative_path: "b.rs".into(),
                    hits: vec![LineHit {
                        line: 1,
                        column: 1,
                        preview: "x".into(),
                        ranges: vec![],
                    }],
                },
            ],
            total: 3,
            truncated: false,
        };
        assert_eq!(
            result_rows(&outcome, &HashSet::new()),
            vec![
                ResultRow::File(0),
                ResultRow::Hit(0, 0),
                ResultRow::Hit(0, 1),
                ResultRow::File(1),
                ResultRow::Hit(1, 0)
            ]
        );
        let collapsed = HashSet::from([PathBuf::from("a.rs")]);
        assert_eq!(
            result_rows(&outcome, &collapsed),
            vec![ResultRow::File(0), ResultRow::File(1), ResultRow::Hit(1, 0)]
        );
    }

    #[test]
    fn workspace_search_respects_filters_and_cancellation() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join(".git")).unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(workspace.path().join("src/a.rs"), "fn needle() {}\n").unwrap();
        std::fs::write(workspace.path().join("notes.md"), "a needle here\n").unwrap();
        let intelligence = CodeIntelligence::for_session(workspace.path()).unwrap();
        let everything = run(
            &intelligence,
            "needle",
            FindOptions::default(),
            &SearchFilters::default(),
            || false,
        )
        .unwrap();
        assert_eq!(everything.total, 2);
        let rust_only = run(
            &intelligence,
            "needle",
            FindOptions::default(),
            &SearchFilters {
                include: "*.rs".into(),
                exclude: String::new(),
            },
            || false,
        )
        .unwrap();
        assert_eq!(rust_only.files.len(), 1);
        assert_eq!(rust_only.files[0].relative_path, Path::new("src/a.rs"));
        let cancelled = run(
            &intelligence,
            "needle",
            FindOptions::default(),
            &SearchFilters::default(),
            || true,
        )
        .unwrap();
        assert_eq!(cancelled.total, 0);
        let regex = FindOptions {
            regex: true,
            ..FindOptions::default()
        };
        assert!(
            run(&intelligence, "(", regex, &SearchFilters::default(), || {
                false
            })
            .is_err()
        );
    }
}
