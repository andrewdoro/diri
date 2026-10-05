// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//
//! The review's diff rows: Ely's `DiffViewer` look, drawn from diri's
//! [`DiffSnapshot`].
//!
//! Each file opens with a header (status badge, path, +/− stat, collapse); each
//! hunk with its `@@` range and enclosing context. Lines carry old and new
//! numbers in a quiet gutter, a sign, and the code in the terminal's font with
//! its changed words washed. Inline layout draws one column; split layout puts
//! the old file beside the new one. The list is virtualized by its owner: this
//! module only turns a range of [`ViewRow`]s into elements, so a 100k-line
//! diff builds the few dozen rows on screen.
//!
//! Interaction is reported through [`DiffHandlers`]; the owner decides what a
//! stage, discard, or ask means.

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use diri_ui::Radius;
use gpui::{
    AnyElement, App, ClickEvent, FontWeight, HighlightStyle, MouseButton, Rgba, SharedString,
    StyledText, Window, div, prelude::*, px,
};

use super::palette::DiffPalette;
use super::rows::{DiffLayout, ViewRow};
use crate::diff::{DiffFile, DiffFileStatus, DiffHunk, DiffLayer, DiffRowKind, DiffSnapshot};
use crate::icons::sf_symbol;

/// Every list row has this height; `uniform_list` measures only the first.
pub(crate) const ROW_HEIGHT: f32 = 20.0;
/// Code size inside the review, a step under the terminal's default.
pub(crate) const CODE_SIZE: f32 = 11.5;
/// Approximate advance of one monospace column at [`CODE_SIZE`].
const COLUMN_WIDTH: f32 = 6.95;
const SIGN_WIDTH: f32 = 14.0;
const DIGIT_WIDTH: f32 = 6.6;
/// Omitted file names sit under the notice text, past its disclosure chevron.
const OMITTED_PATH_INDENT: f32 = 15.0;
const ROW_GROUP: &str = "review-diff-row";
const TAB_STOP: usize = 4;

/// What a file header can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileAction {
    Toggle,
    Open,
    Ask,
    Stage,
    Unstage,
}

/// What a hunk header can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HunkAction {
    Ask,
    Stage,
    Unstage,
    Discard,
}

/// What an omitted file name can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OmittedAction {
    Open,
    Stage,
}

type RowHandler = Box<dyn Fn(usize, bool, &mut Window, &mut App)>;
type FileHandler = Box<dyn Fn(usize, FileAction, &mut Window, &mut App)>;
type HunkHandler = Box<dyn Fn(usize, usize, HunkAction, &mut Window, &mut App)>;
type NoticeHandler = Box<dyn Fn(&mut Window, &mut App)>;
type OmittedHandler = Box<dyn Fn(usize, OmittedAction, &mut Window, &mut App)>;

/// The owner's answers to clicks in the diff. Indices are snapshot rows,
/// file indices into [`DiffSnapshot::file_diffs`], and hunk indices into that
/// file's hunks.
pub(crate) struct DiffHandlers {
    pub select_row: RowHandler,
    pub file: FileHandler,
    pub hunk: HunkHandler,
    pub toggle_omitted: NoticeHandler,
    pub omitted: OmittedHandler,
}

/// Everything one frame of the diff needs, cheap to clone into the list's
/// render callback.
#[derive(Clone)]
pub(crate) struct DiffViewProps {
    pub snapshot: Arc<DiffSnapshot>,
    pub rows: Arc<Vec<ViewRow>>,
    pub layout: DiffLayout,
    pub palette: DiffPalette,
    pub font: SharedString,
    pub selection: Option<Range<usize>>,
    pub armed_hunk: Option<u64>,
    pub omitted_open: bool,
    pub collapsed: Arc<HashSet<PathBuf>>,
    /// Whether Ask is offered (any session that can take a prompt).
    pub can_ask: bool,
    /// Whether the review may stage, unstage, or discard (a local lane).
    pub mutable: bool,
    pub digits: usize,
    pub content_width: f32,
    pub handlers: Rc<DiffHandlers>,
}

impl DiffViewProps {
    /// Width of one line-number column.
    fn number_width(&self) -> f32 {
        self.digits as f32 * DIGIT_WIDTH + 12.0
    }

    /// Width of the inline gutter: both numbers and the sign.
    fn gutter_width(&self) -> f32 {
        self.number_width() * 2.0
    }
}

/// The widest an inline row gets, so the list can scroll sideways to the end
/// of the longest line. Split rows always fit their column instead.
#[must_use]
pub(crate) fn inline_content_width(digits: usize, text_columns: usize) -> f32 {
    let gutter = (digits as f32 * DIGIT_WIDTH + 12.0) * 2.0;
    (gutter + SIGN_WIDTH + 24.0 + text_columns as f32 * COLUMN_WIDTH).clamp(320.0, 4_200.0)
}

/// Builds the elements for `range` of the view rows.
pub(crate) fn render_rows(props: &DiffViewProps, range: Range<usize>) -> Vec<AnyElement> {
    range
        .map(|position| match props.rows[position] {
            ViewRow::Gap => div()
                .id(("review-diff-gap", position))
                .h(px(ROW_HEIGHT))
                .w_full()
                .min_w(px(row_min_width(props)))
                .into_any_element(),
            ViewRow::Row(row) => render_full_row(props, position, row),
            ViewRow::Pair { left, right } => render_pair(props, position, left, right),
            ViewRow::OmittedPath(ordinal) => render_omitted_path(props, position, ordinal),
        })
        .collect()
}

fn row_min_width(props: &DiffViewProps) -> f32 {
    match props.layout {
        DiffLayout::Inline => props.content_width,
        DiffLayout::Split => 0.0,
    }
}

fn render_full_row(props: &DiffViewProps, position: usize, row: usize) -> AnyElement {
    match props.snapshot.rows[row].kind {
        DiffRowKind::File => render_file_header(props, position, row),
        DiffRowKind::Hunk => render_hunk_header(props, position, row),
        DiffRowKind::Context | DiffRowKind::Addition | DiffRowKind::Deletion => {
            render_inline_line(props, position, row)
        }
        DiffRowKind::Meta => render_meta(props, position, row),
    }
}

/// The file and hunk a header row opens, by index.
fn locate(snapshot: &DiffSnapshot, row: usize) -> (Option<usize>, Option<usize>) {
    let files = &snapshot.file_diffs;
    let at = files.partition_point(|file| file.row_range.start <= row);
    let Some(file_index) = at.checked_sub(1) else {
        return (None, None);
    };
    let file = &files[file_index];
    if !file.row_range.contains(&row) {
        return (None, None);
    }
    let hunk = file
        .hunks
        .iter()
        .position(|hunk| hunk.row_range.start == row);
    (Some(file_index), hunk)
}

fn status_letter(status: DiffFileStatus) -> (&'static str, &'static str) {
    match status {
        DiffFileStatus::Modified => ("M", "Modified"),
        DiffFileStatus::Added => ("A", "Added"),
        DiffFileStatus::Deleted => ("D", "Deleted"),
        DiffFileStatus::Renamed => ("R", "Renamed"),
        DiffFileStatus::Copied => ("C", "Copied"),
    }
}

fn status_hue(status: DiffFileStatus, palette: &DiffPalette) -> Rgba {
    match status {
        DiffFileStatus::Added => palette.added,
        DiffFileStatus::Deleted => palette.removed,
        DiffFileStatus::Modified => palette.modified,
        DiffFileStatus::Renamed | DiffFileStatus::Copied => palette.accent,
    }
}

/// A file's git status as its letter in its tone.
pub(crate) fn status_badge(status: DiffFileStatus, palette: &DiffPalette) -> AnyElement {
    let hue = status_hue(status, palette);
    let (letter, _) = status_letter(status);
    div()
        .flex_none()
        .w(px(15.0))
        .h(px(14.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.5))
        .bg(hue.alpha(0.14))
        .text_size(px(9.0))
        .font_weight(FontWeight::BOLD)
        .text_color(hue)
        .child(letter)
        .into_any_element()
}

/// Dots a diff stat shows.
const STAT_SQUARES: usize = 5;

/// How many of the dots read as added, the rest as removed, when anything
/// changed.
#[must_use]
pub(crate) fn stat_split(added: usize, removed: usize) -> Option<usize> {
    let total = added + removed;
    (total > 0).then(|| (added * STAT_SQUARES + total / 2) / total)
}

/// Lines added and removed: the two counts, and five dots shared between them.
pub(crate) fn diff_stat(added: usize, removed: usize, palette: &DiffPalette) -> AnyElement {
    let green = stat_split(added, removed);
    let square = |index: usize| {
        let color = match green {
            Some(green) if index < green => palette.added,
            Some(_) => palette.removed,
            None => palette.rule,
        };
        div().size(px(6.0)).rounded(px(1.5)).bg(color)
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(5.0))
        .text_size(px(10.0))
        .font_weight(FontWeight::MEDIUM)
        .child(div().text_color(palette.added).child(format!("+{added}")))
        .child(
            div()
                .text_color(palette.removed)
                .child(format!("−{removed}")),
        )
        .child(
            div()
                .flex()
                .gap(px(1.5))
                .children((0..STAT_SQUARES).map(square)),
        )
        .into_any_element()
}

/// A small text button for a header's action cluster.
fn action_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    color: Rgba,
    hover: Rgba,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h(px(16.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .cursor_pointer()
        .hover(move |button| button.bg(hover))
        .text_size(px(9.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(label)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |event, window, cx| {
            on_click(event, window, cx);
            cx.stop_propagation();
        })
        .into_any_element()
}

/// The hover-revealed actions of a header row.
fn action_cluster(visible: bool) -> gpui::Div {
    div()
        .flex_none()
        .ml(px(6.0))
        .flex()
        .items_center()
        .gap(px(1.0))
        .when(!visible, |cluster| {
            cluster
                .invisible()
                .group_hover(ROW_GROUP, |cluster| cluster.visible())
        })
}

fn file_actions(
    props: &DiffViewProps,
    position: usize,
    file_index: usize,
    selected: bool,
) -> gpui::Div {
    let palette = props.palette;
    let mut cluster = action_cluster(selected);
    if props.can_ask {
        let handlers = props.handlers.clone();
        cluster = cluster.child(action_button(
            ("ask-diff-file", position),
            crate::i18n::t("git.diff.ask"),
            palette.accent,
            palette.hover,
            move |_, window, cx| (handlers.file)(file_index, FileAction::Ask, window, cx),
        ));
    }
    if props.mutable {
        match props.snapshot.layer {
            DiffLayer::Working => {
                let handlers = props.handlers.clone();
                cluster = cluster.child(action_button(
                    ("stage-diff-file", position),
                    crate::i18n::t("git.diff.stage"),
                    palette.secondary,
                    palette.hover,
                    move |_, window, cx| (handlers.file)(file_index, FileAction::Stage, window, cx),
                ));
            }
            DiffLayer::Staged => {
                let handlers = props.handlers.clone();
                cluster = cluster.child(action_button(
                    ("unstage-diff-file", position),
                    crate::i18n::t("panel.review.unstage"),
                    palette.secondary,
                    palette.hover,
                    move |_, window, cx| {
                        (handlers.file)(file_index, FileAction::Unstage, window, cx)
                    },
                ));
            }
            DiffLayer::Branch => {}
        }
    }
    let handlers = props.handlers.clone();
    cluster.child(action_button(
        ("open-diff-file", position),
        crate::i18n::t("git.diff.open"),
        palette.secondary,
        palette.hover,
        move |_, window, cx| (handlers.file)(file_index, FileAction::Open, window, cx),
    ))
}

fn render_file_header(props: &DiffViewProps, position: usize, row: usize) -> AnyElement {
    let palette = props.palette;
    let (Some(file_index), _) = locate(&props.snapshot, row) else {
        return render_meta(props, position, row);
    };
    let file: &DiffFile = &props.snapshot.file_diffs[file_index];
    let collapsed = props.collapsed.contains(&file.path);
    let path = file.path.to_string_lossy().into_owned();
    let (directory, name) = match path.rfind('/') {
        Some(slash) => (path[..=slash].to_owned(), path[slash + 1..].to_owned()),
        None => (String::new(), path.clone()),
    };
    let renamed_from = file
        .old_path
        .as_ref()
        .map(|old| old.to_string_lossy().into_owned());
    let toggle = props.handlers.clone();
    let open = props.handlers.clone();
    div()
        .id(("review-diff-file", position))
        .group(ROW_GROUP)
        .h(px(ROW_HEIGHT))
        .w_full()
        .min_w(px(row_min_width(props)))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .bg(palette.file)
        .border_t_1()
        .border_b_1()
        .border_color(palette.rule)
        .cursor_pointer()
        .hover(move |row| row.bg(palette.hover))
        .on_click(move |_, window, cx| {
            (toggle.file)(file_index, FileAction::Toggle, window, cx);
            cx.stop_propagation();
        })
        .child(sf_symbol(
            if collapsed {
                "chevron.right"
            } else {
                "chevron.down"
            },
            8.5,
            palette.muted,
        ))
        .child(status_badge(file.status, &palette))
        .child(
            div()
                .id(("review-diff-file-path", position))
                .flex_none()
                .flex()
                .items_center()
                .text_size(px(11.0))
                .cursor_pointer()
                .hover(|path| path.underline())
                .on_click(move |_, window, cx| {
                    (open.file)(file_index, FileAction::Open, window, cx);
                    cx.stop_propagation();
                })
                .when_some(renamed_from, |path, from| {
                    path.child(div().text_color(palette.muted).child(format!("{from} → ")))
                })
                .when(!directory.is_empty(), |path| {
                    path.child(div().text_color(palette.muted).child(directory))
                })
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(palette.text)
                        .child(name),
                ),
        )
        .child(diff_stat(file.additions, file.deletions, &palette))
        .child(file_actions(props, position, file_index, false))
        .into_any_element()
}

/// Splits `@@ -a,b +c,d @@ context` into the range and the context.
fn hunk_parts(header: &str) -> (&str, &str) {
    let Some(rest) = header.strip_prefix("@@") else {
        return (header, "");
    };
    match rest.find("@@") {
        Some(end) => {
            let split = 2 + end + 2;
            (&header[..split], header[split..].trim())
        }
        None => (header, ""),
    }
}

fn render_hunk_header(props: &DiffViewProps, position: usize, row: usize) -> AnyElement {
    let palette = props.palette;
    let (file_index, hunk_index) = locate(&props.snapshot, row);
    let selected = props
        .selection
        .as_ref()
        .is_some_and(|range| range.start == row);
    let text = &props.snapshot.rows[row].text;
    let (range, context) = hunk_parts(text);
    let hunk: Option<(usize, usize, &DiffHunk)> = file_index
        .zip(hunk_index)
        .map(|(file, hunk)| (file, hunk, &props.snapshot.file_diffs[file].hunks[hunk]));
    let armed = hunk.is_some_and(|(_, _, hunk)| props.armed_hunk == Some(hunk.fingerprint));
    let mut actions = action_cluster(selected || armed);
    if let Some((file, hunk_at, hunk)) = hunk {
        if props.can_ask {
            let handlers = props.handlers.clone();
            actions = actions.child(action_button(
                ("ask-diff-hunk", position),
                crate::i18n::t("git.diff.ask"),
                palette.accent,
                palette.hover,
                move |_, window, cx| (handlers.hunk)(file, hunk_at, HunkAction::Ask, window, cx),
            ));
        }
        if props.mutable {
            match props.snapshot.layer {
                DiffLayer::Working => {
                    let handlers = props.handlers.clone();
                    actions = actions.child(action_button(
                        ("stage-diff-hunk", position),
                        crate::i18n::t("git.diff.stage"),
                        palette.secondary,
                        palette.hover,
                        move |_, window, cx| {
                            (handlers.hunk)(file, hunk_at, HunkAction::Stage, window, cx)
                        },
                    ));
                    if !crate::git_ui::patch_creates_file(&hunk.patch) {
                        let handlers = props.handlers.clone();
                        actions = actions.child(action_button(
                            ("discard-diff-hunk", position),
                            if armed {
                                crate::i18n::t("git.diff.confirm_discard")
                            } else {
                                crate::i18n::t("panel.review.discard")
                            },
                            palette.removed,
                            palette.removed_line,
                            move |_, window, cx| {
                                (handlers.hunk)(file, hunk_at, HunkAction::Discard, window, cx)
                            },
                        ));
                    }
                }
                DiffLayer::Staged => {
                    let handlers = props.handlers.clone();
                    actions = actions.child(action_button(
                        ("unstage-diff-hunk", position),
                        crate::i18n::t("panel.review.unstage"),
                        palette.secondary,
                        palette.hover,
                        move |_, window, cx| {
                            (handlers.hunk)(file, hunk_at, HunkAction::Unstage, window, cx)
                        },
                    ));
                }
                DiffLayer::Branch => {}
            }
        }
    }
    let select = props.handlers.clone();
    div()
        .id(("review-diff-hunk", position))
        .group(ROW_GROUP)
        .relative()
        .h(px(ROW_HEIGHT))
        .w_full()
        .min_w(px(row_min_width(props)))
        .flex()
        .items_center()
        .bg(if selected {
            palette.selection
        } else {
            palette.hunk
        })
        .cursor_pointer()
        .on_click(move |event: &ClickEvent, window, cx| {
            (select.select_row)(row, event.modifiers().shift, window, cx);
            cx.stop_propagation();
        })
        .when(selected, |line| line.child(selection_edge(&palette)))
        .child(
            div()
                .flex_none()
                .w(px(match props.layout {
                    DiffLayout::Inline => props.gutter_width(),
                    DiffLayout::Split => props.number_width(),
                }))
                .h_full()
                .flex()
                .items_center()
                .justify_end()
                .pr(px(8.0))
                .child(sf_symbol("ellipsis", 10.0, palette.muted)),
        )
        .child(
            div()
                .pl(px(SIGN_WIDTH - 6.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .whitespace_nowrap()
                .font_family(props.font.clone())
                .text_size(px(CODE_SIZE - 0.5))
                .child(
                    div()
                        .flex_none()
                        .text_color(palette.muted)
                        .child(range.to_owned()),
                )
                .when(!context.is_empty(), |header| {
                    header.child(
                        div()
                            .flex_none()
                            .text_color(palette.secondary)
                            .child(context.to_owned()),
                    )
                }),
        )
        .child(actions)
        .into_any_element()
}

fn selection_edge(palette: &DiffPalette) -> AnyElement {
    div()
        .absolute()
        .left_0()
        .top_0()
        .bottom_0()
        .w(px(2.0))
        .bg(palette.selection_edge)
        .into_any_element()
}

struct LineLook {
    sign: &'static str,
    sign_color: Rgba,
    line: Option<Rgba>,
    gutter: Option<Rgba>,
    word: Rgba,
}

fn line_look(kind: DiffRowKind, palette: &DiffPalette) -> LineLook {
    match kind {
        DiffRowKind::Addition => LineLook {
            sign: "+",
            sign_color: palette.added,
            line: Some(palette.added_line),
            gutter: Some(palette.added_gutter),
            word: palette.added_word,
        },
        DiffRowKind::Deletion => LineLook {
            sign: "−",
            sign_color: palette.removed,
            line: Some(palette.removed_line),
            gutter: Some(palette.removed_gutter),
            word: palette.removed_word,
        },
        _ => LineLook {
            sign: "",
            sign_color: palette.muted,
            line: None,
            gutter: None,
            word: palette.hover,
        },
    }
}

/// A line's code with tabs expanded, its changed words washed.
fn code(text: &str, words: &[Range<usize>], wash: Rgba) -> StyledText {
    let (text, words) = expand_tabs(text, words);
    let highlights: Vec<(Range<usize>, HighlightStyle)> = words
        .into_iter()
        .filter(|range| range.start < range.end)
        .map(|range| {
            (
                range,
                HighlightStyle {
                    background_color: Some(wash.into()),
                    ..HighlightStyle::default()
                },
            )
        })
        .collect();
    StyledText::new(text).with_highlights(highlights)
}

/// Expands tabs to the next [`TAB_STOP`] column and moves byte ranges with
/// the text they cover.
fn expand_tabs(text: &str, ranges: &[Range<usize>]) -> (String, Vec<Range<usize>>) {
    if !text.contains('\t') {
        return (text.to_owned(), ranges.to_vec());
    }
    let mut expanded = String::with_capacity(text.len() + 8);
    // `moved[i]` is where byte `i` of `text` lands; filled at char starts and
    // at the end.
    let mut moved = vec![0; text.len() + 1];
    let mut column = 0;
    for (at, character) in text.char_indices() {
        moved[at] = expanded.len();
        if character == '\t' {
            let spaces = TAB_STOP - column % TAB_STOP;
            expanded.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else {
            expanded.push(character);
            column += 1;
        }
    }
    moved[text.len()] = expanded.len();
    let ranges = ranges
        .iter()
        .filter(|range| range.end <= text.len())
        .map(|range| moved[range.start]..moved[range.end])
        .collect();
    (expanded, ranges)
}

fn line_number(value: Option<u32>, width: f32, color: Rgba) -> gpui::Div {
    div()
        .flex_none()
        .w(px(width))
        .h_full()
        .pr(px(6.0))
        .flex()
        .items_center()
        .justify_end()
        .text_color(color)
        .children(value.map(|value| value.to_string()))
}

fn render_inline_line(props: &DiffViewProps, position: usize, row: usize) -> AnyElement {
    let palette = props.palette;
    let line = &props.snapshot.rows[row];
    let look = line_look(line.kind, &palette);
    let selected = props
        .selection
        .as_ref()
        .is_some_and(|range| range.contains(&row));
    let select = props.handlers.clone();
    let number_width = props.number_width();
    let number_color = if selected {
        palette.secondary
    } else {
        palette.muted
    };
    div()
        .id(("review-diff-line", position))
        .relative()
        .h(px(ROW_HEIGHT))
        .w_full()
        .min_w(px(props.content_width))
        .flex()
        .items_center()
        .font_family(props.font.clone())
        .text_size(px(CODE_SIZE))
        .whitespace_nowrap()
        .when_some(look.line, |row, wash| row.bg(wash))
        .when(selected, |row| row.bg(palette.selection))
        .cursor_pointer()
        .hover(move |row| {
            row.bg(if selected {
                palette.selection
            } else {
                palette.hover
            })
        })
        .on_click(move |event: &ClickEvent, window, cx| {
            (select.select_row)(row, event.modifiers().shift, window, cx);
            cx.stop_propagation();
        })
        .child(
            div()
                .flex_none()
                .h_full()
                .flex()
                .when(!selected, |gutter| {
                    gutter.when_some(look.gutter, |gutter, wash| gutter.bg(wash))
                })
                .text_size(px(CODE_SIZE - 1.0))
                .child(line_number(line.old_line, number_width, number_color))
                .child(line_number(line.new_line, number_width, number_color)),
        )
        .child(
            div()
                .flex_none()
                .w(px(SIGN_WIDTH))
                .flex()
                .justify_center()
                .text_color(look.sign_color)
                .child(look.sign),
        )
        .child(
            div()
                .flex_none()
                .pr(px(16.0))
                .text_color(palette.text)
                .child(code(&line.text, props.snapshot.words_for(row), look.word)),
        )
        .when(selected, |row| row.child(selection_edge(&palette)))
        .into_any_element()
}

fn render_side(
    props: &DiffViewProps,
    position: usize,
    side: &'static str,
    row: Option<usize>,
    old_side: bool,
) -> AnyElement {
    let palette = props.palette;
    let Some(row) = row else {
        return div()
            .id((SharedString::from(format!("review-diff-{side}")), position))
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(palette.empty_side)
            .into_any_element();
    };
    let line = &props.snapshot.rows[row];
    let look = line_look(line.kind, &palette);
    let selected = props
        .selection
        .as_ref()
        .is_some_and(|range| range.contains(&row));
    let number = if old_side {
        line.old_line
    } else {
        line.new_line
    };
    let number_color = if selected {
        palette.secondary
    } else {
        palette.muted
    };
    let select = props.handlers.clone();
    div()
        .id((SharedString::from(format!("review-diff-{side}")), position))
        .relative()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .overflow_hidden()
        .when_some(look.line, |side, wash| side.bg(wash))
        .when(selected, |side| side.bg(palette.selection))
        .cursor_pointer()
        .hover(move |side| {
            side.bg(if selected {
                palette.selection
            } else {
                palette.hover
            })
        })
        .on_click(move |event: &ClickEvent, window, cx| {
            (select.select_row)(row, event.modifiers().shift, window, cx);
            cx.stop_propagation();
        })
        .child(
            div()
                .flex_none()
                .h_full()
                .when(!selected, |gutter| {
                    gutter.when_some(look.gutter, |gutter, wash| gutter.bg(wash))
                })
                .text_size(px(CODE_SIZE - 1.0))
                .child(line_number(number, props.number_width(), number_color)),
        )
        .child(
            div()
                .flex_none()
                .w(px(SIGN_WIDTH))
                .flex()
                .justify_center()
                .text_color(look.sign_color)
                .child(look.sign),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_color(palette.text)
                .child(code(&line.text, props.snapshot.words_for(row), look.word)),
        )
        .when(selected, |side| side.child(selection_edge(&palette)))
        .into_any_element()
}

fn render_pair(
    props: &DiffViewProps,
    position: usize,
    left: Option<usize>,
    right: Option<usize>,
) -> AnyElement {
    div()
        .id(("review-diff-pair", position))
        .h(px(ROW_HEIGHT))
        .w_full()
        .flex()
        .font_family(props.font.clone())
        .text_size(px(CODE_SIZE))
        .whitespace_nowrap()
        .child(render_side(props, position, "old", left, true))
        .child(div().flex_none().w(px(1.0)).h_full().bg(props.palette.rule))
        .child(render_side(props, position, "new", right, false))
        .into_any_element()
}

fn render_meta(props: &DiffViewProps, position: usize, row: usize) -> AnyElement {
    let palette = props.palette;
    let notice = props.snapshot.omitted_untracked_notice_row() == Some(row);
    let toggle = props.handlers.clone();
    let indent = match props.layout {
        DiffLayout::Inline => props.gutter_width() + SIGN_WIDTH,
        DiffLayout::Split => props.number_width() + SIGN_WIDTH,
    };
    div()
        .id(("review-diff-meta", position))
        .h(px(ROW_HEIGHT))
        .w_full()
        .min_w(px(row_min_width(props)))
        .pl(px(indent))
        .flex()
        .items_center()
        .gap(px(6.0))
        .whitespace_nowrap()
        .text_size(px(10.5))
        .text_color(palette.muted)
        .when(notice, |line| {
            line.debug_selector(|| "INSPECTOR_OMITTED_UNTRACKED_NOTICE".to_owned())
                .cursor_pointer()
                .hover(move |line| line.bg(palette.hover))
                .on_click(move |_, window, cx| {
                    (toggle.toggle_omitted)(window, cx);
                    cx.stop_propagation();
                })
                .child(sf_symbol(
                    if props.omitted_open {
                        "chevron.down"
                    } else {
                        "chevron.right"
                    },
                    8.5,
                    palette.muted,
                ))
        })
        .child(props.snapshot.rows[row].text.clone())
        .into_any_element()
}

/// One file name from the expanded omitted-untracked notice. It carries no
/// diff body: clicking opens the file, and the Working lane can stage it.
fn render_omitted_path(props: &DiffViewProps, position: usize, ordinal: usize) -> AnyElement {
    let palette = props.palette;
    let path = props.snapshot.omitted_untracked_paths[ordinal]
        .to_string_lossy()
        .into_owned();
    let indent = match props.layout {
        DiffLayout::Inline => props.gutter_width() + SIGN_WIDTH,
        DiffLayout::Split => props.number_width() + SIGN_WIDTH,
    } + OMITTED_PATH_INDENT;
    let open = props.handlers.clone();
    let stageable = props.mutable && props.snapshot.layer == DiffLayer::Working;
    let mut actions = action_cluster(false);
    if stageable {
        let stage = props.handlers.clone();
        actions = actions.child(action_button(
            ("stage-omitted-path", position),
            crate::i18n::t("git.diff.stage"),
            palette.secondary,
            palette.hover,
            move |_, window, cx| (stage.omitted)(ordinal, OmittedAction::Stage, window, cx),
        ));
    }
    div()
        .id(("review-diff-omitted", position))
        .debug_selector(move || format!("INSPECTOR_OMITTED_UNTRACKED_PATH_{ordinal}"))
        .group(ROW_GROUP)
        .h(px(ROW_HEIGHT))
        .w_full()
        .min_w(px(row_min_width(props)))
        .pl(px(indent))
        .flex()
        .items_center()
        .whitespace_nowrap()
        .font_family(props.font.clone())
        .text_size(px(CODE_SIZE))
        .text_color(palette.secondary)
        .cursor_pointer()
        .hover(move |line| line.bg(palette.hover))
        .on_click(move |_, window, cx| {
            (open.omitted)(ordinal, OmittedAction::Open, window, cx);
            cx.stop_propagation();
        })
        .child(path)
        .child(actions)
        .into_any_element()
}

/// A segmented control in the review's toolbar style: a quiet track with the
/// selected segment lifted. `segments` are `(key, label, debug selector)`.
pub(crate) fn segmented<K: Copy + PartialEq + 'static>(
    id: &'static str,
    segments: &[(K, &'static str, &'static str)],
    selected: K,
    palette: &DiffPalette,
    on_select: impl Fn(K, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let on_select = Rc::new(on_select);
    let palette = *palette;
    div()
        .id(id)
        .flex_none()
        .h(px(24.0))
        .p(px(2.0))
        .flex()
        .items_center()
        .gap(px(1.0))
        .rounded(px(Radius::BADGE))
        .bg(palette.control)
        .children(segments.iter().map(|&(key, label, selector)| {
            let on = key == selected;
            let on_select = on_select.clone();
            div()
                .id(SharedString::from(format!("{id}-{label}")))
                .debug_selector(move || selector.to_owned())
                .h_full()
                .px(px(8.0))
                .flex()
                .items_center()
                .rounded(px(Radius::inner(Radius::BADGE, 2.0)))
                .when(on, |segment| segment.bg(palette.surface_lift).shadow_xs())
                .cursor_pointer()
                .when(!on, |segment| {
                    segment.hover(move |segment| segment.bg(palette.hover))
                })
                .text_size(px(10.5))
                .font_weight(if on {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(if on { palette.text } else { palette.muted })
                .child(label)
                .on_click(move |_, window, cx| {
                    on_select(key, window, cx);
                    cx.stop_propagation();
                })
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_squares_split_by_share() {
        assert_eq!(stat_split(0, 0), None);
        assert_eq!(stat_split(10, 0), Some(5));
        assert_eq!(stat_split(120, 45), Some(4));
        assert_eq!(stat_split(1, 9), Some(1), "a little still shows");
    }

    #[test]
    fn hunk_headers_split_into_range_and_context() {
        assert_eq!(
            hunk_parts("@@ -10,3 +10,4 @@ fn main() {"),
            ("@@ -10,3 +10,4 @@", "fn main() {")
        );
        assert_eq!(hunk_parts("@@ -1 +1 @@"), ("@@ -1 +1 @@", ""));
        assert_eq!(hunk_parts("not a hunk"), ("not a hunk", ""));
    }

    #[test]
    fn tabs_expand_and_carry_word_ranges_with_them() {
        let one = |range: Range<usize>| vec![range];
        let (text, ranges) = expand_tabs("\tx = 1;", &one(5..6));
        assert_eq!(text, "    x = 1;");
        assert_eq!(&text[ranges[0].clone()], "1");
        let (text, ranges) = expand_tabs("ab\tc", &one(3..4));
        assert_eq!(text, "ab  c");
        assert_eq!(&text[ranges[0].clone()], "c");
        let (text, ranges) = expand_tabs("plain", &one(0..5));
        assert_eq!(text, "plain");
        assert_eq!(ranges, one(0..5));
    }

    #[test]
    fn header_rows_locate_their_file_and_hunk() {
        let snapshot = crate::diff::parse_unified_diff(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n@@ -9 +9 @@\n-p\n+q\ndiff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-m\n+n\n",
        );
        assert_eq!(locate(&snapshot, 0), (Some(0), None));
        assert_eq!(locate(&snapshot, 1), (Some(0), Some(0)));
        assert_eq!(locate(&snapshot, 4), (Some(0), Some(1)));
        assert_eq!(locate(&snapshot, 7), (Some(1), None));
        assert_eq!(locate(&snapshot, 8), (Some(1), Some(0)));
        assert_eq!(locate(&snapshot, 2), (Some(0), None));
    }
}
