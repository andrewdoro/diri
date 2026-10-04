// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components

//! Source text and where each line starts, in bytes.

use std::ops::Range;

/// The editor's text. Line starts are kept in step with every edit so that
/// line lookups stay logarithmic, and `version` moves on every change so
/// derived data (tokens, folds, brackets) can be cached against it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buffer {
    text: String,
    starts: Vec<usize>,
    version: u64,
}

fn starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect()
}

impl Buffer {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let starts = starts(&text);
        Self {
            text,
            starts,
            version: 0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Moves on with every edit.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// How many lines it holds; an empty buffer holds one.
    pub fn lines(&self) -> usize {
        self.starts.len()
    }

    /// A line's bytes, without its line break (and without a `\r` before it).
    pub fn line_range(&self, line: usize) -> Range<usize> {
        let start = self.starts[line];
        let mut end = self
            .starts
            .get(line + 1)
            .map_or(self.text.len(), |next| next - 1);
        if end > start && self.text.as_bytes()[end - 1] == b'\r' {
            end -= 1;
        }
        start..end
    }

    pub fn line(&self, line: usize) -> &str {
        &self.text[self.line_range(line)]
    }

    /// The line holding `offset`.
    pub fn line_of(&self, offset: usize) -> usize {
        self.starts.partition_point(|start| *start <= offset) - 1
    }

    /// An offset's line, and its column in characters.
    pub fn point(&self, offset: usize) -> (usize, usize) {
        let line = self.line_of(offset);
        let end = offset.min(self.line_range(line).end).max(self.starts[line]);
        let column = self.text[self.starts[line]..end].chars().count();
        (line, column)
    }

    /// The offset at a line and a column in characters, held to the line's end.
    pub fn offset(&self, line: usize, column: usize) -> usize {
        let range = self.line_range(line.min(self.lines() - 1));
        self.text[range.clone()]
            .char_indices()
            .nth(column)
            .map_or(range.end, |(at, _)| range.start + at)
    }

    /// The character after `offset`, or the end. A `\r\n` pair is one step.
    pub fn next(&self, offset: usize) -> usize {
        if self.text[offset..].starts_with("\r\n") {
            return offset + 2;
        }
        self.text[offset..]
            .chars()
            .next()
            .map_or(offset, |ch| offset + ch.len_utf8())
    }

    /// The character before `offset`, or the start. A `\r\n` pair is one step.
    pub fn previous(&self, offset: usize) -> usize {
        if self.text[..offset].ends_with("\r\n") {
            return offset - 2;
        }
        self.text[..offset]
            .chars()
            .next_back()
            .map_or(offset, |ch| offset - ch.len_utf8())
    }

    /// How far a line is indented, in characters.
    pub fn indent(&self, line: usize) -> usize {
        self.line(line)
            .chars()
            .take_while(|ch| *ch == ' ' || *ch == '\t')
            .count()
    }

    /// The indent of a line in columns, a tab counting to the next stop.
    pub fn indent_columns(&self, line: usize, tab: usize) -> usize {
        let mut columns = 0;
        for ch in self.line(line).chars() {
            match ch {
                ' ' => columns += 1,
                '\t' => columns += tab - columns % tab,
                _ => break,
            }
        }
        columns
    }

    /// The line break this text uses most: `\r\n` when any line has one.
    pub fn line_break(&self) -> &'static str {
        if self.text.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        }
    }

    /// Replaces a range. Line starts are patched rather than rebuilt: starts
    /// inside the range go, starts after it shift, and the inserted text
    /// brings its own.
    pub(crate) fn replace(&mut self, range: Range<usize>, text: &str) {
        assert!(
            range.start <= range.end
                && range.end <= self.text.len()
                && self.text.is_char_boundary(range.start)
                && self.text.is_char_boundary(range.end),
            "an edit covers whole characters inside the text: {range:?} of {}",
            self.text.len()
        );
        self.text.replace_range(range.clone(), text);
        let delta = text.len() as isize - range.len() as isize;
        let first = self.starts.partition_point(|start| *start <= range.start);
        let last = self.starts.partition_point(|start| *start <= range.end);
        let inserted: Vec<usize> = text
            .match_indices('\n')
            .map(|(at, _)| range.start + at + 1)
            .collect();
        let tail: Vec<usize> = self.starts[last..]
            .iter()
            .map(|start| (*start as isize + delta) as usize)
            .collect();
        self.starts.truncate(first);
        self.starts.extend(inserted);
        self.starts.extend(tail);
        self.version = self.version.wrapping_add(1);
        debug_assert_eq!(self.starts, starts(&self.text));
    }
}

/// What kind of character a word motion sees.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Mark,
}

fn class(ch: char) -> Class {
    if ch.is_alphanumeric() || ch == '_' {
        Class::Word
    } else if ch.is_whitespace() {
        Class::Space
    } else {
        Class::Mark
    }
}

pub(crate) fn is_word_char(ch: char) -> bool {
    class(ch) == Class::Word
}

impl Buffer {
    /// The start of the word before `offset`, over any spaces first; a line start stops it.
    pub fn word_start(&self, offset: usize) -> usize {
        let line_start = self.starts[self.line_of(offset)];
        if offset == line_start {
            return self.previous(offset);
        }
        let mut chars = self.text[line_start..offset]
            .char_indices()
            .rev()
            .peekable();
        while chars
            .next_if(|(_, ch)| class(*ch) == Class::Space)
            .is_some()
        {}
        let Some(&(_, first)) = chars.peek() else {
            return line_start;
        };
        let kind = class(first);
        let mut at = offset;
        for (ix, ch) in chars {
            if class(ch) != kind {
                break;
            }
            at = line_start + ix;
        }
        at.min(offset)
    }

    /// The end of the word after `offset`, over any spaces first; a line end stops it.
    pub fn word_end(&self, offset: usize) -> usize {
        let line_end = self.line_range(self.line_of(offset)).end;
        if offset >= line_end {
            return self.next(offset);
        }
        let mut chars = self.text[offset..line_end].char_indices().peekable();
        while chars
            .next_if(|(_, ch)| class(*ch) == Class::Space)
            .is_some()
        {}
        let Some(&(_, first)) = chars.peek() else {
            return line_end;
        };
        let kind = class(first);
        let mut at = line_end;
        for (ix, ch) in chars {
            if class(ch) != kind {
                at = offset + ix;
                break;
            }
        }
        at
    }

    /// The word around `offset`, or the run of marks or spaces it sits in.
    pub fn word_at(&self, offset: usize) -> Range<usize> {
        let range = self.line_range(self.line_of(offset));
        let offset = offset.clamp(range.start, range.end);
        let line = &self.text[range.clone()];
        let at = offset - range.start;
        let kind = line[at..]
            .chars()
            .next()
            .or_else(|| line[..at].chars().next_back())
            .map(class);
        let Some(kind) = kind else {
            return offset..offset;
        };
        let start = line[..at]
            .char_indices()
            .rev()
            .take_while(|(_, ch)| class(*ch) == kind)
            .last()
            .map_or(at, |(ix, _)| ix);
        let end = line[at..]
            .char_indices()
            .find(|(_, ch)| class(*ch) != kind)
            .map_or(line.len(), |(ix, _)| at + ix);
        range.start + start..range.start + end
    }

    /// The identifier touching `offset`, if any: the word under it, or the
    /// one that ends right before it.
    pub fn identifier_at(&self, offset: usize) -> Option<Range<usize>> {
        let word = |at: usize| {
            let word = self.word_at(at);
            let text = &self.text[word.clone()];
            (!word.is_empty() && text.chars().all(is_word_char)).then_some(word)
        };
        let line_start = self.starts[self.line_of(offset)];
        word(offset).or_else(|| {
            (offset > line_start)
                .then(|| word(self.previous(offset)))
                .flatten()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_at_breaks_and_points_count_characters() {
        let buffer = Buffer::new("fn main() {\n    let π = 3.14;\n}");
        assert_eq!(buffer.lines(), 3);
        assert_eq!(buffer.line(1), "    let π = 3.14;");
        let pi = buffer.text().find('π').expect("pi");
        assert_eq!(buffer.point(pi), (1, 8));
        assert_eq!(buffer.point(pi + 'π'.len_utf8()), (1, 9));
        assert_eq!(buffer.offset(1, 9), pi + 'π'.len_utf8());
        assert_eq!(buffer.offset(0, 99), 11, "held to the line's end");
        assert_eq!(Buffer::new("").lines(), 1);
        assert_eq!(buffer.indent(1), 4);
        assert_eq!(Buffer::new("\t  x").indent_columns(0, 4), 6);
    }

    #[test]
    fn crlf_lines_hide_their_carriage_return() {
        let buffer = Buffer::new("one\r\ntwo\r\n");
        assert_eq!(buffer.lines(), 3);
        assert_eq!(buffer.line(0), "one");
        assert_eq!(buffer.line(1), "two");
        assert_eq!(buffer.next(3), 5, "a CRLF pair is one step");
        assert_eq!(buffer.previous(5), 3);
        assert_eq!(buffer.line_break(), "\r\n");
    }

    #[test]
    fn edits_keep_line_starts_in_step_and_bump_the_version() {
        let mut buffer = Buffer::new("a\nb");
        let before = buffer.version();
        buffer.replace(1..1, "\nc");
        assert_eq!(buffer.text(), "a\nc\nb");
        assert_eq!(buffer.lines(), 3);
        assert_eq!(buffer.line(2), "b");
        assert_ne!(buffer.version(), before);
        buffer.replace(0..3, "x");
        assert_eq!(buffer.text(), "x\nb");
        assert_eq!(buffer.lines(), 2);
        buffer.replace(1..2, "");
        assert_eq!((buffer.text(), buffer.lines()), ("xb", 1));
        buffer.replace(2..2, "\n\n");
        assert_eq!(buffer.lines(), 3);
    }

    #[test]
    fn words_stop_at_their_class_and_at_line_edges() {
        let buffer = Buffer::new("let total = price * 2;\nnext");
        assert_eq!(buffer.word_start(9), 4);
        assert_eq!(buffer.word_start(4), 0);
        assert_eq!(buffer.word_end(4), 9);
        assert_eq!(buffer.word_end(9), 11);
        assert_eq!(buffer.word_end(22), 23, "a line end steps to the next line");
        assert_eq!(buffer.word_at(6), 4..9);
        assert_eq!(buffer.word_at(10), 10..11, "a mark is its own run");
        assert_eq!(buffer.identifier_at(9), Some(4..9), "touching the end");
        assert_eq!(buffer.identifier_at(10), None);
    }
}
