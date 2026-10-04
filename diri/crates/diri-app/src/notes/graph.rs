// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//! The notes graph: every note as a dot, every link between two notes as a
//! line, placed by forces (links pull, notes push apart) the way Ely's
//! `NetworkGraph` and Obsidian's graph view lay them out.
//!
//! Two scopes: the open note's neighbourhood (notes within two links, either
//! direction) and every note. Drag the background to pan, scroll to zoom,
//! drag a dot to move it, hover a dot to light its neighbours, click a dot
//! to open that note.
//!
//! The simulation runs on the main thread but is bounded: a few steps per
//! frame within a small time budget, and it asks for another frame only
//! until the layout settles. A settled graph does no work at all.

use std::collections::{HashMap, HashSet};
use std::f32::consts::TAU;
use std::sync::Arc;
use std::time::{Duration, Instant};

use diri_notes::backlinks::{LinkGraph, LinkIndex};
use diri_ui::SemanticColors;
use gpui::{
    Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point, Render,
    ScrollWheelEvent, SharedString, Window, canvas, div, fill, point, prelude::*, px, size,
};

use super::todos::TodosModel;
use crate::icons::sf_symbol;

/// How far the neighbourhood reaches from the open note.
pub(crate) const LOCAL_DEPTH: usize = 2;
/// Steps a frame may run, and the time it may spend on them.
const STEPS_PER_FRAME: usize = 12;
const FRAME_BUDGET: Duration = Duration::from_millis(5);
/// The heat a fresh layout starts at, and a warm restart after an edit.
const HOT: f32 = 0.08;
const WARM: f32 = 0.025;
const SETTLED_HEAT: f32 = 0.001;
const COOLING: f32 = 0.96;
/// Pointer travel that turns a press on a dot into a drag.
const DRAG_SLOP: f32 = 3.0;
const MIN_ZOOM: f32 = 0.3;
const MAX_ZOOM: f32 = 5.0;
/// Space kept between the unit square and the view's edge, in points.
const MARGIN: f32 = 56.0;
/// More notes than this show names only on hover, for their neighbours,
/// for the open note, and once zoomed in.
const LABEL_ALL_BELOW: usize = 60;
const LABEL_ZOOM: f32 = 1.6;

fn fade(color: gpui::Rgba, alpha: f32) -> gpui::Rgba {
    gpui::Rgba {
        a: color.a * alpha,
        ..color
    }
}

/// A stable 64-bit hash (FNV-1a), so a note starts in the same place on
/// every run.
fn hash(text: &str, seed: u64) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ seed;
    for byte in text.bytes() {
        h ^= u64::from(byte);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Places for a graph's nodes in a unit square: every pair pushes apart,
/// every edge pulls together (Fruchterman and Reingold), a little gravity
/// keeps it centred, and each step moves a node no farther than the heat,
/// which cools until the layout settles.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Layout {
    pub(crate) places: Vec<(f32, f32)>,
    heat: f32,
    pinned: HashSet<usize>,
}

impl Layout {
    /// Nodes on a circle in id order, nudged by a seeded hash of their id so
    /// symmetric graphs still unfold. The same ids and seed always give the
    /// same layout.
    pub(crate) fn new(ids: &[&str], seed: u64) -> Self {
        let count = ids.len();
        let places = ids
            .iter()
            .enumerate()
            .map(|(ix, id)| {
                let h = hash(id, seed);
                let jitter = (h & 0xffff) as f32 / 65_535.0 - 0.5;
                let radius = 0.3 + 0.05 * (((h >> 16) & 0xffff) as f32 / 65_535.0);
                let angle = TAU * (ix as f32 + 0.35 * jitter) / count.max(1) as f32;
                (0.5 + radius * angle.cos(), 0.5 + radius * angle.sin())
            })
            .collect();
        Self {
            places,
            heat: HOT,
            pinned: HashSet::new(),
        }
    }

    /// A layout for `ids` that keeps the places of nodes `previous` already
    /// had, starting new ones by their first placed neighbour: an edit to
    /// one note nudges the graph instead of re-shuffling it.
    pub(crate) fn carry(
        ids: &[&str],
        edges: &[(usize, usize)],
        previous: &HashMap<String, (f32, f32)>,
        seed: u64,
    ) -> Self {
        let mut layout = Self::new(ids, seed);
        let mut kept = 0;
        for (ix, id) in ids.iter().enumerate() {
            if let Some(place) = previous.get(*id) {
                layout.places[ix] = *place;
                kept += 1;
            }
        }
        if kept == 0 {
            return layout;
        }
        for (ix, id) in ids.iter().enumerate() {
            if previous.contains_key(*id) {
                continue;
            }
            let neighbour = edges.iter().find_map(|(a, b)| match (*a == ix, *b == ix) {
                (true, _) if previous.contains_key(ids[*b]) => Some(*b),
                (_, true) if previous.contains_key(ids[*a]) => Some(*a),
                _ => None,
            });
            if let Some(n) = neighbour {
                let h = hash(id, seed);
                let angle = TAU * (h & 0xffff) as f32 / 65_535.0;
                let (x, y) = layout.places[n];
                layout.places[ix] = (
                    (x + 0.05 * angle.cos()).clamp(0.0, 1.0),
                    (y + 0.05 * angle.sin()).clamp(0.0, 1.0),
                );
            }
        }
        layout.heat = if kept == ids.len() { WARM / 2.0 } else { WARM };
        layout
    }

    pub(crate) fn settled(&self) -> bool {
        self.heat < SETTLED_HEAT
    }

    /// Holds a node where the pointer put it and warms the rest to settle
    /// around it.
    pub(crate) fn pin(&mut self, ix: usize, place: (f32, f32)) {
        if ix >= self.places.len() {
            return;
        }
        self.places[ix] = (place.0.clamp(0.0, 1.0), place.1.clamp(0.0, 1.0));
        self.pinned.insert(ix);
        self.heat = self.heat.max(WARM);
    }

    pub(crate) fn unpin(&mut self, ix: usize) {
        self.pinned.remove(&ix);
    }

    /// One step of the simulation.
    pub(crate) fn step(&mut self, edges: &[(usize, usize)]) {
        let count = self.places.len();
        if count == 0 {
            self.heat = 0.0;
            return;
        }
        let k = 0.6 / (count as f32).sqrt();
        let mut moves = vec![(0.0f32, 0.0f32); count];
        let apart = |a: (f32, f32), b: (f32, f32)| {
            let (dx, dy) = (a.0 - b.0, a.1 - b.1);
            let distance = dx.hypot(dy).max(1e-3);
            (dx / distance, dy / distance, distance)
        };
        for a in 0..count {
            for b in a + 1..count {
                let (x, y, distance) = apart(self.places[a], self.places[b]);
                let push = k * k / distance;
                moves[a] = (moves[a].0 + x * push, moves[a].1 + y * push);
                moves[b] = (moves[b].0 - x * push, moves[b].1 - y * push);
            }
        }
        for (a, b) in edges {
            if *a >= count || *b >= count {
                continue;
            }
            let (x, y, distance) = apart(self.places[*a], self.places[*b]);
            let pull = distance * distance / k;
            moves[*a] = (moves[*a].0 - x * pull, moves[*a].1 - y * pull);
            moves[*b] = (moves[*b].0 + x * pull, moves[*b].1 + y * pull);
        }
        for (ix, place) in self.places.iter_mut().enumerate() {
            if self.pinned.contains(&ix) {
                continue;
            }
            let (x, y) = (
                moves[ix].0 + (0.5 - place.0) * k,
                moves[ix].1 + (0.5 - place.1) * k,
            );
            let length = x.hypot(y).max(1e-6);
            let reach = length.min(self.heat);
            *place = (
                (place.0 + x / length * reach).clamp(0.0, 1.0),
                (place.1 + y / length * reach).clamp(0.0, 1.0),
            );
        }
        self.heat *= COOLING;
    }

    /// Steps until settled, at most `steps` times or until `budget` is
    /// spent. Returns whether it still needs more.
    pub(crate) fn advance(
        &mut self,
        edges: &[(usize, usize)],
        steps: usize,
        budget: Duration,
    ) -> bool {
        let start = Instant::now();
        for _ in 0..steps {
            if self.settled() {
                break;
            }
            self.step(edges);
            if start.elapsed() >= budget {
                break;
            }
        }
        !self.settled()
    }
}

/// Which notes the graph shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The open note and the notes within [`LOCAL_DEPTH`] links of it.
    Local,
    All,
}

pub(crate) enum GraphEvent {
    /// Open this note.
    Open(String),
    Close,
}

/// What a press on the graph is doing.
#[derive(Clone, Copy, Debug)]
enum Press {
    Node {
        ix: usize,
        from: Point<Pixels>,
        dragging: bool,
    },
    Pan {
        from: Point<Pixels>,
        pan: (f32, f32),
    },
}

pub(crate) struct NoteGraphView {
    model: Entity<TodosModel>,
    /// The index the graph was built from, to skip rebuilds.
    built_from: Option<Arc<LinkIndex>>,
    center: String,
    pub(crate) scope: Scope,
    pub(crate) graph: LinkGraph,
    pub(crate) layout: Layout,
    zoom: f32,
    pan: (f32, f32),
    hover: Option<usize>,
    press: Option<Press>,
    /// Where the canvas was painted, for the pointer.
    bounds: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    focus: FocusHandle,
    colors: SemanticColors,
    _observe: gpui::Subscription,
}

impl EventEmitter<GraphEvent> for NoteGraphView {}

impl Focusable for NoteGraphView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Points per unit of the layout's square at `zoom` in `bounds`.
fn scale(bounds: Bounds<Pixels>, zoom: f32) -> f32 {
    let w = f32::from(bounds.size.width) - MARGIN * 2.0;
    let h = f32::from(bounds.size.height) - MARGIN * 2.0;
    w.min(h).max(80.0) * zoom
}

/// Where a unit-square place lands in `bounds` (window coordinates).
fn project(bounds: Bounds<Pixels>, zoom: f32, pan: (f32, f32), place: (f32, f32)) -> Point<Pixels> {
    let center = bounds.center();
    let scale = scale(bounds, zoom);
    point(
        center.x + px((place.0 - 0.5) * scale + pan.0),
        center.y + px((place.1 - 0.5) * scale + pan.1),
    )
}

/// The graph's own seed: fixed, so a note keeps its place between opens.
const SEED: u64 = 0x6469_7269;

impl NoteGraphView {
    pub(crate) fn new(
        model: Entity<TodosModel>,
        center: String,
        scope: Scope,
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe(&model, |this, _, cx| {
            if this.rebuild(cx) {
                cx.notify();
            }
        });
        let mut view = Self {
            model,
            built_from: None,
            center,
            scope,
            graph: LinkGraph::default(),
            layout: Layout::new(&[], SEED),
            zoom: 1.0,
            pan: (0.0, 0.0),
            hover: None,
            press: None,
            bounds: Default::default(),
            focus: cx.focus_handle(),
            colors,
            _observe: observe,
        };
        view.rebuild(cx);
        view
    }

    pub(crate) fn set_colors(&mut self, colors: SemanticColors) {
        self.colors = colors;
    }

    pub(crate) fn set_center(&mut self, center: &str, cx: &mut Context<Self>) {
        if self.center != center {
            self.center = center.to_owned();
            self.built_from = None;
            self.rebuild(cx);
            cx.notify();
        }
    }

    pub(crate) fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.built_from = None;
            self.zoom = 1.0;
            self.pan = (0.0, 0.0);
            self.rebuild(cx);
            cx.notify();
        }
    }

    /// Re-reads the graph when the index changed. Returns whether it did.
    fn rebuild(&mut self, cx: &mut Context<Self>) -> bool {
        let links = self.model.read(cx).links();
        if self
            .built_from
            .as_ref()
            .is_some_and(|built| Arc::ptr_eq(built, &links))
        {
            return false;
        }
        let graph = match self.scope {
            Scope::Local => links.neighborhood(&self.center, LOCAL_DEPTH),
            Scope::All => links.graph(),
        };
        self.built_from = Some(links);
        if graph == self.graph {
            return false;
        }
        let previous: HashMap<String, (f32, f32)> = self
            .graph
            .nodes
            .iter()
            .map(|n| n.id.clone())
            .zip(self.layout.places.iter().copied())
            .collect();
        let ids: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
        self.layout = Layout::carry(&ids, &graph.edges, &previous, SEED);
        self.graph = graph;
        self.hover = None;
        self.press = None;
        true
    }

    /// Whether another frame is wanted: only while the layout moves.
    #[cfg(test)]
    pub(crate) fn animating(&self) -> bool {
        !self.layout.settled()
    }

    /// Runs the simulation to rest, for tests and screenshots.
    #[cfg(test)]
    pub(crate) fn settle_for_test(&mut self) -> usize {
        let mut steps = 0;
        while !self.layout.settled() && steps < 10_000 {
            self.layout.step(&self.graph.edges);
            steps += 1;
        }
        steps
    }

    /// A unit-square place on screen (window coordinates).
    fn to_screen(&self, place: (f32, f32)) -> Point<Pixels> {
        project(self.bounds.get(), self.zoom, self.pan, place)
    }

    fn to_unit(&self, at: Point<Pixels>) -> (f32, f32) {
        let bounds = self.bounds.get();
        let center = bounds.center();
        let scale = scale(bounds, self.zoom);
        (
            (f32::from(at.x - center.x) - self.pan.0) / scale + 0.5,
            (f32::from(at.y - center.y) - self.pan.1) / scale + 0.5,
        )
    }

    fn radius(&self, ix: usize) -> f32 {
        let degree = self.graph.nodes.get(ix).map_or(0, |n| n.degree) as f32;
        (3.5 + degree.sqrt() * 1.6) * self.zoom.clamp(0.7, 1.8)
    }

    fn hit(&self, at: Point<Pixels>) -> Option<usize> {
        (0..self.graph.nodes.len())
            .map(|ix| {
                let p = self.to_screen(self.layout.places[ix]);
                (ix, f32::from(p.x - at.x).hypot(f32::from(p.y - at.y)))
            })
            .filter(|(ix, far)| *far <= self.radius(*ix) + 5.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(ix, _)| ix)
    }

    fn neighbours(&self, ix: usize) -> HashSet<usize> {
        self.graph
            .edges
            .iter()
            .filter_map(|(a, b)| {
                if *a == ix {
                    Some(*b)
                } else if *b == ix {
                    Some(*a)
                } else {
                    None
                }
            })
            .chain([ix])
            .collect()
    }

    fn on_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.press = Some(match self.hit(event.position) {
            Some(ix) => Press::Node {
                ix,
                from: event.position,
                dragging: false,
            },
            None => Press::Pan {
                from: event.position,
                pan: self.pan,
            },
        });
        cx.stop_propagation();
    }

    fn on_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let pressed = event.pressed_button == Some(MouseButton::Left);
        match (self.press, pressed) {
            (Some(Press::Node { ix, from, dragging }), true) => {
                let moved = f32::from(event.position.x - from.x)
                    .hypot(f32::from(event.position.y - from.y));
                if dragging || moved > DRAG_SLOP {
                    self.press = Some(Press::Node {
                        ix,
                        from,
                        dragging: true,
                    });
                    let place = self.to_unit(event.position);
                    self.layout.pin(ix, place);
                    cx.notify();
                }
            }
            (Some(Press::Pan { from, pan }), true) => {
                self.pan = (
                    pan.0 + f32::from(event.position.x - from.x),
                    pan.1 + f32::from(event.position.y - from.y),
                );
                cx.notify();
            }
            _ => {
                let next = self.hit(event.position);
                if next != self.hover {
                    self.hover = next;
                    cx.notify();
                }
            }
        }
    }

    fn on_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        match self.press.take() {
            Some(Press::Node {
                ix,
                dragging: false,
                ..
            }) => {
                if let Some(node) = self.graph.nodes.get(ix) {
                    cx.emit(GraphEvent::Open(node.id.clone()));
                }
            }
            Some(Press::Node {
                ix, dragging: true, ..
            }) => {
                // Let go: it settles back among its neighbours.
                self.layout.unpin(ix);
                cx.notify();
            }
            _ => {}
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let dy = f32::from(event.delta.pixel_delta(px(16.0)).y);
        if dy == 0.0 {
            return;
        }
        let before = self.to_unit(event.position);
        self.zoom = (self.zoom * (dy * 0.004).exp()).clamp(MIN_ZOOM, MAX_ZOOM);
        // Zoom around the pointer: the place under it stays under it.
        let after = self.to_screen(before);
        self.pan.0 += f32::from(event.position.x - after.x);
        self.pan.1 += f32::from(event.position.y - after.y);
        cx.stop_propagation();
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            cx.emit(GraphEvent::Close);
        }
    }

    fn scope_button(
        &self,
        scope: Scope,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = self.colors;
        let on = self.scope == scope;
        div()
            .id(label)
            .px(px(10.0))
            .h(px(24.0))
            .flex()
            .items_center()
            .rounded(px(6.0))
            .cursor_pointer()
            .text_size(px(diri_ui::Typo::META.size))
            .font_weight(diri_ui::Typo::META.weight)
            .text_color(if on { colors.primary } else { colors.secondary })
            .when(on, |b| b.bg(fade(colors.primary, 0.08)))
            .hover(|b| b.text_color(colors.primary))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.set_scope(scope, cx);
                }),
            )
            .child(label)
    }
}

impl Render for NoteGraphView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self
            .layout
            .advance(&self.graph.edges, STEPS_PER_FRAME, FRAME_BUDGET)
        {
            window.request_animation_frame();
        }
        let colors = self.colors;
        let accent = super::editor_view::accent();
        let count = self.graph.nodes.len();
        let center_ix = self.graph.nodes.iter().position(|n| n.id == self.center);
        let focus_ix = self.hover.or(match self.press {
            Some(Press::Node { ix, .. }) => Some(ix),
            _ => None,
        });
        let near = focus_ix.map(|ix| self.neighbours(ix));
        let lit = |ix: usize| near.as_ref().is_none_or(|near| near.contains(&ix));

        let screen: Vec<Point<Pixels>> = self
            .layout
            .places
            .iter()
            .map(|place| self.to_screen(*place))
            .collect();
        let radii: Vec<f32> = (0..count).map(|ix| self.radius(ix)).collect();
        let origin = self.bounds.get().origin;

        // Names: all of them for a small graph or once zoomed in, else the
        // ones that matter right now.
        let show_all = count <= LABEL_ALL_BELOW || self.zoom >= LABEL_ZOOM;
        let labels: Vec<gpui::AnyElement> = (0..count)
            .filter(|ix| {
                show_all
                    || Some(*ix) == center_ix
                    || near.as_ref().is_some_and(|near| near.contains(ix))
            })
            .take(400)
            .map(|ix| {
                let p = screen[ix] - origin;
                let strong = Some(ix) == focus_ix || Some(ix) == center_ix;
                div()
                    .absolute()
                    .left(p.x - px(80.0))
                    .top(p.y + px(radii[ix] + 3.0))
                    .w(px(160.0))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .max_w(px(160.0))
                            .px(px(3.0))
                            .rounded(px(3.0))
                            // Lines pass under a name, never through it.
                            .bg(fade(colors.work_surface_nested(), 0.85))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(11.0))
                            .text_color(if strong {
                                colors.primary
                            } else {
                                colors.secondary
                            })
                            .when(!lit(ix), |label| label.opacity(0.3))
                            .child(SharedString::from(self.graph.nodes[ix].title.clone())),
                    )
                    .into_any_element()
            })
            .collect();

        let edges = self.graph.edges.clone();
        let fills: Vec<gpui::Rgba> = (0..count)
            .map(|ix| {
                let base = if Some(ix) == center_ix || Some(ix) == focus_ix {
                    accent
                } else if self.graph.nodes[ix].degree == 0 {
                    colors.tertiary
                } else {
                    colors.secondary
                };
                if lit(ix) { base } else { fade(base, 0.25) }
            })
            .collect();
        let edge_lit: Vec<bool> = edges
            .iter()
            .map(|(a, b)| focus_ix.is_some_and(|f| f == *a || f == *b))
            .collect();
        let any_focus = focus_ix.is_some();
        let bounds_cell = std::rc::Rc::clone(&self.bounds);
        let (places, zoom, pan) = (self.layout.places.clone(), self.zoom, self.pan);
        let plot = canvas(
            move |bounds, window, _| {
                // Names are placed from the last frame's bounds: when the
                // view moved or resized, draw once more with the new ones.
                if bounds_cell.get() != bounds {
                    bounds_cell.set(bounds);
                    window.refresh();
                }
            },
            move |bounds, _, window, _| {
                let paint_screen: Vec<Point<Pixels>> = places
                    .iter()
                    .map(|place| project(bounds, zoom, pan, *place))
                    .collect();
                let mut quiet = PathBuilder::stroke(px(1.0));
                let mut strong = PathBuilder::stroke(px(1.4));
                let (mut any_quiet, mut any_strong) = (false, false);
                for (i, (a, b)) in edges.iter().enumerate() {
                    let (Some(pa), Some(pb)) = (paint_screen.get(*a), paint_screen.get(*b)) else {
                        continue;
                    };
                    let path = if edge_lit[i] {
                        any_strong = true;
                        &mut strong
                    } else {
                        any_quiet = true;
                        &mut quiet
                    };
                    path.move_to(*pa);
                    path.line_to(*pb);
                }
                let quiet_ink = if any_focus {
                    fade(colors.primary, 0.06)
                } else {
                    fade(colors.primary, 0.16)
                };
                if any_quiet && let Ok(path) = quiet.build() {
                    window.paint_path(path, quiet_ink);
                }
                if any_strong && let Ok(path) = strong.build() {
                    window.paint_path(path, fade(accent, 0.75));
                }
                for (ix, p) in paint_screen.iter().enumerate() {
                    let r = px(radii[ix]);
                    let dot = Bounds::new(point(p.x - r, p.y - r), size(r * 2.0, r * 2.0));
                    window.paint_quad(fill(dot, fills[ix]).corner_radii(r));
                }
            },
        )
        .absolute()
        .inset_0();

        let links = self.graph.edges.len();
        let summary = format!(
            "{count} {} · {links} {}",
            if count == 1 { "note" } else { "notes" },
            if links == 1 { "link" } else { "links" }
        );
        let cursor_pointer = self.hover.is_some();
        let header = div()
            .absolute()
            .top(px(12.0))
            .left(px(16.0))
            .right(px(12.0))
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(diri_ui::Typo::TITLE.size))
                            .font_weight(diri_ui::Typo::TITLE.weight)
                            .text_color(colors.primary)
                            .child("Graph"),
                    )
                    .child(
                        div()
                            .text_size(px(diri_ui::Typo::META.size))
                            .text_color(colors.tertiary)
                            .child(summary),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .p(px(2.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(fade(colors.primary, 0.08))
                            .child(self.scope_button(Scope::Local, "This note", cx))
                            .child(self.scope_button(Scope::All, "All notes", cx)),
                    )
                    .child(
                        div()
                            .id("note-graph-close")
                            .size(px(26.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .hover(|b| b.bg(fade(colors.primary, 0.06)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_, _: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(GraphEvent::Close);
                                }),
                            )
                            .child(sf_symbol("xmark", 12.0, colors.secondary)),
                    ),
            );
        let empty = (count <= 1).then(|| {
            div()
                .absolute()
                .bottom(px(48.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .text_size(px(diri_ui::Typo::ROW.size))
                .text_color(colors.tertiary)
                .child(match self.scope {
                    Scope::Local => "No links yet. Type [[ or @ in a note to link another note.",
                    Scope::All => "No notes yet.",
                })
        });
        let hint = div()
            .absolute()
            .bottom(px(12.0))
            .left(px(16.0))
            .text_size(px(diri_ui::Typo::META.size))
            .text_color(colors.tertiary)
            .child("Click a note to open it · drag to move · scroll to zoom");

        div()
            .id("note-graph")
            .key_context("DiriNoteGraph")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(colors.work_surface_nested())
            .when(cursor_pointer, |el| el.cursor_pointer())
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(plot)
            .children(labels)
            .children(empty)
            .child(header)
            .child(hint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settle(layout: &mut Layout, edges: &[(usize, usize)]) -> usize {
        let mut steps = 0;
        while !layout.settled() {
            layout.step(edges);
            steps += 1;
            assert!(steps < 1_000, "the layout must cool and stop");
        }
        steps
    }

    #[test]
    fn the_same_seed_gives_the_same_layout() {
        let ids = ["a", "b", "c", "d", "e"];
        let edges = [(0, 1), (1, 2), (2, 3), (0, 3)];
        let mut one = Layout::new(&ids, 7);
        let mut two = Layout::new(&ids, 7);
        let steps = settle(&mut one, &edges);
        assert_eq!(settle(&mut two, &edges), steps);
        assert_eq!(one, two, "deterministic for a seed");
        assert!(steps < 200, "settles in bounded steps: {steps}");
        let mut other = Layout::new(&ids, 8);
        settle(&mut other, &edges);
        assert_ne!(
            one.places, other.places,
            "a different seed, a different start"
        );
    }

    #[test]
    fn linked_notes_sit_closer_and_a_settled_layout_stops() {
        let ids = ["a", "b", "c", "loner"];
        let edges = [(0, 1), (1, 2)];
        let mut layout = Layout::new(&ids, 1);
        settle(&mut layout, &edges);
        let far = |a: usize, b: usize| {
            let (pa, pb) = (layout.places[a], layout.places[b]);
            (pa.0 - pb.0).hypot(pa.1 - pb.1)
        };
        assert!(far(0, 1) < far(0, 3) && far(1, 2) < far(2, 3));
        // Settled: advancing does nothing and wants no frame.
        let before = layout.clone();
        assert!(!layout.advance(&edges, 50, Duration::from_secs(1)));
        assert_eq!(layout, before);
        // A drag pins one node and warms the rest; letting go settles again.
        layout.pin(3, (0.1, 0.1));
        assert!(!layout.settled());
        layout.step(&edges);
        assert_eq!(layout.places[3], (0.1, 0.1), "a pinned node stays put");
        layout.unpin(3);
        settle(&mut layout, &edges);
    }

    #[test]
    fn an_edit_keeps_known_places_and_starts_new_notes_by_a_neighbour() {
        let ids = ["a", "b"];
        let mut first = Layout::new(&ids, 3);
        settle(&mut first, &[(0, 1)]);
        let previous: HashMap<String, (f32, f32)> = ids
            .iter()
            .map(|id| (*id).to_owned())
            .zip(first.places.iter().copied())
            .collect();
        let grown = Layout::carry(&["a", "b", "c"], &[(0, 1), (1, 2)], &previous, 3);
        assert_eq!(grown.places[0], first.places[0]);
        assert_eq!(grown.places[1], first.places[1]);
        let (b, c) = (grown.places[1], grown.places[2]);
        assert!((b.0 - c.0).hypot(b.1 - c.1) < 0.06, "c starts beside b");
        assert!(!grown.settled(), "warm enough to make room");
        let empty = Layout::new(&[], 3);
        let mut empty = empty;
        empty.step(&[]);
        assert!(empty.settled());
    }
}
