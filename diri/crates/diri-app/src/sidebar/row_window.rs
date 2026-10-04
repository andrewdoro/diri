// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//
//! Windowed rendering for the session list: the idea of Ely's `VirtualList`
//! (build only the rows in view) inside the sidebar's own scroll container.
//!
//! Ely hands rows to GPUI's `list`, which owns the scroll and measures each
//! row as it comes into view. The sidebar cannot give its rows away like
//! that: whole project sections lift with the pointer and slide into new
//! slots, disclosures clip a section's body while it folds, rows grow in and
//! collapse out, and drops, edge fades and keyboard reveal all read the
//! bounds a row painted at. So the list keeps its layout and only stops
//! *building* rows nobody can see. Every row slot has a height known before
//! layout (rows are `SIDEBAR_NAV_ROW_HEIGHT` scaled by their presence), so
//! the sidebar walks its rows top to bottom with a cursor, in the same order
//! and with the same spacing the flex column lays them out, and a row whose
//! slot falls outside the viewport (plus overscan) becomes part of one plain
//! spacer per contiguous run. The column lays out exactly as before, and
//! scrolling rebuilds the window, because GPUI notifies the sidebar for
//! every scroll step.
//!
//! Rows that something is holding (the selection, the keyboard cursor, the
//! hovered row, a rename, the rows being dragged, a title still settling)
//! are always built, and any motion that moves rows away from their cursor
//! slot (a disclosure, a section shift, a lifted section, a row growing in)
//! builds its rows as before.

use std::collections::HashMap;

use diri_proto::SessionId;

/// A vertical span in list content space: zero at the top of the scrolled
/// content (its top padding included), growing down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Span {
    pub top: f32,
    pub bottom: f32,
}

impl Span {
    pub fn new(top: f32, bottom: f32) -> Self {
        Self { top, bottom }
    }

    fn intersects(self, other: Span) -> bool {
        self.top < other.bottom && other.top < self.bottom
    }
}

/// Built past each edge of the viewport, so a frame whose scroll moved after
/// the sidebar rendered (a clamp in layout, a scroll handled elsewhere)
/// still finds rows there, and the keyboard's next row is already painted.
pub(super) const MIN_OVERSCAN: f32 = 240.0;

/// The content-space band rows are built for: the visible part of the list
/// plus overscan on each side. `scroll_top` is how far the content has
/// scrolled (GPUI's `-offset.y`), `viewport` the visible height, and
/// `content` the list's height as last walked. Layout clamps a scroll the
/// content no longer reaches, so the band also covers where that clamp
/// would land; `content` can be a frame stale, so it widens the band and
/// never moves it.
pub(super) fn band(scroll_top: f32, viewport: f32, content: f32) -> Span {
    let viewport = viewport.max(0.0);
    let clamped = scroll_top.clamp(0.0, (content - viewport).max(0.0));
    let overscan = MIN_OVERSCAN.max(viewport / 2.0);
    Span::new(
        scroll_top.min(clamped) - overscan,
        scroll_top.max(clamped) + viewport + overscan,
    )
}

/// One render's walk down the list. Created with the band rows are built
/// for, or `None` to build everything.
#[derive(Default)]
pub(super) struct RowWindow {
    band: Option<Span>,
    cursor: f32,
    /// Where every row walked this render lays out, built or not.
    slots: HashMap<SessionId, Span>,
    /// Rows walked but not built this render.
    skipped_ids: Vec<SessionId>,
    built: usize,
    skipped: usize,
}

impl RowWindow {
    pub fn new(band: Option<Span>, top: f32) -> Self {
        Self {
            band,
            cursor: top,
            slots: HashMap::new(),
            skipped_ids: Vec::new(),
            built: 0,
            skipped: 0,
        }
    }

    /// The content-space y the next element lays out at.
    pub fn cursor(&self) -> f32 {
        self.cursor
    }

    /// Moves past an element that is always built (a header, a gap).
    pub fn advance(&mut self, by: f32) {
        self.cursor += by;
    }

    /// Puts the cursor at `y`, after a stretch whose layout the walk does not
    /// follow row by row (a body clipped mid-disclosure).
    pub fn set_cursor(&mut self, y: f32) {
        self.cursor = y;
    }

    /// Places a row `height` tall after a `margin` above it and says whether
    /// it should be built. `keep` builds it wherever it lands. The slot is
    /// recorded under `id` either way, so a reveal can scroll to a row that
    /// was never built.
    pub fn place(&mut self, id: Option<&SessionId>, margin: f32, height: f32, keep: bool) -> bool {
        let top = self.cursor + margin;
        let slot = Span::new(top, top + height);
        self.cursor = slot.bottom;
        if let Some(id) = id {
            self.slots.insert(id.clone(), slot);
        }
        let build = keep || self.band.is_none_or(|band| slot.intersects(band));
        if build {
            self.built += 1;
        } else {
            self.skipped += 1;
            if let Some(id) = id {
                self.skipped_ids.push(id.clone());
            }
        }
        build
    }

    /// Whether this walk can skip rows at all.
    pub fn windowed(&self) -> bool {
        self.band.is_some()
    }

    /// (every walked row's slot, the rows that were not built)
    pub fn finish(self) -> (HashMap<SessionId, Span>, Vec<SessionId>) {
        (self.slots, self.skipped_ids)
    }

    #[cfg(test)]
    pub fn counts(&self) -> (usize, usize) {
        (self.built, self.skipped)
    }
}

/// One row of a section body: built, or skipped and only holding its slot.
/// `height` and `margin` are the row's slot height and the margin above it.
pub(super) enum Slot<T> {
    Built { row: T, height: f32, margin: f32 },
    Skipped { height: f32, margin: f32 },
}

/// Collapses each run of skipped rows into one spacer, so a long list lays
/// out a handful of elements instead of one per row. A spacer stands where
/// the run's first row stood: it keeps that row's margin and spans the rest
/// of the run, margins included, so everything after it lays out exactly
/// where it did. `spacer(height)` builds the spacer element.
pub(super) fn merge_skipped<T>(
    slots: Vec<Slot<T>>,
    mut spacer: impl FnMut(f32) -> T,
) -> Vec<(T, f32, f32)> {
    let mut merged = Vec::with_capacity(slots.len());
    // (margin above the run, height from the first row's top to the run's end)
    let mut run: Option<(f32, f32)> = None;
    for slot in slots {
        match slot {
            Slot::Skipped { height, margin } => {
                run = Some(match run {
                    None => (margin, height),
                    Some((first, span)) => (first, span + margin + height),
                });
            }
            Slot::Built {
                row,
                height,
                margin,
            } => {
                if let Some((first, span)) = run.take() {
                    merged.push((spacer(span), span, first));
                }
                merged.push((row, height, margin));
            }
        }
    }
    if let Some((first, span)) = run {
        merged.push((spacer(span), span, first));
    }
    merged
}

/// Where a row should scroll to be fully visible: the new `offset.y` (GPUI
/// convention, zero at the top and negative below) for a row whose
/// content-space `slot` is known.
pub(super) fn offset_revealing(offset: f32, viewport: f32, slot: Span) -> f32 {
    let scroll_top = -offset;
    if slot.top < scroll_top {
        -slot.top
    } else if slot.bottom > scroll_top + viewport {
        -(slot.bottom - viewport)
    } else {
        offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> SessionId {
        SessionId::new(value)
    }

    #[test]
    fn the_band_covers_the_viewport_and_overscan_on_both_sides() {
        let band = band(1_000.0, 600.0, 10_000.0);
        assert_eq!(band, Span::new(1_000.0 - 300.0, 1_600.0 + 300.0));
        // A short viewport still keeps the minimum overscan.
        let short = super::band(0.0, 100.0, 10_000.0);
        assert_eq!(short, Span::new(-MIN_OVERSCAN, 100.0 + MIN_OVERSCAN));
    }

    #[test]
    fn the_band_covers_a_scroll_the_layout_is_about_to_clamp() {
        // The content shrank to 2,000 px under a scroll of 5,000: layout will
        // clamp the scroll to 1,400, so the band reaches up to cover that.
        let band = band(5_000.0, 600.0, 2_000.0);
        assert_eq!(band.top, 1_400.0 - 300.0);
        assert!(band.bottom >= 5_600.0 + 300.0);
        // Content shorter than the viewport never scrolls.
        assert_eq!(super::band(80.0, 600.0, 300.0).top, -300.0);
        // A stale, shorter content height cannot pull the band off a scroll
        // that is still valid (the list grew since it was walked).
        let grown = super::band(3_000.0, 600.0, 2_000.0);
        assert!(grown.top <= 1_400.0 - 300.0 && grown.bottom >= 3_600.0 + 300.0);
        // Rubber-banding past the top keeps the top rows built.
        let bounced = super::band(-80.0, 600.0, 5_000.0);
        assert!(bounced.top <= -80.0 - 300.0 && bounced.bottom >= 600.0 + 300.0);
    }

    #[test]
    fn rows_are_built_only_where_they_meet_the_band() {
        let mut window = RowWindow::new(Some(Span::new(100.0, 200.0)), 0.0);
        let built: Vec<bool> = (0..10)
            .map(|index| window.place(Some(&id(&format!("{index}"))), 2.0, 30.0, false))
            .collect();
        // Rows lay out at 2..32, 34..64, 66..96, 98..128, ..., 162..192, 194..224.
        assert_eq!(
            built,
            [
                false, false, false, true, true, true, true, false, false, false
            ]
        );
        assert_eq!(window.counts(), (4, 6));
        let (slots, skipped) = window.finish();
        assert_eq!(skipped.len(), 6);
        assert!(!skipped.contains(&id("3")));
        assert_eq!(slots[&id("0")], Span::new(2.0, 32.0));
        assert_eq!(slots[&id("9")], Span::new(290.0, 320.0));
    }

    #[test]
    fn a_kept_row_is_built_anywhere_and_no_band_builds_everything() {
        let mut window = RowWindow::new(Some(Span::new(0.0, 10.0)), 500.0);
        assert!(window.place(Some(&id("selected")), 2.0, 30.0, true));
        assert!(!window.place(Some(&id("other")), 2.0, 30.0, false));
        let mut everything = RowWindow::new(None, 0.0);
        assert!(!everything.windowed());
        assert!((0..100).all(|_| everything.place(None, 2.0, 30.0, false)));
    }

    #[test]
    fn the_cursor_walks_headers_gaps_and_rows_in_layout_order() {
        let mut window = RowWindow::new(None, 8.0);
        window.advance(30.0);
        window.place(Some(&id("a")), 2.0, 30.0, false);
        window.place(Some(&id("b")), 1.0, 15.0, false);
        window.advance(8.0);
        assert_eq!(window.cursor(), 8.0 + 30.0 + 32.0 + 16.0 + 8.0);
        window.set_cursor(12.0);
        window.place(Some(&id("c")), 0.0, 10.0, false);
        assert_eq!(window.finish().0[&id("c")], Span::new(12.0, 22.0));
    }

    fn total(rows: &[(String, f32, f32)]) -> f32 {
        rows.iter().map(|(_, height, margin)| height + margin).sum()
    }

    #[test]
    fn merging_skipped_runs_keeps_the_body_height_and_every_built_row_in_place() {
        let rows = |skip: &[bool]| -> Vec<Slot<String>> {
            skip.iter()
                .enumerate()
                .map(|(index, skip)| {
                    // Row 2 is half grown in: half its height and margin.
                    let scale = if index == 2 { 0.5 } else { 1.0 };
                    if *skip {
                        Slot::Skipped {
                            height: 30.0 * scale,
                            margin: 2.0 * scale,
                        }
                    } else {
                        Slot::Built {
                            row: format!("row {index}"),
                            height: 30.0 * scale,
                            margin: 2.0 * scale,
                        }
                    }
                })
                .collect()
        };
        let everything = rows(&[false; 7]);
        let unmerged = merge_skipped(everything, |_| unreachable!());
        let skip = [true, true, false, true, true, true, false];
        let merged = merge_skipped(rows(&skip), |height| format!("spacer {height}"));
        let names: Vec<_> = merged.iter().map(|(row, _, _)| row.as_str()).collect();
        assert_eq!(
            names,
            ["spacer 62", "row 2", "spacer 94", "row 6"],
            "one spacer per run, built rows untouched"
        );
        assert_eq!(total(&merged), total(&unmerged));
        // Built rows start where they started before merging.
        let top_of = |rows: &[(String, f32, f32)], name: &str| {
            let mut y = 0.0;
            for (row, height, margin) in rows {
                y += margin;
                if row == name {
                    return y;
                }
                y += height;
            }
            unreachable!()
        };
        for name in ["row 2", "row 6"] {
            assert_eq!(top_of(&merged, name), top_of(&unmerged, name));
        }
        // A trailing run and an all-skipped body.
        let trailing = merge_skipped(rows(&[false, true, true, true, true, true, true]), |h| {
            format!("spacer {h}")
        });
        assert_eq!(trailing.len(), 2);
        assert_eq!(total(&trailing), total(&unmerged));
        let none = merge_skipped(rows(&[true; 7]), |h| format!("spacer {h}"));
        assert_eq!(none.len(), 1);
        assert_eq!(total(&none), total(&unmerged));
    }

    #[test]
    fn revealing_scrolls_the_least_distance_that_shows_the_whole_row() {
        // Viewport of 300 scrolled 1,000 down: content 1,000..1,300 visible.
        let visible = Span::new(1_100.0, 1_130.0);
        assert_eq!(offset_revealing(-1_000.0, 300.0, visible), -1_000.0);
        let above = Span::new(400.0, 430.0);
        assert_eq!(offset_revealing(-1_000.0, 300.0, above), -400.0);
        let below = Span::new(5_000.0, 5_030.0);
        assert_eq!(offset_revealing(-1_000.0, 300.0, below), -(5_030.0 - 300.0));
    }
}
