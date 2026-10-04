// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//! Backlinks under a note: the other notes that link to it, each with the
//! words around its link (the link lit), and the notes that write its title
//! without linking it, each with a Link button. A press opens the other note
//! at that line. Ely's `Backlinks`, laid out the way Obsidian ends a note.
//!
//! Everything shown is read from the shared [`TodosModel`]'s link index and
//! recomputed only when that index changes (a note was saved), never per
//! frame.

use std::sync::Arc;

use diri_notes::backlinks::{LinkIndex, Reference};
use diri_ui::SemanticColors;
use gpui::{
    Context, Entity, EventEmitter, FontWeight, HighlightStyle, MouseButton, MouseDownEvent, Render,
    SharedString, StyledText, Window, div, prelude::*, px,
};

use super::todos::TodosModel;
use crate::icons::sf_symbol;

/// Unlinked mentions listed at most.
const UNLINKED_LIMIT: usize = 50;

pub(crate) enum BacklinksEvent {
    /// Open note `note_id` with the caret on `block`.
    Open { note_id: String, block: usize },
    /// Turn an unlinked mention into a link to the open note.
    Link(Reference),
    /// Show the graph around the open note.
    ShowGraph,
}

pub(crate) struct BacklinksView {
    model: Entity<TodosModel>,
    note_id: String,
    built_from: Option<Arc<LinkIndex>>,
    pub(crate) linked: Vec<Reference>,
    pub(crate) unlinked: Vec<Reference>,
    /// Notes this one links to that the index knows, for the graph button.
    outgoing: usize,
    show_unlinked: bool,
    colors: SemanticColors,
    _observe: gpui::Subscription,
}

impl EventEmitter<BacklinksEvent> for BacklinksView {}

fn fade(color: gpui::Rgba, alpha: f32) -> gpui::Rgba {
    gpui::Rgba {
        a: color.a * alpha,
        ..color
    }
}

impl BacklinksView {
    pub(crate) fn new(
        model: Entity<TodosModel>,
        note_id: String,
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe(&model, |this, _, cx| {
            if this.refresh(cx) {
                cx.notify();
            }
        });
        let mut view = Self {
            model,
            note_id,
            built_from: None,
            linked: Vec::new(),
            unlinked: Vec::new(),
            outgoing: 0,
            show_unlinked: false,
            colors,
            _observe: observe,
        };
        view.refresh(cx);
        view
    }

    pub(crate) fn set_colors(&mut self, colors: SemanticColors) {
        self.colors = colors;
    }

    /// Re-reads the open note's references when the index changed.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) -> bool {
        let links = self.model.read(cx).links();
        if self
            .built_from
            .as_ref()
            .is_some_and(|built| Arc::ptr_eq(built, &links))
        {
            return false;
        }
        let linked = links.backlinks(&self.note_id);
        let unlinked = links.unlinked_mentions(&self.note_id, UNLINKED_LIMIT);
        let outgoing = links.get(&self.note_id).map_or(0, |note| {
            note.targets().iter().filter(|t| links.contains(t)).count()
        });
        self.built_from = Some(links);
        let changed =
            linked != self.linked || unlinked != self.unlinked || outgoing != self.outgoing;
        self.linked = linked;
        self.unlinked = unlinked;
        self.outgoing = outgoing;
        changed
    }

    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn expand_unlinked_for_test(&mut self, cx: &mut Context<Self>) {
        self.show_unlinked = true;
        cx.notify();
    }

    /// The unlinked mention just linked leaves the list at once; the next
    /// index read confirms it.
    pub(crate) fn forget_unlinked(&mut self, reference: &Reference, cx: &mut Context<Self>) {
        let before = self.unlinked.len();
        self.unlinked.retain(|r| r != reference);
        if self.unlinked.len() != before {
            cx.notify();
        }
    }

    fn section_header(
        &self,
        id: &'static str,
        label: &'static str,
        count: usize,
        disclosure: Option<bool>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = self.colors;
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(6.0))
            .h(px(24.0))
            .text_size(px(diri_ui::Typo::SECTION_HEADER.size))
            .font_weight(diri_ui::Typo::SECTION_HEADER.weight)
            .text_color(colors.secondary)
            .when_some(disclosure, |row, open| {
                row.cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.show_unlinked = !this.show_unlinked;
                            cx.notify();
                        }),
                    )
                    .child(sf_symbol(
                        if open {
                            "chevron.down"
                        } else {
                            "chevron.right"
                        },
                        10.0,
                        colors.tertiary,
                    ))
            })
            .child(label)
            .child(
                div()
                    .text_color(colors.tertiary)
                    .font_weight(FontWeight::NORMAL)
                    .child(count.to_string()),
            )
    }

    fn reference_rows(
        &self,
        group: &'static str,
        references: &[Reference],
        linkable: bool,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let colors = self.colors;
        let accent = super::editor_view::accent();
        let mut rows = Vec::new();
        let mut last_source: Option<&str> = None;
        for (i, reference) in references.iter().enumerate() {
            // One title per referring note, its lines below it.
            if last_source != Some(reference.source.as_str()) {
                last_source = Some(reference.source.as_str());
                let note_id = reference.source.clone();
                let block = reference.block;
                rows.push(
                    div()
                        .id((group, i * 2))
                        .mt(px(6.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .px(px(6.0))
                        .h(px(24.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .hover(|row| row.bg(fade(colors.primary, 0.05)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_, _: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                cx.emit(BacklinksEvent::Open {
                                    note_id: note_id.clone(),
                                    block,
                                });
                            }),
                        )
                        .child(sf_symbol("doc.text", 12.0, colors.secondary))
                        .child(
                            div()
                                .text_size(px(diri_ui::Typo::ROW_EMPHASIZED.size))
                                .font_weight(diri_ui::Typo::ROW_EMPHASIZED.weight)
                                .text_color(colors.primary)
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(SharedString::from(reference.source_title.clone())),
                        )
                        .into_any_element(),
                );
            }
            let lit = HighlightStyle {
                color: Some(if linkable {
                    colors.primary.into()
                } else {
                    accent.into()
                }),
                font_weight: Some(FontWeight::MEDIUM),
                background_color: linkable.then(|| fade(accent, 0.16).into()),
                ..HighlightStyle::default()
            };
            let note_id = reference.source.clone();
            let block = reference.block;
            let group_name: SharedString = format!("{group}-line-{i}").into();
            let link = linkable.then(|| {
                let reference = reference.clone();
                div()
                    .id((group, i * 2 + 1))
                    .flex_none()
                    .px(px(8.0))
                    .h(px(20.0))
                    .flex()
                    .items_center()
                    .rounded(px(5.0))
                    .border_1()
                    .border_color(fade(colors.primary, 0.12))
                    .text_size(px(diri_ui::Typo::META.size))
                    .font_weight(diri_ui::Typo::META.weight)
                    .text_color(colors.secondary)
                    .cursor_pointer()
                    .opacity(0.0)
                    .group_hover(group_name.clone(), |b| b.opacity(1.0))
                    .hover(|b| b.text_color(colors.primary).bg(fade(colors.primary, 0.05)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            cx.emit(BacklinksEvent::Link(reference.clone()));
                        }),
                    )
                    .child("Link")
            });
            rows.push(
                div()
                    .id(SharedString::from(format!("{group}-ctx-{i}")))
                    .group(group_name)
                    .ml(px(24.0))
                    .px(px(6.0))
                    .py(px(3.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .hover(|row| row.bg(fade(colors.primary, 0.04)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            cx.emit(BacklinksEvent::Open {
                                note_id: note_id.clone(),
                                block,
                            });
                        }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(diri_ui::Typo::ROW.size))
                            .line_height(px(19.0))
                            .text_color(colors.secondary)
                            .child(
                                StyledText::new(SharedString::from(reference.context.clone()))
                                    .with_highlights([(reference.mention.clone(), lit)]),
                            ),
                    )
                    .children(link)
                    .into_any_element(),
            );
        }
        rows
    }
}

impl Render for BacklinksView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.colors;
        let linked = self.linked.clone();
        let unlinked = self.unlinked.clone();
        let graph_button = div()
            .id("note-backlinks-graph")
            .flex()
            .items_center()
            .gap(px(5.0))
            .px(px(8.0))
            .h(px(24.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .text_size(px(diri_ui::Typo::META.size))
            .font_weight(diri_ui::Typo::META.weight)
            .text_color(colors.secondary)
            .hover(|b| b.bg(fade(colors.primary, 0.06)).text_color(colors.primary))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    cx.emit(BacklinksEvent::ShowGraph);
                }),
            )
            .child(sf_symbol(
                "point.3.filled.connected.trianglepath.dotted",
                12.0,
                colors.secondary,
            ))
            .child("Graph");
        let mut root = div()
            .id("note-backlinks")
            .w_full()
            .pt(px(18.0))
            .border_t_1()
            .border_color(fade(colors.primary, 0.08))
            .cursor_default()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(self.section_header(
                        "note-backlinks-linked",
                        "Linked mentions",
                        linked.len(),
                        None,
                        cx,
                    ))
                    .child(graph_button),
            );
        if linked.is_empty() {
            root = root.child(
                div()
                    .py(px(6.0))
                    .text_size(px(diri_ui::Typo::ROW.size))
                    .text_color(colors.tertiary)
                    .child(if self.outgoing > 0 {
                        "No notes link here yet."
                    } else {
                        "No notes link here yet. Type [[ to link another note."
                    }),
            );
        } else {
            root = root.children(self.reference_rows("note-backlink", &linked, false, cx));
        }
        if !unlinked.is_empty() {
            let open = self.show_unlinked;
            root = root.child(div().mt(px(14.0)).child(self.section_header(
                "note-backlinks-unlinked",
                "Unlinked mentions",
                unlinked.len(),
                Some(open),
                cx,
            )));
            if open {
                root = root.children(self.reference_rows("note-unlinked", &unlinked, true, cx));
            }
        }
        root
    }
}
