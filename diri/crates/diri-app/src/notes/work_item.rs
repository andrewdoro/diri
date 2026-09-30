//! To-dos as tracked agent work, in the editor.
//!
//! The rules live in `diri_notes::work` (what counts as context, how state is
//! derived, what the agent receives); this module is the pixels and the
//! little view state they need: which to-dos are folded, which starts are in
//! flight, and the two panels (Start, and "still working" on tick). The pane
//! feeds it live session facts every frame and turns its requests into
//! spawns, selections and archives.

use std::collections::HashMap;

use diri_notes::doc::{Block, BlockId, BlockKind};
use diri_notes::edit::Pos;
use diri_notes::work::{self, Brief, SessionFacts, WorkState};
use diri_proto::{AgentKind, SessionId};
use diri_ui::{AgentLogo, GlassMenuRow as _, Ink, SemanticColors, Typo};
use gpui::{
    AnyElement, Context, FontWeight, MouseButton, MouseDownEvent, ScrollHandle, SharedString,
    anchored, deferred, div, point, prelude::*, px,
};

use super::editor_view::{EditorEvent, NoteEditorView, accent};
use crate::icons::sf_symbol;

/// The keyboard shortcut that starts work from the to-do at the caret.
pub(crate) const START_SHORTCUT: &str = "⌥⌘↩";
const PANEL_WIDTH: f32 = 440.0;
const PREVIEW_HEIGHT: f32 = 200.0;
const ROW_HEIGHT: f32 = 32.0;
const ROW_ICON_SLOT: f32 = 22.0;
const ROW_RADIUS: f32 = 12.0;
/// The Start affordance sits in the margin right of the text column.
const ACCESSORY_GAP: f32 = 12.0;

/// What the editor asks its host to do for a work item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkRequest {
    /// Open the Start panel for this to-do: the host assembles the brief and
    /// the installed agents and calls [`NoteEditorView::open_start_panel`].
    Prepare { block: BlockId },
    /// Start `kind` on this to-do with the brief the panel showed.
    Start {
        block: BlockId,
        kind: AgentKind,
        agent_name: String,
    },
    /// Show this session (the sidebar selects it).
    Open { session: String },
    /// Stop this session's agent (the to-do was ticked while it ran).
    Stop { session: String },
}

/// One installed agent the Start panel offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentChoice {
    pub kind: AgentKind,
    pub name: String,
    pub is_default: bool,
}

pub(crate) struct StartPanel {
    block: BlockId,
    agents: Vec<AgentChoice>,
    selected: usize,
    brief: Brief,
    scroll: ScrollHandle,
}

struct TickPanel {
    block: BlockId,
    session: String,
    selected: usize,
}

const TICK_CHOICES: [&str; 3] = ["Tick and stop the agent", "Tick, keep it running", "Cancel"];

enum Pending {
    Starting { ticket: u64 },
    Failed(String),
}

/// Work-item view state for one open note. Nothing here is saved: the file
/// holds the facts, the Engine holds the live state.
#[derive(Default)]
pub(crate) struct WorkView {
    facts: HashMap<String, SessionFacts>,
    /// Folds the person chose; to-dos without an entry use the default
    /// (folded once work started).
    folds: HashMap<BlockId, bool>,
    pending: HashMap<BlockId, Pending>,
    start: Option<StartPanel>,
    tick: Option<TickPanel>,
    /// Per editor block, from the last frame: hidden inside a folded to-do.
    hidden: Vec<bool>,
}

impl WorkView {
    /// Live facts for the sessions the note links, keyed by session id. A
    /// linked session missing here is unknown to the Engine.
    pub(crate) fn set_facts(&mut self, facts: HashMap<String, SessionFacts>) {
        self.facts = facts;
    }

    /// Tickets of starts still waiting on the Engine.
    pub(crate) fn pending_tickets(&self) -> Vec<(BlockId, u64)> {
        self.pending
            .iter()
            .filter_map(|(block, pending)| match pending {
                Pending::Starting { ticket } => Some((*block, *ticket)),
                Pending::Failed(_) => None,
            })
            .collect()
    }

    pub(crate) fn state(&self, block: &Block) -> WorkState {
        if matches!(self.pending.get(&block.id), Some(Pending::Starting { .. })) {
            return WorkState::Starting;
        }
        let checked = block.kind == BlockKind::Todo { checked: true };
        let session = work::current_session(block);
        work::state(
            checked,
            session.as_ref().map(|id| self.facts.get(id.as_str())),
        )
    }

    fn failure(&self, block: BlockId) -> Option<&str> {
        match self.pending.get(&block) {
            Some(Pending::Failed(reason)) => Some(reason),
            _ => None,
        }
    }

    fn folded(&self, block: &Block) -> bool {
        self.folds.get(&block.id).copied().unwrap_or_else(|| {
            !work::sessions(block).is_empty() || self.pending.contains_key(&block.id)
        })
    }

    /// Recomputes which blocks sit inside a folded to-do. A folded to-do
    /// whose children hold the caret opens, so the caret is never hidden.
    pub(crate) fn layout_folds(&mut self, blocks: &[Block], caret: Pos) {
        let mut hidden = vec![false; blocks.len()];
        let mut index = 0;
        while index < blocks.len() {
            let block = &blocks[index];
            let children = work::children(blocks, index);
            if matches!(block.kind, BlockKind::Todo { .. })
                && !children.is_empty()
                && self.folded(block)
            {
                if children.contains(&caret.block) {
                    self.folds.insert(block.id, false);
                } else {
                    for flag in &mut hidden[children.clone()] {
                        *flag = true;
                    }
                    index = children.end;
                    continue;
                }
            }
            index += 1;
        }
        self.hidden = hidden;
    }

    pub(crate) fn is_hidden(&self, index: usize) -> bool {
        self.hidden.get(index).copied().unwrap_or(false)
    }

    pub(crate) fn panel_open(&self) -> bool {
        self.start.is_some() || self.tick.is_some()
    }

    #[cfg(test)]
    pub(crate) fn start_panel_brief(&self) -> Option<&Brief> {
        self.start.as_ref().map(|panel| &panel.brief)
    }

    #[cfg(test)]
    pub(crate) fn tick_panel_open(&self) -> bool {
        self.tick.is_some()
    }
}

/// Keys the open panel consumes before the editor sees them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanelKey {
    Up,
    Down,
    Enter,
    Escape,
}

impl NoteEditorView {
    fn block_index(&self, id: BlockId) -> Option<usize> {
        self.editor.blocks().iter().position(|b| b.id == id)
    }

    /// ⌥⌘↩: start work from the to-do at the caret.
    pub(super) fn start_work_at_caret(&mut self, cx: &mut Context<Self>) {
        let head = self.editor.selection.head.block;
        let block = self.editor.block(head);
        if matches!(block.kind, BlockKind::Todo { checked: false })
            && self.work.state(block).can_restart()
        {
            cx.emit(EditorEvent::Work(WorkRequest::Prepare { block: block.id }));
        }
    }

    /// Shows the Start panel under a to-do with what the agent will get.
    pub(crate) fn open_start_panel(
        &mut self,
        block: BlockId,
        agents: Vec<AgentChoice>,
        brief: Brief,
        cx: &mut Context<Self>,
    ) {
        let selected = agents.iter().position(|a| a.is_default).unwrap_or(0);
        self.work.tick = None;
        self.work.start = Some(StartPanel {
            block,
            agents,
            selected,
            brief,
            scroll: ScrollHandle::new(),
        });
        cx.notify();
    }

    fn confirm_start(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(panel) = self.work.start.take() else {
            return;
        };
        let Some(agent) = panel.agents.get(index) else {
            cx.notify();
            return;
        };
        self.work.pending.remove(&panel.block);
        cx.emit(EditorEvent::Work(WorkRequest::Start {
            block: panel.block,
            kind: agent.kind.clone(),
            agent_name: agent.name.clone(),
        }));
        cx.notify();
    }

    /// The host asked the Engine to spawn; the to-do shows "Starting…".
    pub(crate) fn work_started(&mut self, block: BlockId, ticket: u64, cx: &mut Context<Self>) {
        self.work.pending.insert(block, Pending::Starting { ticket });
        cx.notify();
    }

    /// The Engine answered a start: link the new session into the to-do
    /// (one undo step, saved like typing), or say why it did not start.
    pub(crate) fn work_finished(
        &mut self,
        block: BlockId,
        outcome: Result<(SessionId, String), String>,
        now_ms: u64,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Ok((session, label)) => {
                self.work.pending.remove(&block);
                if let Some(index) = self.block_index(block)
                    && self.editor.link_session(index, &label, &session.0, now_ms)
                {
                    cx.emit(EditorEvent::Changed);
                }
            }
            Err(reason) => {
                self.work.pending.insert(block, Pending::Failed(reason));
            }
        }
        cx.notify();
    }

    /// Ticking a to-do whose agent is still running asks first. Returns
    /// whether the tick was intercepted.
    pub(super) fn guard_tick(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        let block = self.editor.block(index);
        if block.kind != (BlockKind::Todo { checked: false }) || !self.work.state(block).is_running()
        {
            return false;
        }
        let Some(session) = work::current_session(block) else {
            return false;
        };
        self.work.start = None;
        self.work.tick = Some(TickPanel {
            block: block.id,
            session,
            selected: 0,
        });
        cx.notify();
        true
    }

    fn confirm_tick(&mut self, choice: usize, cx: &mut Context<Self>) {
        let Some(panel) = self.work.tick.take() else {
            return;
        };
        if choice < 2
            && let Some(index) = self.block_index(panel.block)
        {
            if choice == 0 {
                cx.emit(EditorEvent::Work(WorkRequest::Stop {
                    session: panel.session,
                }));
            }
            self.set_todo_checked(index, true, cx);
        }
        cx.notify();
    }

    /// Arrow keys, Return and Escape drive an open panel.
    pub(super) fn work_panel_key(&mut self, key: PanelKey, cx: &mut Context<Self>) -> bool {
        if let Some(panel) = &mut self.work.start {
            let count = panel.agents.len().max(1);
            match key {
                PanelKey::Up => panel.selected = (panel.selected + count - 1) % count,
                PanelKey::Down => panel.selected = (panel.selected + 1) % count,
                PanelKey::Enter => {
                    let selected = panel.selected;
                    self.confirm_start(selected, cx);
                    return true;
                }
                PanelKey::Escape => self.work.start = None,
            }
            cx.notify();
            return true;
        }
        if let Some(panel) = &mut self.work.tick {
            let count = TICK_CHOICES.len();
            match key {
                PanelKey::Up => panel.selected = (panel.selected + count - 1) % count,
                PanelKey::Down => panel.selected = (panel.selected + 1) % count,
                PanelKey::Enter => {
                    let selected = panel.selected;
                    self.confirm_tick(selected, cx);
                    return true;
                }
                PanelKey::Escape => self.work.tick = None,
            }
            cx.notify();
            return true;
        }
        false
    }

    fn toggle_fold(&mut self, block: BlockId, cx: &mut Context<Self>) {
        let Some(index) = self.block_index(block) else {
            return;
        };
        let folded = self.work.folded(self.editor.block(index));
        self.work.folds.insert(block, !folded);
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Pixels

    /// The disclosure triangle left of a to-do's checkbox, when it has
    /// children to fold.
    pub(super) fn work_disclosure(
        &self,
        index: usize,
        block: &Block,
        line: f32,
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !matches!(block.kind, BlockKind::Todo { .. })
            || work::children(self.editor.blocks(), index).is_empty()
        {
            return None;
        }
        let id = block.id;
        let folded = self.work.folded(block);
        Some(
            div()
                .id(("todo-fold", id))
                .absolute()
                .left(px(-20.0))
                .top_0()
                .h(px(line))
                .w(px(16.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .cursor_pointer()
                .hover(|el| el.bg(colors.primary.alpha(0.06)))
                .child(sf_symbol(
                    if folded { "chevron.right" } else { "chevron.down" },
                    10.0,
                    colors.tertiary,
                ))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.toggle_fold(id, cx);
                    }),
                )
                .into_any_element(),
        )
    }

    /// The quiet Start affordance in the margin right of an unstarted to-do.
    /// It shows while the row is hovered or holds the caret.
    pub(super) fn work_accessory(
        &self,
        block: &Block,
        caret_here: bool,
        group: SharedString,
        line: f32,
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if block.kind != (BlockKind::Todo { checked: false })
            || block.text.trim().is_empty()
            || self.work.state(block) != WorkState::Ready
            || self.work.failure(block.id).is_some()
        {
            return None;
        }
        let id = block.id;
        Some(
            div()
                .absolute()
                .top_0()
                .left_full()
                .h(px(line))
                .pl(px(ACCESSORY_GAP))
                .flex()
                .items_center()
                .child(
                    div()
                        .id(("todo-start", id))
                        .h(px(24.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .rounded(px(7.0))
                        .cursor_pointer()
                        .text_size(px(12.0))
                        .text_color(colors.secondary)
                        .when(!caret_here, |el| {
                            el.opacity(0.0).group_hover(group, |el| el.opacity(1.0))
                        })
                        .hover(|el| el.bg(colors.primary.alpha(0.06)).text_color(colors.primary))
                        .child("Start")
                        .child(
                            div()
                                .text_color(colors.tertiary)
                                .child(START_SHORTCUT),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_, _: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                cx.emit(EditorEvent::Work(WorkRequest::Prepare { block: id }));
                            }),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The live line under a started to-do: where the work stands, the
    /// question when the agent waits, and what can be done about it.
    pub(super) fn work_status_line(
        &self,
        block: &Block,
        indent: f32,
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !matches!(block.kind, BlockKind::Todo { .. }) {
            return None;
        }
        let id = block.id;
        let session = work::current_session(block);
        let (dot, text, detail): (Option<gpui::Rgba>, String, Option<String>) =
            if let Some(reason) = self.work.failure(id) {
                (
                    Some(Ink::DANGER),
                    "Couldn't start".to_owned(),
                    Some(one_line(reason, 160)),
                )
            } else {
                match self.work.state(block) {
                    WorkState::Ready | WorkState::Done => return None,
                    WorkState::Starting => (
                        Some(colors.primary.alpha(0.3)),
                        "Starting…".to_owned(),
                        None,
                    ),
                    WorkState::Working => (
                        Some(colors.primary.alpha(0.54)),
                        "Working on it".to_owned(),
                        None,
                    ),
                    WorkState::NeedsYou(question) => (
                        Some(Ink::ATTENTION),
                        "Needs you".to_owned(),
                        Some(one_line(
                            question.excerpt.as_deref().unwrap_or(&question.summary),
                            200,
                        )),
                    ),
                    WorkState::Review(pr) => (
                        Some(Ink::FRESH),
                        match pr {
                            Some(pr) => format!("Ready to review · PR #{} open", pr.number),
                            None => "Ready to review".to_owned(),
                        },
                        None,
                    ),
                    state @ (WorkState::Stopped | WorkState::Archived | WorkState::Missing) => {
                        (None, state.label().to_owned(), None)
                    }
                }
            };
        let state = self.work.state(block);
        let mut actions: Vec<(&'static str, WorkRequest)> = Vec::new();
        if let Some(session) = session.clone()
            && !matches!(state, WorkState::Missing | WorkState::Archived)
            && self.work.failure(id).is_none()
        {
            let label = if matches!(state, WorkState::NeedsYou(_)) {
                "Answer in session"
            } else {
                "Open"
            };
            actions.push((label, WorkRequest::Open { session }));
        }
        if self.work.failure(id).is_some() {
            actions.push(("Try again", WorkRequest::Prepare { block: id }));
        } else if block.kind == (BlockKind::Todo { checked: false }) && state.can_restart() {
            actions.push(("Start again", WorkRequest::Prepare { block: id }));
        }

        let mut line = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(20.0))
            .text_size(px(12.5))
            .text_color(colors.secondary)
            .when_some(dot, |el, color| {
                el.child(div().flex_none().size(px(6.0)).rounded_full().bg(color))
            })
            .child(div().font_weight(FontWeight::MEDIUM).child(text));
        for (index, (label, request)) in actions.into_iter().enumerate() {
            line = line.child(div().text_color(colors.tertiary).child("·")).child(
                div()
                    .id(SharedString::from(format!("work-action-{id}-{index}")))
                    .cursor_pointer()
                    .text_color(colors.tertiary)
                    .hover(|el| el.text_color(accent()))
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            cx.emit(EditorEvent::Work(request.clone()));
                        }),
                    ),
            );
        }
        Some(
            div()
                .id(("work-status", id))
                .w_full()
                .pl(px(indent + super::editor_view::MARKER_WIDTH))
                .pb(px(4.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(line)
                .when_some(detail, |el, detail| {
                    el.child(
                        div()
                            .pl(px(14.0))
                            .text_size(px(12.5))
                            .line_height(px(18.0))
                            .text_color(colors.primary.alpha(0.78))
                            .child(detail),
                    )
                })
                .into_any_element(),
        )
    }

    /// The panel under a to-do row, when one is open for it.
    pub(super) fn work_panel_for(
        &self,
        block: BlockId,
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let content = if self.work.start.as_ref().is_some_and(|p| p.block == block) {
            self.start_panel_content(colors, cx)?
        } else if self.work.tick.as_ref().is_some_and(|p| p.block == block) {
            self.tick_panel_content(colors, cx)?
        } else {
            return None;
        };
        // Glass menus are panel windows in the app; fixtures and opaque
        // windows keep the in-window surface they can inspect.
        let surface = diri_ui::FloatingSurface::new(colors, content)
            .radius(crate::floating::MENU_RADIUS);
        Some(
            deferred(
                anchored()
                    .position_mode(gpui::AnchoredPositionMode::Local)
                    .position(point(px(-8.0), px(4.0)))
                    .snap_to_window_with_margin(px(8.0))
                    .child(div().w(px(PANEL_WIDTH)).occlude().child(surface)),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    fn start_panel_content(
        &self,
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let panel = self.work.start.as_ref()?;
        let mut list = div().flex().flex_col().py(px(5.0));
        list = list.child(section_label("Start with", colors));
        if panel.agents.is_empty() {
            list = list.child(
                div()
                    .mx(px(6.0))
                    .px(px(10.0))
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .text_size(px(Typo::ROW.size))
                    .text_color(colors.secondary)
                    .child("No agents installed. Add one in Settings › Agents."),
            );
        }
        for (index, agent) in panel.agents.iter().enumerate() {
            let active = index == panel.selected;
            list = list.child(
                div()
                    .id(("work-agent", index))
                    .mx(px(6.0))
                    .px(px(10.0))
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .rounded(px(ROW_RADIUS))
                    .cursor_pointer()
                    .glass_menu_row(colors, active)
                    .child(
                        div()
                            .w(px(ROW_ICON_SLOT))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                AgentLogo::new(
                                    crate::session_presentation::ui_agent_kind(&agent.kind),
                                    20.0,
                                    colors,
                                )
                                .badged(false),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(Typo::ROW.size))
                            .text_color(colors.primary)
                            .child(agent.name.clone()),
                    )
                    .when(agent.is_default, |el| {
                        el.child(
                            div()
                                .text_size(px(Typo::ROW.size))
                                .text_color(colors.tertiary)
                                .child("Default"),
                        )
                    })
                    .when(active, |el| {
                        el.child(
                            div()
                                .w(px(18.0))
                                .text_size(px(Typo::ROW.size))
                                .text_color(colors.tertiary)
                                .child("↩"),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.confirm_start(index, cx);
                        }),
                    ),
            );
        }
        let brief = &panel.brief;
        let mut summary = vec![format_bytes(brief.prompt.len())];
        if brief.context_lines > 0 {
            summary.push(plural(brief.context_lines, "context line", "context lines"));
        }
        if brief.links > 0 {
            summary.push(plural(brief.links, "link", "links"));
        }
        if brief.notes > 0 {
            summary.push(plural(brief.notes, "note", "notes"));
        }
        if brief.sessions > 0 {
            summary.push(plural(brief.sessions, "session", "sessions"));
        }
        if brief.truncated {
            summary.push("shortened".to_owned());
        }
        list = list
            .child(
                div()
                    .mx(px(12.0))
                    .my(px(5.0))
                    .h(px(1.0))
                    .bg(colors.primary.alpha(0.08)),
            )
            .child(
                div()
                    .px(px(16.0))
                    .pt(px(4.0))
                    .pb(px(6.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(11.5))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.secondary)
                            .child("What the agent gets"),
                    )
                    .child(div().text_color(colors.tertiary).child(summary.join(" · "))),
            )
            .child(
                div()
                    .id("work-brief")
                    .mx(px(12.0))
                    .mb(px(6.0))
                    .max_h(px(PREVIEW_HEIGHT))
                    .overflow_y_scroll()
                    .track_scroll(&panel.scroll)
                    .rounded(px(8.0))
                    .bg(colors.primary.alpha(0.04))
                    .px(px(10.0))
                    .py(px(8.0))
                    .font_family(crate::fonts::mono_family())
                    .text_size(px(11.0))
                    .line_height(px(16.0))
                    .text_color(colors.primary.alpha(0.82))
                    .child(brief.prompt.trim_end().to_owned()),
            );
        Some(list.into_any_element())
    }

    fn tick_panel_content(
        &self,
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let panel = self.work.tick.as_ref()?;
        let mut list = div()
            .flex()
            .flex_col()
            .py(px(5.0))
            .child(section_label("The agent is still working", colors));
        for (index, label) in TICK_CHOICES.iter().enumerate() {
            let active = index == panel.selected;
            list = list.child(
                div()
                    .id(("work-tick", index))
                    .mx(px(6.0))
                    .px(px(10.0))
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .rounded(px(ROW_RADIUS))
                    .cursor_pointer()
                    .glass_menu_row(colors, active)
                    .text_size(px(Typo::ROW.size))
                    .text_color(colors.primary)
                    .when(index == 2, |el| {
                        el.mt(px(1.0)).text_color(colors.secondary)
                    })
                    .child(div().flex_1().child(*label))
                    .when(index == 2, |el| {
                        el.child(div().text_color(colors.tertiary).child("esc"))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.confirm_tick(index, cx);
                        }),
                    ),
            );
        }
        Some(list.into_any_element())
    }
}

fn section_label(text: &'static str, colors: SemanticColors) -> impl IntoElement {
    div()
        .px(px(16.0))
        .pt(px(4.0))
        .pb(px(3.0))
        .text_size(px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(colors.tertiary)
        .child(text)
}

fn one_line(text: &str, max_chars: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max_chars {
        return line;
    }
    let cut: String = line.chars().take(max_chars - 1).collect();
    format!("{}…", cut.trim_end())
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

/// Writes the new session's chip into the to-do in the note file, so the
/// link lands even when the note was closed while the agent started. The
/// to-do is found by the text it had when Start was pressed; the open
/// editor links by block id as well, and both writes are idempotent.
pub(crate) fn link_in_file(link: &crate::store::WorkLink, session: &SessionId) {
    let Ok(store) = diri_notes::store::NoteStore::open(link.notes_dir.clone()) else {
        return;
    };
    let _ = store.update(&link.note_id, |note| {
        let found = note.doc.blocks.iter().position(|block| {
            matches!(block.kind, BlockKind::Todo { .. })
                && work::task_title_matches(block, &link.todo_text)
        });
        if let Some(index) = found {
            diri_notes::handoff::link_session(note, index, &link.label, &session.0);
        }
        Ok(())
    });
}
