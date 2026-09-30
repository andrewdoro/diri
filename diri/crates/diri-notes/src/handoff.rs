//! Handing work from a note to agents, and agents writing back.
//!
//! Everything here is a pure edit of a [`Note`]; callers apply it through
//! [`crate::store::NoteStore::update`] so outside writes never race the
//! editor. Agents only ever add to a note: check a to-do, link a session to
//! it, or append an update. Nothing here deletes user text.

use crate::doc::{Block, BlockKind};
use crate::markdown;
use crate::mentions::{self, MentionTarget};
use crate::store::{Note, append_markdown};

/// Initial prompts carry at most this much of the note; the rest is one
/// `read_note` away.
pub const PROMPT_NOTE_BYTES: usize = 16 * 1024;
pub const UPDATES_HEADING: &str = "Updates";

/// How a caller names a to-do: its block index (as `read_note` reports it)
/// or its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TodoSelector {
    Index(usize),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodoRef {
    /// Block index.
    pub index: usize,
    pub text: String,
    pub checked: bool,
    /// Sessions linked to this to-do, in order.
    pub sessions: Vec<String>,
}

/// Every to-do in document order, with the sessions linked to it.
pub fn todos(note: &Note) -> Vec<TodoRef> {
    note.doc
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            let BlockKind::Todo { checked } = block.kind else {
                return None;
            };
            Some(TodoRef {
                index,
                text: block.text.clone(),
                checked,
                sessions: mentions::block_mentions(block)
                    .filter_map(|(_, target)| match target {
                        MentionTarget::Session(id) => Some(id),
                        MentionTarget::Note(_) => None,
                    })
                    .collect(),
            })
        })
        .collect()
}

/// Resolves a to-do: an exact index, else an exact (case-insensitive) text
/// match, else the single to-do whose text contains `text`.
pub fn find_todo(note: &Note, selector: &TodoSelector) -> Result<usize, String> {
    let all = todos(note);
    match selector {
        TodoSelector::Index(index) => all
            .iter()
            .find(|todo| todo.index == *index)
            .map(|todo| todo.index)
            .ok_or_else(|| format!("block {index} is not a to-do")),
        TodoSelector::Text(text) => {
            let needle = text.trim().to_lowercase();
            if needle.is_empty() {
                return Err("to-do text is empty".into());
            }
            let exact: Vec<&TodoRef> = all
                .iter()
                .filter(|todo| todo.text.trim().to_lowercase() == needle)
                .collect();
            if let [one] = exact.as_slice() {
                return Ok(one.index);
            }
            let partial: Vec<&TodoRef> = all
                .iter()
                .filter(|todo| todo.text.to_lowercase().contains(&needle))
                .collect();
            match partial.as_slice() {
                [one] => Ok(one.index),
                [] => Err(format!("no to-do matches \"{text}\"")),
                many => Err(format!(
                    "\"{text}\" matches {} to-dos (blocks {}); pass an index",
                    many.len(),
                    many.iter()
                        .map(|todo| todo.index.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
    }
}

/// Sets a to-do's checkbox. Returns whether anything changed.
pub fn set_checked(note: &mut Note, index: usize, checked: bool) -> bool {
    let Some(block) = note.doc.blocks.get_mut(index) else {
        return false;
    };
    if !matches!(block.kind, BlockKind::Todo { .. }) || block.kind == (BlockKind::Todo { checked })
    {
        return false;
    }
    block.kind = BlockKind::Todo { checked };
    true
}

/// Links a session to a to-do by appending its mention. Returns whether
/// anything changed (a session is linked at most once).
pub fn link_session(note: &mut Note, index: usize, label: &str, session_id: &str) -> bool {
    match note.doc.blocks.get_mut(index) {
        Some(block) if matches!(block.kind, BlockKind::Todo { .. }) => {
            mentions::append(block, label, &MentionTarget::Session(session_id.to_owned()))
        }
        _ => false,
    }
}

/// Appends a dated, attributed line under the note's `## Updates` heading,
/// creating the heading at the end of the note when it is missing. Lines
/// already under the heading stay in order; new ones go last.
pub fn append_update(note: &mut Note, date: &str, label: &str, session_id: &str, text: &str) {
    let has_heading = note.doc.blocks.iter().any(|block| {
        matches!(block.kind, BlockKind::Heading(_)) && block.text.trim() == UPDATES_HEADING
    });
    if !has_heading {
        append_markdown(note, &format!("## {UPDATES_HEADING}"));
    }
    let author = mentions::markdown_link(label, &MentionTarget::Session(session_id.to_owned()));
    let body = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut line = Block::new(0, BlockKind::Bullet, "");
    line.text = format!("{date} {body}");
    let escaped = markdown::write_inline(&line, false);
    append_markdown(note, &format!("- {author} {escaped}"));
}

/// A session a note mentions, as the handoff prompt describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Related {
    pub session_id: String,
    pub kind: String,
    pub title: String,
    pub status: String,
}

/// The initial prompt for an agent started from `note` (and optionally one
/// of its to-dos). It names the note so the agent can re-read it, carries
/// the note itself up to [`PROMPT_NOTE_BYTES`], and lists the sessions the
/// note mentions so the agent can coordinate through its Diri tools.
pub fn prompt(
    note_id: &str,
    note: &Note,
    todo: Option<usize>,
    related: &[Related],
    extra: Option<&str>,
) -> String {
    let title = if note.doc.title.trim().is_empty() {
        "Untitled"
    } else {
        note.doc.title.trim()
    };
    let mut out = String::new();
    match todo.and_then(|index| note.doc.blocks.get(index)) {
        Some(block) => {
            out.push_str(&format!(
                "Your task is this to-do from the Diri note \"{title}\":\n\n{}\n",
                plain_line(block)
            ));
        }
        None => out.push_str(&format!(
            "Your task is to carry out the Diri note \"{title}\".\n"
        )),
    }
    if let Some(extra) = extra.map(str::trim).filter(|extra| !extra.is_empty()) {
        out.push_str(&format!("\n{extra}\n"));
    }

    let body = note.to_markdown();
    let body = strip_front_matter(&body);
    out.push_str(&format!("\n--- note {note_id} ---\n"));
    if body.len() > PROMPT_NOTE_BYTES {
        let cut = crate::doc::floor_boundary(body, PROMPT_NOTE_BYTES);
        out.push_str(&body[..cut]);
        out.push_str(&format!(
            "\n[… note truncated; read the rest with the read_note tool, note \"{note_id}\"]\n"
        ));
    } else {
        out.push_str(body);
    }
    out.push_str("--- end of note ---\n");

    if !related.is_empty() {
        out.push_str(
            "\nThe note mentions these Diri sessions. They may be working on related things; \
             inspect them with read_output, get_diff or get_status, or wait on them with \
             wait_for_agent, before you duplicate their work:\n",
        );
        for session in related {
            out.push_str(&format!(
                "- {} ({}, {}): {}\n",
                session.session_id, session.kind, session.status, session.title
            ));
        }
    }
    out.push_str(&format!(
        "\nThis note is your parent in Diri. Re-read it any time with read_note (note \"{note_id}\" \
         or \"origin\"). Post progress and your final result with report_to_parent; reports \
         are added to the note's Updates section. Check off a finished to-do with write_note.\n"
    ));
    out
}

fn plain_line(block: &Block) -> String {
    let marker = match block.kind {
        BlockKind::Todo { checked: true } => "- [x] ",
        BlockKind::Todo { checked: false } => "- [ ] ",
        _ => "",
    };
    format!("{marker}{}", markdown::write_inline(block, false))
}

fn strip_front_matter(source: &str) -> &str {
    let Some(rest) = source.strip_prefix("---\n") else {
        return source;
    };
    match rest.find("\n---\n") {
        Some(end) => rest[end + "\n---\n".len()..].trim_start_matches('\n'),
        None => source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::parse_note;

    fn note(source: &str) -> Note {
        parse_note(source)
    }

    const PRD: &str = "---\nid: n1\nproject: /work/diri\n---\n# Resize PRD\n\n\
        Coordinate with [@Codex: fix resize](diri://session/s_other).\n\n\
        - [ ] Fix resize flicker\n- [x] Write repro\n- [ ] Fix resize tests\n";

    #[test]
    fn finds_todos_by_index_and_text() {
        let note = note(PRD);
        let all = todos(&note);
        assert_eq!(all.len(), 3);
        let flicker = all[0].index;
        assert_eq!(find_todo(&note, &TodoSelector::Index(flicker)), Ok(flicker));
        assert_eq!(
            find_todo(&note, &TodoSelector::Text("fix resize FLICKER".into())),
            Ok(flicker)
        );
        assert!(
            find_todo(&note, &TodoSelector::Text("fix resize".into()))
                .unwrap_err()
                .contains("matches 2")
        );
        assert!(find_todo(&note, &TodoSelector::Index(0)).is_err());
    }

    #[test]
    fn links_and_checks_survive_a_round_trip() {
        let mut note = note(PRD);
        let index = find_todo(&note, &TodoSelector::Text("flicker".into())).unwrap();
        assert!(link_session(&mut note, index, "Claude: flicker", "s_child"));
        assert!(!link_session(&mut note, index, "again", "s_child"));
        assert!(set_checked(&mut note, index, true));
        assert!(!set_checked(&mut note, index, true));

        let back = parse_note(&note.to_markdown());
        let todo = &todos(&back)[0];
        assert!(todo.checked);
        assert_eq!(todo.sessions, vec!["s_child".to_owned()]);
        assert!(
            back.to_markdown()
                .contains("- [x] Fix resize flicker [@Claude: flicker](diri://session/s_child)")
        );
    }

    #[test]
    fn updates_collect_under_one_heading() {
        let mut note = note(PRD);
        append_update(
            &mut note,
            "2026-09-30 14:02",
            "Claude",
            "s_c",
            "Found *the* cause.\nFixing.",
        );
        append_update(
            &mut note,
            "2026-09-30 15:10",
            "Claude",
            "s_c",
            "Done: PR #600",
        );
        let source = note.to_markdown();
        assert_eq!(source.matches("## Updates").count(), 1, "{source}");
        let first = source
            .find("Found \\*the\\* cause. Fixing.")
            .expect(&source);
        let second = source.find("Done: PR #600").expect(&source);
        assert!(first < second);
        assert!(source.contains("- [@Claude](diri://session/s_c) 2026-09-30 14:02 Found"));
    }

    #[test]
    fn prompt_names_the_todo_note_and_related_sessions() {
        let note = note(PRD);
        let index = find_todo(&note, &TodoSelector::Text("flicker".into())).unwrap();
        let related = [Related {
            session_id: "s_other".into(),
            kind: "codex".into(),
            title: "fix resize".into(),
            status: "working".into(),
        }];
        let text = prompt("n1", &note, Some(index), &related, Some("Use a worktree."));
        assert!(text.starts_with(
            "Your task is this to-do from the Diri note \"Resize PRD\":\n\n- [ ] Fix resize flicker\n"
        ));
        assert!(text.contains("Use a worktree."));
        assert!(text.contains("--- note n1 ---\n# Resize PRD"));
        assert!(
            !text.contains("project: /work/diri"),
            "front matter stays out"
        );
        assert!(text.contains("- s_other (codex, working): fix resize"));
        assert!(text.contains("report_to_parent"));
    }

    #[test]
    fn prompt_truncates_long_notes() {
        let long = format!("# Big\n\n{}\n", "word ".repeat(10_000));
        let text = prompt("big", &note(&long), None, &[], None);
        assert!(text.contains("note truncated"));
        assert!(text.len() < PROMPT_NOTE_BYTES + 2048);
    }
}
