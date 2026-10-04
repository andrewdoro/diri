// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//
//! Pretty JSON for the response viewer, with folding.
//!
//! Ely's `reformat` round-trips through `serde_json::Value`, which sorts keys
//! and rewrites numbers. A response viewer must show what the server sent, so
//! this re-indents the text itself: keys keep their order, numbers and
//! escapes keep their spelling. Each line carries its colour spans and, for a
//! line that opens a non-empty object or array, the line that closes it, which
//! is all folding needs.

use std::collections::HashSet;
use std::ops::Range;

/// Bodies past this size are shown raw: indenting them is not worth the
/// memory, and nobody reads 8 MiB of JSON by eye.
pub const PRETTY_LIMIT: usize = 4 * 1024 * 1024;
const INDENT: &str = "  ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Key,
    String,
    Number,
    Literal,
    Punctuation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonLine {
    pub text: String,
    pub depth: usize,
    pub spans: Vec<(Range<usize>, Token)>,
    /// For a line that opens a non-empty object or array: the closing line.
    pub fold_end: Option<usize>,
}

/// A JSON document laid out one value per line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrettyJson {
    pub lines: Vec<JsonLine>,
}

impl PrettyJson {
    /// `None` when `text` is not JSON (or is too large to indent).
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > PRETTY_LIMIT || text.trim().is_empty() {
            return None;
        }
        serde_json::from_str::<serde::de::IgnoredAny>(text).ok()?;
        Some(Self {
            lines: layout(text),
        })
    }

    pub fn text(&self) -> String {
        let mut out = String::new();
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            out.push_str(&line.text);
        }
        out
    }

    /// The line indices to draw with `folded` (opening lines) collapsed: a
    /// folded line hides everything up to and including its closing line.
    pub fn visible(&self, folded: &HashSet<usize>) -> Vec<usize> {
        let mut rows = Vec::with_capacity(self.lines.len());
        let mut index = 0;
        while index < self.lines.len() {
            rows.push(index);
            match self.lines[index].fold_end {
                Some(end) if folded.contains(&index) => index = end + 1,
                _ => index += 1,
            }
        }
        rows
    }

    /// The closing bracket a folded line shows after its `…`, with the comma
    /// that followed it.
    pub fn folded_tail(&self, index: usize) -> Option<String> {
        let end = self.lines.get(index)?.fold_end?;
        Some(self.lines[end].text.trim_start().to_owned())
    }

    /// Every opening line whose depth is at least `depth`: what "collapse
    /// to level" folds.
    pub fn folds_at_depth(&self, depth: usize) -> HashSet<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.fold_end.is_some() && line.depth >= depth)
            .map(|(index, _)| index)
            .collect()
    }
}

/// Indents already-validated JSON. Strings are copied byte for byte.
fn layout(text: &str) -> Vec<JsonLine> {
    let mut lines: Vec<JsonLine> = Vec::new();
    let mut line = JsonLine {
        text: String::new(),
        depth: 0,
        spans: Vec::new(),
        fold_end: None,
    };
    let mut depth = 0usize;
    let mut opened: Vec<usize> = Vec::new();
    // After `:` the next string is a value; otherwise, inside an object, a
    // string that starts a member is its key.
    let mut containers: Vec<u8> = Vec::new();
    let mut expect_key = false;
    let bytes = text.as_bytes();
    let mut index = 0;

    let push = |line: &mut JsonLine, piece: &str, token: Token| {
        let start = line.text.len();
        line.text.push_str(piece);
        line.spans.push((start..line.text.len(), token));
    };
    let next_line = |lines: &mut Vec<JsonLine>, line: &mut JsonLine, depth: usize| {
        let done = std::mem::replace(
            line,
            JsonLine {
                text: INDENT.repeat(depth),
                depth,
                spans: Vec::new(),
                fold_end: None,
            },
        );
        lines.push(done);
    };

    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b' ' | b'\t' | b'\n' | b'\r' => {
                index += 1;
            }
            b'{' | b'[' => {
                let close = if byte == b'{' { b'}' } else { b']' };
                let mut peek = index + 1;
                while peek < bytes.len() && bytes[peek].is_ascii_whitespace() {
                    peek += 1;
                }
                if bytes.get(peek) == Some(&close) {
                    push(
                        &mut line,
                        if byte == b'{' { "{}" } else { "[]" },
                        Token::Punctuation,
                    );
                    index = peek + 1;
                    expect_key = false;
                    continue;
                }
                push(&mut line, &(byte as char).to_string(), Token::Punctuation);
                opened.push(lines.len());
                containers.push(byte);
                depth += 1;
                next_line(&mut lines, &mut line, depth);
                expect_key = byte == b'{';
                index += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                containers.pop();
                if line.text.trim().is_empty() {
                    line.text = INDENT.repeat(depth);
                    line.depth = depth;
                } else {
                    next_line(&mut lines, &mut line, depth);
                }
                push(&mut line, &(byte as char).to_string(), Token::Punctuation);
                if let Some(open) = opened.pop() {
                    let close_line = lines.len();
                    if let Some(opening) = lines.get_mut(open) {
                        opening.fold_end = Some(close_line);
                    }
                }
                expect_key = false;
                index += 1;
            }
            b',' => {
                push(&mut line, ",", Token::Punctuation);
                next_line(&mut lines, &mut line, depth);
                expect_key = containers.last() == Some(&b'{');
                index += 1;
            }
            b':' => {
                push(&mut line, ": ", Token::Punctuation);
                expect_key = false;
                index += 1;
            }
            b'"' => {
                let start = index;
                index += 1;
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' => index += 2,
                        b'"' => {
                            index += 1;
                            break;
                        }
                        _ => index += 1,
                    }
                }
                let end = index.min(bytes.len());
                let token = if expect_key {
                    Token::Key
                } else {
                    Token::String
                };
                push(&mut line, &text[start..end], token);
            }
            _ => {
                let start = index;
                while index < bytes.len()
                    && !matches!(
                        bytes[index],
                        b',' | b'}' | b']' | b':' | b' ' | b'\t' | b'\n' | b'\r'
                    )
                {
                    index += 1;
                }
                let word = &text[start..index];
                let token = if matches!(word, "true" | "false" | "null") {
                    Token::Literal
                } else {
                    Token::Number
                };
                push(&mut line, word, token);
            }
        }
    }
    if !line.text.trim().is_empty() {
        lines.push(line);
    }
    lines
}

/// Ely's `reformat` for the request body editor's Format button: indents
/// valid JSON, keeping key order.
pub fn format_body(text: &str) -> Option<String> {
    PrettyJson::parse(text).map(|pretty| pretty.text())
}
