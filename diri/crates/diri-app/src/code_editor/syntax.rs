// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components

//! Lexical highlighting, bracket pairing, and fold ranges.
//!
//! The lexer is deliberately small: one pass per line carrying a state
//! across lines (block comments, multi-line strings, fenced code), so a
//! whole file is analysed in one linear walk and no grammar crate or
//! parser has to ship with the app. Everything here is pure and tested.

use std::ops::Range;

use crate::code_intelligence::SourceLanguage;

use super::buffer::Buffer;

/// What a span of code is, for color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Comment,
    String,
    Number,
    Keyword,
    Type,
    Function,
    Constant,
    Attribute,
    Property,
    Tag,
    Bracket,
    Punctuation,
}

/// A colored span, in bytes of its line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub range: Range<usize>,
    pub kind: TokenKind,
}

/// What the lexer carries from one line into the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LexState {
    #[default]
    Normal,
    /// Inside a block comment, nested `depth` deep.
    Comment { depth: u8 },
    /// Inside a string that may span lines.
    Str {
        quote: u8,
        hashes: u8,
        triple: bool,
        raw: bool,
    },
}

/// How one family of languages spells comments, strings, and words.
#[derive(Debug)]
pub struct Grammar {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    nested_comments: bool,
    keywords: &'static [&'static str],
    constants: &'static [&'static str],
    quotes: &'static [u8],
    /// Quotes whose strings run on past the end of a line.
    multiline_quotes: &'static [u8],
    triple_quotes: bool,
    rust: bool,
    at_decorators: bool,
    dollar_identifiers: bool,
    case_insensitive: bool,
    markup: bool,
    /// `key:` / `key =` at a line start is a property (YAML, TOML, INI).
    keyed: bool,
    /// A quoted string before `:` is an object key (JSON).
    quoted_keys: bool,
    /// Blocks are delimited by brackets; otherwise folds follow indentation.
    pub braces: bool,
    markdown: bool,
}

const NONE: &[&str] = &[];

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "type", "unsafe", "use",
    "where", "while", "yield",
];
const RUST_CONSTANTS: &[&str] = &["true", "false", "None", "Some", "Ok", "Err"];
const SWIFT_KEYWORDS: &[&str] = &[
    "actor",
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "continue",
    "default",
    "defer",
    "do",
    "else",
    "enum",
    "extension",
    "fileprivate",
    "for",
    "func",
    "guard",
    "if",
    "import",
    "in",
    "init",
    "internal",
    "is",
    "let",
    "mutating",
    "private",
    "protocol",
    "public",
    "return",
    "self",
    "Self",
    "some",
    "static",
    "struct",
    "switch",
    "throw",
    "throws",
    "try",
    "typealias",
    "var",
    "where",
    "while",
];
const SWIFT_CONSTANTS: &[&str] = &["true", "false", "nil"];
const JS_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "keyof",
    "let",
    "new",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "satisfies",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "type",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];
const JS_CONSTANTS: &[&str] = &["true", "false", "null", "undefined", "NaN", "Infinity"];
const PYTHON_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "match", "case", "nonlocal", "not", "or", "pass", "raise", "return", "self", "try", "while",
    "with", "yield",
];
const PYTHON_CONSTANTS: &[&str] = &["True", "False", "None"];
const GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
];
const GO_CONSTANTS: &[&str] = &["true", "false", "nil", "iota"];
const C_FAMILY_KEYWORDS: &[&str] = &[
    "abstract",
    "auto",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "constexpr",
    "continue",
    "data",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "extends",
    "extern",
    "final",
    "for",
    "fun",
    "goto",
    "if",
    "implements",
    "import",
    "include",
    "inline",
    "interface",
    "internal",
    "namespace",
    "new",
    "object",
    "operator",
    "override",
    "package",
    "private",
    "protected",
    "public",
    "record",
    "return",
    "sealed",
    "sizeof",
    "static",
    "struct",
    "super",
    "switch",
    "template",
    "this",
    "throw",
    "throws",
    "try",
    "typedef",
    "typename",
    "union",
    "using",
    "val",
    "var",
    "virtual",
    "void",
    "volatile",
    "when",
    "while",
];
const C_FAMILY_CONSTANTS: &[&str] = &["true", "false", "null", "nullptr", "NULL"];
const RUBY_KEYWORDS: &[&str] = &[
    "alias", "and", "begin", "break", "case", "class", "def", "do", "else", "elsif", "end",
    "ensure", "for", "if", "in", "module", "next", "not", "or", "redo", "rescue", "retry",
    "return", "self", "super", "then", "unless", "until", "when", "while", "yield",
];
const RUBY_CONSTANTS: &[&str] = &["true", "false", "nil"];
const SHELL_KEYWORDS: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in",
    "local", "readonly", "return", "set", "then", "until", "while", "end", "begin", "and", "or",
    "not", "switch",
];
const SQL_KEYWORDS: &[&str] = &[
    "add",
    "all",
    "alter",
    "and",
    "as",
    "asc",
    "begin",
    "between",
    "by",
    "case",
    "commit",
    "create",
    "default",
    "delete",
    "desc",
    "distinct",
    "drop",
    "else",
    "end",
    "exists",
    "foreign",
    "from",
    "group",
    "having",
    "if",
    "in",
    "index",
    "inner",
    "insert",
    "into",
    "is",
    "join",
    "key",
    "left",
    "like",
    "limit",
    "not",
    "on",
    "or",
    "order",
    "outer",
    "primary",
    "references",
    "returning",
    "right",
    "rollback",
    "select",
    "set",
    "table",
    "then",
    "transaction",
    "union",
    "unique",
    "update",
    "values",
    "view",
    "when",
    "where",
    "with",
];
const SQL_CONSTANTS: &[&str] = &["null", "true", "false"];
const DATA_CONSTANTS: &[&str] = &["true", "false", "null", "yes", "no", "on", "off", "~"];
const CSS_KEYWORDS: &[&str] = &["important", "media", "import", "keyframes", "supports"];

const fn grammar_base() -> Grammar {
    Grammar {
        line_comments: &["//"],
        block_comment: Some(("/*", "*/")),
        nested_comments: false,
        keywords: NONE,
        constants: NONE,
        quotes: b"\"'",
        multiline_quotes: b"",
        triple_quotes: false,
        rust: false,
        at_decorators: false,
        dollar_identifiers: false,
        case_insensitive: false,
        markup: false,
        keyed: false,
        quoted_keys: false,
        braces: true,
        markdown: false,
    }
}

static RUST: Grammar = Grammar {
    keywords: RUST_KEYWORDS,
    constants: RUST_CONSTANTS,
    quotes: b"\"'",
    multiline_quotes: b"\"",
    nested_comments: true,
    rust: true,
    ..grammar_base()
};
static SWIFT: Grammar = Grammar {
    keywords: SWIFT_KEYWORDS,
    constants: SWIFT_CONSTANTS,
    quotes: b"\"",
    triple_quotes: true,
    nested_comments: true,
    at_decorators: true,
    ..grammar_base()
};
static JS: Grammar = Grammar {
    keywords: JS_KEYWORDS,
    constants: JS_CONSTANTS,
    quotes: b"\"'`",
    multiline_quotes: b"`",
    at_decorators: true,
    dollar_identifiers: true,
    ..grammar_base()
};
static PYTHON: Grammar = Grammar {
    line_comments: &["#"],
    block_comment: None,
    keywords: PYTHON_KEYWORDS,
    constants: PYTHON_CONSTANTS,
    triple_quotes: true,
    at_decorators: true,
    braces: false,
    ..grammar_base()
};
static GO: Grammar = Grammar {
    keywords: GO_KEYWORDS,
    constants: GO_CONSTANTS,
    quotes: b"\"'`",
    multiline_quotes: b"`",
    ..grammar_base()
};
static C_FAMILY: Grammar = Grammar {
    keywords: C_FAMILY_KEYWORDS,
    constants: C_FAMILY_CONSTANTS,
    at_decorators: true,
    triple_quotes: true,
    ..grammar_base()
};
static RUBY: Grammar = Grammar {
    line_comments: &["#"],
    block_comment: None,
    keywords: RUBY_KEYWORDS,
    constants: RUBY_CONSTANTS,
    braces: false,
    ..grammar_base()
};
static SHELL: Grammar = Grammar {
    line_comments: &["#"],
    block_comment: None,
    keywords: SHELL_KEYWORDS,
    dollar_identifiers: true,
    braces: false,
    ..grammar_base()
};
static SQL: Grammar = Grammar {
    line_comments: &["--"],
    keywords: SQL_KEYWORDS,
    constants: SQL_CONSTANTS,
    case_insensitive: true,
    braces: false,
    ..grammar_base()
};
static JSON: Grammar = Grammar {
    constants: DATA_CONSTANTS,
    quotes: b"\"",
    quoted_keys: true,
    ..grammar_base()
};
static TOML: Grammar = Grammar {
    line_comments: &["#"],
    block_comment: None,
    constants: DATA_CONSTANTS,
    triple_quotes: true,
    keyed: true,
    braces: false,
    ..grammar_base()
};
static YAML: Grammar = Grammar {
    line_comments: &["#"],
    block_comment: None,
    constants: DATA_CONSTANTS,
    keyed: true,
    braces: false,
    ..grammar_base()
};
static CSS: Grammar = Grammar {
    line_comments: NONE,
    keywords: CSS_KEYWORDS,
    ..grammar_base()
};
static HTML: Grammar = Grammar {
    line_comments: NONE,
    block_comment: Some(("<!--", "-->")),
    markup: true,
    braces: false,
    ..grammar_base()
};
static MARKDOWN: Grammar = Grammar {
    line_comments: NONE,
    block_comment: Some(("<!--", "-->")),
    quotes: b"",
    braces: false,
    markdown: true,
    ..grammar_base()
};
static PLAIN: Grammar = Grammar {
    line_comments: &["#", "//"],
    block_comment: None,
    braces: false,
    ..grammar_base()
};

pub fn grammar(language: SourceLanguage) -> &'static Grammar {
    match language {
        SourceLanguage::Rust => &RUST,
        SourceLanguage::Swift => &SWIFT,
        SourceLanguage::TypeScript
        | SourceLanguage::Tsx
        | SourceLanguage::JavaScript
        | SourceLanguage::Jsx => &JS,
        SourceLanguage::Python => &PYTHON,
        SourceLanguage::Go => &GO,
        SourceLanguage::Java
        | SourceLanguage::Kotlin
        | SourceLanguage::C
        | SourceLanguage::Cpp
        | SourceLanguage::CSharp => &C_FAMILY,
        SourceLanguage::Ruby => &RUBY,
        SourceLanguage::Shell => &SHELL,
        SourceLanguage::Sql => &SQL,
        SourceLanguage::Json => &JSON,
        SourceLanguage::Toml => &TOML,
        SourceLanguage::Yaml => &YAML,
        SourceLanguage::Css => &CSS,
        SourceLanguage::Html => &HTML,
        SourceLanguage::Markdown => &MARKDOWN,
        SourceLanguage::PlainText => &PLAIN,
    }
}

/// The line comment prefix a language toggles with ⌘/, if it has one.
pub fn comment_prefix(language: SourceLanguage) -> Option<&'static str> {
    grammar(language).line_comments.first().copied()
}

fn is_ident_start(byte: u8, grammar: &Grammar) -> bool {
    byte.is_ascii_alphabetic()
        || byte == b'_'
        || byte >= 0x80
        || (grammar.dollar_identifiers && byte == b'$')
}

fn is_ident(byte: u8, grammar: &Grammar) -> bool {
    byte.is_ascii_alphanumeric()
        || byte == b'_'
        || byte >= 0x80
        || (grammar.dollar_identifiers && byte == b'$')
}

fn push(tokens: &mut Vec<Token>, range: Range<usize>, kind: TokenKind) {
    if range.is_empty() {
        return;
    }
    // Neighbouring punctuation reads as one operator.
    if kind == TokenKind::Punctuation
        && let Some(last) = tokens.last_mut()
        && last.kind == TokenKind::Punctuation
        && last.range.end == range.start
    {
        last.range.end = range.end;
        return;
    }
    tokens.push(Token { range, kind });
}

/// Where a string opened by `quote` ends on `line` from `from`, honouring
/// escapes (none in raw strings); `None` when it runs past the line.
fn string_end(
    line: &[u8],
    from: usize,
    quote: u8,
    hashes: u8,
    triple: bool,
    raw: bool,
) -> Option<usize> {
    let mut at = from;
    while at < line.len() {
        let byte = line[at];
        if byte == b'\\' && !raw {
            at += 2;
            continue;
        }
        if byte == quote {
            if triple {
                if line.len() >= at + 3 && line[at + 1] == quote && line[at + 2] == quote {
                    return Some(at + 3);
                }
            } else {
                let closing = &line[at + 1..];
                let hashes = hashes as usize;
                if closing.len() >= hashes && closing[..hashes].iter().all(|byte| *byte == b'#') {
                    return Some(at + 1 + hashes);
                }
            }
        }
        at += 1;
    }
    None
}

/// Where a block comment that is `depth` deep closes on `line` from `from`,
/// and the depth it closes back to; `Err(depth)` when it runs past the line.
fn comment_end(line: &str, from: usize, depth: u8, grammar: &Grammar) -> Result<usize, u8> {
    let (open, close) = grammar
        .block_comment
        .expect("a block comment has delimiters");
    let mut depth = depth;
    let mut at = from;
    while at < line.len() {
        let rest = &line[at..];
        if rest.starts_with(close) {
            at += close.len();
            depth -= 1;
            if depth == 0 {
                return Ok(at);
            }
        } else if grammar.nested_comments && rest.starts_with(open) {
            at += open.len();
            depth = depth.saturating_add(1);
        } else {
            at += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    Err(depth)
}

/// One line's tokens, from the state the previous line left, and the state
/// this one leaves for the next.
pub fn lex_line(line: &str, state: LexState, grammar: &Grammar) -> (Vec<Token>, LexState) {
    if grammar.markdown {
        return lex_markdown(line, state);
    }
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0;
    match state {
        LexState::Normal => {}
        LexState::Comment { depth } => match comment_end(line, 0, depth, grammar) {
            Ok(end) => {
                push(&mut tokens, 0..end, TokenKind::Comment);
                at = end;
            }
            Err(depth) => {
                push(&mut tokens, 0..line.len(), TokenKind::Comment);
                return (tokens, LexState::Comment { depth });
            }
        },
        LexState::Str {
            quote,
            hashes,
            triple,
            raw,
        } => match string_end(bytes, 0, quote, hashes, triple, raw) {
            Some(end) => {
                push(&mut tokens, 0..end, TokenKind::String);
                at = end;
            }
            None => {
                push(&mut tokens, 0..line.len(), TokenKind::String);
                return (tokens, state);
            }
        },
    }
    let line_start_key = grammar.keyed.then(|| keyed_property(line)).flatten();
    while at < bytes.len() {
        let byte = bytes[at];
        let rest = &line[at..];
        if byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if let Some(key) = &line_start_key
            && key.start == at
        {
            push(&mut tokens, key.clone(), TokenKind::Property);
            at = key.end;
            continue;
        }
        if grammar
            .line_comments
            .iter()
            .any(|prefix| rest.starts_with(prefix))
            // `#` only opens a comment at a word boundary in shells (`$#`, `a#b`).
            && !(byte == b'#' && at > 0 && !bytes[at - 1].is_ascii_whitespace() && grammar.dollar_identifiers)
        {
            push(&mut tokens, at..line.len(), TokenKind::Comment);
            return (tokens, LexState::Normal);
        }
        if let Some((open, _)) = grammar.block_comment
            && rest.starts_with(open)
        {
            match comment_end(line, at + open.len(), 1, grammar) {
                Ok(end) => {
                    push(&mut tokens, at..end, TokenKind::Comment);
                    at = end;
                    continue;
                }
                Err(depth) => {
                    push(&mut tokens, at..line.len(), TokenKind::Comment);
                    return (tokens, LexState::Comment { depth });
                }
            }
        }
        // Rust raw strings: r"…", r#"…"#, br"…".
        if grammar.rust && (byte == b'r' || (byte == b'b' && bytes.get(at + 1) == Some(&b'r'))) {
            let prefix = if byte == b'b' { 2 } else { 1 };
            let hashes = bytes[at + prefix..]
                .iter()
                .take_while(|byte| **byte == b'#')
                .count();
            if bytes.get(at + prefix + hashes) == Some(&b'"')
                && (at == 0 || !is_ident(bytes[at - 1], grammar))
            {
                let hashes = hashes.min(u8::MAX as usize) as u8;
                let from = at + prefix + hashes as usize + 1;
                match string_end(bytes, from, b'"', hashes, false, true) {
                    Some(end) => {
                        push(&mut tokens, at..end, TokenKind::String);
                        at = end;
                        continue;
                    }
                    None => {
                        push(&mut tokens, at..line.len(), TokenKind::String);
                        return (
                            tokens,
                            LexState::Str {
                                quote: b'"',
                                hashes,
                                triple: false,
                                raw: true,
                            },
                        );
                    }
                }
            }
        }
        if grammar.quotes.contains(&byte) {
            // Rust lifetimes and labels: 'a, 'static, 'outer: — not chars.
            if grammar.rust && byte == b'\'' {
                let is_char = match bytes.get(at + 1) {
                    Some(b'\\') => true,
                    Some(_) => {
                        let next = rest[1..].chars().next().map_or(1, char::len_utf8);
                        bytes.get(at + 1 + next) == Some(&b'\'')
                    }
                    None => false,
                };
                if !is_char {
                    let end = at
                        + 1
                        + bytes[at + 1..]
                            .iter()
                            .take_while(|byte| is_ident(**byte, grammar))
                            .count();
                    push(&mut tokens, at..end, TokenKind::Keyword);
                    at = end;
                    continue;
                }
            }
            let triple = grammar.triple_quotes
                && bytes.get(at + 1) == Some(&byte)
                && bytes.get(at + 2) == Some(&byte);
            let from = if triple { at + 3 } else { at + 1 };
            match string_end(bytes, from, byte, 0, triple, false) {
                Some(end) => {
                    let kind = if grammar.quoted_keys && followed_by_colon(bytes, end) {
                        TokenKind::Property
                    } else {
                        TokenKind::String
                    };
                    push(&mut tokens, at..end, kind);
                    at = end;
                    continue;
                }
                None => {
                    push(&mut tokens, at..line.len(), TokenKind::String);
                    let carries = triple || grammar.multiline_quotes.contains(&byte);
                    return (
                        tokens,
                        if carries {
                            LexState::Str {
                                quote: byte,
                                hashes: 0,
                                triple,
                                raw: false,
                            }
                        } else {
                            LexState::Normal
                        },
                    );
                }
            }
        }
        if byte.is_ascii_digit() {
            let mut end = at;
            loop {
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    // An exponent sign: 1e-5.
                    if matches!(bytes[end], b'e' | b'E')
                        && matches!(bytes.get(end + 1), Some(b'+' | b'-'))
                        && bytes.get(end + 2).is_some_and(u8::is_ascii_digit)
                        && !line[at..end].starts_with("0x")
                    {
                        end += 2;
                    }
                    end += 1;
                }
                if bytes.get(end) == Some(&b'.')
                    && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
                {
                    end += 1;
                    continue;
                }
                break;
            }
            push(&mut tokens, at..end, TokenKind::Number);
            at = end;
            continue;
        }
        if grammar.rust && byte == b'#' && matches!(bytes.get(at + 1), Some(b'[' | b'!')) {
            let end = line[at..]
                .rfind(']')
                .map_or(line.len(), |close| at + close + 1);
            push(&mut tokens, at..end, TokenKind::Attribute);
            at = end;
            continue;
        }
        if grammar.at_decorators
            && byte == b'@'
            && bytes
                .get(at + 1)
                .is_some_and(|byte| is_ident_start(*byte, grammar))
        {
            let end = at
                + 1
                + bytes[at + 1..]
                    .iter()
                    .take_while(|byte| is_ident(**byte, grammar) || **byte == b'.')
                    .count();
            push(&mut tokens, at..end, TokenKind::Attribute);
            at = end;
            continue;
        }
        if is_ident_start(byte, grammar) {
            let mut end = at;
            while end < bytes.len() && is_ident(bytes[end], grammar) {
                end += 1;
            }
            // Keep a multi-byte character whole.
            while !line.is_char_boundary(end) {
                end += 1;
            }
            let word = &line[at..end];
            let kind = classify(word, bytes, at, end, grammar);
            let end = if kind == Some(TokenKind::Function)
                && grammar.rust
                && bytes.get(end) == Some(&b'!')
            {
                end + 1
            } else {
                end
            };
            if let Some(kind) = kind {
                push(&mut tokens, at..end, kind);
            }
            at = end;
            continue;
        }
        if matches!(byte, b'(' | b')' | b'[' | b']' | b'{' | b'}') {
            tokens.push(Token {
                range: at..at + 1,
                kind: TokenKind::Bracket,
            });
            at += 1;
            continue;
        }
        let width = rest.chars().next().map_or(1, char::len_utf8);
        if byte.is_ascii_punctuation() {
            push(&mut tokens, at..at + width, TokenKind::Punctuation);
        }
        at += width;
    }
    (tokens, LexState::Normal)
}

fn followed_by_colon(bytes: &[u8], end: usize) -> bool {
    bytes[end..].iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b':')
}

/// `key:` (YAML) or `key =` (TOML/INI) at the start of a line, past its
/// indent and any list dash.
fn keyed_property(line: &str) -> Option<Range<usize>> {
    let bytes = line.as_bytes();
    let mut at = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_whitespace())
        .count();
    if bytes.get(at) == Some(&b'-') && bytes.get(at + 1) == Some(&b' ') {
        at += 2;
    }
    let start = at;
    while at < bytes.len()
        && (bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'_' | b'-' | b'.'))
    {
        at += 1;
    }
    if at == start {
        return None;
    }
    let after = bytes[at..].iter().find(|byte| **byte != b' ');
    matches!(after, Some(b':' | b'=')).then_some(start..at)
}

fn classify(
    word: &str,
    bytes: &[u8],
    start: usize,
    end: usize,
    grammar: &Grammar,
) -> Option<TokenKind> {
    let listed = |list: &[&str]| {
        if grammar.case_insensitive {
            list.iter().any(|known| known.eq_ignore_ascii_case(word))
        } else {
            list.contains(&word)
        }
    };
    if grammar.markup {
        let before = &bytes[..start];
        if before.ends_with(b"<") || before.ends_with(b"</") {
            return Some(TokenKind::Tag);
        }
        if bytes.get(end) == Some(&b'=') {
            return Some(TokenKind::Property);
        }
        return None;
    }
    if listed(grammar.keywords) {
        return Some(TokenKind::Keyword);
    }
    if listed(grammar.constants) {
        return Some(TokenKind::Constant);
    }
    let next = bytes[end..].iter().find(|byte| **byte != b' ');
    if grammar.rust && bytes.get(end) == Some(&b'!') && bytes.get(end + 1) != Some(&b'=') {
        return Some(TokenKind::Function);
    }
    if next == Some(&b'(') {
        return Some(TokenKind::Function);
    }
    if grammar.dollar_identifiers && word.starts_with('$') {
        return Some(TokenKind::Constant);
    }
    let first = word.chars().next()?;
    if first.is_uppercase() {
        let shouting = word.len() > 1
            && word
                .chars()
                .all(|ch| ch.is_uppercase() || ch.is_ascii_digit() || ch == '_');
        return Some(if shouting {
            TokenKind::Constant
        } else {
            TokenKind::Type
        });
    }
    // A field or method after a dot reads as a property only when called;
    // plain identifiers keep the foreground.
    None
}

fn lex_markdown(line: &str, state: LexState) -> (Vec<Token>, LexState) {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
    if let LexState::Str { .. } = state {
        let tokens = vec![Token {
            range: 0..line.len(),
            kind: TokenKind::String,
        }];
        let next = if fence { LexState::Normal } else { state };
        return (tokens, next);
    }
    if fence {
        return (
            vec![Token {
                range: 0..line.len(),
                kind: TokenKind::String,
            }],
            LexState::Str {
                quote: b'`',
                hashes: 0,
                triple: true,
                raw: false,
            },
        );
    }
    let mut tokens = Vec::new();
    if trimmed.starts_with('#') {
        tokens.push(Token {
            range: indent..line.len(),
            kind: TokenKind::Keyword,
        });
        return (tokens, LexState::Normal);
    }
    if trimmed.starts_with('>') {
        tokens.push(Token {
            range: indent..line.len(),
            kind: TokenKind::Comment,
        });
        return (tokens, LexState::Normal);
    }
    let marker = ["- ", "* ", "+ "]
        .iter()
        .find(|marker| trimmed.starts_with(**marker))
        .map(|_| 1)
        .or_else(|| {
            let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
            (digits > 0 && trimmed[digits..].starts_with(". ")).then_some(digits + 1)
        });
    if let Some(width) = marker {
        tokens.push(Token {
            range: indent..indent + width,
            kind: TokenKind::Punctuation,
        });
    }
    // Inline code and links.
    let bytes = line.as_bytes();
    let mut at = indent;
    while at < bytes.len() {
        match bytes[at] {
            b'`' => {
                let end = line[at + 1..]
                    .find('`')
                    .map_or(line.len(), |close| at + 2 + close);
                tokens.push(Token {
                    range: at..end,
                    kind: TokenKind::String,
                });
                at = end;
            }
            b'[' => {
                if let Some(close) = line[at..].find("](") {
                    let label_end = at + close + 1;
                    let url_end = line[label_end..]
                        .find(')')
                        .map_or(line.len(), |end| label_end + end + 1);
                    tokens.push(Token {
                        range: at..label_end,
                        kind: TokenKind::Function,
                    });
                    tokens.push(Token {
                        range: label_end..url_end,
                        kind: TokenKind::Comment,
                    });
                    at = url_end;
                } else {
                    at += 1;
                }
            }
            b'*' | b'_' if bytes.get(at + 1) == Some(&bytes[at]) => {
                let marker = &line[at..at + 2];
                let end = line[at + 2..]
                    .find(marker)
                    .map_or(at + 2, |close| at + 4 + close);
                tokens.push(Token {
                    range: at..end,
                    kind: TokenKind::Type,
                });
                at = end;
            }
            _ => at += 1,
        }
    }
    tokens.sort_by_key(|token| token.range.start);
    (tokens, LexState::Normal)
}

/// A bracket in the code, how deep it sits, and where its partner is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bracket {
    pub offset: usize,
    pub line: usize,
    pub depth: usize,
    pub partner: Option<usize>,
    pub open: bool,
}

fn opener(close: u8) -> u8 {
    match close {
        b')' => b'(',
        b']' => b'[',
        _ => b'{',
    }
}

/// A block that folds: its header line stays, `header + 1 ..= end` hide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fold {
    pub header: usize,
    pub end: usize,
}

/// Everything derived from one version of the text.
#[derive(Debug, Default)]
pub struct Analysis {
    pub version: u64,
    pub tokens: Vec<Vec<Token>>,
    pub brackets: Vec<Bracket>,
    pub folds: Vec<Fold>,
    /// The widest line, in characters (tabs count as `tab` columns).
    pub widest: usize,
}

/// Lexes every line, pairs brackets outside strings and comments, and finds
/// fold ranges. Linear in the size of the text.
pub fn analyze(buffer: &Buffer, language: SourceLanguage, tab: usize) -> Analysis {
    let grammar = grammar(language);
    let mut state = LexState::Normal;
    let mut tokens = Vec::with_capacity(buffer.lines());
    let mut brackets: Vec<Bracket> = Vec::new();
    let mut open: Vec<(usize, u8)> = Vec::new();
    let mut widest = 0;
    for line in 0..buffer.lines() {
        let text = buffer.line(line);
        let start = buffer.line_range(line).start;
        let (line_tokens, next) = lex_line(text, state, grammar);
        state = next;
        for token in line_tokens
            .iter()
            .filter(|token| token.kind == TokenKind::Bracket)
        {
            let byte = text.as_bytes()[token.range.start];
            let offset = start + token.range.start;
            match byte {
                b'(' | b'[' | b'{' => {
                    open.push((brackets.len(), byte));
                    brackets.push(Bracket {
                        offset,
                        line,
                        depth: open.len() - 1,
                        partner: None,
                        open: true,
                    });
                }
                _ => match open.last() {
                    Some((ix, wanted)) if *wanted == opener(byte) => {
                        let ix = *ix;
                        open.pop();
                        brackets[ix].partner = Some(offset);
                        brackets.push(Bracket {
                            offset,
                            line,
                            depth: open.len(),
                            partner: Some(brackets[ix].offset),
                            open: false,
                        });
                    }
                    _ => brackets.push(Bracket {
                        offset,
                        line,
                        depth: open.len(),
                        partner: None,
                        open: false,
                    }),
                },
            }
        }
        widest = widest.max(columns(text, tab));
        tokens.push(line_tokens);
    }
    let folds = if grammar.markdown {
        heading_folds(buffer)
    } else if grammar.braces {
        bracket_folds(buffer, &brackets)
    } else {
        indent_folds(buffer)
    };
    Analysis {
        version: buffer.version(),
        tokens,
        brackets,
        folds,
        widest,
    }
}

/// How many columns a line spans, tabs to their stops.
pub fn columns(text: &str, tab: usize) -> usize {
    let mut columns = 0;
    for ch in text.chars() {
        columns += if ch == '\t' { tab - columns % tab } else { 1 };
    }
    columns
}

/// The column a byte offset of a line sits at, tabs to their stops.
pub fn column_at(text: &str, byte: usize, tab: usize) -> usize {
    columns(&text[..byte.min(text.len())], tab)
}

/// The byte of a line nearest a display column, tabs to their stops.
pub fn byte_at_column(text: &str, column: usize, tab: usize) -> usize {
    let mut at = 0;
    for (byte, ch) in text.char_indices() {
        if column <= at {
            return byte;
        }
        let width = if ch == '\t' { tab - at % tab } else { 1 };
        if column < at + width {
            // Inside a tab: the nearer of its two edges.
            return if column - at <= width / 2 {
                byte
            } else {
                byte + ch.len_utf8()
            };
        }
        at += width;
    }
    text.len()
}

/// Folds from paired brackets that span lines. The closing line stays in
/// view when it starts with its bracket; several pairs opening on one line
/// fold to the farthest.
pub fn bracket_folds(buffer: &Buffer, brackets: &[Bracket]) -> Vec<Fold> {
    let mut by_header: Vec<Option<usize>> = vec![None; buffer.lines()];
    for bracket in brackets.iter().filter(|bracket| bracket.open) {
        let Some(partner) = bracket.partner else {
            continue;
        };
        let close_line = buffer.line_of(partner);
        if close_line <= bracket.line {
            continue;
        }
        let close_text = buffer.line(close_line);
        let leads = close_text.trim_start().len() + (partner - buffer.line_range(close_line).start)
            == close_text.len();
        let end = if leads { close_line - 1 } else { close_line };
        if end > bracket.line {
            let slot = &mut by_header[bracket.line];
            *slot = Some(slot.map_or(end, |known| known.max(end)));
        }
    }
    by_header
        .into_iter()
        .enumerate()
        .filter_map(|(header, end)| end.map(|end| Fold { header, end }))
        .collect()
}

/// Folds from indentation: a line opens a block when the non-blank lines
/// after it sit deeper; blank lines inside are skipped. Linear, by a stack.
pub fn indent_folds(buffer: &Buffer) -> Vec<Fold> {
    let mut folds = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut last = None;
    for line in 0..buffer.lines() {
        if buffer.line(line).trim().is_empty() {
            continue;
        }
        let indent = buffer.indent_columns(line, 4);
        while let Some(&(depth, header)) = stack.last() {
            if indent > depth {
                break;
            }
            stack.pop();
            if let Some(end) = last
                && end > header
            {
                folds.push(Fold { header, end });
            }
        }
        stack.push((indent, line));
        last = Some(line);
    }
    while let Some((_, header)) = stack.pop() {
        if let Some(end) = last
            && end > header
        {
            folds.push(Fold { header, end });
        }
    }
    folds.sort_by_key(|fold| fold.header);
    folds
}

/// Markdown folds by heading: a heading holds everything until the next
/// heading of its level or higher, less trailing blank lines.
pub fn heading_folds(buffer: &Buffer) -> Vec<Fold> {
    let mut headings = Vec::new();
    let mut fenced = false;
    for line in 0..buffer.lines() {
        let text = buffer.line(line).trim_start();
        if text.starts_with("```") {
            fenced = !fenced;
        }
        if fenced {
            continue;
        }
        let level = text.bytes().take_while(|byte| *byte == b'#').count();
        if level > 0 && text.as_bytes().get(level) == Some(&b' ') {
            headings.push((line, level));
        }
    }
    let mut folds = Vec::new();
    for (ix, (header, level)) in headings.iter().enumerate() {
        let next = headings[ix + 1..]
            .iter()
            .find(|(_, other)| other <= level)
            .map_or(buffer.lines(), |(line, _)| *line);
        let mut end = next - 1;
        while end > *header && buffer.line(end).trim().is_empty() {
            end -= 1;
        }
        if end > *header {
            folds.push(Fold {
                header: *header,
                end,
            });
        }
    }
    folds
}

/// The pair beside a caret: the bracket just after it, or else the one just before.
pub fn matched(brackets: &[Bracket], caret: usize) -> Option<(usize, usize)> {
    let at = |offset: usize| {
        brackets
            .binary_search_by_key(&offset, |bracket| bracket.offset)
            .ok()
            .map(|ix| brackets[ix])
    };
    let bracket = at(caret).or_else(|| caret.checked_sub(1).and_then(at))?;
    Some((bracket.offset, bracket.partner?))
}

/// The brackets on a line, by binary search over the sorted list.
pub fn brackets_in(brackets: &[Bracket], range: Range<usize>) -> &[Bracket] {
    let start = brackets.partition_point(|bracket| bracket.offset < range.start);
    let end = brackets.partition_point(|bracket| bracket.offset < range.end);
    &brackets[start..end]
}

/// Where `needle` appears in one line, whole words only when it is a word.
pub fn occurrences_in(text: &str, needle: &str) -> Vec<Range<usize>> {
    if needle.is_empty() || needle.contains('\n') {
        return Vec::new();
    }
    let word = needle.chars().all(super::buffer::is_word_char);
    let boundary = |ch: Option<char>| ch.is_none_or(|ch| !super::buffer::is_word_char(ch));
    text.match_indices(needle)
        .filter(|(at, _)| {
            let end = at + needle.len();
            !word
                || (boundary(text[..*at].chars().next_back())
                    && boundary(text[end..].chars().next()))
        })
        .map(|(at, _)| at..at + needle.len())
        .collect()
}

/// The folds holding `line`, outermost first.
pub fn enclosing(folds: &[Fold], line: usize) -> Vec<Fold> {
    // Folds are sorted by header, so only those starting before the line can hold it.
    let before = folds.partition_point(|fold| fold.header < line);
    folds[..before]
        .iter()
        .filter(|fold| line <= fold.end)
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str, language: SourceLanguage) -> Vec<(&str, TokenKind)> {
        lex_line(line, LexState::Normal, grammar(language))
            .0
            .into_iter()
            .map(|token| (&line[token.range], token.kind))
            .collect()
    }

    fn has(line: &str, language: SourceLanguage, word: &str, kind: TokenKind) -> bool {
        kinds(line, language).contains(&(word, kind))
    }

    #[test]
    fn rust_lines_color_keywords_strings_comments_types_and_calls() {
        use SourceLanguage::Rust;
        let line =
            r#"pub fn main() -> Result<()> { let v = "hi \" there"; println!("{v}"); } // done"#;
        assert!(has(line, Rust, "pub", TokenKind::Keyword));
        assert!(has(line, Rust, "fn", TokenKind::Keyword));
        assert!(has(line, Rust, "main", TokenKind::Function));
        assert!(has(line, Rust, "Result", TokenKind::Type));
        assert!(has(line, Rust, r#""hi \" there""#, TokenKind::String));
        assert!(has(line, Rust, "println!", TokenKind::Function));
        assert!(has(line, Rust, "// done", TokenKind::Comment));
        assert!(has(
            "let x = 0x1F + 1.5e-3;",
            Rust,
            "0x1F",
            TokenKind::Number
        ));
        assert!(has(
            "let x = 0x1F + 1.5e-3;",
            Rust,
            "1.5e-3",
            TokenKind::Number
        ));
        assert!(has(
            "const MAX_LEN: usize = 4;",
            Rust,
            "MAX_LEN",
            TokenKind::Constant
        ));
        assert!(has(
            "#[derive(Debug)]",
            Rust,
            "#[derive(Debug)]",
            TokenKind::Attribute
        ));
        assert!(
            kinds("let format = before;", Rust)
                .iter()
                .all(|(word, _)| *word != "format" && *word != "before")
        );
    }

    #[test]
    fn rust_lifetimes_are_not_char_literals() {
        use SourceLanguage::Rust;
        let line = "fn f<'a>(x: &'a str) -> char { 'x' }";
        assert!(has(line, Rust, "'a", TokenKind::Keyword));
        assert!(has(line, Rust, "'x'", TokenKind::String));
        assert!(has("let c = '\\n';", Rust, "'\\n'", TokenKind::String));
    }

    #[test]
    fn block_comments_and_strings_carry_across_lines() {
        let rust = grammar(SourceLanguage::Rust);
        let (tokens, state) = lex_line("let a = 1; /* open", LexState::Normal, rust);
        assert_eq!(state, LexState::Comment { depth: 1 });
        assert_eq!(tokens.last().unwrap().kind, TokenKind::Comment);
        let (tokens, state) = lex_line("still /* nested */ inside", state, rust);
        assert_eq!(state, LexState::Comment { depth: 1 }, "Rust nests comments");
        assert_eq!(tokens.len(), 1);
        let (tokens, state) = lex_line("done */ let b", state, rust);
        assert_eq!(state, LexState::Normal);
        assert_eq!(
            tokens[0],
            Token {
                range: 0..7,
                kind: TokenKind::Comment
            }
        );

        let (_, state) = lex_line(r##"let s = r#"raw "##, LexState::Normal, rust);
        assert!(matches!(state, LexState::Str { hashes: 1, .. }));
        let (tokens, state) = lex_line(r##"still "# ; let x"##, state, rust);
        assert_eq!(state, LexState::Normal);
        assert_eq!(tokens[0].range, 0..8);

        let python = grammar(SourceLanguage::Python);
        let (_, state) = lex_line("doc = \"\"\"start", LexState::Normal, python);
        assert!(matches!(state, LexState::Str { triple: true, .. }));
        let (_, state) = lex_line("end\"\"\"", state, python);
        assert_eq!(state, LexState::Normal);

        let js = grammar(SourceLanguage::TypeScript);
        let (_, state) = lex_line("const t = `multi", LexState::Normal, js);
        assert!(matches!(state, LexState::Str { quote: b'`', .. }));
        let (_, state) = lex_line("const s = 'single", LexState::Normal, js);
        assert_eq!(
            state,
            LexState::Normal,
            "a quote that cannot span lines stops"
        );
    }

    #[test]
    fn other_languages_get_their_own_words() {
        assert!(has(
            "def run(self): pass  # note",
            SourceLanguage::Python,
            "# note",
            TokenKind::Comment
        ));
        assert!(has(
            "def run(self): pass",
            SourceLanguage::Python,
            "run",
            TokenKind::Function
        ));
        assert!(has(
            "@dataclass",
            SourceLanguage::Python,
            "@dataclass",
            TokenKind::Attribute
        ));
        assert!(has(
            "SELECT id FROM users",
            SourceLanguage::Sql,
            "SELECT",
            TokenKind::Keyword
        ));
        assert!(has(
            "name = \"diri\"",
            SourceLanguage::Toml,
            "name",
            TokenKind::Property
        ));
        assert!(has(
            "  image: nginx",
            SourceLanguage::Yaml,
            "image",
            TokenKind::Property
        ));
        assert!(has(
            "{\"key\": true}",
            SourceLanguage::Json,
            "\"key\"",
            TokenKind::Property
        ));
        assert!(has(
            "{\"key\": true}",
            SourceLanguage::Json,
            "true",
            TokenKind::Constant
        ));
        assert!(has(
            "<div class=\"x\">",
            SourceLanguage::Html,
            "div",
            TokenKind::Tag
        ));
        assert!(has(
            "<div class=\"x\">",
            SourceLanguage::Html,
            "class",
            TokenKind::Property
        ));
        assert!(has(
            "echo $HOME # hi",
            SourceLanguage::Shell,
            "$HOME",
            TokenKind::Constant
        ));
        assert!(has(
            "## Title",
            SourceLanguage::Markdown,
            "## Title",
            TokenKind::Keyword
        ));
        assert!(has(
            "use `code` here",
            SourceLanguage::Markdown,
            "`code`",
            TokenKind::String
        ));
    }

    #[test]
    fn tokens_never_overlap_and_stay_on_char_boundaries() {
        for line in [
            "let π = \"ünïcode\"; // ✓ done",
            "fn a() { b(c[d{e}]) }",
            "x = 'unterminated",
            "'",
            "r#",
            "#[",
        ] {
            for language in [
                SourceLanguage::Rust,
                SourceLanguage::Python,
                SourceLanguage::TypeScript,
            ] {
                let (tokens, _) = lex_line(line, LexState::Normal, grammar(language));
                for pair in tokens.windows(2) {
                    assert!(
                        pair[0].range.end <= pair[1].range.start,
                        "{line:?} {language:?}"
                    );
                }
                for token in &tokens {
                    assert!(
                        line.is_char_boundary(token.range.start)
                            && line.is_char_boundary(token.range.end)
                    );
                    assert!(token.range.end <= line.len());
                }
            }
        }
    }

    #[test]
    fn brackets_pair_by_depth_outside_strings_and_comments() {
        let buffer = Buffer::new("f(a[0], \"(\") // )\n{ }");
        let analysis = analyze(&buffer, SourceLanguage::Rust, 4);
        let offsets: Vec<usize> = analysis
            .brackets
            .iter()
            .map(|bracket| bracket.offset)
            .collect();
        assert_eq!(offsets, [1, 3, 5, 11, 18, 20]);
        assert_eq!(analysis.brackets[1].depth, 1, "rainbow depth");
        assert_eq!(
            analysis.brackets[2].depth, 1,
            "a closer takes its opener's depth"
        );
        assert_eq!(analysis.brackets[3].partner, Some(1));
        assert_eq!(matched(&analysis.brackets, 12), Some((11, 1)));
        assert_eq!(matched(&analysis.brackets, 18), Some((18, 20)));
        assert_eq!(matched(&analysis.brackets, 7), None);
        assert_eq!(brackets_in(&analysis.brackets, 0..17).len(), 4);
    }

    #[test]
    fn unbalanced_closers_do_not_pair() {
        let buffer = Buffer::new("a) (b]");
        let analysis = analyze(&buffer, SourceLanguage::Rust, 4);
        assert!(
            analysis
                .brackets
                .iter()
                .all(|bracket| bracket.partner.is_none())
        );
    }

    #[test]
    fn bracket_folds_keep_a_leading_closer_visible() {
        let buffer = Buffer::new(
            "fn a() {\n    one(\n        x,\n    );\n}\nfn b() {}\nlet v = vec![1,\n  2];",
        );
        let analysis = analyze(&buffer, SourceLanguage::Rust, 4);
        assert_eq!(
            analysis.folds,
            vec![
                Fold { header: 0, end: 3 },
                Fold { header: 1, end: 2 },
                Fold { header: 6, end: 7 },
            ]
        );
        assert_eq!(
            enclosing(&analysis.folds, 2),
            vec![Fold { header: 0, end: 3 }, Fold { header: 1, end: 2 }]
        );
        assert!(enclosing(&analysis.folds, 0).is_empty());
    }

    #[test]
    fn indent_folds_skip_blank_lines_and_nest() {
        let buffer = Buffer::new("def a():\n    one()\n\n    if x:\n        two()\ndef b(): pass");
        assert_eq!(
            indent_folds(&buffer),
            vec![Fold { header: 0, end: 4 }, Fold { header: 3, end: 4 }]
        );
        assert_eq!(
            indent_folds(&Buffer::new("a\n  b\n\n  c\nd")),
            vec![Fold { header: 0, end: 3 }]
        );
    }

    #[test]
    fn markdown_folds_by_heading_level() {
        let buffer = Buffer::new("# A\ntext\n## B\nmore\n\n# C\nend");
        assert_eq!(
            heading_folds(&buffer),
            vec![
                Fold { header: 0, end: 3 },
                Fold { header: 2, end: 3 },
                Fold { header: 5, end: 6 }
            ]
        );
    }

    #[test]
    fn occurrences_of_a_word_skip_longer_words() {
        assert_eq!(
            occurrences_in("let total = total_cost + total;", "total"),
            [4..9, 25..30]
        );
        assert_eq!(
            occurrences_in("a+b a+b", "a+b"),
            [0..3, 4..7],
            "marks match anywhere"
        );
    }

    #[test]
    fn display_columns_expand_tabs() {
        assert_eq!(columns("\tab", 4), 6);
        assert_eq!(column_at("\tab", 1, 4), 4);
        assert_eq!(byte_at_column("\tab", 4, 4), 1);
        assert_eq!(byte_at_column("\tab", 1, 4), 0, "inside a tab's first half");
        assert_eq!(byte_at_column("\tab", 3, 4), 1, "inside its second half");
        assert_eq!(byte_at_column("abc", 9, 4), 3);
        assert_eq!(byte_at_column("abc", 1, 4), 1);
    }

    #[test]
    fn a_large_file_analyses_in_linear_time() {
        let text = "fn f() {\n    if x { y(); }\n}\n".repeat(20_000);
        let buffer = Buffer::new(text);
        let started = std::time::Instant::now();
        let analysis = analyze(&buffer, SourceLanguage::Rust, 4);
        assert_eq!(analysis.folds.len(), 20_000);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
