//! Whole-block operations behind the block handle (the ⋮⋮ grip beside a
//! block): drag it to reorder, or open its menu to turn it into another kind,
//! duplicate it, delete it, or add a block below.
//!
//! A block moves with what belongs to it: a list item with the items nested
//! under it, a table cell with its whole table. Each operation is one undo
//! step and leaves the caret in the block it acted on.

use std::ops::Range;

use super::{EditKind, Editor, Pos, Selection, Turn};
use crate::doc::{Block, BlockKind};

impl Editor {
    /// The blocks that move with block `index`: a list item and its nested
    /// items, a table whole, else just the block. Empty for the title.
    pub fn block_span(&self, index: usize) -> Range<usize> {
        if index == 0 || index >= self.blocks.len() {
            return index..index;
        }
        if let Some(table) = self.table_pos(index) {
            return table.range;
        }
        index..self.children(index).end.max(index + 1)
    }

    /// Moves block `from` (with its span) to where block `to` is: above it
    /// when moving up, below it (and its span) when moving down, as a list
    /// reorders under a dragged row. Returns the moved block's new index, or
    /// `None` when nothing moved (onto itself, into its own span, the title).
    pub fn move_block_to(&mut self, from: usize, to: usize, now_ms: u64) -> Option<usize> {
        let span = self.block_span(from);
        if span.is_empty() || to == 0 || to >= self.blocks.len() || span.contains(&to) {
            return None;
        }
        // Never drop into the middle of a table or a list item's children.
        let target = self.block_span(to);
        let insert = if to < span.start {
            target.start
        } else {
            target.end
        };
        if insert == span.start || insert == span.end {
            return None;
        }
        self.checkpoint(EditKind::Other, now_ms);
        let moved: Vec<Block> = self.blocks.drain(span.clone()).collect();
        let len = moved.len();
        let at = if insert > span.start {
            insert - len
        } else {
            insert
        };
        self.blocks.splice(at..at, moved);
        self.fit_indent(at..at + len);
        let end = self.blocks[at].text.len();
        self.selection = Selection::caret(Pos::new(at, end));
        self.pending = None;
        self.changed();
        let selection = self.selection;
        self.set_selection(selection);
        Some(at)
    }

    /// A list item may sit at most one level deeper than the list item
    /// before it (and at the top level after anything else): a moved subtree
    /// shifts out as a whole until it fits.
    fn fit_indent(&mut self, range: Range<usize>) {
        let first = &self.blocks[range.start];
        if !first.kind.is_list() || first.indent == 0 {
            return;
        }
        let allowed = match self.blocks.get(range.start.wrapping_sub(1)) {
            Some(prev) if range.start > 1 && prev.kind.is_list() => prev.indent + 1,
            _ => 0,
        };
        let shift = first.indent.saturating_sub(allowed);
        if shift == 0 {
            return;
        }
        for block in &mut self.blocks[range] {
            if block.kind.is_list() {
                block.indent = block.indent.saturating_sub(shift);
            }
        }
    }

    /// Copies block `index` (with its span) right below it. Returns the
    /// copy's index.
    pub fn duplicate_block(&mut self, index: usize, now_ms: u64) -> Option<usize> {
        let span = self.block_span(index);
        if span.is_empty() {
            return None;
        }
        self.checkpoint(EditKind::Other, now_ms);
        let mut copies: Vec<Block> = self.blocks[span.clone()].to_vec();
        for block in &mut copies {
            block.id = self.fresh_id();
        }
        let at = span.end;
        self.blocks.splice(at..at, copies);
        let end = self.blocks[at].text.len();
        self.selection = Selection::caret(Pos::new(at, end));
        self.pending = None;
        self.changed();
        let selection = self.selection;
        self.set_selection(selection);
        Some(at)
    }

    /// Removes block `index` with its span; the caret goes to the end of
    /// the block before it.
    pub fn delete_block(&mut self, index: usize, now_ms: u64) -> bool {
        let span = self.block_span(index);
        if span.is_empty() {
            return false;
        }
        self.checkpoint(EditKind::Other, now_ms);
        self.blocks.drain(span.clone());
        // A nested item whose parent went shifts out to fit.
        if span.start < self.blocks.len() {
            let mut end = span.start + 1;
            while self
                .blocks
                .get(end)
                .is_some_and(|b| b.kind.is_list() && b.indent > self.blocks[span.start].indent)
            {
                end += 1;
            }
            self.fit_indent(span.start..end);
        }
        let before = span.start - 1;
        let end = self.blocks[before].text.len();
        self.selection = Selection::caret(Pos::new(before, end));
        self.pending = None;
        self.changed();
        let selection = self.selection;
        self.set_selection(selection);
        true
    }

    /// Turns block `index` into `turn`, as the slash menu does for the
    /// caret's block.
    pub fn turn_block(&mut self, index: usize, turn: Turn, now_ms: u64) {
        if index == 0 || index >= self.blocks.len() || self.blocks[index].kind.is_cell() {
            return;
        }
        let end = self.blocks[index].text.len();
        self.set_selection(Selection::caret(Pos::new(index, end)));
        self.turn_into(turn, now_ms);
    }

    /// Adds an empty paragraph below block `index` (and its span) and puts
    /// the caret in it. Returns its index.
    pub fn insert_block_below(&mut self, index: usize, now_ms: u64) -> Option<usize> {
        let span = self.block_span(index);
        if span.is_empty() {
            return None;
        }
        self.checkpoint(EditKind::Other, now_ms);
        let id = self.fresh_id();
        self.blocks
            .insert(span.end, Block::new(id, BlockKind::Paragraph, ""));
        self.selection = Selection::caret(Pos::new(span.end, 0));
        self.pending = None;
        self.changed();
        Some(span.end)
    }
}

#[cfg(test)]
mod tests {
    use crate::doc::BlockKind;
    use crate::edit::{Editor, Turn};
    use crate::markdown;

    fn editor(md: &str) -> Editor {
        Editor::new(&markdown::parse(md).1)
    }

    fn body(editor: &Editor) -> String {
        let text = markdown::write(&markdown::FrontMatter::default(), &editor.document());
        text.split_once("\n\n")
            .map_or(String::new(), |(_, body)| body.trim_end().to_owned())
    }

    #[test]
    fn moving_a_block_down_and_back_round_trips() {
        let source = "# T\n\nOne\n\nTwo\n\nThree\n";
        let mut e = editor(source);
        // Blocks: 0 title, 1 One, 2 Two, 3 Three, 4 trailing.
        assert_eq!(e.move_block_to(1, 3, 0), Some(3));
        assert_eq!(body(&e), "Two\n\nThree\n\nOne");
        assert_eq!(e.selection.head.block, 3, "the caret follows the block");
        assert_eq!(e.move_block_to(3, 1, 0), Some(1));
        assert_eq!(body(&e), "One\n\nTwo\n\nThree");
        let (_, original) = markdown::parse(source);
        assert!(e.document().same_content(&original));
        // Undo walks the moves back one at a time.
        assert!(e.undo());
        assert_eq!(body(&e), "Two\n\nThree\n\nOne");
    }

    #[test]
    fn a_list_item_moves_with_its_children_and_tables_move_whole() {
        let mut e = editor("# T\n\n- a\n  - a1\n  - a2\n- b\n\nPara\n");
        // 1 a, 2 a1, 3 a2, 4 b, 5 Para.
        assert_eq!(e.block_span(1), 1..4);
        assert_eq!(e.move_block_to(1, 5, 0), Some(3));
        assert_eq!(body(&e), "- b\n\nPara\n\n- a\n  - a1\n  - a2");
        // Into its own children: nothing.
        assert_eq!(e.move_block_to(3, 4, 0), None);
        let mut t = editor("# T\n\nAbove\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\nBelow\n");
        // 1 Above, 2..6 cells, 6 Below.
        assert_eq!(t.block_span(4), 2..6);
        assert_eq!(t.move_block_to(1, 5, 0), Some(5));
        assert!(body(&t).starts_with("| a "), "{}", body(&t));
        assert!(body(&t).ends_with("Above\n\nBelow"), "{}", body(&t));
    }

    #[test]
    fn a_nested_item_moved_out_of_its_list_shifts_left() {
        let mut e = editor("# T\n\n- a\n  - a1\n\nPara\n");
        assert_eq!(e.move_block_to(2, 3, 0), Some(3));
        assert_eq!(body(&e), "- a\n\nPara\n\n- a1");
    }

    #[test]
    fn handle_menu_duplicate_delete_turn_and_add_below() {
        let mut e = editor("# T\n\n- [ ] ship\n  - [ ] notes\n\nEnd\n");
        assert_eq!(e.duplicate_block(1, 0), Some(3));
        assert_eq!(
            body(&e),
            "- [ ] ship\n  - [ ] notes\n- [ ] ship\n  - [ ] notes\n\nEnd"
        );
        let ids: std::collections::HashSet<u64> = e.blocks().iter().map(|b| b.id).collect();
        assert_eq!(ids.len(), e.blocks().len(), "copies get fresh ids");
        assert!(e.delete_block(3, 0));
        assert_eq!(body(&e), "- [ ] ship\n  - [ ] notes\n\nEnd");
        // The title is the note's only `#`: a top heading is written `##`.
        e.turn_block(3, Turn::Kind(BlockKind::Heading(1)), 0);
        assert_eq!(body(&e), "- [ ] ship\n  - [ ] notes\n\n## End");
        assert_eq!(e.insert_block_below(1, 0), Some(3));
        assert_eq!(e.block(3).kind, BlockKind::Paragraph);
        assert_eq!(e.selection.head.block, 3);
        assert!(!e.delete_block(0, 0), "the title stays");
        // A list item goes with the items nested under it.
        let mut p = editor("# T\n\nIntro\n\n- a\n  - a1\n");
        assert!(p.delete_block(2, 0));
        assert_eq!(body(&p), "Intro");
    }
}
