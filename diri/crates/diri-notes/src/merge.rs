//! Three-way merge of a note: the person's unsaved typing with a write made
//! outside the editor (an agent appending, the CLI ticking a to-do).
//!
//! Blocks are the unit. Stretches of the note that only one side changed take
//! that side; stretches both changed keep the person's version and add the
//! blocks the outside write inserted after it. A block both sides edited keeps
//! the person's text with the outside checkbox, or with a suffix the outside
//! write appended (a linked session chip). Nothing the outside write did is
//! ever lost: when a change cannot be combined, the person's text wins in the
//! note and the outside change stays in version history.

use crate::doc::{Block, BlockKind, Document};

/// A merged document and where each of the person's blocks went.
#[derive(Clone, Debug)]
pub struct Merged {
    pub doc: Document,
    /// For each block of `mine` (by index), its index in `doc`, if it
    /// survived the merge.
    pub mine_to_merged: Vec<Option<usize>>,
}

/// Merges `mine` (the editor) and `theirs` (the file now) against `base`
/// (the file the editor last loaded or saved).
pub fn merge3(base: &Document, mine: &Document, theirs: &Document) -> Merged {
    let base_blocks = content(&base.blocks);
    let their_blocks = content(&theirs.blocks);
    // The editor always keeps an empty paragraph to type into at the end;
    // it is not content, so it is merged as if absent.
    let mine_len = content(&mine.blocks).len();
    let mine_blocks = &mine.blocks[..mine_len];

    let to_mine = matching(base_blocks, mine_blocks);
    let to_theirs = matching(base_blocks, their_blocks);

    let mut out: Vec<Block> = Vec::new();
    let mut mine_to_merged = vec![None; mine.blocks.len()];
    let (mut b, mut m, mut t) = (0, 0, 0);
    loop {
        // The next base block both sides kept unchanged anchors a chunk.
        let stable = (b..base_blocks.len()).find(|&j| to_mine[j].is_some() && to_theirs[j].is_some());
        let (b_end, m_end, t_end) = match stable {
            Some(j) => (j, to_mine[j].unwrap(), to_theirs[j].unwrap()),
            None => (base_blocks.len(), mine_blocks.len(), their_blocks.len()),
        };
        merge_chunk(
            &base_blocks[b..b_end],
            (m, &mine_blocks[m..m_end]),
            &their_blocks[t..t_end],
            &mut out,
            &mut mine_to_merged,
        );
        let Some(j) = stable else { break };
        mine_to_merged[m_end] = Some(out.len());
        out.push(mine_blocks[m_end].clone());
        (b, m, t) = (j + 1, m_end + 1, to_theirs[j].unwrap() + 1);
    }
    let title = if mine.title == base.title {
        theirs.title.clone()
    } else {
        mine.title.clone()
    };
    Merged {
        doc: Document::new(title, out),
        mine_to_merged,
    }
}

fn merge_chunk(
    base: &[Block],
    (mine_start, mine): (usize, &[Block]),
    theirs: &[Block],
    out: &mut Vec<Block>,
    mine_to_merged: &mut [Option<usize>],
) {
    let mut push_mine = |block: Block, index: usize, out: &mut Vec<Block>| {
        mine_to_merged[mine_start + index] = Some(out.len());
        out.push(block);
    };
    if same(mine, base) {
        out.extend(theirs.iter().cloned());
        return;
    }
    if same(theirs, base) || same(theirs, mine) {
        for (i, block) in mine.iter().enumerate() {
            push_mine(block.clone(), i, out);
        }
        return;
    }
    if mine.len() == base.len() && theirs.len() == base.len() {
        for (i, ((b, m), t)) in base.iter().zip(mine).zip(theirs).enumerate() {
            push_mine(merge_block(b, m, t), i, out);
        }
        return;
    }
    // Both changed the stretch in different shapes: keep the person's
    // version, then what the outside write added that was not there before.
    for (i, block) in mine.iter().enumerate() {
        push_mine(block.clone(), i, out);
    }
    for block in theirs {
        if !base.iter().any(|b| equal(b, block)) && !mine.iter().any(|m| equal(m, block)) {
            out.push(block.clone());
        }
    }
}

/// One block both sides changed.
fn merge_block(base: &Block, mine: &Block, theirs: &Block) -> Block {
    let mut merged = mine.clone();
    if mine.kind == base.kind {
        merged.kind = theirs.kind;
    }
    if mine.indent == base.indent {
        merged.indent = theirs.indent;
    }
    if mine.text == base.text && mine.marks == base.marks {
        merged.text = theirs.text.clone();
        merged.marks = theirs.marks.clone();
    } else if theirs.text.len() > base.text.len()
        && theirs.text.starts_with(&base.text)
        && theirs.marks.iter().all(|mark| {
            mark.range.start >= base.text.len() || base.marks.contains(mark)
        })
    {
        // The outside write appended to this block (a session chip on a
        // to-do): carry the appended tail onto the person's text.
        let shift = merged.text.len();
        let start = base.text.len();
        merged.text.push_str(&theirs.text[start..]);
        for mark in theirs.marks.iter().filter(|mark| mark.range.start >= start) {
            let range = mark.range.start - start + shift..mark.range.end - start + shift;
            merged.add_mark(range, mark.style.clone());
        }
    }
    merged
}

/// Blocks without the editor's trailing empty paragraphs.
fn content(blocks: &[Block]) -> &[Block] {
    let mut end = blocks.len();
    while end > 0 && blocks[end - 1].kind == BlockKind::Paragraph && blocks[end - 1].text.is_empty() {
        end -= 1;
    }
    &blocks[..end]
}

fn equal(a: &Block, b: &Block) -> bool {
    a.kind == b.kind && a.indent == b.indent && a.text == b.text && a.marks == b.marks
}

fn same(a: &[Block], b: &[Block]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
}

/// For each block of `base`, the index of the block it matches in `other`
/// (a longest common subsequence). Shared prefixes and suffixes are matched
/// directly so typical notes never reach the quadratic middle.
fn matching(base: &[Block], other: &[Block]) -> Vec<Option<usize>> {
    let mut result = vec![None; base.len()];
    let mut prefix = 0;
    while prefix < base.len() && prefix < other.len() && equal(&base[prefix], &other[prefix]) {
        result[prefix] = Some(prefix);
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < base.len() - prefix
        && suffix < other.len() - prefix
        && equal(&base[base.len() - 1 - suffix], &other[other.len() - 1 - suffix])
    {
        result[base.len() - 1 - suffix] = Some(other.len() - 1 - suffix);
        suffix += 1;
    }
    let a = &base[prefix..base.len() - suffix];
    let b = &other[prefix..other.len() - suffix];
    // Beyond this the middle is matched greedily rather than optimally.
    const MAX_CELLS: usize = 4_000_000;
    if a.len().saturating_mul(b.len()) > MAX_CELLS {
        let mut next = 0;
        for (i, block) in a.iter().enumerate() {
            if let Some(found) = (next..b.len()).find(|&j| equal(block, &b[j])) {
                result[prefix + i] = Some(prefix + found);
                next = found + 1;
            }
        }
        return result;
    }
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[at(i, j)] = if equal(&a[i], &b[j]) {
                lcs[at(i + 1, j + 1)] + 1
            } else {
                lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if equal(&a[i], &b[j]) {
            result[prefix + i] = Some(prefix + j);
            i += 1;
            j += 1;
        } else if lcs[at(i + 1, j)] >= lcs[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown;

    fn doc(source: &str) -> Document {
        markdown::parse(source).1
    }

    fn text(merged: &Merged) -> String {
        markdown::write(&Default::default(), &merged.doc)
    }

    const BASE: &str = "# Launch\n\nIntro.\n\n- [ ] call the venue\n- [ ] book flights\n";

    #[test]
    fn an_outside_append_lands_after_the_persons_typing() {
        let mine = doc("# Launch\n\nIntro, now longer.\n\n- [ ] call the venue\n- [ ] book flights\n\nA new thought\n");
        let theirs = doc(&format!("{BASE}\n## Updates\n\n- agent: venue confirmed\n"));
        let merged = merge3(&doc(BASE), &mine, &theirs);
        assert_eq!(
            text(&merged),
            "# Launch\n\nIntro, now longer.\n\n- [ ] call the venue\n- [ ] book flights\n\nA new thought\n\n## Updates\n\n- agent: venue confirmed\n"
        );
    }

    #[test]
    fn a_tick_and_a_chip_combine_with_typing_in_the_same_todo() {
        let mine = doc("# Launch\n\nIntro.\n\n- [ ] call the venue today\n- [ ] book flights\n");
        let theirs = doc("# Launch\n\nIntro.\n\n- [x] call the venue [@Codex](diri://session/s_1)\n- [ ] book flights\n");
        let merged = merge3(&doc(BASE), &mine, &theirs);
        assert_eq!(
            text(&merged),
            "# Launch\n\nIntro.\n\n- [x] call the venue today [@Codex](diri://session/s_1)\n- [ ] book flights\n"
        );
    }

    #[test]
    fn one_sided_changes_pass_through_and_blocks_are_mapped() {
        let mine = doc("# Launch\n\nIntro.\n\nMine\n\n- [ ] call the venue\n- [ ] book flights\n");
        let theirs = doc("# Launch\n\nIntro.\n\n- [ ] call the venue\n- [x] book flights\n");
        let merged = merge3(&doc(BASE), &mine, &theirs);
        assert_eq!(
            text(&merged),
            "# Launch\n\nIntro.\n\nMine\n\n- [ ] call the venue\n- [x] book flights\n"
        );
        // "Mine" is mine[1] and stays at merged[1]; the ticked to-do is
        // theirs, so mine's unticked copy maps nowhere.
        assert_eq!(merged.mine_to_merged[1], Some(1));
        assert_eq!(merged.mine_to_merged[3], None);
    }

    #[test]
    fn the_persons_text_wins_a_real_conflict() {
        let mine = doc("# Launch\n\nMy intro.\n\n- [ ] call the venue\n- [ ] book flights\n");
        let theirs = doc("# Launch\n\nTheir intro.\n\n- [ ] call the venue\n- [ ] book flights\n");
        let merged = merge3(&doc(BASE), &mine, &theirs);
        assert!(text(&merged).contains("My intro."));
        assert!(!text(&merged).contains("Their intro."));
    }

    #[test]
    fn the_trailing_empty_paragraph_is_not_content() {
        let mut mine = doc(BASE);
        mine.blocks.push(Block::new(0, BlockKind::Paragraph, ""));
        let theirs = doc(&format!("{BASE}\n- [ ] print badges\n"));
        let merged = merge3(&doc(BASE), &mine, &theirs);
        assert_eq!(text(&merged), format!("{BASE}- [ ] print badges\n"));
    }
}
