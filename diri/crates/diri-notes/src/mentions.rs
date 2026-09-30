//! @-mentions: links from a note to Diri sessions and other notes.
//!
//! A mention is an ordinary Markdown link whose URL uses the `diri://`
//! scheme, so the codec, the editor, and plain-text readers all share one
//! representation and nothing new is stored:
//!
//! ```text
//! [@Codex: fix resize](diri://session/s_4e97a43bd495)
//! [@Resize PRD](diri://note/20260930-141201-ab12)
//! ```
//!
//! The label is display text only; tools resolve mentions by id, so renaming
//! a session never breaks one. A link whose id is malformed stays a plain link.

use std::ops::Range;

use crate::doc::{Block, Document, Style};
use crate::store::is_valid_id;

pub const SCHEME: &str = "diri://";
const SESSION_PREFIX: &str = "diri://session/";
const NOTE_PREFIX: &str = "diri://note/";

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MentionTarget {
    /// A Diri session id (`s_…`). Note sessions are sessions too.
    Session(String),
    /// A notes file id.
    Note(String),
}

impl MentionTarget {
    pub fn parse(url: &str) -> Option<Self> {
        let target = if let Some(id) = url.strip_prefix(SESSION_PREFIX) {
            Self::Session(id.to_owned())
        } else {
            Self::Note(url.strip_prefix(NOTE_PREFIX)?.to_owned())
        };
        is_valid_id(target.id()).then_some(target)
    }

    pub fn url(&self) -> String {
        match self {
            Self::Session(id) => format!("{SESSION_PREFIX}{id}"),
            Self::Note(id) => format!("{NOTE_PREFIX}{id}"),
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Session(id) | Self::Note(id) => id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention {
    /// Index into [`Document::blocks`].
    pub block: usize,
    /// Byte range of the label inside the block's text.
    pub range: Range<usize>,
    pub label: String,
    pub target: MentionTarget,
}

/// Every mention in document order. The title cannot hold marks, so only
/// blocks are searched.
pub fn mentions(doc: &Document) -> Vec<Mention> {
    doc.blocks
        .iter()
        .enumerate()
        .flat_map(|(index, block)| {
            block_mentions(block).map(move |(range, target)| (index, range, target))
        })
        .map(|(block, range, target)| Mention {
            label: doc.blocks[block].text[range.clone()].to_owned(),
            block,
            range,
            target,
        })
        .collect()
}

/// Distinct targets in first-mention order.
pub fn targets(doc: &Document) -> Vec<MentionTarget> {
    let mut out: Vec<MentionTarget> = Vec::new();
    for mention in mentions(doc) {
        if !out.contains(&mention.target) {
            out.push(mention.target);
        }
    }
    out
}

/// The mentions inside one block, in order.
pub fn block_mentions(block: &Block) -> impl Iterator<Item = (Range<usize>, MentionTarget)> + '_ {
    block.marks.iter().filter_map(|mark| match &mark.style {
        Style::Link(url) => MentionTarget::parse(url).map(|t| (mark.range.clone(), t)),
        _ => None,
    })
}

/// The Markdown for a mention, e.g. `[@Codex](diri://session/s_1)`.
pub fn markdown_link(label: &str, target: &MentionTarget) -> String {
    let mut block = Block::new(0, crate::doc::BlockKind::Paragraph, "");
    append(&mut block, label, target);
    crate::markdown::write_inline(&block, false)
}

/// Appends a mention to the end of `block`, separated by a space. Returns
/// false (and changes nothing) when the block already mentions `target`.
pub fn append(block: &mut Block, label: &str, target: &MentionTarget) -> bool {
    if block_mentions(block).any(|(_, t)| &t == target) {
        return false;
    }
    let label = label_text(label);
    if !block.text.is_empty() && !block.text.ends_with(' ') {
        let end = block.text.len();
        block.replace(end..end, " ", &[]);
    }
    let start = block.text.len();
    block.replace(start..start, &label, &[]);
    block.add_mark(start..block.text.len(), Style::Link(target.url()));
    true
}

/// Labels are single-line and start with `@`.
fn label_text(label: &str) -> String {
    let flat: String = label.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = if flat.is_empty() {
        "note".to_owned()
    } else {
        flat
    };
    if flat.starts_with('@') {
        flat
    } else {
        format!("@{flat}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::BlockKind;
    use crate::markdown;

    #[test]
    fn parses_only_well_formed_targets() {
        assert_eq!(
            MentionTarget::parse("diri://session/s_4e97a43bd495"),
            Some(MentionTarget::Session("s_4e97a43bd495".into()))
        );
        assert_eq!(
            MentionTarget::parse("diri://note/20260930-141201-ab12"),
            Some(MentionTarget::Note("20260930-141201-ab12".into()))
        );
        for bad in [
            "diri://session/",
            "diri://session/../x",
            "diri://session/a/b",
            "diri://note/.hidden",
            "diri://tab/1",
            "https://diri.sh/session/s_1",
        ] {
            assert_eq!(MentionTarget::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn finds_mentions_in_parsed_markdown() {
        let (_, doc) = markdown::parse(
            "# Resize\n\nAsk [@Codex: fix resize](diri://session/s_1) and [docs](https://x.dev).\n\n\
             - [ ] Port [@Resize PRD](diri://note/n-1) then [@Codex again](diri://session/s_1)\n",
        );
        let found = mentions(&doc);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].label, "@Codex: fix resize");
        assert_eq!(found[0].target, MentionTarget::Session("s_1".into()));
        assert_eq!(found[1].target, MentionTarget::Note("n-1".into()));
        assert_eq!(
            targets(&doc),
            vec![
                MentionTarget::Session("s_1".into()),
                MentionTarget::Note("n-1".into())
            ]
        );
    }

    #[test]
    fn appending_round_trips_and_is_idempotent() {
        let target = MentionTarget::Session("s_9".into());
        let mut block = Block::new(1, BlockKind::Todo { checked: false }, "Fix resize");
        assert!(append(&mut block, "Codex\nfix  resize", &target));
        assert!(!append(&mut block, "Codex", &target));
        assert_eq!(block.text, "Fix resize @Codex fix resize");

        let doc = Document::new("T", vec![block]);
        let source = markdown::write(&Default::default(), &doc);
        assert!(
            source.contains("- [ ] Fix resize [@Codex fix resize](diri://session/s_9)"),
            "{source}"
        );
        let (_, back) = markdown::parse(&source);
        assert_eq!(targets(&back), vec![target]);
    }

    #[test]
    fn markdown_link_escapes_label() {
        let link = markdown_link("a [b] *c*", &MentionTarget::Note("n".into()));
        let (_, doc) = markdown::parse(&link);
        let found = mentions(&doc);
        assert_eq!(found[0].label, "@a [b] *c*");
        assert_eq!(found[0].target, MentionTarget::Note("n".into()));
    }
}
