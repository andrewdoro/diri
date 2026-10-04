// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components

//! What the editor knows about code without a language server: words in
//! the buffer, declarations from the workspace symbol index, the call a
//! caret sits in, and the blocks around a line. These feed the completion
//! menu, signature help, hover cards, code lenses, and the breadcrumb.

use std::collections::HashMap;
use std::ops::Range;

use crate::code_intelligence::{SourceLanguage, symbol_name};

use super::buffer::{Buffer, is_word_char};
use super::syntax::{Fold, enclosing};

/// Where a completion came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CandidateKind {
    /// A word already in this file.
    Word,
    /// A declaration somewhere in the workspace.
    Symbol,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub label: String,
    pub detail: Option<String>,
    pub kind: CandidateKind,
}

/// Every identifier in the text and how often it appears. Words shorter
/// than two characters and numbers are left out.
pub fn word_counts(text: &str) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut start = None;
    for (at, ch) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if is_word_char(ch) {
            start.get_or_insert(at);
        } else if let Some(from) = start.take() {
            let word = &text[from..at];
            if word.len() >= 2 && !word.starts_with(|ch: char| ch.is_ascii_digit()) {
                *counts.entry(word.to_owned()).or_default() += 1;
            }
        }
    }
    counts
}

/// Candidates for `prefix`, best first: those starting with it (case
/// first matching exactly), then those containing its letters in order.
/// The prefix itself is never offered back.
pub fn rank(
    prefix: &str,
    candidates: impl IntoIterator<Item = Candidate>,
    limit: usize,
) -> Vec<Candidate> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let lower = prefix.to_lowercase();
    let mut scored: Vec<(u32, Candidate)> = candidates
        .into_iter()
        .filter(|candidate| candidate.label != prefix)
        .filter_map(|candidate| {
            let label = candidate.label.to_lowercase();
            let score = if candidate.label.starts_with(prefix) {
                0
            } else if label.starts_with(&lower) {
                1
            } else if subsequence(&lower, &label) {
                2
            } else {
                return None;
            };
            Some((score, candidate))
        })
        .collect();
    scored.sort_by(|(left_score, left), (right_score, right)| {
        left_score
            .cmp(right_score)
            .then_with(|| {
                (left.kind == CandidateKind::Symbol).cmp(&(right.kind == CandidateKind::Symbol))
            })
            .then_with(|| left.label.len().cmp(&right.label.len()))
            .then_with(|| left.label.cmp(&right.label))
    });
    let mut seen = std::collections::HashSet::new();
    scored
        .into_iter()
        .map(|(_, candidate)| candidate)
        .filter(|candidate| seen.insert(candidate.label.clone()))
        .take(limit)
        .collect()
}

fn subsequence(query: &str, candidate: &str) -> bool {
    let mut wanted = query.chars().peekable();
    for ch in candidate.chars() {
        if wanted.peek() == Some(&ch) {
            wanted.next();
        }
    }
    wanted.peek().is_none()
}

/// The call a caret sits inside on its line: the callee's name and which
/// argument (from zero) the caret is in. Brackets and strings before the
/// caret are balanced on the way back.
pub fn call_context(before_caret: &str) -> Option<(String, usize)> {
    let bytes = before_caret.as_bytes();
    let mut depth = 0usize;
    let mut argument = 0usize;
    let mut at = bytes.len();
    let mut quote: Option<u8> = None;
    while at > 0 {
        at -= 1;
        let byte = bytes[at];
        if let Some(open) = quote {
            if byte == open && (at == 0 || bytes[at - 1] != b'\\') {
                quote = None;
            }
            continue;
        }
        match byte {
            b'"' | b'`' => quote = Some(byte),
            b')' | b']' | b'}' => depth += 1,
            b'[' | b'{' if depth > 0 => depth -= 1,
            b'[' | b'{' => return None,
            b'(' if depth > 0 => depth -= 1,
            b'(' => {
                let name_end = before_caret[..at].trim_end_matches([' ', '!']).len();
                let name: String = before_caret[..name_end]
                    .chars()
                    .rev()
                    .take_while(|ch| is_word_char(*ch))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                if name.is_empty() || name.starts_with(|ch: char| ch.is_ascii_digit()) {
                    return None;
                }
                return Some((name, argument));
            }
            b',' if depth == 0 => argument += 1,
            b';' if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

/// The parameter list of a declaration line: the span inside its first
/// parentheses and each top-level parameter's span, as bytes of `line`.
pub fn parameters(line: &str) -> Option<Vec<Range<usize>>> {
    let open = line.find('(')?;
    let bytes = line.as_bytes();
    let mut depth = 0usize;
    let mut start = open + 1;
    let mut out = Vec::new();
    for (at, byte) in bytes.iter().enumerate().skip(open + 1) {
        match byte {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' if depth == 0 => {
                let range = trim_range(line, start..at);
                if !range.is_empty() {
                    out.push(range);
                }
                return Some(out);
            }
            b')' | b']' | b'}' | b'>' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                out.push(trim_range(line, start..at));
                start = at + 1;
            }
            _ => {}
        }
    }
    let range = trim_range(line, start..line.len());
    if !range.is_empty() {
        out.push(range);
    }
    Some(out)
}

fn trim_range(text: &str, range: Range<usize>) -> Range<usize> {
    let slice = &text[range.clone()];
    let start = range.start + (slice.len() - slice.trim_start().len());
    let end = range.end - (slice.len() - slice.trim_end().len());
    start..end.max(start)
}

/// A code lens over a declaration: how many other times its name appears
/// in this file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lens {
    pub line: usize,
    pub name: String,
    pub uses: usize,
}

const LENS_DECLARATIONS: &[&str] = &[
    "fn ",
    "struct ",
    "enum ",
    "trait ",
    "type ",
    "mod ",
    "class ",
    "def ",
    "func ",
    "function ",
    "interface ",
    "protocol ",
    "actor ",
    "extension ",
    "module ",
    "record ",
    "object ",
];

/// Strips access modifiers so the declaration keyword leads.
fn declaration(line: &str) -> &str {
    let mut line = line.trim_start();
    loop {
        let next = [
            "pub(crate) ",
            "pub(super) ",
            "pub ",
            "public ",
            "private ",
            "protected ",
            "internal ",
            "open ",
            "final ",
            "static ",
            "async ",
            "export default ",
            "export ",
            "default ",
            "unsafe ",
            "const ",
            "abstract ",
            "override ",
        ]
        .into_iter()
        .find_map(|prefix| line.strip_prefix(prefix));
        let Some(next) = next else {
            break;
        };
        line = next.trim_start();
    }
    line
}

/// Lenses for the declarations in a buffer.
pub fn lenses(
    buffer: &Buffer,
    language: SourceLanguage,
    counts: &HashMap<String, usize>,
) -> Vec<Lens> {
    (0..buffer.lines())
        .filter_map(|line| {
            let text = buffer.line(line);
            let declared = declaration(text);
            if !LENS_DECLARATIONS
                .iter()
                .any(|keyword| declared.starts_with(keyword))
            {
                return None;
            }
            let name = symbol_name(text, language)?;
            let name: String = name.chars().take_while(|ch| is_word_char(*ch)).collect();
            if name.len() < 2 {
                return None;
            }
            let uses = counts.get(&name).copied().unwrap_or(1).saturating_sub(1);
            // A declaration nothing else here mentions needs no lens.
            (uses > 0).then_some(Lens { line, name, uses })
        })
        .collect()
}

/// One step of the breadcrumb: a block around the cursor and its header line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub line: usize,
}

/// The named blocks around `line`, outermost first. A header without a
/// declaration name is shown by its text up to its opening bracket.
pub fn breadcrumb(
    buffer: &Buffer,
    folds: &[Fold],
    language: SourceLanguage,
    line: usize,
) -> Vec<Crumb> {
    enclosing(folds, line)
        .into_iter()
        .filter_map(|fold| {
            let text = buffer.line(fold.header);
            let label = symbol_name(text, language).or_else(|| {
                let head = text.trim().trim_end_matches(['{', '(', '[', ':']).trim();
                let head = head.split(" {").next().unwrap_or(head).trim();
                let named = head.starts_with("impl")
                    || head.starts_with("class ")
                    || head.starts_with("extension ")
                    || head.starts_with("namespace ")
                    || head.starts_with("describe(")
                    || head.starts_with('#');
                (named && !head.is_empty()).then(|| head.chars().take(48).collect())
            })?;
            Some(Crumb {
                label,
                line: fold.header,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_editor::syntax::{analyze, heading_folds};

    #[test]
    fn word_counts_skip_numbers_and_single_letters() {
        let counts = word_counts("let total = total + x1 + 42 + a;");
        assert_eq!(counts.get("total"), Some(&2));
        assert_eq!(counts.get("x1"), Some(&1));
        assert!(!counts.contains_key("42") && !counts.contains_key("a"));
    }

    #[test]
    fn ranking_prefers_exact_case_prefixes_then_fuzzy_and_never_echoes() {
        let word = |label: &str| Candidate {
            label: label.into(),
            detail: None,
            kind: CandidateKind::Word,
        };
        let symbol = |label: &str| Candidate {
            label: label.into(),
            detail: Some("fn".into()),
            kind: CandidateKind::Symbol,
        };
        let ranked = rank(
            "ren",
            [
                word("ren"),
                word("render_row"),
                symbol("Renderer"),
                word("current"),
                word("rename"),
                symbol("render_row"),
            ],
            10,
        );
        let labels: Vec<_> = ranked
            .iter()
            .map(|candidate| candidate.label.as_str())
            .collect();
        assert_eq!(labels, ["rename", "render_row", "Renderer", "current"]);
        assert_eq!(
            ranked[1].kind,
            CandidateKind::Word,
            "the buffer's own word wins a tie"
        );
        assert!(rank("", [word("x")], 5).is_empty());
        assert_eq!(rank("r", [word("ab"), word("rb"), word("rc")], 1).len(), 1);
    }

    #[test]
    fn call_context_finds_the_callee_and_argument() {
        assert_eq!(
            call_context("let x = render(a, b"),
            Some(("render".into(), 1))
        );
        assert_eq!(call_context("render("), Some(("render".into(), 0)));
        assert_eq!(
            call_context("outer(inner(a, b), c"),
            Some(("outer".into(), 1))
        );
        assert_eq!(
            call_context("println!(\"a, b\", c"),
            Some(("println".into(), 1))
        );
        assert_eq!(call_context("f(g(x)"), Some(("f".into(), 0)));
        assert_eq!(call_context("vec![a, b"), None);
        assert_eq!(call_context("done(); next"), None);
        assert_eq!(call_context("(a, b"), None);
    }

    #[test]
    fn parameters_split_at_top_level_commas() {
        let line = "pub fn open(&mut self, path: HashMap<K, V>, cx: &mut Context<Self>) -> bool {";
        let params = parameters(line).unwrap();
        let texts: Vec<_> = params.iter().map(|range| &line[range.clone()]).collect();
        assert_eq!(
            texts,
            ["&mut self", "path: HashMap<K, V>", "cx: &mut Context<Self>"]
        );
        assert_eq!(parameters("fn none() {}"), Some(vec![]));
        assert_eq!(parameters("struct X;"), None);
        let open = parameters("def f(a, b").unwrap();
        assert_eq!(open.len(), 2, "a cut-off list still splits");
    }

    #[test]
    fn lenses_mark_declarations_with_their_uses() {
        let buffer = Buffer::new(
            "pub fn render() {}\nfn main() { render(); render(); }\nlet x = 1;\nstruct Row;",
        );
        let counts = word_counts(buffer.text());
        let found = lenses(&buffer, SourceLanguage::Rust, &counts);
        assert_eq!(
            found,
            vec![Lens {
                line: 0,
                name: "render".into(),
                uses: 2
            }],
            "declarations used nowhere else in the file get no lens"
        );
    }

    #[test]
    fn breadcrumbs_name_the_blocks_around_a_line() {
        let buffer = Buffer::new(
            "impl Editor {\n    fn render(&self) {\n        if x {\n            y();\n        }\n    }\n}",
        );
        let analysis = analyze(&buffer, SourceLanguage::Rust, 4);
        let crumbs = breadcrumb(&buffer, &analysis.folds, SourceLanguage::Rust, 3);
        let labels: Vec<_> = crumbs.iter().map(|crumb| crumb.label.as_str()).collect();
        assert_eq!(
            labels,
            ["impl Editor", "render"],
            "unnamed blocks are skipped"
        );
        assert_eq!(crumbs[1].line, 1);

        let markdown = Buffer::new("# Guide\n## Install\ntext");
        let crumbs = breadcrumb(
            &markdown,
            &heading_folds(&markdown),
            SourceLanguage::Markdown,
            2,
        );
        assert_eq!(crumbs.len(), 2);
    }
}
