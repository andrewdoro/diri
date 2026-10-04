// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//
//! The review's commit history: Ely's `CommitList` with its graph, and a
//! `CommitItem` per row.
//!
//! A row is two lines tall so it fits a narrow panel: the refs and subject on
//! top; the short id, author, age, and push state underneath. The graph's
//! lanes are painted beside it from rows computed once per history load.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Bounds, FontWeight, PathBuilder, Pixels, Rgba, SharedString, Window, canvas,
    div, point, prelude::*, px, size,
};

use super::graph::{GraphRow, Half, lanes};
use super::palette::DiffPalette;
use crate::git_review::{CommitHistory, CommitRefKind, CommitSummary};
use crate::icons::sf_symbol;

pub(crate) const COMMIT_ROW_HEIGHT: f32 = 40.0;
const LANE_WIDTH: f32 = 12.0;
/// Graphs wider than this many lanes are clipped at the right edge.
const MAX_LANES: usize = 8;

/// A loaded history and its graph, built off the main thread.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LoadedHistory {
    pub history: CommitHistory,
    pub graph: Vec<GraphRow>,
    pub span: usize,
}

impl LoadedHistory {
    #[must_use]
    pub(crate) fn new(history: CommitHistory) -> Self {
        let graph = lanes(history.commits.iter().map(|commit| {
            (
                commit.oid.as_str(),
                commit.parents.iter().map(String::as_str),
            )
        }));
        let span = graph
            .iter()
            .map(|row| row.width)
            .max()
            .unwrap_or(1)
            .min(MAX_LANES);
        Self {
            history,
            graph,
            span,
        }
    }

    /// Commits on the branch that no remote has.
    #[must_use]
    pub(crate) fn unpushed(&self) -> usize {
        if !self.history.has_remote {
            return 0;
        }
        self.history
            .commits
            .iter()
            .filter(|commit| !commit.pushed)
            .count()
    }
}

type OnPick = Rc<dyn Fn(usize, &mut Window, &mut App)>;

#[derive(Clone)]
pub(crate) struct CommitListProps {
    pub loaded: Arc<LoadedHistory>,
    pub selected: Option<String>,
    pub palette: DiffPalette,
    pub mono: SharedString,
    /// Seconds since the Unix epoch, read once per frame.
    pub now: i64,
    pub on_pick: OnPick,
}

/// "now", "5m", "3h", "2d", "3w", "4mo", "2y".
#[must_use]
pub(crate) fn relative_age(now: i64, then: i64) -> String {
    let seconds = (now - then).max(0);
    match seconds {
        0..=59 => "now".to_owned(),
        60..=3_599 => format!("{}m", seconds / 60),
        3_600..=86_399 => format!("{}h", seconds / 3_600),
        86_400..=1_209_599 => format!("{}d", seconds / 86_400),
        1_209_600..=5_183_999 => format!("{}w", seconds / 604_800),
        5_184_000..=31_535_999 => format!("{}mo", seconds / 2_592_000),
        _ => format!("{}y", seconds / 31_536_000),
    }
}

/// Builds the elements for `range` of the history's commits.
pub(crate) fn render_commit_rows(
    props: &CommitListProps,
    range: std::ops::Range<usize>,
) -> Vec<AnyElement> {
    range.map(|index| render_commit(props, index)).collect()
}

fn lane_color(palette: &DiffPalette, lane: usize) -> Rgba {
    palette.lanes[lane % palette.lanes.len()]
}

/// Paints one row's lanes, `span` lanes wide so every row lines up: strokes
/// into and out of its commit, lanes passing it, and the commit's dot.
fn lanes_cell(
    row: GraphRow,
    span: usize,
    palette: DiffPalette,
    on_branch: bool,
    head: bool,
) -> AnyElement {
    let lane = px(LANE_WIDTH);
    let height = px(COMMIT_ROW_HEIGHT);
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window, _| {
            let x = |at: usize| bounds.origin.x + lane * (at as f32 + 0.5);
            let (top, middle, bottom) = (
                bounds.origin.y,
                bounds.origin.y + height / 2.0,
                bounds.origin.y + height,
            );
            for piece in &row.strokes {
                if piece.from >= span || piece.to >= span {
                    continue;
                }
                let (from, to) = match piece.half {
                    Half::Top => (point(x(piece.from), top), point(x(piece.to), middle)),
                    Half::Bottom => (point(x(piece.from), middle), point(x(piece.to), bottom)),
                    Half::Through => (point(x(piece.from), top), point(x(piece.to), bottom)),
                };
                let color = lane_color(&palette, piece.from.max(piece.to));
                let mut path = PathBuilder::stroke(px(1.5));
                path.move_to(from);
                if from.x == to.x {
                    path.line_to(to);
                } else {
                    // Lanes that change column bend instead of cutting across.
                    let mid_y = (from.y + to.y) / 2.0;
                    path.cubic_bezier_to(to, point(from.x, mid_y), point(to.x, mid_y));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            }
            if row.lane >= span {
                return;
            }
            let center = point(x(row.lane), middle);
            let color = lane_color(&palette, row.lane);
            let dot = px(if head { 9.0 } else { 7.0 });
            let corner = dot / 2.0;
            let origin = center - point(corner, corner);
            if on_branch {
                window.paint_quad(
                    gpui::fill(Bounds::new(origin, size(dot, dot)), color).corner_radii(corner),
                );
            } else {
                // Base commits are hollow: history the branch builds on.
                window.paint_quad(
                    gpui::fill(Bounds::new(origin, size(dot, dot)), palette.surface)
                        .corner_radii(corner),
                );
                window.paint_quad(
                    gpui::outline(
                        Bounds::new(origin, size(dot, dot)),
                        color,
                        gpui::BorderStyle::Solid,
                    )
                    .corner_radii(corner),
                );
            }
        },
    )
    .flex_none()
    .w(lane * span as f32)
    .h(height)
    .into_any_element()
}

fn ref_pill(name: &str, kind: CommitRefKind, palette: &DiffPalette) -> AnyElement {
    let hue = match kind {
        CommitRefKind::Head => palette.accent,
        CommitRefKind::Branch => palette.added,
        CommitRefKind::Remote => palette.muted,
        CommitRefKind::Tag => palette.modified,
    };
    div()
        .flex_none()
        .max_w(px(140.0))
        .h(px(15.0))
        .px(px(5.0))
        .flex()
        .items_center()
        .gap(px(3.0))
        .rounded(px(4.0))
        .bg(hue.alpha(0.13))
        .text_size(px(9.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(hue)
        .when(kind == CommitRefKind::Head, |pill| {
            pill.child(sf_symbol("arrow.branch", 8.0, hue))
        })
        .child(div().truncate().child(name.to_owned()))
        .into_any_element()
}

fn render_commit(props: &CommitListProps, index: usize) -> AnyElement {
    let palette = props.palette;
    let loaded = &props.loaded;
    let commit: &CommitSummary = &loaded.history.commits[index];
    let selected = props.selected.as_deref() == Some(commit.oid.as_str());
    let head = loaded.history.head.as_deref() == Some(commit.oid.as_str());
    let short: String = commit.oid.chars().take(7).collect();
    let unpushed = loaded.history.has_remote && !commit.pushed;
    let pick = props.on_pick.clone();
    let graph = loaded.graph.get(index).cloned();
    div()
        .id(("review-commit", index))
        .debug_selector(move || format!("INSPECTOR_COMMIT_{index}"))
        .h(px(COMMIT_ROW_HEIGHT))
        .w_full()
        .pl(px(6.0))
        .pr(px(10.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .bg(if selected {
            palette.selection
        } else {
            palette.hover.alpha(0.0)
        })
        .cursor_pointer()
        .when(!selected, |row| row.hover(move |row| row.bg(palette.hover)))
        .on_click(move |_, window, cx| {
            pick(index, window, cx);
            cx.stop_propagation();
        })
        .children(graph.map(|row| lanes_cell(row, loaded.span, palette, commit.on_branch, head)))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(5.0))
                        .children(
                            commit.refs.iter().take(3).map(|reference| {
                                ref_pill(&reference.name, reference.kind, &palette)
                            }),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_size(px(12.0))
                                .font_weight(if selected {
                                    FontWeight::SEMIBOLD
                                } else {
                                    FontWeight::MEDIUM
                                })
                                .text_color(if commit.on_branch || selected {
                                    palette.text
                                } else {
                                    palette.secondary
                                })
                                .child(commit.subject.clone()),
                        ),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(px(10.5))
                        .text_color(palette.muted)
                        .child(
                            div()
                                .flex_none()
                                .font_family(props.mono.clone())
                                .text_color(palette.secondary)
                                .child(short),
                        )
                        .child(div().min_w_0().truncate().child(commit.author.clone()))
                        .child(
                            div()
                                .flex_none()
                                .child(relative_age(props.now, commit.timestamp)),
                        )
                        .when(unpushed, |meta| {
                            meta.child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(2.0))
                                    .child(sf_symbol("arrow.up", 8.5, palette.modified))
                                    .child(crate::i18n::t("git.commit.not_pushed")),
                            )
                        }),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_review::CommitSummary;

    fn commit(oid: &str, parents: &[&str], pushed: bool) -> CommitSummary {
        CommitSummary {
            oid: oid.to_owned(),
            parents: parents.iter().map(|parent| (*parent).to_owned()).collect(),
            author: "Ada".to_owned(),
            timestamp: 0,
            subject: format!("commit {oid}"),
            refs: Vec::new(),
            on_branch: true,
            pushed,
        }
    }

    #[test]
    fn ages_read_compactly() {
        let now = 10_000_000;
        assert_eq!(relative_age(now, now), "now");
        assert_eq!(
            relative_age(now, now + 50),
            "now",
            "clock skew reads as now"
        );
        assert_eq!(relative_age(now, now - 300), "5m");
        assert_eq!(relative_age(now, now - 3 * 3_600), "3h");
        assert_eq!(relative_age(now, now - 2 * 86_400), "2d");
        assert_eq!(relative_age(now, now - 21 * 86_400), "3w");
        assert_eq!(relative_age(now, now - 120 * 86_400), "4mo");
        assert_eq!(relative_age(now, now - 800 * 86_400), "2y");
    }

    #[test]
    fn loaded_history_carries_its_graph_and_push_state() {
        let history = CommitHistory {
            head: Some("c".to_owned()),
            base: Some("main".to_owned()),
            commits: vec![
                commit("c", &["b"], false),
                commit("b", &["a", "x"], true),
                commit("a", &[], true),
            ],
            ahead: 3,
            truncated: false,
            has_remote: true,
        };
        let loaded = LoadedHistory::new(history.clone());
        assert_eq!(loaded.graph.len(), 3);
        assert_eq!(loaded.span, 2, "the merge's second parent opens a lane");
        assert_eq!(loaded.unpushed(), 1);

        let local = LoadedHistory::new(CommitHistory {
            has_remote: false,
            ..history
        });
        assert_eq!(local.unpushed(), 0, "nothing is unpushed without a remote");
    }
}
