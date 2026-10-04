// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//! Developer performance tooling: a frame meter overlay (Ely's `FpsMeter`)
//! and per-view render counters (Ely's `RenderCounter`). Both are off by
//! default and toggled from the command palette, ⌥⌘P / ⌥⌘R, or Settings ›
//! General › Developer.
//!
//! Unlike Ely's meter, this one never asks for frames. diri draws on demand,
//! so a meter that requested an animation frame every frame would spin the
//! app at the display rate and mostly measure itself. Instead it reads the
//! frames diri draws anyway, from the probe telemetry already paints last in
//! every main window ([`crate::telemetry::frame_probe`]), and shows "idle"
//! once nothing has drawn for a second. Its only frame of its own is the one
//! that repaints it as idle; that frame is marked and left out of the stats.
//!
//! Disabled, each instrumented `render` pays one relaxed atomic load and a
//! branch; nothing is allocated, timed or stored.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use diri_ui::{Ink, Radius, SemanticColors};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, FontFeatures, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Rgba, StatefulInteractiveElement as _, Styled as _, Window,
    canvas, div, fill, point, px, size,
};

use crate::store::Prefs;

/// A frame's budget at 60 Hz.
const BUDGET: Duration = Duration::from_micros(16_667);
/// How far back "per second" figures look.
const SPAN: Duration = Duration::from_secs(1);
/// Frames kept for percentiles.
const HISTORY: usize = 120;
/// Bars the sparkline shows.
const BARS: usize = 60;
/// Frames further apart than this are separate bursts: the gap between them
/// is diri having nothing to draw, not frames it dropped.
const BURST_GAP: Duration = Duration::from_millis(250);
/// Render stamps kept per view, enough for a view rendering every frame at
/// 120 Hz with room to spare.
const RECENT_RENDERS: usize = 512;

// ---------------------------------------------------------------------------
// Enablement
// ---------------------------------------------------------------------------

/// The two switches. Production keeps them in process-wide atomics, read
/// once per instrumented render. Tests run in parallel on their own threads,
/// each with its own GPUI app, so there they are per thread: one test
/// turning the overlay on must not leak into another's windows.
mod gate {
    pub(crate) use imp::{overlay, renders, set};

    #[cfg(not(test))]
    mod imp {
        use std::sync::atomic::{AtomicBool, Ordering};

        static OVERLAY: AtomicBool = AtomicBool::new(false);
        static RENDERS: AtomicBool = AtomicBool::new(false);

        #[inline]
        pub(crate) fn overlay() -> bool {
            OVERLAY.load(Ordering::Relaxed)
        }

        #[inline]
        pub(crate) fn renders() -> bool {
            RENDERS.load(Ordering::Relaxed)
        }

        pub(crate) fn set(overlay: bool, renders: bool) {
            OVERLAY.store(overlay, Ordering::Relaxed);
            RENDERS.store(renders, Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    mod imp {
        use std::cell::Cell;

        thread_local! {
            static OVERLAY: Cell<bool> = const { Cell::new(false) };
            static RENDERS: Cell<bool> = const { Cell::new(false) };
        }

        pub(crate) fn overlay() -> bool {
            OVERLAY.with(Cell::get)
        }

        pub(crate) fn renders() -> bool {
            RENDERS.with(Cell::get)
        }

        pub(crate) fn set(overlay: bool, renders: bool) {
            OVERLAY.with(|cell| cell.set(overlay));
            RENDERS.with(|cell| cell.set(renders));
        }
    }
}

/// Whether the frame meter overlay is showing.
#[inline]
pub(crate) fn overlay_enabled() -> bool {
    gate::overlay()
}

/// Whether views count their renders and wear a count badge.
#[inline]
pub(crate) fn render_counters_enabled() -> bool {
    gate::renders()
}

/// Brings the switches in line with saved preferences. Call at launch and
/// after every preference edit that may have changed them; the caller
/// refreshes windows so badges appear in views that are otherwise cached.
pub(crate) fn apply_prefs(prefs: &Prefs) {
    let was_overlay = overlay_enabled();
    gate::set(prefs.perf_overlay, prefs.render_counters);
    if prefs.perf_overlay && !was_overlay {
        // Stats from the last time it was shown would read as current.
        FRAMES.with(|frames| frames.borrow_mut().clear());
    }
}

// ---------------------------------------------------------------------------
// Frame stats
// ---------------------------------------------------------------------------

/// Frames missed before one arriving `interval` after the last, allowing half
/// a frame of jitter.
fn missed(interval: Duration) -> u32 {
    (interval.as_secs_f64() / BUDGET.as_secs_f64() - 0.5).max(0.0) as u32
}

/// One frame a main window drew, as the telemetry probe saw it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FrameSample {
    /// When the probe painted: the end of the frame's CPU work.
    pub(crate) end: Instant,
    /// Root render start to probe: render, layout, prepaint and paint.
    pub(crate) cost: Duration,
    pub(crate) views_rendered: u32,
    pub(crate) views_reused: u32,
    pub(crate) terminal_paints: u64,
}

/// A window's recent frames.
#[derive(Default)]
pub(crate) struct FrameLog {
    /// Oldest first, each with the frames missed just before it.
    samples: VecDeque<(FrameSample, u32)>,
    frames_total: u64,
    dropped_total: u64,
    /// The next frame is the overlay repainting itself as idle.
    skip_next: bool,
}

/// What the overlay shows about a window's frames at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FrameReadout {
    /// Frames drawn in the last second.
    pub(crate) fps: u32,
    /// Nothing drew in the last second.
    pub(crate) idle: bool,
    pub(crate) last: Option<FrameSample>,
    pub(crate) p50: Option<Duration>,
    pub(crate) p95: Option<Duration>,
    pub(crate) worst: Option<Duration>,
    /// Frames missed inside bursts over the last second, and since reset.
    pub(crate) dropped_recent: u32,
    pub(crate) dropped_total: u64,
    pub(crate) frames_total: u64,
    /// The most recent frames' costs, oldest first, at most [`BARS`].
    pub(crate) bars: Vec<Duration>,
}

impl FrameLog {
    pub(crate) fn record(&mut self, sample: FrameSample) {
        if std::mem::take(&mut self.skip_next) {
            return;
        }
        let missed = self
            .samples
            .back()
            .map(|(previous, _)| sample.end.saturating_duration_since(previous.end))
            .filter(|interval| *interval < BURST_GAP)
            .map_or(0, missed);
        self.samples.push_back((sample, missed));
        while self.samples.len() > HISTORY {
            self.samples.pop_front();
        }
        self.frames_total += 1;
        self.dropped_total += u64::from(missed);
    }

    /// Leaves the next frame out: the overlay asked for it.
    pub(crate) fn skip_next(&mut self) {
        self.skip_next = true;
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn last_end(&self) -> Option<Instant> {
        self.samples.back().map(|(sample, _)| sample.end)
    }

    pub(crate) fn readout(&self, now: Instant) -> FrameReadout {
        let recent = self
            .samples
            .iter()
            .filter(|(sample, _)| now.saturating_duration_since(sample.end) < SPAN);
        let (fps, dropped_recent) = recent.fold((0u32, 0u32), |(n, dropped), (_, missed)| {
            (n + 1, dropped + missed)
        });
        let mut costs: Vec<Duration> = self.samples.iter().map(|(s, _)| s.cost).collect();
        let bars = costs[costs.len().saturating_sub(BARS)..].to_vec();
        costs.sort_unstable();
        FrameReadout {
            fps,
            idle: fps == 0,
            last: self.samples.back().map(|(sample, _)| *sample),
            p50: percentile(&costs, 0.50),
            p95: percentile(&costs, 0.95),
            worst: costs.last().copied(),
            dropped_recent,
            dropped_total: self.dropped_total,
            frames_total: self.frames_total,
            bars,
        }
    }
}

/// Nearest-rank percentile of sorted values.
fn percentile(sorted: &[Duration], p: f64) -> Option<Duration> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted.get(rank.clamp(1, sorted.len()) - 1).copied()
}

thread_local! {
    /// Per main window, keyed by window id. GPUI draws on the main thread.
    static FRAMES: RefCell<HashMap<u64, FrameLog>> = RefCell::new(HashMap::new());
    static MEMORY: Cell<Option<(Instant, Memory)>> = const { Cell::new(None) };
}

/// A main window finished a frame. Only called while the overlay shows.
pub(crate) fn record_frame(window: u64, sample: FrameSample) {
    FRAMES.with(|frames| {
        frames
            .borrow_mut()
            .entry(window)
            .or_default()
            .record(sample)
    });
}

/// The window's next frame is the overlay's own idle repaint.
pub(crate) fn skip_next_frame(window: u64) {
    FRAMES.with(|frames| frames.borrow_mut().entry(window).or_default().skip_next());
}

/// Whether the window is waiting on the overlay's own repaint.
pub(crate) fn repaint_pending(window: u64) -> bool {
    FRAMES.with(|frames| {
        frames
            .borrow()
            .get(&window)
            .is_some_and(|log| log.skip_next)
    })
}

/// How long ago the window last drew a counted frame.
pub(crate) fn since_last_frame(window: u64, now: Instant) -> Option<Duration> {
    FRAMES.with(|frames| {
        frames
            .borrow()
            .get(&window)
            .and_then(FrameLog::last_end)
            .map(|end| now.saturating_duration_since(end))
    })
}

pub(crate) fn forget_window(window: u64) {
    FRAMES.with(|frames| frames.borrow_mut().remove(&window));
}

fn frame_readout(window: u64, now: Instant) -> FrameReadout {
    FRAMES.with(|frames| {
        frames
            .borrow()
            .get(&window)
            .map(|log| log.readout(now))
            .unwrap_or_else(|| FrameReadout {
                idle: true,
                ..FrameReadout::default()
            })
    })
}

/// How long the overlay waits after the last frame before it repaints itself
/// as idle.
pub(crate) const IDLE_AFTER: Duration = SPAN;

/// The process's memory, sampled at most once a second while the overlay
/// draws (one `proc_pidinfo` and a `/dev/fd` count).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Memory {
    pub(crate) footprint_bytes: u64,
    pub(crate) open_fds: u64,
}

fn memory(now: Instant) -> Memory {
    MEMORY.with(|cell| {
        if let Some((at, memory)) = cell.get()
            && now.saturating_duration_since(at) < SPAN
        {
            return memory;
        }
        let stats = diri_telemetry::ProcessStats::current();
        let memory = Memory {
            footprint_bytes: stats.footprint_bytes,
            open_fds: stats.open_fds,
        };
        cell.set(Some((now, memory)));
        memory
    })
}

// ---------------------------------------------------------------------------
// Render counters
// ---------------------------------------------------------------------------

/// Renders per view name: a total and the stamps of the last second.
#[derive(Default)]
pub(crate) struct RenderCounts {
    entries: Vec<RenderEntry>,
}

struct RenderEntry {
    name: &'static str,
    total: u64,
    recent: VecDeque<Instant>,
}

/// One row of the overlay's render list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderRow {
    pub(crate) name: &'static str,
    pub(crate) per_second: u32,
    pub(crate) total: u64,
}

impl RenderCounts {
    /// Counts a render of `name` at `now` and returns its total. A handful of
    /// names, so a linear scan beats hashing.
    pub(crate) fn hit(&mut self, name: &'static str, now: Instant) -> u64 {
        let index = match self.entries.iter().position(|entry| entry.name == name) {
            Some(index) => index,
            None => {
                self.entries.push(RenderEntry {
                    name,
                    total: 0,
                    recent: VecDeque::new(),
                });
                self.entries.len() - 1
            }
        };
        let entry = &mut self.entries[index];
        entry.total += 1;
        while entry
            .recent
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= SPAN)
            || entry.recent.len() >= RECENT_RENDERS
        {
            entry.recent.pop_front();
        }
        entry.recent.push_back(now);
        entry.total
    }

    pub(crate) fn total(&self, name: &str) -> u64 {
        self.entries
            .iter()
            .find(|entry| entry.name == name)
            .map_or(0, |entry| entry.total)
    }

    /// Every counted view, busiest over the last second first.
    pub(crate) fn rows(&self, now: Instant) -> Vec<RenderRow> {
        let mut rows: Vec<RenderRow> = self
            .entries
            .iter()
            .map(|entry| RenderRow {
                name: entry.name,
                per_second: entry
                    .recent
                    .iter()
                    .filter(|at| now.saturating_duration_since(**at) < SPAN)
                    .count() as u32,
                total: entry.total,
            })
            .collect();
        rows.sort_by(|a, b| {
            b.per_second
                .cmp(&a.per_second)
                .then(b.total.cmp(&a.total))
                .then(a.name.cmp(b.name))
        });
        rows
    }

    pub(crate) fn reset(&mut self) {
        self.entries.clear();
    }
}

thread_local! {
    static RENDERS: RefCell<RenderCounts> = RefCell::new(RenderCounts::default());
}

/// Counts one render of the view `name`. Call first thing in `render`; a
/// relaxed load and a branch while render counters are off.
#[inline]
pub(crate) fn rendered(name: &'static str) {
    if render_counters_enabled() {
        count_render(name);
    }
}

#[cold]
#[inline(never)]
fn count_render(name: &'static str) {
    RENDERS.with(|counts| counts.borrow_mut().hit(name, Instant::now()));
}

/// Clears render counts and frame totals.
pub(crate) fn reset() {
    RENDERS.with(|counts| counts.borrow_mut().reset());
    FRAMES.with(|frames| frames.borrow_mut().values_mut().for_each(FrameLog::reset));
}

/// How many times `name` rendered since the last reset, shown on the view
/// itself. `None` while render counters are off, so call sites cost nothing.
/// Like Ely's counter it shows a count and nothing moves: motion would
/// redraw the view it counts.
pub(crate) fn badge(name: &'static str) -> Option<AnyElement> {
    if !render_counters_enabled() {
        return None;
    }
    let total = RENDERS.with(|counts| counts.borrow().total(name));
    Some(render_badge(name, total))
}

pub(crate) fn render_badge(name: &'static str, total: u64) -> AnyElement {
    // A fixed dark ink rather than the theme: it is a measurement laid over
    // whatever the view paints, light or dark, and must read on both.
    div()
        .absolute()
        .left(px(6.0))
        .bottom(px(6.0))
        .h(px(16.0))
        .px(px(5.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .rounded(px(Radius::CHIP))
        .bg(gpui::rgba(0x0b0c0edd))
        .border_1()
        .border_color(gpui::rgba(0xffffff1f))
        .text_size(px(10.0))
        .font_family(crate::fonts::mono_family())
        .debug_selector(move || format!("render-badge-{name}"))
        .child(div().text_color(gpui::rgba(0xffffffa6)).child(name))
        .child(
            div()
                .text_color(gpui::rgba(0xffffffff))
                .font_weight(FontWeight::SEMIBOLD)
                .child(group_digits(total)),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Overlay
// ---------------------------------------------------------------------------

/// Which window corner the overlay sits in. Not saved: it is moved out of
/// the way of whatever is being measured, which differs every time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Corner {
    #[default]
    TopRight,
    BottomRight,
    BottomLeft,
    TopLeft,
}

impl Corner {
    pub(crate) const fn next(self) -> Self {
        match self {
            Self::TopRight => Self::BottomRight,
            Self::BottomRight => Self::BottomLeft,
            Self::BottomLeft => Self::TopLeft,
            Self::TopLeft => Self::TopRight,
        }
    }
}

thread_local! {
    static CORNER: Cell<Corner> = const { Cell::new(Corner::TopRight) };
}

pub(crate) fn cycle_corner() {
    CORNER.with(|corner| corner.set(corner.get().next()));
}

/// Everything the overlay shows, gathered once per frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OverlayModel {
    pub(crate) frames: FrameReadout,
    pub(crate) memory: Option<Memory>,
    /// The render list, while render counters are on.
    pub(crate) renders: Option<Vec<RenderRow>>,
    pub(crate) corner: Corner,
}

impl OverlayModel {
    /// `now` is the executor's clock, the one frames are stamped with (and
    /// which tests advance by hand).
    pub(crate) fn current(window: u64, now: Instant) -> Self {
        let wall = Instant::now();
        Self {
            frames: frame_readout(window, now),
            memory: Some(memory(wall)),
            renders: render_counters_enabled()
                .then(|| RENDERS.with(|counts| counts.borrow().rows(wall))),
            corner: CORNER.with(Cell::get),
        }
    }
}

pub(crate) type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// The overlay's three buttons.
pub(crate) struct OverlayActions {
    pub(crate) move_corner: Handler,
    pub(crate) reset: Handler,
    pub(crate) close: Handler,
}

const WIDTH: f32 = 236.0;
const INSET: f32 = 12.0;
/// Clears the title bar's traffic lights and toolbar.
const TOP_INSET: f32 = 48.0;

fn tnum() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

fn ms(duration: Duration) -> String {
    let ms = duration.as_secs_f64() * 1000.0;
    if ms >= 100.0 {
        format!("{ms:.0}")
    } else {
        format!("{ms:.1}")
    }
}

fn opt_ms(duration: Option<Duration>) -> String {
    duration.map_or_else(|| "—".to_owned(), ms)
}

/// `1234567` as `1,234,567`.
fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn megabytes(bytes: u64) -> String {
    format!("{} MB", group_digits(bytes / (1024 * 1024)))
}

/// Bar color for a frame's cost against the 60 Hz budget: within it, over
/// it, more than twice over.
fn bar_color(cost: Duration) -> Rgba {
    if cost <= BUDGET {
        Ink::FRESH
    } else if cost <= BUDGET * 2 {
        Ink::ATTENTION
    } else {
        Ink::DANGER
    }
}

/// The frame meter: frames in the last second (or idle), the worst recent
/// frame, a bar per recent frame's CPU cost against the 60 Hz budget, cost
/// percentiles, dropped frames, what the last frame rendered, memory, and,
/// with render counters on, renders per view.
pub(crate) fn overlay(
    model: OverlayModel,
    colors: SemanticColors,
    actions: OverlayActions,
) -> AnyElement {
    let frames = &model.frames;
    let primary = colors.primary;
    let secondary = colors.secondary;
    let tertiary = colors.tertiary;
    let hairline = colors.primary.alpha(0.10);

    let headline = if frames.idle {
        div()
            .debug_selector(|| "perf-fps-idle".into())
            .text_size(px(18.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(secondary)
            .child("idle")
    } else {
        let fps = frames.fps;
        div()
            .debug_selector(move || format!("perf-fps-{fps}"))
            .flex()
            .items_baseline()
            .gap(px(3.0))
            .child(
                div()
                    .text_size(px(18.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .font_features(tnum())
                    .text_color(primary)
                    .child(fps.to_string()),
            )
            .child(div().text_size(px(11.0)).text_color(tertiary).child("fps"))
    };

    let button = |id: &'static str, icon: &'static str, handler: Handler| {
        div()
            .id(id)
            .debug_selector(move || id.into())
            .size(px(20.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(Radius::CHIP))
            .cursor_pointer()
            .hover(move |style| style.bg(primary.alpha(0.08)))
            .on_click(move |event, window, cx| {
                cx.stop_propagation();
                handler(event, window, cx);
            })
            .child(crate::icons::sf_symbol(icon, 11.0, secondary))
    };

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(headline)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(1.0))
                .child(button(
                    "perf-overlay-corner",
                    "arrow.up.left.and.arrow.down.right",
                    actions.move_corner,
                ))
                .child(button(
                    "perf-overlay-reset",
                    "arrow.counterclockwise",
                    actions.reset,
                ))
                .child(button("perf-overlay-close", "xmark", actions.close)),
        );

    let worst = frames.worst;
    let dropped = frames.dropped_recent;
    let summary = div()
        .flex()
        .justify_between()
        .text_size(px(11.0))
        .font_features(tnum())
        .text_color(secondary)
        .child(format!("worst {} ms", opt_ms(worst)))
        .child(
            div()
                .debug_selector(move || format!("perf-dropped-{dropped}"))
                .text_color(if dropped > 0 {
                    Ink::ATTENTION
                } else {
                    secondary
                })
                .child(format!("{dropped} dropped")),
        );

    let bars = frames.bars.clone();
    let budget_line = colors.primary.alpha(0.28);
    let track = colors.primary.alpha(0.04);
    let graph = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            window.paint_quad(fill(bounds, track));
            let slot = bounds.size.width / BARS as f32;
            // The graph tops out at three budgets; the hairline marks one.
            let ceiling = BUDGET.as_secs_f32() * 3.0;
            for (index, cost) in bars.iter().enumerate() {
                let share = (cost.as_secs_f32() / ceiling).clamp(0.0, 1.0);
                let height = (bounds.size.height * share).max(px(1.0));
                let left = bounds.right() - slot * (bars.len() - index) as f32;
                window.paint_quad(fill(
                    Bounds::new(
                        point(left, bounds.bottom() - height),
                        size(slot * 0.7, height),
                    ),
                    bar_color(*cost),
                ));
            }
            let budget = bounds.bottom() - bounds.size.height / 3.0;
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.left(), budget),
                    size(bounds.size.width, px(1.0)),
                ),
                budget_line,
            ));
        },
    )
    .w_full()
    .h(px(34.0))
    .rounded(px(Radius::CHIP));

    let stat = |label: &'static str, value: String| {
        div()
            .flex()
            .justify_between()
            .gap(px(8.0))
            .child(div().text_color(tertiary).child(label))
            .child(
                div()
                    .text_color(primary)
                    .font_features(tnum())
                    .debug_selector(move || format!("perf-stat-{label}"))
                    .child(value),
            )
    };

    let last = frames.last;
    let mut stats = div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .text_size(px(11.0))
        .child(stat(
            "last frame",
            format!("{} ms", opt_ms(last.map(|frame| frame.cost))),
        ))
        .child(stat(
            "median / p95",
            format!("{} / {} ms", opt_ms(frames.p50), opt_ms(frames.p95)),
        ))
        .child(stat(
            "views",
            last.map_or_else(
                || "—".to_owned(),
                |frame| {
                    format!(
                        "{} drawn · {} reused",
                        frame.views_rendered, frame.views_reused
                    )
                },
            ),
        ))
        .child(stat(
            "terminals",
            last.map_or_else(
                || "—".to_owned(),
                |frame| format!("{} painted", frame.terminal_paints),
            ),
        ))
        .child(stat(
            "frames",
            format!(
                "{} · {} dropped",
                group_digits(frames.frames_total),
                group_digits(frames.dropped_total)
            ),
        ));
    if let Some(memory) = model.memory {
        stats = stats.child(stat(
            "memory",
            format!(
                "{} · {} fds",
                megabytes(memory.footprint_bytes),
                memory.open_fds
            ),
        ));
    }

    let renders = model.renders.map(|rows| {
        let mut list = div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .pt(px(8.0))
            .border_t_1()
            .border_color(hairline)
            .text_size(px(11.0))
            .child(
                div()
                    .flex()
                    .text_color(tertiary)
                    .font_weight(FontWeight::MEDIUM)
                    .child(div().flex_1().child("renders"))
                    .child(div().w(px(40.0)).flex().justify_end().child("/s"))
                    .child(div().w(px(56.0)).flex().justify_end().child("total")),
            );
        if rows.is_empty() {
            list = list.child(
                div()
                    .text_color(tertiary)
                    .child("nothing rendered since reset"),
            );
        }
        for row in rows {
            let hot = row.per_second >= 30;
            list = list.child(
                div()
                    .flex()
                    .font_features(tnum())
                    .debug_selector(move || format!("perf-renders-{}", row.name))
                    .child(div().flex_1().text_color(secondary).child(row.name))
                    .child(
                        div()
                            .w(px(40.0))
                            .flex()
                            .justify_end()
                            .text_color(if hot { Ink::ATTENTION } else { primary })
                            .child(row.per_second.to_string()),
                    )
                    .child(
                        div()
                            .w(px(56.0))
                            .flex()
                            .justify_end()
                            .text_color(primary)
                            .child(group_digits(row.total)),
                    ),
            );
        }
        list
    });

    let mut panel = div()
        .id("perf-overlay")
        .debug_selector(|| "perf-overlay".into())
        .absolute()
        .w(px(WIDTH))
        .p(px(10.0))
        .flex()
        .flex_col()
        .gap(px(8.0))
        .rounded(px(Radius::CARD))
        .border_1()
        .border_color(colors.floating_stroke())
        .bg(colors.floating_surface())
        .shadow_md()
        .font_family(crate::fonts::ui_family())
        .text_color(primary)
        // Clicks and scrolls stop here instead of reaching the terminal.
        .occlude()
        .child(header)
        .child(summary)
        .child(graph)
        .child(stats)
        .children(renders);
    panel = match model.corner {
        Corner::TopRight => panel.top(px(TOP_INSET)).right(px(INSET)),
        Corner::BottomRight => panel.bottom(px(INSET)).right(px(INSET)),
        Corner::BottomLeft => panel.bottom(px(INSET)).left(px(INSET)),
        Corner::TopLeft => panel.top(px(TOP_INSET)).left(px(INSET)),
    };
    panel.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(end: Instant, cost_ms: u64) -> FrameSample {
        FrameSample {
            end,
            cost: Duration::from_millis(cost_ms),
            views_rendered: 2,
            views_reused: 5,
            terminal_paints: 1,
        }
    }

    #[test]
    fn a_frame_is_late_by_whole_frames() {
        let tenths = |tenths: u64| Duration::from_micros(tenths * 100);
        assert_eq!(missed(tenths(169)), 0, "jitter is on time");
        assert_eq!(missed(tenths(333)), 1);
        assert_eq!(missed(tenths(500)), 2);
    }

    #[test]
    fn fps_counts_the_frames_of_the_last_second() {
        let start = Instant::now();
        let mut log = FrameLog::default();
        assert!(log.readout(start).idle, "no frames reads as idle");
        for n in 0..=100u32 {
            log.record(sample(start + Duration::from_millis(20) * n, 4));
        }
        let now = start + Duration::from_millis(2000);
        let readout = log.readout(now);
        assert_eq!(readout.fps, 50, "a frame each 20 ms over the last second");
        assert!(!readout.idle);
        assert_eq!(readout.frames_total, 101);
        assert_eq!(readout.dropped_total, 0, "20 ms apart is on time");
        assert_eq!(readout.bars.len(), BARS);
        assert!(
            log.readout(now + Duration::from_millis(1001)).idle,
            "a quiet second is idle, not 0 fps of jank"
        );
    }

    #[test]
    fn gaps_inside_a_burst_drop_frames_and_gaps_between_bursts_do_not() {
        let start = Instant::now();
        let mut log = FrameLog::default();
        let at = |ms: u64| start + Duration::from_millis(ms);
        log.record(sample(at(0), 4));
        log.record(sample(at(17), 4));
        // 50 ms after the last: two frames missed.
        log.record(sample(at(67), 4));
        // Two seconds of nothing, then a frame: idle, not dropped.
        log.record(sample(at(2067), 4));
        let readout = log.readout(at(2100));
        assert_eq!(readout.dropped_total, 2);
        assert_eq!(readout.dropped_recent, 0, "the drop was over a second ago");
        assert_eq!(readout.fps, 1);
    }

    #[test]
    fn percentiles_use_nearest_rank_over_recent_costs() {
        let start = Instant::now();
        let mut log = FrameLog::default();
        for cost in 1..=100u64 {
            log.record(sample(start + Duration::from_millis(cost * 16), cost));
        }
        let readout = log.readout(start + Duration::from_millis(1600));
        assert_eq!(readout.p50, Some(Duration::from_millis(50)));
        assert_eq!(readout.p95, Some(Duration::from_millis(95)));
        assert_eq!(readout.worst, Some(Duration::from_millis(100)));
        assert_eq!(
            readout.last.map(|frame| frame.cost),
            Some(Duration::from_millis(100))
        );
        assert_eq!(percentile(&[], 0.5), None);
        assert_eq!(
            percentile(&[Duration::from_millis(7)], 0.95),
            Some(Duration::from_millis(7))
        );
    }

    #[test]
    fn history_is_bounded() {
        let start = Instant::now();
        let mut log = FrameLog::default();
        for n in 0..(HISTORY as u32 * 3) {
            log.record(sample(start + Duration::from_millis(16) * n, 1));
        }
        assert_eq!(log.samples.len(), HISTORY);
        assert_eq!(log.frames_total, HISTORY as u64 * 3);
    }

    #[test]
    fn the_overlays_own_repaint_is_left_out() {
        let start = Instant::now();
        let mut log = FrameLog::default();
        log.record(sample(start, 3));
        log.skip_next();
        log.record(sample(start + Duration::from_millis(1100), 3));
        assert_eq!(log.frames_total, 1, "the idle repaint is not a frame");
        log.record(sample(start + Duration::from_millis(1200), 3));
        assert_eq!(log.frames_total, 2, "only one frame is skipped");
    }

    #[test]
    fn render_counts_roll_over_after_a_second_and_reset() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut counts = RenderCounts::default();
        assert_eq!(counts.hit("sidebar", at(0)), 1);
        counts.hit("sidebar", at(500));
        counts.hit("terminal", at(600));
        assert_eq!(counts.hit("sidebar", at(1200)), 3);
        let rows = counts.rows(at(1300));
        assert_eq!(
            rows,
            vec![
                RenderRow {
                    name: "sidebar",
                    per_second: 2,
                    total: 3
                },
                RenderRow {
                    name: "terminal",
                    per_second: 1,
                    total: 1
                },
            ],
            "the render at 0 ms left the window; busiest first"
        );
        let later = counts.rows(at(5000));
        assert!(later.iter().all(|row| row.per_second == 0));
        assert_eq!(counts.total("sidebar"), 3, "totals outlive the window");
        counts.reset();
        assert!(counts.rows(at(5000)).is_empty());
        assert_eq!(counts.total("sidebar"), 0);
    }

    #[test]
    fn render_stamps_are_bounded_per_view() {
        let start = Instant::now();
        let mut counts = RenderCounts::default();
        for n in 0..(RECENT_RENDERS as u64 * 2) {
            counts.hit("root", start + Duration::from_micros(n));
        }
        assert_eq!(counts.entries[0].recent.len(), RECENT_RENDERS);
        assert_eq!(counts.total("root"), RECENT_RENDERS as u64 * 2);
    }

    #[test]
    fn disabled_counters_record_nothing_and_badge_nothing() {
        gate::set(false, false);
        reset();
        for _ in 0..1000 {
            rendered("sidebar");
        }
        assert_eq!(RENDERS.with(|counts| counts.borrow().total("sidebar")), 0);
        assert!(badge("sidebar").is_none());

        gate::set(false, true);
        rendered("sidebar");
        rendered("sidebar");
        assert_eq!(RENDERS.with(|counts| counts.borrow().total("sidebar")), 2);
        assert!(badge("sidebar").is_some());
        reset();
        assert_eq!(RENDERS.with(|counts| counts.borrow().total("sidebar")), 0);
        gate::set(false, false);
    }

    #[test]
    fn preferences_drive_both_switches_and_default_off() {
        let prefs = Prefs::default();
        assert!(!prefs.perf_overlay);
        assert!(!prefs.render_counters);
        let saved: Prefs = serde_json::from_str("{}").unwrap();
        assert!(!saved.perf_overlay && !saved.render_counters);
        let saved: Prefs =
            serde_json::from_str(r#"{"perfOverlay":true,"renderCounters":true}"#).unwrap();
        apply_prefs(&saved);
        assert!(overlay_enabled() && render_counters_enabled());
        apply_prefs(&prefs);
        assert!(!overlay_enabled() && !render_counters_enabled());
    }

    #[test]
    fn corner_cycles_clockwise_back_to_the_start() {
        let mut corner = Corner::default();
        for _ in 0..4 {
            corner = corner.next();
        }
        assert_eq!(corner, Corner::TopRight);
        assert_eq!(Corner::TopRight.next(), Corner::BottomRight);
    }

    #[test]
    fn bars_turn_amber_over_budget_and_red_at_twice_it() {
        assert_eq!(bar_color(Duration::from_millis(16)), Ink::FRESH);
        assert_eq!(bar_color(Duration::from_micros(19_500)), Ink::ATTENTION);
        assert_eq!(bar_color(Duration::from_millis(40)), Ink::DANGER);
    }

    #[test]
    fn numbers_read_at_a_glance() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_234_567), "1,234,567");
        assert_eq!(ms(Duration::from_micros(4_150)), "4.2");
        assert_eq!(ms(Duration::from_millis(250)), "250");
        assert_eq!(megabytes(412 * 1024 * 1024), "412 MB");
    }
}
