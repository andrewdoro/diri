//! Links between notes, and the index that answers "what links here".
//!
//! A link from one note to another is a note mention: an ordinary Markdown
//! link to `diri://note/<id>` (see [`crate::mention`]). It names the note by
//! its file id, never by title, so renaming a note keeps every link to it.
//! Agents may also write `[[Title]]` (Obsidian's wiki link); the tools turn
//! it into a mention before it is stored ([`rewrite_wiki_links`]), so the
//! file only ever holds one link syntax.
//!
//! [`LinkIndex`] is built from saved notes and updated one note at a time
//! ([`LinkIndex::upsert`] when a file is written, [`LinkIndex::remove`] when
//! it goes): nothing here runs per frame. It answers backlinks (with the
//! words around each link), unlinked mentions (a note's title written as
//! plain text elsewhere), and the link graph.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::Arc;

use crate::doc::{Block, BlockKind, Document, Style};
use crate::markdown;
use crate::mention::{self, MentionTarget};

/// Characters of context kept on each side of a link in a snippet.
pub const CONTEXT_CHARS: usize = 60;
/// Titles shorter than this are too common to count as a mention.
const MIN_UNLINKED_TITLE_CHARS: usize = 3;

/// One link from a note to another note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutLink {
    /// The linked note's id.
    pub target: String,
    /// Block index in the note (title not counted).
    pub block: usize,
    /// The chip's text as written, `@Release plan`.
    pub label: String,
    /// The words around the link, one line.
    pub context: String,
    /// Where the link's text sits in `context`.
    pub mention: Range<usize>,
    /// Byte range of the link's text in the block.
    pub range: Range<usize>,
}

/// One searchable line of a note, for unlinked mentions.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Line {
    block: usize,
    text: String,
    /// Byte ranges already covered by a link (any link), which never count
    /// as an unlinked mention.
    linked: Vec<Range<usize>>,
}

/// What the index keeps for one note: its title, its outgoing note links
/// and its text lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteLinks {
    pub id: String,
    pub title: String,
    pub links: Vec<OutLink>,
    lines: Vec<Line>,
}

impl NoteLinks {
    /// Reads `doc`'s links. Code blocks hold no links and no mentions.
    pub fn from_doc(id: &str, doc: &Document) -> Self {
        let mut links = Vec::new();
        let mut lines = Vec::new();
        for (index, block) in doc.blocks.iter().enumerate() {
            if block.text.trim().is_empty() || matches!(block.kind, BlockKind::Code) {
                continue;
            }
            for chip in mention::in_block(block) {
                let MentionTarget::Note(target) = chip.target else {
                    continue;
                };
                // A note linking to itself is no link.
                if target == id {
                    continue;
                }
                let (context, range) = context(&block.text, chip.range.clone(), CONTEXT_CHARS);
                links.push(OutLink {
                    target,
                    block: index,
                    label: block.text[chip.range.clone()].to_owned(),
                    context,
                    mention: range,
                    range: chip.range,
                });
            }
            if block.kind.is_atomic() {
                continue;
            }
            lines.push(Line {
                block: index,
                text: block.text.clone(),
                linked: block
                    .marks
                    .iter()
                    .filter(|mark| matches!(mark.style, Style::Link(_)))
                    .map(|mark| mark.range.clone())
                    .collect(),
            });
        }
        Self {
            id: id.to_owned(),
            title: doc.title.trim().to_owned(),
            links,
            lines,
        }
    }

    /// The notes this one links to, each once, in first-link order.
    pub fn targets(&self) -> Vec<&str> {
        let mut seen = Vec::new();
        for link in &self.links {
            if !seen.contains(&link.target.as_str()) {
                seen.push(link.target.as_str());
            }
        }
        seen
    }

    pub fn display_title(&self) -> &str {
        if self.title.is_empty() {
            "Untitled"
        } else {
            &self.title
        }
    }
}

/// A note that refers to another: by a link (a backlink) or by writing its
/// title as plain text (an unlinked mention).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    /// The referring note.
    pub source: String,
    pub source_title: String,
    pub block: usize,
    pub context: String,
    /// Where the reference sits in `context`.
    pub mention: Range<usize>,
    /// Byte range of the reference in the block's text: what
    /// [`link_mention`] turns into a link.
    pub range: Range<usize>,
}

/// Notes and the links between them, for a graph.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkGraph {
    pub nodes: Vec<GraphNode>,
    /// Undirected, each pair once as (lower index, higher index), sorted.
    pub edges: Vec<(usize, usize)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphNode {
    pub id: String,
    pub title: String,
    /// Edges touching this node.
    pub degree: usize,
}

/// Every saved note's links, with the reverse map kept beside them.
#[derive(Clone, Debug, Default)]
pub struct LinkIndex {
    notes: HashMap<String, Arc<NoteLinks>>,
    /// Target id → the notes linking to it.
    incoming: HashMap<String, BTreeSet<String>>,
}

impl LinkIndex {
    /// Reads every note in `store`. For one-off readers (agents' tools, the
    /// CLI); the app keeps its index current note by note instead.
    pub fn read(store: &crate::store::NoteStore) -> std::io::Result<Self> {
        let mut index = Self::default();
        for meta in store.list()? {
            if let Ok(note) = store.load(&meta.id) {
                index.upsert_doc(&meta.id, &note.doc);
            }
        }
        Ok(index)
    }

    pub fn len(&self) -> usize {
        self.notes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.notes.contains_key(id)
    }

    pub fn get(&self, id: &str) -> Option<&NoteLinks> {
        self.notes.get(id).map(Arc::as_ref)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.notes.keys().map(String::as_str)
    }

    /// Puts in (or replaces) one note: called when its file is written.
    /// Returns whether anything the index answers changed.
    pub fn upsert(&mut self, links: NoteLinks) -> bool {
        if self.notes.get(&links.id).is_some_and(|old| **old == links) {
            return false;
        }
        self.unlink(&links.id);
        for target in links.targets() {
            self.incoming
                .entry(target.to_owned())
                .or_default()
                .insert(links.id.clone());
        }
        self.notes.insert(links.id.clone(), Arc::new(links));
        true
    }

    /// Reads and puts in one note.
    pub fn upsert_doc(&mut self, id: &str, doc: &Document) -> bool {
        self.upsert(NoteLinks::from_doc(id, doc))
    }

    /// Takes a note out: its file was deleted or trashed. Links to it from
    /// other notes stay (they point at a missing note until it returns).
    pub fn remove(&mut self, id: &str) -> bool {
        self.unlink(id);
        self.notes.remove(id).is_some()
    }

    /// Drops every note `keep` rejects; returns whether any went.
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) -> bool {
        let gone: Vec<String> = self.notes.keys().filter(|id| !keep(id)).cloned().collect();
        for id in &gone {
            self.remove(id);
        }
        !gone.is_empty()
    }

    fn unlink(&mut self, source: &str) {
        let Some(old) = self.notes.get(source).cloned() else {
            return;
        };
        for target in old.targets() {
            if let Some(sources) = self.incoming.get_mut(target) {
                sources.remove(source);
                if sources.is_empty() {
                    self.incoming.remove(target);
                }
            }
        }
    }

    /// A note's title as the index knows it, `Untitled` for a blank one.
    pub fn title(&self, id: &str) -> Option<&str> {
        self.get(id).map(NoteLinks::display_title)
    }

    /// Links out of `id`, in document order.
    pub fn outgoing(&self, id: &str) -> &[OutLink] {
        self.get(id).map_or(&[], |note| note.links.as_slice())
    }

    /// Every note linking to `target`: one entry per link, notes by title
    /// then id, links in document order.
    pub fn backlinks(&self, target: &str) -> Vec<Reference> {
        let Some(sources) = self.incoming.get(target) else {
            return Vec::new();
        };
        let mut sources: Vec<&NoteLinks> = sources
            .iter()
            .filter_map(|id| self.get(id))
            .filter(|note| note.id != target)
            .collect();
        sources.sort_by(|a, b| {
            a.display_title()
                .to_lowercase()
                .cmp(&b.display_title().to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        let mut out = Vec::new();
        for note in sources {
            for link in note.links.iter().filter(|link| link.target == target) {
                out.push(Reference {
                    source: note.id.clone(),
                    source_title: note.display_title().to_owned(),
                    block: link.block,
                    context: link.context.clone(),
                    mention: link.mention.clone(),
                    range: link.range.clone(),
                });
            }
        }
        out
    }

    /// Notes linking to `target`, each once.
    pub fn linking_notes(&self, target: &str) -> Vec<&str> {
        self.incoming
            .get(target)
            .map(|sources| {
                sources
                    .iter()
                    .map(String::as_str)
                    .filter(|id| *id != target)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Places where another note writes `target`'s title as plain text, not
    /// as a link: whole words, case-insensitively. At most `limit`. Empty for
    /// a title too short or generic to mean this note.
    pub fn unlinked_mentions(&self, target: &str, limit: usize) -> Vec<Reference> {
        let Some(note) = self.get(target) else {
            return Vec::new();
        };
        let title = note.title.trim();
        if title.chars().count() < MIN_UNLINKED_TITLE_CHARS
            || title.eq_ignore_ascii_case("untitled")
        {
            return Vec::new();
        }
        let mut sources: Vec<&NoteLinks> = self
            .notes
            .values()
            .map(Arc::as_ref)
            .filter(|other| other.id != target)
            .collect();
        sources.sort_by(|a, b| {
            a.display_title()
                .to_lowercase()
                .cmp(&b.display_title().to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        let mut out = Vec::new();
        for source in sources {
            for line in &source.lines {
                for found in find_words(&line.text, title) {
                    if line
                        .linked
                        .iter()
                        .any(|linked| linked.start < found.end && found.start < linked.end)
                    {
                        continue;
                    }
                    let (context, mention) = context(&line.text, found.clone(), CONTEXT_CHARS);
                    out.push(Reference {
                        source: source.id.clone(),
                        source_title: source.display_title().to_owned(),
                        block: line.block,
                        context,
                        mention,
                        range: found,
                    });
                    if out.len() >= limit {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// Every note and every link between two notes the index knows. Nodes
    /// are ordered by id so the same notes always give the same graph.
    pub fn graph(&self) -> LinkGraph {
        let mut ids: Vec<&str> = self.ids().collect();
        ids.sort_unstable();
        self.graph_of(&ids)
    }

    /// `center` and the notes within `depth` links of it, either direction.
    pub fn neighborhood(&self, center: &str, depth: usize) -> LinkGraph {
        if !self.contains(center) {
            return LinkGraph::default();
        }
        let mut seen: HashSet<&str> = HashSet::from([center]);
        let mut queue = VecDeque::from([(center, 0usize)]);
        while let Some((id, at)) = queue.pop_front() {
            if at >= depth {
                continue;
            }
            let out = self.get(id).into_iter().flat_map(NoteLinks::targets);
            let incoming = self.linking_notes(id);
            for next in out.chain(incoming) {
                if self.contains(next) && seen.insert(next) {
                    queue.push_back((next, at + 1));
                }
            }
        }
        let mut ids: Vec<&str> = seen.into_iter().collect();
        ids.sort_unstable();
        // The center leads, so a view can find it at index 0.
        if let Some(at) = ids.iter().position(|id| *id == center) {
            let id = ids.remove(at);
            ids.insert(0, id);
        }
        self.graph_of(&ids)
    }

    fn graph_of(&self, ids: &[&str]) -> LinkGraph {
        let at: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let mut edges = BTreeSet::new();
        for (a, id) in ids.iter().enumerate() {
            for target in self.get(id).into_iter().flat_map(NoteLinks::targets) {
                if let Some(&b) = at.get(target)
                    && a != b
                {
                    edges.insert((a.min(b), a.max(b)));
                }
            }
        }
        let edges: Vec<(usize, usize)> = edges.into_iter().collect();
        let mut degree = vec![0usize; ids.len()];
        for (a, b) in &edges {
            degree[*a] += 1;
            degree[*b] += 1;
        }
        LinkGraph {
            nodes: ids
                .iter()
                .enumerate()
                .map(|(i, id)| GraphNode {
                    id: (*id).to_owned(),
                    title: self.title(id).unwrap_or("Untitled").to_owned(),
                    degree: degree[i],
                })
                .collect(),
            edges,
        }
    }
}

/// Byte ranges where `needle` appears in `text` as whole words, ignoring
/// ASCII case. Non-ASCII letters must match exactly, which keeps every
/// range on character boundaries.
fn find_words(text: &str, needle: &str) -> Vec<Range<usize>> {
    let hay = text.as_bytes();
    let pin = needle.as_bytes();
    let mut out = Vec::new();
    if pin.is_empty() || pin.len() > hay.len() {
        return out;
    }
    let mut start = 0;
    while start + pin.len() <= hay.len() {
        if text.is_char_boundary(start)
            && hay[start..start + pin.len()].eq_ignore_ascii_case(pin)
            && text.is_char_boundary(start + pin.len())
        {
            let before = text[..start].chars().next_back();
            let after = text[start + pin.len()..].chars().next();
            let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            if !word(before) && !word(after) {
                out.push(start..start + pin.len());
                start += pin.len();
                continue;
            }
        }
        start += 1;
    }
    out
}

/// One line of `text` around `range`: at most `radius` characters on each
/// side, cut at a word where it can be, with `…` where text was dropped.
/// Returns the snippet and where `range` landed in it.
pub fn context(text: &str, range: Range<usize>, radius: usize) -> (String, Range<usize>) {
    let start = range.start.min(text.len());
    let range = start..range.end.clamp(start, text.len());
    let before = &text[..range.start];
    let after = &text[range.end..];
    let mut lead: &str = before;
    let lead_chars = before.chars().count();
    let mut cut_lead = false;
    if lead_chars > radius {
        let skip = before
            .char_indices()
            .nth(lead_chars - radius)
            .map_or(0, |(i, _)| i);
        lead = &before[skip..];
        // Start on a word.
        if let Some(space) = lead.find(char::is_whitespace)
            && space + 1 < lead.len()
        {
            lead = &lead[space..];
        }
        cut_lead = true;
    }
    let mut tail: &str = after;
    let mut cut_tail = false;
    if after.chars().count() > radius {
        let end = after
            .char_indices()
            .nth(radius)
            .map_or(after.len(), |(i, _)| i);
        tail = &after[..end];
        if let Some(space) = tail.rfind(char::is_whitespace)
            && space > 0
        {
            tail = &tail[..space];
        }
        cut_tail = true;
    }
    let flat = |s: &str| s.replace(['\n', '\t'], " ");
    let mut out = String::new();
    if cut_lead {
        out.push('…');
    }
    let lead = flat(lead);
    let lead = if cut_lead { lead.trim_start() } else { &lead };
    out.push_str(lead);
    let start = out.len();
    out.push_str(&flat(&text[range.clone()]));
    let end = out.len();
    let tail = flat(tail);
    out.push_str(if cut_tail { tail.trim_end() } else { &tail });
    if cut_tail {
        out.push('…');
    }
    (out, start..end)
}

/// Turns the plain text at `range` of block `block` (as an unlinked mention
/// reported it) into a link to note `target`: the person's words stay, as a
/// chip with an `@`. Returns false when the text there is not `expected`
/// any more, or already links somewhere.
pub fn link_mention(
    doc: &mut Document,
    block: usize,
    range: Range<usize>,
    expected: &str,
    target: &str,
) -> bool {
    let Some(b) = doc.blocks.get_mut(block) else {
        return false;
    };
    if b.text.get(range.clone()) != Some(expected)
        || matches!(b.kind, BlockKind::Code)
        || b.marks.iter().any(|m| {
            matches!(m.style, Style::Link(_))
                && m.range.start < range.end
                && range.start < m.range.end
        })
    {
        return false;
    }
    let label = format!("@{expected}");
    b.replace(range.clone(), &label, &[]);
    b.add_mark(
        range.start..range.start + label.len(),
        Style::Link(MentionTarget::Note(target.to_owned()).url()),
    );
    true
}

// ---------------------------------------------------------------------------
// Wiki links

/// What [`rewrite_wiki_links`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WikiRewrite {
    pub text: String,
    /// Note ids linked, in order, each once.
    pub linked: Vec<String>,
    /// `[[…]]` titles no single note matched; left as written.
    pub unresolved: Vec<String>,
}

/// The longest `[[title]]` taken as a link.
const MAX_WIKI_CHARS: usize = 200;

/// Rewrites `[[Title]]` and `[[Title|shown words]]` in Markdown to note
/// mentions, `[@Title](diri://note/<id>)`. `resolve` maps a title to the one
/// note it names (id, title), or `None`. Fenced code blocks and inline code
/// are left alone, as is anything `resolve` cannot place.
pub fn rewrite_wiki_links(
    markdown: &str,
    resolve: impl Fn(&str) -> Option<(String, String)>,
) -> WikiRewrite {
    let mut out = WikiRewrite::default();
    let mut text = String::with_capacity(markdown.len());
    let mut fence: Option<&str> = None;
    for (i, line) in markdown.split('\n').enumerate() {
        if i > 0 {
            text.push('\n');
        }
        let trimmed = line.trim_start();
        let marker = ["```", "~~~"].into_iter().find(|m| trimmed.starts_with(m));
        match (fence, marker) {
            (Some(open), Some(m)) if m == open => {
                fence = None;
                text.push_str(line);
                continue;
            }
            (None, Some(m)) => {
                fence = Some(m);
                text.push_str(line);
                continue;
            }
            (Some(_), _) => {
                text.push_str(line);
                continue;
            }
            (None, None) => {}
        }
        rewrite_line(line, &resolve, &mut text, &mut out);
    }
    out.text = text;
    out
}

fn rewrite_line(
    line: &str,
    resolve: &impl Fn(&str) -> Option<(String, String)>,
    text: &mut String,
    out: &mut WikiRewrite,
) {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut copied = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'`' => {
                // Skip an inline code span whole.
                let ticks = bytes[i..].iter().take_while(|b| **b == b'`').count();
                let fence = &line[i..i + ticks];
                match line[i + ticks..].find(fence) {
                    Some(close) => i += ticks + close + ticks,
                    None => i += ticks,
                }
                continue;
            }
            b'[' if bytes.get(i + 1) == Some(&b'[') => {
                let inner_start = i + 2;
                let Some(close) = line[inner_start..].find("]]") else {
                    i += 2;
                    continue;
                };
                let inner = &line[inner_start..inner_start + close];
                if inner.is_empty()
                    || inner.contains(['[', ']'])
                    || inner.chars().count() > MAX_WIKI_CHARS
                {
                    i += 2;
                    continue;
                }
                let (name, shown) = match inner.split_once('|') {
                    Some((name, shown)) => (name.trim(), Some(shown.trim())),
                    None => (inner.trim(), None),
                };
                let end = inner_start + close + 2;
                match resolve(name) {
                    Some((id, title)) => {
                        text.push_str(&line[copied..i]);
                        let label = match shown.filter(|s| !s.is_empty()) {
                            Some(shown) => mention::note_label(shown),
                            None => mention::note_label(&title),
                        };
                        text.push_str(&mention_markdown(&label, &id));
                        if !out.linked.contains(&id) {
                            out.linked.push(id);
                        }
                        copied = end;
                    }
                    None => {
                        if !out.unresolved.iter().any(|u| u == name) {
                            out.unresolved.push(name.to_owned());
                        }
                    }
                }
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    text.push_str(&line[copied.min(line.len())..]);
}

/// Turns `[[Title]]` written as text in the blocks `only` accepts into note
/// mentions, the model-level twin of [`rewrite_wiki_links`] for text that
/// has already been parsed (an agent's entry, an edit). Code and existing
/// links are left alone. Returns what was linked and what could not be.
pub fn link_wiki_blocks(
    doc: &mut Document,
    only: impl Fn(&Block) -> bool,
    resolve: impl Fn(&str) -> Option<(String, String)>,
) -> WikiRewrite {
    let mut out = WikiRewrite::default();
    for block in &mut doc.blocks {
        if matches!(block.kind, BlockKind::Code) || block.kind.is_atomic() || !only(block) {
            continue;
        }
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(at) = block.text[from..].find("[[").map(|i| i + from) {
            let inner_start = at + 2;
            let Some(close) = block.text[inner_start..].find("]]") else {
                break;
            };
            let end = inner_start + close + 2;
            let inner = &block.text[inner_start..inner_start + close];
            let covered = block.marks.iter().any(|m| {
                matches!(m.style, Style::Code | Style::Link(_))
                    && m.range.start < end
                    && at < m.range.end
            });
            if inner.is_empty()
                || inner.contains(['[', ']', '\n'])
                || inner.chars().count() > MAX_WIKI_CHARS
                || covered
            {
                from = inner_start;
                continue;
            }
            found.push((at..end, inner.to_owned()));
            from = end;
        }
        for (range, inner) in found.into_iter().rev() {
            let (name, shown) = match inner.split_once('|') {
                Some((name, shown)) => (name.trim(), Some(shown.trim())),
                None => (inner.trim(), None),
            };
            let Some((id, title)) = resolve(name) else {
                if !out.unresolved.iter().any(|u| u == name) {
                    out.unresolved.push(name.to_owned());
                }
                continue;
            };
            let label = mention::note_label(shown.filter(|s| !s.is_empty()).unwrap_or(&title));
            block.replace(range.clone(), &label, &[]);
            block.add_mark(
                range.start..range.start + label.len(),
                Style::Link(MentionTarget::Note(id.clone()).url()),
            );
            if !out.linked.contains(&id) {
                out.linked.push(id);
            }
        }
    }
    out.linked.reverse();
    out
}

/// The one note `name` names for a `[[name]]` link: an exact id, else the
/// one note (unarchived first) whose title is `name`, ignoring case. `None`
/// when no note or several notes match. Returns (id, title).
pub fn wiki_target(notes: &[crate::store::NoteMeta], name: &str) -> Option<(String, String)> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if let Some(note) = notes.iter().find(|n| n.id == name) {
        return Some((note.id.clone(), note.display_title().to_owned()));
    }
    let titled: Vec<&crate::store::NoteMeta> = notes
        .iter()
        .filter(|n| n.title.trim().to_lowercase() == name.to_lowercase())
        .collect();
    let live: Vec<&&crate::store::NoteMeta> = titled.iter().filter(|n| !n.archived).collect();
    let pick = match (live.as_slice(), titled.as_slice()) {
        ([one], _) => **one,
        ([], [one]) => *one,
        _ => return None,
    };
    Some((pick.id.clone(), pick.display_title().to_owned()))
}

/// A note mention as Markdown, escaped the way the store writes it.
pub fn mention_markdown(label: &str, note_id: &str) -> String {
    let mut block = Block::new(0, BlockKind::Paragraph, label);
    block.add_mark(
        0..label.len(),
        Style::Link(MentionTarget::Note(note_id.to_owned()).url()),
    );
    markdown::write_inline(&block, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::parse_note;

    fn doc(md: &str) -> Document {
        parse_note(md).doc
    }

    fn index(notes: &[(&str, &str)]) -> LinkIndex {
        let mut index = LinkIndex::default();
        for (id, md) in notes {
            index.upsert_doc(id, &doc(md));
        }
        index
    }

    #[test]
    fn note_mentions_are_links_and_session_mentions_are_not() {
        let links = NoteLinks::from_doc(
            "a",
            &doc(
                "# Plan\n\nSee [@Release](diri://note/b) and [@Codex](diri://session/s_1).\n\n- [ ] ask [@Release](diri://note/b) [@me](diri://note/a)\n\n```\n[@x](diri://note/c)\n```\n",
            ),
        );
        assert_eq!(links.title, "Plan");
        assert_eq!(links.targets(), ["b"]);
        assert_eq!(links.links.len(), 2, "two links, the self-link dropped");
        let first = &links.links[0];
        assert_eq!(first.block, 0);
        assert_eq!(first.label, "@Release");
        assert_eq!(&first.context[first.mention.clone()], "@Release");
        assert_eq!(first.context, "See @Release and @Codex.");
    }

    #[test]
    fn backlinks_follow_creates_edits_renames_and_deletes() {
        let mut index = index(&[
            ("a", "# Alpha\n\nLinks to [@Beta](diri://note/b).\n"),
            ("b", "# Beta\n\nNothing.\n"),
            (
                "c",
                "# Gamma\n\n[@Beta](diri://note/b) twice: [@B](diri://note/b)\n",
            ),
        ]);
        let sources = |index: &LinkIndex| {
            index
                .backlinks("b")
                .into_iter()
                .map(|r| (r.source_title, r.context[r.mention].to_owned()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            sources(&index),
            [
                ("Alpha".to_owned(), "@Beta".to_owned()),
                ("Gamma".to_owned(), "@Beta".to_owned()),
                ("Gamma".to_owned(), "@B".to_owned())
            ]
        );
        // Renaming the target keeps every link: they name its id.
        assert!(index.upsert_doc("b", &doc("# Beta, renamed\n\nNothing.\n")));
        assert_eq!(index.backlinks("b").len(), 3);
        assert_eq!(index.title("b"), Some("Beta, renamed"));
        // Renaming a source shows its new title.
        index.upsert_doc(
            "a",
            &doc("# Aardvark\n\nLinks to [@Beta](diri://note/b).\n"),
        );
        assert_eq!(sources(&index)[0].0, "Aardvark");
        // An edit that drops the link drops the backlink.
        index.upsert_doc("c", &doc("# Gamma\n\nNo links now.\n"));
        assert_eq!(sources(&index).len(), 1);
        // Saving the same text again changes nothing.
        assert!(!index.upsert_doc("c", &doc("# Gamma\n\nNo links now.\n")));
        // A deleted source takes its backlinks with it.
        assert!(index.remove("a"));
        assert!(index.backlinks("b").is_empty());
        assert!(index.linking_notes("b").is_empty());
        // A new note linking in shows up.
        index.upsert_doc("d", &doc("# Delta\n\n- [ ] read [@Beta](diri://note/b)\n"));
        assert_eq!(index.linking_notes("b"), ["d"]);
        assert!(index.retain(|id| id != "d"));
        assert!(index.backlinks("b").is_empty());
    }

    #[test]
    fn unlinked_mentions_are_whole_words_outside_links() {
        let index = index(&[
            ("p", "# Pricing test\n\nBody.\n"),
            (
                "q",
                "# Weekly\n\nWe ran the pricing test again. [@Pricing test](diri://note/p)\n\nPricing tests are fun; PRICING TEST done.\n",
            ),
            (
                "r",
                "# Other\n\n`pricing test` in code still counts as words.\n",
            ),
        ]);
        let found: Vec<(String, String)> = index
            .unlinked_mentions("p", 10)
            .into_iter()
            .map(|r| (r.source.clone(), r.context[r.mention.clone()].to_owned()))
            .collect();
        assert_eq!(
            found,
            [
                ("r".to_owned(), "pricing test".to_owned()),
                ("q".to_owned(), "pricing test".to_owned()),
                ("q".to_owned(), "PRICING TEST".to_owned()),
            ]
        );
        assert_eq!(index.unlinked_mentions("p", 1).len(), 1);
        let short = self::index(&[("x", "# Go\n\n"), ("y", "# Y\n\nGo go go\n")]);
        assert!(short.unlinked_mentions("x", 10).is_empty());
    }

    #[test]
    fn linking_an_unlinked_mention_keeps_the_words() {
        let index = index(&[
            ("p", "# Pricing test\n\nBody.\n"),
            ("q", "# Weekly\n\nWe ran the pricing test again.\n"),
        ]);
        let found = index.unlinked_mentions("p", 10).remove(0);
        let mut q = parse_note("# Weekly\n\nWe ran the pricing test again.\n");
        assert!(link_mention(
            &mut q.doc,
            found.block,
            found.range.clone(),
            "pricing test",
            "p"
        ));
        assert_eq!(
            q.to_markdown().trim_end(),
            "# Weekly\n\nWe ran the [@pricing test](diri://note/p) again."
        );
        // A second try finds a link there and does nothing.
        assert!(!link_mention(
            &mut q.doc,
            found.block,
            found.range,
            "pricing test",
            "p"
        ));
    }

    #[test]
    fn graphs_are_deterministic_and_neighborhoods_bounded() {
        let index = index(&[
            ("a", "# A\n\n[@B](diri://note/b) [@C](diri://note/c)\n"),
            ("b", "# B\n\n[@A](diri://note/a)\n"),
            (
                "c",
                "# C\n\n[@D](diri://note/d) [@missing](diri://note/zz)\n",
            ),
            ("d", "# D\n"),
            ("e", "# E\n"),
        ]);
        let graph = index.graph();
        let ids: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c", "d", "e"]);
        assert_eq!(
            graph.edges,
            [(0, 1), (0, 2), (2, 3)],
            "a↔b once, no missing node"
        );
        assert_eq!(graph.nodes[0].degree, 2);
        assert_eq!(graph.nodes[4].degree, 0);
        let local = index.neighborhood("c", 1);
        let ids: Vec<&str> = local.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["c", "a", "d"], "center first, both directions");
        assert_eq!(index.neighborhood("c", 2).nodes.len(), 4);
        assert!(index.neighborhood("nope", 2).nodes.is_empty());
    }

    #[test]
    fn context_trims_long_lines_around_the_link() {
        let text = format!("{} LINK {}", "word ".repeat(40), "tail ".repeat(40));
        let at = text.find("LINK").unwrap();
        let (snippet, range) = context(&text, at..at + 4, 20);
        assert_eq!(&snippet[range], "LINK");
        assert!(
            snippet.starts_with('…') && snippet.ends_with('…'),
            "{snippet}"
        );
        assert!(snippet.chars().count() <= 4 + 2 * 20 + 2);
        let (short, range) = context("a\nb LINK", 4..8, 20);
        assert_eq!((short.as_str(), range), ("a b LINK", 4..8));
    }

    #[test]
    fn wiki_links_become_mentions_outside_code() {
        let resolve = |title: &str| {
            (title.to_lowercase() == "release plan")
                .then(|| ("n-1".to_owned(), "Release plan".to_owned()))
        };
        let src = "See [[release plan]] and [[Release plan|the plan]].\n\n`[[release plan]]` stays.\n\n```\n[[release plan]]\n```\n\n[[Nope]] and \\[[release plan]]";
        let out = rewrite_wiki_links(src, resolve);
        assert_eq!(
            out.text,
            "See [@Release plan](diri://note/n-1) and [@the plan](diri://note/n-1).\n\n`[[release plan]]` stays.\n\n```\n[[release plan]]\n```\n\n[[Nope]] and \\[[release plan]]"
        );
        assert_eq!(out.linked, ["n-1"]);
        assert_eq!(out.unresolved, ["Nope"]);
        // What it writes reads back as note links.
        let parsed = doc(&format!("# T\n\n{}", out.text));
        assert_eq!(NoteLinks::from_doc("t", &parsed).targets(), ["n-1"]);
        // Parsed text links the same way, leaving code and other blocks.
        let mut d = doc(
            "# T\n\nRead \\[\\[release plan\\]\\] and `[[release plan]]`.\n\nKeep \\[\\[release plan\\]\\]\n",
        );
        assert_eq!(
            d.blocks[0].text,
            "Read [[release plan]] and [[release plan]]."
        );
        let done = link_wiki_blocks(&mut d, |b| b.text.starts_with("Read"), resolve);
        assert_eq!(done.linked, ["n-1"]);
        assert_eq!(d.blocks[0].text, "Read @Release plan and [[release plan]].");
        assert_eq!(d.blocks[1].text, "Keep [[release plan]]");
        assert_eq!(d.mentions(), [MentionTarget::Note("n-1".into())]);
        // Labels with Markdown punctuation are escaped.
        let md = mention_markdown("@A [weird] *one*", "x");
        let parsed = doc(&format!("# T\n\n{md}\n"));
        assert_eq!(parsed.blocks[0].text, "@A [weird] *one*");
        assert_eq!(parsed.mentions(), [MentionTarget::Note("x".into())]);
    }
}
