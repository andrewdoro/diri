// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components

//! Find and replace over the editor's text: plain or regular-expression
//! queries, case and whole-word options, and replacements that expand
//! `$1`-style captures.

use std::ops::Range;

use regex::{Regex, RegexBuilder};

/// How a search matches: case exactly, whole words only, the query as a regular expression.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FindOptions {
    pub case: bool,
    pub word: bool,
    pub regex: bool,
}

/// Matches past this count stop the search, so a one-letter query on a
/// large file never builds an unbounded list.
pub const MATCH_LIMIT: usize = 20_000;

/// The query compiled under `options`; a pattern that does not compile is an
/// error to show. Plain queries are escaped and smart-cased off.
pub fn compile(query: &str, options: FindOptions) -> Result<Regex, String> {
    let pattern = if options.regex {
        query.to_string()
    } else {
        regex::escape(query)
    };
    let pattern = if options.word {
        format!(r"\b(?:{pattern})\b")
    } else {
        pattern
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(!options.case)
        .multi_line(true)
        .size_limit(4 * 1024 * 1024)
        .build()
        .map_err(|error| {
            // The regex crate's message is multi-line; the first line names the problem.
            error
                .to_string()
                .lines()
                .rev()
                .find(|line| line.starts_with("error:"))
                .map_or_else(
                    || "Invalid pattern".to_owned(),
                    |line| line.trim_start_matches("error: ").to_owned(),
                )
        })
}

/// Where `query` appears in `text` under `options`, at most [`MATCH_LIMIT`].
pub fn find_all(
    text: &str,
    query: &str,
    options: FindOptions,
) -> Result<Vec<Range<usize>>, String> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let pattern = compile(query, options)?;
    Ok(pattern
        .find_iter(text)
        .filter(|hit| !hit.is_empty())
        .take(MATCH_LIMIT)
        .map(|hit| hit.range())
        .collect())
}

/// The text one match is replaced with: captures expand under a regular
/// expression, plain replacements are taken literally.
pub fn replacement_for(
    text: &str,
    range: Range<usize>,
    query: &str,
    replacement: &str,
    options: FindOptions,
) -> Result<String, String> {
    if !options.regex {
        return Ok(replacement.to_owned());
    }
    let pattern = compile(query, options)?;
    // Match from the start of the range so look-behind sees its context.
    let captures = pattern
        .captures_at(text, range.start)
        .filter(|captures| captures.get(0).is_some_and(|whole| whole.range() == range))
        .ok_or_else(|| "The match moved; search again".to_owned())?;
    let mut expanded = String::new();
    captures.expand(&unescape(replacement), &mut expanded);
    Ok(expanded)
}

/// Every match replaced, as edits. Matches are found once against the
/// current text so replacements never rematch their own output.
pub fn replace_all(
    text: &str,
    query: &str,
    replacement: &str,
    options: FindOptions,
) -> Result<Vec<(Range<usize>, String)>, String> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let pattern = compile(query, options)?;
    let template = unescape(replacement);
    let mut edits = Vec::new();
    for captures in pattern.captures_iter(text) {
        let whole = captures.get(0).expect("a match has its whole");
        if whole.is_empty() {
            continue;
        }
        let text = if options.regex {
            let mut expanded = String::new();
            captures.expand(&template, &mut expanded);
            expanded
        } else {
            replacement.to_owned()
        };
        edits.push((whole.range(), text));
    }
    Ok(edits)
}

/// `\n` and `\t` in a regular-expression replacement mean a line break and a tab.
fn unescape(replacement: &str) -> String {
    let mut out = String::with_capacity(replacement.len());
    let mut chars = replacement.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The match a step lands on from `caret`: the first starting at or after it
/// going forward, the last ending before it going back, wrapping around.
pub fn step_from(matches: &[Range<usize>], caret: usize, forward: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    Some(if forward {
        let next = matches.partition_point(|hit| hit.start < caret);
        if next == matches.len() { 0 } else { next }
    } else {
        let before = matches.partition_point(|hit| hit.end < caret);
        if before == 0 {
            matches.len() - 1
        } else {
            before - 1
        }
    })
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    #[test]
    fn finds_by_case_word_and_pattern() {
        let text = "Let total = total_cost + TOTAL;";
        let plain = FindOptions::default();
        assert_eq!(
            find_all(text, "total", plain).map(|found| found.len()),
            Ok(3)
        );
        let case = FindOptions {
            case: true,
            ..plain
        };
        assert_eq!(
            find_all(text, "total", case).map(|found| found.len()),
            Ok(2)
        );
        let word = FindOptions {
            word: true,
            ..plain
        };
        assert_eq!(find_all(text, "total", word), Ok(vec![4..9, 25..30]));
        let pattern = FindOptions {
            regex: true,
            ..plain
        };
        assert_eq!(find_all(text, r"total_\w+", pattern), Ok([12..22].to_vec()));
        assert!(
            find_all(text, "(", pattern).is_err(),
            "a broken pattern says so"
        );
        assert_eq!(
            find_all(text, "(", plain),
            Ok(Vec::new()),
            "plain text is escaped"
        );
        assert_eq!(find_all(text, "", plain), Ok(Vec::new()));
        assert_eq!(
            find_all("a\nb", "^b", pattern),
            Ok([2..3].to_vec()),
            "lines anchor"
        );
        assert_eq!(
            find_all("aaa", "a*", pattern),
            Ok([0..3].to_vec()),
            "empty matches are skipped"
        );
    }

    #[test]
    fn replacements_expand_captures_only_for_patterns() {
        let pattern = FindOptions {
            regex: true,
            ..FindOptions::default()
        };
        let text = "fn alpha() {}\nfn beta() {}";
        let edits = replace_all(text, r"fn (\w+)", "func ${1}_x", pattern).unwrap();
        assert_eq!(
            edits,
            vec![
                (0..8, "func alpha_x".into()),
                (14..21, "func beta_x".into())
            ]
        );
        let plain = replace_all(text, "fn", "$1", FindOptions::default()).unwrap();
        assert_eq!(plain[0].1, "$1", "plain replacements are literal");
        assert_eq!(replace_all("a,b", ",", r"\n", pattern).unwrap()[0].1, "\n");
        assert_eq!(
            replacement_for(text, 14..21, r"fn (\w+)", "$1", pattern),
            Ok("beta".into())
        );
        assert!(replacement_for(text, 15..21, r"fn (\w+)", "$1", pattern).is_err());
        assert_eq!(
            replacement_for(text, 0..2, "fn", "$1", FindOptions::default()),
            Ok("$1".into())
        );
    }

    #[test]
    fn steps_wrap_around_from_the_caret() {
        let matches = [2..4, 10..12, 20..22];
        assert_eq!(step_from(&matches, 0, true), Some(0));
        assert_eq!(step_from(&matches, 5, true), Some(1));
        assert_eq!(step_from(&matches, 21, true), Some(0), "wraps forward");
        assert_eq!(step_from(&matches, 10, false), Some(0));
        assert_eq!(step_from(&matches, 1, false), Some(2), "wraps back");
        assert_eq!(step_from(&[], 1, false), None);
    }
}
