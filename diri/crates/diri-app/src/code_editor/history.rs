//! Undo and redo as recorded edits rather than whole-text snapshots, so a
//! long session on a 2 MB file costs what was typed, not copies of the file.

use std::ops::Range;
use std::time::{Duration, Instant};

use super::buffer::Buffer;
use super::cursor::Selection;

/// Transactions kept before the oldest are dropped.
const DEPTH: usize = 1_000;
/// Typing within this long of the last keystroke joins its undo step.
const TYPING_GROUP: Duration = Duration::from_millis(900);

/// One edit as it happened: what `before` covered and what replaced it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Change {
    /// The replaced range, in the text before the step.
    before: Range<usize>,
    /// Where the replacement sits in the text after the step.
    after: Range<usize>,
    removed: String,
    inserted: String,
}

/// Edits applied at once (one per cursor).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Step {
    changes: Vec<Change>,
}

#[derive(Clone, Debug)]
struct Transaction {
    id: u64,
    steps: Vec<Step>,
    selections_before: Vec<Selection>,
    selections_after: Vec<Selection>,
    typing: bool,
    sealed: bool,
    at: Instant,
}

/// What an undo or redo hands back: the selections to restore.
pub(crate) type Restored = Vec<Selection>;

#[derive(Debug, Default)]
pub(crate) struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    next_id: u64,
    saved: u64,
    /// While an input method composes, every edit joins one transaction.
    grouping: bool,
    group: Option<u64>,
}

impl History {
    /// Applies `edits` (non-overlapping, any order) to `buffer` and records
    /// them as one step. Typing joins the previous typing transaction when
    /// it continues from where that one left the cursors.
    pub(crate) fn apply(
        &mut self,
        buffer: &mut Buffer,
        edits: &[(Range<usize>, String)],
        selections_before: &[Selection],
        typing: bool,
        now: Instant,
    ) -> bool {
        let step = apply_step(buffer, edits);
        if step.changes.is_empty() {
            return false;
        }
        self.redo.clear();
        let grouped = self.grouping
            && self.group.is_some()
            && self.undo.last().map(|last| last.id) == self.group;
        let joins = grouped
            || typing
                && self.undo.last().is_some_and(|last| {
                    last.typing
                        && !last.sealed
                        && last.selections_after == selections_before
                        && now.saturating_duration_since(last.at) < TYPING_GROUP
                        && !step
                            .changes
                            .iter()
                            .any(|change| change.inserted.contains('\n'))
                });
        if joins {
            let last = self.undo.last_mut().expect("joins an existing transaction");
            last.steps.push(step);
            last.at = now;
        } else {
            self.next_id += 1;
            self.undo.push(Transaction {
                id: self.next_id,
                steps: vec![step],
                selections_before: selections_before.to_vec(),
                selections_after: Vec::new(),
                typing,
                sealed: false,
                at: now,
            });
            if self.undo.len() > DEPTH {
                self.undo.remove(0);
            }
            if self.grouping {
                self.group = Some(self.next_id);
            }
        }
        true
    }

    /// Edits until [`History::end_group`] become one undo step.
    pub(crate) fn begin_group(&mut self) {
        self.grouping = true;
        self.group = None;
    }

    pub(crate) fn end_group(&mut self) {
        self.grouping = false;
        self.group = None;
        self.seal();
    }

    /// Records where the cursors ended after the latest transaction.
    pub(crate) fn set_selections_after(&mut self, selections: &[Selection]) {
        if let Some(last) = self.undo.last_mut() {
            last.selections_after = selections.to_vec();
        }
    }

    /// Stops the latest transaction from absorbing further typing.
    pub(crate) fn seal(&mut self) {
        if let Some(last) = self.undo.last_mut() {
            last.sealed = true;
        }
    }

    pub(crate) fn undo(&mut self, buffer: &mut Buffer) -> Option<Restored> {
        let transaction = self.undo.pop()?;
        for step in transaction.steps.iter().rev() {
            let mut changes: Vec<&Change> = step.changes.iter().collect();
            changes.sort_by_key(|change| std::cmp::Reverse(change.after.start));
            for change in changes {
                buffer.replace(change.after.clone(), &change.removed);
            }
        }
        let restored = transaction.selections_before.clone();
        self.redo.push(transaction);
        Some(restored)
    }

    pub(crate) fn redo(&mut self, buffer: &mut Buffer) -> Option<Restored> {
        let mut transaction = self.redo.pop()?;
        for step in &transaction.steps {
            let mut changes: Vec<&Change> = step.changes.iter().collect();
            changes.sort_by_key(|change| std::cmp::Reverse(change.before.start));
            for change in changes {
                buffer.replace(change.before.clone(), &change.inserted);
            }
        }
        transaction.sealed = true;
        let restored = transaction.selections_after.clone();
        self.undo.push(transaction);
        Some(restored)
    }

    /// The text is as it was when last marked saved.
    pub(crate) fn is_clean(&self) -> bool {
        self.undo.last().map_or(0, |last| last.id) == self.saved
    }

    pub(crate) fn mark_saved(&mut self) {
        self.saved = self.undo.last().map_or(0, |last| last.id);
        self.seal();
    }

    #[cfg(test)]
    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
}

/// Applies non-overlapping edits at once, back to front so earlier offsets
/// hold, and returns them as a step. No-op edits are dropped.
fn apply_step(buffer: &mut Buffer, edits: &[(Range<usize>, String)]) -> Step {
    let mut sorted: Vec<&(Range<usize>, String)> = edits
        .iter()
        .filter(|(range, text)| buffer.text()[range.clone()] != *text)
        .collect();
    sorted.sort_by_key(|(range, _)| range.start);
    assert!(
        sorted
            .windows(2)
            .all(|pair| pair[0].0.end <= pair[1].0.start),
        "edits do not overlap"
    );
    let mut shift: isize = 0;
    let mut changes = Vec::with_capacity(sorted.len());
    for (range, text) in &sorted {
        let start = (range.start as isize + shift) as usize;
        changes.push(Change {
            before: range.clone(),
            after: start..start + text.len(),
            removed: buffer.text()[range.clone()].to_string(),
            inserted: text.clone(),
        });
        shift += text.len() as isize - range.len() as isize;
    }
    for (range, text) in sorted.iter().rev() {
        buffer.replace(range.clone(), text);
    }
    Step { changes }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64, start: Instant) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn undo_and_redo_restore_multi_cursor_edits_exactly() {
        let start = Instant::now();
        let mut buffer = Buffer::new("a1\nb2\nc3");
        let mut history = History::default();
        let cursors = vec![Selection::caret(1), Selection::caret(4)];
        history.apply(
            &mut buffer,
            &[(4..5, "XYZ".into()), (1..2, String::new())],
            &cursors,
            false,
            start,
        );
        assert_eq!(buffer.text(), "a\nbXYZ\nc3");
        history.set_selections_after(&[Selection::caret(9)]);
        assert_eq!(history.undo(&mut buffer), Some(cursors.clone()));
        assert_eq!(buffer.text(), "a1\nb2\nc3");
        assert_eq!(history.redo(&mut buffer), Some(vec![Selection::caret(9)]));
        assert_eq!(buffer.text(), "a\nbXYZ\nc3");
        assert!(history.redo(&mut buffer).is_none());
    }

    #[test]
    fn typing_bursts_group_until_a_pause_a_newline_or_a_save() {
        let start = Instant::now();
        let mut buffer = Buffer::new("");
        let mut history = History::default();
        let mut cursor = vec![Selection::caret(0)];
        for (ix, ch) in ["h", "i", "!"].iter().enumerate() {
            let at_ms = at(ix as u64 * 100, start);
            history.apply(
                &mut buffer,
                &[(ix..ix, ch.to_string())],
                &cursor,
                true,
                at_ms,
            );
            cursor = vec![Selection::caret(ix + 1)];
            history.set_selections_after(&cursor);
        }
        assert_eq!(buffer.text(), "hi!");
        history.undo(&mut buffer);
        assert_eq!(buffer.text(), "", "one burst is one step");
        history.redo(&mut buffer);

        // A pause starts a new step.
        history.apply(
            &mut buffer,
            &[(3..3, "?".into())],
            &cursor,
            true,
            at(5_000, start),
        );
        history.set_selections_after(&[Selection::caret(4)]);
        history.undo(&mut buffer);
        assert_eq!(buffer.text(), "hi!");

        // A save seals the step, so the next keystroke dirties the buffer.
        history.mark_saved();
        assert!(history.is_clean());
        let cursor = vec![Selection::caret(3)];
        history.apply(
            &mut buffer,
            &[(3..3, ".".into())],
            &cursor,
            true,
            at(5_100, start),
        );
        assert!(!history.is_clean());
        history.undo(&mut buffer);
        assert!(history.is_clean(), "undo back to the saved text is clean");
        assert_eq!(buffer.text(), "hi!");
    }

    #[test]
    fn a_composition_group_is_one_step_whatever_the_cursors_do() {
        let start = Instant::now();
        let mut buffer = Buffer::new("x");
        let mut history = History::default();
        history.begin_group();
        history.apply(
            &mut buffer,
            &[(1..1, "n".into())],
            &[Selection::caret(1)],
            true,
            start,
        );
        history.apply(
            &mut buffer,
            &[(1..2, "ni".into())],
            &[Selection::caret(0)],
            true,
            start,
        );
        history.apply(
            &mut buffer,
            &[(1..3, "に".into())],
            &[Selection::caret(9)],
            true,
            start,
        );
        history.end_group();
        assert_eq!(buffer.text(), "xに");
        history.undo(&mut buffer);
        assert_eq!(buffer.text(), "x");
        history.apply(
            &mut buffer,
            &[(1..1, "y".into())],
            &[Selection::caret(1)],
            true,
            start,
        );
        history.apply(
            &mut buffer,
            &[(2..2, "z".into())],
            &[Selection::caret(9)],
            true,
            start,
        );
        assert_eq!(
            history.undo.len(),
            2,
            "outside a group, moved cursors split steps"
        );
    }

    #[test]
    fn no_op_edits_record_nothing() {
        let mut buffer = Buffer::new("same");
        let mut history = History::default();
        assert!(!history.apply(
            &mut buffer,
            &[(0..4, "same".into())],
            &[Selection::caret(0)],
            false,
            Instant::now()
        ));
        assert!(!history.can_undo());
    }
}
