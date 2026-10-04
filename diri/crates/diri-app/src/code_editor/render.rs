// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components

//! Painting the editor: virtualized rows with a gutter, indent guides,
//! rulers, rainbow brackets, selections and carets; sticky block headers;
//! a minimap; the find bar; and the hover, completion and signature cards.

use std::ops::Range;

use gpui::{
    AnyElement, Bounds, Context, DispatchPhase, ElementInputHandler, FontStyle, FontWeight,
    HighlightStyle, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Render, Rgba,
    ScrollWheelEvent, SharedString, StyledText, Window, canvas, div, fill, point, prelude::*, px,
    size, uniform_list,
};

use super::editor::{CodeEditor, EditorEvent, Row};
use super::input::{EDITOR_CONTEXT, listen};
use super::intel::CandidateKind;
use super::syntax::{self, TokenKind, brackets_in, enclosing, occurrences_in};
use crate::icons::sf_symbol;
use diri_ui::Radius;

/// Row height as a multiple of the font size.
const LEADING: f32 = 1.55;
/// Space between the gutter and column zero.
const CODE_PAD: f32 = 10.0;
/// Width of the fold chevron column.
const FOLD_COLUMN: f32 = 16.0;
/// Block headers sticky scroll shows at most.
const STICKY: usize = 3;
const MINIMAP_WIDTH: f32 = 58.0;
/// Pixels per row in the minimap.
const MINIMAP_ROW: f32 = 2.0;
/// The minimap only shows when the editor is at least this wide.
const MINIMAP_MIN_WIDTH: f32 = 440.0;
/// Columns rendered past the visible width, so fast horizontal scrolls
/// never show a gap before the next frame.
const SLACK_COLUMNS: usize = 24;

/// The part of a line a row draws: display text for columns `first..` with
/// tabs expanded, and where each source byte landed in it.
pub(crate) struct Slice {
    pub text: String,
    pub first_column: usize,
    /// `(source byte, display byte)` at the start of every character kept.
    starts: Vec<(usize, usize)>,
    source_end: usize,
}

impl Slice {
    pub(crate) fn new(line: &str, tab: usize, first: usize, last: usize) -> Self {
        let mut text = String::new();
        let mut starts = Vec::new();
        let mut column = 0;
        let mut first_column = None;
        let mut source_end = line.len();
        for (byte, ch) in line.char_indices() {
            let width = if ch == '\t' { tab - column % tab } else { 1 };
            if column + width <= first {
                column += width;
                continue;
            }
            if column >= last {
                source_end = byte;
                break;
            }
            first_column.get_or_insert(column.max(first));
            starts.push((byte, text.len()));
            if ch == '\t' {
                let shown = (column + width).min(last) - column.max(first);
                text.extend(std::iter::repeat_n(' ', shown));
            } else {
                text.push(ch);
            }
            column += width;
        }
        Self {
            text,
            first_column: first_column.unwrap_or(first),
            starts,
            source_end,
        }
    }

    /// Where a source byte range lands in the display text, clipped.
    pub(crate) fn map(&self, range: Range<usize>) -> Option<Range<usize>> {
        let (first_source, _) = *self.starts.first()?;
        let start = range.start.max(first_source);
        let end = range.end.min(self.source_end);
        if start >= end {
            return None;
        }
        let display = |byte: usize| {
            let at = self.starts.partition_point(|(source, _)| *source <= byte);
            match at {
                0 => 0,
                _ => {
                    let (source, display) = self.starts[at - 1];
                    if source == byte {
                        display
                    } else {
                        self.starts.get(at).map_or(self.text.len(), |next| next.1)
                    }
                }
            }
        };
        let (start, end) = (
            display(start),
            if end >= self.source_end {
                self.text.len()
            } else {
                display(end)
            },
        );
        (start < end).then_some(start..end)
    }
}

fn shadow(alpha: f32, offset: f32, blur: f32) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow {
        color: gpui::hsla(0.0, 0.0, 0.0, alpha),
        offset: point(px(0.0), px(offset)),
        blur_radius: px(blur),
        spread_radius: px(0.0),
        inset: false,
    }]
}

impl CodeEditor {
    fn gutter_width(&self, advance: Pixels) -> Pixels {
        let lines = self.doc.as_ref().map_or(1, |doc| doc.buffer.lines());
        let digits = lines.to_string().len().max(2) as f32;
        advance * digits + px(12.0 + FOLD_COLUMN)
    }

    /// The columns a row draws, from the horizontal scroll and the width.
    fn visible_columns(&self) -> (usize, usize) {
        let advance = self.metrics.advance;
        if advance <= px(0.0) {
            return (0, 400);
        }
        let first = (self.h_offset / advance).floor().max(0.0) as usize;
        let width = (self.metrics.width / advance).ceil().max(80.0) as usize;
        (
            first.saturating_sub(SLACK_COLUMNS),
            first + width + SLACK_COLUMNS,
        )
    }

    /// The text the primary selection or its word gives, for highlighting
    /// where else it appears.
    fn needle(&self) -> Option<String> {
        let doc = self.doc.as_ref()?;
        let primary = self.primary();
        let range = if primary.is_empty() {
            doc.buffer.identifier_at(primary.head)?
        } else {
            primary.range()
        };
        let text = &doc.buffer.text()[range];
        (!text.is_empty() && !text.contains('\n') && text.trim().len() == text.len())
            .then(|| text.to_string())
    }

    fn line_styles(&self, line: usize, slice: &Slice) -> Vec<(Range<usize>, HighlightStyle)> {
        let Some(doc) = &self.doc else {
            return Vec::new();
        };
        let palette = self.palette;
        let line_start = doc.buffer.line_range(line).start;
        let mut styles: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        let tokens = self
            .analysis
            .tokens
            .get(line)
            .map_or(&[][..], Vec::as_slice);
        for token in tokens {
            if token.kind == TokenKind::Bracket {
                continue;
            }
            let Some(range) = slice.map(token.range.clone()) else {
                continue;
            };
            let mut style = HighlightStyle {
                color: Some(palette.token(token.kind).into()),
                ..HighlightStyle::default()
            };
            match token.kind {
                TokenKind::Comment => style.font_style = Some(FontStyle::Italic),
                TokenKind::Keyword => style.font_weight = Some(FontWeight::MEDIUM),
                _ => {}
            }
            styles.push((range, style));
        }
        let range = doc.buffer.line_range(line);
        let matched = syntax::matched(&self.analysis.brackets, self.primary().head);
        for bracket in brackets_in(&self.analysis.brackets, range) {
            let local = bracket.offset - line_start;
            let Some(display) = slice.map(local..local + 1) else {
                continue;
            };
            let color: Rgba = if bracket.partner.is_none() {
                palette.punctuation
            } else {
                palette.rainbow(bracket.depth)
            };
            let highlighted = matched
                .is_some_and(|(open, close)| bracket.offset == open || bracket.offset == close);
            styles.push((
                display,
                HighlightStyle {
                    color: Some(color.into()),
                    font_weight: highlighted.then_some(FontWeight::BOLD),
                    ..HighlightStyle::default()
                },
            ));
        }
        styles.sort_by_key(|(range, _)| range.start);
        styles
    }

    /// Column spans to wash behind a line: find matches, occurrences of the
    /// selected word, and the matched bracket pair.
    fn washes(&self, line: usize, needle: Option<&str>) -> Vec<(Range<usize>, Rgba)> {
        let Some(doc) = &self.doc else {
            return Vec::new();
        };
        let palette = self.palette;
        let range = doc.buffer.line_range(line);
        let text = doc.buffer.line(line);
        let column = |offset: usize| syntax::column_at(text, offset - range.start, self.tab);
        let mut washes = Vec::new();
        if self.find.open {
            let matches = &self.find.matches;
            let first = matches.partition_point(|hit| hit.end <= range.start);
            for (index, hit) in matches.iter().enumerate().skip(first) {
                if hit.start > range.end {
                    break;
                }
                let start = hit.start.max(range.start);
                let end = hit.end.min(range.end);
                let columns = column(start)..column(end).max(column(start) + 1);
                let color = if Some(index) == self.find.current {
                    palette.find_current
                } else {
                    palette.find_match
                };
                washes.push((columns, color));
            }
        } else if let Some(needle) = needle {
            let primary = self.primary().range();
            for hit in occurrences_in(text, needle) {
                let absolute = range.start + hit.start..range.start + hit.end;
                if absolute == primary {
                    continue;
                }
                washes.push((
                    column(absolute.start)..column(absolute.end),
                    palette.occurrence,
                ));
            }
        }
        if let Some((open, close)) = syntax::matched(&self.analysis.brackets, self.primary().head) {
            for at in [open, close] {
                if range.contains(&at) {
                    washes.push((column(at)..column(at) + 1, palette.bracket_match));
                }
            }
        }
        washes
    }

    /// Indent guide columns on a line: every indent stop inside its indent,
    /// or the next non-blank line's when it is blank.
    fn guides(&self, line: usize) -> Vec<usize> {
        let Some(doc) = &self.doc else {
            return Vec::new();
        };
        let indent_of = |line: usize| doc.buffer.indent_columns(line, self.tab);
        let indent = if doc.buffer.line(line).trim().is_empty() {
            let next = (line + 1..doc.buffer.lines().min(line + 200))
                .find(|at| !doc.buffer.line(*at).trim().is_empty())
                .map_or(0, indent_of);
            let previous = (line.saturating_sub(200)..line)
                .rev()
                .find(|at| !doc.buffer.line(*at).trim().is_empty())
                .map_or(0, indent_of);
            next.min(previous.max(next))
        } else {
            indent_of(line)
        };
        let step = self.tab.max(1);
        (step..indent + 1)
            .step_by(step)
            .map(|column| column - step)
            .collect()
    }

    /// The guide column of the innermost block holding the primary cursor,
    /// and the lines it spans.
    fn active_guide(&self) -> Option<(usize, Range<usize>)> {
        let doc = self.doc.as_ref()?;
        let line = doc.buffer.line_of(self.primary().head);
        let fold = enclosing(&self.analysis.folds, line).pop()?;
        Some((
            doc.buffer.indent_columns(fold.header, self.tab),
            fold.header + 1..fold.end + 1,
        ))
    }

    fn rulers(&self) -> &'static [usize] {
        use crate::code_intelligence::SourceLanguage::*;
        match self.doc.as_ref().map(|doc| doc.language) {
            Some(Rust) => &[100],
            Some(Python) => &[88],
            Some(
                Go | Swift | TypeScript | Tsx | JavaScript | Jsx | Java | Kotlin | C | Cpp | CSharp,
            ) => &[120],
            _ => &[],
        }
    }

    fn row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let height = self.metrics.line;
        let base = div()
            .id(("editor-row", ix))
            .w_full()
            .h(height)
            .flex()
            .flex_none();
        match self.layout.rows.get(ix).copied() {
            Some(Row::Line(line)) => self.line_row(base, line, false, window, cx),
            Some(Row::Lens(lens)) => self.lens_row(base, lens, cx),
            None => base.into_any_element(),
        }
    }

    fn gutter(&self, line: Option<usize>, sticky: bool, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette;
        let advance = self.metrics.advance;
        let width = self.gutter_width(advance);
        let base = div()
            .flex_none()
            .w(width)
            .h_full()
            .flex()
            .items_center()
            .when(sticky, |gutter| gutter.bg(palette.background));
        let Some(line) = line else {
            return base.into_any_element();
        };
        let Some(doc) = &self.doc else {
            return base.into_any_element();
        };
        let current = doc.buffer.line_of(self.primary().head) == line;
        let opens = self
            .analysis
            .folds
            .binary_search_by_key(&line, |fold| fold.header)
            .is_ok();
        let folded = doc.folded.contains(&line);
        base.child(
            div()
                .flex_1()
                .pr(px(4.0))
                .text_right()
                .whitespace_nowrap()
                .text_size(self.font_size * 0.92)
                .text_color(if current {
                    palette.line_number_current
                } else {
                    palette.line_number
                })
                .child((line + 1).to_string()),
        )
        .child(
            div()
                .id(("fold", line))
                .flex_none()
                .w(px(FOLD_COLUMN))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .when(opens && !sticky, |slot| {
                    slot.cursor_pointer()
                        .opacity(if folded { 1.0 } else { 0.0 })
                        .group_hover("editor-gutter", |style| style.opacity(1.0))
                        .child(sf_symbol(
                            if folded {
                                "chevron.right"
                            } else {
                                "chevron.down"
                            },
                            9.0,
                            palette.line_number_current,
                        ))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |editor, _, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                                editor.toggle_fold(line, cx);
                            }),
                        )
                }),
        )
        .child(div().flex_none().w(px(8.0)))
        .into_any_element()
    }

    fn line_row(
        &self,
        base: gpui::Stateful<gpui::Div>,
        line: usize,
        sticky: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(doc) = &self.doc else {
            return base.into_any_element();
        };
        let palette = self.palette;
        let (advance, height) = (self.metrics.advance, self.metrics.line);
        let text = doc.buffer.line(line);
        let range = doc.buffer.line_range(line);
        let (first, last) = self.visible_columns();
        let slice = Slice::new(text, self.tab, first, last);
        let styles = self.line_styles(line, &slice);
        let shift = self.h_offset;
        let x = move |column: usize| advance * column as f32 - shift + px(CODE_PAD);
        let focused = self.focused;
        let primary = self.primary();
        let column_of =
            |offset: usize| syntax::column_at(text, offset.saturating_sub(range.start), self.tab);
        let current_line =
            !sticky && primary.is_empty() && doc.buffer.line_of(primary.head) == line;

        let mut layers: Vec<AnyElement> = Vec::new();
        if current_line {
            layers.push(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(palette.current_line)
                    .into_any_element(),
            );
        }
        if !sticky {
            let needle = self.needle();
            for (columns, color) in self.washes(line, needle.as_deref()) {
                layers.push(
                    div()
                        .absolute()
                        .top(px(1.0))
                        .h(height - px(2.0))
                        .left(x(columns.start))
                        .w(advance * (columns.end - columns.start) as f32)
                        .rounded(px(2.0))
                        .bg(color)
                        .into_any_element(),
                );
            }
            for selection in self.selections() {
                let span = selection.range();
                if span.is_empty() || span.end < range.start || span.start > range.end {
                    continue;
                }
                let start = if span.start <= range.start {
                    0
                } else {
                    column_of(span.start)
                };
                let end = if span.end > range.end {
                    column_of(range.end) + 1
                } else {
                    column_of(span.end)
                };
                if end > start {
                    layers.push(
                        div()
                            .absolute()
                            .top_0()
                            .h(height)
                            .left(x(start))
                            .w(advance * (end - start) as f32)
                            .bg(if focused {
                                palette.selection
                            } else {
                                palette.selection_unfocused
                            })
                            .into_any_element(),
                    );
                }
            }
        }
        let active = self.active_guide();
        for column in self.guides(line) {
            let strong = !sticky
                && active
                    .as_ref()
                    .is_some_and(|(at, lines)| *at == column && lines.contains(&line));
            layers.push(
                div()
                    .absolute()
                    .top_0()
                    .h(height)
                    .left(x(column) + advance * 0.5)
                    .w(px(1.0))
                    .bg(if strong {
                        palette.guide_active
                    } else {
                        palette.guide
                    })
                    .into_any_element(),
            );
        }
        for column in self.rulers() {
            layers.push(
                div()
                    .absolute()
                    .top_0()
                    .h(height)
                    .left(x(*column))
                    .w(px(1.0))
                    .bg(palette.ruler)
                    .into_any_element(),
            );
        }
        layers.push(
            div()
                .absolute()
                .top_0()
                .left(x(slice.first_column))
                .h(height)
                .line_height(height)
                .whitespace_nowrap()
                .text_color(palette.foreground)
                .child(
                    StyledText::new(SharedString::from(slice.text.clone())).with_highlights(styles),
                )
                .into_any_element(),
        );
        if doc.folded.contains(&line) {
            let end = syntax::columns(text, self.tab);
            layers.push(
                div()
                    .id(("unfold", line))
                    .absolute()
                    .top_0()
                    .left(x(end + 1))
                    .h(height)
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .px(px(5.0))
                            .h(height * 0.72)
                            .flex()
                            .items_center()
                            .rounded(px(Radius::CHIP))
                            .bg(palette.occurrence)
                            .text_color(palette.line_number_current)
                            .text_size(self.font_size * 0.85)
                            .child("⋯"),
                    )
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |editor, _, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                            editor.toggle_fold(line, cx);
                        }),
                    )
                    .into_any_element(),
            );
        }
        if !sticky && focused && self.caret_on {
            for selection in self.selections() {
                if doc.buffer.line_of(selection.head) != line {
                    continue;
                }
                layers.push(
                    div()
                        .absolute()
                        .top(px(1.0))
                        .h(height - px(2.0))
                        .left(x(column_of(selection.head)))
                        .w(px(2.0))
                        .bg(palette.cursor)
                        .into_any_element(),
                );
            }
        }
        if let Some(marked) = &self.marked
            && !sticky
            && doc.buffer.line_of(marked.start) == line
        {
            let start = column_of(marked.start);
            let end = column_of(marked.end.min(range.end)).max(start + 1);
            layers.push(
                div()
                    .absolute()
                    .bottom(px(1.0))
                    .h(px(1.0))
                    .left(x(start))
                    .w(advance * (end - start) as f32)
                    .bg(palette.foreground)
                    .into_any_element(),
            );
        }
        let _ = window;
        let row = base.child(self.gutter(Some(line), sticky, cx)).child(
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_hidden()
                .children(layers),
        );
        if sticky {
            row.bg(palette.background)
                .cursor_pointer()
                .hover(move |row| row.bg(palette.current_line))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |editor, _, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                        let column = editor.doc.as_ref().map_or(0, |doc| doc.buffer.indent(line));
                        editor.go_to(line + 1, column + 1, cx);
                        window.focus(&editor.focus, cx);
                    }),
                )
                .into_any_element()
        } else {
            row.into_any_element()
        }
    }

    fn lens_row(
        &self,
        base: gpui::Stateful<gpui::Div>,
        lens: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(item) = self.layout.lenses.get(lens) else {
            return base.into_any_element();
        };
        let Some(doc) = &self.doc else {
            return base.into_any_element();
        };
        let palette = self.palette;
        let indent = doc.buffer.indent_columns(item.line, self.tab);
        let label = match item.uses {
            0 => "no other uses in this file".to_owned(),
            1 => "1 use in this file".to_owned(),
            uses => format!("{uses} uses in this file"),
        };
        let name = item.name.clone();
        let left = self.metrics.advance * indent as f32 - self.h_offset + px(CODE_PAD);
        base.child(self.gutter(None, false, cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .left(left)
                            .h_full()
                            .flex()
                            .items_end()
                            .pb(px(1.0))
                            .gap(px(6.0))
                            .font_family(crate::fonts::ui_family())
                            .text_size(px(10.0))
                            .text_color(palette.line_number)
                            .child(label)
                            .child(
                                div()
                                    .id(("lens-search", lens))
                                    .cursor_pointer()
                                    .hover(move |link| link.text_color(palette.function))
                                    .child("Search workspace")
                                    .on_mouse_down(MouseButton::Left, |_, window, _| {
                                        window.prevent_default()
                                    })
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(EditorEvent::SearchWorkspace(name.clone()));
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn sticky(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let doc = self.doc.as_ref()?;
        let line_height = self.metrics.line;
        if line_height <= px(0.0) {
            return None;
        }
        let scrolled = -self.scroll.0.borrow().base_handle.offset().y;
        let top_row = (scrolled / line_height).floor().max(0.0) as usize;
        let line_at = |row: usize| match self.layout.rows.get(row) {
            Some(Row::Line(line)) => Some(*line),
            Some(Row::Lens(lens)) => self.layout.lenses.get(*lens).map(|lens| lens.line),
            None => None,
        };
        // The innermost blocks around the first line the stack leaves
        // visible. The stack's own height moves that line, so settle it.
        let headers_at = |row: usize| -> Vec<usize> {
            let Some(line) = line_at(row) else {
                return Vec::new();
            };
            let mut headers: Vec<usize> = enclosing(&self.analysis.folds, line)
                .into_iter()
                .map(|fold| fold.header)
                .filter(|header| !doc.folded.contains(header))
                .collect();
            if headers.len() > STICKY {
                headers.drain(..headers.len() - STICKY);
            }
            headers
        };
        let mut covered = 0;
        let mut headers = headers_at(top_row);
        for _ in 0..=STICKY {
            if headers.len() == covered {
                break;
            }
            covered = headers.len();
            headers = headers_at(top_row + covered);
        }
        if headers.is_empty() {
            return None;
        }
        let rows: Vec<AnyElement> = headers
            .into_iter()
            .map(|line| {
                let base = div()
                    .id(("sticky-row", line))
                    .w_full()
                    .h(line_height)
                    .flex()
                    .flex_none();
                self.line_row(base, line, true, window, cx)
            })
            .collect();
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .flex()
                .flex_col()
                .bg(self.palette.background)
                .border_b_1()
                .border_color(self.palette.guide_active)
                .shadow(shadow(
                    if self.palette.light { 0.06 } else { 0.25 },
                    2.0,
                    4.0,
                ))
                .children(rows)
                .into_any_element(),
        )
    }

    fn minimap_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(doc) = &self.doc else {
            return div().into_any_element();
        };
        let palette = self.palette;
        let line_height = self.metrics.line.max(px(1.0));
        let scroll = self.scroll.clone();
        let total = self.layout.rows.len();
        // Up to a screenful of minimap rows around the view; built per frame.
        let state = scroll.0.borrow();
        let viewport = state.base_handle.bounds().size.height;
        let scrolled = -state.base_handle.offset().y;
        drop(state);
        let capacity = ((viewport / px(MINIMAP_ROW)).floor() as usize).max(1);
        let content = line_height * total as f32;
        let fraction = if content > viewport {
            (scrolled / (content - viewport)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let first = if total > capacity {
            ((total - capacity) as f32 * fraction).round() as usize
        } else {
            0
        };
        let rows: Vec<Vec<(f32, f32, Rgba)>> = self.layout.rows
            [first..(first + capacity).min(total)]
            .iter()
            .map(|row| {
                let Row::Line(line) = row else {
                    return Vec::new();
                };
                let text = doc.buffer.line(*line);
                let mut spans = Vec::new();
                // Words in the foreground, then tokens in their colors on top.
                let mut column = 0usize;
                let mut run: Option<usize> = None;
                for ch in text.chars().chain(std::iter::once(' ')) {
                    let blank = ch.is_whitespace();
                    match (blank, run) {
                        (false, None) => run = Some(column),
                        (true, Some(start)) => {
                            spans.push((
                                start as f32,
                                (column - start) as f32,
                                palette.foreground.alpha(0.32),
                            ));
                            run = None;
                        }
                        _ => {}
                    }
                    column += if ch == '\t' {
                        self.tab - column % self.tab
                    } else {
                        1
                    };
                    if column > 120 {
                        break;
                    }
                }
                for token in self.analysis.tokens.get(*line).into_iter().flatten() {
                    if matches!(token.kind, TokenKind::Punctuation | TokenKind::Bracket) {
                        continue;
                    }
                    let start = syntax::column_at(text, token.range.start, self.tab);
                    let end = syntax::column_at(text, token.range.end, self.tab);
                    spans.push((
                        start as f32,
                        (end - start) as f32,
                        palette.token(token.kind).alpha(0.75),
                    ));
                }
                spans
            })
            .collect();
        let entity = cx.entity();
        let (press, drag) = (cx.entity(), cx.entity());
        let slider = palette
            .foreground
            .alpha(if palette.light { 0.07 } else { 0.08 });
        let view_rows = (viewport / line_height).max(1.0);
        let top_row = scrolled / line_height;
        div()
            .id("minimap")
            .relative()
            .flex_none()
            .w(px(MINIMAP_WIDTH))
            .h_full()
            .border_l_1()
            .border_color(palette.guide)
            .cursor_pointer()
            .child(
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |editor, _| editor.minimap_bounds = bounds);
                    },
                    move |bounds, _, window, _| {
                        let origin = bounds.origin;
                        let slider_top = origin.y + px(MINIMAP_ROW) * (top_row - first as f32);
                        window.paint_quad(fill(
                            Bounds::new(
                                point(origin.x, slider_top.max(origin.y)),
                                size(bounds.size.width, px(MINIMAP_ROW) * view_rows),
                            ),
                            slider,
                        ));
                        let scale = 0.42;
                        for (ix, spans) in rows.iter().enumerate() {
                            let y = origin.y + px(MINIMAP_ROW * ix as f32);
                            for (start, len, color) in spans {
                                let x = origin.x + px(5.0 + start * scale);
                                let width = px((len * scale).max(0.6))
                                    .min(bounds.size.width - (x - origin.x));
                                if width <= px(0.0) {
                                    continue;
                                }
                                window.paint_quad(fill(
                                    Bounds::new(point(x, y), size(width, px(MINIMAP_ROW * 0.7))),
                                    *color,
                                ));
                            }
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, cx| {
                press.update(cx, |editor, cx| {
                    editor.minimap_jump(event.position.y, first, cx)
                });
            })
            .on_mouse_move(move |event: &MouseMoveEvent, _, cx| {
                if event.dragging() {
                    drag.update(cx, |editor, cx| {
                        editor.minimap_jump(event.position.y, first, cx)
                    });
                }
            })
            .into_any_element()
    }

    /// Scrolls so the row under a minimap point sits in the middle of the view.
    fn minimap_jump(&mut self, y: Pixels, first: usize, cx: &mut Context<Self>) {
        let bounds = self.minimap_bounds;
        let row = first as f32 + ((y - bounds.origin.y) / px(MINIMAP_ROW)).max(0.0);
        let state = self.scroll.0.borrow();
        let viewport = state.base_handle.bounds().size.height;
        let line = self.metrics.line;
        let content = line * self.layout.rows.len() as f32;
        let top = (line * row - viewport / 2.0).clamp(px(0.0), (content - viewport).max(px(0.0)));
        state.base_handle.set_offset(point(px(0.0), -top));
        drop(state);
        cx.notify();
    }

    fn find_bar(&self, focused_bar: bool, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette;
        let colors = self.colors;
        let find = &self.find;
        let field = |id: &'static str,
                     editor: &crate::query_editor::QueryEditor,
                     active: bool,
                     placeholder: &'static str,
                     replacement: bool,
                     cx: &mut Context<Self>| {
            let content = if editor.is_empty() && !(active && focused_bar) {
                div()
                    .text_color(colors.tertiary)
                    .child(placeholder)
                    .into_any_element()
            } else if active && focused_bar {
                crate::navigation::query_label(editor)
            } else {
                div().child(editor.text().to_owned()).into_any_element()
            };
            div()
                .id(id)
                .flex_1()
                .min_w(px(0.0))
                .h(px(24.0))
                .px(px(7.0))
                .flex()
                .items_center()
                .overflow_hidden()
                .whitespace_nowrap()
                .rounded(px(Radius::CHIP))
                .bg(colors.primary.alpha(0.05))
                .border_1()
                .border_color(if active && focused_bar {
                    palette.function.alpha(0.55)
                } else {
                    colors.primary.alpha(0.08)
                })
                .cursor_text()
                .child(content)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |editor, _, window, cx| {
                        editor.find.in_replacement = replacement;
                        window.focus(&editor.find_focus, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
        };
        let toggle = |id: &'static str,
                      label: &'static str,
                      on: bool,
                      tooltip_option: fn(&mut super::find::FindOptions) -> &mut bool,
                      cx: &mut Context<Self>| {
            div()
                .id(id)
                .size(px(22.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(Radius::CHIP))
                .font_family(crate::fonts::mono_family())
                .text_size(px(10.5))
                .text_color(if on {
                    palette.function
                } else {
                    colors.secondary
                })
                .bg(if on {
                    palette.function.alpha(0.16)
                } else {
                    colors.primary.alpha(0.0)
                })
                .border_1()
                .border_color(if on {
                    palette.function.alpha(0.45)
                } else {
                    colors.primary.alpha(0.0)
                })
                .cursor_pointer()
                .hover(move |button| button.bg(colors.primary.alpha(0.08)))
                .child(label)
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .on_click(cx.listener(move |editor, _, _, cx| {
                    editor.toggle_find_option(tooltip_option, cx)
                }))
        };
        let icon_button =
            |id: &'static str,
             symbol: &'static str,
             enabled: bool,
             cx: &mut Context<Self>,
             action: fn(&mut CodeEditor, &mut Window, &mut Context<CodeEditor>)| {
                div()
                    .id(id)
                    .size(px(22.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(Radius::CHIP))
                    .when(enabled, |button| {
                        button
                            .cursor_pointer()
                            .hover(move |button| button.bg(colors.primary.alpha(0.08)))
                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                window.prevent_default()
                            })
                            .on_click(
                                cx.listener(move |editor, _, window, cx| {
                                    action(editor, window, cx)
                                }),
                            )
                    })
                    .child(sf_symbol(
                        symbol,
                        10.0,
                        if enabled {
                            colors.secondary
                        } else {
                            colors.tertiary
                        },
                    ))
            };
        let found = !find.matches.is_empty();
        let (count, missing) = match (&find.error, find.current) {
            (Some(error), _) => (error.clone(), true),
            (None, _) if find.query.is_empty() => (String::new(), false),
            (None, _) if find.matches.is_empty() => ("No results".to_owned(), true),
            (None, Some(current)) => (
                format!(
                    "{} of {}{}",
                    current + 1,
                    find.matches.len(),
                    if find.matches.len() >= super::find::MATCH_LIMIT {
                        "+"
                    } else {
                        ""
                    }
                ),
                false,
            ),
            (None, None) => (format!("{} found", find.matches.len()), false),
        };
        let replace_open = find.replace_open;
        let in_replacement = find.in_replacement;
        let options = find.options;
        let mut bar = div()
            .id("find-bar")
            .track_focus(&self.find_focus)
            .on_key_down(
                cx.listener(|editor, event, window, cx| editor.find_key(event, window, cx)),
            )
            .occlude()
            .w(px(372.0))
            .max_w_full()
            .p(px(5.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .rounded(px(Radius::BADGE + 2.0))
            .bg(colors.floating_surface())
            .border_1()
            .border_color(colors.floating_stroke())
            .shadow(shadow(if palette.light { 0.10 } else { 0.35 }, 4.0, 14.0))
            .font_family(crate::fonts::mono_family())
            .text_size(px(11.0))
            .text_color(colors.primary)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .child(icon_button(
                        "find-toggle-replace",
                        if replace_open {
                            "chevron.down"
                        } else {
                            "chevron.right"
                        },
                        true,
                        cx,
                        |editor, window, cx| {
                            editor.find.replace_open = !editor.find.replace_open;
                            editor.find.in_replacement = editor.find.replace_open;
                            window.focus(&editor.find_focus, cx);
                            cx.notify();
                        },
                    ))
                    .child(field(
                        "find-query",
                        &find.query,
                        !in_replacement,
                        "Find",
                        false,
                        cx,
                    ))
                    .child(toggle(
                        "find-case",
                        "Aa",
                        options.case,
                        |options| &mut options.case,
                        cx,
                    ))
                    .child(toggle(
                        "find-word",
                        "ab",
                        options.word,
                        |options| &mut options.word,
                        cx,
                    ))
                    .child(toggle(
                        "find-regex",
                        ".*",
                        options.regex,
                        |options| &mut options.regex,
                        cx,
                    ))
                    .child(
                        div()
                            .min_w(px(58.0))
                            .px(px(4.0))
                            .flex_none()
                            .text_center()
                            .whitespace_nowrap()
                            .font_family(crate::fonts::ui_family())
                            .text_size(px(10.0))
                            .text_color(if missing {
                                palette.deleted
                            } else {
                                colors.tertiary
                            })
                            .child(count),
                    )
                    .child(icon_button(
                        "find-previous",
                        "arrow.up",
                        found,
                        cx,
                        |editor, _, cx| editor.find_step(false, cx),
                    ))
                    .child(icon_button(
                        "find-next",
                        "arrow.down",
                        found,
                        cx,
                        |editor, _, cx| editor.find_step(true, cx),
                    ))
                    .child(icon_button(
                        "find-close",
                        "xmark",
                        true,
                        cx,
                        |editor, window, cx| {
                            editor.close_find(cx);
                            window.focus(&editor.focus, cx);
                        },
                    )),
            );
        if replace_open {
            let text_button =
                |id: &'static str,
                 label: &'static str,
                 cx: &mut Context<Self>,
                 action: fn(&mut CodeEditor, &mut Context<CodeEditor>)| {
                    div()
                        .id(id)
                        .h(px(22.0))
                        .px(px(7.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .rounded(px(Radius::CHIP))
                        .font_family(crate::fonts::ui_family())
                        .text_size(px(10.5))
                        .text_color(if found {
                            colors.secondary
                        } else {
                            colors.tertiary
                        })
                        .when(found, |button| {
                            button
                                .cursor_pointer()
                                .hover(move |button| button.bg(colors.primary.alpha(0.08)))
                                .on_mouse_down(MouseButton::Left, |_, window, _| {
                                    window.prevent_default()
                                })
                                .on_click(cx.listener(move |editor, _, _, cx| action(editor, cx)))
                        })
                        .child(label)
                };
            bar = bar.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .pl(px(25.0))
                    .child(field(
                        "find-replacement",
                        &find.replacement,
                        in_replacement,
                        "Replace",
                        true,
                        cx,
                    ))
                    .child(text_button("replace-one", "Replace", cx, |editor, cx| {
                        editor.replace_current(cx)
                    }))
                    .child(text_button("replace-all", "All", cx, |editor, cx| {
                        editor.replace_all(cx)
                    })),
            );
        }
        bar.into_any_element()
    }

    /// The local position of a byte offset inside the code area.
    fn local_point(&self, offset: usize) -> Option<gpui::Point<Pixels>> {
        let doc = self.doc.as_ref()?;
        let line = doc.buffer.line_of(offset);
        let row = self.row_of_line(line);
        let text = doc.buffer.line(line);
        let column = syntax::column_at(text, offset - doc.buffer.line_range(line).start, self.tab);
        let scrolled = -self.scroll.0.borrow().base_handle.offset().y;
        Some(point(
            self.gutter_width(self.metrics.advance)
                + px(CODE_PAD)
                + self.metrics.advance * column as f32
                - self.h_offset,
            self.metrics.line * row as f32 - scrolled,
        ))
    }

    fn card(&self) -> gpui::Div {
        let colors = self.colors;
        div()
            .absolute()
            .occlude()
            .flex()
            .flex_col()
            .rounded(px(Radius::BADGE + 2.0))
            .bg(colors.floating_surface())
            .border_1()
            .border_color(colors.floating_stroke())
            .shadow(shadow(
                if self.palette.light { 0.10 } else { 0.35 },
                4.0,
                14.0,
            ))
            .text_color(colors.primary)
            .overflow_hidden()
    }

    /// A declaration line with its syntax colors.
    fn code_line(&self, text: &str) -> AnyElement {
        let language = self
            .doc
            .as_ref()
            .map_or(crate::code_intelligence::SourceLanguage::PlainText, |doc| {
                doc.language
            });
        let (tokens, _) =
            syntax::lex_line(text, syntax::LexState::Normal, syntax::grammar(language));
        let styles: Vec<(Range<usize>, HighlightStyle)> = tokens
            .into_iter()
            .filter(|token| !matches!(token.kind, TokenKind::Bracket | TokenKind::Punctuation))
            .map(|token| {
                (
                    token.range,
                    HighlightStyle {
                        color: Some(self.palette.token(token.kind).into()),
                        ..HighlightStyle::default()
                    },
                )
            })
            .collect();
        StyledText::new(SharedString::from(text.to_owned()))
            .with_highlights(styles)
            .into_any_element()
    }

    fn hover_card(&self, width: Pixels, cx: &mut Context<Self>) -> Option<AnyElement> {
        let hover = self.hover.as_ref()?;
        let at = self.local_point(hover.word.start)?;
        let colors = self.colors;
        let palette = self.palette;
        let shown = hover
            .definitions
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>();
        let more = hover.definitions.len().saturating_sub(shown.len());
        let card_width = px(380.0).min(width - px(16.0));
        let left = at.x.min(width - card_width - px(8.0)).max(px(8.0));
        let rows: Vec<AnyElement> = shown
            .into_iter()
            .enumerate()
            .map(|(ix, hit)| {
                let path = hit.relative_path.clone();
                let line = hit.line.unwrap_or(1);
                div()
                    .id(("hover-definition", ix))
                    .px(px(10.0))
                    .py(px(6.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .when(ix > 0, |row| {
                        row.border_t_1().border_color(colors.primary.alpha(0.06))
                    })
                    .cursor_pointer()
                    .hover(move |row| row.bg(colors.primary.alpha(0.05)))
                    .child(
                        div()
                            .font_family(self.font_family.clone())
                            .text_size(px(11.5))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(self.code_line(&hit.preview)),
                    )
                    .child(
                        div()
                            .font_family(crate::fonts::ui_family())
                            .text_size(px(10.0))
                            .text_color(colors.tertiary)
                            .truncate()
                            .child(format!("{}:{line}", path.display())),
                    )
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |editor, _, _, cx| {
                        editor.hover = None;
                        let current = editor.relative_path().map(std::path::Path::to_path_buf);
                        if current.as_ref() == Some(&path) {
                            editor.go_to(line, 1, cx);
                        } else {
                            cx.emit(EditorEvent::OpenLocation {
                                path: path.clone(),
                                line,
                            });
                        }
                    }))
                    .into_any_element()
            })
            .collect();
        let line = self.metrics.line;
        let below = at.y < line * 6.0;
        let card = self
            .card()
            .id("hover-card")
            .left(left)
            .w(card_width)
            .when(below, |card| card.top(at.y + line + px(2.0)))
            .child(
                div()
                    .px(px(10.0))
                    .pt(px(7.0))
                    .pb(px(2.0))
                    .font_family(crate::fonts::ui_family())
                    .text_size(px(10.0))
                    .text_color(colors.tertiary)
                    .child(format!(
                        "{} — {} declaration{}",
                        hover.name,
                        hover.definitions.len(),
                        if hover.definitions.len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    )),
            )
            .children(rows)
            .when(more > 0, |card| {
                card.child(
                    div()
                        .px(px(10.0))
                        .py(px(5.0))
                        .text_size(px(10.0))
                        .text_color(colors.tertiary)
                        .child(format!("and {more} more · ⌘-click to go to definition")),
                )
            })
            .on_mouse_move(|_, _, cx| cx.stop_propagation());
        let _ = palette;
        Some(if below {
            card.into_any_element()
        } else {
            // Anchor above the word: a zero-height wrapper at the word's top.
            div()
                .absolute()
                .top(at.y - px(2.0))
                .left_0()
                .right_0()
                .h(px(0.0))
                .child(card.bottom(px(0.0)))
                .into_any_element()
        })
    }

    fn completion_menu(&self, width: Pixels, cx: &mut Context<Self>) -> Option<AnyElement> {
        let completion = self.completion.as_ref()?;
        let at = self.local_point(completion.word.start)?;
        let colors = self.colors;
        let palette = self.palette;
        let menu_width = px(300.0).min(width - px(16.0));
        let left = at.x.min(width - menu_width - px(8.0)).max(px(4.0));
        let items: Vec<AnyElement> = completion
            .items
            .iter()
            .enumerate()
            .map(|(ix, item)| {
                let selected = ix == completion.selected;
                div()
                    .id(("completion", ix))
                    .h(px(22.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .rounded(px(Radius::CHIP))
                    .when(selected, |row| row.bg(palette.function.alpha(0.18)))
                    .cursor_pointer()
                    .hover(move |row| row.bg(colors.primary.alpha(0.06)))
                    .child(
                        div()
                            .flex_none()
                            .w(px(14.0))
                            .text_center()
                            .text_size(px(9.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(match item.kind {
                                CandidateKind::Symbol => palette.function,
                                CandidateKind::Word => colors.tertiary,
                            })
                            .child(match item.kind {
                                CandidateKind::Symbol => "ƒ",
                                CandidateKind::Word => "ab",
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(self.font_family.clone())
                            .text_size(px(11.5))
                            .whitespace_nowrap()
                            .child(item.label.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .text_right()
                            .font_family(crate::fonts::ui_family())
                            .text_size(px(10.0))
                            .text_color(colors.tertiary)
                            .child(item.detail.clone().unwrap_or_default()),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |editor, _, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                            editor.accept_completion_at(ix, cx);
                        }),
                    )
                    .into_any_element()
            })
            .collect();
        let line = self.metrics.line;
        Some(
            self.card()
                .id("completion-menu")
                .left(left)
                .top(at.y + line + px(2.0))
                .w(menu_width)
                .p(px(3.0))
                .children(items)
                .into_any_element(),
        )
    }

    fn signature_card(&self, width: Pixels) -> Option<AnyElement> {
        let signature = self.signature.as_ref()?;
        let doc = self.doc.as_ref()?;
        let head = self.primary().head;
        let line_start = doc.buffer.line_range(doc.buffer.line_of(head)).start;
        let at = self.local_point(line_start + doc.buffer.indent(doc.buffer.line_of(head)))?;
        let colors = self.colors;
        let declaration = signature.declaration.trim().to_string();
        let parameters = super::intel::parameters(&declaration).unwrap_or_default();
        let mut styled: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        if let Some(active) = parameters.get(signature.argument) {
            styled.push((
                active.clone(),
                HighlightStyle {
                    color: Some(self.palette.function.into()),
                    font_weight: Some(FontWeight::BOLD),
                    underline: Some(gpui::UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(self.palette.function.into()),
                        wavy: false,
                    }),
                    ..HighlightStyle::default()
                },
            ));
        }
        let line = self.metrics.line;
        let card_width = px(420.0).min(width - px(16.0));
        let above = at.y > line * 2.0;
        let card = self
            .card()
            .id("signature-help")
            .left(at.x.min(width - card_width - px(8.0)).max(px(4.0)))
            .max_w(card_width)
            .px(px(9.0))
            .py(px(5.0))
            .gap(px(2.0))
            .child(
                div()
                    .font_family(self.font_family.clone())
                    .text_size(px(11.5))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(
                        StyledText::new(SharedString::from(declaration)).with_highlights(styled),
                    ),
            )
            .child(
                div()
                    .font_family(crate::fonts::ui_family())
                    .text_size(px(9.5))
                    .text_color(colors.tertiary)
                    .child(format!(
                        "{}:{} · parameter {}",
                        signature.location.0.display(),
                        signature.location.1,
                        signature.argument + 1
                    )),
            );
        Some(if above {
            div()
                .absolute()
                .top(at.y - px(3.0))
                .left_0()
                .right_0()
                .h(px(0.0))
                .child(card.bottom(px(0.0)))
                .into_any_element()
        } else {
            card.top(at.y + line + px(3.0)).into_any_element()
        })
    }

    fn banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = self.colors;
        let palette = self.palette;
        let (message, actions): (String, bool) = if self.conflict {
            (
                "This file changed on disk since it was opened.".into(),
                true,
            )
        } else {
            (self.save_error.clone()?, false)
        };
        let button = |id: &'static str,
                      label: &'static str,
                      cx: &mut Context<Self>,
                      action: fn(&mut CodeEditor, &mut Context<CodeEditor>)| {
            div()
                .id(id)
                .h(px(20.0))
                .px(px(8.0))
                .flex()
                .items_center()
                .rounded(px(Radius::CHIP))
                .bg(colors.primary.alpha(0.07))
                .cursor_pointer()
                .hover(move |button| button.bg(colors.primary.alpha(0.12)))
                .child(label)
                .on_click(cx.listener(move |editor, _, _, cx| action(editor, cx)))
        };
        Some(
            div()
                .flex_none()
                .h(px(30.0))
                .px(px(10.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .bg(palette.warning.alpha(0.10))
                .border_b_1()
                .border_color(palette.warning.alpha(0.25))
                .font_family(crate::fonts::ui_family())
                .text_size(px(11.0))
                .text_color(colors.primary)
                .child(sf_symbol("exclamationmark.triangle", 11.0, palette.warning))
                .child(div().flex_1().min_w(px(0.0)).truncate().child(message))
                .when(actions, |bar| {
                    bar.child(button("conflict-reload", "Reload", cx, |editor, cx| {
                        editor.reload(cx)
                    }))
                    .child(button(
                        "conflict-overwrite",
                        "Overwrite",
                        cx,
                        |editor, cx| editor.save(true, cx),
                    ))
                })
                .into_any_element(),
        )
    }
}

impl Render for CodeEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        self.sync_focus(focused, cx);
        self.refresh_layout();
        self.refresh_find_matches();
        let palette = self.palette;
        let font = gpui::font(self.font_family.clone());
        let font_id = window.text_system().resolve_font(&font);
        let advance = window
            .text_system()
            .advance(font_id, self.font_size, 'm')
            .map(|size| size.width)
            .unwrap_or(self.font_size * 0.6);
        let line = (self.font_size * LEADING).round();
        self.metrics.advance = advance;
        self.metrics.line = line;
        if self.doc.is_none() {
            return div().size_full().bg(palette.background).into_any_element();
        }
        let gutter = self.gutter_width(advance);
        let count = self.layout.rows.len();
        let list = uniform_list(
            "editor-rows",
            count,
            cx.processor(|editor, range: Range<usize>, window, cx| {
                range.map(|ix| editor.row(ix, window, cx)).collect()
            }),
        )
        .track_scroll(&self.scroll)
        .size_full();
        let sticky = self.sticky(window, cx);
        let area_width = self.metrics.width + gutter + px(CODE_PAD);
        let hover = self.hover_card(area_width, cx);
        let completion = self.completion_menu(area_width, cx);
        let signature = if completion.is_none() {
            self.signature_card(area_width)
        } else {
            None
        };
        let (metrics_entity, input_entity, wheel_entity) = (cx.entity(), cx.entity(), cx.entity());
        let focus = self.focus.clone();
        let content_width = advance * (self.analysis.widest + 8) as f32;
        let code_area = div()
            .id("code-area")
            .key_context(EDITOR_CONTEXT)
            .track_focus(&self.focus)
            .group("editor-gutter")
            .relative()
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .overflow_hidden()
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|editor, event: &MouseDownEvent, window, cx| {
                    editor.press(event, window, cx)
                }),
            )
            .on_mouse_move(
                cx.listener(|editor, event: &MouseMoveEvent, _, cx| {
                    editor.pointer_moved(event, cx)
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|editor, _, _, _| editor.dragging = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|editor, _, _, _| editor.dragging = false),
            )
            .on_hover(cx.listener(|editor, hovered: &bool, _, cx| {
                if !hovered {
                    editor.hover_at(None, cx);
                }
            }))
            .child(list)
            .children(sticky)
            .child(
                canvas(
                    move |bounds, _, cx| {
                        metrics_entity.update(cx, |editor, _| {
                            editor.metrics.top = bounds.origin.y;
                            editor.metrics.left =
                                bounds.origin.x + gutter + px(CODE_PAD) - editor.h_offset;
                            editor.metrics.width =
                                (bounds.size.width - gutter - px(CODE_PAD)).max(px(0.0));
                        });
                    },
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, input_entity.clone()),
                            cx,
                        );
                        let wheel = wheel_entity.clone();
                        window.on_mouse_event(
                            move |event: &ScrollWheelEvent, phase, window, cx| {
                                if phase != DispatchPhase::Capture
                                    || !bounds.contains(&event.position)
                                {
                                    return;
                                }
                                let delta = event.delta.pixel_delta(px(18.0));
                                if delta.x.abs() <= delta.y.abs() || delta.x == px(0.0) {
                                    return;
                                }
                                wheel.update(cx, |editor, cx| {
                                    let visible = editor.metrics.width;
                                    let limit = (content_width - visible).max(px(0.0));
                                    let next = (editor.h_offset - delta.x).clamp(px(0.0), limit);
                                    if next != editor.h_offset {
                                        editor.h_offset = next;
                                        cx.notify();
                                    }
                                });
                                window.refresh();
                                cx.stop_propagation();
                            },
                        );
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .children(hover)
            .children(completion)
            .children(signature);
        let code_area = listen(code_area, cx);
        let show_minimap = self.minimap && self.metrics.width + gutter >= px(MINIMAP_MIN_WIDTH);
        let minimap = show_minimap.then(|| self.minimap_view(cx));
        let banner = self.banner(cx);
        let find_focused = self.find_focus.is_focused(window);
        let find = self.find.open.then(|| self.find_bar(find_focused, cx));
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(palette.background)
            .font_family(self.font_family.clone())
            .text_size(self.font_size)
            .text_color(palette.foreground)
            .children(banner)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex()
                    .child(code_area)
                    .children(minimap)
                    .children(find.map(|find| {
                        div()
                            .absolute()
                            .top(px(6.0))
                            .right(px(if show_minimap {
                                MINIMAP_WIDTH + 8.0
                            } else {
                                10.0
                            }))
                            .left(px(10.0))
                            .flex()
                            .justify_end()
                            .child(find)
                    })),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_expand_tabs_and_clip_to_visible_columns() {
        let slice = Slice::new("\tab\tc", 4, 0, 100);
        assert_eq!(slice.text, "    ab  c");
        assert_eq!(slice.map(1..3), Some(4..6), "a and b after the tab");
        assert_eq!(
            slice.map(3..4),
            Some(6..8),
            "the second tab fills to its stop"
        );
        assert_eq!(slice.map(0..5), Some(0..9));

        let clipped = Slice::new("0123456789", 4, 3, 6);
        assert_eq!(clipped.text, "345");
        assert_eq!(clipped.first_column, 3);
        assert_eq!(clipped.map(0..4), Some(0..1), "clipped at the left edge");
        assert_eq!(clipped.map(5..9), Some(2..3), "and at the right");
        assert_eq!(clipped.map(7..9), None);

        let inside_tab = Slice::new("\tx", 4, 2, 10);
        assert_eq!(
            inside_tab.text, "  x",
            "a tab cut by the edge keeps its visible part"
        );
        assert_eq!(inside_tab.first_column, 2);
        assert_eq!(inside_tab.map(1..2), Some(2..3));
    }

    #[test]
    fn slices_keep_multibyte_characters_whole() {
        let slice = Slice::new("aπb", 4, 0, 10);
        assert_eq!(slice.map(1..3), Some(1..3));
        assert_eq!(slice.map(3..4), Some(3..4));
        let empty = Slice::new("", 4, 0, 10);
        assert_eq!(empty.map(0..0), None);
    }
}
